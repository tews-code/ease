//! Paint the stack

/// Sentinel placed at the bottom word of each thread's stack.
/// Checked by the scheduler / panic path to detect stack overflow.
pub const STACK_CANARY: usize = 0xDEAD_BEEF;

/// Stack paint pattern
pub const STACK_PAINT_PATTERN: usize = 0x5A5A5A5A;

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
