//! User mode functions

use core::arch::naked_asm;

#[cfg(not(test))]
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_print_a() {
    loop {
        unsafe {
            core::arch::asm!(
                "ecall",
                inout("a0") b'A' as usize => _,
                in("a7") crate::syscall::PUT_CHAR,
            );
        }
    }
}

#[cfg(not(test))]
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_print_b() {
    loop {
        unsafe {
            core::arch::asm!(
                "ecall",
                inout("a0") b'B' as usize => _,
                in("a7") crate::syscall::PUT_CHAR,
            );
        }
    }
}

#[allow(dead_code)]
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_test() {
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") crate::syscall::EXIT,
        );
    }
    loop {
        core::hint::spin_loop();
    }
}

unsafe extern "C" {
    static __user_text_start: u8;
}

#[allow(dead_code)]
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_fault_test() {
    unsafe {
        core::ptr::read_volatile(&raw const __user_text_start);
    }
    loop {
        core::hint::spin_loop();
    }
}

#[allow(dead_code)]
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
