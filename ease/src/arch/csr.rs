//! RISC-V 32 bit CSRs

use core::arch::asm;

macro_rules! define_csr {
    ($csr:ident) => {
        pub mod $csr {
            pub fn read() -> usize {
                let $csr: usize;
                // Safety: CSR is safe to read
                unsafe {
                    core::arch::asm!(concat!("csrr {}, ", stringify!($csr)), out(reg) $csr);
                }
                $csr
            }

            /// Write the PMP
            ///
            #[doc = concat!("Caller must ensure the `", stringify!($csr), "` value does not prevent valid region use")]
            pub unsafe fn write($csr: usize) {
                // Safety: CSR is safe to write and caller ensures valid region
                unsafe {
                    core::arch::asm!(concat!("csrw ", stringify!($csr), ", {}"), in(reg) $csr);
                }
            }
        }
    };
}

/// PMP
pub mod pmp {
    pub(crate) const R: u8 = 1;
    pub(crate) const W: u8 = 1 << 1;
    pub(crate) const X: u8 = 1 << 2;
    pub(crate) const OFF: u8 = 0;
    pub(crate) const NAPOT: u8 = 0b11 << 3;
    pub(crate) const NO_ACCESS: u8 = 0;
    pub(crate) const LOCK: u8 = 1 << 7;

    define_csr!(pmpaddr0);
    define_csr!(pmpaddr1);
    define_csr!(pmpaddr2);
    define_csr!(pmpaddr3);
    define_csr!(pmpaddr4);
    define_csr!(pmpaddr5);
    define_csr!(pmpaddr6);
    define_csr!(pmpaddr7);

    define_csr!(pmpcfg0);
    define_csr!(pmpcfg1);
}

/// Machine cause register (mcause)
pub mod mcause {
    #[derive(Debug)]
    pub enum Trap {
        Exception(usize),
        Interrupt(usize),
    }

    pub mod exception {
        pub const INSTRUCTION_ACCESS_FAULT: usize = 1;
        pub const ILLEGAL_INSTRUCTION: usize = 2;
        pub const LOAD_ACCESS_FAULT: usize = 5;
        pub const STORE_ACCESS_FAULT: usize = 7;
        pub const ECALL_FROM_U: usize = 8;
        pub const ECALL_FROM_M: usize = 11;
    }

    pub mod interrupt {
        pub const SOFTWARE: usize = 3;
        pub const TIMER: usize = 7;
        pub const EXTERNAL: usize = 11;
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

/// Machine exception program counter (mepc)
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

/// Machine HART id
pub mod mhartid {
    pub fn read() -> usize {
        let mhartid: usize;
        unsafe {
            core::arch::asm!("csrr {}, mhartid", out(reg) mhartid);
        }
        mhartid
    }
}

// Machine scratch register
define_csr!(mscratch);

/// Machine exception program counter
pub mod mtval {
    /// Read the mtval CSR.
    pub fn read() -> usize {
        let mtval: usize;
        unsafe {
            core::arch::asm!("csrr {}, mtval", out(reg) mtval);
        }
        mtval
    }
}

/// Machine interrupt enable register (mie) operations.
pub mod mie {
    pub const MSIE: usize = 1 << 3; // Machine software interrupts enable
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

/// Machine Interrupt Status.
pub mod mip {
    /// Machine software interrupt enable bit (bit 3).
    pub const MSIP: usize = 1 << 3;
}

/// Main registers
pub mod regs {
    #[inline(always)]
    pub fn sp() -> usize {
        let sp: usize;
        unsafe {
            core::arch::asm!("mv {}, sp", out(reg) sp);
        }
        sp
    }
}

/// Read the RISC-V cycle counter (64-bit)
///
/// Returns the number of clock cycles since reset.
/// On RV32, this reads both `cycleh` and `cycle` CSRs.
pub fn rdcycles() -> u64 {
    let mut lo: u32;
    let mut hi1: u32;
    let mut hi2: u32;
    loop {
        unsafe {
            asm!(
                "rdcycleh {hi1}",
                 "rdcycle {lo}",
                 "rdcycleh {hi2}",
                 hi1 = out(reg) hi1,
                 lo = out(reg) lo,
                 hi2 = out(reg) hi2,
                 options(nomem, nostack),
            );
        }
        if hi1 == hi2 {
            return ((hi1 as u64) << 32) | (lo as u64);
        }
    }
}
