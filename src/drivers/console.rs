//! Console driver
//!
//! Classic 80 columns x 30 rows dumb terminal

use crate::arch::timer::sleep_ms;
use crate::drivers::font::Font;
use crate::drivers::ramfb::{self, Colour};
use crate::kernel::sync::SpinLock;

#[repr(u8)]
pub enum ControlChar {
    Bell = 0x07,
    BackSpace = 0x08,       // Backspace
    Tab = 0x09,             // Advance to multiple of 8
    LineFeed = 0x0A,        // Cursor down, scroll if needed
    FormFeed = 0x0C,        // Clear screen
    CarriageReturn = 0x0D,  // Cursor to column 0
    Delete = 0x7F,          // Treat as backspace
}

impl ControlChar {
    fn from_byte(byte: u8) -> Option<ControlChar> {
        match byte {
            0x07 => Some(ControlChar::Bell),
            0x08 => Some(ControlChar::BackSpace),
            0x09 => Some(ControlChar::Tab),
            0x0A => Some(ControlChar::LineFeed),
            0x0C => Some(ControlChar::FormFeed),
            0x0D => Some(ControlChar::CarriageReturn),
            0x7F => Some(ControlChar::Delete),
            _ => None,
        }
    }
}

pub static CONSOLE: SpinLock<Console> = SpinLock::new(Console::new());

const ROWS: usize = 30;
const COLUMNS: usize = 80;
const TAB_SIZE: usize = 8;

pub struct Console {
    buffer: [[u8; COLUMNS]; ROWS],
    cursor_x: usize,
    cursor_y: usize,
    fg: Colour,
    bg: Colour,
}

impl Console {
    pub const fn new() -> Self {
        Self {
            buffer: [[b' '; COLUMNS]; ROWS],
            cursor_x: 0,
            cursor_y: 0,
            fg: Colour::WHITE,
            bg: Colour::BLACK,
        }
    }

    /// Write a character using the font at the current cursor position
    ///
    /// # `ch` is a byte
    pub fn write_char(&mut self, ch: u8) {
        match ControlChar::from_byte(ch) {
            Some(ControlChar::Bell) => {
                let current_ch = self.buffer[self.cursor_y][self.cursor_x];
                // Flash an asterisc at the cursor
                for _ in 0..2 {
                    self.draw_char(b'*');
                    sleep_ms(50);
                    self.draw_char(b' ');
                }
                self.draw_char(current_ch);
                self.show_cursor();
            },
            Some(ControlChar::BackSpace) | Some(ControlChar::Delete) => {
                if self.cursor_x > 0 {
                    self.hide_cursor();
                    self.cursor_x -= 1; // Note - shell responsibility to print space
                    self.show_cursor();
                }
            },
            Some(ControlChar::Tab) => {
                self.hide_cursor();
                let next_tab = ((self.cursor_x / TAB_SIZE) + 1) * TAB_SIZE;
                let next_tab = next_tab.min(COLUMNS - 1);
                while self.cursor_x < next_tab {
                    self.buffer[self.cursor_y][self.cursor_x] = b' ';
                    self.draw_char(b' ');
                    self.cursor_x += 1;
                }
                self.show_cursor();
            },
            Some(ControlChar::CarriageReturn) => {
                self.hide_cursor();
                self.cursor_x = 0;
                self.show_cursor();
            },
            Some(ControlChar::FormFeed) => {
                self.hide_cursor();
                self.clear();
                self.show_cursor();
            },
            Some(ControlChar::LineFeed) => {
                self.hide_cursor();
                self.cursor_x = 0;
                if self.cursor_y < ROWS - 1 {
                    self.cursor_y += 1;
                } else {
                    self.scroll();
                }
                self.show_cursor();
            },
            _ => { // Writeable char
                self.buffer[self.cursor_y][self.cursor_x] = ch; // Save the byte
                self.draw_char(ch);
                self.hide_cursor();
                self.cursor_x += 1;
                if self.cursor_x >= COLUMNS {
                    self.cursor_x = 0;
                    self.cursor_y += 1;
                    if self.cursor_y >= ROWS {
                        self.scroll();
                    }
                }
                self.show_cursor();
            }
        }
    }

    /// Scrolls console by one line
    //     scroll()
    //     - Shift all rows up by one (row 1 → row 0, row 2 → row 1, etc.)
    //     - Clear the bottom row
    //     - Redraw the screen from the buffer
    pub fn scroll(&mut self) {
        for row in 0..ROWS - 1 {
            self.buffer[row] = self.buffer[row + 1];
        }
        self.buffer[ROWS - 1] = [b' '; COLUMNS];
        for row in 0..ROWS {
            Font::draw_string(0, row * Font::height(), self.buffer[row].as_slice(), self.fg, self.bg);
        }
        self.cursor_x = 0;
        self.cursor_y = ROWS - 1;
    }

    // Clear the screen
    //     clear()
    //     - Fill buffer with spaces
    //     - Clear framebuffer
    //     - Reset cursor to (0, 0)
    pub fn clear(&mut self) {
        self.buffer = [[b' '; COLUMNS]; ROWS];
        ramfb::clear(self.bg);
        self.cursor_x = 0;
        self.cursor_y = 0;
    }

    // Draw char at current position
    fn draw_char(&self, ch: u8) {
        Font::draw_char(
            self.cursor_x * Font::width(),
            self.cursor_y * Font::height(),
            ch,
            self.fg,
            self.bg,
        );
    }

    // Hide the cursor at current position
    pub fn hide_cursor(&self) {
        Font::draw_char(
            self.cursor_x * Font::width(),
            self.cursor_y * Font::height(),
            self.buffer[self.cursor_y][self.cursor_x],
            self.fg,
            self.bg,
        );
    }

    // Show the cursor at current position
    pub fn show_cursor(&self) {
        Font::draw_char(
            self.cursor_x * Font::width(),
            self.cursor_y * Font::height(),
            self.buffer[self.cursor_y][self.cursor_x],
            self.bg,
            self.fg,
        );
    }

    // Get the current cursor position
    pub fn cursor_position(&self) -> (usize, usize) {
        (self.cursor_x, self.cursor_y)
    }
}

impl core::fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> Result<(), core::fmt::Error> {
        for b in s.bytes() {
            self.write_char(b);
        }
        Ok(())
    }
}
