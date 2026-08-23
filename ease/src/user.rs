//! User mode functions

use core::arch::naked_asm;

use ease_abi::syscall;

pub(crate) const PROGRAMS: Programs = Programs(&[
    Program {
        name: "shell",
        image: Image::Blob(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../ease-user/target/riscv32imac-unknown-none-elf/debug/",
            "shell.bin"
        ))),
    },
    Program {
        name: "echo",
        image: Image::Flash(echo),
    },
    Program {
        name: "user_test",
        image: Image::Flash(user_test),
    },
    Program {
        name: "user_spin_forever",
        image: Image::Flash(user_spin_forever),
    },
]);

/// User programs are either built into the OS binary as functions and loaded from flash
/// or binary blobs created by the ease-user package and loaded as bytes
#[derive(Clone, Copy)]
pub(crate) enum Image {
    Blob(&'static [u8]),    // The blob holds the entire user program as a byte array
    Flash(extern "C" fn()), // Function linked into the kernel image
}
/// A program and its name for lookup
struct Program {
    name: &'static str,
    image: Image,
}
/// Table of user programs
pub(crate) struct Programs(&'static [Program]);

impl Programs {
    pub(crate) fn find(&self, name: &str) -> Option<Image> {
        for program in self.0.iter() {
            if program.name == name {
                return Some(program.image); // Only return the first if there are duplicate names
            }
        }
        None
    }
}

mod lib {
    use ease_abi::syscall;

    #[allow(dead_code)]
    #[unsafe(link_section = ".user_text")]
    pub(super) fn put_char(b: u8) {
        unsafe {
            core::arch::asm!(
                "ecall",
                in("a0") b,
                in("a7") syscall::PUT_CHAR,
            );
        }
    }

    #[allow(dead_code)]
    #[unsafe(link_section = ".user_text")]
    pub(super) fn get_key() -> Option<usize> {
        let mut key: usize = 0;
        unsafe {
            core::arch::asm!(
                "ecall",
                clobber_abi("C"),
                out("a0") key,
                in("a7") syscall::GET_CHAR,
            );
        }
        if key == 0 { None } else { Some(key) }
    }
}

/// Echos character read from the keyboard until `x` at which
/// point it quits
#[allow(dead_code)]
#[unsafe(link_section = ".user_text")]
pub extern "C" fn echo() {
    let mut ch: usize = 0;
    loop {
        unsafe {
            core::arch::asm!(
                "ecall",
                clobber_abi("C"),
                out("a0") ch,
                in("a7") syscall::GET_CHAR,
            );
        }
        match ch {
            0 => {}
            val if val == b'x' as usize => user_exit(),
            _ => {
                user_print(ch);
                ch = 0;
            }
        }
    }
}

#[allow(dead_code)]
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_print_a() {
    loop {
        unsafe {
            core::arch::asm!(
                "ecall",
                inout("a0") b'A' as usize => _,
                in("a7") syscall::PUT_CHAR,
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
                in("a7") syscall::PUT_CHAR,
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
            in("a7") syscall::EXIT,
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

#[allow(dead_code)]
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_print(b: usize) {
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a0") b,
            in("a7") syscall::PUT_CHAR,
        );
    }
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_exit() -> ! {
    naked_asm!(
        "li a7, {exit}",
        "ecall",
        "unimp",
        exit = const syscall::EXIT,
    );
}
