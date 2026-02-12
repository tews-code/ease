//! Render trait

use crate::drivers::font::Font;
use crate::drivers::ramfb::{Colour, FrameBuffer};

pub trait Renderer {
    fn draw_char(&mut self, row: usize, column: usize, ch: u8, inverted: bool);

    fn fill(&mut self);

    fn scroll(&mut self);
}

pub struct FrameBufferRenderer {
    pub fb: FrameBuffer,
    fg: Colour,
    bg: Colour,
}

impl FrameBufferRenderer {
    pub fn new(fb: FrameBuffer, fg: Colour, bg: Colour) -> Self {
        Self { fb, fg, bg }
    }
}

impl Renderer for FrameBufferRenderer {
    // Draw char at current position
    #[inline]
    fn draw_char(&mut self, row: usize, column: usize, ch: u8, inverted: bool) {
        Font::draw_char(
            // Takes x,y coordinates
            &mut self.fb,
            column * Font::width(),
            row * Font::height(),
            ch,
            if inverted { self.bg } else { self.fg },
            if inverted { self.fg } else { self.bg },
        );
    }

    // Clear the screen
    #[inline]
    fn fill(&mut self) {
        self.fb.fill(self.bg);
    }

    /// Scrolls by one line of font height
    #[inline]
    fn scroll(&mut self) {
        self.fb.scroll(Font::height(), self.bg);
    }
}
