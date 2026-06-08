//! Stack

/// Sentinel placed at the bottom word of each stack.
/// Checked by the scheduler / panic path to detect stack overflow.
pub(crate) const STACK_CANARY: usize = 0xDEAD_BEEF;

/// Stack paint pattern
pub(crate) const STACK_PAINT_PATTERN: usize = 0x5A5A5A5A;

/// Check the canary at stack base
///
/// Caller must ensure that base_addr is a valid stack base (16 byte aligned)
pub(crate) fn canary_is_ok(base_addr: usize) -> Result<(), usize> {
    // Safety: Caller has provided aligned address safe for reading
    let val = unsafe { core::ptr::read_volatile(base_addr as *const usize) };
    if val == STACK_CANARY {
        Ok(())
    } else {
        Err(val)
    }
}

/// Check the stack paint high water mark
/// - Returns the lowest address written on a descending stack, or `end_addr` if the stack was never used.
/// - Returns None if stack canary not found
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
