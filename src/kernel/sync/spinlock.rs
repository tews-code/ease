//! Spinlocks

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::{disable_interrupts, restore_interrupts};

//-------------------------------------------------------------------------
//
//  IrqSpinLock
//
//-------------------------------------------------------------------------

/// SpinLock which disables interrupts and restores on exit
pub struct IrqSpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for IrqSpinLock<T> {}
unsafe impl<T: Send> Send for IrqSpinLock<T> {}

impl<T> IrqSpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> IrqSpinLockGuard<'_, T> {
        let mut prev_mstatus: usize;
        loop {
            // Spin with cheap relaxed loads while locked
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }

            // Disable interrupts before taking the lock
            prev_mstatus = disable_interrupts();
            // Only attempt CAS when we see it's free
            if self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
            restore_interrupts(prev_mstatus);
        }
        IrqSpinLockGuard {
            lock: self,
            prev_interrupt_status: prev_mstatus,
        }
    }

    #[expect(dead_code)]
    pub fn try_lock(&self) -> Option<IrqSpinLockGuard<'_, T>> {
        // Disable interrupts before taking the lock
        let prev_mstatus = disable_interrupts();
        // Only attempt CAS
        if self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(IrqSpinLockGuard {
                lock: self,
                prev_interrupt_status: prev_mstatus,
            })
        } else {
            restore_interrupts(prev_mstatus);
            None
        }
    }
}

#[must_use = "if unused, the lock is released immediately"]
pub struct IrqSpinLockGuard<'a, T> {
    lock: &'a IrqSpinLock<T>,
    prev_interrupt_status: usize,
}

impl<'a, T> Deref for IrqSpinLockGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.value.get() }
    }
}

impl<'a, T> DerefMut for IrqSpinLockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<'a, T> Drop for IrqSpinLockGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        // Enable interrupts if previously enabled
        restore_interrupts(self.prev_interrupt_status);
    }
}

//-------------------------------------------------------------------------
//
//  SpinLock
//
//-------------------------------------------------------------------------

///SpinLock that leaves interrupts enabled
pub struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        loop {
            // Spin with cheap relaxed loads while locked
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
            // Only attempt CAS when we see it's free
            if self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }
        SpinLockGuard { lock: self }
    }

    #[allow(dead_code)]
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        // Only attempt CAS
        if self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(SpinLockGuard { lock: self })
        } else {
            None
        }
    }
}

pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<'a, T> Deref for SpinLockGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.value.get() }
    }
}

impl<'a, T> DerefMut for SpinLockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<'a, T> Drop for SpinLockGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}
