//! Handles user slices to prevent UB
//!
//! Specifically - assembly to copy from user memory to kernel memory (and vice versa)
//! without triggering UB from user process threads concurrent writes. Uses PMP MPRV
//! for hardware protection.

use core::arch::naked_asm;

use crate::arch::csr;

// External functions for fault and fixup labelled addresses
unsafe extern "C" {
    fn _uaccess_copy_from_user_fault();
    fn _uaccess_copy_from_user_fixup();
    fn _uaccess_copy_to_user_fault();
    fn _uaccess_copy_to_user_fixup();
}
/// Number of fault/fixup pairs
const FAULT_FIXUP_COUNT: usize = 2;
/// Static to hold the fault/fixup pairs
static FAULT_FIXUPS: [(unsafe extern "C" fn(), unsafe extern "C" fn()); FAULT_FIXUP_COUNT] = [
    (_uaccess_copy_from_user_fault, _uaccess_copy_from_user_fixup),
    (_uaccess_copy_to_user_fault, _uaccess_copy_to_user_fixup),
];
/// Helper function to return the address of the fixup from a faulting PC
pub(crate) fn fixup_for(faulting_pc: usize) -> Option<usize> {
    for (pc, fixup_addr) in FAULT_FIXUPS.iter() {
        if faulting_pc == *pc as *const () as usize {
            return Some(*fixup_addr as *const () as usize);
        }
    }
    None
}
/// Copies bytes from a user slice to a kernel slice.
/// Caller must ensure the `len` is the minimum of both user buffer
/// and kernel buffer sizes.
///
/// Returns the number of bytes _not_ copied (i.e. remaining). On success
/// this is zero, but if there is a hardware exception it will instead return
/// number of remaining bytes.
///
/// In order to ensure irqsoff tracing, call this within the [crate::kernel::interrupts::with_interrupts_disabled] closure.
///
/// # Safety
/// Caller must ensure that the kernel buffer is valid for len writes and
/// there are no concurrent readers or writers.
#[unsafe(naked)]
#[unsafe(no_mangle)] // To force the compiler to create the symbols; strictly speaking we don't care about mangled names here
pub(crate) unsafe extern "C" fn copy_from_user(src: *const u8, dst: *mut u8, len: usize) -> usize {
    naked_asm!(
        // Disable interrupts and store the current mstatus
        "csrrci a3, mstatus, {MIE}",
        // Create MPRV and MPP mask for u-mode - we can't use immediates above
        // 5 bits so these need to be in register
        "li a4, {MPRV}",
        "li a5, {MPP}",
        // Set MPRV for the duration of this function as if the permissions are
        // appropriate for u-mode they will also be for m-mode.
        // We can then flip MPP between u-mode and m-mode
        // and only restore MPRV at the end of the function to keep the per-byte CSR
        // asm as small as possible.
        "csrs mstatus, a4",
        // If there are zero bytes we are already done
        "beqz a2, _uaccess_copy_from_user_fixup",
        // Copy loop only copies one byte at a time
        "1:",
            // Set MPP to u-mode ahead of copying the byte
            "csrc mstatus, a5",
            ".globl _uaccess_copy_from_user_fault",
            "_uaccess_copy_from_user_fault:", // Need the address for fault on hardware exception in the trap handler
            // Copy one byte
            "lbu a6, 0(a0)",    // Unsigned - zeros the top
            // Restore MPP
            "csrs mstatus, a5",
            // Store the byte in kernel space
            "sb a6, 0(a1)",
            // Decrement the len and advance the pointers
            "addi a0, a0, 1",
            "addi a1, a1, 1",
            "addi a2, a2, -1",
            "bnez a2, 1b",
        ".globl _uaccess_copy_from_user_fixup",
        "_uaccess_copy_from_user_fixup:", // Make the fixup label a global name so that we can reach it from the trap handler
            // Return number of bytes remaining, i.e. zero on success or some if there was a hardware exception
            // and we are in the fixup
            "mv a0, a2",
            // Restore mstatus, which will coincidentally also clear MPRV
            "csrw mstatus, a3",
            "ret",
        MIE = const csr::mstatus::MIE,
        MPP = const csr::mstatus::MPP,
        MPRV = const csr::mstatus::MPRV,
    );
}
/// Copies bytes from a kernel slice to a user slice.
/// Caller must ensure the `len` is the minimum of both user buffer
/// and kernel buffer sizes.
///
/// Returns the number of bytes _not_ copied (i.e. remaining). On success
/// this is zero, but if there is a hardware exception it will instead return
/// number of remaining bytes.
///
/// In order to ensure irqsoff tracing, call this within the [crate::kernel::interrupts::with_interrupts_disabled] closure.
///
/// # Safety
/// Caller must ensure that the kernel buffer is valid for len reads and
/// there are no concurrent writers.
#[unsafe(naked)]
#[unsafe(no_mangle)] // To force the compiler to create the symbols; strictly speaking we don't care about mangled names here
pub(crate) unsafe extern "C" fn copy_to_user(src: *const u8, dst: *mut u8, len: usize) -> usize {
    naked_asm!(
        // Disable interrupts and store the current mstatus
        "csrrci a3, mstatus, {MIE}",
        // Create MPRV and MPP mask for u-mode - we can't use immediates above
        // 5 bits so these need to be in register
        "li a4, {MPRV}",
        "li a5, {MPP}",
        // Set MPRV for the duration of this function as if the permissions are
        // appropriate for u-mode they will also be for m-mode.
        // We can then flip MPP between u-mode and m-mode
        // and only restore MPRV at the end of the function to keep the per-byte CSR
        // asm as small as possible.
        "csrs mstatus, a4",
        // If there are zero bytes we are already done
        "beqz a2, _uaccess_copy_to_user_fixup",
        // Set MPP to m-mode ahead of the copy loop
        "csrs mstatus, a5",
        // Copy loop only copies one byte at a time
        "1:",
            // Copy one byte from the kernel slice
            "lbu a6, 0(a0)",    // Unsigned - zeros the top
            // Set MPP to u-mode ahead of copying the byte
            "csrc mstatus, a5",
            ".globl _uaccess_copy_to_user_fault",
            "_uaccess_copy_to_user_fault:", // Need the address for fault on hardware exception in the trap handler
            // Store the byte in user space
            "sb a6, 0(a1)",
            // Restore MPP
            "csrs mstatus, a5",
            // Decrement the len and advance the pointers
            "addi a0, a0, 1",
            "addi a1, a1, 1",
            "addi a2, a2, -1",
            "bnez a2, 1b",
        ".globl _uaccess_copy_to_user_fixup",
        "_uaccess_copy_to_user_fixup:", // Make the fixup label a global name so that we can reach it from the trap handler
        // Return number of bytes remaining, i.e. zero on success or some if there was a hardware exception
        // and we are in the fixup
        "mv a0, a2",
        // Restore mstatus, which will coincidentally also clear MPRV
        "csrw mstatus, a3",
        "ret",
        MIE = const csr::mstatus::MIE,
        MPP = const csr::mstatus::MPP,
        MPRV = const csr::mstatus::MPRV,
    );
}

// The copy routines are only meaningful against a real U-mode PMP view, so
// each test grants U-mode a 32-byte window (the minimum NAPOT granule) at the
// start of a 64-byte aligned buffer, leaving the second half outside every
// user region. Faults are therefore genuine PMP access faults, routed through
// the trap handler's fixup lookup.
//
// The grant lives in user PMP slot 7, which the scheduler only rewrites when
// it switches to a user thread. Each test holds interrupts off from grant to
// teardown, so it can neither be preempted nor migrate to the other hart
// (PMP is per-hart) while the grant is in place.
#[cfg(all(test, feature = "test-pmp"))]
mod test {
    use super::{copy_from_user, copy_to_user};
    use crate::arch::csr::pmp;
    use crate::arch::interrupts;
    use crate::arch::pmp::Pmp;
    use crate::board;

    const SLOT: usize = 7;
    const WINDOW: usize = 32;

    #[repr(C, align(64))]
    struct UserArea([u8; 2 * WINDOW]);

    fn mstatus() -> usize {
        let value: usize;
        // Safety: reading mstatus has no side effects
        unsafe { core::arch::asm!("csrr {}, mstatus", out(reg) value) };
        value
    }

    /// Run `body` with U-mode granted `permissions` on the first `WINDOW`
    /// bytes of `area`, then revoke the grant. Also checks the copy left
    /// mstatus exactly as it found it (MIE, MPP, and MPRV cleared).
    fn with_user_window<R>(
        area: &mut UserArea,
        permissions: u8,
        body: impl FnOnce(*mut u8) -> R,
    ) -> R {
        let prev = interrupts::disable();
        let base = area.0.as_mut_ptr();
        let mut grant = Pmp::new();
        grant.set_region(SLOT, base.addr(), WINDOW, pmp::NAPOT, permissions);
        grant.activate();
        let before = mstatus();
        let result = body(base);
        let after = mstatus();
        Pmp::new().activate();
        interrupts::restore(prev);
        assert_eq!(after, before, "copy must restore mstatus exactly");
        result
    }

    fn filled(seed: u8) -> UserArea {
        let mut area = UserArea([0; 2 * WINDOW]);
        for (i, byte) in area.0.iter_mut().enumerate() {
            *byte = seed.wrapping_add(i as u8);
        }
        area
    }

    #[test_case]
    fn copy_from_user_copies_accessible_bytes() {
        let mut area = filled(0x10);
        let mut dst = [0u8; WINDOW];
        let remaining = with_user_window(&mut area, board::pmp::R, |user| {
            // Safety: dst is a valid kernel buffer of WINDOW bytes
            unsafe { copy_from_user(user, dst.as_mut_ptr(), WINDOW) }
        });
        assert_eq!(remaining, 0);
        assert_eq!(dst, area.0[..WINDOW]);
    }

    #[test_case]
    fn copy_to_user_copies_accessible_bytes() {
        let mut area = filled(0);
        let src: [u8; WINDOW] = core::array::from_fn(|i| 0xA0u8.wrapping_add(i as u8));
        let remaining = with_user_window(&mut area, board::pmp::R | board::pmp::W, |user| {
            // Safety: src is a valid kernel buffer of WINDOW bytes
            unsafe { copy_to_user(src.as_ptr(), user, WINDOW) }
        });
        assert_eq!(remaining, 0);
        assert_eq!(area.0[..WINDOW], src);
    }

    // Zero length must not touch memory at all: point both directions at the
    // inaccessible half, where any access would fault and return non-zero.
    #[test_case]
    fn zero_length_copies_touch_nothing() {
        let mut area = filled(0x40);
        let mut kernel = [0x55u8; 4];
        let (from, to) = with_user_window(&mut area, board::pmp::R | board::pmp::W, |user| {
            let outside = user.wrapping_add(WINDOW);
            // Safety: zero-length copies; kernel buffer is valid regardless
            unsafe {
                (
                    copy_from_user(outside, kernel.as_mut_ptr(), 0),
                    copy_to_user(kernel.as_ptr(), outside, 0),
                )
            }
        });
        assert_eq!((from, to), (0, 0));
        assert_eq!(kernel, [0x55; 4]);
        assert_eq!(area.0[WINDOW], 0x40u8.wrapping_add(WINDOW as u8));
    }

    #[test_case]
    fn copy_from_inaccessible_user_returns_full_len() {
        let mut area = filled(0x20);
        let mut dst = [0xEEu8; 8];
        let remaining = with_user_window(&mut area, board::pmp::R, |user| {
            // Safety: dst is a valid kernel buffer of 8 bytes
            unsafe { copy_from_user(user.wrapping_add(WINDOW), dst.as_mut_ptr(), dst.len()) }
        });
        assert_eq!(remaining, dst.len());
        assert_eq!(
            dst, [0xEE; 8],
            "nothing may be written after the first fault"
        );
    }

    #[test_case]
    fn copy_to_inaccessible_user_returns_full_len() {
        let mut area = filled(0x30);
        let src = [0xEEu8; 8];
        let remaining = with_user_window(&mut area, board::pmp::R | board::pmp::W, |user| {
            // Safety: src is a valid kernel buffer of 8 bytes
            unsafe { copy_to_user(src.as_ptr(), user.wrapping_add(WINDOW), src.len()) }
        });
        assert_eq!(remaining, src.len());
        assert_eq!(
            area.0[WINDOW..WINDOW + 8],
            filled(0x30).0[WINDOW..WINDOW + 8]
        );
    }

    // A buffer that runs off the end of the window copies the accessible
    // prefix exactly and reports precisely the bytes left behind.
    #[test_case]
    fn copy_from_user_crossing_window_end_is_partial() {
        let mut area = filled(0x60);
        let mut dst = [0u8; 16];
        let remaining = with_user_window(&mut area, board::pmp::R, |user| {
            // Safety: dst is a valid kernel buffer of 16 bytes
            unsafe { copy_from_user(user.wrapping_add(WINDOW - 8), dst.as_mut_ptr(), dst.len()) }
        });
        assert_eq!(remaining, 8);
        assert_eq!(dst[..8], area.0[WINDOW - 8..WINDOW]);
        assert_eq!(dst[8..], [0; 8]);
    }

    #[test_case]
    fn copy_to_user_crossing_window_end_is_partial() {
        let mut area = filled(0);
        let src = [0xC3u8; 16];
        let remaining = with_user_window(&mut area, board::pmp::R | board::pmp::W, |user| {
            // Safety: src is a valid kernel buffer of 16 bytes
            unsafe { copy_to_user(src.as_ptr(), user.wrapping_add(WINDOW - 8), src.len()) }
        });
        assert_eq!(remaining, 8);
        assert_eq!(area.0[WINDOW - 8..WINDOW], [0xC3; 8]);
        assert_eq!(area.0[WINDOW..WINDOW + 8], filled(0).0[WINDOW..WINDOW + 8]);
    }

    // PMP permissions, not just address ranges, are enforced: a read-only
    // window refuses a store from the very first byte.
    #[test_case]
    fn copy_to_read_only_user_is_refused() {
        let mut area = filled(0x70);
        let src = [0xEEu8; 8];
        let remaining = with_user_window(&mut area, board::pmp::R, |user| {
            // Safety: src is a valid kernel buffer of 8 bytes
            unsafe { copy_to_user(src.as_ptr(), user, src.len()) }
        });
        assert_eq!(remaining, src.len());
        assert_eq!(area.0[..8], filled(0x70).0[..8]);
    }
}
