//! User mode functions

use core::arch::naked_asm;

#[allow(dead_code)]
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

#[allow(dead_code)]
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

/// Takes a PMP access fault immediately: no user region covers low
/// memory, so the load traps. Used by the fault-kills-process test.
/// (Reads address 4 rather than 0 so we exercise an ordinary unmapped
/// access, not anything null-pointer-special.)
#[allow(dead_code)]
#[allow(clippy::manual_dangling_ptr)]
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_fault_now() {
    unsafe {
        core::ptr::read_volatile(4 as *const u32);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Test-only: issues the TEST_BLOCK syscall, which parks the thread in kernel
/// space forever. Used to prove fault-kill reaches a Blocked sibling. The
/// spin loop after the ecall is defensive — the kernel never resumes this
/// thread's user code.
#[cfg(all(test, feature = "test-sched"))]
#[allow(dead_code)]
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_block_forever() {
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") crate::syscall::TEST_BLOCK,
        );
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Spins forever and never exits — only dies if the kernel kills it.
/// Used to prove fault-kill reaches sibling threads.
#[allow(dead_code)]
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_spin_forever() {
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
