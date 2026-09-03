//! Spinlocks
//!
//! The IrqSpinLock disables interrupts for the duration that the lock is held.
//! It is a fair (ordered) and pure ticket lock with two operations:
//! - Acquire: `my_ticket = next_ticket.fetch_add(1)` draws the new ticket; spin until now_serving == my_ticket
//! - Release (Drop): now_serving.fetch_add(1) — the single mutation of now_serving.
//!
//! The state of the lock is found by comparing the lock's ticket atomics. `now_serving` is the ticket
//! being served (or waiting to be served), while `next_ticket` is the next ticket to be issued
//! - Lock held: now_serving == my_ticket;
//! - Lock free: now_serving == next_ticket
//!
//! SpinLock does not disable interrupts. It is also unfair in its selection of
//! the next lock holder and uses a single atomic boolean for the lock.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::arch::interrupts;

//-------------------------------------------------------------------------
//
//  IrqSpinLock
//
//-------------------------------------------------------------------------

/// SpinLock which disables interrupts and restores on exit
pub struct IrqSpinLock<T> {
    next_ticket: AtomicU8,
    now_serving: AtomicU8,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for IrqSpinLock<T> {}
unsafe impl<T: Send> Send for IrqSpinLock<T> {}

impl<T> IrqSpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            next_ticket: AtomicU8::new(0), // The ticket the next caller will draw
            now_serving: AtomicU8::new(0), // The current ticket that is being served
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> IrqSpinLockGuard<'_, T> {
        // Disable interrupts before trying to take the lock
        let prev_interrupt_status = interrupts::disable();
        // Take a ticket
        let my_ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        // Spin while the lock is held elsewhere
        while my_ticket != self.now_serving.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
        IrqSpinLockGuard {
            lock: self,
            prev_interrupt_status,
        }
    }

    pub fn try_lock(&self) -> Option<IrqSpinLockGuard<'_, T>> {
        // Check if the queue is busy
        let my_ticket = self.next_ticket.load(Ordering::Relaxed);
        if my_ticket != self.now_serving.load(Ordering::Acquire) {
            return None;
        }
        // Disable interrupts before attempting to claim the ticket
        let prev_interrupt_status = interrupts::disable();
        // Try to take the next ticket
        if self
            .next_ticket
            .compare_exchange_weak(
                my_ticket,
                my_ticket.wrapping_add(1),
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_err()
        {
            interrupts::restore(prev_interrupt_status);
            return None;
        }
        Some(IrqSpinLockGuard {
            lock: self,
            prev_interrupt_status,
        })
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
        self.lock.now_serving.fetch_add(1, Ordering::Release);
        // Enable interrupts if previously enabled
        interrupts::restore(self.prev_interrupt_status);
    }
}

//-------------------------------------------------------------------------
//
//  SpinLock
//
//-------------------------------------------------------------------------

/// SpinLock that leaves interrupts enabled
/// This is an unfair lock
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
