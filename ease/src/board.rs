//! QEMU `virt` board for RISC-V

#![allow(dead_code)]

// Core-local interrupt (timer)
pub mod clint {
    pub const BASE: usize = 0x0200_0000;
    pub const TIMER_FREQ_HZ: u64 = 10_000_000;
}

pub mod virtio {
    pub const VIRTQ_PAGE_SIZE: usize = 512;
    // Virtio block device
    pub mod blk {
        pub const BASE: usize = 0x10001000; // Attached to MMIO bus 0
        pub const BLOCK_SIZE: usize = 512;
        pub const IRQ: u32 = 1;
    }
    // Virtio keyboard device
    pub mod keyboard {
        pub const BASE: usize = 0x10002000; // Attached to MMIO bus 1
        pub const IRQ: u32 = 2;
    }
}

// Platform Level Interrupt Controller
pub mod plic {
    pub const BASE: usize = 0x0C00_0000;
    pub const PRIORITY: usize = 0x0000_0000; // Priority for source N is at BASE + 4*N, where N is IRQ number
    pub const HART0_ENABLE: usize = 0x0000_2000; // Enable bits for context 0 (HART0 M-mode)
    pub const HART1_ENABLE: usize = 0x0000_2100; // Enable bits for context 2 (HART1 M-mode)
    pub const HART0_THRESHOLD: usize = 0x0020_0000; // Priority threshold for context 0
    pub const HART1_THRESHOLD: usize = 0x0020_2000; // Priority threshold for context 2
    pub const HART0_CLAIM_COMPLETE: usize = 0x0020_0004; // Claim/complete for context 0
    pub const HART1_CLAIM_COMPLETE: usize = 0x0020_2004; // Claim/complete for context 2
}

// UART base address for 16550 compatible Uart on QEMU virt
pub mod uart {
    pub const BASE: usize = 0x1000_0000;
    pub const IRQ: u32 = 10;
}

// HARTS
pub const HARTS_MAX: usize = 2;

// PMP
pub const PMP_ADDR_COUNT: usize = 8;
