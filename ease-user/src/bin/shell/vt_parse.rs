//! Escape sequence parser
//!
//! Parses raw bytes into [KeyEvent]s. Control sequences are parsed
//! to identify more complex keystrokes.
//! See [KeyEventParser] for the API.

/// VT escape sequences, used to instruct the console
pub mod vt_out {
    /// Move cursor one step to the right
    pub const CURSOR_RIGHT: &[u8] = b"\x1b[C";
    /// Erase to the end of the line
    pub const ERASE_TO_END_OF_LINE: &[u8] = b"\x1b[K";
    /// Move the cursor to the top left home - location (1, 1)
    pub const CURSOR_HOME: &[u8] = b"\x1b[H";
    /// Clear the whole screen
    pub const CLEAR_SCREEN: &[u8] = b"\x1b[2J";
}

/// Special keys as chars
mod special {
    use ease_ulib::ascii;
    pub const BS: char = ascii::BS as char;
    pub const CR: char = ascii::CR as char;
    pub const DEL: char = ascii::DEL as char;
    pub const ESC: char = ascii::ESC as char;
    pub const LF: char = ascii::LF as char;
    pub const TAB: char = ascii::TAB as char;
}

/// Special keys recognised by the parsers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Key {
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Backspace,
    Delete,
    End,
    Enter,
    Esc,
    Home,
    Insert,
    PageDown,
    PageUp,
    Tab,
}

/// Special key modifier
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifier {
    alt: bool,
    ctrl: bool,
    shift: bool,
}

/// Create a Modifier from raw bits
///
/// The xterm formula is val - 1 and test bit 0 (shift), bit 1 (alt), and bit 2 (ctrl)
impl Modifier {
    fn from_raw(value: u8) -> Self {
        let r = value.saturating_sub(1);
        let shift = (r & 1) != 0;
        let alt = (r & 2) != 0;
        let ctrl = (r & 4) != 0;
        Self { alt, ctrl, shift }
    }
}

/// Keyboard events produced by the parser from the input bytes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    /// Regular char (letter, number, symbol), could be multi-byte
    Char(char),
    /// Control char. Note that the char is stored as the uppercase caret-notation char (e.g. Ctrl('C'))
    Ctrl(char),
    /// Special key (arrow, Enter, etc.) and possible modifier (ctrl, alt, shift)
    Special(Key, Modifier),
}

/// KeyEventParser combines a multi-byte char decoder with an
/// escape sequence parser, and its [KeyEventParser::parse] method
/// takes a sequence of bytes and returns [KeyEvent]s
pub struct KeyEventParser {
    ch_decoder: CharDecoder,
    esc_seq_parser: EscSeqParser,
}

impl KeyEventParser {
    /// New [KeyEventParser]
    pub const fn new() -> Self {
        Self {
            ch_decoder: CharDecoder::new(),
            esc_seq_parser: EscSeqParser::new(),
        }
    }

    /// Helper function to map Char(ch) to special key events
    fn map_special_char(key_event: Option<KeyEvent>) -> Option<KeyEvent> {
        const CARET_NOTATION: u8 = 0x40;

        match key_event {
            Some(KeyEvent::Char(special::BS | special::DEL)) => {
                Some(KeyEvent::Special(Key::Backspace, Modifier::default()))
            }
            Some(KeyEvent::Char(special::CR | special::LF)) => {
                Some(KeyEvent::Special(Key::Enter, Modifier::default()))
            }
            Some(KeyEvent::Char(special::TAB)) => {
                Some(KeyEvent::Special(Key::Tab, Modifier::default()))
            }
            Some(KeyEvent::Char(ch)) if ch.is_control() => {
                match ch {
                    '\u{80}'..='\u{9F}' => None, // C1 set is not supported, drop
                    _ => Some(KeyEvent::Ctrl(char::from(ch as u8 + CARET_NOTATION))),
                }
            }
            other => other,
        }
    }

    /// Takes a series of bytes and returns [KeyEvent]s.
    ///
    /// Returns `None` if no [KeyEvent] has been identified and another byte
    /// is needed.
    /// Invalid sequences are dropped immediately.
    pub fn parse(&mut self, byte: u8) -> Option<KeyEvent> {
        // First decode the byte as a char
        let ch = self.ch_decoder.parse(byte)?;
        // Then look for an escape sequence
        Self::map_special_char(self.esc_seq_parser.parse(ch))
    }

    /// Returns the pushed back event (if present)
    pub fn pending_event(&mut self) -> Option<KeyEvent> {
        let ch = self.esc_seq_parser.pending_char()?;
        Self::map_special_char(self.esc_seq_parser.parse(ch))
    }

    /// Returns whether the escape sequence parser is mid-sequence
    pub fn is_mid_sequence(&self) -> bool {
        self.esc_seq_parser.is_mid_sequence()
    }

    /// Returns Esc if the escape sequence parser has timed out
    pub fn esc_seq_timeout(&mut self) -> Option<KeyEvent> {
        self.esc_seq_parser.esc_seq_timeout()
    }
}

/// A decoder for accumulating bytes into `char`.
/// There are min 1 and max 4 bytes per char.
#[derive(Default)]
struct CharDecoder {
    /// Byte buffer
    buf: [u8; 4],
    /// Next position to add a byte in the buffer
    pos: usize,
}

impl CharDecoder {
    /// New char decoder with size zero to indicate it is empty
    const fn new() -> Self {
        Self {
            buf: [0u8; 4],
            pos: 0,
        }
    }

    /// Parse a byte
    ///
    /// Returns `None` if the sequence is incomplete, or `Some(char)` if completed.
    /// Invalid byte sequences are immediately discarded without pushback but the
    /// last byte is kept and parsed again
    fn parse(&mut self, byte: u8) -> Option<char> {
        // Add the byte to our buffer
        self.buf[self.pos] = byte;
        self.pos += 1;
        // Now attempt to turn in a one-char str
        // Note that pos is never set to larger than 4 (buffer size) so no guard
        let buf = &self.buf[..self.pos];
        match str::from_utf8(buf) {
            Ok(s) => {
                // Reset and return the char
                self.pos = 0;
                s.chars().next()
            }
            Err(e) => match e.error_len() {
                None => None, // Mid-sequence, wait for next byte
                Some(_) => {
                    // If the latest byte the problem discard
                    if self.pos == 1 {
                        self.pos = 0;
                        None
                    } else {
                        // Invalid sequence, discard all but latest byte
                        self.pos = 0;
                        self.parse(byte)
                    }
                }
            },
        }
    }
}

/// Parser state enum
#[derive(Clone, Copy, Debug, Default)]
enum State {
    /// Normal state, not inside a sequence
    #[default]
    Normal,
    /// Found Esc - could be start of a sequence
    GotEscape,
    /// First optional parameter - identifies the key pressed
    KeyParam(u8),
    /// Second optional parameter - identifies the key and the modifier (alt, ctrl, etc.)
    ModifierParam { key: u8, modifier: u8 },
}

/// VT-100/ANSI escape sequence parser. Converts `char` sequences into [KeyEvent]s.
///
/// Parses ECMA-48 control sequences (CSI).
/// The full CSI control sequence has three byte classes:
/// 1. Parameter bytes are 0x30–0x3F: the digits plus : ; < = > ?.
/// 2. Intermediate bytes are 0x20–0x2F: space and punctuation like ! " # $. They're rare in input.
/// 3. Final bytes are 0x40–0x7E:
///
/// ┌───────────┬─────────────┐
/// │   Range   │ Characters  │
/// ├───────────┼─────────────┤
/// │ 0x40      │ @           │
/// ├───────────┼─────────────┤
/// │ 0x41–0x5A │ A–Z         │
/// ├───────────┼─────────────┤
/// │ 0x5B–0x60 │ [ \ ] ^ _ ` │
/// ├───────────┼─────────────┤
/// │ 0x61–0x7A │ a–z         │
/// ├───────────┼─────────────┤
/// │ 0x7B–0x7E │ { | } ~     │
/// └───────────┴─────────────┘
///
/// Every sequence
/// 1. Starts with Esc
/// 2. Followed by [
/// 3. Parameter bytes (optional numbers separated by ;)
/// 4. Intermediate bytes (optional, ignored and dropped in EASE)
/// 5. Exactly one final byte
///
/// Note SS3 (Esc, O, single byte) is not supported in EASE.
#[derive(Default)]
struct EscSeqParser {
    /// Current parser state
    state: State,
    /// Byte that would otherwise be lost in a sequence
    pushback: Option<char>,
}

impl EscSeqParser {
    /// Create a new EscSeqParser in Normal state with empty pushback
    const fn new() -> Self {
        Self {
            state: State::Normal,
            pushback: None,
        }
    }

    /// Helper function at the end of sequence
    ///
    /// Caller must ensure this is only called on end of sequence bytes 0x40..0x7E inclusive
    fn end_sequence(
        &mut self,
        final_char: char,
        modifier_raw: u8,
        key_param: u8,
    ) -> Option<KeyEvent> {
        // End of sequence, so  make sure we are back to Normal
        self.state = State::Normal;
        // Get the modifier for this modifier byte value
        let modifier = Modifier::from_raw(modifier_raw);
        match final_char {
            'A' => Some(KeyEvent::Special(Key::ArrowUp, modifier)),
            'B' => Some(KeyEvent::Special(Key::ArrowDown, modifier)),
            'C' => Some(KeyEvent::Special(Key::ArrowRight, modifier)),
            'D' => Some(KeyEvent::Special(Key::ArrowLeft, modifier)),
            'H' => Some(KeyEvent::Special(Key::Home, modifier)),
            'F' => Some(KeyEvent::Special(Key::End, modifier)),
            '~' => {
                // End of a digit sequence
                match key_param {
                    2 => Some(KeyEvent::Special(Key::Insert, modifier)),
                    3 => Some(KeyEvent::Special(Key::Delete, modifier)),
                    5 => Some(KeyEvent::Special(Key::PageUp, modifier)),
                    6 => Some(KeyEvent::Special(Key::PageDown, modifier)),
                    _ => None,
                }
            }
            // We do not support further sequences, e.g. M for mouse event.
            _ => None,
        }
    }

    /// Feeds a char into the parser and returns an `Option<KeyEvent>`.
    ///
    /// Returns the [KeyEvent] if the char sequence is matched, or
    /// `None` if the char fed into the parser does not complete a `KeyEvent`
    /// and more chars are needed to complete an escape sequence.
    ///
    /// If the sequence fails part way, all but the last pushback char is lost.
    ///
    /// Normal ──ESC──► GotEscape ──'['──► KeyParam(n) ──';'──► ModifierParam{key,mod} ──final──► Normal
    /// ▲                │                 │ digits: n*10+d        │ digits: mod*10+d
    /// │                └─other: Esc +    └─final 0x40–0x7E ──────┴──► map (final, key, mod) → Key or nothing
    /// │                  pushback                                   other char (control etc.) → abort + pushback
    ///
    fn parse(&mut self, ch: char) -> Option<KeyEvent> {
        match self.state {
            State::Normal => {
                if ch == special::ESC {
                    // Esc key - could be start of sequence
                    self.state = State::GotEscape;
                    None
                } else {
                    Some(KeyEvent::Char(ch))
                }
            }
            State::GotEscape => {
                match ch {
                    '[' => {
                        // Escape sequence is continuing
                        self.state = State::KeyParam(0); // Note that 0 is an invalid key value so will not propagate to an incorrect character or special key
                        None
                    }
                    // We don't support SS3 - everything else is treated as a plain escape
                    _ => {
                        self.state = State::Normal;
                        // Push back the last char so it is not lost
                        self.pushback = Some(ch);
                        Some(KeyEvent::Special(Key::Esc, Modifier::default()))
                    }
                }
            }
            State::KeyParam(n) => {
                match ch {
                    '0'..='9' => {
                        // Accumulate the digit's value using n*10 + val
                        let val = ch as u8 - b'0'; // Cannot underflow due to match arm
                        self.state = State::KeyParam(n.saturating_mul(10).saturating_add(val));
                        None
                    }
                    ';' => {
                        // Modifier key is also held, need to accumulate those digits
                        self.state = State::ModifierParam {
                            key: n,
                            modifier: 0,
                        };
                        None
                    }
                    '\x20'..='\x3F' => None, // Just swallow these unwanted chars
                    '\x40'..='\x7E' => self.end_sequence(ch, 0, n),
                    // Failed sequence, save the last char but ESC [ are lost
                    _ => {
                        self.state = State::Normal;
                        self.pushback = Some(ch);
                        None
                    }
                }
            }
            State::ModifierParam { key, modifier: m } => {
                match ch {
                    '0'..='9' => {
                        // Accumulate the modifier's value using m*10 + val
                        let val = ch as u8 - b'0'; // Cannot underflow due to match arm
                        self.state = State::ModifierParam {
                            key,
                            modifier: m.saturating_mul(10).saturating_add(val),
                        };
                        None
                    }
                    '\x20'..='\x3F' => None, // Just swallow these unwanted chars
                    '\x40'..='\x7E' => self.end_sequence(ch, m, key),
                    // Failed sequence, save the last char but ESC [ and key are lost
                    _ => {
                        self.state = State::Normal;
                        self.pushback = Some(ch);
                        None
                    }
                }
            }
        }
    }

    /// Called when no more chars arrive within the timeout. Returns `Some(Esc)` if
    /// a bare ESC was pending, or `None` if an incomplete CSI sequence is discarded.
    fn esc_seq_timeout(&mut self) -> Option<KeyEvent> {
        self.pushback = None;
        match self.state {
            State::Normal => None,
            State::GotEscape => {
                self.state = State::Normal;
                Some(KeyEvent::Special(Key::Esc, Modifier::default()))
            }
            _ => {
                self.state = State::Normal;
                None
            }
        }
    }

    /// Takes the pushed-back char, if any. A char is pushed back when an escape sequence
    /// terminates with a char that wasn't part of the sequence.
    fn pending_char(&mut self) -> Option<char> {
        self.pushback.take()
    }

    /// Returns whether the parser is mid-sequence or not
    fn is_mid_sequence(&self) -> bool {
        match self.state {
            State::Normal => false,
            State::GotEscape
            | State::KeyParam(_)
            | State::ModifierParam {
                key: _,
                modifier: _,
            } => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use ease_ulib::ascii;

    /// No-modifier shorthand used throughout the tests.
    const NO_MOD: Modifier = Modifier {
        alt: false,
        ctrl: false,
        shift: false,
    };

    /// Helper: feed a byte slice into the parser and collect every result,
    /// draining the pushed-back event after each byte (the same loop shape
    /// as main.rs).
    fn parse_all(p: &mut KeyEventParser, bytes: &[u8]) -> Vec<Option<KeyEvent>> {
        let mut events = Vec::new();
        for &b in bytes {
            events.push(p.parse(b));
            while let Some(event) = p.pending_event() {
                events.push(Some(event));
            }
        }
        events
    }

    // ── Normal state: single-byte keys ──────────────────────────────

    #[test]
    fn regular_ascii_bytes() {
        let mut p = KeyEventParser::new();
        assert_eq!(p.parse(b'a'), Some(KeyEvent::Char('a')));
        assert_eq!(p.parse(b'z'), Some(KeyEvent::Char('z')));
        assert_eq!(p.parse(b' '), Some(KeyEvent::Char(' ')));
        assert_eq!(p.parse(b'0'), Some(KeyEvent::Char('0')));
        assert_eq!(p.parse(b'~'), Some(KeyEvent::Char('~')));
    }

    #[test]
    fn enter() {
        let mut p = KeyEventParser::new();
        assert_eq!(
            p.parse(ascii::CR),
            Some(KeyEvent::Special(Key::Enter, NO_MOD))
        );
    }

    #[test]
    fn line_feed_is_enter() {
        // LF (Ctrl-J) is Enter, as in readline: it is what arrives when
        // text reaches the console with Unix line endings.
        let mut p = KeyEventParser::new();
        assert_eq!(
            p.parse(ascii::LF),
            Some(KeyEvent::Special(Key::Enter, NO_MOD))
        );
    }

    #[test]
    fn pushed_back_line_feed_is_enter() {
        // ESC then LF: the LF is pushed back and must still be Enter.
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, &[ascii::ESC, ascii::LF]);
        assert_eq!(
            events,
            [
                None,
                Some(KeyEvent::Special(Key::Esc, NO_MOD)),
                Some(KeyEvent::Special(Key::Enter, NO_MOD)),
            ]
        );
    }

    #[test]
    fn cr_lf_is_two_enters() {
        // Known behaviour: a Windows line ending gives an empty second line.
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, &[ascii::CR, ascii::LF]);
        assert_eq!(
            events,
            [
                Some(KeyEvent::Special(Key::Enter, NO_MOD)),
                Some(KeyEvent::Special(Key::Enter, NO_MOD)),
            ]
        );
    }

    #[test]
    fn backspace_del() {
        let mut p = KeyEventParser::new();
        assert_eq!(
            p.parse(ascii::DEL),
            Some(KeyEvent::Special(Key::Backspace, NO_MOD))
        );
    }

    #[test]
    fn backspace_bs() {
        let mut p = KeyEventParser::new();
        assert_eq!(
            p.parse(ascii::BS),
            Some(KeyEvent::Special(Key::Backspace, NO_MOD))
        );
    }

    // ── VT100 letter-final keys (family 1) ──────────────────────────

    #[test]
    fn arrow_up() {
        let mut p = KeyEventParser::new();
        assert_eq!(p.parse(ascii::ESC), None);
        assert_eq!(p.parse(b'['), None);
        assert_eq!(p.parse(b'A'), Some(KeyEvent::Special(Key::ArrowUp, NO_MOD)));
    }

    #[test]
    fn arrow_down() {
        let mut p = KeyEventParser::new();
        assert_eq!(p.parse(ascii::ESC), None);
        assert_eq!(p.parse(b'['), None);
        assert_eq!(
            p.parse(b'B'),
            Some(KeyEvent::Special(Key::ArrowDown, NO_MOD))
        );
    }

    #[test]
    fn arrow_right() {
        let mut p = KeyEventParser::new();
        assert_eq!(p.parse(ascii::ESC), None);
        assert_eq!(p.parse(b'['), None);
        assert_eq!(
            p.parse(b'C'),
            Some(KeyEvent::Special(Key::ArrowRight, NO_MOD))
        );
    }

    #[test]
    fn arrow_left() {
        let mut p = KeyEventParser::new();
        assert_eq!(p.parse(ascii::ESC), None);
        assert_eq!(p.parse(b'['), None);
        assert_eq!(
            p.parse(b'D'),
            Some(KeyEvent::Special(Key::ArrowLeft, NO_MOD))
        );
    }

    #[test]
    fn home() {
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[H");
        assert_eq!(
            events,
            [None, None, Some(KeyEvent::Special(Key::Home, NO_MOD))]
        );
    }

    #[test]
    fn end() {
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[F");
        assert_eq!(
            events,
            [None, None, Some(KeyEvent::Special(Key::End, NO_MOD))]
        );
    }

    // ── VT220 tilde-final keys (family 2) ───────────────────────────

    #[test]
    fn insert() {
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[2~");
        assert_eq!(
            events,
            [
                None,
                None,
                None,
                Some(KeyEvent::Special(Key::Insert, NO_MOD))
            ]
        );
    }

    #[test]
    fn delete() {
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[3~");
        assert_eq!(
            events,
            [
                None,
                None,
                None,
                Some(KeyEvent::Special(Key::Delete, NO_MOD))
            ]
        );
    }

    #[test]
    fn page_up() {
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[5~");
        assert_eq!(
            events,
            [
                None,
                None,
                None,
                Some(KeyEvent::Special(Key::PageUp, NO_MOD))
            ]
        );
    }

    #[test]
    fn page_down() {
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[6~");
        assert_eq!(
            events,
            [
                None,
                None,
                None,
                Some(KeyEvent::Special(Key::PageDown, NO_MOD))
            ]
        );
    }

    // ── Modifiers ───────────────────────────────────────────────────

    #[test]
    fn ctrl_arrow_up() {
        // ESC [ 1 ; 5 A — Ctrl+Up
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[1;5A");
        let ctrl = Modifier {
            alt: false,
            ctrl: true,
            shift: false,
        };
        assert_eq!(
            *events.last().unwrap(),
            Some(KeyEvent::Special(Key::ArrowUp, ctrl))
        );
    }

    #[test]
    fn shift_delete() {
        // ESC [ 3 ; 2 ~ — Shift+Delete
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[3;2~");
        let shift = Modifier {
            alt: false,
            ctrl: false,
            shift: true,
        };
        assert_eq!(
            *events.last().unwrap(),
            Some(KeyEvent::Special(Key::Delete, shift))
        );
    }

    #[test]
    fn ctrl_shift_arrow_right() {
        // ESC [ 1 ; 6 C — Ctrl+Shift+Right
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[1;6C");
        let ctrl_shift = Modifier {
            alt: false,
            ctrl: true,
            shift: true,
        };
        assert_eq!(
            *events.last().unwrap(),
            Some(KeyEvent::Special(Key::ArrowRight, ctrl_shift))
        );
    }

    #[test]
    fn alt_arrow_left() {
        // ESC [ 1 ; 3 D — Alt+Left
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[1;3D");
        let alt = Modifier {
            alt: true,
            ctrl: false,
            shift: false,
        };
        assert_eq!(
            *events.last().unwrap(),
            Some(KeyEvent::Special(Key::ArrowLeft, alt))
        );
    }

    #[test]
    fn ctrl_alt_home() {
        // ESC [ 1 ; 7 H — Ctrl+Alt+Home
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[1;7H");
        let ctrl_alt = Modifier {
            alt: true,
            ctrl: true,
            shift: false,
        };
        assert_eq!(
            *events.last().unwrap(),
            Some(KeyEvent::Special(Key::Home, ctrl_alt))
        );
    }

    // ── Modifier::from_raw edge cases ───────────────────────────────

    #[test]
    fn modifier_from_raw_zero_is_no_modifier() {
        // raw 0 (no modifier parameter present) → saturating_sub(1) = 0, all bits clear
        assert_eq!(Modifier::from_raw(0), NO_MOD);
    }

    #[test]
    fn modifier_from_raw_one_is_no_modifier() {
        // raw 1 means "unmodified" in xterm encoding (1 + 0 bits)
        assert_eq!(Modifier::from_raw(1), NO_MOD);
    }

    // ── Bare Esc and pushback ───────────────────────────────────────

    #[test]
    fn bare_esc_pushes_back_following_byte() {
        let mut p = KeyEventParser::new();
        assert_eq!(p.parse(ascii::ESC), None);
        // A non-bracket char after ESC → bare Esc, with the char pushed back
        assert_eq!(p.parse(b'x'), Some(KeyEvent::Special(Key::Esc, NO_MOD)));
        assert_eq!(p.pending_event(), Some(KeyEvent::Char('x')));
        assert_eq!(p.pending_event(), None); // drained
    }

    #[test]
    fn bare_esc_pushback_drains_to_char() {
        // The full caller loop: ESC then 'x' produces Esc + Char('x')
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1bx");
        assert_eq!(
            events,
            [
                None,                                      // ESC consumed
                Some(KeyEvent::Special(Key::Esc, NO_MOD)), // 'x' triggers bare Esc
                Some(KeyEvent::Char('x')),                 // pushback 'x' parsed
            ]
        );
    }

    // ── Unknown sequences ───────────────────────────────────────────

    #[test]
    fn unknown_csi_final_drops_silently() {
        // ESC [ Z (Shift+Tab) — valid sequence, not a key we support
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[Z");
        assert_eq!(events, [None, None, None]);
        assert_eq!(p.pending_event(), None); // no pushback
    }

    #[test]
    fn unknown_tilde_key_param_drops_silently() {
        // ESC [ 1 5 ~ (F5) — valid sequence, key param 15 not in our table
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[15~");
        assert_eq!(*events.last().unwrap(), None);
        assert_eq!(p.pending_event(), None);
    }

    #[test]
    fn delete_does_not_leak_tilde() {
        // ESC [ 3 ~ then 'a' — the tilde must not appear as Char('~')
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[3~a");
        assert_eq!(
            events,
            [
                None,
                None,
                None,
                Some(KeyEvent::Special(Key::Delete, NO_MOD)),
                Some(KeyEvent::Char('a')),
            ]
        );
    }

    // ── Abort (non-sequence byte mid-CSI) ───────────────────────────

    #[test]
    fn control_byte_mid_csi_aborts_and_pushes_back() {
        // ESC [ then a control byte (0x01 = Ctrl+A) — not part of the CSI
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, &[ascii::ESC, b'[', 0x01]);
        // The abort drops ESC [, pushback produces Ctrl('A')
        assert_eq!(events, [None, None, None, Some(KeyEvent::Ctrl('A'))]);
    }

    // ── Intermediate bytes swallowed ────────────────────────────────

    #[test]
    fn intermediate_bytes_swallowed() {
        // ESC [ $ A — the '$' (0x24, an intermediate byte) is consumed
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[$A");
        assert_eq!(
            *events.last().unwrap(),
            Some(KeyEvent::Special(Key::ArrowUp, NO_MOD))
        );
    }

    // ── Timeout ─────────────────────────────────────────────────────

    #[test]
    fn timeout_in_normal_returns_none() {
        let mut p = KeyEventParser::new();
        assert_eq!(p.esc_seq_timeout(), None);
    }

    #[test]
    fn timeout_in_got_escape_returns_esc() {
        let mut p = KeyEventParser::new();
        let _ = p.parse(ascii::ESC);
        assert_eq!(
            p.esc_seq_timeout(),
            Some(KeyEvent::Special(Key::Esc, NO_MOD))
        );
        // Back to Normal
        assert_eq!(p.parse(b'a'), Some(KeyEvent::Char('a')));
    }

    #[test]
    fn timeout_in_key_param_discards() {
        let mut p = KeyEventParser::new();
        let _ = p.parse(ascii::ESC);
        let _ = p.parse(b'[');
        assert_eq!(p.esc_seq_timeout(), None);
        // Back to Normal
        assert_eq!(p.parse(b'a'), Some(KeyEvent::Char('a')));
    }

    #[test]
    fn timeout_in_modifier_param_discards() {
        let mut p = KeyEventParser::new();
        let _ = p.parse(ascii::ESC);
        let _ = p.parse(b'[');
        let _ = p.parse(b'1');
        let _ = p.parse(b';');
        assert_eq!(p.esc_seq_timeout(), None);
        assert_eq!(p.parse(b'a'), Some(KeyEvent::Char('a')));
    }

    // ── State reset ─────────────────────────────────────────────────

    #[test]
    fn state_resets_after_complete_sequence() {
        let mut p = KeyEventParser::new();
        let _ = parse_all(&mut p, b"\x1b[A");
        assert_eq!(p.parse(b'x'), Some(KeyEvent::Char('x')));
    }

    #[test]
    fn state_resets_after_unknown_final() {
        let mut p = KeyEventParser::new();
        let _ = parse_all(&mut p, b"\x1b[Z");
        assert_eq!(p.parse(b'a'), Some(KeyEvent::Char('a')));
    }

    #[test]
    fn consecutive_arrows() {
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[A\x1b[B");
        assert_eq!(
            events,
            [
                None,
                None,
                Some(KeyEvent::Special(Key::ArrowUp, NO_MOD)),
                None,
                None,
                Some(KeyEvent::Special(Key::ArrowDown, NO_MOD)),
            ]
        );
    }

    // ── Digit overflow (saturating) ─────────────────────────────────

    #[test]
    fn long_digit_run_saturates_and_drops() {
        // ESC [ 9 9 9 ~ — saturates to 255, which is not in the tilde table
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[999~");
        assert_eq!(*events.last().unwrap(), None);
    }

    // ── Mixed input ─────────────────────────────────────────────────

    #[test]
    fn mixed_typing_and_sequences() {
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"hi\x0d\x1b[A!");
        assert_eq!(
            events,
            [
                Some(KeyEvent::Char('h')),
                Some(KeyEvent::Char('i')),
                Some(KeyEvent::Special(Key::Enter, NO_MOD)),
                None,
                None,
                Some(KeyEvent::Special(Key::ArrowUp, NO_MOD)),
                Some(KeyEvent::Char('!')),
            ]
        );
    }

    // ── Keys mapped from single chars ───────────────────────────────

    #[test]
    fn tab_is_a_key() {
        let mut p = KeyEventParser::new();
        assert_eq!(
            p.parse(ascii::TAB),
            Some(KeyEvent::Special(Key::Tab, NO_MOD))
        );
    }

    #[test]
    fn other_control_chars_are_ctrl_keys() {
        // Ctrl-C (0x03) has no key of its own; it comes out as Ctrl with
        // its caret-notation letter and the line editor decides.
        let mut p = KeyEventParser::new();
        assert_eq!(p.parse(0x03), Some(KeyEvent::Ctrl('C')));
    }

    #[test]
    fn every_control_byte_maps_to_its_caret_letter() {
        // Byte + 0x40 for the whole C0 range: 0x00 is ^@, 0x1F is ^_.
        // BS, TAB, LF, CR and ESC have keys of their own.
        let mut p = KeyEventParser::new();
        for byte in 0x00..=0x1Fu8 {
            if matches!(
                byte,
                ascii::BS | ascii::TAB | ascii::LF | ascii::CR | ascii::ESC
            ) {
                continue;
            }
            let letter = char::from(byte + 0x40);
            assert_eq!(
                p.parse(byte),
                Some(KeyEvent::Ctrl(letter)),
                "byte {byte:#04x}"
            );
        }
    }

    #[test]
    fn no_control_char_comes_out_as_char() {
        // Whatever each control byte becomes, it is never a Char that the
        // line editor would put on the line.
        for byte in (0x00..=0x1Fu8).chain([ascii::DEL]) {
            let mut p = KeyEventParser::new();
            let event = p.parse(byte).or_else(|| p.esc_seq_timeout());
            assert!(
                !matches!(event, Some(KeyEvent::Char(_))),
                "byte {byte:#04x}"
            );
        }
    }

    #[test]
    fn c1_control_chars_are_dropped() {
        // U+0085 (NEL) is the two bytes C2 85. It is a control character
        // but not a Ctrl key, so it produces nothing.
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, "\u{85}\u{80}\u{9F}".as_bytes());
        assert!(events.iter().all(Option::is_none));
        // The parser is still usable afterwards
        assert_eq!(p.parse(b'a'), Some(KeyEvent::Char('a')));
    }

    #[test]
    fn pushed_back_char_is_mapped_to_its_key() {
        // ESC then Enter: the CR is pushed back, and must still come out
        // as Enter and not as a raw Char.
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, &[ascii::ESC, ascii::CR]);
        assert_eq!(
            events,
            [
                None,
                Some(KeyEvent::Special(Key::Esc, NO_MOD)),
                Some(KeyEvent::Special(Key::Enter, NO_MOD)),
            ]
        );
    }

    #[test]
    fn esc_esc_leaves_second_esc_pending() {
        // The second ESC ends the first as a bare Esc and starts again.
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, &[ascii::ESC, ascii::ESC]);
        assert_eq!(events, [None, Some(KeyEvent::Special(Key::Esc, NO_MOD))]);
        assert!(p.is_mid_sequence());
        assert_eq!(
            p.esc_seq_timeout(),
            Some(KeyEvent::Special(Key::Esc, NO_MOD))
        );
        assert!(!p.is_mid_sequence());
    }

    #[test]
    fn is_mid_sequence_tracks_the_escape_parser() {
        let mut p = KeyEventParser::new();
        assert!(!p.is_mid_sequence());
        let _ = p.parse(ascii::ESC);
        assert!(p.is_mid_sequence());
        let _ = p.parse(b'[');
        assert!(p.is_mid_sequence());
        let _ = p.parse(b'A');
        assert!(!p.is_mid_sequence());
    }

    // ── Multi-byte chars through the whole parser ───────────────────

    #[test]
    fn multibyte_chars_arrive_whole() {
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, "é€😀".as_bytes());
        assert_eq!(
            events,
            [
                None,
                Some(KeyEvent::Char('é')),
                None,
                None,
                Some(KeyEvent::Char('€')),
                None,
                None,
                None,
                Some(KeyEvent::Char('😀')),
            ]
        );
    }

    #[test]
    fn is_not_mid_sequence_inside_a_multibyte_char() {
        // Only an escape sequence arms the read timeout; a half-received
        // char waits for its next byte.
        let mut p = KeyEventParser::new();
        assert_eq!(p.parse(0xE2), None);
        assert!(!p.is_mid_sequence());
    }

    #[test]
    fn enter_after_truncated_char_is_not_lost() {
        // E2 82 is the start of € with its last byte missing.
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, &[0xE2, 0x82, ascii::CR]);
        assert_eq!(
            events,
            [None, None, Some(KeyEvent::Special(Key::Enter, NO_MOD))]
        );
    }

    #[test]
    fn arrow_after_truncated_char_still_parses() {
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\xE2\x82\x1b[A");
        assert_eq!(
            *events.last().unwrap(),
            Some(KeyEvent::Special(Key::ArrowUp, NO_MOD))
        );
        // The stale E2 82 is gone: a late AC must not complete a €
        assert_eq!(p.parse(0xAC), None);
        assert_eq!(p.parse(b'a'), Some(KeyEvent::Char('a')));
    }

    #[test]
    fn multibyte_char_mid_csi_aborts_and_pushes_back() {
        // ESC [ then é: not part of a sequence, so the sequence is
        // dropped and the é comes back through the pushback.
        let mut p = KeyEventParser::new();
        let events = parse_all(&mut p, b"\x1b[\xC3\xA9");
        assert_eq!(events, [None, None, None, None, Some(KeyEvent::Char('é'))]);
        assert!(!p.is_mid_sequence());
    }

    // ── CharDecoder ─────────────────────────────────────────────────

    /// Helper: feed bytes into a decoder and collect every result.
    fn decode_all(d: &mut CharDecoder, bytes: &[u8]) -> Vec<Option<char>> {
        bytes.iter().map(|&b| d.parse(b)).collect()
    }

    #[test]
    fn decoder_ascii_comes_out_at_once() {
        let mut d = CharDecoder::new();
        for byte in 0x00..=0x7Fu8 {
            assert_eq!(d.parse(byte), Some(char::from(byte)));
        }
    }

    #[test]
    fn decoder_two_byte_char() {
        let mut d = CharDecoder::new();
        assert_eq!(decode_all(&mut d, "é".as_bytes()), [None, Some('é')]);
    }

    #[test]
    fn decoder_three_byte_char() {
        let mut d = CharDecoder::new();
        assert_eq!(decode_all(&mut d, "€".as_bytes()), [None, None, Some('€')]);
    }

    #[test]
    fn decoder_four_byte_char() {
        let mut d = CharDecoder::new();
        assert_eq!(
            decode_all(&mut d, "😀".as_bytes()),
            [None, None, None, Some('😀')]
        );
    }

    #[test]
    fn decoder_first_and_last_char_of_each_length() {
        let mut d = CharDecoder::new();
        for c in [
            '\u{0}',
            '\u{7F}',
            '\u{80}',
            '\u{7FF}',
            '\u{800}',
            '\u{D7FF}', // last before the surrogates
            '\u{E000}', // first after the surrogates
            '\u{FFFF}',
            '\u{10000}',
            '\u{10FFFF}',
        ] {
            let mut buf = [0u8; 4];
            let bytes = c.encode_utf8(&mut buf).as_bytes();
            let (last, rest) = bytes.split_last().unwrap();
            for &b in rest {
                assert_eq!(d.parse(b), None, "{c:?}");
            }
            assert_eq!(d.parse(*last), Some(c), "{c:?}");
        }
    }

    #[test]
    fn decoder_stray_continuation_byte_is_dropped() {
        // The chars after it must arrive on time and all of them.
        let mut d = CharDecoder::new();
        assert_eq!(
            decode_all(&mut d, &[0x80, b'a', b'b']),
            [None, Some('a'), Some('b')]
        );
    }

    #[test]
    fn decoder_byte_that_is_never_valid_is_dropped() {
        // C0 and C1 could only start an overlong form; F5..FF are too big.
        let mut d = CharDecoder::new();
        for bad in [0xC0, 0xC1, 0xF5, 0xF8, 0xFE, 0xFF] {
            assert_eq!(d.parse(bad), None, "{bad:#x}");
            assert_eq!(d.parse(b'a'), Some('a'), "{bad:#x}");
        }
    }

    #[test]
    fn decoder_ascii_after_truncated_char_is_kept() {
        let mut d = CharDecoder::new();
        assert_eq!(
            decode_all(&mut d, &[0xE2, 0x82, b'a', b'b']),
            [None, None, Some('a'), Some('b')]
        );
    }

    #[test]
    fn decoder_new_char_after_truncated_char_is_kept() {
        // E2 (start of €) then a whole é
        let mut d = CharDecoder::new();
        assert_eq!(
            decode_all(&mut d, &[0xE2, 0xC3, 0xA9]),
            [None, None, Some('é')]
        );
    }

    #[test]
    fn decoder_bad_byte_after_truncated_char_drops_both() {
        let mut d = CharDecoder::new();
        assert_eq!(
            decode_all(&mut d, &[0xE2, 0xFF, b'a']),
            [None, None, Some('a')]
        );
    }

    #[test]
    fn decoder_overlong_forms_are_rejected() {
        // Each of these would decode to a char that has a shorter form.
        let mut d = CharDecoder::new();
        for bytes in [
            &[0xC0, 0x80][..],             // NUL in 2 bytes
            &[0xC1, 0xBF][..],             // DEL in 2 bytes
            &[0xE0, 0x80, 0x80][..],       // NUL in 3 bytes
            &[0xE0, 0x9F, 0xBF][..],       // U+07FF in 3 bytes
            &[0xF0, 0x80, 0x80, 0x80][..], // NUL in 4 bytes
            &[0xF0, 0x8F, 0xBF, 0xBF][..], // U+FFFF in 4 bytes
        ] {
            for &b in bytes {
                assert_eq!(d.parse(b), None, "{bytes:x?}");
            }
            assert_eq!(d.parse(b'a'), Some('a'), "{bytes:x?}");
        }
    }

    #[test]
    fn decoder_surrogates_are_rejected() {
        // U+D800 and U+DFFF written as 3-byte sequences
        let mut d = CharDecoder::new();
        for bytes in [[0xED, 0xA0, 0x80], [0xED, 0xBF, 0xBF]] {
            assert_eq!(decode_all(&mut d, &bytes), [None, None, None]);
            assert_eq!(d.parse(b'a'), Some('a'));
        }
    }

    #[test]
    fn decoder_above_last_code_point_is_rejected() {
        // F4 90 80 80 would be U+110000
        let mut d = CharDecoder::new();
        assert_eq!(
            decode_all(&mut d, &[0xF4, 0x90, 0x80, 0x80]),
            [None, None, None, None]
        );
        assert_eq!(d.parse(b'a'), Some('a'));
    }

    #[test]
    fn decoder_any_byte_after_any_prefix_recovers() {
        // Whatever arrives, the decoder must not panic (the buffer index
        // stays in range) and must decode plain ASCII straight afterwards.
        for prefix in [
            &[][..],
            &[0xC3][..],
            &[0xE2][..],
            &[0xE2, 0x82][..],
            &[0xF0][..],
            &[0xF0, 0x9F][..],
            &[0xF0, 0x9F, 0x98][..],
        ] {
            for byte in 0x00..=0xFFu8 {
                let mut d = CharDecoder::new();
                let _ = decode_all(&mut d, prefix);
                let got = d.parse(byte);
                if byte.is_ascii() {
                    assert_eq!(got, Some(char::from(byte)), "{prefix:x?} {byte:#x}");
                }
                // Three ASCII bytes flush anything still held
                let tail = decode_all(&mut d, b"xyz");
                assert_eq!(tail.last(), Some(&Some('z')), "{prefix:x?} {byte:#x}");
            }
        }
    }
}
