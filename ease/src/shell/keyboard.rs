//! Keyboard event reader

use crate::kernel::timer;
use crate::shell::vt_parse::{EscapeParser, Key, ParseResult};

const ESC_TIMEOUT_MS: u64 = 2;

/// A keyboard event: either a regular byte or a special key (arrow, enter, etc.).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    Byte(u8),
    Special(Key),
}

/// Keyboard input handler. Wraps an `EscapeParser` to convert raw UART bytes into `KeyEvent`s.
#[derive(Default)]
pub struct Keyboard {
    parser: EscapeParser,
}

impl Keyboard {
    /// Polls for a keyboard event. Calls `read_byte` to get raw UART input.
    /// Returns `None` if no input is available. May block briefly (up to 2ms)
    /// when resolving escape sequences.
    pub fn poll(&mut self, mut read_byte: impl FnMut() -> Option<usize>) -> Option<KeyEvent> {
        if let Some(b) = self.parser.pending_byte() {
            return Some(KeyEvent::Byte(b));
        }
        let byte = read_byte()?; // If no byte return None immediately

        match self.parser.parse(byte as u8) {
            ParseResult::Byte(b) => Some(KeyEvent::Byte(b)),
            ParseResult::Special(k) => Some(KeyEvent::Special(k)),
            ParseResult::InvalidSequence => None,
            ParseResult::Pending => {
                // Wait for ESC_TIMEOUT_MS for rest of escape key sequence
                let start = timer::elapsed_ms();
                loop {
                    if let Some(next) = read_byte() {
                        match self.parser.parse(next as u8) {
                            ParseResult::Byte(b) => return Some(KeyEvent::Byte(b)), // Fallen out of sequence with ordinary byte
                            ParseResult::Special(k) => return Some(KeyEvent::Special(k)), // Fallen out of sequence with special char
                            ParseResult::InvalidSequence => return None,
                            ParseResult::Pending => continue, // still in sequence get next
                        }
                    }
                    if timer::elapsed_ms().wrapping_sub(start) >= ESC_TIMEOUT_MS {
                        // Allow 2 ms
                        break;
                    }
                    // Yield while waiting
                    crate::kernel::sched::yield_now();
                }
                // Timed out - let parser decide what to emit
                self.parser.timeout().map(KeyEvent::Special)
            }
        }
    }
}

#[cfg(all(test, feature = "test-shell"))]
mod tests {
    use super::*;

    /// Helper: poll with a fixed sequence of bytes, then None thereafter
    fn poll_bytes(kb: &mut Keyboard, bytes: &[usize]) -> Option<KeyEvent> {
        let mut iter = bytes.iter();
        kb.poll(|| iter.next().copied())
    }

    #[test_case]
    fn test_no_byte_available() {
        let mut kb = Keyboard::default();
        assert_eq!(kb.poll(|| None), None);
    }

    #[test_case]
    fn test_regular_byte() {
        let mut kb = Keyboard::default();
        assert_eq!(
            poll_bytes(&mut kb, &[b'a'.into()]),
            Some(KeyEvent::Byte(b'a'))
        );
    }

    #[test_case]
    fn test_enter() {
        let mut kb = Keyboard::default();
        assert_eq!(
            poll_bytes(&mut kb, &[0x0D]),
            Some(KeyEvent::Special(Key::Enter))
        );
    }

    #[test_case]
    fn test_backspace() {
        let mut kb = Keyboard::default();
        assert_eq!(
            poll_bytes(&mut kb, &[0x7F]),
            Some(KeyEvent::Special(Key::Backspace))
        );
    }

    #[test_case]
    fn test_arrow_up() {
        let mut kb = Keyboard::default();
        assert_eq!(
            poll_bytes(&mut kb, &[0x1B, b'['.into(), b'A'.into()]),
            Some(KeyEvent::Special(Key::ArrowUp))
        );
    }

    #[test_case]
    fn test_arrow_down() {
        let mut kb = Keyboard::default();
        assert_eq!(
            poll_bytes(&mut kb, &[0x1B, b'['.into(), b'B'.into()]),
            Some(KeyEvent::Special(Key::ArrowDown))
        );
    }

    #[test_case]
    fn test_arrow_right() {
        let mut kb = Keyboard::default();
        assert_eq!(
            poll_bytes(&mut kb, &[0x1B, b'['.into(), b'C'.into()]),
            Some(KeyEvent::Special(Key::ArrowRight))
        );
    }

    #[test_case]
    fn test_arrow_left() {
        let mut kb = Keyboard::default();
        assert_eq!(
            poll_bytes(&mut kb, &[0x1B, b'['.into(), b'D'.into()]),
            Some(KeyEvent::Special(Key::ArrowLeft))
        );
    }

    #[test_case]
    fn test_bare_esc_timeout() {
        // ESC with no follow-up bytes — spins until timeout, returns Esc
        let mut kb = Keyboard::default();
        assert_eq!(
            poll_bytes(&mut kb, &[0x1B]),
            Some(KeyEvent::Special(Key::Esc))
        );
    }

    #[test_case]
    fn test_bare_esc_preserves_following_byte() {
        let mut kb = Keyboard::default();
        // ESC followed immediately by 'x' — parser returns Esc, pushes back 'x'
        assert_eq!(
            poll_bytes(&mut kb, &[0x1B, b'x'.into()]),
            Some(KeyEvent::Special(Key::Esc))
        );
        // Next poll should return the pushed-back byte
        assert_eq!(kb.poll(|| None), Some(KeyEvent::Byte(b'x')));
    }

    #[test_case]
    fn test_invalid_csi_returns_none() {
        let mut kb = Keyboard::default();
        // ESC [ Z — unknown final byte
        assert_eq!(poll_bytes(&mut kb, &[0x1B, b'['.into(), b'Z'.into()]), None);
    }

    #[test_case]
    fn test_invalid_csi_preserves_byte() {
        let mut kb = Keyboard::default();
        let _ = poll_bytes(&mut kb, &[0x1B, b'['.into(), b'Z'.into()]); // InvalidSequence
        // Pushed-back byte returned on next poll
        assert_eq!(kb.poll(|| None), Some(KeyEvent::Byte(b'Z'.into())));
    }

    #[test_case]
    fn test_csi_parameters_consumed() {
        let mut kb = Keyboard::default();
        // ESC [ 3 ~ (Delete key) — parameters consumed, '~' is unrecognized final byte
        assert_eq!(
            poll_bytes(&mut kb, &[0x1B, b'['.into(), b'3'.into(), b'~'.into()]),
            None
        );
        // '~' is in pushback
        assert_eq!(kb.poll(|| None), Some(KeyEvent::Byte(b'~')));
    }

    #[test_case]
    fn test_consecutive_keys() {
        let mut kb = Keyboard::default();
        assert_eq!(
            poll_bytes(&mut kb, &[b'h'.into()]),
            Some(KeyEvent::Byte(b'h'))
        );
        assert_eq!(
            poll_bytes(&mut kb, &[b'i'.into()]),
            Some(KeyEvent::Byte(b'i'))
        );
        assert_eq!(
            poll_bytes(&mut kb, &[0x0D]),
            Some(KeyEvent::Special(Key::Enter))
        );
    }

    #[test_case]
    fn test_arrow_then_regular_byte() {
        let mut kb = Keyboard::default();
        assert_eq!(
            poll_bytes(&mut kb, &[0x1B, b'['.into(), b'A'.into()]),
            Some(KeyEvent::Special(Key::ArrowUp))
        );
        // No pushback — next poll works normally
        assert_eq!(kb.poll(|| None), None);
        assert_eq!(
            poll_bytes(&mut kb, &[b'z'.into()]),
            Some(KeyEvent::Byte(b'z'))
        );
    }

    #[test_case]
    fn test_pushback_drained_before_new_byte() {
        let mut kb = Keyboard::default();
        // Set up pushback via bare ESC + non-bracket
        let _ = poll_bytes(&mut kb, &[0x1B, b'x'.into()]);
        // Provide a new byte, but pushback should be returned first
        assert_eq!(
            poll_bytes(&mut kb, &[b'y'.into()]),
            Some(KeyEvent::Byte(b'x'))
        );
        // Now the new byte
        assert_eq!(
            poll_bytes(&mut kb, &[b'y'.into()]),
            Some(KeyEvent::Byte(b'y'))
        );
    }
}
