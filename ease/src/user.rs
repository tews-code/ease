//! User mode functions

pub fn user_test() -> ! {
    unsafe {
        core::arch::asm!("li a7, 0", "ecall");
    }
    loop {
        core::hint::spin_loop();
    }
}
