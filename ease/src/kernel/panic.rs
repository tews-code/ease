//! Panic handler

use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::hart_id;
use crate::arch::interrupts::wait_for_interrupt;
use crate::kernel::ipi;
use crate::kernel::percpu;
use crate::kernel::stack;
#[cfg(test)]
use crate::qemu;

pub(crate) static STOP: AtomicBool = AtomicBool::new(false);
pub(crate) static PARKED: AtomicBool = AtomicBool::new(false);
pub(super) static PANIC_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    static __hart0_idle_stack_base: u8;
    static __hart1_idle_stack_base: u8;
    static __hart0_irq_stack_base: u8;
    static __hart1_irq_stack_base: u8;
}

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
        use core::fmt::Write;
        let mut console = DirectConsoleWriter { x: 0, y: 0 };

        // If there is already a panic ongoing, just print this message and stop - we are panicing inside a panic.
        if PANIC_IN_PROGRESS
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            dprintln!("PANIC: {info}");
            let _ = write!(console, "PANIC: {info}");
            #[cfg(test)]
            qemu::exit_failure();
            #[cfg(not(test))]
            loop {
                wait_for_interrupt();
            }
        }
        // No more interrupts
        crate::arch::interrupts::disable();
        // Tell other hart to stop
        STOP.store(true, Ordering::Relaxed);
        ipi::send(ipi::RESCHEDULE);
        // Wait until other hart has parked
        let mut counter = 0;
        while !PARKED.load(Ordering::Acquire) && counter < 1_000 {
            counter += 1;
            core::hint::spin_loop();
        }

        // Now go ahead with panic info dump
        dprintln!("PANIC: {info}");
        let _ = write!(console, "PANIC: {info}");

        // Check the stack canaries
        let this_hart_idle_stack_base = match hart_id() {
            0 => &raw const __hart0_idle_stack_base,
            1 => &raw const __hart1_idle_stack_base,
            _ => unreachable!("only have two harts"),
        };
        dprint!("This HART's idle stack canary: ");
        let _ = write!(console, "This HART's idle stack canary: ");
        // Safety: The idle/boot stack base is set by the linker script to be aligned and is valid for reads
        let idle_stack_status = unsafe { stack::check_canary(this_hart_idle_stack_base.addr()) };
        match idle_stack_status {
            Ok(_) => {
                dprintln!("intact");
                let _ = write!(console, "intact");
            }
            Err(b) => {
                dprintln!("corrupted: read {b:x}");
                let _ = write!(console, "corrupted: read {b:x}");
            }
        }
        // Check the stack canaries
        let this_hart_irq_stack_base = match hart_id() {
            0 => &raw const __hart0_irq_stack_base,
            1 => &raw const __hart1_irq_stack_base,
            _ => unreachable!("only have two harts"),
        };
        dprint!("This HART's IRQ stack canary: ");
        let _ = write!(console, "This HART's IRQ stack canary: ");
        // Safety: The IRQ stack base is set by the linker script to be aligned and is valid for reads
        let idle_stack_status = unsafe { stack::check_canary(this_hart_irq_stack_base.addr()) };
        match idle_stack_status {
            Ok(_) => {
                dprintln!("intact");
                let _ = write!(console, "intact");
            }
            Err(b) => {
                dprintln!("corrupted: read {b:x}");
                let _ = write!(console, "corrupted: read {b:x}");
            }
        }
        if percpu::try_current_thread().is_some() {
            // Check the stack canaries
            dprint!("Kernel stack canary: ");
            let _ = write!(console, "Kernel stack canary: ");
            // Safety: The kernel stack base is set by the linker script or buddy to be aligned and valid for reads
            let kernel_stack_status =
                unsafe { stack::check_canary(percpu::current_kernel_stack_base().addr().into()) };
            match kernel_stack_status {
                Ok(_) => {
                    dprintln!("intact");
                    let _ = write!(console, "intact");
                }
                Err(b) => {
                    dprintln!("corrupted: read {b:x}");
                    let _ = write!(console, "corrupted: read {b:x}");
                }
            }
            dprint!("User stack canary: ");
            let _ = write!(console, "User stack canary: ");
            if let Some(user_stack_status) = percpu::current_user_stack_base()
                .map(|base| unsafe { stack::check_canary(base.addr().into()) })
            {
                match user_stack_status {
                    Ok(_) => {
                        dprintln!("intact");
                        let _ = write!(console, "intact");
                    }
                    Err(b) => {
                        dprintln!("corrupted: read {b:x}");
                        let _ = write!(console, "corrupted: read {b:x}");
                    }
                }
            } else {
                dprintln!("Not a user thread");
                let _ = write!(console, "Not a user thread");
            }
        } else {
            dprintln!("Current thread not installed");
            let _ = write!(console, "Current thread not installed");
        }

        #[cfg(feature = "paint-stack")]
        crate::kernel::stack::print_irq_idle_stacks();
    }
    // Dump the trace before any path that exits/loops, so it appears in
    // both the test build (which exit_failure()s below) and normal runs.
    #[cfg(feature = "trace")]
    crate::sched::trace::dump_trace();
    #[cfg(feature = "irqsoff")]
    let _ = crate::kernel::irqsoff::write_report(&mut crate::io::DirectWriter);

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
