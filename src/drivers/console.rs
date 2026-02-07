//! Console driver
//!
//! Classic 80 columns x 30 rows dumb terminal

use crate::arch::timer::sleep_ms;
use crate::drivers::font::Font;
use crate::drivers::ramfb::{self, Colour};
use crate::hal::ascii;
use crate::kernel::sync::SpinLock;

pub static CONSOLE: SpinLock<Console> = SpinLock::new(Console::new());

const ROWS: usize = 30;
const COLUMNS: usize = 80;
const TAB_SIZE: usize = 8;

struct Cursor {
    x: usize,
    y: usize,
    visible: bool,
}

pub struct Console {
    buffer: [[u8; COLUMNS]; ROWS],
    cursor: Cursor,
    fg: Colour,
    bg: Colour,
}

impl Console {
    pub const fn new() -> Self {
        Self {
            buffer: [[b' '; COLUMNS]; ROWS],
            cursor: Cursor {
                x: 0,
                y: 0,
                visible: false,
            },
            fg: Colour::WHITE,
            bg: Colour::BLACK,
        }
    }

    /// Write a character using the font at the current cursor position but do not advance cursor
    ///
    /// # `ch` is a byte
    pub fn put_char(&mut self, ch: u8) {
        match ch {
            ascii::BELL => {
                let current_ch = self.buffer[self.cursor.y][self.cursor.x];
                // Flash an asterisc at the cursor
                for _ in 0..2 {
                    self.draw_char(b'*');
                    sleep_ms(25);
                    self.draw_char(b' ');
                }
                self.draw_char(current_ch);
                self.cursor.visible = false;
            }
            ascii::BS | ascii::DEL => {
                if self.cursor.x > 0 {
                    self.hide_cursor();
                    self.cursor.x -= 1; // Note - shell responsibility to print space
                }
            }
            ascii::TAB => {
                self.hide_cursor();
                let next_tab = ((self.cursor.x / TAB_SIZE) + 1) * TAB_SIZE;
                let next_tab = next_tab.min(COLUMNS - 1);
                while self.cursor.x < next_tab {
                    self.buffer[self.cursor.y][self.cursor.x] = b' ';
                    self.draw_char(b' ');
                    self.cursor.x += 1;
                }
            }
            ascii::CR => {
                self.hide_cursor();
                self.cursor.x = 0;
            }
            ascii::FF => {
                self.hide_cursor();
                self.clear();
            }
            ascii::LF => {
                self.hide_cursor();
                self.cursor.x = 0;
                if self.cursor.y < ROWS - 1 {
                    self.cursor.y += 1;
                } else {
                    self.scroll();
                }
            }
            _ => {
                // Writeable char
                self.buffer[self.cursor.y][self.cursor.x] = ch; // Draw char overwriting cursor
                self.draw_char(ch);
                self.cursor.visible = false;
                self.cursor.x += 1;
                if self.cursor.x >= COLUMNS {
                    self.cursor.x = 0;
                    self.cursor.y += 1;
                    if self.cursor.y >= ROWS {
                        self.scroll();
                    }
                }
            }
        }
    }

    /// Write a character using the font at the current cursor position and advance cursor
    ///
    /// # `ch` is a byte
    #[allow(dead_code)]
    pub fn write_char(&mut self, ch: u8) {
        self.put_char(ch);
        self.show_cursor();
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
        ramfb::scroll(Font::height(), self.bg);
        self.cursor.x = 0;
        self.cursor.y = ROWS - 1;
    }

    // Clear the screen
    //     clear()
    //     - Fill buffer with spaces
    //     - Clear framebuffer
    //     - Reset cursor to (0, 0)
    pub fn clear(&mut self) {
        self.buffer = [[b' '; COLUMNS]; ROWS];
        ramfb::clear(self.bg);
        self.cursor.x = 0;
        self.cursor.y = 0;
    }

    // Draw char at current position
    fn draw_char(&self, ch: u8) {
        Font::draw_char(
            self.cursor.x * Font::width(),
            self.cursor.y * Font::height(),
            ch,
            self.fg,
            self.bg,
        );
    }

    // Hide the cursor at current position
    pub fn hide_cursor(&mut self) {
        if self.cursor.visible {
            Font::draw_char(
                self.cursor.x * Font::width(),
                self.cursor.y * Font::height(),
                self.buffer[self.cursor.y][self.cursor.x],
                self.fg,
                self.bg,
            );
            self.cursor.visible = false;
        }
    }

    // Show the cursor at current position
    pub fn show_cursor(&mut self) {
        if !self.cursor.visible {
            Font::draw_char(
                self.cursor.x * Font::width(),
                self.cursor.y * Font::height(),
                self.buffer[self.cursor.y][self.cursor.x],
                self.bg,
                self.fg,
            );
            self.cursor.visible = true;
        };
    }
}

impl core::fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> Result<(), core::fmt::Error> {
        for b in s.bytes() {
            self.put_char(b);
        }
        self.show_cursor();
        Ok(())
    }
}
