//! Physical Memory Protection

// PMP is per-hart.
// It uses address registers to define regions, and config registers
// (one for each set of four addresses) to set the config.
// Addresses in NAPOT saves the last two bit (as they are always zero)
//      base_addr >> 2
// The lower bytes are set to 1 to indicate the region size through order
// = 0x2000_8000 >> 2 = 0x0800_2000
//      (size >> 3) - 1
//
// Example: 4 KiB region of SRAM at 0x2000_8000, as PMP entry 0,
// with permissions read + execute, no write.
//  base_addr >> 2  0x0800_2000
//  size (0x1000 >> 3) - 1 = 512 - 1 = 0x1FF  ← nine 1-bits
//  pmpaddr0 = 0x0800_2000 | 0x1FF = 0x0800_21FF
//
//  Low bits of 0x0800_21FF:
//
//  bit:  13            9 8                0
//  1  0  0  0  0  0  1  1  1  1  1  1  1  1  1
//  └── lowest 0 ──┘└── nine 1s ─┘
//
//   Entry 0 lives in bits [7:0] of pmpcfg0:
//
//   ┌─────┬───────┬───────┬────────────┐
//   │ bit │ field │ value │  meaning   │
//   ├─────┼───────┼───────┼────────────┤
//   │ 0   │ R     │ 1     │ read       │
//   ├─────┼───────┼───────┼────────────┤
//   │ 1   │ W     │ 0     │ no write   │
//   ├─────┼───────┼───────┼────────────┤
//   │ 2   │ X     │ 1     │ execute    │
//   ├─────┼───────┼───────┼────────────┤
//   │ 4:3 │ A     │ 0b11  │ NAPOT or 0 │   // 0 is off
//   ├─────┼───────┼───────┼────────────┤
//   │ 6:5 │ —     │ 0     │ reserved   │
//   ├─────┼───────┼───────┼────────────┤
//   │ 7   │ L     │ 0     │ not locked │
//   └─────┴───────┴───────┴────────────┘
//
//    Byte = 0b0001_1101 = 0x1D. Entries 1–3 stay 0 (disabled), so:
//
//    pmpcfg0 = 0x0000_001D
//
//   Addresses are in a heirarchy, with the lower address number taking precendence over higher
//   address numbers. Nesting is allowed.

use crate::arch::csr::pmp;
use crate::board::{self, PMP_ADDR_COUNT};

#[derive(PartialEq, Eq)]
struct PmpAddr(usize);

impl PmpAddr {
    const fn new() -> Self {
        Self(0)
    }

    fn from_base_size(base: usize, size: usize) -> Self {
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
        Self((base | (size / 2 - 1)) >> 2)
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

struct PmpCfg(usize);

impl PmpCfg {
    const fn new(
        addr0_settings: u8,
        addr1_settings: u8,
        addr2_settings: u8,
        addr3_settings: u8,
    ) -> Self {
        Self(
            (Self::check_cfg(addr0_settings) as usize)
                | (Self::check_cfg(addr1_settings) as usize) << 8
                | (Self::check_cfg(addr2_settings) as usize) << 16
                | (Self::check_cfg(addr3_settings) as usize) << 24,
        )
    }

    const fn check_cfg(permissions: u8) -> u8 {
        let clean_permissions = (permissions << 5) >> 5;
        assert!(
            (clean_permissions & (pmp::W | pmp::X)) != (pmp::W | pmp::X),
            "attempt to set a PMP region to write and execute"
        );
        clean_permissions
    }

    fn clear(&mut self) {
        self.0 = 0;
    }

    fn set(&mut self, region: usize, access: u8, permissions: u8) {
        assert!(
            access == pmp::OFF || access == pmp::NAPOT,
            "PMP access only supports NAPOT or off"
        );
        let mut cfg = if permissions == 0 {
            0
        } else {
            Self::check_cfg(permissions)
        };
        cfg |= access;
        self.0 |= (cfg as usize) << (8 * (region % 4));
    }

    fn set_lock(&mut self, region: usize) {
        self.0 |= (pmp::LOCK as usize) << (8 * (region % 4));
    }
}

pub(crate) struct Pmp {
    addr: [PmpAddr; board::PMP_ADDR_COUNT],
    cfg: [PmpCfg; board::PMP_ADDR_COUNT / 4], // Each register holds permissions for 4 addresses
}

impl Pmp {
    pub(crate) const fn new() -> Self {
        Self {
            addr: [const { PmpAddr::new() }; board::PMP_ADDR_COUNT],
            cfg: [const { PmpCfg::new(0, 0, 0, 0) }; board::PMP_ADDR_COUNT / 4],
        }
    }
    fn clear(&mut self) {
        for i in 0..PMP_ADDR_COUNT {
            self.addr[i].clear();
        }
        for i in 0..PMP_ADDR_COUNT / 4 {
            self.cfg[i].clear();
        }
    }

    pub(crate) fn set_region(
        &mut self,
        region: usize,
        base: usize,
        size: usize,
        access: u8,
        permissions: u8,
    ) {
        assert!(region < board::PMP_ADDR_COUNT);
        self.addr[region] = PmpAddr::from_base_size(base, size);
        self.cfg[region / 4].set(region, access, permissions);
    }

    fn set_lock(&mut self, region: usize) {
        self.cfg[region / 4].set_lock(region);
    }

    pub(crate) fn activate(&self) {
        // Safety: The Pmp configuration is consistent and ready for write
        unsafe {
            pmp::pmpaddr0::write(self.addr[0].0);
            pmp::pmpaddr1::write(self.addr[1].0);
            pmp::pmpaddr2::write(self.addr[2].0);
            pmp::pmpaddr3::write(self.addr[3].0);
            pmp::pmpaddr4::write(self.addr[4].0);
            pmp::pmpaddr5::write(self.addr[5].0);
            pmp::pmpaddr6::write(self.addr[6].0);
            pmp::pmpaddr7::write(self.addr[7].0);
            pmp::pmpcfg0::write(self.cfg[0].0);
            pmp::pmpcfg1::write(self.cfg[1].0);
        }
    }
}

unsafe extern "C" {
    static __sram8_text_start: u8;
    static __sram8_text_end: u8;
    static __sram9_text_start: u8;
    static __sram9_text_end: u8;
}

/// Set up PMP protection to catch null or near-null pointer deferences
///
/// Note on QEMU these are already caught as 0x0 is not mapped.
/// On RP2350 this is the boot ROM
pub(crate) extern "C" fn protect_null_ptr_deref() {
    let mut pmp = Pmp::new();
    pmp.set_region(0, 0, 4096, pmp::NAPOT, pmp::NO_ACCESS); // Set a 4096 size region to no access starting at address 0
    pmp.set_lock(0);
    pmp.activate();
}

/// Set up protection for .text in each HARTs scratch RAM (SRAM8 and SRAM9).
///
/// `.text` is just below the IRQ stack - this gives IRQ stack protection in M-mode
/// This uses the first two PMP addresses. The remaining 6 are available for use in U-mode
///
/// Safety: Caller must call this function _after_ .text has been copied from flash, but
/// _before_ any U-mode PMP.
pub(crate) extern "C" fn protect_sram_text(sram_text_start: usize, sram_text_end: usize) {
    let mut pmp = Pmp::new();
    pmp.set_region(
        1,
        sram_text_start,
        sram_text_end - sram_text_start,
        pmp::NAPOT,
        pmp::R | pmp::X,
    );
    pmp.set_lock(1);
    pmp.activate();
    assert_text_guard_locked(1);
}

/// Read the just-written guard config back and confirm it is a locked,
/// write-denying R+X NAPOT region. Runs on the configuring hart at boot, so a
/// stripped lock bit or a misrouted config write fails fast here rather than
/// silently leaving the scratch `.text` unprotected.
fn assert_text_guard_locked(region: usize) {
    let cfg_word = if region < 4 {
        pmp::pmpcfg0::read()
    } else {
        pmp::pmpcfg1::read()
    };
    let cfg = ((cfg_word >> (8 * (region % 4))) & 0xFF) as u8;
    assert!(
        cfg & pmp::LOCK != 0,
        "scratch .text guard not locked: cfg={:#x}",
        cfg
    );
    assert!(
        cfg & pmp::W == 0,
        "scratch .text guard is writable: cfg={:#x}",
        cfg
    );
    assert!(
        cfg & pmp::R != 0,
        "scratch .text guard not readable: cfg={:#x}",
        cfg
    );
    assert!(
        cfg & pmp::X != 0,
        "scratch .text guard not executable: cfg={:#x}",
        cfg
    );
    assert!(
        cfg & pmp::NAPOT == pmp::NAPOT,
        "scratch .text guard A-field not NAPOT: cfg={:#x}",
        cfg
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
    use crate::arch::pmp::PmpAddr;

    // 32-byte region (the Hazard3 granule) at a 32-aligned base.
    #[test_case]
    fn encodes_minimum_granule_region() {
        assert_eq!(PmpAddr::from_base_size(0x1000, 32).0, 0x403);
    }

    // 16 MiB flash region at 0x2000_0000 — the kernel/user code grant.
    #[test_case]
    fn encodes_16mib_flash_region() {
        assert_eq!(
            PmpAddr::from_base_size(0x2000_0000, 0x100_0000).0,
            0x081F_FFFF
        );
    }

    // 256 KiB SRAM_PD0 region at 0x8000_0000 — the user stack/data grant.
    #[test_case]
    fn encodes_256kib_pd0_region() {
        assert_eq!(
            PmpAddr::from_base_size(0x8000_0000, 0x4_0000).0,
            0x2000_7FFF
        );
    }
}
