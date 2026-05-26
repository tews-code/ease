//! Escape sequence parser

/// Special keys recognised by the escape sequence parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Key {
    Backspace,
    Enter,
    Esc,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
}

/// Result of feeding a byte to the escape parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum ParseResult {
    Byte(u8),
    Special(Key),
    Pending,
    InvalidSequence,
}

#[derive(Clone, Copy, Debug, Default)]
enum State {
    #[default]
    Normal,
    GotEscape,
    GotBracket,
}

/// VT-100/ANSI escape sequence parser. Converts raw byte sequences into `Key` values.
#[derive(Default)]
pub struct EscapeParser {
    state: State,
    pushback: Option<u8>, // Char that would otherwise be lost in a sequence
}

impl EscapeParser {
    /// Feeds a byte into the parser. Returns the result immediately or `Pending` if
    /// more bytes are needed to complete an escape sequence.
    pub fn parse(&mut self, byte: u8) -> ParseResult {
        match self.state {
            State::Normal => {
                match byte {
                    0x1B => {
                        // Esc key - could be start of sequence
                        self.state = State::GotEscape;
                        ParseResult::Pending
                    }
                    0x0D => {
                        // Enter
                        ParseResult::Special(Key::Enter)
                    }
                    0x7F | 0x08 => {
                        // Backspace
                        ParseResult::Special(Key::Backspace)
                    }
                    _ => ParseResult::Byte(byte),
                }
            }
            State::GotEscape => {
                match byte {
                    b'[' => {
                        // Escape sequence
                        self.state = State::GotBracket;
                        ParseResult::Pending
                    }
                    _ => {
                        self.state = State::Normal;
                        self.pushback = Some(byte);
                        ParseResult::Special(Key::Esc)
                    }
                }
            }
            State::GotBracket => {
                match byte {
                    b'0'..=b'9' | b';' => ParseResult::Pending, // consume unsupported parameters
                    b'A' => {
                        self.state = State::Normal;
                        ParseResult::Special(Key::ArrowUp)
                    }
                    b'B' => {
                        self.state = State::Normal;
                        ParseResult::Special(Key::ArrowDown)
                    }
                    b'C' => {
                        self.state = State::Normal;
                        ParseResult::Special(Key::ArrowRight)
                    }
                    b'D' => {
                        self.state = State::Normal;
                        ParseResult::Special(Key::ArrowLeft)
                    }
                    _ => {
                        self.state = State::Normal;
                        self.pushback = Some(byte);
                        ParseResult::InvalidSequence
                    }
                }
            }
        }
    }

    /// Called when no more bytes arrive within the timeout. Returns `Some(Esc)` if
    /// a bare ESC was pending, or `None` if an incomplete CSI sequence is discarded.
    pub fn timeout(&mut self) -> Option<Key> {
        self.pushback = None;
        match self.state {
            State::Normal => None,
            State::GotEscape => {
                self.state = State::Normal;
                Some(Key::Esc)
            }
            State::GotBracket => {
                self.state = State::Normal;
                None
            }
        }
    }

    /// Takes the pushed-back byte, if any. A byte is pushed back when an escape sequence
    /// terminates with a byte that wasn't part of the sequence.
    pub fn pending_byte(&mut self) -> Option<u8> {
        self.pushback.take()
    }
}

#[cfg(all(test, feature = "test-shell"))]
mod tests {
    use super::*;

    #[test_case]
    fn test_regular_ascii_bytes() {
        let mut p = EscapeParser::default();
        assert_eq!(p.parse(b'a'), ParseResult::Byte(b'a'));
        assert_eq!(p.parse(b'z'), ParseResult::Byte(b'z'));
        assert_eq!(p.parse(b' '), ParseResult::Byte(b' '));
        assert_eq!(p.parse(b'0'), ParseResult::Byte(b'0'));
        assert_eq!(p.parse(b'~'), ParseResult::Byte(b'~'));
    }

    #[test_case]
    fn test_enter() {
        let mut p = EscapeParser::default();
        assert_eq!(p.parse(0x0D), ParseResult::Special(Key::Enter));
    }

    #[test_case]
    fn test_backspace_del() {
        let mut p = EscapeParser::default();
        assert_eq!(p.parse(0x7F), ParseResult::Special(Key::Backspace));
    }

    #[test_case]
    fn test_backspace_bs() {
        let mut p = EscapeParser::default();
        assert_eq!(p.parse(0x08), ParseResult::Special(Key::Backspace));
    }

    #[test_case]
    fn test_arrow_up() {
        let mut p = EscapeParser::default();
        assert_eq!(p.parse(0x1B), ParseResult::Pending);
        assert_eq!(p.parse(b'['), ParseResult::Pending);
        assert_eq!(p.parse(b'A'), ParseResult::Special(Key::ArrowUp));
    }

    #[test_case]
    fn test_arrow_down() {
        let mut p = EscapeParser::default();
        assert_eq!(p.parse(0x1B), ParseResult::Pending);
        assert_eq!(p.parse(b'['), ParseResult::Pending);
        assert_eq!(p.parse(b'B'), ParseResult::Special(Key::ArrowDown));
    }

    #[test_case]
    fn test_arrow_right() {
        let mut p = EscapeParser::default();
        assert_eq!(p.parse(0x1B), ParseResult::Pending);
        assert_eq!(p.parse(b'['), ParseResult::Pending);
        assert_eq!(p.parse(b'C'), ParseResult::Special(Key::ArrowRight));
    }

    #[test_case]
    fn test_arrow_left() {
        let mut p = EscapeParser::default();
        assert_eq!(p.parse(0x1B), ParseResult::Pending);
        assert_eq!(p.parse(b'['), ParseResult::Pending);
        assert_eq!(p.parse(b'D'), ParseResult::Special(Key::ArrowLeft));
    }

    #[test_case]
    fn test_bare_esc_preserves_byte() {
        let mut p = EscapeParser::default();
        assert_eq!(p.parse(0x1B), ParseResult::Pending);
        // Non-bracket after ESC → bare Esc, byte preserved in pushback
        assert_eq!(p.parse(b'x'), ParseResult::Special(Key::Esc));
        assert_eq!(p.pending_byte(), Some(b'x'));
        assert_eq!(p.pending_byte(), None); // drained
    }

    #[test_case]
    fn test_invalid_csi_preserves_byte() {
        let mut p = EscapeParser::default();
        assert_eq!(p.parse(0x1B), ParseResult::Pending);
        assert_eq!(p.parse(b'['), ParseResult::Pending);
        // Unknown CSI final byte → preserved in pushback
        assert_eq!(p.parse(b'Z'), ParseResult::InvalidSequence);
        assert_eq!(p.pending_byte(), Some(b'Z'));
        assert_eq!(p.pending_byte(), None);
    }

    #[test_case]
    fn test_csi_parameters_consumed() {
        let mut p = EscapeParser::default();
        let _ = p.parse(0x1B);
        let _ = p.parse(b'[');
        // Parameter digits and semicolons are consumed, not injected as input
        assert_eq!(p.parse(b'3'), ParseResult::Pending);
        assert_eq!(p.parse(b';'), ParseResult::Pending);
        assert_eq!(p.parse(b'5'), ParseResult::Pending);
        // Final byte resolves the sequence
        assert_eq!(p.parse(b'~'), ParseResult::InvalidSequence);
        assert_eq!(p.pending_byte(), Some(b'~'));
    }

    #[test_case]
    fn test_state_resets_after_complete_sequence() {
        let mut p = EscapeParser::default();
        let _ = p.parse(0x1B);
        let _ = p.parse(b'[');
        let _ = p.parse(b'A');
        // Parser should be back in Normal state
        assert_eq!(p.parse(b'x'), ParseResult::Byte(b'x'));
    }

    #[test_case]
    fn test_state_resets_after_invalid_sequence() {
        let mut p = EscapeParser::default();
        let _ = p.parse(0x1B);
        let _ = p.parse(b'[');
        let _ = p.parse(b'Z');
        // Drain pushback, then Normal state resumes
        assert_eq!(p.pending_byte(), Some(b'Z'));
        assert_eq!(p.parse(b'a'), ParseResult::Byte(b'a'));
    }

    #[test_case]
    fn test_state_resets_after_bare_esc() {
        let mut p = EscapeParser::default();
        let _ = p.parse(0x1B);
        let _ = p.parse(b'x');
        // Drain pushback, then Normal state resumes
        assert_eq!(p.pending_byte(), Some(b'x'));
        assert_eq!(p.parse(b'a'), ParseResult::Byte(b'a'));
    }

    #[test_case]
    fn test_consecutive_arrows() {
        let mut p = EscapeParser::default();
        let _ = p.parse(0x1B);
        let _ = p.parse(b'[');
        assert_eq!(p.parse(b'A'), ParseResult::Special(Key::ArrowUp));

        let _ = p.parse(0x1B);
        let _ = p.parse(b'[');
        assert_eq!(p.parse(b'B'), ParseResult::Special(Key::ArrowDown));
    }

    #[test_case]
    fn test_timeout_in_got_escape() {
        let mut p = EscapeParser::default();
        let _ = p.parse(0x1B);
        assert_eq!(p.timeout(), Some(Key::Esc));
        // Back to Normal
        assert_eq!(p.parse(b'a'), ParseResult::Byte(b'a'));
    }

    #[test_case]
    fn test_timeout_in_got_bracket() {
        let mut p = EscapeParser::default();
        let _ = p.parse(0x1B);
        let _ = p.parse(b'[');
        // Incomplete sequence discarded
        assert_eq!(p.timeout(), None);
        assert_eq!(p.parse(b'a'), ParseResult::Byte(b'a'));
    }

    #[test_case]
    fn test_timeout_in_normal() {
        let mut p = EscapeParser::default();
        assert_eq!(p.timeout(), None);
    }

    #[test_case]
    fn test_mixed_input() {
        let mut p = EscapeParser::default();
        assert_eq!(p.parse(b'h'), ParseResult::Byte(b'h'));
        assert_eq!(p.parse(b'i'), ParseResult::Byte(b'i'));
        assert_eq!(p.parse(0x0D), ParseResult::Special(Key::Enter));
        let _ = p.parse(0x1B);
        let _ = p.parse(b'[');
        assert_eq!(p.parse(b'A'), ParseResult::Special(Key::ArrowUp));
        assert_eq!(p.parse(b'!'), ParseResult::Byte(b'!'));
    }
}
