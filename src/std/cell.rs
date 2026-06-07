//! A minimal, dependency‑free `RefCell` implementation.
//!
//! This replaces the forwarding stub to `std::cell::RefCell`. It uses
//! `core::cell::Cell<usize>` for borrow flags and raw pointers for the
//! inner data.

use core::cell::Cell;
use core::fmt;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;

/// A mutable memory location with dynamically checked borrow rules.
pub struct RefCell<T: ?Sized> {
    borrow: Cell<usize>,
    value: NonNull<T>,
    _not_send: PhantomData<*mut T>, // makes !Send and !Sync on stable
}

impl<T> RefCell<T> {
    /// Creates a new `RefCell` containing `value`.
    pub fn new(value: T) -> Self {
        let boxed = Box::into_raw(Box::new(value));
        RefCell {
            borrow: Cell::new(0),
            value: NonNull::new(boxed).expect("allocation failed"),
            _not_send: PhantomData,
        }
    }

    /// Returns the inner value, consuming the `RefCell`.
    pub fn into_inner(self) -> T {
        let ptr = self.value.as_ptr();
        // Take ownership of the box without running destructor on the RefCell
        let _ = self; // consume self
        unsafe { *Box::from_raw(ptr) }
    }
}

impl<T: ?Sized> RefCell<T> {
    /// Immutably borrows the wrapped value.
    ///
    /// # Panics
    /// Panics if the value is currently mutably borrowed.
    pub fn borrow(&self) -> Ref<'_, T> {
        self.try_borrow().expect("already mutably borrowed")
    }

    /// Mutably borrows the wrapped value.
    ///
    /// # Panics
    /// Panics if the value is currently borrowed.
    pub fn borrow_mut(&self) -> RefMut<'_, T> {
        self.try_borrow_mut().expect("already borrowed")
    }

    /// Tries to immutably borrow the value, returning `None` if already mutably borrowed.
    pub fn try_borrow(&self) -> Option<Ref<'_, T>> {
        let b = self.borrow.get();
        if b & MUT_BIT == 0 {
            self.borrow.set(b + SHARED_INCREMENT);
            Some(Ref { cell: self })
        } else {
            None
        }
    }

    /// Tries to mutably borrow the value, returning `None` if already borrowed.
    pub fn try_borrow_mut(&self) -> Option<RefMut<'_, T>> {
        let b = self.borrow.get();
        if b == 0 {
            self.borrow.set(MUT_BIT);
            Some(RefMut { cell: self })
        } else {
            None
        }
    }

    /// Returns the raw pointer to the underlying data.
    pub fn as_ptr(&self) -> *mut T {
        self.value.as_ptr()
    }

    /// Returns the number of active shared borrows (0 when mutably borrowed).
    pub fn borrow_count(&self) -> usize {
        self.borrow.get() >> SHARED_COUNT_SHIFT
    }

    /// Returns `true` if the value is currently mutably borrowed.
    pub fn is_mutably_borrowed(&self) -> bool {
        self.borrow.get() & MUT_BIT != 0
    }
}

impl<T: ?Sized> Drop for RefCell<T> {
    fn drop(&mut self) {
        // The underlying box is deallocated when we drop the RefCell.
        let ptr = self.value.as_ptr();
        unsafe {
            drop(Box::from_raw(ptr));
        }
    }
}

/// A guard that holds an immutable borrow of a `RefCell`.
pub struct Ref<'a, T: ?Sized> {
    cell: &'a RefCell<T>,
}

impl<'a, T: ?Sized> Deref for Ref<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.cell.value.as_ptr() }
    }
}

impl<'a, T: ?Sized> Drop for Ref<'a, T> {
    fn drop(&mut self) {
        let b = self.cell.borrow.get();
        debug_assert!(b >= SHARED_INCREMENT);
        self.cell.borrow.set(b - SHARED_INCREMENT);
    }
}

impl<'a, T: ?Sized + fmt::Debug> fmt::Debug for Ref<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

/// A guard that holds a mutable borrow of a `RefCell`.
pub struct RefMut<'a, T: ?Sized> {
    cell: &'a RefCell<T>,
}

impl<'a, T: ?Sized> Deref for RefMut<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.cell.value.as_ptr() }
    }
}

impl<'a, T: ?Sized> DerefMut for RefMut<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.cell.value.as_ptr() }
    }
}

impl<'a, T: ?Sized> Drop for RefMut<'a, T> {
    fn drop(&mut self) {
        debug_assert!(self.cell.borrow.get() == MUT_BIT);
        self.cell.borrow.set(0);
    }
}

impl<'a, T: ?Sized + fmt::Debug> fmt::Debug for RefMut<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

// Borrow flags: low bit indicates mutable borrow, remaining bits count shared borrows.
const MUT_BIT: usize = 1;
const SHARED_COUNT_SHIFT: usize = 1;
const SHARED_INCREMENT: usize = 1 << SHARED_COUNT_SHIFT;

#[cfg(test)]
mod tests {
    use super::RefCell;

    #[test]
    fn borrow_and_borrow_mut() {
        let cell = RefCell::new(42);
        {
            let b1 = cell.borrow();
            let b2 = cell.borrow();
            assert_eq!(*b1, 42);
            assert_eq!(*b2, 42);
        }
        let mut b = cell.borrow_mut();
        *b = 100;
        assert_eq!(*b, 100);
    }

    #[test]
    #[should_panic]
    fn borrow_mut_while_borrowed() {
        let cell = RefCell::new(5);
        let _b = cell.borrow();
        let _bm = cell.borrow_mut(); // panics
    }

    #[test]
    fn try_borrow() {
        let cell = RefCell::new("hello");
        let b = cell.try_borrow().unwrap();
        assert_eq!(*b, "hello");
        assert!(cell.try_borrow_mut().is_none());
    }

    #[test]
    fn into_inner() {
        let cell = RefCell::new(99);
        let val = cell.into_inner();
        assert_eq!(val, 99);
    }
}
