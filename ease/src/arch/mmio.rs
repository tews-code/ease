//! MMIO helper functions

/// Reads a 8 bit MMIO register at address `base` + `offset`
pub fn read8(base: usize, offset: usize) -> u8 {
    unsafe {
        // Safety:
        // * base + offset is valid for reads
        // * base + offset points to an initialized `u8`
        // * `u8` is Copy
        core::ptr::read_volatile((base + offset) as *const u8)
    }
}

/// Write `value` to the 8 bit MMIO register at address `base` + `offset`
pub fn write8(base: usize, offset: usize, value: u8) {
    unsafe {
        // Safety:
        // * base + offset is valid for writes.
        core::ptr::write_volatile((base + offset) as *mut u8, value)
    }
}

/// Reads a 32 bit MMIO register at address `base` + `offset`
pub fn read32(base: usize, offset: usize) -> u32 {
    assert_eq!((base + offset) % align_of::<u32>(), 0);
    unsafe {
        // Safety:
        // * base + offset is valid for reads
        // * base + offset is 32-bit aligned and offset is 32-bit aligned
        // * base + offset points to an initialized `u32`
        // * `u32` is Copy
        core::ptr::read_volatile((base + offset) as *const u32)
    }
}

/// Write `value` to the 32 bit MMIO register at address `base` + `offset`
pub fn write32(base: usize, offset: usize, value: u32) {
    assert_eq!((base + offset) % align_of::<u32>(), 0);
    unsafe {
        // Safety:
        // * base + offset is valid for writes.
        // * base + offset is properly 32-bit aligned.
        core::ptr::write_volatile((base + offset) as *mut u32, value)
    }
}
