//! Test case - requests a heap from kernel and uses heap
//! allocation to create a Vec.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use alloc::boxed::Box;
use ease_ulib::heap_init;
use ease_ulib::println;

/// All user programs _must_ export "main"
#[unsafe(no_mangle)]
pub extern "C" fn main() {
    let size = heap_init(4095).expect("there should be enough heap space");
    assert!(size == 4096, "kernel should round to NAPOT");
    let mut v = Vec::<u16>::new();
    for i in 1..=100 {
        v.push(i);
    }
    assert_eq!(v.iter().sum::<u16>(), 5050, "missing vec items");
    drop(v);
    let b = Box::new(0usize);
    println!("Box address: {:p}", b);
    println!("HEAP OK");
}
