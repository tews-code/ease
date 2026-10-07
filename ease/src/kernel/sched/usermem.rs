//! User thread memory map
//!
//! Creates a memory map for user programs with correct PMP protection for the segments
//!
//! Supports both fixed address backed or heap address backed memory
//!
//! RP2350 supports up to 8 NAPOT regions and we use two of these in M-mode.
//! A user process `Map` therefore consists of up to 6 segments covered by differing PMP rules

use core::marker::PhantomData;
use core::num::NonZero;
use core::ptr::NonNull;

use crate::arch::csr::pmp;
use crate::arch::pmp::Pmp;
use crate::arch::uaccess;
use crate::board;
use crate::kernel::alloc::{MemRegion, Order, Pool};
use crate::kernel::sched::process::SyscallContext;
use crate::kernel::sync::with_interrupts_disabled;

// All programs are currently forced to have the same layout by linker script
unsafe extern "C" {
    static __user_text_start: u8;
    static __user_data_start: u8;
    static __user_bss_start: u8;
}
/// A user buffer that has survived the validation process.
///
/// The lifetime specifier links the buffer to the `SyscallContext` which in turn
/// ensures the the process is still alive while the syscall completes
pub(crate) struct ValidatedUserBuf<'a> {
    buf: NonNull<[u8]>,
    direction: Transfer,
    _borrow: PhantomData<&'a SyscallContext>, // Ties the lifetime of the struct to the lifetime of the SyscallContext.
}
impl<'a> ValidatedUserBuf<'a> {
    /// New validated user buffer
    pub(super) fn new(
        validated_user_buf: NonNull<[u8]>,
        direction: Transfer,
        _syscall_context: &'a SyscallContext,
    ) -> Self {
        Self {
            buf: validated_user_buf,
            direction,
            _borrow: PhantomData,
        }
    }
    /// Length of the buffer. The buffer is bytes, so length is also the number of bytes.
    pub(crate) fn len(&self) -> usize {
        self.buf.len()
    }
    /// Copy from a given source slice into the user buffer.
    ///
    /// Returns the number of bytes copied. If the transfer direction is wrong
    /// `0` bytes are copied.
    pub(crate) fn copy_to_user(&self, src: &[u8]) -> usize {
        if self.direction == Transfer::ToUser {
            let copy_len = self.buf.len().min(src.len());
            let bytes_uncopied = with_interrupts_disabled(|_c|
                // Safety:
                // - The borrow ensures that the validated buffer still belongs to a live process
                // - the kernel slice is a Rust reference, so it's valid, and &mut rules out other readers or writers
                // - PMP checks every access to the user side, so concurrent writes from other user threads are harmless.
                unsafe {
                    uaccess::copy_to_user(src.as_ptr(), self.buf.cast::<u8>().as_ptr(), copy_len)
                });
            return copy_len - bytes_uncopied;
        }
        0
    }
    /// Copy from the user buffer into a slice.
    ///
    /// Returns the number of bytes copied. If the transfer direction is wrong
    /// `0` bytes are copied.
    pub(crate) fn copy_from_user(&self, dest: &mut [u8]) -> usize {
        if self.direction == Transfer::FromUser {
            let copy_len = self.buf.len().min(dest.len());
            let bytes_uncopied = with_interrupts_disabled(|_c|
                // Safety:
                // - The borrow ensures that the validated buffer still belongs to a live process
                // - the kernel slice is a Rust reference, so it's valid, and &mut rules out other readers or writers
                // - PMP checks every access to the user side, so concurrent writes from other user threads are harmless.
                unsafe {
                    uaccess::copy_from_user(self.buf.cast::<u8>().as_ptr(), dest.as_mut_ptr(), copy_len)
                });
            return copy_len - bytes_uncopied;
        }
        0
    }
}
/// This informs what access the kernel will have to a provided user buffer
/// This must match the PMP access for that buffer
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Transfer {
    FromUser,
    ToUser,
}

impl Transfer {
    pub(crate) fn pmp_permission(&self) -> u8 {
        match self {
            // Match what the user process has permission to do
            Self::FromUser => board::pmp::R,
            Self::ToUser => board::pmp::W,
        }
    }
}
/// A contiguous stretch of memory with a data transfer direction
/// The values come from u-mode and are not trusted
#[derive(Clone, Copy)]
pub(crate) struct UserBuf {
    base_addr: usize,
    len: usize,
    pub(super) direction: Transfer, // Direction the data is moving. Must match the PMP of that memory extent
}
/// The form of backing for the memory region - either fixed from the linker or
/// dynamically from the buddy allocator
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
            Role::Text => board::pmp::R | board::pmp::X,
            Role::Data | Role::Bss | Role::Stack | Role::Heap | Role::BufRW => {
                board::pmp::R | board::pmp::W
            }
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
    map: [Option<Segment>; board::pmp::ADDR_COUNT],
}

impl Map {
    /// A new user memory map
    pub(super) const fn new() -> Self {
        Self {
            map: [const { None }; board::pmp::ADDR_COUNT],
        }
    }
    /// Determines whether the whole buffer is contained within a single memory region with the right access type.
    /// The access type ensures that the kernel is not using the user memory in a way not permitted as if PMP were
    /// still active.
    ///
    /// Returns `None` if the user buffer doesn't match any of the process and thread memory regions (including access type).
    /// Returns `Some(NonNull<[u8]>)` if the user buffer is valid.
    /// A zero-length buffer is always `Some`.
    pub(crate) fn validate(
        &self,
        user_stack: &MemRegion,
        user_buf: UserBuf,
    ) -> Option<NonNull<[u8]>> {
        if user_buf.len == 0 {
            return Some(NonNull::slice_from_raw_parts(NonNull::<u8>::dangling(), 0));
        }
        // Check if the user buffer's extent is within the user stack
        // Note we don't check the direction of the data transfer as a stack is always read & write
        if user_stack.contains(user_buf.base_addr, user_buf.len) {
            // Derive the provenance of the base pointer from the stack, not the user buffer
            return Some(NonNull::slice_from_raw_parts(
                user_stack
                    .base()
                    .with_addr(NonZero::new(user_buf.base_addr)?),
                user_buf.len,
            ));
        }
        // Now check each of the process's memory segments
        for segment in self.map.iter().flatten() {
            if segment.region.contains(user_buf.base_addr, user_buf.len)
                // Check if the PMP access matches the data transfer direction
                && ((segment.role.permissions() & user_buf.direction.pmp_permission()) != 0)
            {
                return Some(NonNull::slice_from_raw_parts(
                    segment
                        .region
                        .base()
                        .with_addr(NonZero::new(user_buf.base_addr)?),
                    user_buf.len,
                ));
            }
        }
        None
    }
    /// Get a reference to memory region for the given role
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

// `validate` is the kernel's only defence against confused-deputy syscalls:
// a user buffer that reaches outside the process's own regions, or that asks
// the kernel to write where the user may only read. The map is the real
// process layout (linker-fixed text/data/bss) plus a heap-allocated stack, so
// these exercise the same regions a syscall would see.
#[cfg(all(test, feature = "test-sched"))]
mod test {
    use super::{Map, Role, Transfer, UserBuf};
    use crate::kernel::alloc::{MemRegion, Order, Pool};

    fn process_map() -> (Map, MemRegion) {
        let mut map = Map::new();
        map.try_for_process().expect("process map should allocate");
        let stack = MemRegion::from_heap(Pool::UserPd0, Order::KB1).expect("stack should allocate");
        (map, stack)
    }

    fn buf(base_addr: usize, len: usize, direction: Transfer) -> UserBuf {
        UserBuf {
            base_addr,
            len,
            direction,
        }
    }

    fn bounds(map: &Map, role: Role) -> (usize, usize) {
        let region = map.region(role).expect("role should be mapped");
        (region.base_addr(), region.base_addr() + region.size())
    }

    // A valid buffer comes back with the user's address and length intact.
    #[test_case]
    fn buffer_inside_data_is_returned_unchanged() {
        let (map, stack) = process_map();
        let (base, _) = bounds(&map, Role::Data);
        let slice = map
            .validate(&stack, buf(base + 16, 32, Transfer::ToUser))
            .expect("buffer inside data should validate");
        assert_eq!(slice.addr().get(), base + 16);
        assert_eq!(slice.len(), 32);
    }

    // `top` is one past the end, so a buffer ending exactly there is inside.
    #[test_case]
    fn buffer_ending_at_region_top_validates() {
        let (map, stack) = process_map();
        let (_, top) = bounds(&map, Role::Data);
        assert!(
            map.validate(&stack, buf(top - 8, 8, Transfer::ToUser))
                .is_some()
        );
    }

    #[test_case]
    fn buffer_straddling_region_top_is_rejected() {
        let (map, stack) = process_map();
        let (_, top) = bounds(&map, Role::Data);
        assert!(
            map.validate(&stack, buf(top - 4, 8, Transfer::ToUser))
                .is_none()
        );
    }

    #[test_case]
    fn buffer_straddling_region_base_is_rejected() {
        let (map, stack) = process_map();
        let (base, _) = bounds(&map, Role::Text);
        assert!(
            map.validate(&stack, buf(base - 4, 8, Transfer::FromUser))
                .is_none()
        );
    }

    // Starts above every user region: the shape of a buffer aimed at kernel
    // memory. An inverted end-check would accept this.
    #[test_case]
    fn buffer_above_all_regions_is_rejected() {
        let (map, stack) = process_map();
        let above = 0xF000_0000;
        assert!(
            map.validate(&stack, buf(above, 8, Transfer::FromUser))
                .is_none()
        );
        assert!(
            map.validate(&stack, buf(above, 8, Transfer::ToUser))
                .is_none()
        );
    }

    #[test_case]
    fn buffer_wrapping_address_space_is_rejected() {
        let (map, stack) = process_map();
        assert!(
            map.validate(&stack, buf(usize::MAX - 3, 8, Transfer::FromUser))
                .is_none()
        );
    }

    // `write(1, "literal", n)`: string literals live in text, which the user
    // can read, so the kernel may read them on the user's behalf.
    #[test_case]
    fn kernel_may_read_from_text() {
        let (map, stack) = process_map();
        let (base, _) = bounds(&map, Role::Text);
        assert!(
            map.validate(&stack, buf(base, 16, Transfer::FromUser))
                .is_some()
        );
    }

    // `read(fd, <text address>, n)`: the user can't write text, so the kernel
    // must not write it for them.
    #[test_case]
    fn kernel_must_not_write_to_text() {
        let (map, stack) = process_map();
        let (base, _) = bounds(&map, Role::Text);
        assert!(
            map.validate(&stack, buf(base, 16, Transfer::ToUser))
                .is_none()
        );
    }

    #[test_case]
    fn stack_buffer_validates_in_both_directions() {
        let (map, stack) = process_map();
        let base = stack.base_addr();
        assert!(
            map.validate(&stack, buf(base, 64, Transfer::FromUser))
                .is_some()
        );
        assert!(
            map.validate(&stack, buf(base, 64, Transfer::ToUser))
                .is_some()
        );
    }

    #[test_case]
    fn buffer_straddling_stack_top_is_rejected() {
        let (map, stack) = process_map();
        let top = stack.base_addr() + stack.size();
        assert!(
            map.validate(&stack, buf(top - 4, 8, Transfer::ToUser))
                .is_none()
        );
    }

    // Zero bytes touch no memory, so any address is accepted, and the user's
    // junk address is never echoed back as a kernel pointer.
    #[test_case]
    fn zero_length_buffer_validates_without_user_address() {
        let (map, stack) = process_map();
        for junk in [0, 1, 0xDEAD_BEEF, usize::MAX] {
            let slice = map
                .validate(&stack, buf(junk, 0, Transfer::ToUser))
                .expect("zero-length buffer should always validate");
            assert_eq!(slice.len(), 0);
            if junk != 1 {
                assert_ne!(slice.addr().get(), junk);
            }
        }
    }
}
