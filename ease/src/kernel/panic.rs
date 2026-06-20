//! Panic handler

use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::interrupts::wait_for_interrupt;
use crate::kernel::paintstack::check_canary;
use crate::kernel::percpu;
#[cfg(feature = "paint-stack")]
use crate::kernel::stack::print_stack_watermark;
#[cfg(test)]
use crate::qemu;

pub(super) static STOP: AtomicBool = AtomicBool::new(false);
pub(super) static PARKED: AtomicBool = AtomicBool::new(false);

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
    {
        // No more interrupts
        crate::arch::interrupts::disable();
        // Tell other hart to stop
        STOP.store(true, Ordering::Relaxed);
        crate::kernel::ipi::send(crate::arch::hart_id() ^ 1);
        // Wait until other hart has parked
        let mut counter = 0;
        while !PARKED.load(Ordering::Acquire) && counter < 1_000 {
            counter += 1;
            core::hint::spin_loop();
        }
        // Now go ahead with panic info dump
        // Check that the stack canary has been set up
        let stack_base = percpu::current_stack_base();
        let stack_ok = if stack_base.is_null() {
            None
        } else {
            // Safety: stack base is set in percpu from an aligned stack address either from the linker (for idle) or from buddy allocation
            match unsafe { check_canary(stack_base.addr()) } {
                Ok(()) => Some(true),
                Err(_) => Some(false),
            }
        };

        dprintln!("PANIC: {info}");
        dprintln!(
            "Stack canary: {}",
            match stack_ok {
                None => "unavailable",
                Some(true) => "intact",
                Some(false) => "corrupted",
            }
        );

        #[cfg(feature = "paint-stack")]
        // Safety: base and top addresses are aligned by linker and valid for reads
        unsafe {
            unsafe extern "C" {
                static __hart0_irq_stack_base: u8;
                static __hart0_irq_stack_top: u8;
                static __hart1_irq_stack_base: u8;
                static __hart1_irq_stack_top: u8;
                static __hart0_idle_stack_base: u8;
                static __hart0_idle_stack_top: u8;
                static __hart1_idle_stack_base: u8;
                static __hart1_idle_stack_top: u8;
            }
            print_stack_watermark(
                "Irq Hart",
                0,
                &raw const __hart0_irq_stack_base as usize,
                &raw const __hart0_irq_stack_top as usize,
            );
            print_stack_watermark(
                "Irq Hart",
                1,
                &raw const __hart1_irq_stack_base as usize,
                &raw const __hart1_irq_stack_top as usize,
            );
            print_stack_watermark(
                "Idle",
                0,
                &raw const __hart0_idle_stack_base as usize,
                &raw const __hart0_idle_stack_top as usize,
            );
            print_stack_watermark(
                "Idle",
                1,
                &raw const __hart1_idle_stack_base as usize,
                &raw const __hart1_idle_stack_top as usize,
            );
        }

        use core::fmt::Write;

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
    // Dump the trace before any path that exits/loops, so it appears in
    // both the test build (which exit_failure()s below) and normal runs.
    #[cfg(feature = "trace")]
    crate::sched::trace::dump_trace();

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
        core::hint::spin_loop();
    }
}
