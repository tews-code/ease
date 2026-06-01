//! User Thread Memory Map

use core::ptr::NonNull;

use crate::arch::csr::pmp;
use crate::arch::pmp::Pmp;
use crate::board;
use crate::kernel::alloc::{MemRegion, Order, Pool};

// For now, put the user .text in the PSRAM buffer area (immediately after the 4MB heap region)
unsafe extern "C" {
    static __user_text_start: u8;
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
    Buf1,
    Buf2,
    Buf3,
    Buf4,
}

impl Role {
    fn permissions(self) -> u8 {
        match self {
            Role::Text => pmp::R | pmp::X,
            Role::DataBss
            | Role::Stack
            | Role::Heap
            | Role::Buf1
            | Role::Buf2
            | Role::Buf3
            | Role::Buf4 => pmp::R | pmp::W,
        }
    }

    fn backing(self) -> Backing {
        match self {
            Role::Text => Backing::Fixed {
                base: &raw const __user_text_start as usize,
            },
            Role::DataBss | Role::Stack | Role::Heap => Backing::Heap(Pool::UserPd0),
            Role::Buf1 | Role::Buf2 | Role::Buf3 | Role::Buf4 => Backing::Heap(Pool::Psram),
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
    const fn new() -> Self {
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
        self.map[role as usize] = Some(Segment { role, region });
        Ok(())
    }

    pub(crate) fn to_pmp(&self) -> Pmp {
        let mut pmp = Pmp::new();
        for (i, slot) in self.map.iter().enumerate() {
            if let Some(segment) = slot {
                pmp.set_region(
                    i,
                    segment.region.base_addr(),
                    segment.region.size(),
                    segment.role.permissions(),
                );
            }
        }
        pmp
    }

    fn clear_region(&mut self, role: Role) {
        self.map[role as usize] = None; // Triggers Drop on MemRegion which deallocs for heap-based regions
    }

    pub(crate) fn for_user_thread(stack_order: Order) -> Result<UserMemMap, ()> {
        let mut memmap = Self::new();
        memmap.add_region(Role::Text, Order::KB4)?;
        memmap.add_region(Role::DataBss, Order::KB2)?;
        memmap.add_region(Role::Stack, stack_order)?;
        Ok(memmap)
    }
}
