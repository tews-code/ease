//! Device drivers

pub mod clint;
pub mod console;
pub mod font;
pub mod plic;
pub mod ramfb;
pub mod render;
pub mod uart;
pub mod virtio;

use crate::drivers::console::Console;
use crate::drivers::ramfb::FrameBuffer;
use crate::drivers::render::{FrameBufferRenderer, Renderer};
use crate::kernel::sync::SpinLock;

pub static DISPLAY: SpinLock<DisplayManager<FrameBufferRenderer>> =
    SpinLock::new(DisplayManager::new());

#[allow(clippy::large_enum_variant)]
#[expect(dead_code)]
enum DisplayMode<R: Renderer> {
    Headless,
    Console(Console<R>), // renderer attached to console
    App(FrameBuffer),    // renderer detached, FB used directly
}

pub struct DisplayManager<R: Renderer> {
    display_mode: DisplayMode<R>,
}

impl<R: Renderer> DisplayManager<R> {
    pub const fn new() -> Self {
        Self {
            display_mode: DisplayMode::Headless,
        }
    }

    // Initialise console
    pub fn init(&mut self, renderer: R) {
        self.display_mode = DisplayMode::Console(Console::new(renderer))
    }

    /// Release the framebuffer to other user
    #[allow(dead_code)]
    pub fn release_to_app(&mut self) -> Option<R> {
        match core::mem::replace(&mut self.display_mode, DisplayMode::Headless) {
            DisplayMode::Console(console) => Some(console.into_renderer()),
            other => {
                self.display_mode = other;
                None
            }
        }
    }

    /// Return the framebuffer to the console
    #[allow(dead_code)]
    pub fn return_to_console(&mut self, renderer: R) {
        self.display_mode = DisplayMode::Console(Console::new(renderer));
    }

    /// Write a character at the current cursor position
    #[cfg(test)]
    pub fn put_char(&mut self, ch: u8) {
        if let DisplayMode::Console(console) = &mut self.display_mode {
            console.put_char(ch);
        }
    }

    /// Hide the cursor
    #[cfg(test)]
    pub fn hide_cursor(&mut self) {
        if let DisplayMode::Console(console) = &mut self.display_mode {
            console.hide_cursor();
        }
    }

    /// Show the cursor
    #[cfg(test)]
    pub fn show_cursor(&mut self) {
        if let DisplayMode::Console(console) = &mut self.display_mode {
            console.show_cursor();
        }
    }
}

impl<R: Renderer> core::fmt::Write for DisplayManager<R> {
    fn write_str(&mut self, s: &str) -> Result<(), core::fmt::Error> {
        // Deconstruct display_mode to get a console
        if let DisplayMode::Console(console) = &mut self.display_mode
            && !s.is_empty()
        {
            console.hide_cursor();
            for b in s.bytes() {
                console.process_char(b);
            }
            console.show_cursor();
        }
        Ok(())
    }
}
