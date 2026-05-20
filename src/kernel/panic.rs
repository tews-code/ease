//! Panic handler

use crate::arch::STACK_CANARY;
use crate::hal::wait_for_interrupt;
use crate::kernel::percpu;
#[cfg(test)]
use crate::qemu;

mod fb_panic_writer {

    unsafe extern "C" {
        static __fb_addr: u8;
    }

    pub(super) const FONT_WIDTH: usize = 8;
    pub(super) const FONT_HEIGHT: usize = 16;
    pub(super) const FB_WIDTH: usize = 640;
    pub(super) const FB_HEIGHT: usize = 480;

    const FONT_DATA: &[u8] = include_bytes!("../../resources/VGA8.F16");
    const BYTES_PER_GLYPH: usize = 16;

    fn glyph(char_code: u8) -> &'static [u8] {
        let offset = char_code as usize * BYTES_PER_GLYPH;
        &FONT_DATA[offset..offset + BYTES_PER_GLYPH]
    }

    // Helper function to create a slice over the framebuffer
    fn buffer() -> &'static mut [u32] {
        unsafe {
            // Safety: This is unsafe (UB) but we are panic=abort
            core::slice::from_raw_parts_mut(&raw const __fb_addr as *mut u32, FB_WIDTH * FB_HEIGHT)
        }
    }

    /// Set a row of pixels at (x, y) to colours provided in a u32 slice
    fn set_pixels(x: usize, y: usize, colours: &[u32]) {
        if x + colours.len() <= FB_WIDTH && y < FB_HEIGHT {
            let w = FB_WIDTH;
            buffer()[y * w + x..y * w + x + colours.len()].copy_from_slice(colours);
        }
    }

    /// Draw a glyph in VGA 8x16 font. (x, y) are coordinates of top left of the glyph
    ///
    /// - ch is a byte
    #[allow(dead_code)]
    pub(super) fn panic_draw_char(x: usize, y: usize, ch: u8) {
        const FG: u32 = 0xFF0000; // red
        const BG: u32 = 0x000000; // black

        if x + FONT_WIDTH <= FB_WIDTH && y + FONT_HEIGHT <= FB_HEIGHT {
            let glyph_data = glyph(ch);
            for (row, byte) in glyph_data.iter().enumerate() {
                let mut pixels = [0u32; FONT_WIDTH];
                for (column, pixel) in pixels.iter_mut().enumerate() {
                    let bit_set = (byte >> (7 - column)) & 1 != 0;
                    *pixel = if bit_set { FG } else { BG }
                }
                set_pixels(x, y + row, &pixels);
            }
        }
    }
}

// Console writer for direct writing in panic situation.
#[allow(dead_code)]
struct DirectConsoleWriter {
    x: usize, // x pixel position
    y: usize, // y pixel position
}

use fb_panic_writer::{FB_WIDTH, FONT_HEIGHT, FONT_WIDTH, panic_draw_char};

impl core::fmt::Write for DirectConsoleWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if b == b'\n' {
                self.x = 0;
                self.y += FONT_HEIGHT;
                continue;
            }
            panic_draw_char(self.x, self.y, b);
            self.x += FONT_WIDTH;
            // Wrap at screen edge
            if self.x + FONT_WIDTH > FB_WIDTH {
                self.x = 0;
                self.y += FONT_HEIGHT;
            }
        }
        Ok(())
    }
}

// Panic handler writes straight to UART without locking
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    #[cfg(not(test))]
    {
        // Check that the stack canary has been set up
        let stack_base = percpu::current_stack_base();
        let stack_ok = if stack_base.is_null() {
            None
        } else {
            // Safety: current_stack_base is aligned and valid for reading
            Some(unsafe { core::ptr::read_volatile(stack_base as *const usize) } == STACK_CANARY)
        };
        use crate::io::DirectWriter;
        use core::fmt::Write;
        let _ = writeln!(DirectWriter, "PANIC: {info}");
        let _ = writeln!(
            DirectWriter,
            "Stack canary in place: {}",
            match stack_ok {
                None => "unavailable",
                Some(true) => "intact",
                Some(false) => "corrupted",
            }
        );
        let mut console = DirectConsoleWriter { x: 0, y: 0 };
        let _ = write!(console, "PANIC: {info}");
        let _ = writeln!(
            console,
            "Stack canary in place: {}",
            match stack_ok {
                None => "unavailable",
                Some(true) => "intact",
                Some(false) => "corrupted",
            }
        );
    }
    #[cfg(test)]
    {
        use crate::io::DirectWriter;
        use core::fmt::Write;
        let _ = writeln!(DirectWriter, "\x1b[31mfailed\x1b[0m");
        let _ = writeln!(DirectWriter, "Error: {}", info);
        qemu::exit_failure();
    }

    #[allow(unreachable_code)]
    loop {
        wait_for_interrupt();
    }
}
