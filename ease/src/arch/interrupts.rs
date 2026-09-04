//! Interrupt control for each Hart
//!
//! These three functions are the only software doors through which `mstatus.MIE`
//! changes, so under the `irqsoff` feature they are also where interrupts-off
//! sections are opened and closed (see [`crate::kernel::irqsoff`]).

use super::csr;
#[cfg(feature = "irqsoff")]
use crate::kernel::irqsoff::{self, Site};
#[cfg(feature = "irqsoff")]
use core::panic::Location;

/// Enables machine-wide interrupts for this hart
#[cfg_attr(feature = "irqsoff", track_caller)]
pub(crate) fn enable() {
    // Close the section before re-enabling, so a trap landing straight after
    // `csrsi` opens a fresh one. A redundant enable (MIE already set) closes
    // nothing: any pending open is a dangling one from an invisible close.
    #[cfg(feature = "irqsoff")]
    if !enabled() {
        irqsoff::close(Site::At(Location::caller()));
    }
    unsafe {
        // Write mstatus to set MIE
        core::arch::asm!("csrsi mstatus, {}", const csr::mstatus::MIE);
    }
}

/// Disables interrupts
///
/// - Returns prior machine status
#[cfg_attr(feature = "irqsoff", track_caller)]
pub(crate) fn disable() -> usize {
    let mstatus: usize;
    unsafe {
        // Use csrrc to atomically read mstatus and clear MIE (bit 3)
        core::arch::asm!("csrrc {}, mstatus, {}", out(reg) mstatus, const csr::mstatus::MIE);
    }
    // Outermost if MIE was set: this call is what turned interrupts off
    #[cfg(feature = "irqsoff")]
    if mstatus & csr::mstatus::MIE != 0 {
        irqsoff::open(Site::At(Location::caller()));
    }
    mstatus
}

/// Enables interrupts if previously enabled
///
/// - `prev` is the previous machine status register
///
/// This is a no-op if interrupts were already disabled (handles nested locks correctly).
#[cfg_attr(feature = "irqsoff", track_caller)]
pub(crate) fn restore(prev: usize) {
    #[cfg(feature = "irqsoff")]
    restore_at(prev, Location::caller());
    #[cfg(not(feature = "irqsoff"))]
    if prev & csr::mstatus::MIE != 0 {
        unsafe {
            core::arch::asm!("csrsi mstatus, {}", const csr::mstatus::MIE);
        }
    }
}

/// [`restore`] with an explicit closing site, for guards whose `Drop` cannot
/// name the caller that released them: they report their lock site instead.
#[cfg(feature = "irqsoff")]
pub(crate) fn restore_at(prev: usize, site: &'static Location<'static>) {
    // Outermost if MIE was set before: this call is what turns interrupts on.
    // Close before `csrsi` so a trap landing straight after opens a fresh one.
    if prev & csr::mstatus::MIE != 0 {
        irqsoff::close(Site::At(site));
        unsafe {
            core::arch::asm!("csrsi mstatus, {}", const csr::mstatus::MIE);
        }
    }
}

/// Checks if interrupts are enabled
#[cfg_attr(not(feature = "irqsoff"), allow(dead_code))]
pub(crate) fn enabled() -> bool {
    let mstatus: usize;
    unsafe { core::arch::asm!("csrr {}, mstatus", out(reg) mstatus) };
    mstatus & csr::mstatus::MIE != 0
}

/// Wait for interrupts
///
/// `wfi` returns once an enabled interrupt is pending even when MIE is clear,
/// which is how the idle thread sleeps inside a critical section. That time is
/// idle, not latency, so under `irqsoff` the section closes before the wait
/// and a fresh one opens after it: the wake-to-enable tail is what gets measured.
pub(crate) fn wait_for_interrupt() {
    #[cfg(feature = "irqsoff")]
    irqsoff::close(Site::Wfi);
    unsafe { core::arch::asm!("wfi") };
    #[cfg(feature = "irqsoff")]
    irqsoff::open(Site::Wfi);
}
