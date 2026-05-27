//! User mode functions

#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_test() -> ! {
    unsafe {
        core::arch::asm!("li a7, 0", "ecall");
    }
    loop {
        core::hint::spin_loop();
    }
}

unsafe extern "C" {
    static __heap_pd1_start: u8;
}

#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_fault_test() -> ! {
    unsafe {
        core::ptr::read_volatile(&raw const __heap_pd1_start);
    }
    loop {
        core::hint::spin_loop();
    }
}
