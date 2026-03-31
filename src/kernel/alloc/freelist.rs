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
//                      |     Vec           |       FreeBlock -> next
// 0x8002_0132          |                   |
//                      ~                   ~       *allocated memory*
//                      |                   |       FreeBlock -> None
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

impl FreeList {
    pub fn init(&self) {
        //
        //                  ~                   ~
        //                  +-------------------+
        // __heap_start     |  FreeBlock::next  |  Set to None - tail.
        //                  |  FreeBlock::size  |  Set to __heap_end
        //                  |                   |
        //                  |                   |
        //                  |                   |
        //                  |                   |
        // __heap_end       +-------------------+
        //                  |                   |   stack end
        let mut free_list_head = self.head.lock();
        debug_assert!(free_list_head.is_none());
        // Initialise - set entire heap memory to free block at heap start
        let free_block_ptr = &raw const __heap_start as *mut FreeBlock;
        // Safety: Heap start is valid for writes and aligned
        unsafe {
            (*free_block_ptr).next = None;
            (*free_block_ptr).size =
                &raw const __heap_end as usize - &raw const __heap_start as usize;
        }
        // Saftey: I've just created a valid free_block_ptr which is non-null
        *free_list_head = Some(unsafe { NonNull::new_unchecked(free_block_ptr) });
    }
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

// Need a minimum of 8 bytes to store FreeBlock
// For simplicity, make sure *any* allocation is at least this size
const ALLOC_MIN_BYTES: usize = core::mem::size_of::<FreeBlock>();

unsafe impl GlobalAlloc for FreeList {
    // Don't forget to call init() first!
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        //
        //  ~                   ~
        //  +-------------------+
        //  |  FreeBlock::next  |   prev_free_block_ptr
        //  |  FreeBlock::size  |
        //  |                   |
        //  +-------------------+
        //  |                   |
        //  |    allocated      |
        //  |                   |
        //  +-------------------+
        //  |  FreeBlock::next  |   free_block_ptr
        //  |  FreeBlock::size  |
        //  |                   |
        //  |                   |
        //  | -----padding----- |   Padding for new alloc
        //  |                   |   Return this addres for new alloc
        //  |  allocation here  |
        //  |                   |   New allocation will be here
        //  |                   |
        //  | ----alloc ends--- |   Rounded to ALLOC_MIN_BYTES
        //  |  FreeBlock::next  |   new_free_block_ptr
        //  |  FreeBlock::size  |
        //  |                   |
        //  +-------------------+
        //  |                   |
        //  |  allocation here  |
        //  |                   |
        //  +-------------------+
        //  |  FreeBlock::next  |   next_free_block_ptr
        //  |  FreeBlock::size  |
        //  |                   |
        //  ~                   ~

        // Start at the list head
        let mut free_list_head = self.head.lock();
        // Work out the allocation size needed
        // Regardless of request size, the minimum allocation is ALLOC_MIN_BYTES
        // We always round size up to ALLOC_MIN_BYTES alignment to simplify next allocation
        // Note we can only work out the padded size when we have actual addresses
        let unpadded_req_size = align_up(layout.size(), ALLOC_MIN_BYTES);
        // Walk the free block list looking for suitable space size
        let mut current_free_block = *free_list_head;
        // A mutable reference to the slot that points to the current node.
        // Initially that's &mut *free_list.
        // After advancing, it's &mut (*previous_block).next.
        // Then to unlink or relink, just write to that one slot.
        // It handles head-of-list and middle-of-list uniformly.
        let mut prev_free_block_ref: &mut Option<NonNull<FreeBlock>> = &mut free_list_head;
        while let Some(block) = current_free_block {
            // Get the free block pointer
            let free_block_ptr = block.as_ptr();
            // Check if size is big enough
            // Calculate the padding requirement
            let padding =
                align_up(free_block_ptr as usize, layout.align()) - free_block_ptr as usize;
            let aligned_req_size = padding + unpadded_req_size;
            if unsafe { (*free_block_ptr).size } >= aligned_req_size {
                // Success - found a free block large enough for padded aligned rounded up layout request
                // Check if padding is creating enough space for a new free block
                if padding >= ALLOC_MIN_BYTES {
                    // We have enough padding space to create a FreeBlock
                    // Reduce the current FreeBlock size to the padding
                    let orig_size = unsafe { (*free_block_ptr).size };
                    unsafe { (*free_block_ptr).size = padding };
                    // Create a new temporary free block directly after the padding free block
                    let temp_free_block_addr = free_block_ptr as usize + padding;
                    let temp_free_block_ptr = temp_free_block_addr as *mut FreeBlock;
                    unsafe {
                        (*temp_free_block_ptr).size = orig_size - padding;
                        (*temp_free_block_ptr).next = (*free_block_ptr).next;
                    }
                    // Link the temporary free block into the chain
                    unsafe {
                        (*free_block_ptr).next = Some(NonNull::new_unchecked(temp_free_block_ptr))
                    };
                    // On re-evaluate: shrunken block will be too small, loop
                    // will advance to the aligned temp block
                    continue;
                }
                // Create a free block at the end of the allocation if there is enough space
                if unsafe { (*free_block_ptr).size } > aligned_req_size {
                    // Calculate the address at the end of the new allocation
                    let new_free_block_addr = free_block_ptr as usize + aligned_req_size;
                    // First create a new block that is the remainder of the currently free space
                    let new_free_block_ptr = new_free_block_addr as *mut FreeBlock;
                    unsafe {
                        (*new_free_block_ptr).size = (*free_block_ptr).size - aligned_req_size;
                        (*new_free_block_ptr).next = (*free_block_ptr).next;
                    }
                    // Now link the new block into the chain
                    *prev_free_block_ref =
                        unsafe { Some(NonNull::new_unchecked(new_free_block_ptr)) };
                } else {
                    *prev_free_block_ref = unsafe { (*free_block_ptr).next };
                }
                // Finally, return aligned address
                return (free_block_ptr as usize + padding) as *mut u8;
            } else {
                current_free_block = unsafe { (*free_block_ptr).next };
                prev_free_block_ref = unsafe { &mut (*free_block_ptr).next };
            }
        }
        // Reached the end of the list
        // Not enough free memory
        core::ptr::null_mut()
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: core::alloc::Layout) {}
}

pub fn init() {
    FREE_LIST.init();
}
