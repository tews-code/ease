//! RISC-V 32 bit CSRs

/// Machine status register (mstatus) operations.
pub mod mstatus {
    /// Machine interrupt enable bit (bit 3). Controls global interrupt enable.
    pub const MIE: usize = 1 << 3;
    /// Machine previous interrupt enable bit (bit 7).
    pub const MPIE: usize = 1 << 7;
    /// Machine previous priority - mret stays in M-mode
    pub const MPP: usize = 3 << 11;

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

/// Machine Except Program Counter (mepc)
pub mod mepc {
    /// Read the mepc CSR.
    pub fn read() -> usize {
        let mepc: usize;
        unsafe {
            core::arch::asm!("csrr {}, mepc", out(reg) mepc);
        }
        mepc
    }
}

/// Machine cause register (mcause)
pub mod mcause {
    pub enum Trap {
        Exception(usize),
        Interrupt(usize),
    }
    pub mod exception {
        pub const ILLEGAL_INSTRUCTION: usize = 2;
    }
    pub mod interrupt {
        pub const EXTERNAL: usize = 11;
        pub const TIMER: usize = 7;
    }

    /// Read the mcause CSR.
    pub fn read() -> Trap {
        let mcause: usize;
        unsafe {
            core::arch::asm!("csrr {}, mcause", out(reg) mcause);
        }
        if (mcause >> 31) & 1 == 1 {
            Trap::Interrupt(mcause & 0x7FFFFFFF)
        } else {
            Trap::Exception(mcause & 0x7FFFFFFF)
        }
    }
}
