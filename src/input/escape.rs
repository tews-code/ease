//! Escape sequence parser

pub enum Key {
    Backspace,
    Enter,
    Esc,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
}

pub enum ParseResult {
    Byte(u8),
    Special(Key),
    Pending,
    InvalidSequence,
}

enum State {
    Normal,
    GotEscape,
    GotBracket,
}

pub struct EscapeParser {
    state: State,
}

impl EscapeParser {
    pub const fn new() -> Self {
        Self {
            state: State::Normal,
        }
    }

    pub fn parse(&mut self, byte: u8) -> ParseResult {
        match self.state {
            State::GotBracket => {
                self.state = State::Normal; // Get back to normal after this
                match byte {
                    b'A' => ParseResult::Special(Key::ArrowUp),
                    b'B' => ParseResult::Special(Key::ArrowDown),
                    b'C' => ParseResult::Special(Key::ArrowRight),
                    b'D' => ParseResult::Special(Key::ArrowLeft),
                    _ => ParseResult::InvalidSequence,
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
                        ParseResult::Special(Key::Esc)
                    }
                }
            }
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_regular_ascii_bytes() {
        let mut p = EscapeParser::new();
        assert!(matches!(p.parse(b'a'), ParseResult::Byte(b'a')));
        assert!(matches!(p.parse(b'z'), ParseResult::Byte(b'z')));
        assert!(matches!(p.parse(b' '), ParseResult::Byte(b' ')));
        assert!(matches!(p.parse(b'0'), ParseResult::Byte(b'0')));
        assert!(matches!(p.parse(b'~'), ParseResult::Byte(b'~')));
    }

    #[test_case]
    fn test_enter() {
        let mut p = EscapeParser::new();
        assert!(matches!(p.parse(0x0D), ParseResult::Special(Key::Enter)));
    }

    #[test_case]
    fn test_backspace_del() {
        let mut p = EscapeParser::new();
        assert!(matches!(
            p.parse(0x7F),
            ParseResult::Special(Key::Backspace)
        ));
    }

    #[test_case]
    fn test_backspace_bs() {
        let mut p = EscapeParser::new();
        assert!(matches!(
            p.parse(0x08),
            ParseResult::Special(Key::Backspace)
        ));
    }

    #[test_case]
    fn test_arrow_up() {
        let mut p = EscapeParser::new();
        assert!(matches!(p.parse(0x1B), ParseResult::Pending));
        assert!(matches!(p.parse(b'['), ParseResult::Pending));
        assert!(matches!(p.parse(b'A'), ParseResult::Special(Key::ArrowUp)));
    }

    #[test_case]
    fn test_arrow_down() {
        let mut p = EscapeParser::new();
        assert!(matches!(p.parse(0x1B), ParseResult::Pending));
        assert!(matches!(p.parse(b'['), ParseResult::Pending));
        assert!(matches!(
            p.parse(b'B'),
            ParseResult::Special(Key::ArrowDown)
        ));
    }

    #[test_case]
    fn test_arrow_right() {
        let mut p = EscapeParser::new();
        assert!(matches!(p.parse(0x1B), ParseResult::Pending));
        assert!(matches!(p.parse(b'['), ParseResult::Pending));
        assert!(matches!(
            p.parse(b'C'),
            ParseResult::Special(Key::ArrowRight)
        ));
    }

    #[test_case]
    fn test_arrow_left() {
        let mut p = EscapeParser::new();
        assert!(matches!(p.parse(0x1B), ParseResult::Pending));
        assert!(matches!(p.parse(b'['), ParseResult::Pending));
        assert!(matches!(
            p.parse(b'D'),
            ParseResult::Special(Key::ArrowLeft)
        ));
    }

    #[test_case]
    fn test_bare_esc_non_bracket() {
        let mut p = EscapeParser::new();
        assert!(matches!(p.parse(0x1B), ParseResult::Pending));
        // Non-bracket after ESC → treated as bare Esc key
        assert!(matches!(p.parse(b'x'), ParseResult::Special(Key::Esc)));
    }

    #[test_case]
    fn test_invalid_csi_sequence() {
        let mut p = EscapeParser::new();
        assert!(matches!(p.parse(0x1B), ParseResult::Pending));
        assert!(matches!(p.parse(b'['), ParseResult::Pending));
        // Unknown CSI final byte
        assert!(matches!(p.parse(b'Z'), ParseResult::InvalidSequence));
    }

    #[test_case]
    fn test_state_resets_after_complete_sequence() {
        let mut p = EscapeParser::new();
        // Complete an arrow sequence
        p.parse(0x1B);
        p.parse(b'[');
        p.parse(b'A');
        // Parser should be back in Normal state — regular byte works
        assert!(matches!(p.parse(b'x'), ParseResult::Byte(b'x')));
    }

    #[test_case]
    fn test_state_resets_after_invalid_sequence() {
        let mut p = EscapeParser::new();
        // Invalid CSI sequence
        p.parse(0x1B);
        p.parse(b'[');
        p.parse(b'Z'); // InvalidSequence
        // Parser should be back in Normal — regular byte works
        assert!(matches!(p.parse(b'a'), ParseResult::Byte(b'a')));
    }

    #[test_case]
    fn test_state_resets_after_bare_esc() {
        let mut p = EscapeParser::new();
        p.parse(0x1B);
        p.parse(b'x'); // bare Esc
        // Back to Normal
        assert!(matches!(p.parse(b'a'), ParseResult::Byte(b'a')));
    }

    #[test_case]
    fn test_consecutive_arrows() {
        let mut p = EscapeParser::new();
        // Up then Down in sequence
        p.parse(0x1B);
        p.parse(b'[');
        assert!(matches!(p.parse(b'A'), ParseResult::Special(Key::ArrowUp)));

        p.parse(0x1B);
        p.parse(b'[');
        assert!(matches!(
            p.parse(b'B'),
            ParseResult::Special(Key::ArrowDown)
        ));
    }

    #[test_case]
    fn test_mixed_input() {
        let mut p = EscapeParser::new();
        assert!(matches!(p.parse(b'h'), ParseResult::Byte(b'h')));
        assert!(matches!(p.parse(b'i'), ParseResult::Byte(b'i')));
        assert!(matches!(p.parse(0x0D), ParseResult::Special(Key::Enter)));
        p.parse(0x1B);
        p.parse(b'[');
        assert!(matches!(p.parse(b'A'), ParseResult::Special(Key::ArrowUp)));
        assert!(matches!(p.parse(b'!'), ParseResult::Byte(b'!')));
    }
}
