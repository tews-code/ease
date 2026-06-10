//! Architecture-specific code for RISC-V

pub mod boot;
pub mod context;
#[allow(dead_code)]
pub mod csr;
pub mod interrupts;
pub mod mmio;
pub mod percore_text;
#[allow(dead_code)]
pub mod pmp;
pub mod trap;
pub mod usermode;

/// Get HART id that this thread is running on
pub(crate) fn hart_id() -> usize {
    crate::arch::csr::mhartid::read()
}
