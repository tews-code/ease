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
        name: "wait-exit",
        image: Image::Blob(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../ease-user/target/riscv32imac-unknown-none-elf/debug/",
            "wait-exit.bin"
        ))),
    },
    Program {
        name: "echo",
        image: Image::Flash(echo),
    },
    Program {
        name: "user_canary_stomp",
        image: Image::Flash(user_canary_stomp),
    },
    Program {
        name: "user_mutex_block",
        image: Image::Flash(user_mutex_block),
    },
    Program {
        name: "user_fault_now",
        image: Image::Flash(user_fault_now),
    },
    Program {
        name: "user_return_test",
        image: Image::Flash(user_return_test),
    },
    Program {
        name: "user_spin_forever",
        image: Image::Flash(user_spin_forever),
    },
    Program {
        name: "user_test",
        image: Image::Flash(user_test),
    },
    Program {
        name: "user_register_probe",
        image: Image::Flash(user_register_probe),
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

/// Echos character read from the keyboard until `x` at which
/// point it quits
#[unsafe(link_section = ".user_text")]
pub extern "C" fn echo() {
    loop {
        if let Some(ch) = user_get_char() {
            if ch == b'x' as usize {
                user_exit();
            }
            user_put_char(ch);
        }
    }
}

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

/// Issues the test-only TEST_MUTEX_BLOCK syscall: the kernel side takes
/// a static mutex and parks holding it on a never-signalled completion.
/// Used by the boundary-kill test to prove that fault-killing the
/// holder frees the mutex (guard dropped via Err propagation) instead
/// of orphaning it. In non-test builds the syscall number is unknown
/// to the kernel, so this program is never spawned outside tests.
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_mutex_block() {
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") syscall::TEST_MUTEX_BLOCK,
        );
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Overwrites the canary word at the base of its own user stack — legal
/// under PMP, since the stack is the program's own memory — then spins
/// until preempted. The scheduler's slice_ended canary check must catch
/// the corruption and fault-kill the whole process. The stack base is
/// found by rounding sp down to the stack's 4KB size class (valid
/// because buddy regions are size-aligned); the `sp - 1` keeps the
/// rounding inside the region even if sp still sits exactly at the top.
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_canary_stomp() {
    unsafe {
        let sp: usize;
        core::arch::asm!("mv {}, sp", out(reg) sp);
        core::ptr::write_volatile(((sp - 1) & !0xFFF) as *mut usize, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Takes a PMP access fault immediately: no user region covers low
/// memory, so the load traps. Used by the fault-kills-process test.
/// (Reads address 4 rather than 0 so we exercise an ordinary unmapped
/// access, not anything null-pointer-special.)
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

/// Executes an illegal instruction immediately: `unimp` is a guaranteed
/// invalid encoding, so this traps as ILLEGAL_INSTRUCTION rather than a
/// PMP access fault. Used by the illegal-instruction-kills-process test
/// to prove U-mode illegal instructions fault-kill the process instead
/// of panicking the kernel.
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_illegal_now() {
    unsafe {
        core::arch::asm!("unimp");
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

/// A user thread that does a little work and returns normally — no explicit
/// `ecall`. The `ret` lands in [`user_exit`] (by TrapFrame::init_for_user_entry), which issues the EXIT syscall, so this still exits cleanly.
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_return_test() {
    core::hint::black_box(0u32);
}

/// Number of registers [`user_register_probe`] checks: s0–s11, t0–t6, a2–a7.
const PROBE_REGS: usize = 25;

/// Expected value of probe register `i` after the syscall. Distinct per
/// register so a swap shows up, and distinct per class so a stale value
/// from another register is recognisable in the mismatch mask.
#[unsafe(link_section = ".user_text")]
extern "C" fn probe_expected(i: usize) -> usize {
    if i < 12 {
        0x5000_0000 + i // s0..s11
    } else if i < 19 {
        0x7000_0000 + (i - 12) // t0..t6
    } else if i < 24 {
        0x6000_0000 + (i - 19 + 2) // a2..a6
    } else {
        syscall::GET_CHAR // a7 carried the syscall number
    }
}

/// Regression test for the single trap-return path: a blocking syscall must
/// hand back every register except the two results.
///
/// Loads a distinctive value into s0–s11 (callee-saved: the user ABI relies
/// on these surviving) and into t0–t6 and a2–a7 (caller-saved: the full
/// frame restore preserves them too), blocks in GET_CHAR until the kernel
/// test injects a key, then dumps all 25 registers and compares. All match:
/// exits cleanly. Any mismatch: prints REG MISMATCH with a bitmask (bit i =
/// register i in [`probe_expected`]'s order) and spins, so the kernel-side
/// test times out and the console says why.
///
/// Everything runs from `.user_text`: no calls into `core`, which lives in
/// kernel text that U-mode cannot execute.
#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_register_probe() {
    // Uninitialised on purpose: zero-filling an array this size becomes a
    // memset call into kernel text. The asm writes every slot before it is read.
    let mut regs = core::mem::MaybeUninit::<[usize; PROBE_REGS]>::uninit();
    unsafe {
        core::arch::asm!(
            // We own s0–s11 for the duration: save the compiler's copies
            "addi sp, sp, -64",
            "sw s0,   0(sp)", "sw s1,   4(sp)", "sw s2,   8(sp)", "sw s3,  12(sp)",
            "sw s4,  16(sp)", "sw s5,  20(sp)", "sw s6,  24(sp)", "sw s7,  28(sp)",
            "sw s8,  32(sp)", "sw s9,  36(sp)", "sw s10, 40(sp)", "sw s11, 44(sp)",
            // The dump buffer pointer arrives in a0, which the syscall
            // overwrites with its error code, so park it on the stack
            "sw a0,  48(sp)",
            // Distinctive value per register
            "li s0, 0x50000000", "li s1, 0x50000001", "li s2,  0x50000002", "li s3,  0x50000003",
            "li s4, 0x50000004", "li s5, 0x50000005", "li s6,  0x50000006", "li s7,  0x50000007",
            "li s8, 0x50000008", "li s9, 0x50000009", "li s10, 0x5000000a", "li s11, 0x5000000b",
            "li t0, 0x70000000", "li t1, 0x70000001", "li t2, 0x70000002", "li t3, 0x70000003",
            "li t4, 0x70000004", "li t5, 0x70000005", "li t6, 0x70000006",
            "li a2, 0x60000002", "li a3, 0x60000003", "li a4, 0x60000004", "li a5, 0x60000005",
            "li a6, 0x60000006",
            "li a7, {get_char}",
            // Block until the kernel test injects a key
            "ecall",
            // Dump every register under test before touching any of them
            "lw a0,  48(sp)",
            "sw s0,   0(a0)", "sw s1,   4(a0)", "sw s2,   8(a0)", "sw s3,  12(a0)",
            "sw s4,  16(a0)", "sw s5,  20(a0)", "sw s6,  24(a0)", "sw s7,  28(a0)",
            "sw s8,  32(a0)", "sw s9,  36(a0)", "sw s10, 40(a0)", "sw s11, 44(a0)",
            "sw t0,  48(a0)", "sw t1,  52(a0)", "sw t2,  56(a0)", "sw t3,  60(a0)",
            "sw t4,  64(a0)", "sw t5,  68(a0)", "sw t6,  72(a0)",
            "sw a2,  76(a0)", "sw a3,  80(a0)", "sw a4,  84(a0)", "sw a5,  88(a0)",
            "sw a6,  92(a0)", "sw a7,  96(a0)",
            // Give the compiler its callee-saved registers back
            "lw s0,   0(sp)", "lw s1,   4(sp)", "lw s2,   8(sp)", "lw s3,  12(sp)",
            "lw s4,  16(sp)", "lw s5,  20(sp)", "lw s6,  24(sp)", "lw s7,  28(sp)",
            "lw s8,  32(sp)", "lw s9,  36(sp)", "lw s10, 40(sp)", "lw s11, 44(sp)",
            "addi sp, sp, 64",
            get_char = const syscall::GET_CHAR,
            inout("a0") regs.as_mut_ptr().cast::<usize>() => _,
            out("a1") _, out("a2") _, out("a3") _, out("a4") _, out("a5") _,
            out("a6") _, out("a7") _,
            out("t0") _, out("t1") _, out("t2") _, out("t3") _, out("t4") _,
            out("t5") _, out("t6") _,
        );
    }
    // Safety: the asm stored all PROBE_REGS slots
    let regs = unsafe { regs.assume_init_ref() };
    let mut mismatch: usize = 0;
    let mut i = 0;
    while i < PROBE_REGS {
        if regs[i] != probe_expected(i) {
            mismatch |= 1 << i;
        }
        i += 1;
    }
    if mismatch == 0 {
        // Positive signal for the kernel-side test: a fault-kill would also
        // drain the process, so a clean exit alone proves nothing
        const OK: [u8; 7] = *b"REG OK\n";
        let mut i = 0;
        while i < 7 {
            user_put_char(OK[i] as usize);
            i += 1;
        }
        user_exit();
    }
    // Fixed-size arrays and index loops only: `.len()` and iterators are
    // calls into core, which U-mode cannot execute
    const MSG: [u8; 15] = *b"REG MISMATCH 0x";
    const HEX: [u8; 16] = *b"0123456789abcdef";
    let mut i = 0;
    while i < 15 {
        user_put_char(MSG[i] as usize);
        i += 1;
    }
    let mut shift = 32;
    while shift > 0 {
        shift -= 4;
        user_put_char(HEX[(mismatch >> shift) & 0xf] as usize);
    }
    user_put_char(b'\n' as usize);
    // Not `spin_loop()`: that is a call into core, which U-mode cannot execute
    loop {
        unsafe { core::arch::asm!("nop") };
    }
}

#[unsafe(link_section = ".user_text")]
pub fn user_get_char() -> Option<usize> {
    let error: usize;
    let value: usize;
    unsafe {
        core::arch::asm!(
            "ecall",
            out("a0") error,
            out("a1") value,
            in("a7") syscall::GET_CHAR,
        );
    }
    if error == 0 { Some(value) } else { None }
}

#[unsafe(link_section = ".user_text")]
pub extern "C" fn user_put_char(b: usize) {
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
