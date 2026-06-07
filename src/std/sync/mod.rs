//! Minimal `std::sync` replacement using Windows SRWLock and Unix pthreads.
//! No dependency on `std::sync`.

pub mod atomic {
    pub use core::sync::atomic::*;
}

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
#[cfg(unix)]
use core::ptr;

// -----------------------------------------------------------------------------
// Mutex
// -----------------------------------------------------------------------------
#[cfg(windows)]
mod mutex_impl {
    use super::*;
    use core::cell::UnsafeCell;
    use core::ptr;

    #[repr(C)]
    struct SRWLOCK {
        ptr: *mut core::ffi::c_void,
    }

    unsafe extern "system" {
        fn InitializeSRWLock(lpSRWLock: *mut SRWLOCK);
        fn AcquireSRWLockExclusive(lpSRWLock: *mut SRWLOCK);
        fn ReleaseSRWLockExclusive(lpSRWLock: *mut SRWLOCK);
        fn TryAcquireSRWLockExclusive(lpSRWLock: *mut SRWLOCK) -> u8;
    }

    pub struct Mutex<T: ?Sized> {
        lock: UnsafeCell<SRWLOCK>,
        data: UnsafeCell<T>,
    }

    unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
    unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

    impl<T> Mutex<T> {
        pub fn new(t: T) -> Self {
            let lock = UnsafeCell::new(SRWLOCK {
                ptr: ptr::null_mut(),
            });
            unsafe {
                InitializeSRWLock(lock.get());
            }
            Mutex {
                lock,
                data: UnsafeCell::new(t),
            }
        }
    }

    impl<T: ?Sized> Mutex<T> {
        pub fn lock(&self) -> MutexGuard<'_, T> {
            unsafe {
                AcquireSRWLockExclusive(self.lock.get());
            }
            MutexGuard { mutex: self }
        }

        pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
            unsafe {
                if TryAcquireSRWLockExclusive(self.lock.get()) != 0 {
                    Some(MutexGuard { mutex: self })
                } else {
                    None
                }
            }
        }

        pub fn get_mut(&mut self) -> &mut T {
            unsafe { &mut *self.data.get() }
        }
    }

    pub struct MutexGuard<'a, T: ?Sized> {
        mutex: &'a Mutex<T>,
    }

    impl<T: ?Sized> Deref for MutexGuard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            unsafe { &*self.mutex.data.get() }
        }
    }

    impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            unsafe { &mut *self.mutex.data.get() }
        }
    }

    impl<T: ?Sized> Drop for MutexGuard<'_, T> {
        fn drop(&mut self) {
            unsafe { ReleaseSRWLockExclusive(self.mutex.lock.get()) }
        }
    }
}

#[cfg(unix)]
mod mutex_impl {
    use super::*;
    use core::cell::UnsafeCell;
    use core::ptr;

    #[repr(C)]
    struct pthread_mutex_t {
        data: [u64; 5],
    }

    unsafe extern "C" {
        fn pthread_mutex_init(mutex: *mut pthread_mutex_t, attr: *const core::ffi::c_void) -> i32;
        fn pthread_mutex_lock(mutex: *mut pthread_mutex_t) -> i32;
        fn pthread_mutex_trylock(mutex: *mut pthread_mutex_t) -> i32;
        fn pthread_mutex_unlock(mutex: *mut pthread_mutex_t) -> i32;
        fn pthread_mutex_destroy(mutex: *mut pthread_mutex_t) -> i32;
    }

    pub struct Mutex<T: ?Sized> {
        lock: UnsafeCell<pthread_mutex_t>,
        data: UnsafeCell<T>,
    }

    unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
    unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

    impl<T> Mutex<T> {
        pub fn new(t: T) -> Self {
            let lock = UnsafeCell::new(pthread_mutex_t { data: [0; 5] });
            unsafe {
                pthread_mutex_init(lock.get(), ptr::null());
            }
            Mutex {
                lock,
                data: UnsafeCell::new(t),
            }
        }
    }

    impl<T: ?Sized> Mutex<T> {
        pub fn lock(&self) -> MutexGuard<'_, T> {
            unsafe {
                pthread_mutex_lock(self.lock.get());
            }
            MutexGuard { mutex: self }
        }

        pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
            unsafe {
                if pthread_mutex_trylock(self.lock.get()) == 0 {
                    Some(MutexGuard { mutex: self })
                } else {
                    None
                }
            }
        }

        pub fn get_mut(&mut self) -> &mut T {
            unsafe { &mut *self.data.get() }
        }
    }

    impl<T: ?Sized> Drop for Mutex<T> {
        fn drop(&mut self) {
            unsafe {
                pthread_mutex_destroy(self.lock.get());
            }
        }
    }

    pub struct MutexGuard<'a, T: ?Sized> {
        mutex: &'a Mutex<T>,
    }

    impl<T: ?Sized> Deref for MutexGuard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            unsafe { &*self.mutex.data.get() }
        }
    }

    impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            unsafe { &mut *self.mutex.data.get() }
        }
    }

    impl<T: ?Sized> Drop for MutexGuard<'_, T> {
        fn drop(&mut self) {
            unsafe { pthread_mutex_unlock(self.mutex.lock.get()) }
        }
    }
}

pub use mutex_impl::Mutex;
pub use mutex_impl::MutexGuard;

// -----------------------------------------------------------------------------
// RwLock
// -----------------------------------------------------------------------------
#[cfg(windows)]
mod rwlock_impl {
    use super::*;
    use core::cell::UnsafeCell;
    use core::ptr;

    #[repr(C)]
    struct SRWLOCK {
        ptr: *mut core::ffi::c_void,
    }

    unsafe extern "system" {
        fn InitializeSRWLock(lpSRWLock: *mut SRWLOCK);
        fn AcquireSRWLockExclusive(lpSRWLock: *mut SRWLOCK);
        fn ReleaseSRWLockExclusive(lpSRWLock: *mut SRWLOCK);
        fn TryAcquireSRWLockExclusive(lpSRWLock: *mut SRWLOCK) -> u8;
        fn AcquireSRWLockShared(lpSRWLock: *mut SRWLOCK);
        fn ReleaseSRWLockShared(lpSRWLock: *mut SRWLOCK);
        fn TryAcquireSRWLockShared(lpSRWLock: *mut SRWLOCK) -> u8;
    }

    pub struct RwLock<T: ?Sized> {
        lock: UnsafeCell<SRWLOCK>,
        data: UnsafeCell<T>,
    }

    unsafe impl<T: ?Sized + Send> Send for RwLock<T> {}
    unsafe impl<T: ?Sized + Send> Sync for RwLock<T> {}

    impl<T> RwLock<T> {
        pub fn new(t: T) -> Self {
            let lock = UnsafeCell::new(SRWLOCK {
                ptr: ptr::null_mut(),
            });
            unsafe {
                InitializeSRWLock(lock.get());
            }
            RwLock {
                lock,
                data: UnsafeCell::new(t),
            }
        }
    }

    impl<T: ?Sized> RwLock<T> {
        pub fn read(&self) -> RwLockReadGuard<'_, T> {
            unsafe {
                AcquireSRWLockShared(self.lock.get());
            }
            RwLockReadGuard { rwlock: self }
        }

        pub fn write(&self) -> RwLockWriteGuard<'_, T> {
            unsafe {
                AcquireSRWLockExclusive(self.lock.get());
            }
            RwLockWriteGuard { rwlock: self }
        }

        pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
            unsafe {
                if TryAcquireSRWLockShared(self.lock.get()) != 0 {
                    Some(RwLockReadGuard { rwlock: self })
                } else {
                    None
                }
            }
        }

        pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
            unsafe {
                if TryAcquireSRWLockExclusive(self.lock.get()) != 0 {
                    Some(RwLockWriteGuard { rwlock: self })
                } else {
                    None
                }
            }
        }
    }

    pub struct RwLockReadGuard<'a, T: ?Sized> {
        rwlock: &'a RwLock<T>,
    }

    impl<T: ?Sized> Deref for RwLockReadGuard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            unsafe { &*self.rwlock.data.get() }
        }
    }

    impl<T: ?Sized> Drop for RwLockReadGuard<'_, T> {
        fn drop(&mut self) {
            unsafe { ReleaseSRWLockShared(self.rwlock.lock.get()) }
        }
    }

    pub struct RwLockWriteGuard<'a, T: ?Sized> {
        rwlock: &'a RwLock<T>,
    }

    impl<T: ?Sized> Deref for RwLockWriteGuard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            unsafe { &*self.rwlock.data.get() }
        }
    }

    impl<T: ?Sized> DerefMut for RwLockWriteGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            unsafe { &mut *self.rwlock.data.get() }
        }
    }

    impl<T: ?Sized> Drop for RwLockWriteGuard<'_, T> {
        fn drop(&mut self) {
            unsafe { ReleaseSRWLockExclusive(self.rwlock.lock.get()) }
        }
    }
}

#[cfg(unix)]
mod rwlock_impl {
    use super::*;
    use core::cell::UnsafeCell;
    use core::ptr;

    #[repr(C)]
    struct pthread_rwlock_t {
        data: [u64; 8],
    }

    unsafe extern "C" {
        fn pthread_rwlock_init(lock: *mut pthread_rwlock_t, attr: *const core::ffi::c_void) -> i32;
        fn pthread_rwlock_rdlock(lock: *mut pthread_rwlock_t) -> i32;
        fn pthread_rwlock_wrlock(lock: *mut pthread_rwlock_t) -> i32;
        fn pthread_rwlock_tryrdlock(lock: *mut pthread_rwlock_t) -> i32;
        fn pthread_rwlock_trywrlock(lock: *mut pthread_rwlock_t) -> i32;
        fn pthread_rwlock_unlock(lock: *mut pthread_rwlock_t) -> i32;
        fn pthread_rwlock_destroy(lock: *mut pthread_rwlock_t) -> i32;
    }

    pub struct RwLock<T: ?Sized> {
        lock: UnsafeCell<pthread_rwlock_t>,
        data: UnsafeCell<T>,
    }

    unsafe impl<T: ?Sized + Send> Send for RwLock<T> {}
    unsafe impl<T: ?Sized + Send> Sync for RwLock<T> {}

    impl<T> RwLock<T> {
        pub fn new(t: T) -> Self {
            let lock = UnsafeCell::new(pthread_rwlock_t { data: [0; 8] });
            unsafe {
                pthread_rwlock_init(lock.get(), ptr::null());
            }
            RwLock {
                lock,
                data: UnsafeCell::new(t),
            }
        }
    }

    impl<T: ?Sized> RwLock<T> {
        pub fn read(&self) -> RwLockReadGuard<'_, T> {
            unsafe {
                pthread_rwlock_rdlock(self.lock.get());
            }
            RwLockReadGuard { rwlock: self }
        }

        pub fn write(&self) -> RwLockWriteGuard<'_, T> {
            unsafe {
                pthread_rwlock_wrlock(self.lock.get());
            }
            RwLockWriteGuard { rwlock: self }
        }

        pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
            unsafe {
                if pthread_rwlock_tryrdlock(self.lock.get()) == 0 {
                    Some(RwLockReadGuard { rwlock: self })
                } else {
                    None
                }
            }
        }

        pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
            unsafe {
                if pthread_rwlock_trywrlock(self.lock.get()) == 0 {
                    Some(RwLockWriteGuard { rwlock: self })
                } else {
                    None
                }
            }
        }
    }

    impl<T: ?Sized> Drop for RwLock<T> {
        fn drop(&mut self) {
            unsafe {
                pthread_rwlock_destroy(self.lock.get());
            }
        }
    }

    pub struct RwLockReadGuard<'a, T: ?Sized> {
        rwlock: &'a RwLock<T>,
    }

    impl<T: ?Sized> Deref for RwLockReadGuard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            unsafe { &*self.rwlock.data.get() }
        }
    }

    impl<T: ?Sized> Drop for RwLockReadGuard<'_, T> {
        fn drop(&mut self) {
            unsafe { pthread_rwlock_unlock(self.rwlock.lock.get()) }
        }
    }

    pub struct RwLockWriteGuard<'a, T: ?Sized> {
        rwlock: &'a RwLock<T>,
    }

    impl<T: ?Sized> Deref for RwLockWriteGuard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            unsafe { &*self.rwlock.data.get() }
        }
    }

    impl<T: ?Sized> DerefMut for RwLockWriteGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            unsafe { &mut *self.rwlock.data.get() }
        }
    }

    impl<T: ?Sized> Drop for RwLockWriteGuard<'_, T> {
        fn drop(&mut self) {
            unsafe { pthread_rwlock_unlock(self.rwlock.lock.get()) }
        }
    }
}

pub use rwlock_impl::RwLock;
pub use rwlock_impl::RwLockReadGuard;
pub use rwlock_impl::RwLockWriteGuard;

// -----------------------------------------------------------------------------
// Once (simple atomic bool)
// -----------------------------------------------------------------------------
pub struct Once {
    done: atomic::AtomicBool,
}

impl Once {
    pub const fn new() -> Once {
        Once {
            done: atomic::AtomicBool::new(false),
        }
    }

    pub fn call_once<F: FnOnce()>(&self, f: F) {
        if !self.done.load(atomic::Ordering::Acquire) {
            // Double-check (simple, not perfect but enough for minimal use)
            if !self.done.load(atomic::Ordering::Acquire) {
                f();
                self.done.store(true, atomic::Ordering::Release);
            }
        }
    }
}

// -----------------------------------------------------------------------------
// OnceLock using Once + UnsafeCell (minimal, avoids move error)
// -----------------------------------------------------------------------------
pub struct OnceLock<T> {
    once: Once,
    value: UnsafeCell<Option<T>>,
}

impl<T> OnceLock<T> {
    pub const fn new() -> Self {
        OnceLock {
            once: Once::new(),
            value: UnsafeCell::new(None),
        }
    }

    pub fn get(&self) -> Option<&T> {
        unsafe { (*self.value.get()).as_ref() }
    }

    pub fn set(&self, value: T) -> Result<(), T> {
        // Use a raw pointer to the Option to avoid moving `value` into the closure.
        let mut val = Some(value);
        let val_ptr: *mut Option<T> = &mut val;
        self.once.call_once(|| unsafe {
            // Take the value from val and store it in the cell
            (*self.value.get()) = (*val_ptr).take();
        });
        // After call_once, if val is still `Some`, the closure didn't run
        // (because the Once was already called), so return the value back.
        match val {
            Some(v) => Err(v),
            None => Ok(()),
        }
    }
}

unsafe impl<T: Send + Sync> Sync for OnceLock<T> {}
unsafe impl<T: Send> Send for OnceLock<T> {}
