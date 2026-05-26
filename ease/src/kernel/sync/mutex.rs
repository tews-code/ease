//! Mutex

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU8, Ordering};

use super::SpinLock;

use crate::kernel::sched::ThreadHandle;

//-------------------------------------------------------------------------
//
//  Mutex
//
//-------------------------------------------------------------------------

const FREE: u8 = MutexState::Free as u8;
const LOCKED: u8 = MutexState::Locked as u8;
const LOCKED_WITH_WAITERS: u8 = MutexState::LockedWithWaiters as u8;

#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
enum MutexState {
    Free = 0,
    Locked = 1,
    LockedWithWaiters = 2,
}

pub struct Mutex<T> {
    state: AtomicU8, // 0 - unlocked; 1 - locked no waiters; 2 - locked with waiter
    data: UnsafeCell<T>,
    waiters: SpinLock<Option<ThreadHandle>>,
}

#[cfg(target_os = "none")]
impl<T> Mutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            state: AtomicU8::new(0),
            data: UnsafeCell::new(data),
            waiters: SpinLock::new(None),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        const SPIN_MAX: usize = 100;
        // CAS, spin a bit, then block
        let mut count = 0;
        loop {
            if self
                .state
                .compare_exchange_weak(
                    MutexState::Free as u8,
                    MutexState::Locked as u8,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                // We got the lock, no need to block
                return MutexGuard { lock: self };
            } else {
                count += 1;
                if count == SPIN_MAX {
                    break;
                }
            }
            core::hint::spin_loop();
        }
        // No luck, time to block
        let mut head = self.waiters.lock();
        // Check state
        loop {
            match self.state.load(Ordering::Relaxed) {
                FREE => {
                    // Let's try to take this lock
                    if self
                        .state
                        .compare_exchange_weak(
                            MutexState::Free as u8,
                            MutexState::Locked as u8,
                            Ordering::Acquire,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        // Successfully caught the lock this time, unwind and return the mutex guard
                        drop(head);
                        return MutexGuard { lock: self };
                    }
                }
                LOCKED => {
                    // Let's add a waiter
                    if self
                        .state
                        .compare_exchange_weak(
                            MutexState::Locked as u8,
                            MutexState::LockedWithWaiters as u8,
                            Ordering::Acquire,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        // Successfully changed state to having waiters
                        break;
                    }
                }
                LOCKED_WITH_WAITERS => {
                    // We need to block the thread
                    break;
                }
                _ => {
                    unreachable!("should not reach this lock state");
                }
            }
            core::hint::spin_loop();
        }
        // Now block
        // Enqueue self at the head of the waiter list
        let curr_handle = crate::kernel::sched::current_thread();
        crate::kernel::sched::set_next_waiter(&curr_handle, *head);
        *head = Some(curr_handle);

        // Set the state to Blocked - in case a thread has called Drop in the mean time
        crate::kernel::sched::set_self_blocked();
        drop(head);
        crate::kernel::sched::park_if_blocked();
        // Add an Acquire fence (matched by Release fence in Drop)
        core::sync::atomic::fence(Ordering::Acquire);
        MutexGuard { lock: self }
    }
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

#[must_use = "if unused, the lock is released immediately"]
pub struct MutexGuard<'a, T> {
    lock: &'a Mutex<T>,
}

impl<'a, T> Deref for MutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &(*self.lock.data.get()) }
    }
}

impl<'a, T> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut (*self.lock.data.get()) }
    }
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        if self
            .lock
            .state
            .compare_exchange(
                MutexState::Locked as u8,
                MutexState::Free as u8,
                Ordering::Release,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            return;
        }
        // Pop first thread from the list
        let mut head = self.lock.waiters.lock();
        let popped = head
            .take()
            .expect("there should be waiters in the blocked list");
        let next_waiter_in_list = crate::kernel::sched::get_next_waiter(&popped);
        crate::kernel::sched::set_next_waiter(&popped, None);
        *head = next_waiter_in_list;
        // Change the state
        if next_waiter_in_list.is_none() {
            self.lock
                .state
                .store(MutexState::Locked as u8, Ordering::Release);
        }
        core::sync::atomic::fence(Ordering::Release);
        drop(head);
        crate::kernel::sched::unpark(&popped);
    }
}
