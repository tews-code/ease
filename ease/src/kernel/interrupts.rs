//! Handle interrupts
#[cfg(target_os = "none")]
use crate::arch::interrupts;
#[cfg(target_os = "none")]
use core::marker::PhantomData;

/// Zero-sized proof that interrupts are disabled.
/// Private constructor — only `with_interrupts_disabled` can create one.
#[cfg(target_os = "none")]
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct CriticalSection<'cs> {
    _lifetime: PhantomData<&'cs ()>, // lifetime linked to struct existence
}

#[cfg(target_os = "none")]
impl<'cs> CriticalSection<'cs> {
    // # Safety
    // Interrupts must be disabled for the duration of 'cs.
    unsafe fn new() -> Self {
        Self {
            _lifetime: PhantomData,
        }
    }
}

/// Runs the closure with interrupts disabled, providing a `CriticalSection` token
/// as proof. Interrupts are restored to their previous state when the closure returns.
#[cfg(target_os = "none")]
#[cfg_attr(feature = "irqsoff", track_caller)] // irqsoff attributes the section to our caller
pub fn with_interrupts_disabled<F, R>(f: F) -> R
where
    F: FnOnce(CriticalSection<'_>) -> R,
{
    let prev = interrupts::disable();
    let result = f(unsafe { CriticalSection::new() });
    interrupts::restore(prev);

    result
}
