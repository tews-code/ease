//! User thread memory map
//!
//! Creates a memory map for user programs with correct PMP protection for the segments
//!
//! Supports both fixed address backed or heap address backed memory
//!
//! RP2350 supports up to 8 NAPOT regions and we use two of these in M-mode.
//! A user process `Map` therefore consists of up to 6 segments covered by differing PMP rules

use core::ptr::NonNull;

use crate::arch::csr::pmp;
use crate::arch::pmp::Pmp;
use crate::board;
use crate::kernel::alloc::{MemRegion, Order, Pool};

// All programs are currently forced to have the same layout by linker script
unsafe extern "C" {
    static __user_text_start: u8;
    static __user_data_start: u8;
    static __user_bss_start: u8;
}

pub(crate) enum Backing {
    Heap(Pool),
    Fixed(usize),
}

#[allow(dead_code)]
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Text,
    Data,
    Bss,
    Stack,
    Heap,
    BufRW,
}

impl Role {
    fn permissions(self) -> u8 {
        match self {
            Role::Text => pmp::R | pmp::X,
            Role::Data | Role::Bss | Role::Stack | Role::Heap | Role::BufRW => pmp::R | pmp::W,
        }
    }

    fn backing(self) -> Backing {
        match self {
            Role::Text => Backing::Fixed(&raw const __user_text_start as usize),
            Role::Data => Backing::Fixed(&raw const __user_data_start as usize),
            Role::Bss => Backing::Fixed(&raw const __user_bss_start as usize),
            Role::Stack | Role::Heap => Backing::Heap(Pool::UserPd0),
            Role::BufRW => Backing::Heap(Pool::Psram),
        }
    }
    // We only have 8 slots available
    // We already use PmpAddr0 and PmpAddr1 in M-mode, so these are all that are left
    fn addr_slot(self) -> usize {
        match self {
            Role::Text => 2,
            Role::Data => 3,
            Role::Bss => 4,
            Role::Stack => 5,
            Role::Heap => 6,
            Role::BufRW => 7,
        }
    }
}

struct Segment {
    role: Role,
    region: MemRegion,
}

pub(crate) struct Map {
    map: [Option<Segment>; board::PMP_ADDR_COUNT],
}

impl Map {
    pub(super) const fn new() -> Self {
        Self {
            map: [const { None }; board::PMP_ADDR_COUNT],
        }
    }
    /// Get a reference to memory region from an existing user memory map
    ///
    /// Returns None if the region is not found.
    pub(super) fn region(&self, role: Role) -> Option<&MemRegion> {
        self.map[role.addr_slot()].as_ref().map(|s| &s.region)
    }
    /// Add a memory region to an existing user memory map
    pub(super) fn add_region(&mut self, role: Role, order: Order) -> Result<(), ()> {
        let region = match role.backing() {
            Backing::Heap(pool) => MemRegion::from_heap(pool, order).ok_or(())?,
            Backing::Fixed(base) => MemRegion::from_fixed(
                NonNull::new(base as *mut u8).expect("base should not be null"),
                order,
            ),
        };
        self.map[role.addr_slot()] = Some(Segment { role, region });
        Ok(())
    }
    /// Attempts to add required memory regions to a memory map for a user process.
    ///
    /// Returns a `Ok(())` on success or an `Err(())` on failure. Failure
    /// is caused by being unable to allocate memory.
    pub(crate) fn try_for_process(&mut self) -> Result<(), ()> {
        self.add_region(Role::Text, crate::Order::KB4)?;
        self.add_region(Role::Data, crate::Order::KB2)?;
        self.add_region(Role::Bss, crate::Order::KB2)?;
        Ok(())
    }
    /// Derive a PMP set from a Map for a specific thread
    ///
    /// Iterates through the program's Map segments and uses
    /// given roles to choose the access type for each.
    /// Then adds a PMP region for the individual thread's stack
    pub(super) fn to_pmp(&self, user_stack: &MemRegion) -> Pmp {
        let mut pmp = Pmp::new();
        for (i, slot) in self.map.iter().enumerate() {
            if let Some(segment) = slot {
                pmp.set_region(
                    i,
                    segment.region.base_addr(),
                    segment.region.size(),
                    pmp::NAPOT,
                    segment.role.permissions(),
                );
            }
        }
        // Add the thread's stack
        let role = Role::Stack;
        pmp.set_region(
            role.addr_slot(),
            user_stack.base().addr().into(),
            user_stack.size(),
            pmp::NAPOT,
            role.permissions(),
        );
        pmp
    }
}
