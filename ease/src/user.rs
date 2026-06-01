//! User mode functions

use core::arch::naked_asm;

#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_test() {
    unsafe {
        core::arch::asm!(
            "li a7, {exit}",
            "ecall",
            exit = const crate::syscall::EXIT
        );
    }
    loop {
        core::hint::spin_loop();
    }
}

unsafe extern "C" {
    static __user_text_start: u8;
}

#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_fault_test() {
    unsafe {
        core::ptr::read_volatile(&raw const __user_text_start);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// A user thread that does a little work and returns normally — no explicit
/// `ecall`. The `ret` lands in [`user_exit`] (installed as `ra` by
/// `user_entry`), which issues the EXIT syscall, so this still exits cleanly.
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_return_test() {
    core::hint::black_box(0u32);
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_exit() -> ! {
    naked_asm!(
        "li a7, {exit}",
        "ecall",
        "unimp",
        exit = const crate::syscall::EXIT,
    );
}
