//! Keyboard event reader

use crate::hal::Reader;
use crate::input::escape::{EscapeParser, Key, ParseResult};

pub enum KeyEvent {
    Byte(u8),
    Special(Key),
}

pub trait Keyboard {
    fn poll(&mut self) -> Option<KeyEvent>;
}

pub struct KeyboardInput<R: Reader> {
    reader: R,
    parser: EscapeParser,
}

impl<R: Reader> KeyboardInput<R> {
    pub const fn new(reader: R) -> Self {
        Self {
            reader,
            parser: EscapeParser::new(),
        }
    }
}

impl<R: Reader> Keyboard for KeyboardInput<R> {
    fn poll(&mut self) -> Option<KeyEvent> {
        let byte = self.reader.read_byte()?; // If no byte return None immediately
        match self.parser.parse(byte) {
            ParseResult::Byte(b) => Some(KeyEvent::Byte(b)),
            ParseResult::Special(k) => Some(KeyEvent::Special(k)),
            ParseResult::InvalidSequence => None,
            ParseResult::Pending => {
                // Wait for 10ms for rest of escape key sequence
                let deadline = crate::kernel::timer::ticks_ms() + 10;
                loop {
                    if let Some(next) = self.reader.read_byte() {
                        match self.parser.parse(next) {
                            ParseResult::Byte(b) => return Some(KeyEvent::Byte(b)), // Fallen out of sequence with ordinary byte
                            ParseResult::Special(k) => return Some(KeyEvent::Special(k)), // Fallen out of sequence with special char
                            ParseResult::InvalidSequence => return None,
                            ParseResult::Pending => continue, // still in sequence get next
                        }
                    }
                    if crate::kernel::timer::ticks_ms() >= deadline {
                        break;
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
