//! Keyboard event reader

use crate::hal::Reader;
use crate::hal::qemu_virt::UartReader;
use crate::input::escape::{EscapeParser, Key, ParseResult};

pub enum KeyEvent {
    Byte(u8),
    Special(Key),
}

pub trait Keyboard {
    fn poll(&mut self) -> Option<KeyEvent>;
}

pub struct UartKeyboard {
    reader: UartReader,
    parser: EscapeParser,
}

impl UartKeyboard {
    pub const fn new() -> Self {
        Self {
            reader: UartReader,
            parser: EscapeParser::new(),
        }
    }
}

impl Keyboard for UartKeyboard {
    fn poll(&mut self) -> Option<KeyEvent> {
        let byte = self.reader.read_byte()?; // If no byte return None immediately
        match self.parser.parse(byte) {
            ParseResult::Byte(b) => Some(KeyEvent::Byte(b)),
            ParseResult::Special(k) => Some(KeyEvent::Special(k)),
            ParseResult::InvalidSequence => None,
            ParseResult::Pending => {
                // Try at most 100 times to get next char in escape sequence - give UART time
                for _ in 0..100 {
                    if let Some(next) = self.reader.read_byte() {
                        match self.parser.parse(next) {
                            ParseResult::Byte(b) => return Some(KeyEvent::Byte(b)), // Fallen out of sequence with ordinary byte
                            ParseResult::Special(k) => return Some(KeyEvent::Special(k)), // Fallen out of sequence with special char
                            ParseResult::InvalidSequence => return None,
                            ParseResult::Pending => continue, // still in sequence get next
                        }
                    }
                    core::hint::spin_loop();
                }
                // Timed out - standalone Esc
                self.parser.reset();
                Some(KeyEvent::Special(Key::Esc))
            }
        }
    }
}
