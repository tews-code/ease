//! User Thread Memory Map

use core::ptr::NonNull;

use crate::arch::csr::pmp;
use crate::arch::pmp::Pmp;
use crate::board;
use crate::kernel::alloc::{MemRegion, Order, Pool};

// For now, put the user .text in the PSRAM buffer area (immediately after the 4MB heap region)
unsafe extern "C" {
    static __user_text_start: u8;
    static __user_data_bss_start: u8;
    static __user_data_lma: u8;
    static mut __user_data_start: u8;
    static __user_data_end: u8;
    static mut __user_bss_start: u8;
    static __user_bss_end: u8;
}

pub(crate) enum Backing {
    Heap(Pool),
    Fixed { base: usize },
}

#[repr(u8)]
#[derive(Clone, Copy)]
pub(crate) enum Role {
    Text,
    DataBss,
    Stack,
    Heap,
    BufRW,
    BufRO,
}

impl Role {
    fn permissions(self) -> u8 {
        match self {
            Role::Text => pmp::R | pmp::X,
            Role::DataBss | Role::Stack | Role::Heap | Role::BufRW => pmp::R | pmp::W,
            Role::BufRO => pmp::R,
        }
    }

    fn backing(self) -> Backing {
        match self {
            Role::Text => Backing::Fixed {
                base: &raw const __user_text_start as usize,
            },
            Role::DataBss => Backing::Fixed {
                base: &raw const __user_data_bss_start as usize,
            },
            Role::Stack | Role::Heap => Backing::Heap(Pool::UserPd0),
            Role::BufRW | Role::BufRO => Backing::Heap(Pool::Psram),
        }
    }

    fn addr_slot(self) -> usize {
        match self {
            Role::Text => 2, // PmpAddr0 and PmpAddr1 are use in M-mode
            Role::DataBss => 3,
            Role::Stack => 4,
            Role::Heap => 5,
            Role::BufRW => 6,
            Role::BufRO => 7,
        }
    }
}

struct Segment {
    role: Role,
    region: MemRegion,
}

pub(crate) struct UserMemMap {
    map: [Option<Segment>; board::PMP_ADDR_COUNT],
}

impl UserMemMap {
    pub(super) const fn new() -> Self {
        Self {
            map: [const { None }; board::PMP_ADDR_COUNT],
        }
    }

    fn add_region(&mut self, role: Role, order: Order) -> Result<(), ()> {
        let region = match role.backing() {
            Backing::Heap(pool) => MemRegion::from_heap(pool, order).ok_or(())?,
            Backing::Fixed { base } => MemRegion::from_fixed(
                NonNull::new(base as *mut u8).expect("base should not be null"),
                order,
            ),
        };
        self.map[role.addr_slot()] = Some(Segment { role, region });
        Ok(())
    }

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

    fn clear_region(&mut self, role: Role) {
        self.map[role.addr_slot()] = None; // Triggers Drop on MemRegion which deallocs for heap-based regions
    }

    pub(crate) fn for_user_thread(stack_order: Order) -> Result<UserMemMap, ()> {
        let mut memmap = Self::new();
        memmap.add_region(Role::Text, Order::KB4)?;
        memmap.add_region(Role::DataBss, Order::KB4)?;
        memmap.add_region(Role::Stack, stack_order)?;
        Ok(memmap)
    }
}

pub(crate) fn init() {
    // Copy the user .data from flash to PSRAM
    // Safety: Linker script sets up symbols to an aligned writeable region
    unsafe {
        core::ptr::copy_nonoverlapping(
            &raw const __user_data_lma,
            &raw mut __user_data_start,
            &raw const __user_data_end as usize - &raw const __user_data_start as usize,
        );
    }
    // Zero the user .bss
    // Safety: Linker script sets up symbols to an aligned writeable region
    unsafe {
        core::ptr::write_bytes(
            &raw mut __user_bss_start,
            0,
            &raw const __user_bss_end as usize - &raw const __user_bss_start as usize,
        );
    }
}
