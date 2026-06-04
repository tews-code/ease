//! Thread stack creation

use core::ptr::NonNull;

use crate::arch::context::Context;
use crate::arch::trap::TrapFrame;
use crate::kernel::alloc::MemRegion;

use super::STACK_CANARY;
use super::STACK_PAINT_PATTERN;

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

// Forges a thread Context for a kernel thread
// The thread entry function is stored in s0
// Returns the stack pointer
// Safety: stack_base must be class.size()-aligned and point to
// writeable memory of at least class.size() bytes
pub unsafe fn init_for_kernel_entry(
    stack: &mut MemRegion,
    closure_run: extern "C" fn(*mut u8) -> !,
    closure_ptr: *mut u8,
) -> Option<NonNull<u8>> {
    debug_assert!(
        stack.size() > core::mem::size_of::<Context>(),
        "stack memory region too small for context switch"
    );
    // Safety: Caller has provided a valid stack base pointer
    unsafe {
        core::ptr::write(stack.base().as_ptr() as *mut usize, STACK_CANARY);
    }
    let context_ptr = unsafe {
        stack
            .base()
            .as_ptr()
            .add(stack.size() - core::mem::size_of::<Context>()) as *mut Context
    };
    // Safety: context_ptr is derived from stack_base, and
    // aligned because sizeof(Context) is a multiple of align(Context).
    unsafe {
        *context_ptr = Context::init_for_kernel_entry(closure_run, closure_ptr);
    }

    NonNull::new(context_ptr as *mut u8)
}

// Forges a thread Context for a user thread
// Returns the stack pointer
// Safety:
// - stack_base must be class.size()-aligned and point to
// writeable memory of at least class.size() bytes.
// - user stack top must be the top of a live, U-mode-accessible region
pub unsafe fn init_for_user_entry(
    kernel_stack: &mut MemRegion,
    user_entry: extern "C" fn(),
    user_stack_top: NonNull<u8>,
) -> Option<NonNull<u8>> {
    debug_assert!(
        kernel_stack.size() > core::mem::size_of::<Context>() + core::mem::size_of::<TrapFrame>(),
        "kernel stack memory region too small for context switch and trap return"
    );
    // Set up a stack canary
    // Safety: Caller has provided a valid stack base pointer
    unsafe {
        core::ptr::write(kernel_stack.base().as_ptr() as *mut usize, STACK_CANARY);
    }
    // Safety: trap_frame_ptr is derived from stack_base and aligned
    unsafe {
        // Set up a trap frame so trap return arrives in U-mode
        let trap_frame_ptr = kernel_stack
            .base()
            .as_ptr()
            .add(kernel_stack.size() - core::mem::size_of::<TrapFrame>())
            as *mut TrapFrame;
        core::ptr::write_bytes(trap_frame_ptr, 0, 1); // First zero
        (*trap_frame_ptr).ra = crate::user::user_exit as *const () as usize;
        (*trap_frame_ptr).mepc = user_entry as usize;
        (*trap_frame_ptr).mstatus = 0; //  MPP=U, MPIE=0. later step will enable interrupts in U-mode
        (*trap_frame_ptr).user_sp = user_stack_top.addr().into();
    }

    // Set up a switch context
    // Safety: context_ptr is derived from stack_base, and
    // aligned because sizeof(TrapFrame) + sizeof(Context) is a multiple of align(Context).
    let context_ptr = unsafe {
        kernel_stack.base().as_ptr().add(
            kernel_stack.size()
                - core::mem::size_of::<TrapFrame>()
                - core::mem::size_of::<Context>(),
        ) as *mut Context
    };
    unsafe {
        *context_ptr = Context::init_for_user_entry();
    }

    NonNull::new(context_ptr as *mut u8) // Return pointer to the context which is below the trap frame
}
