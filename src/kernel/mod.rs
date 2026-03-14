//! Kernel services
//!
//! Future home of:
//! - Memory management (heap allocator)
//! - Synchronization primitives (Mutex, Spinlock)
//! - Timer and scheduling

pub mod alloc;
pub mod collection;
pub mod stack_guard;
pub mod sync;
pub mod timer;
