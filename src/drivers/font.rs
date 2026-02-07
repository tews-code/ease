//! Fonts

use core::ops::Index;

use crate::drivers::ramfb::{self, Colour};

pub struct Font {
    data: &'static [u8],
}

// Font data is 16 bytes per glyph
pub const FONT: Font = Font {
    data: include_bytes!("../../resources/VGA8.F16"),
};

impl Index<u8> for Font {
    type Output = [u8];

    fn index(&self, char_code: u8) -> &Self::Output {
        const BYTES_PER_GLYPH: usize = 16;
        let offset = char_code as usize * BYTES_PER_GLYPH;
        &self.data[offset..offset + BYTES_PER_GLYPH]
    }
}

impl Font {
    const WIDTH: usize = 8;
    const HEIGHT: usize = 16;

    /// Draw a glyph in VGA 8x16 font. (x, y) are coordinates of top left of the glyph
    ///
    /// # ch is a byte
    pub fn draw_char(x: usize, y: usize, ch: u8, fg: Colour, bg: Colour) {
        if (x <= ramfb::width() - Self::WIDTH) && (y <= ramfb::height() - Self::HEIGHT) {
            let glyph_data = &FONT[ch];
            for (row, byte) in glyph_data.iter().enumerate().take(Self::HEIGHT) {
                let row_offset = (y + row) * ramfb::width() as usize;
                for column in 0..Self::WIDTH {
                    let bit_set = (byte >> (7 - column)) & 1 != 0;
                    let fb_offset = row_offset + x + column;
                    unsafe {
                        // Safety: Checked x and y within bounds above
                        ramfb::set_pixel(fb_offset, if bit_set { fg } else { bg });
                    }
                }
            }
        }
    }

    /// Draw a string in FONT
    ///
    /// # Each element of `s` is treated as a byte
    pub fn draw_string(x: usize, y: usize, s: &[u8], fg: Colour, bg: Colour) {
        for (i, b) in s.iter().enumerate() {
            Self::draw_char(x + i * Self::WIDTH, y, *b, fg, bg);
        }
    }

    /// Width of the font
    pub fn width() -> usize {
        Self::WIDTH
    }

    /// Height of the font
    pub fn height() -> usize {
        Self::HEIGHT
    }
}
