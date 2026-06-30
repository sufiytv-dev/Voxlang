//! A minimal, dependency‑free `HashMap` implementation.
//!
//! This replaces the forwarding stub to `std::collections::HashMap`. It uses
//! open addressing with quadratic probing and the FNV‑1a hash function,
//! providing only the operations required by the Vox compiler during bootstrap.

use super::vec::Vec;
use core::hash::{BuildHasher, Hash, Hasher};
use core::mem;

/// The FNV‑1a hash implementation.
#[derive(Default)]
pub(crate) struct FNVHasher(u64);

impl Hasher for FNVHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

/// Build hasher for FNV‑1a.
pub type FNVBuildHasher = core::hash::BuildHasherDefault<FNVHasher>;

/// A hash map with FNV‑1a hashing and quadratic probing.
///
/// This implementation is minimal and deterministic, suitable for compiler
/// internal data structures.
pub struct HashMap<K, V> {
    buckets: Vec<Option<Entry<K, V>>>,
    size: usize,
    hasher: FNVBuildHasher,
}

struct Entry<K, V> {
    key: K,
    value: V,
    hash: u64,
}

impl<K, V> HashMap<K, V> {
    /// Creates an empty `HashMap` with default capacity.
    pub fn new() -> Self {
        Self::with_capacity(16)
    }

    /// Creates an empty `HashMap` with at least the specified capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        let cap = capacity.next_power_of_two();
        let mut buckets = Vec::with_capacity(cap);
        for _ in 0..cap {
            buckets.push(None);
        }
        Self {
            buckets,
            size: 0,
            hasher: FNVBuildHasher::default(),
        }
    }

    /// Returns the number of elements in the map.
    pub fn len(&self) -> usize {
        self.size
    }

    /// Returns `true` if the map contains no elements.
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// Inserts a key‑value pair into the map, returning the previous value if the key already existed.
    pub fn insert(&mut self, key: K, value: V) -> Option<V>
    where
        K: Hash + Eq,
    {
        let hash = self.hash(&key);
        let idx = self.probe(hash, &key);
        if let Some(entry) = self.buckets[idx].as_mut() {
            let old = mem::replace(&mut entry.value, value);
            Some(old)
        } else {
            if self.load_factor() > 0.75 {
                self.resize();
                let hash = self.hash(&key);
                let new_idx = self.probe(hash, &key);
                self.buckets[new_idx] = Some(Entry { key, value, hash });
            } else {
                self.buckets[idx] = Some(Entry { key, value, hash });
            }
            self.size += 1;
            None
        }
    }

    /// Returns a reference to the value corresponding to the key.
    pub fn get(&self, key: &K) -> Option<&V>
    where
        K: Hash + Eq,
    {
        let hash = self.hash(key);
        let idx = self.probe(hash, key);
        self.buckets[idx].as_ref().map(|entry| &entry.value)
    }

    /// Returns a mutable reference to the value corresponding to the key.
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V>
    where
        K: Hash + Eq,
    {
        let hash = self.hash(key);
        let idx = self.probe(hash, key);
        self.buckets[idx].as_mut().map(|entry| &mut entry.value)
    }

    /// Returns `true` if the map contains the specified key.
    pub fn contains_key(&self, key: &K) -> bool
    where
        K: Hash + Eq,
    {
        let hash = self.hash(key);
        let idx = self.probe(hash, key);
        self.buckets[idx].is_some()
    }

    /// Removes a key from the map, returning the value if it existed.
    pub fn remove(&mut self, key: &K) -> Option<V>
    where
        K: Hash + Eq,
    {
        let hash = self.hash(key);
        let idx = self.probe(hash, key);
        if let Some(entry) = self.buckets[idx].take() {
            self.size -= 1;
            self.backward_shift(idx);
            Some(entry.value)
        } else {
            None
        }
    }

    /// Clears the map, removing all key‑value pairs.
    pub fn clear(&mut self) {
        let cap = 16;
        let mut buckets = Vec::with_capacity(cap);
        for _ in 0..cap {
            buckets.push(None);
        }
        self.buckets = buckets;
        self.size = 0;
    }

    /// Returns an immutable iterator over key‑value pairs.
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter {
            inner: self.buckets.iter(),
        }
    }

    // --- internal helpers ---

    fn hash(&self, key: &K) -> u64
    where
        K: Hash,
    {
        let mut h = self.hasher.build_hasher();
        key.hash(&mut h);
        h.finish()
    }

    fn load_factor(&self) -> f64 {
        self.size as f64 / self.buckets.len() as f64
    }

    fn probe(&self, hash: u64, key: &K) -> usize
    where
        K: Eq,
    {
        let mask = self.buckets.len() - 1;
        let mut idx = (hash & mask as u64) as usize;
        let mut step = 1;
        loop {
            match &self.buckets[idx] {
                None => return idx,
                Some(entry) if entry.hash == hash && entry.key == *key => return idx,
                _ => {
                    idx = (idx + step) & mask;
                    step += 1;
                }
            }
        }
    }

    fn resize(&mut self)
    where
        K: Hash + Eq,
    {
        let new_cap = self.buckets.len() * 2;
        let mut new_buckets = Vec::with_capacity(new_cap);
        for _ in 0..new_cap {
            new_buckets.push(None);
        }
        mem::swap(&mut self.buckets, &mut new_buckets);
        self.size = 0;

        for entry in new_buckets.into_iter().flatten() {
            let hash = entry.hash;
            let idx = self.probe_in_table(hash);
            self.buckets[idx] = Some(entry);
            self.size += 1;
        }
    }

    // probe_in_table now only needs the hash (key not used)
    fn probe_in_table(&self, hash: u64) -> usize {
        let mask = self.buckets.len() - 1;
        let mut idx = (hash & mask as u64) as usize;
        let mut step = 1;
        loop {
            if self.buckets[idx].is_none() {
                return idx;
            }
            idx = (idx + step) & mask;
            step += 1;
        }
    }

    fn backward_shift(&mut self, mut idx: usize)
    where
        K: Eq,
    {
        let mask = self.buckets.len() - 1;
        let mut step = 1;
        loop {
            let next_idx = (idx + step) & mask;
            if let Some(entry) = self.buckets[next_idx].take() {
                let rehash = entry.hash;
                let target_idx = self.probe_in_table(rehash);
                self.buckets[target_idx] = Some(entry);
                if target_idx == idx {
                    break;
                }
                idx = next_idx;
                step = 1;
            } else {
                break;
            }
        }
    }
}

impl<K, V> Default for HashMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

/// Immutable iterator over a `HashMap`.
pub struct Iter<'a, K, V> {
    inner: core::slice::Iter<'a, Option<Entry<K, V>>>,
}

impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(entry) = self.inner.next() {
            if let Some(entry) = entry.as_ref() {
                return Some((&entry.key, &entry.value));
            }
        }
        None
    }
}

/// Owning iterator over a `HashMap`.
pub struct IntoIter<K, V> {
    inner: super::vec::IntoIter<Option<Entry<K, V>>>,
}

impl<K, V> IntoIterator for HashMap<K, V> {
    type Item = (K, V);
    type IntoIter = IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        IntoIter {
            inner: self.buckets.into_iter(),
        }
    }
}

impl<K, V> Iterator for IntoIter<K, V> {
    type Item = (K, V);

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(opt) = self.inner.next() {
            if let Some(entry) = opt {
                return Some((entry.key, entry.value));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::HashMap;

    #[test]
    fn insert_and_get() {
        let mut map = HashMap::new();
        map.insert("a", 1);
        map.insert("b", 2);
        assert_eq!(map.get(&"a"), Some(&1));
        assert_eq!(map.get(&"c"), None);
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn overwrite() {
        let mut map = HashMap::new();
        map.insert("a", 1);
        assert_eq!(map.insert("a", 10), Some(1));
        assert_eq!(map.get(&"a"), Some(&10));
    }

    #[test]
    fn contains_key() {
        let mut map = HashMap::new();
        map.insert("x", 100);
        assert!(map.contains_key(&"x"));
        assert!(!map.contains_key(&"y"));
    }

    #[test]
    fn remove() {
        let mut map = HashMap::new();
        map.insert("a", 1);
        assert_eq!(map.remove(&"a"), Some(1));
        assert_eq!(map.get(&"a"), None);
        assert!(map.is_empty());
    }

    #[test]
    fn clear() {
        let mut map = HashMap::new();
        map.insert("a", 1);
        map.clear();
        assert_eq!(map.len(), 0);
        assert!(!map.contains_key(&"a"));
    }

    #[test]
    fn iter() {
        let mut map = HashMap::new();
        map.insert("a", 1);
        map.insert("b", 2);
        let mut pairs: Vec<_> = map.iter().collect();
        pairs.sort_by_key(|(k, _)| **k);
        assert_eq!(pairs, [(&"a", &1), (&"b", &2)]);
    }

    #[test]
    fn into_iter() {
        let mut map = HashMap::new();
        map.insert("a", 1);
        map.insert("b", 2);
        let mut pairs: Vec<_> = map.into_iter().collect();
        pairs.sort_by_key(|(k, _)| *k);
        assert_eq!(pairs, [("a", 1), ("b", 2)]);
    }
}
