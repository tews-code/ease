//! Trap handler for both interrupts and exceptions
use crate::arch::csr::mcause::exception::*;
use crate::arch::csr::mcause::interrupt::*;
use crate::arch::csr::mcause::{self, Trap};
use crate::arch::csr::mepc;
use crate::arch::trap::TrapFrame;

#[unsafe(no_mangle)]
extern "C" fn trap_handler(sp: *mut TrapFrame) -> *mut TrapFrame {
    match mcause::read() {
        Trap::Interrupt(code) => match code {
            TIMER => {
                crate::kernel::timer::handle_interrupt();
                return crate::kernel::sched::preempt_into(sp);
            }
            EXTERNAL => {
                let irq = crate::drivers::plic::claim();
                match irq {
                    0 => {} // Spurious interrupt
                    crate::board::plic::UART0_IRQ => {
                        crate::drivers::uart::handle_interrupt();
                    }
                    crate::board::plic::VIRTIO0_IRQ => {
                        crate::drivers::virtio::handle_virtio_interrupt();
                    }
                    _ => panic!("Unknown external interrupt: {}", irq),
                }
                if irq != 0 {
                    crate::drivers::plic::complete(irq);
                }
            }
            _ => crate::println!("Unknown interrupt {}", code),
        },
        Trap::Exception(code) => match code {
            ILLEGAL_INSTRUCTION => panic!("Illegal instruction at {:x}", mepc::read()),
            _ => panic!("Unknown exception code {:x} mepc {:x}", code, mepc::read()),
        },
    }
    sp
}
