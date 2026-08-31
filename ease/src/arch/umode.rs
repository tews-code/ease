//! U-Mode threads
//!
//! U-Mode threads are spawned with a forged kernel stack that allows for the scheduler
//! to pick up the thread as if it had been a running thread suspended, and a forged trap
//! frame to allow an mret into U-Mode, as if it had been an existing U-Mode thread that
//! had trapped.
//!
//! User Thread Spawn
//!
//! User threads must be spawned by an existing thread running in M-mode. User mode threads
//! are always part of a user process, which is a user program (of unknown provenance) running
//! in U-mode with PMP enabled. A process has common memory map for .text, .data and .bss but
//! an individual user stack for each thread that is part of that process. At each switch the
//! PMP is updated to support the needed changes.
//!
//! In order to spawn a user thread, the user program must first be loaded from its initial
//! storage location to the active memory locations. EASE currently supports
//! - "flash" programs - compiled within EASE and loaded from LMA to VMA. See [crate::user].
//! - "blob" programs - written and compiled against the EASE user library and stored within EASE as a series of bytes.
//!
//! See [crate::user::PROGRAMS] for the list of available programs.
//!
//! A user thread spawn is handled by [sched::spawn_process] which takes the program by name.
//! If the program is found in the PROGRAMS table it is loaded into the right memory regions
//! in [sched::userloader]. The user program loader hence takes responsibility for getting the
//! required memory allocations, creating the user memory map (see [sched::usermem]), and
//! call the required `fence.i` for both HARTs, and returns the user entry point - for flash
//! programs this is the function address as a symbol, while for blob programs this is the very
//! first address in the .text segment.
//!
//! On successful load, the work is taken over by [sched::spawn::spawn_process] which takes
//! responsibility for selecting a process slot [sched::process::Procs], allocating the
//! user and kernel stacks, and acquiring a thread control block with a suitably forged stack.
//!
//! The forged stack is set up in the thread's kernel stack as two frames one above the other
//! (See [init_stack_for_user_thread]):
//!
//!         +-> +--kernel stack top-+
//!         |   |       mepc        |
//!         |   +-------------------+
//!     Forged  |      mstatus      |
//!     trap    +-------------------+
//!     frame   |       ...         |
//!         |   +-------------------+
//!         |   |       gp          |
//!         |   +-------------------+
//!         |   |       ra          |
//!         +-> +-------------------+
//!         |   |       s0          |
//!         |   +-------------------+
//!     Forged  |       s1          |
//!     context +-------------------+
//!     switch  |       ...         |
//!     frame   +-------------------+
//!         |   |       sp          |
//!         |   +-------------------+
//!         |   |       ra          |
//!    sp   +-> +-------------------+
//!             |      ...          |
//!             +-kernel stack base-+
//!
//! The context switch frame is forged [context::Frame::forge_for_user_entry] to point to [user_first_run].
//! In turn, [user_first_run] restores the forged trap frame [trap::Frame::forge_for_user_entry] to:
//! - Set `mepc` to the user entry address, as provided by the user loader.
//! - Set `mstatus` to make the `mret` command "return" to U-mode.
//! - Set the stack pointer to the *user* stack top.

use super::{context, umode};
use crate::arch::trap;
use crate::drivers::keyboard;
use crate::kernel::alloc::MemRegion;
use crate::kernel::sched::ExitReason;
use crate::kernel::sched::{self, post_switch_cleanup, userloader::UserEntry};
use crate::kernel::stack;
use core::arch::naked_asm;
use core::ptr::NonNull;
use ease_abi::syscall;

impl trap::Frame {
    /// Forge a trap frame in the thread's kernel stack'
    ///
    /// The trap return will accept the forged frame
    /// and "recover" this to the registers.
    ///
    /// Ahead of mret we set:
    /// - mepc to the user process entry address
    /// - mstatus.MPIE to 0 and mstatus.MPP to 00 (U)
    ///
    /// We also set the return address to `user_exit`;
    pub(crate) fn forge_for_user_entry(
        entry: UserEntry,
        user_stack_top: NonNull<u8>,
        user_exit: usize,
    ) -> Self {
        Self {
            ra: user_exit,
            mepc: entry.addr(),
            mstatus: 0, //  MPP=U, Note that MPIE is set to zero but interrupts are always enabled in u-mode
            sp: user_stack_top.addr().into(),
            ..Default::default()
        }
    }
}

impl context::Frame {
    /// Forge a context frame ready for the scheduler `switch_to` to restore
    ///
    /// For user threads we jump straight to [user_first_run]
    pub fn forge_for_user_entry() -> Self {
        Self {
            ra: umode::user_first_run as *const () as usize, // switch_to's ret lands in user_first_run; the trap frame above it holds the U-mode state"
            ..Self::default()
        }
    }
}
/// Forges a trap frame and context frame in the thread's kernel stack
/// Returns the stack pointer to base of the forged context frame, which is below
/// the forged trap frame.
///
/// # Safety #
/// - stack_base must be class.size()-aligned and point to writeable memory of at least class.size() bytes.
/// - user stack top must be the top of a live, U-mode-accessible memory region
pub unsafe fn init_stack_for_user_thread(
    kernel_stack: &mut MemRegion,
    user_stack_base: NonNull<u8>,
    user_stack_top: NonNull<u8>,
    entry: UserEntry,
    user_exit: usize,
) -> NonNull<u8> {
    debug_assert!(
        kernel_stack.size()
            > core::mem::size_of::<context::Frame>() + core::mem::size_of::<trap::Frame>(),
        "kernel stack memory region too small for context switch and trap return"
    );
    // Safety: kernel stack has aligned addresses and region is valid for writes
    unsafe {
        #[cfg(feature = "paint-stack")]
        {
            stack::paint(kernel_stack.base_addr(), kernel_stack.top().addr().into());
            stack::paint(user_stack_base.addr().into(), user_stack_top.addr().into());
        }
        stack::set_canary(kernel_stack.base_addr());
        stack::set_canary(user_stack_base.addr().into());
    }
    // First forge the trap return
    // Safety: trap_frame_ptr is derived from stack_base and aligned
    unsafe {
        // Set up a trap frame so trap returns to U-mode
        let trap_frame_ptr = kernel_stack
            .base()
            .as_ptr()
            .add(kernel_stack.size() - core::mem::size_of::<trap::Frame>())
            as *mut trap::Frame;
        core::ptr::write(
            trap_frame_ptr,
            trap::Frame::forge_for_user_entry(entry, user_stack_top, user_exit),
        );
    }
    // Now forge the context
    // Safety: context_ptr is derived from stack_base, and
    // aligned because sizeof(TrapFrame) + sizeof(Context) is a multiple of align(Context).
    let context_ptr = unsafe {
        kernel_stack
            .base()
            .add(
                kernel_stack.size()
                    - core::mem::size_of::<trap::Frame>()
                    - core::mem::size_of::<context::Frame>(),
            )
            .cast()
    };
    unsafe {
        context_ptr.write(context::Frame::forge_for_user_entry());
    }
    context_ptr.cast::<u8>()
}
/// User threads that exit via this function are faulting or voluntary exit.
/// Threads that are exited in `post_switch_cleanup` do not pass through this function.
/// It is possible for multiple threads in the same process running on different HARTs to
/// arrive in this function simultaneously.
///
/// It is entered via trap return from `exit_from_user`; `a0` carries the ExitReason.
///
/// # Panics #
/// Panics if the exit reason is unknown
pub(crate) extern "C" fn user_thread_exit(reason: usize) -> ! {
    let exit_reason = match reason {
        0 => ExitReason::Exit,
        1 => ExitReason::Fault,
        _ => panic!("unknown user thread exit reason"),
    };
    sched::exit_user_thread(exit_reason);
}
/// Handles blocking system calls for user threads.
/// Checks if the user thread should be exited and performs exit call.
///
/// # Panics #
/// Panics if the system call number is unknown
pub(crate) extern "C" fn user_thread_block(
    return_address: usize,
    user_sp: usize,
    syscall: usize,
) -> ! {
    match syscall {
        syscall::GET_CHAR => {
            // GET_CHAR holds nothing across its waits, so exit-on-the-spot is legal here
            sched::exit_user_thread_if_needs_exit();
            loop {
                if let Some(b) = keyboard::read_key() {
                    resume_user(0, b, return_address, user_sp);
                } else {
                    // Block using a completion on the key press
                    keyboard::KEY_PENDING.wait();
                }
                sched::exit_user_thread_if_needs_exit();
            }
        }
        _ => panic!("unexpected blocking syscall: {}", syscall),
    }
}
/// Sets up user thread for first run
#[unsafe(naked)]
pub extern "C" fn user_first_run() -> ! {
    naked_asm!(
        "call {post_switch_cleanup}",
        // Set up mepc and mstatus
        "lw t0,  4 * 30(sp)",
        "csrw mepc, t0",
        "lw t0,  4 * 31(sp)",
        "csrw mstatus, t0",

        // Load GP registers from forged trap frame in thread's kernel stack
        "lw ra,  4 *  0(sp)",
        "lw gp,  4 *  1(sp)",
        "lw tp,  4 *  2(sp)",
        "lw t0,  4 *  3(sp)",
        "lw t1,  4 *  4(sp)",
        "lw t2,  4 *  5(sp)",
        "lw t3,  4 *  6(sp)",
        "lw t4,  4 *  7(sp)",
        "lw t5,  4 *  8(sp)",
        "lw t6,  4 *  9(sp)",
        "lw a0,  4 * 10(sp)",
        "lw a1,  4 * 11(sp)",
        "lw a2,  4 * 12(sp)",
        "lw a3,  4 * 13(sp)",
        "lw a4,  4 * 14(sp)",
        "lw a5,  4 * 15(sp)",
        "lw a6,  4 * 16(sp)",
        "lw a7,  4 * 17(sp)",
        "lw s0,  4 * 18(sp)",
        "lw s1,  4 * 19(sp)",
        "lw s2,  4 * 20(sp)",
        "lw s3,  4 * 21(sp)",
        "lw s4,  4 * 22(sp)",
        "lw s5,  4 * 23(sp)",
        "lw s6,  4 * 24(sp)",
        "lw s7,  4 * 25(sp)",
        "lw s8,  4 * 26(sp)",
        "lw s9,  4 * 27(sp)",
        "lw s10, 4 * 28(sp)",
        "lw s11, 4 * 29(sp)",

        // Load sp from frame
        "lw sp, 4 * 32(sp)",

        "mret",
        post_switch_cleanup = sym post_switch_cleanup,
        // num_slots = const crate::arch::trap::NUM_SLOTS,
    );
}
/// Return to user thread from M mode
///
/// `error` is returned in `a0` with 0 indicating success
/// `value` is returned in `a1`
#[unsafe(naked)]
pub extern "C" fn resume_user(
    error: usize,
    value: usize,
    resume_address: usize,
    user_sp: usize,
) -> ! {
    naked_asm!(
        // Note that `error` is already in a0 and value is already in a1 as they are the first function arguments
        // Set up stack pointer
        "mv sp, a3",
        "li t0, {mstatus_MIE}", //Ensure interrupts are disabled so this asm doesn't get interrupted
        "csrc mstatus, t0",
        "li t0, {mstatus_MPP}", // Ensure mret retuns to U-mode
        "csrc mstatus, t0",
        "li t0, {mstatus_MPIE}",// Not strictly required (as overwritten by next trap) but matches forged context
        "csrc mstatus, t0",
        // Set the return address to the user thread
        "csrw mepc, a2",
        "mret",
        mstatus_MIE = const crate::arch::csr::mstatus::MIE,
        mstatus_MPP = const crate::arch::csr::mstatus::MPP,
        mstatus_MPIE = const crate::arch::csr::mstatus::MPIE
    );
}
