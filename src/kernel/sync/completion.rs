//! Completion

use crate::kernel::sched::{
    ThreadHandle, current_thread, park_if_blocked, set_self_blocked, unpark,
};
use crate::kernel::sync::IrqSpinLock;

struct CompletionInner {
    pending: bool,                // Flag that gets set if signal() fires before any wait().
    waiter: Option<ThreadHandle>, // The single thread parked on this completion, if any.
}

pub struct Completion {
    inner: IrqSpinLock<CompletionInner>,
}

impl Completion {
    pub const fn new() -> Self {
        Self {
            inner: IrqSpinLock::new(CompletionInner {
                pending: false,
                waiter: None,
            }),
        }
    }

    pub fn signal(&self) {
        let mut inner = self.inner.lock();
        inner.pending = true;
        if let Some(handle) = inner.waiter.take() {
            drop(inner);
            unpark(&handle);
        }
    }

    pub fn wait(&self) {
        loop {
            let mut inner = self.inner.lock();
            if inner.pending {
                // Signal already fired
                inner.pending = false;
                drop(inner);
                return;
            } else {
                inner.waiter = Some(current_thread());
                set_self_blocked();
                drop(inner);
                park_if_blocked();
                // Clear waiter in case of spurious wake
                let mut inner = self.inner.lock();
                inner.waiter = None;
            }
        }
    }
}
