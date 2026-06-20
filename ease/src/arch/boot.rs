//! Boot code for QEMU virt machine
//!
//! Contains early startup: stack init, BSS zeroing, jump to main.
//! Fixed at two HARTs

use core::arch::{global_asm, naked_asm};
use core::mem::offset_of;

// # Safety
// Symbols are defined in the linker script and mark aligned addresses
unsafe extern "C" {
    static __data_start: u8;
    static __data_end: u8;
    static __data_lma: u8;

    static __bss_start: u8;
    static __bss_end: u8;

    static __sram8_text_start: u8;
    static __sram8_text_end: u8;
    static __sram8_text_lma: u8;

    static __hart0_irq_stack_base: u8;
    static __hart0_irq_stack_top: u8;
    static __hart0_idle_stack_base: u8;
    static __hart0_idle_stack_top: u8;
    static __hart0_percpu_start: u8;
    static __hart0_percpu_end: u8;

    static __sram9_text_start: u8;
    static __sram9_text_end: u8;
    static __sram9_text_lma: u8;

    static __hart1_irq_stack_base: u8;
    static __hart1_irq_stack_top: u8;
    static __hart1_idle_stack_base: u8;
    static __hart1_idle_stack_top: u8;
    static __hart1_percpu_start: u8;
    static __hart1_percpu_end: u8;
}

// Copy memory region
// Safety: Caller must ensure that the stack is set up with a valid stack pointer
extern "C" fn copy_region(lma_addr: usize, vma_start_addr: usize, vma_end_addr: usize) {
    let region_size_bytes = vma_end_addr - vma_start_addr;
    if region_size_bytes > 0 {
        // Safety: LMA and VMA addresses are aligned; LMA is availble for reads and VMA is suitable for writes
        unsafe {
            core::ptr::copy_nonoverlapping(
                lma_addr as *const u8,
                vma_start_addr as *mut u8,
                region_size_bytes,
            );
        }
    }
}

// Zero a memory region
// Safety: Caller must ensure that the stack is set up with a valid stack pointer
extern "C" fn zero_region(start_addr: usize, end_addr: usize) {
    let region_size_bytes = end_addr - start_addr;
    if region_size_bytes > 0 {
        // Safety: Start and end addresses are aligned; region is suitable for writes
        unsafe {
            core::ptr::write_bytes(start_addr as *mut u8, 0, region_size_bytes);
        }
    }
}

// Hart 1 waiting on doorbell from Hart 0
// Using naked_asm as we do not yet have a stack pointer
#[unsafe(naked)]
extern "C" fn wait_on_doorbell() {
    naked_asm!(
        "li t0, {mie_MSIE}",
        "csrs mie, t0", // Enable software interrupts
        "1:",
            "wfi",  // WFI wakes on a pending interrupt regardless of global enable
            "csrr t0, mip",
            "li t1, {mip_MSIP}",
            "and t0, t0, t1",
            "bnez t0, 2f",
            "j 1b",
        "2:",
            // Clear interrupt
            "li t0, {clint_hart1_msip}",
            "sw zero, 0(t0)",
            "csrw mie, zero",
            "ret",
            mie_MSIE = const crate::arch::csr::mie::MSIE,
            mip_MSIP = const crate::arch::csr::mip::MSIP,
            clint_hart1_msip = const crate::drivers::clint::clint_msip_addr(1),
    );
}

// Low energy park loop
// Note - using global_asm to ensure symbol alignment is 4 bytes for mtvec
global_asm!(
    r#"
    .section .text
    .global _park_loop
    .balign 4
    _park_loop:
        1:  wfi
            j 1b
    "#
);

unsafe extern "C" {
    fn _park_loop();
}

// QEMU virt does not support SIO FIFO. Using a BSS struct to pass the FIFO equivalent data to HART1
#[repr(C)]
pub struct LaunchMailbox {
    sp: usize,
    mtvec: usize,
    entry: usize,
}

// Throughout this kernel we avoid `static mut` as it leads to UB; however this
// static is only addressed through assembly at launch
static mut LAUNCH_MAILBOX: LaunchMailbox = LaunchMailbox {
    sp: 0,
    mtvec: 0,
    entry: 0,
};

#[unsafe(link_section = ".text.init")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
#[allow(clippy::identity_op)]
extern "C" fn _start() -> ! {
    naked_asm!(
        r#" csrw mie, zero                  # Disable interrupts
            csrw mstatus, zero

            csrr t0, mhartid                # Read HART ID
            bnez t0, hart1                  # Set up HART1

            hart0:

            # HART0 setup
            # Set up a temporary trap vector - will not recover
            la t0, {park_loop}
            csrw mtvec, t0"#,

            #[cfg(feature = "paint-stack")]
        r#" # Paint the IRQ stacks
            la a0, {hart0_irq_stack_base}
            la a1, {hart0_irq_stack_top}
            call {paint_stack}

            la a0, {hart1_irq_stack_base}
            la a1, {hart1_irq_stack_top}
            call {paint_stack}

            # Paint the idle/boot stacks before using the stack pointer
            la a0, {hart0_idle_stack_base}
            la a1, {hart0_idle_stack_top}
            call {paint_stack}

            la a0, {hart1_idle_stack_base}
            la a1, {hart1_idle_stack_top}
            call {paint_stack}"#,

        r#" # Set the stack canaries
            la a0, {hart0_irq_stack_base}
            call {set_canary}
            la a0, {hart0_idle_stack_base}
            call {set_canary}
            la a0, {hart1_irq_stack_base}
            call {set_canary}
            la a0, {hart1_idle_stack_base}
            call {set_canary}

            # Set the idle/boot stack pointer
            la sp, {hart0_idle_stack_top}

            # Copy .data from LMA to VMA
            la a0, {data_lma}
            la a1, {data_start}
            la a2, {data_end}
            call {copy_region}

            # Zero BSS segment
            la a0, {bss_start}
            la a1, {bss_end}
            call {zero_region}

            # Lock bottom of RAM against near-null pointer deferences for HART0
            call {protect_null_ptr_deref}

            # Copy scratch RAM text from LMA to VMA
            # Copy for HART0
            la a0, {sram8_text_lma}
            la a1, {sram8_text_start}
            la a2, {sram8_text_end}
            call {copy_region}

            # Copy for HART1
            la a0, {sram9_text_lma}
            la a1, {sram9_text_start}
            la a2, {sram9_text_end}
            call {copy_region}

            # Lock the scratch RAM text region with PMP for HART0
            la a0, {sram8_text_start}
            la a1, {sram8_text_end}
            call {protect_sram_text}

            # Zero PerCpu
            la a0, {hart0_percpu_start}
            la a1, {hart0_percpu_end}
            call {zero_region}

            la a0, {hart1_percpu_start}
            la a1, {hart1_percpu_end}
            call {zero_region}

            # Store the IRQ stack top in mscratch
            la t0, {hart0_irq_stack_top}
            csrw mscratch, t0

            # Set up trap vector
            la t0, _trap_vector_h0
            csrw mtvec, t0

            # Pass secondary hart details - for QEMU virt we use
            # a struct in BSS and the CLINT MSIP instead of SIO FIFO
            la t0, {launch_mailbox}
            la a0, {hart1_idle_stack_top}
            sw a0, {launch_mailbox_sp}(t0)
            la a0, _trap_vector_h1
            sw a0, {launch_mailbox_mtvec}(t0)
            la a0, {secondary_main}
            sw a0, {launch_mailbox_entry}(t0)

            # Fence - store/release
            fence rw, w
            # Fence - for HART0 instruction fetch
            fence.i

            # Ring doorbell for HART1 - note without CLINT locking as yet
            li a0, 1
            la a1, {clint_hart1_msip}
            sw a0, 0(a1)

            call {main}

            unimp

        # Set up HART1
        hart1:

            # Set up a temporary trap vector - will not recover
            la t0, {park_loop}
            csrw mtvec, t0

            # Spin on doorbell (MSIP on QEMU virt board)
            call {wait_on_doorbell}

            # Fence with HART0 - load/acquire
            fence r, rw
            # Fence on copied instruction .text
            fence.i

            # Retrieve boot details
            la t0, {launch_mailbox}
            lw sp, {launch_mailbox_sp}(t0)
            lw t1, {launch_mailbox_mtvec}(t0)
            mv s0, t1
            lw t2, {launch_mailbox_entry}(t0)
            mv s1, t2

            # Lock against near-null pointer deferences for HART1
            call {protect_null_ptr_deref}

            # Lock the text region with PMP for HART1
            la a0, {sram9_text_start}
            la a1, {sram9_text_end}
            call {protect_sram_text}

            # Store the IRQ stack top in mscratch
            la t0, {hart1_irq_stack_top}
            csrw mscratch, t0

            # Set the trap vector
            csrw mtvec, s0

            # Jump to entry
            jr s1

            unimp"#,

        park_loop = sym _park_loop,
        #[cfg(feature = "paint-stack")]
        paint_stack = sym crate::kernel::paintstack::paint_stack,
        set_canary = sym crate::kernel::paintstack::set_canary,
        copy_region = sym copy_region,
        zero_region = sym zero_region,
        wait_on_doorbell = sym wait_on_doorbell,

        launch_mailbox_sp = const offset_of!(LaunchMailbox, sp),
        launch_mailbox_mtvec = const offset_of!(LaunchMailbox, mtvec),
        launch_mailbox_entry = const offset_of!(LaunchMailbox, entry),

        protect_null_ptr_deref = sym crate::arch::pmp::protect_null_ptr_deref,
        protect_sram_text = sym crate::arch::pmp::protect_sram_text,

        data_start = sym __data_start,
        data_end = sym __data_end,
        data_lma = sym __data_lma,

        bss_start = sym __bss_start,
        bss_end = sym __bss_end,

        sram8_text_lma = sym __sram8_text_lma,
        sram8_text_start = sym __sram8_text_start,
        sram8_text_end = sym __sram8_text_end,

        hart0_irq_stack_base = sym __hart0_irq_stack_base,
        hart0_irq_stack_top = sym __hart0_irq_stack_top,
        hart0_idle_stack_base = sym __hart0_idle_stack_base,
        hart0_idle_stack_top = sym __hart0_idle_stack_top,
        hart0_percpu_start = sym __hart0_percpu_start,
        hart0_percpu_end = sym __hart0_percpu_end,

        hart1_irq_stack_base = sym __hart1_irq_stack_base,
        hart1_irq_stack_top = sym __hart1_irq_stack_top,
        hart1_idle_stack_base = sym __hart1_idle_stack_base,
        hart1_idle_stack_top = sym __hart1_idle_stack_top,
        hart1_percpu_start = sym __hart1_percpu_start,
        hart1_percpu_end = sym __hart1_percpu_end,

        sram9_text_lma = sym __sram9_text_lma,
        sram9_text_start = sym __sram9_text_start,
        sram9_text_end = sym __sram9_text_end,

        launch_mailbox = sym LAUNCH_MAILBOX,
        clint_hart1_msip = const crate::drivers::clint::clint_msip_addr(1),
        main = sym crate::main,
        secondary_main = sym crate::secondary_main,
    );
}
