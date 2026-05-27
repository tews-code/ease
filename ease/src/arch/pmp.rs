//! Phyical Memory Protection

use crate::arch::csr::pmp;

// `pmpaddrX` register holds the region address shifted right by 2 (it counts 4-byte units)
fn pmpaddr(base: usize, size: usize) -> usize {
    assert!(
        size.is_power_of_two(),
        "size must power of two (RP2350 requires NAPOT regions)"
    );
    assert!(
        size >= 32,
        "requested size too small to be represented (NA4 unavailable in RP2350)"
    );
    assert!(
        (base & (size - 1)) == 0,
        "base and size not naturally aligned (NAPOT required for RP2350)"
    );
    (base | (size / 2 - 1)) >> 2
}

unsafe extern "C" {
    static __flash_start: u8;
    static __flash_end: u8;
    static __heap_pd0_start: u8;
    static __heap_pd0_end: u8;
}

pub(crate) fn configure() {
    let flash_start = &raw const __flash_start as usize;
    let flash_size = &raw const __flash_end as usize - flash_start;
    let flash_pmpaddr = pmpaddr(flash_start, flash_size);
    let heap_start = &raw const __heap_pd0_start as usize;
    let heap_size = &raw const __heap_pd0_end as usize - heap_start;
    let heap_pmpaddr = pmpaddr(heap_start, heap_size);

    // Safety: flash is NAPOT
    unsafe {
        pmp::pmpaddr0::write(flash_pmpaddr);
    }
    assert!(
        pmp::pmpaddr0::read() == flash_pmpaddr,
        "flash PMP was not configured"
    );
    // Safety: user heap in power domain 0 is NAPOT
    unsafe {
        pmp::pmpaddr1::write(heap_pmpaddr);
    }
    assert!(
        pmp::pmpaddr1::read() == heap_pmpaddr,
        "heap PMP was not configured"
    );

    // Enable the PMP
    let pmpcfg_entry0 = pmp::R | pmp::X | pmp::NAPOT;
    let pmpcfg_entry1 = (pmp::R | pmp::W | pmp::NAPOT) << 8;
    let pmpcfg0 = pmpcfg_entry0 | pmpcfg_entry1;
    // Safety: Address regions have been configured; flash is safe for read/execute and user heap is safe for RW
    unsafe {
        pmp::pmpcfg0::write(pmpcfg0);
    }
    assert!(
        pmp::pmpcfg0::read() == pmpcfg0,
        "pmpcfg0 was not configured"
    );
}

// Encoding tests for `pmpaddr`. `arch` is stubbed out of the host lib
// crate, so these run on the kernel target in QEMU (`cargo test --bin
// ease`), not on the host. The QEMU custom test framework has no
// `#[should_panic]`, so these cover the happy-path encodings — which is
// where the real risk lives: the operator-precedence trap where `base`
// must be shifted together with the size mask, `(base | mask) >> 2`.
#[cfg(all(test, feature = "test-pmp"))]
mod test {
    use super::*;

    // 32-byte region (the Hazard3 granule) at a 32-aligned base.
    #[test_case]
    fn encodes_minimum_granule_region() {
        assert_eq!(pmpaddr(0x1000, 32), 0x403);
    }

    // 16 MiB flash region at 0x2000_0000 — the kernel/user code grant.
    #[test_case]
    fn encodes_16mib_flash_region() {
        assert_eq!(pmpaddr(0x2000_0000, 0x100_0000), 0x081F_FFFF);
    }

    // 256 KiB SRAM_PD0 region at 0x8000_0000 — the user stack/data grant.
    #[test_case]
    fn encodes_256kib_pd0_region() {
        assert_eq!(pmpaddr(0x8000_0000, 0x4_0000), 0x2000_7FFF);
    }
}
