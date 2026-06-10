//! A minimal, dependency‑free `Vec<T>` implementation.
//! Uses only `core` and `std::alloc`. No other `std` modules.

use core::alloc::Layout;
use core::marker::PhantomData;
use core::mem;
use core::ops::{Deref, DerefMut, Index, IndexMut};
use core::ptr;
use core::slice;
use std::alloc::{self, handle_alloc_error};

#[derive(Debug)]
pub struct Vec<T> {
    ptr: *mut T,
    len: usize,
    capacity: usize,
    _marker: PhantomData<T>,
}

impl<T> Vec<T> {
    #[inline]
    pub fn new() -> Self {
        Self {
            ptr: ptr::null_mut(),
            len: 0,
            capacity: 0,
            _marker: PhantomData,
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        if capacity == 0 {
            return Self::new();
        }
        let layout = Layout::array::<T>(capacity).expect("invalid capacity");
        let ptr = unsafe { alloc::alloc(layout) as *mut T };
        if ptr.is_null() {
            handle_alloc_error(layout);
        }
        Self {
            ptr,
            len: 0,
            capacity,
            _marker: PhantomData,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn reserve(&mut self, additional: usize) {
        let new_cap = self.len.checked_add(additional).expect("capacity overflow");
        if new_cap <= self.capacity {
            return;
        }
        self.grow(new_cap);
    }

    pub fn push(&mut self, value: T) {
        if self.len == self.capacity {
            self.grow(self.capacity.saturating_mul(2).max(4));
        }
        unsafe {
            let end = self.ptr.add(self.len);
            ptr::write(end, value);
            self.len += 1;
        }
    }

    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            None
        } else {
            self.len -= 1;
            unsafe { Some(ptr::read(self.ptr.add(self.len))) }
        }
    }

    pub fn clear(&mut self) {
        unsafe {
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(self.ptr, self.len));
            self.len = 0;
        }
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        if index < self.len {
            unsafe { Some(&*self.ptr.add(index)) }
        } else {
            None
        }
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        if index < self.len {
            unsafe { Some(&mut *self.ptr.add(index)) }
        } else {
            None
        }
    }

    pub fn extend_from_slice(&mut self, other: &[T])
    where
        T: Clone,
    {
        let new_len = self.len + other.len();
        self.reserve(other.len());
        unsafe {
            let dest = self.ptr.add(self.len);
            for (i, item) in other.iter().enumerate() {
                ptr::write(dest.add(i), item.clone());
            }
            self.len = new_len;
        }
    }

    fn grow(&mut self, new_cap: usize) {
        let new_layout = Layout::array::<T>(new_cap).expect("invalid capacity");
        let new_ptr = if self.capacity == 0 {
            unsafe { alloc::alloc(new_layout) as *mut T }
        } else {
            let old_layout = Layout::array::<T>(self.capacity).expect("invalid capacity");
            unsafe { alloc::realloc(self.ptr as *mut u8, old_layout, new_layout.size()) as *mut T }
        };
        if new_ptr.is_null() {
            handle_alloc_error(new_layout);
        }
        self.ptr = new_ptr;
        self.capacity = new_cap;
    }
}

impl<T> Deref for Vec<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        if self.len == 0 {
            &[] // safe empty slice
        } else {
            unsafe { slice::from_raw_parts(self.ptr, self.len) }
        }
    }
}

impl<T> DerefMut for Vec<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        if self.len == 0 {
            &mut [] // safe empty slice
        } else {
            unsafe { slice::from_raw_parts_mut(self.ptr, self.len) }
        }
    }
}

impl<T> Default for Vec<T> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Drop for Vec<T> {
    fn drop(&mut self) {
        unsafe {
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(self.ptr, self.len));
            if self.capacity > 0 {
                let layout = Layout::array::<T>(self.capacity).expect("invalid capacity");
                alloc::dealloc(self.ptr as *mut u8, layout);
            }
        }
    }
}

impl<T> Index<usize> for Vec<T> {
    type Output = T;
    #[inline]
    fn index(&self, index: usize) -> &Self::Output {
        self.get(index).expect("index out of bounds")
    }
}

impl<T> IndexMut<usize> for Vec<T> {
    #[inline]
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        self.get_mut(index).expect("index out of bounds")
    }
}

pub struct IntoIter<T> {
    ptr: *mut T,
    end: *mut T,
    cap: usize,
    _marker: PhantomData<T>,
}

impl<T> IntoIterator for Vec<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        let iter = unsafe {
            let raw = self.ptr;
            let len = self.len;
            let cap = self.capacity;
            mem::forget(self);
            IntoIter {
                ptr: raw,
                end: if len == 0 { raw } else { raw.add(len) },
                cap,
                _marker: PhantomData,
            }
        };
        iter
    }
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.ptr == self.end {
            None
        } else {
            unsafe {
                let result = ptr::read(self.ptr);
                self.ptr = self.ptr.add(1);
                Some(result)
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = (self.end as usize - self.ptr as usize) / mem::size_of::<T>();
        (len, Some(len))
    }
}

impl<T> Drop for IntoIter<T> {
    fn drop(&mut self) {
        unsafe {
            while self.ptr != self.end {
                let _ = ptr::read(self.ptr);
                self.ptr = self.ptr.add(1);
            }
            if self.cap > 0 {
                let layout = Layout::array::<T>(self.cap).expect("invalid capacity");
                alloc::dealloc(self.ptr as *mut u8, layout);
            }
        }
    }
}

impl<T: Clone> Clone for Vec<T> {
    fn clone(&self) -> Self {
        let mut new = Vec::with_capacity(self.len);
        for item in self.iter() {
            new.push(item.clone());
        }
        new
    }
}

impl<T> AsRef<[T]> for Vec<T> {
    fn as_ref(&self) -> &[T] {
        self
    }
}

// FromIterator implementation (using core::iter)
impl<T> core::iter::FromIterator<T> for Vec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut vec = Vec::new();
        for item in iter {
            vec.push(item);
        }
        vec
    }
}

#[cfg(test)]
mod tests {
    use super::Vec;

    #[test]
    fn push_and_pop() {
        let mut v = Vec::new();
        v.push(1);
        v.push(2);
        assert_eq!(v.len(), 2);
        assert_eq!(v.pop(), Some(2));
        assert_eq!(v.pop(), Some(1));
        assert_eq!(v.pop(), None);
    }

    #[test]
    fn indexing() {
        let mut v = Vec::new();
        v.push(10);
        v.push(20);
        assert_eq!(v[0], 10);
        assert_eq!(v[1], 20);
        v[0] = 99;
        assert_eq!(v[0], 99);
    }

    #[test]
    fn reserve() {
        let mut v = Vec::with_capacity(2);
        assert_eq!(v.capacity(), 2);
        v.push(1);
        v.push(2);
        v.push(3);
        assert_eq!(v.len(), 3);
        assert!(v.capacity() >= 3);
    }

    #[test]
    fn clear_and_extend() {
        let mut v = Vec::new();
        v.push(1);
        v.push(2);
        v.clear();
        assert!(v.is_empty());
        v.extend_from_slice(&[3, 4, 5]);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], 3);
        assert_eq!(v[2], 5);
    }

    #[test]
    fn into_iter() {
        let v = Vec::from_iter(vec![1, 2, 3]);
        let sum: i32 = v.into_iter().sum();
        assert_eq!(sum, 6);
    }
}
