//! Trap handler for both interrupts and exceptions
use crate::arch::csr::mcause::exception::*;
use crate::arch::csr::mcause::interrupt::*;
use crate::arch::csr::mcause::{self, Trap};
use crate::arch::csr::{mepc, mtval};
use crate::arch::stack::STACK_CANARY;
use crate::board;
use crate::drivers::{plic, uart, virtio};
use crate::kernel::ipi;
use crate::kernel::sched;

#[cfg(feature = "profile")]
use ease_macros::profile;

unsafe extern "C" {
    static __hart0_irq_stack_base: u8;
}

// trap_handler is kept as small as possible to fit into SRAM8 .text
#[unsafe(no_mangle)]
// #[unsafe(link_section = ".sram8_text")]
#[cfg_attr(feature = "profile", profile)]
extern "C" fn trap_handler() {
    // let _g = crate::kernel::profile::ProfileGuard::new("trap_handler");
    // Check if IRQ stack canary is in place
    // Safety: Address is safe to read and aligned from linker script
    let canary =
        unsafe { core::ptr::read_volatile(&raw const __hart0_irq_stack_base as *const usize) };
    if canary != STACK_CANARY {
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
        Trap::Exception(code) => handle_exception(code),
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
