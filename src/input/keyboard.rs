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
        match self.reader.read_byte() {
            None => None,
            Some(byte) => {
                match self.parser.parse(byte) {
                    ParseResult::InvalidSequence => None,
                    ParseResult::Special(special_key) => Some(KeyEvent::Special(special_key)),
                    ParseResult::Byte(byte) => Some(KeyEvent::Byte(byte)),
                    ParseResult::Pending => self.poll(), // Keep reading until we get a result
                }
            }
        }
    }
}
