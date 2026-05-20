//! Types for the scheduler

use core::alloc::Layout;
use core::ptr::NonNull;

use crate::arch::STACK_CANARY;
use crate::arch::context::Context;
use crate::arch::trap::TrapFrame;

pub(super) const THREADS_MAX: usize = 32;

// Stack sizes must be power-of-two and aligned to their own size
// This means that the stack base address is `size` aligned and can be found using a bitmask
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct StackClass(u8);

#[allow(dead_code)]
impl StackClass {
    pub const KB1: Self = Self(10);
    pub const KB2: Self = Self(11);
    pub const KB4: Self = Self(12);
    pub const KB8: Self = Self(13);
    pub const KB16: Self = Self(14);
    pub const KB32: Self = Self(15);
    pub const KB64: Self = Self(16);
    pub const KB128: Self = Self(17);

    const ALL: [Self; 8] = [
        Self::KB1,
        Self::KB2,
        Self::KB4,
        Self::KB8,
        Self::KB16,
        Self::KB32,
        Self::KB64,
        Self::KB128,
    ];

    const _MIN_CLASS_SIZE_CHECK: () =
        assert!(StackClass::KB1.size() >= core::mem::size_of::<TrapFrame>());

    pub(super) const fn size(self) -> usize {
        1usize << self.0
    }

    pub(super) const fn mask(self) -> usize {
        self.size() - 1
    }

    const fn align(self) -> usize {
        self.size()
    }

    const fn layout(self) -> Layout {
        match Layout::from_size_align(self.size(), self.align()) {
            Ok(layout) => layout,
            Err(_) => panic!("invalid layout"),
        }
    }
}

pub(super) struct HeapStack;

impl HeapStack {
    // Allocates a thread stack from the heap and returns a pointer
    // to the stack base
    // Returns None if allocation fails
    pub fn allocate(class: StackClass) -> Option<NonNull<u8>> {
        NonNull::new(unsafe { alloc::alloc::alloc(class.layout()) })
    }

    // Forges a heap-based thread Context
    // The thread entry function is stored in s0
    // Returns the stack pointer
    // Safety: stack_base must be class.size()-aligned and point to
    // writeable memory of at least class.size() bytes
    pub unsafe fn init_for_entry(
        stack_base: NonNull<u8>,
        class: StackClass,
        trampoline_ptr: extern "C" fn(*mut u8) -> !,
        closure_ptr: *mut u8,
    ) -> *mut u8 {
        // Safety: Caller has provided a valid stack base pointer
        unsafe {
            core::ptr::write(stack_base.as_ptr() as *mut usize, STACK_CANARY);
        }
        let context_ptr = unsafe {
            stack_base
                .as_ptr()
                .add(class.size() - core::mem::size_of::<Context>()) as *mut Context
        };
        // Safety: context_ptr is derived from stack_base, and
        // aligned because sizeof(Context) is a multiple of align(Context).
        unsafe {
            core::ptr::write_bytes(context_ptr, 0, 1); // writes 0 across one Context's worth of bytes
            *context_ptr = Context::for_entry(trampoline_ptr, closure_ptr);
        }
        context_ptr as *mut u8
    }

    // Deallocate the heap-backed thread stack.
    // Safety: sp must point inside a stack region that was set up
    // by init_for_entry and has not been deallocated.
    // The caller transfers ownership of the sp; further use is UB.
    pub unsafe fn deallocate(class: StackClass, base: *mut u8) {
        unsafe { alloc::alloc::dealloc(base, class.layout()) };
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Deadline {
    pub(super) min: u64,
    pub(super) fixed_leeway: Option<u64>, // None - use system default leeway
}

#[derive(PartialEq, Debug)]
pub(super) enum PostSwitch {
    Blocked,
    BlockedUntil(Deadline),
    Ready,
    Sleeping(Deadline),
    Dead,
}

#[derive(PartialEq, Debug)]
pub(super) enum State {
    Avail,
    Blocked,
    BlockedUntil(Deadline),
    Ready,
    Running,
    Switching(PostSwitch),
    Sleeping(Deadline),
}

pub enum Qos {
    High,
    Low,
}

pub(super) struct ThreadControlBlock {
    pub(super) id: u32,
    pub(super) state: State,
    pub(super) sp: *mut u8,
    pub(super) stack: Option<StackClass>,
    pub(super) stack_base: *mut u8,
    pub(super) qos: Qos,
    pub(super) priority: u8,             // Lower number is higher priority
    pub(super) pass: u64,                // The next ready thread with lowest pass wins
    pub(super) last_started_cycles: u64, // Cycle stamp from last switch
    pub(super) next_waiter: Option<ThreadHandle>, // Handle of next thread waiting on blocked resource
    pub(super) affinity: Option<u8>,              // Affinity to a particular HART
}

pub(super) struct ThreadsInner {
    pub(super) control_blocks: [ThreadControlBlock; THREADS_MAX],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ThreadHandle {
    pub(super) id: u32,
    pub(super) idx: usize,
}

impl ThreadHandle {
    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn idx(&self) -> usize {
        self.idx
    }
}
