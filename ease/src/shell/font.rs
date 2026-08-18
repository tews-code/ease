//! Fonts

use crate::drivers::ramfb::{Colour, FrameBuffer};

pub const WIDTH: usize = 8;
pub const HEIGHT: usize = 16;

const FONT_DATA: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/resources/VGA8.F16"));
const BYTES_PER_GLYPH: usize = 16;

fn glyph(char_code: u8) -> &'static [u8] {
    let offset = char_code as usize * BYTES_PER_GLYPH;
    &FONT_DATA[offset..offset + BYTES_PER_GLYPH]
}

/// Render a glyph in VGA 8x16 font. (x, y) are coordinates of top left of the glyph
///
/// - ch is a byte
pub fn render_glyph(fb: &mut FrameBuffer, x: usize, y: usize, ch: u8, fg: Colour, bg: Colour) {
    if x + WIDTH <= fb.width() && y + HEIGHT <= fb.height() {
        let glyph_data = glyph(ch);
        for (row, byte) in glyph_data.iter().enumerate() {
            let mut pixels = [0u32; WIDTH];
            for (column, pixel) in pixels.iter_mut().enumerate() {
                let bit_set = (byte >> (7 - column)) & 1 != 0;
                *pixel = if bit_set { fg.as_raw() } else { bg.as_raw() }
            }
            fb.set_pixels(x, y + row, &pixels);
        }
    }
}
