//! Trap handler for both interrupts and exceptions

use core::sync::atomic::Ordering;

use crate::arch::csr::mcause::exception::*;
use crate::arch::csr::mcause::interrupt::*;
use crate::arch::csr::mcause::{self, Trap};
use crate::arch::csr::mstatus;
use crate::arch::csr::{mepc, mtval};
use crate::arch::hart_id;
use crate::arch::trap::TrapFrame;
use crate::arch::usermode;
use crate::board;
use crate::drivers::{plic, uart, virtio};
use crate::kernel::paintstack::check_canary;
use crate::kernel::panic;
use crate::kernel::sched::ExitReason;
use crate::kernel::{ipi, percpu, sched};

#[cfg(feature = "profile")]
use ease_macros::profile;

unsafe extern "C" {
    static __hart0_irq_stack_base: u8;
    static __hart1_irq_stack_base: u8;
    fn preempt_trampoline_h0();
    fn preempt_trampoline_h1();
}

#[unsafe(link_section = ".sram8_text")]
#[cfg_attr(feature = "profile", profile)]
pub(crate) extern "C" fn trap_handler_h0(frame: &mut TrapFrame) {
    trap_handler_impl(frame);
}

#[unsafe(link_section = ".sram9_text")]
#[cfg_attr(feature = "profile", profile)]
pub(crate) extern "C" fn trap_handler_h1(frame: &mut TrapFrame) {
    trap_handler_impl(frame);
}

#[inline(always)]
fn trap_handler_impl(frame: &mut TrapFrame) {
    // Check if other hart has triggered a panic
    if panic::STOP.load(Ordering::Relaxed) {
        panic::PARKED.store(true, Ordering::Release);
        loop {
            crate::arch::interrupts::wait_for_interrupt();
            core::hint::spin_loop();
        }
    }
    // Check if IRQ stack canary is in place
    // Safety: Address is safe to read and aligned from linker script
    let irq_stack_base = if hart_id() == 0 {
        &raw const __hart0_irq_stack_base as *const usize
    } else {
        &raw const __hart1_irq_stack_base as *const usize
    };
    // Safety: IRQ stack base is a valid stack address from linker script
    if unsafe { check_canary(irq_stack_base.addr()) }.is_err() {
        irq_panic();
    }
    match mcause::read() {
        Trap::Interrupt(TIMER) => sched::mark_for_preempt(),
        Trap::Interrupt(SOFTWARE) => {
            ipi::clear_self();
            // Note - if the other hart raises an IPI at this point
            // it will be ignored until after this trap returns,
            // at which point it will trigger.
            //
            // Invariant: we always scan all of threads under lock after the IPI
            // Mark IPI receipt in the trace so we can tell a delivered-but-no-
            // preempt from a never-sent kick during a wake stall.
            #[cfg(feature = "trace")]
            crate::kernel::sched::trace::take_snapshot("ipi-recv");
            sched::mark_for_preempt();
        }
        Trap::Interrupt(EXTERNAL) => handle_external_irq(),
        Trap::Interrupt(code) => handle_unknown_interrupt(code),
        Trap::Exception(ECALL_FROM_U) => {
            handle_ecall(frame);
        }
        Trap::Exception(ECALL_FROM_M) => {
            // Advance mepc
            frame.mepc += 4;
            crate::dprint!("ecall from M");
        }
        Trap::Exception(
            code @ (INSTRUCTION_ACCESS_FAULT | LOAD_ACCESS_FAULT | STORE_ACCESS_FAULT),
        ) => handle_access_fault(frame, code),
        Trap::Exception(code) => handle_exception(frame, code),
    }
    if percpu::needs_reschedule() {
        percpu::set_preempt_mepc(frame.mepc);
        percpu::set_preempt_mstatus(frame.mstatus);
        // Set up frame for trampoline
        frame.mepc = if hart_id() == 0 {
            preempt_trampoline_h0 as *const () as usize
        } else {
            preempt_trampoline_h1 as *const () as usize
        };
        frame.mstatus &= !mstatus::MPIE; // Ensure trampoline executes with interrupts disabled
        frame.mstatus |= mstatus::MPP; // Run the trampoline in M-mode
    }
}

#[inline(never)]
#[cold]
fn handle_ecall(frame: &mut TrapFrame) {
    match frame.syscall() {
        crate::syscall::EXIT => {
            exit_from_user(frame, ExitReason::Exit);
        }
        crate::syscall::PUT_CHAR => {
            // Advance mepc
            frame.mepc += 4;
            if let Some(c) = char::from_u32(frame.a0 as u32) {
                crate::dprint!("{c}");
            }
        }
        _ => {
            // Advance mepc
            frame.mepc += 4;
            crate::dprint!("user ecall code {}", frame.syscall());
        }
    }
}

fn exit_from_user(frame: &mut TrapFrame, reason: ExitReason) {
    // Confirm whether we are a synchronous or scheduled based on presence of PerCpu.kernel_resume_sp
    if percpu::kernel_resume_sp() != 0 {
        #[cfg(all(test, feature = "test-user"))]
        // Store the exit reason in percpu for testing purposes only
        percpu::set_user_exit_reason(reason);
        // Set mstatus to reenable interrupts on return
        frame.mstatus |= mstatus::MPP;
        frame.mstatus |= mstatus::MPIE;
        // Switch mepc to return to M-mode
        frame.mepc = crate::arch::usermode::resume_kernel as *const () as usize;
    } else {
        // Set up frame for user exit trampoline
        frame.a0 = reason as usize;
        frame.mepc = usermode::user_thread_exit as *const () as usize;
        frame.mstatus &= !mstatus::MPIE; // Ensure trampoline executes with interrupts disabled
        frame.mstatus |= mstatus::MPP; // Run the trampoline in M-mode
    }
}

#[inline(never)]
#[cold]
fn handle_access_fault(frame: &mut TrapFrame, code: usize) {
    if frame.is_from_user() {
        exit_from_user(frame, ExitReason::Fault);
    } else {
        handle_exception(frame, code);
    }
}

#[inline(never)]
#[cold]
#[cfg_attr(feature = "profile", profile)]
fn irq_panic() -> ! {
    use crate::io::DirectWriter;
    use core::fmt::Write;
    let _ = writeln!(DirectWriter, "mepc is {:x}", crate::arch::csr::mepc::read());
    let _ = writeln!(
        DirectWriter,
        "mcause is {:?}",
        crate::arch::csr::mcause::read()
    );
    panic!(
        "IRQ stack canary not found for HART {}",
        crate::arch::hart_id()
    );
}

#[inline(never)]
#[cold]
#[cfg_attr(feature = "profile", profile)]
fn handle_external_irq() {
    let irq = plic::claim();
    match irq {
        0 => {} // Spurious interrupt
        board::uart::IRQ => {
            uart::handle_interrupt();
        }
        board::virtio_blk::IRQ => {
            virtio::handle_virtio_interrupt();
        }
        _ => panic!("Unknown external interrupt: {}", irq),
    }
    if irq != 0 {
        plic::complete(irq);
    }
}

#[inline(never)]
#[cold]
#[cfg_attr(feature = "profile", profile)]
fn handle_unknown_interrupt(code: usize) {
    panic!("Unknown interrupt code {:x} mepc {:x}", code, mepc::read());
}

#[inline(never)]
#[cold]
#[cfg_attr(feature = "profile", profile)]
fn handle_exception(frame: &TrapFrame, code: usize) {
    match code {
        ILLEGAL_INSTRUCTION => panic!("Illegal instruction at {:x}", mepc::read()),
        LOAD_ACCESS_FAULT | STORE_ACCESS_FAULT => panic!(
            "{} access fault {} (=mcause) attempted at address {:x} (=mtval) from instruction {:x} (=mepc), return address {:x} (=ra)",
            match code {
                LOAD_ACCESS_FAULT => "Load",
                STORE_ACCESS_FAULT => "Store",
                _ => {
                    ""
                }
            },
            code,
            mtval::read(),
            mepc::read(),
            frame.ra,
        ),
        _ => panic!(
            "Unknown exception code {:x} mepc {:x} mtval {:x}",
            code,
            mepc::read(),
            mtval::read()
        ),
    }
}
