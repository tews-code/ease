 Critical Review: freelist.rs                                                                                                                                                     
                                                                                                                                                                                   
  🔴 Severity: Critical                                                                                                                                                            
                                                                                                                                                                                   
  1. No coalescing in dealloc — guaranteed fragmentation death spiral                                                                                                              
                                                                                                                                                                                   
  This is the single most important flaw. dealloc inserts a new free block at the correct address-ordered position but never merges adjacent free blocks. This means:              
                  
  Allocate A (8 bytes), B (8 bytes), C (8 bytes)  →  contiguous                                                                                                                    
  Free A  →  [free 8] [B] [C]                                                                                                                                                      
  Free C  →  [free 8] [B] [free 8]                                                                                                                                                 
  Free B  →  [free 8] [free 8] [free 8]   ← three 8-byte blocks, NOT one 24-byte block                                                                                             
                                                                                                                                                                                   
  Any subsequent 16-byte allocation will fail even though there are 24 contiguous free bytes. Under sustained alloc/dealloc workloads, the heap will fragment into tiny unusable   
  blocks until OOM. The comment "dealloc [will eventually] return blocks to the free list" in the GlobalAlloc safety justification tacitly acknowledges this is incomplete — but   
  the safety claim depends on correctness that doesn't yet exist.                                                                                                                  
                  
  After the walk in dealloc, you need something like:                                                                                                                              
   
  // Coalesce with next block if adjacent                                                                                                                                          
  if let Some(next) = unsafe { (*new_free_block_ptr).next } {                                                                                                                      
      if new_free_block_ptr as usize + size == next.as_ptr() as usize {
          unsafe {                                                                                                                                                                 
              (*new_free_block_ptr).size += (*next.as_ptr()).size;
              (*new_free_block_ptr).next = (*next.as_ptr()).next;                                                                                                                  
          }       
      }                                                                                                                                                                            
  }               
  // Coalesce with previous block if adjacent
  // (requires prev_free_block is a *mut FreeBlock, not just &mut Option<NonNull>)                                                                                                 
                                                                                                                                                                                   
  2. Memory leak: padding bytes permanently lost on small-padding allocations                                                                                                      
                                                                                                                                                                                   
  When 0 < padding < size_of::<FreeBlock>():                                                                                                                                       
                  
  - alloc includes the padding in the consumed region (the entire free block from its start through padding + unpadded_req_size is consumed).                                      
  - alloc returns the pointer after the padding: free_block_ptr + padding.
  - dealloc receives that pointer and creates a free block of size align_up(layout.size(), BASE_ALIGN).                                                                            
                                                                                                                                                                                   
  The padding bytes between the original free block start and the returned pointer are never returned to any free block. They are permanently leaked. Over time on a system with   
  mixed alignment requests, this bleeds memory silently.                                                                                                                           
                                                                                                                                                                                   
  3. DEALLOCATED_BYTES counter — only incremented on the tail/fallthrough path                                                                                                     
   
  // Line 346-348: early return when inserting in the middle of the list                                                                                                           
  *prev_free_block_ref = unsafe { Some(NonNull::new_unchecked(new_free_block_ptr)) };                                                                                              
  unsafe { (*new_free_block_ptr).next = current_free_block };
  return;  // ← skips the DEALLOCATED_BYTES increment below                                                                                                                        
                                                                                                                                                                                   
  // ...
                                                                                                                                                                                   
  // Line 371-372: only reached on tail insertion                                                                                                                                  
  #[cfg(test)]
  let _ = DEALLOCATED_BYTES.fetch_add(size as u32, Ordering::Relaxed);                                                                                                             
                                                                                                                                                                                   
  Any deallocation that inserts into the middle of the list (the common case) doesn't increment the counter. Your benchmark's Allocated: X bytes line is showing ALLOCATED -       
  DEALLOCATED and will grossly overcount because most deallocations aren't tracked.                                                                                                
                                                                                                                                                                                   
  ---             
  🟠 Severity: Significant Design Issues
                                                                                                                                                                                   
  4. The padding-split continue trick creates permanent micro-fragments
                                                                                                                                                                                   
  When padding >= size_of::<FreeBlock>(), the code shrinks the current block to exactly padding bytes and creates a new block after it, then continues. This works, but the        
  shrunken block (now padding bytes, typically 8–12 bytes) remains permanently in the free list. It's too small for most allocations and, without coalescing, will never be        
  reclaimed. Each alignment-mismatched allocation pollutes the list with one more useless micro-fragment, degrading walk time.                                                     
                  
  5. init() has no release-mode protection against double-init                                                                                                                     
   
  debug_assert!(free_list_head.is_none());                                                                                                                                         
                  
  In release builds this is a no-op. A second call to init() silently overwrites the free list head, losing all metadata about allocated blocks. This should be a hard assert! or  
  better, an AtomicBool initialized-once flag.
                                                                                                                                                                                   
  6. No alignment assertion on __heap_start                                                                                                                                        
   
  let free_block_ptr = &raw const __heap_start as *mut FreeBlock;                                                                                                                  
  // Safety: Heap start is valid for writes and aligned                                                                                                                            
                                                                                                                                                                                   
  The safety comment claims alignment, but nothing verifies it. FreeBlock requires 4-byte alignment (riscv32). If the linker script gets this wrong, you have instant UB. Add a    
  runtime assert:                                                                                                                                                                  
                                                                                                                                                                                   
  assert!(free_block_ptr as usize % core::mem::align_of::<FreeBlock>() == 0,
      "heap start is not aligned for FreeBlock");                                                                                                                                  
   
  7. O(n) alloc + O(n) dealloc with monotonically growing n                                                                                                                        
                  
  First-fit linked-list walk is O(n) per operation. Without coalescing, the number of free blocks only ever increases (each dealloc adds one, alloc can add zero or one). Under    
  sustained workloads this is an unbounded latency hazard. For a kernel allocator running under an IRQ spinlock, this is especially dangerous — long hold times block all other
  cores and all interrupt handling.                                                                                                                                                
                  
  8. BASE_ALIGN is not portable

  const BASE_ALIGN: usize = 8;
  const _: () = assert!(core::mem::size_of::<FreeBlock>() <= BASE_ALIGN);                                                                                                          
                                                                                                                                                                                   
  On riscv32, size_of::<FreeBlock>() is 4 + 4 = 8, so this passes by exactly one byte. On any 64-bit target (even for testing), FreeBlock is 16 bytes and the assertion fails.     
  BASE_ALIGN should be derived:                                                                                                                                                    
                                                                                                                                                                                   
  const BASE_ALIGN: usize = core::mem::size_of::<FreeBlock>();
  // or: max(8, size_of::<FreeBlock>())                                                                                                                                            
                                                                                                                                                                                   
  ---                                                                                                                                                                              
  🟡 Severity: Idiomatic / Hygiene Issues                                                                                                                                          
                                         
  9. Raw-pointer field writes on potentially-uninitialized memory
                                                                                                                                                                                   
  Throughout both alloc and dealloc, new FreeBlocks are created by writing fields individually:                                                                                    
                                                                                                                                                                                   
  (*new_free_block_ptr).size = size;                                                                                                                                               
  // ... later ...
  (*new_free_block_ptr).next = current_free_block;
                                                                                                                                                                                   
  Between these two writes, the struct is partially initialized — next holds whatever was in memory. If a panic or interrupt fires between them (unlikely under the IRQ lock, but a
   code maintenance hazard), the list is corrupted. core::ptr::write on the whole struct is clearer and atomic at the struct level:                                                
                                                                                                                                                                                   
  core::ptr::write(new_free_block_ptr, FreeBlock {
      next: current_free_block,                                                                                                                                                    
      size,
  });                                                                                                                                                                              
                  
  10. unsafe impl Sync for FreeList bypasses IrqSpinLock's own safety design                                                                                                       
   
  IrqSpinLock<T> already provides unsafe impl<T: Send> Sync. The reason it doesn't apply here is that NonNull<FreeBlock> is !Send. Rather than blanket-overriding Sync on the outer
   FreeList (bypassing the lock's design), the proper fix is a newtype wrapper:
                                                                                                                                                                                   
  /// Wrapper asserting that our heap-internal pointers are safe to send across threads.
  struct FreeBlockPtr(Option<NonNull<FreeBlock>>);                                                                                                                                 
  // Safety: FreeBlock pointers reference heap memory which is globally addressable                                                                                                
  // and only accessed under the IrqSpinLock.                                                                                                                                      
  unsafe impl Send for FreeBlockPtr {}                                                                                                                                             
                                                                                                                                                                                   
  Then IrqSpinLock<FreeBlockPtr> is Sync automatically, and the safety argument is localized to the right abstraction level.                                                       
                  
  11. Duplicated cursor pattern between alloc and dealloc                                                                                                                          
                  
  Both functions implement the exact same prev_free_block_ref + current_free_block walk pattern with identical unsafe pointer chasing. This should be a Cursor helper (or at least 
  a shared walk method) to avoid the duplication and reduce the surface area for pointer bugs.
                                                                                                                                                                                   
  12. Test instrumentation pollutes the hot path

  The #[cfg(test)] atomic increments are interleaved with production alloc logic (lines 253–262), making the core algorithm harder to read. Consider extracting these into a       
  #[cfg(test)] helper method, or using a single #[cfg(test)] block with a struct that bundles the stats.
                                                                                                                                                                                   
  13. dealloc writes .size before acquiring the lock                                                                                                                               
   
  unsafe fn dealloc(&self, dealloc_ptr: *mut u8, layout: core::alloc::Layout) {                                                                                                    
      let new_free_block_ptr = dealloc_ptr as *mut FreeBlock;                                                                                                                      
      let size = align_up(layout.size(), BASE_ALIGN);
      unsafe { (*new_free_block_ptr).size = size; }   // ← write here                                                                                                              
      let mut free_list_head = self.head.lock();       // ← lock here
                                                                                                                                                                                   
  The write to (*new_free_block_ptr).size happens before acquiring the lock. On a dual-core system, another core could be walking the free list at this exact moment. While the    
  memory being written to is technically "allocated" (so no other core should be touching it), if there's any corruption or double-free, this write-before-lock order means the    
  corruption happens silently outside any mutual exclusion. Moving the lock acquisition before the write costs nothing and is defensively sounder.                                 
                  
  14. Stale/dead comments                                                                                                                                                          
   
  - Line 500–506: Commented-out BUMP_ALLOCATOR.reset() references a bump allocator that no longer exists in this module. Dead code in comments rots fast.                          
  - Many comments restate the obvious: // Get the free block pointer above let free_block_ptr = block.as_ptr() adds no value.
                                                                                                                                                                                   
  ---             
  Summary — Priority Order                                                                                                                                                         
                          
  ┌───────┬──────────────────────────────┬──────────────────────────────────────────────────────────────────────┐
  │   #   │            Issue             │                                Impact                                │                                                                  
  ├───────┼──────────────────────────────┼──────────────────────────────────────────────────────────────────────┤
  │ 1     │ No coalescing                │ Fatal: heap will fragment to OOM under any real workload             │                                                                  
  ├───────┼──────────────────────────────┼──────────────────────────────────────────────────────────────────────┤
  │ 2     │ Padding bytes leaked         │ Memory leak: silent, cumulative, proportional to alignment diversity │                                                                  
  ├───────┼──────────────────────────────┼──────────────────────────────────────────────────────────────────────┤                                                                  
  │ 3     │ DEALLOCATED_BYTES bug        │ Incorrect metrics: masks the above two issues in benchmarks          │                                                                  
  ├───────┼──────────────────────────────┼──────────────────────────────────────────────────────────────────────┤                                                                  
  │ 4     │ Micro-fragment pollution     │ Perf degradation: O(n) walks grow without bound                      │
  ├───────┼──────────────────────────────┼──────────────────────────────────────────────────────────────────────┤                                                                  
  │ 5     │ No release double-init guard │ Silent corruption risk                                               │
  ├───────┼──────────────────────────────┼──────────────────────────────────────────────────────────────────────┤                                                                  
  │ 6     │ No heap alignment check      │ Potential UB from linker misconfiguration                            │
  ├───────┼──────────────────────────────┼──────────────────────────────────────────────────────────────────────┤                                                                  
  │ 7     │ O(n) under IRQ lock          │ Latency hazard for real-time/interrupt responsiveness                │
  ├───────┼──────────────────────────────┼──────────────────────────────────────────────────────────────────────┤                                                                  
  │ 8     │ BASE_ALIGN not portable      │ Build breakage on 64-bit targets                                     │
  ├───────┼──────────────────────────────┼──────────────────────────────────────────────────────────────────────┤                                                                  
  │ 9     │ Field-by-field init          │ Maintenance hazard, partial-init window                              │
  ├───────┼──────────────────────────────┼──────────────────────────────────────────────────────────────────────┤                                                                  
  │ 10    │ Sync bypass                  │ Unsound abstraction layering                                         │
  ├───────┼──────────────────────────────┼──────────────────────────────────────────────────────────────────────┤                                                                  
  │ 11–14 │ Idiom / hygiene              │ Readability and maintainability                                      │
  └───────┴──────────────────────────────┴──────────────────────────────────────────────────────────────────────┘                                                                  
   
  The allocator is a solid foundation — the address-ordered insertion, the padding-split trick, and the lock design show good understanding of the problem space. But issue #1 (no 
  coalescing) is a showstopper that makes the allocator unsuitable for any workload beyond a demo. Fixing it will also naturally address issues #2 and #4 if the coalescing logic
  considers the predecessor block. I'd tackle #1–3 as a single coherent change. 
