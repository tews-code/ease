//! Trap handler for both interrupts and exceptions
use crate::arch::csr::mcause::exception::*;
use crate::arch::csr::mcause::interrupt::*;
use crate::arch::csr::mcause::{self, Trap};
use crate::arch::csr::mepc;
use crate::board;
use crate::drivers::{plic, uart, virtio};
use crate::kernel::ipi;
use crate::kernel::sched;

// trap_handler is kept as small as possible to fit into SRAM8 .text
#[unsafe(no_mangle)]
#[unsafe(link_section = ".sram8_text")]
extern "C" fn trap_handler() {
    match mcause::read() {
        Trap::Interrupt(TIMER) => sched::preempt(),
        Trap::Interrupt(SOFTWARE) => {
            ipi::clear_self();
            sched::preempt();
        }
        Trap::Interrupt(EXTERNAL) => handle_external_irq(),
        Trap::Interrupt(code) => handle_unknown_interrupt(code),
        Trap::Exception(code) => handle_exception(code),
    }
}

#[inline(never)]
#[cold]
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
fn handle_unknown_interrupt(code: usize) {
    panic!("Unknown interrupt code {:x} mepc {:x}", code, mepc::read());
}

#[inline(never)]
#[cold]
fn handle_exception(code: usize) {
    match code {
        ILLEGAL_INSTRUCTION => panic!("Illegal instruction at {:x}", mepc::read()),
        _ => panic!("Unknown exception code {:x} mepc {:x}", code, mepc::read()),
    }
}
