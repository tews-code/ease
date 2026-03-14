//! RISC-V 32 bit CSRs

/// Machine status register (mstatus) operations.
pub mod mstatus {
    /// Machine interrupt enable bit (bit 3). Controls global interrupt enable.
    pub const MIE: usize = 1 << 3;

    /// Atomically sets bits in the mstatus CSR.
    pub fn enable_bits(bits: usize) {
        // Safety: csrs atomically sets bits in mstatus CSR
        unsafe {
            core::arch::asm!("csrs mstatus, {}", in(reg) bits);
        }
    }
}

/// Machine interrupt enable register (mie) operations.
pub mod mie {
    pub const MTIE: usize = 1 << 7; // Machine timer interrupt enable bit (bit 7).
    pub const MEIE: usize = 1 << 11; // External interrupt

    /// Atomically sets bits in the mie CSR.
    pub fn enable_bits(bits: usize) {
        // Safety: csrs atomically sets bits in mie CSR
        unsafe {
            core::arch::asm!("csrs mie, {}", in(reg) bits);
        }
    }
}
