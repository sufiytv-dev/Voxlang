//! A minimal, dependency‑free `HashSet` implementation.
//!
//! This replaces the forwarding stub to `std::collections::HashSet`. It is
//! implemented as a thin wrapper over `HashMap<K, ()>` using the same FNV‑1a
//! hasher, providing only the operations required by the Vox compiler.

use super::hashmap::HashMap;
use core::hash::Hash;
use core::iter::FromIterator;

/// A hash set with FNV‑1a hashing.
pub struct HashSet<T> {
    map: HashMap<T, ()>,
}

impl<T> HashSet<T> {
    pub fn new() -> Self {
        HashSet {
            map: HashMap::new(),
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        HashSet {
            map: HashMap::with_capacity(capacity),
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn insert(&mut self, value: T) -> bool
    where
        T: Hash + Eq,
    {
        self.map.insert(value, ()).is_none()
    }

    pub fn contains(&self, value: &T) -> bool
    where
        T: Hash + Eq,
    {
        self.map.contains_key(value)
    }

    pub fn remove(&mut self, value: &T) -> bool
    where
        T: Hash + Eq,
    {
        self.map.remove(value).is_some()
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    pub fn iter(&self) -> Iter<'_, T> {
        Iter {
            inner: self.map.iter(),
        }
    }
}

impl<T> Default for HashSet<T> {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Iter<'a, T> {
    inner: super::hashmap::Iter<'a, T, ()>,
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(k, _)| k)
    }
}

impl<T> FromIterator<T> for HashSet<T>
where
    T: Hash + Eq,
{
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut set = HashSet::new();
        for item in iter {
            set.insert(item);
        }
        set
    }
}

impl<T> IntoIterator for HashSet<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        IntoIter {
            inner: self.map.into_iter(),
        }
    }
}

pub struct IntoIter<T> {
    inner: super::hashmap::IntoIter<T, ()>,
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(k, _)| k)
    }
}

#[cfg(test)]
mod tests {
    use super::HashSet;

    #[test]
    fn insert_contains() {
        let mut set = HashSet::new();
        assert!(set.insert(1));
        assert!(!set.insert(1));
        assert!(set.contains(&1));
        assert!(!set.contains(&2));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn remove() {
        let mut set = HashSet::new();
        set.insert(1);
        assert!(set.remove(&1));
        assert!(!set.remove(&1));
        assert!(!set.contains(&1));
        assert!(set.is_empty());
    }

    #[test]
    fn clear() {
        let mut set = HashSet::new();
        set.insert(1);
        set.insert(2);
        set.clear();
        assert_eq!(set.len(), 0);
        assert!(!set.contains(&1));
    }

    #[test]
    fn iter() {
        let mut set = HashSet::new();
        set.insert(1);
        set.insert(2);
        let mut elements: Vec<_> = set.iter().collect();
        elements.sort();
        assert_eq!(elements, [&1, &2]);
    }

    #[test]
    fn from_iter() {
        let set: HashSet<i32> = vec![1, 2, 3].into_iter().collect();
        assert!(set.contains(&1));
        assert!(set.contains(&2));
        assert!(set.contains(&3));
        assert_eq!(set.len(), 3);
    }
}
