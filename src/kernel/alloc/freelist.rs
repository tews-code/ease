//! Free List Allocator

// Rough Diagram of memory layout fo SRAM
//
// 0x8000_0000          +-------------------+       SRAM start
//                      |                   |
//                      |       text        |
//                      |       rodata      |
//                      |       data        |
//                      |       bss         |
//
//                      ~      ~100kb       ~
//                      +-------------------+
// 0x8002_0000          |     String        |       Heap start (aligns to 16 - ends 0 in hex) - linker symbol
//                      |     Vec           |
// 0x8002_0132          |                   |   <-  Next heap allocation
//                      ~                   ~           "
//                      |                   |           v (Grows upwards to end of SRAM)
//                      |                   |
// 0x8006_0000          |-------------------|   <- Top of heap
// 0x8006_0004          |                   |   <- Stack guard
//                      |                   |
//                      ~                   ~           ^ (Grows downwards toward start of SRAM)
//                      |                   |           "
//                      |   StackVec        |   <- Current stack pointer
//                      |   TrapFrame       |
// 0x80082000           +-------------------+   <- End of SRAM
//
//
//
//  Free List allocator using linked list
//  Each node consists of a pointer to the next free block address and a size.
//  Tail has next of None
//  Head stored as static.
//  head (static) -> next -> next -> next -> next -> None
//                   size    size    size    size    size (all remaining)
//
//  When allocating to part of a slot
//  head (static) -> next -       -> next -> next -> None
//                   size |       |  size    size    size (all remaining)
//                        -> next -
//                           size (reduced)
//
//
// Let's say the current position is 0x8002_0008:
//
// Address       Content
// ───────────── ─────────────────────
// 0x8002_0008   [u8; 13] byte 0       ← returned pointer (aligned to 8)
// 0x8002_0009   [u8; 13] byte 1
// 0x8002_000a   [u8; 13] byte 2
// 0x8002_000b   [u8; 13] byte 3
// 0x8002_000c   [u8; 13] byte 4
// 0x8002_000d   [u8; 13] byte 5
// 0x8002_000e   [u8; 13] byte 6
// 0x8002_000f   [u8; 13] byte 7
// 0x8002_0010   [u8; 13] byte 8
// 0x8002_0011   [u8; 13] byte 9
// 0x8002_0012   [u8; 13] byte 10
// 0x8002_0013   [u8; 13] byte 11
// 0x8002_0014   [u8; 13] byte 12
// 0x8002_0015   . (wasted - round up to min block size 16)
// 0x8002_0016   . (wasted - round up to min block size 16)
// 0x8002_0017   . (wasted - round up to min block size 16)
// 0x8002_0018   ← next allocation starts here (aligned to 8)
//

use core::alloc::GlobalAlloc;
use core::ptr::NonNull;

use crate::kernel::alloc::align_up;
use crate::kernel::sync::IrqSpinLock;

unsafe extern "C" {
    static __heap_start: u8;
    static __heap_end: u8;
}

#[global_allocator]
static FREE_LIST: FreeList = FreeList {
    head: IrqSpinLock::new(None),
};

// Free list needs to be protected by:
// - SpinLock - as dual core multiple threads may allocate at the same time
// - Irq disabled - as need to complete an allocation without interruption
//
// From Rust language guide:
// Representation
// Thanks to the null pointer optimization, NonNull<T> and Option<NonNull<T>>
// are guaranteed to have the same size and alignment
struct FreeList {
    head: IrqSpinLock<Option<NonNull<FreeBlock>>>,
}

// FREE_LIST is static, must be Sync
// Safety: A reference to the FreeList struct is safe to share between threads
unsafe impl Sync for FreeList {}

#[repr(C)]
struct FreeBlock {
    // Link to the next struct of the same type (or None)
    // Representation
    // Thanks to the null pointer optimization, NonNull<T> and Option<NonNull<T>> are guaranteed to have the same size and alignment
    next: Option<NonNull<FreeBlock>>,
    size: usize,
}

unsafe impl GlobalAlloc for FreeList {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        // Need a minimum of 8 bytes to store FreeBlock
        // For simplicity, make sure *any* allocation is at least this size
        const ALLOC_MIN_BYTES: usize = 8;

        let mut free_list = self.head.lock();
        if free_list.is_none() {
            // Initialise - set entire memory to free block at heap start
            let free_block_ptr = &raw const __heap_start as *mut FreeBlock;
            // Safety: Heap start is valid for writes and aligned
            unsafe {
                (*free_block_ptr).next = None;
                (*free_block_ptr).size =
                    &raw const __heap_end as usize - &raw const __heap_start as usize;
            }
            *free_list = Some(unsafe { NonNull::new_unchecked(free_block_ptr) });
        }
        // Work out the allocation size needed
        // Regardless of request size, the minimum allocation is ALLOC_MIN_BYTES
        // We always round up to ALLOC_MIN_BYTES alignment to simplify next allocation
        let req_size = align_up(layout.size(), ALLOC_MIN_BYTES);
        // Walk the free block list looking for suitable space size
        let mut current = *free_list;
        //  — a mutable reference to the slot that points to the current node. Initially that's &mut *free_list.
        // After advancing, it's &mut (*previous_block).next. Then to unlink or relink, you just write to that one slot. It handles
        // head-of-list and middle-of-list uniformly.

        let mut prev: &mut Option<NonNull<FreeBlock>> = &mut free_list;
        while let Some(block) = current {
            // Get the next free block
            let free_block = block.as_ptr();
            // Check if size is big enough
            // Calculate the alignment (in case I need to pad at start of free block)
            let padding = align_up(free_block as usize, layout.align()) - free_block as usize;
            let aligned_req_size = padding + req_size;
            if unsafe { (*free_block).size } >= aligned_req_size {
                // Success, need to split, re-link
                if unsafe { (*free_block).size } > aligned_req_size {
                    // Calculate the address at the end of the new allocation
                    let new_free_block_addr = free_block as usize + aligned_req_size;
                    // First create a new block that is the remainder of the currently free space
                    let new_free_block = new_free_block_addr as *mut FreeBlock;
                    unsafe {
                        (*new_free_block).size = (*free_block).size - aligned_req_size;
                        (*new_free_block).next = (*free_block).next;
                    }
                    // Now link the new block into the chain
                    *prev = unsafe { Some(NonNull::new_unchecked(new_free_block)) };
                } else {
                    *prev = unsafe { (*free_block).next };
                }
                // Finally, return aligned address
                return (free_block as usize + padding) as *mut u8;
            } else {
                current = unsafe { (*free_block).next };
                prev = unsafe { &mut (*free_block).next };
            }
        }
        // Reached the end of the list
        // Not enough free memory
        core::ptr::null_mut()
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: core::alloc::Layout) {}
}
