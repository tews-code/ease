//! QEMU `virt` board for RISC-V

#![allow(dead_code)]

// Core-local interrupt (timer)
pub mod clint {
    pub const BASE: usize = 0x0200_0000;
    pub const TIMER_FREQ_HZ: u64 = 10_000_000;
}

// Virtio block device
pub mod virtio_blk {
    pub const BASE: usize = 0x10001000;
}

// Platform Level Interrupt Controller
#[allow(dead_code)]
pub mod plic {
    pub const BASE: usize = 0x0C00_0000;
    pub const PRIORITY: usize = 0x0000_0000; // Priority for source N is at BASE + 4*N, where N is IRQ number
    pub const ENABLE: usize = 0x0000_2000; // Enable bits for context 0
    pub const THRESHOLD: usize = 0x0020_0000; // Priority threshold for context 0
    pub const CLAIM_COMPLETE: usize = 0x0020_0004; // Claim/complete for context 0

    pub const UART0_IRQ: u32 = 10;
    pub const VIRTIO0_IRQ: u32 = 1;
}

// UART base and offset addresses for 16550 compatible Uart on QEMU virt
pub mod uart {
    pub const BASE: usize = 0x1000_0000;
    pub const RBR: usize = 0; // offset +0: receive buffer register (read)
    pub const THR: usize = 0; // offset +0: transmit holding register (write)
    pub const IER: usize = 1; // offset +1: interrupt enable register
    pub const IIR: usize = 2; // offset +2: Interrupt Identification Register
    pub const LSR: usize = 5; // offset +5: line status register

    pub const LSR_TX_READY: u8 = 0x20;
    pub const LSR_BYTE_READY: u8 = 1;
    pub const THRE_INTERRUPT: u8 = 1 << 1; // Transmitter Holding Register Empty - IER register bit 1
}
