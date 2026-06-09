//! Stack

use core::arch::naked_asm;

/// Sentinel placed at the bottom word of each stack.
/// Checked by the scheduler / panic path to detect stack overflow.
pub(crate) const STACK_CANARY: usize = 0xDEAD_BEEF;

/// Add a canary at the bottom of a stack
/// Using naked_asm as we may not yet have a stack pointer
///
/// Safety: Caller must ensure stack base is aligned and safe for writing
#[unsafe(naked)]
pub(crate) extern "C" fn set_canary(base_addr: usize) {
    naked_asm!(
        "li t0, {canary}",
        "sw t0, 0(a0)",
               "ret",
               canary = const STACK_CANARY,
    );
}

/// Check the canary at stack base
///
/// Safety: Caller must ensure that base_addr is a valid stack base (16 byte aligned)
pub(crate) fn check_canary(base_addr: usize) -> Result<(), usize> {
    // Safety: Caller has provided aligned address safe for reading
    let val = unsafe { core::ptr::read_volatile(base_addr as *const usize) };
    if val == STACK_CANARY {
        Ok(())
    } else {
        Err(val)
    }
}

/// Stack paint pattern
#[cfg(feature = "paint-stack")]
pub(crate) const STACK_PAINT_PATTERN: usize = 0x5A5A5A5A;

/// Paint the stack
/// Using naked_asm as we may not yet have a stack pointer
///
/// Safety: Caller must ensure memory region is aligned and safe for writing
#[unsafe(naked)]
#[cfg(feature = "paint-stack")]
pub(crate) extern "C" fn paint_stack(base_addr: usize, top_addr: usize) {
    naked_asm!(
        "li t0, {pattern}",
        "1:",
            "bgeu a0, a1, 2f",
            "sw t0, 0(a0)",
            "addi a0, a0, 4",
            "j 1b",
        "2:",
            "ret",
        pattern = const STACK_PAINT_PATTERN,
    );
}

/// Check the stack paint high water mark
/// - Returns the lowest address written on a descending stack, or `end_addr` if the stack was never used.
/// - Returns None if stack canary not found
#[cfg(feature = "paint-stack")]
pub(crate) fn stack_high_watermark(start_addr: usize, end_addr: usize) -> Option<usize> {
    let step = core::mem::size_of::<usize>();
    // Safety: caller guarantees the region is mapped and aligned.
    let val = unsafe { *(start_addr as *const usize) };
    if val != STACK_CANARY {
        return None;
    }
    for addr in (start_addr + step..end_addr).step_by(step) {
        // Safety: caller guarantees the region is mapped and aligned.
        let val = unsafe { *(addr as *const usize) };
        if val != STACK_PAINT_PATTERN {
            return Some(addr);
        }
    }
    Some(end_addr)
}

/// Print the stack high watermark details
/// Uses dprint to avoid locking
#[cfg(feature = "paint-stack")]
pub(crate) fn print_stack_watermark(
    name: &'static str,
    id: usize,
    start_addr: usize,
    end_addr: usize,
) {
    dprintln!("==== {}{} Stack High Watermark Check ====", name, id);
    if let Some(addr) = stack_high_watermark(start_addr, end_addr) {
        dprintln!("Start address: {start_addr:x}");
        dprintln!("High watermark address: {addr:x}");
        dprintln!("Top address: {end_addr:x}");
        dprintln!("Bytes unused: {}", addr - start_addr);
    } else {
        dprintln!(" * STACK CORRUPT * ");
    };
    dprintln!("==== {}{} Stack High Watermark Check ====", name, id);
}
