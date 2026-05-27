//! Trap handler for both interrupts and exceptions
use crate::arch::cpu_id;
use crate::arch::csr::mcause::exception::*;
use crate::arch::csr::mcause::interrupt::*;
use crate::arch::csr::mcause::{self, Trap};
use crate::arch::csr::mstatus;
use crate::arch::csr::mstatus::MPIE;
use crate::arch::csr::{mepc, mtval};
use crate::arch::stack::STACK_CANARY;
use crate::arch::trap::TrapFrame;
use crate::board;
use crate::drivers::{plic, uart, virtio};
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

// trap_handler is kept as small as possible to fit into SRAM8 .text
#[inline(always)]
fn trap_handler_impl(frame: &mut TrapFrame) {
    // Check if IRQ stack canary is in place
    // Safety: Address is safe to read and aligned from linker script
    let irq_stack_base = if cpu_id() == 0 {
        &raw const __hart0_irq_stack_base as *const usize
    } else {
        &raw const __hart1_irq_stack_base as *const usize
    };
    if unsafe { core::ptr::read_volatile(irq_stack_base) } != STACK_CANARY {
        irq_panic();
    }
    match mcause::read() {
        Trap::Interrupt(TIMER) => sched::mark_for_preempt(),
        Trap::Interrupt(SOFTWARE) => {
            ipi::clear_self();
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
        Trap::Exception(code) => handle_exception(code),
    }
    if percpu::needs_reschedule() {
        percpu::set_preempt_mepc(frame.mepc);
        percpu::set_preempt_mstatus(frame.mstatus);
        // Set up frame for trampoline
        frame.mepc = if cpu_id() == 0 {
            preempt_trampoline_h0 as *const () as usize
        } else {
            preempt_trampoline_h1 as *const () as usize
        };
        frame.mstatus &= !MPIE; // Ensure trampoline executes with interrupts disabled
    }
}

#[inline(never)]
#[cold]
fn handle_ecall(frame: &mut TrapFrame) {
    match frame.syscall() {
        crate::syscall::EXIT => {
            // Set mstatus to return to M mode
            frame.mstatus |= mstatus::MPP;
            frame.mstatus |= mstatus::MPIE;
            // Switch mepc to return to M-mode
            frame.mepc = crate::arch::usermode::resume_kernel as *const () as usize;
        }
        _ => {
            // Advance mepc
            frame.mepc += 4;
            crate::dprint!("user ecall code {}", frame.syscall());
        }
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
        crate::arch::cpu_id()
    );
}

#[inline(never)]
#[cold]
#[cfg_attr(feature = "profile", profile)]
fn handle_external_irq() {
    let irq = plic::with_plic(|p| p.claim());
    match irq {
        0 => {} // Spurious interrupt
        board::plic::UART0_IRQ => {
            uart::handle_interrupt();
        }
        board::plic::VIRTIO0_IRQ => {
            virtio::handle_virtio_interrupt();
        }
        _ => panic!("Unknown external interrupt: {}", irq),
    }
    if irq != 0 {
        plic::with_plic(|p| p.complete(irq));
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
fn handle_exception(code: usize) {
    match code {
        ILLEGAL_INSTRUCTION => panic!("Illegal instruction at {:x}", mepc::read()),
        _ => panic!(
            "Unknown exception code {:x} mepc {:x} mtval {:x}",
            code,
            mepc::read(),
            mtval::read()
        ),
    }
}
