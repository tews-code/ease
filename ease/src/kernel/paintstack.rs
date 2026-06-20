//! Stack

use core::{arch::naked_asm, ptr::NonNull};

use crate::kernel::alloc::MemRegion;

/// Sentinel placed at the bottom word of each stack.
/// Checked by the scheduler / panic path to detect stack overflow.
pub(crate) const STACK_CANARY: usize = 0xDEAD_BEEF;

#[allow(dead_code)]
#[repr(align(16))]
struct Stack {
    region: MemRegion,
    sp: NonNull<u8>,
}

#[allow(dead_code)]
impl Stack {
    fn set_stack_canary(&self) {
        unsafe { set_canary(self.region.base_addr()) };
    }

    #[cfg(feature = "paint-stack")]
    fn paint_full_stack(&self) {
        unsafe { paint_stack(self.region.base_addr(), self.region.top()) };
    }
}

/// Add a canary at the bottom of a stack
/// Using naked_asm as we may not yet have a stack pointer
///
/// Safety: Caller must ensure stack base is aligned and safe for writing
#[unsafe(naked)]
pub(crate) unsafe extern "C" fn set_canary(base_addr: usize) {
    naked_asm!(
        "li t0, {canary}",
        "sw t0, 0(a0)",
        "ret",
        canary = const STACK_CANARY,
    );
}

/// Check the canary at stack base
///
/// Safety: Caller must ensure that base_addr is a valid stack base
pub(crate) unsafe fn check_canary(base_addr: usize) -> Result<(), usize> {
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

/// Size of each step
#[cfg(feature = "paint-stack")]
const STEP: usize = core::mem::size_of::<usize>();

/// Paint the stack
/// Using naked_asm as we may not yet have a stack pointer
///
/// Safety: Caller must ensure base and top addresses are aligned and region is safe for writing
#[unsafe(naked)]
#[cfg(feature = "paint-stack")]
pub(crate) unsafe extern "C" fn paint_stack(base_addr: usize, top_addr: usize) {
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
///
/// Safety: Caller must ensure base and top addresses are aligned and region is valid for reading
#[cfg(feature = "paint-stack")]
pub(crate) unsafe fn stack_high_watermark(base_addr: usize, top_addr: usize) -> Option<usize> {
    // Safety: caller guarantees base address is aligned
    if unsafe { check_canary(base_addr) }.is_err() {
        return None;
    }
    for addr in (base_addr + STEP..top_addr).step_by(STEP) {
        // Safety: caller guarantees the region is mapped and aligned.
        let val = unsafe { core::ptr::read_volatile(addr as *const usize) };
        if val != STACK_PAINT_PATTERN {
            return Some(addr); // Started measuring after canary at base address
        }
    }
    Some(top_addr)
}

/// Print the stack high watermark details
/// Uses dprint to avoid locking
///
/// Safety: Caller must ensure base and top addresses are aligned and valid for reading
#[cfg(feature = "paint-stack")]
pub(crate) unsafe fn print_stack_watermark(
    name: &'static str,
    id: usize,
    base_addr: usize,
    top_addr: usize,
) {
    dprintln!("==== {}{} Stack High Watermark Check ====", name, id);
    // Safety: Caller has ensured base address and top address are aligned and valid for reading
    unsafe {
        if let Some(addr) = stack_high_watermark(base_addr, top_addr) {
            dprintln!("Start address: {base_addr:x}");
            dprintln!("High watermark address: {addr:x}");
            dprintln!("Top address: {top_addr:x}");
            dprintln!("Bytes unused: {}", addr - base_addr - STEP);
        } else {
            dprintln!(" * STACK CORRUPT * ");
        }
    }
    dprintln!(
        "==== {}{} Stack High Watermark Check Complete ====",
        name,
        id
    );
}

// Display stack depth used
#[cfg(feature = "paint-stack")]
pub(crate) fn print_irq_idle_stacks() {
    // Safety: Linker ensures stack addresses are aligned and available for reads
    unsafe {
        unsafe extern "C" {
            static __hart0_irq_stack_base: u8;
            static __hart0_irq_stack_top: u8;
            static __hart1_irq_stack_base: u8;
            static __hart1_irq_stack_top: u8;
            static __hart0_idle_stack_base: u8;
            static __hart0_idle_stack_top: u8;
            static __hart1_idle_stack_base: u8;
            static __hart1_idle_stack_top: u8;
        }

        use crate::kernel::stack::print_stack_watermark;

        print_stack_watermark(
            "IRQ Hart",
            0,
            &raw const __hart0_irq_stack_base as usize,
            &raw const __hart0_irq_stack_top as usize,
        );
        print_stack_watermark(
            "IRQ Hart",
            1,
            &raw const __hart1_irq_stack_base as usize,
            &raw const __hart1_irq_stack_top as usize,
        );
        print_stack_watermark(
            "Idle Hart",
            0,
            &raw const __hart0_idle_stack_base as usize,
            &raw const __hart0_idle_stack_top as usize,
        );
        print_stack_watermark(
            "Idle Hart",
            1,
            &raw const __hart1_idle_stack_base as usize,
            &raw const __hart1_idle_stack_top as usize,
        );
    }
}
