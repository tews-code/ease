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
pub mod plic {
    pub const BASE: usize = 0x0C00_0000;
    pub const PRIORITY: usize = 0x0000_0000; // Priority for source N is at BASE + 4*N, where N is IRQ number
    pub const ENABLE: usize = 0x0000_2000; // Enable bits for context 0
    pub const THRESHOLD: usize = 0x0020_0000; // Priority threshold for context 0
    pub const CLAIM_COMPLETE: usize = 0x0020_0004; // Claim/complete for context 0

    pub const UART0_IRQ: u32 = 10;
    pub const VIRTIO0_IRQ: u32 = 1;
}

// UART base address for 16550 compatible Uart on QEMU virt
pub mod uart {
    pub const BASE: usize = 0x1000_0000;
}

// HARTS
pub const HARTS_MAX: usize = 2;

// PMP
pub const PMP_ADDR_COUNT: usize = 8;
