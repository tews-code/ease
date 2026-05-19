//! Trap handler for both interrupts and exceptions
use crate::arch::csr::mcause::exception::*;
use crate::arch::csr::mcause::interrupt::*;
use crate::arch::csr::mcause::{self, Trap};
use crate::arch::csr::mepc;
use crate::board;
use crate::drivers::{plic, uart, virtio};
use crate::kernel::ipi;
use crate::kernel::sched;

#[unsafe(no_mangle)]
extern "C" fn trap_handler() {
    match mcause::read() {
        Trap::Interrupt(code) => match code {
            TIMER => {
                sched::preempt();
            }
            SOFTWARE => {
                ipi::clear_self(); // Prevent this interrupt from re-firing
                sched::preempt();
            }
            EXTERNAL => {
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
            _ => crate::println!("Unknown interrupt {}", code),
        },
        Trap::Exception(code) => match code {
            ILLEGAL_INSTRUCTION => panic!("Illegal instruction at {:x}", mepc::read()),
            _ => panic!("Unknown exception code {:x} mepc {:x}", code, mepc::read()),
        },
    }
}
