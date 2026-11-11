//! Escape sequence parser
//!
//! Parses raw bytes into [KeyEvent]s. Control sequences are parsed
//! to identify more complex keystrokes.

use ease_ulib::ascii;

/// VT escape sequences, used to instruct the console
pub mod vt_out {
    /// Move cursor one step to the right
    pub const CURSOR_RIGHT: &[u8] = b"\x1b[C";
    /// Erase to the end of the line
    pub const ERASE_TO_END_OF_LINE: &[u8] = b"\x1b[K";
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
        Self {
            alt,
            ctrl,
            shift,
        }
    }
}

/// Special keys recognised by the escape sequence parser.
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
}

/// Keyboard events produced by the parser from the input bytes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    /// Regular byte (letter, number, symbol)
    Char(u8),
    /// Special key (arrow, Enter, etc.) and possible modifier (ctrl, alt, shift)
    Special(Key, Modifier),
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

/// VT-100/ANSI escape sequence parser. Converts raw byte sequences into [KeyEvent]s.
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
pub struct EscSeqParser {
    /// Current parser state
    state: State,
    /// Byte that would otherwise be lost in a sequence
    pushback: Option<u8>,
}

impl EscSeqParser {
    /// Create a new EscSeqParser in Normal state with empty pushback
    pub(super) const fn new() -> Self {
        Self {
            state: State::Normal,
            pushback: None,
        }
    }

    /// Helper function at the end of sequence
    ///
    /// Caller must ensure this is only called on end of sequence bytes 0x40..0x7E inclusive
    fn end_sequence(&mut self, final_byte: u8, modifier_raw: u8, key_param: u8) -> Option<KeyEvent> {
        // End of sequence, so  make sure we are back to Normal
        self.state = State::Normal;
        // Get the modifier for this modifier byte value
        let modifier = Modifier::from_raw(modifier_raw);
        match final_byte {
            b'A' => Some(KeyEvent::Special(Key::ArrowUp, modifier)),
            b'B' => Some(KeyEvent::Special(Key::ArrowDown, modifier)),
            b'C' => Some(KeyEvent::Special(Key::ArrowRight, modifier)),
            b'D' => Some(KeyEvent::Special(Key::ArrowLeft, modifier)),
            b'H' => Some(KeyEvent::Special(Key::Home, modifier)),
            b'F' => Some(KeyEvent::Special(Key::End, modifier)),
            b'~' => {
                // End of a digit sequence
                match key_param {
                    2 => Some(KeyEvent::Special(Key::Insert, modifier)),
                    3 => Some(KeyEvent::Special(Key::Delete, modifier)),
                    5 => Some(KeyEvent::Special(Key::PageUp, modifier)),
                    6 => Some(KeyEvent::Special(Key::PageDown, modifier)),
                    _ => None,
                }
            },
            // We do not support further sequences, e.g. M for mouse event.
            _ => None
        }
    }

    /// Feeds a byte into the parser and returns an `Option<KeyEvent>`.
    ///
    /// Returns the [KeyEvent] if the byte sequence is matched, or
    /// `None` if the byte fed into the parser does not complete a `KeyEvent`
    /// and more bytes are needed to complete an escape sequence.
    ///
    /// If the sequence fails part way, all but the last pushback byte is lost.
    ///
    ///
    /// Normal ──ESC──► GotEscape ──'['──► KeyParam(n) ──';'──► ModifierParam{key,mod} ──final──► Normal
    /// ▲                │                 │ digits: n*10+d        │ digits: mod*10+d
    /// │                └─other: Esc +    └─final 0x40–0x7E ──────┴──► map (final, key, mod) → Key or nothing
    /// │                  pushback                                   other byte (control etc.) → abort + pushback
    ///
    pub fn parse(&mut self, byte: u8) -> Option<KeyEvent> {
        match self.state {
            State::Normal => {
                match byte {
                    ascii::ESC => {
                        // Esc key - could be start of sequence
                        self.state = State::GotEscape;
                        None
                    }
                    ascii::CR => {
                        // Enter
                        Some(KeyEvent::Special(Key::Enter, Modifier::default(),))
                    }
                    ascii::BS | ascii::DEL => {
                        Some(KeyEvent::Special(Key::Backspace, Modifier::default()))
                    }
                    _ => Some(KeyEvent::Char(byte)),
                }
            }
            State::GotEscape => {
                match byte {
                    b'[' => {
                        // Escape sequence is continuing
                        self.state = State::KeyParam(0);    // Note that 0 is an invalid key value so will not propagate to an incorrect character or special key
                        None
                    }
                    // We don't support SS3 - everything else is treated as a plain escape
                    _ => {
                        self.state = State::Normal;
                        // Push back the last byte so it is not lost
                        self.pushback = Some(byte);
                        Some(KeyEvent::Special(Key::Esc, Modifier::default()))
                    }
                }
            }
            State::KeyParam(n) => {
                match byte {
                    b'0'..=b'9' => {
                        // Accumulate the digit's value using n*10 + val
                        let val = byte - b'0'; // Cannot underflow due to match arm
                        self.state = State::KeyParam(n.saturating_mul(10).saturating_add(val));
                        None
                    }
                    b';' => {
                        // Modifier key is also held, need to accumulate those digits
                        self.state = State::ModifierParam { key: n, modifier: 0};
                        None
                    }
                    0x20..=0x3F => None, // Just swallow these unwanted bytes
                    0x40..=0x7E => self.end_sequence(byte, 0, n),
                    // Failed sequence, save the last byte but ESC [ are lost
                    _ => {
                        self.state = State::Normal;
                        self.pushback = Some(byte);
                        None
                    }
                }
            }
            State::ModifierParam { key, modifier: m } => {
                match byte {
                    b'0'..=b'9' => {
                        // Accumulate the modifier's value using m*10 + val
                        let val = byte - b'0'; // Cannot underflow due to match arm
                        self.state = State::ModifierParam {key, modifier: m.saturating_mul(10).saturating_add(val)};
                        None
                    }
                    0x20..=0x3F => None, // Just swallow these unwanted bytes
                    0x40..=0x7E => self.end_sequence(byte, m, key),
                    // Failed sequence, save the last byte but ESC [ and key are lost
                    _ => {
                        self.state = State::Normal;
                        self.pushback = Some(byte);
                        None
                    }
                }
            }
        }
    }

    /// Called when no more bytes arrive within the timeout. Returns `Some(Esc)` if
    /// a bare ESC was pending, or `None` if an incomplete CSI sequence is discarded.
    pub fn esc_seq_timeout(&mut self) -> Option<KeyEvent> {
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

    /// Takes the pushed-back byte, if any. A byte is pushed back when an escape sequence
    /// terminates with a byte that wasn't part of the sequence.
    pub fn pending_byte(&mut self) -> Option<u8> {
        self.pushback.take()
    }

    /// Returns whether the parser is mid-sequence or not
    pub fn is_mid_sequence(&self) -> bool {
        match self.state {
            State::Normal => false,
            State::GotEscape | State::KeyParam(_) | State::ModifierParam { key: _, modifier: _ } => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No-modifier shorthand used throughout the tests.
    const NO_MOD: Modifier = Modifier { alt: false, ctrl: false, shift: false };

    /// Helper: feed a byte slice into the parser and collect all events,
    /// draining pushback after each byte (the same loop shape as main.rs).
    fn parse_all(p: &mut EscSeqParser, bytes: &[u8]) -> alloc::vec::Vec<Option<KeyEvent>> {
        let mut events = alloc::vec::Vec::new();
        for &b in bytes {
            events.push(p.parse(b));
            while let Some(pb) = p.pending_byte() {
                events.push(p.parse(pb));
            }
        }
        events
    }

    // ── Normal state: single-byte keys ──────────────────────────────

    #[test]
    fn regular_ascii_bytes() {
        let mut p = EscSeqParser::default();
        assert_eq!(p.parse(b'a'), Some(KeyEvent::Char(b'a')));
        assert_eq!(p.parse(b'z'), Some(KeyEvent::Char(b'z')));
        assert_eq!(p.parse(b' '), Some(KeyEvent::Char(b' ')));
        assert_eq!(p.parse(b'0'), Some(KeyEvent::Char(b'0')));
        assert_eq!(p.parse(b'~'), Some(KeyEvent::Char(b'~')));
    }

    #[test]
    fn enter() {
        let mut p = EscSeqParser::default();
        assert_eq!(p.parse(ascii::CR), Some(KeyEvent::Special(Key::Enter, NO_MOD)));
    }

    #[test]
    fn backspace_del() {
        let mut p = EscSeqParser::default();
        assert_eq!(p.parse(ascii::DEL), Some(KeyEvent::Special(Key::Backspace, NO_MOD)));
    }

    #[test]
    fn backspace_bs() {
        let mut p = EscSeqParser::default();
        assert_eq!(p.parse(ascii::BS), Some(KeyEvent::Special(Key::Backspace, NO_MOD)));
    }

    // ── VT100 letter-final keys (family 1) ──────────────────────────

    #[test]
    fn arrow_up() {
        let mut p = EscSeqParser::default();
        assert_eq!(p.parse(ascii::ESC), None);
        assert_eq!(p.parse(b'['), None);
        assert_eq!(p.parse(b'A'), Some(KeyEvent::Special(Key::ArrowUp, NO_MOD)));
    }

    #[test]
    fn arrow_down() {
        let mut p = EscSeqParser::default();
        assert_eq!(p.parse(ascii::ESC), None);
        assert_eq!(p.parse(b'['), None);
        assert_eq!(p.parse(b'B'), Some(KeyEvent::Special(Key::ArrowDown, NO_MOD)));
    }

    #[test]
    fn arrow_right() {
        let mut p = EscSeqParser::default();
        assert_eq!(p.parse(ascii::ESC), None);
        assert_eq!(p.parse(b'['), None);
        assert_eq!(p.parse(b'C'), Some(KeyEvent::Special(Key::ArrowRight, NO_MOD)));
    }

    #[test]
    fn arrow_left() {
        let mut p = EscSeqParser::default();
        assert_eq!(p.parse(ascii::ESC), None);
        assert_eq!(p.parse(b'['), None);
        assert_eq!(p.parse(b'D'), Some(KeyEvent::Special(Key::ArrowLeft, NO_MOD)));
    }

    #[test]
    fn home() {
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[H");
        assert_eq!(events, [None, None, Some(KeyEvent::Special(Key::Home, NO_MOD))]);
    }

    #[test]
    fn end() {
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[F");
        assert_eq!(events, [None, None, Some(KeyEvent::Special(Key::End, NO_MOD))]);
    }

    // ── VT220 tilde-final keys (family 2) ───────────────────────────

    #[test]
    fn insert() {
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[2~");
        assert_eq!(events, [None, None, None, Some(KeyEvent::Special(Key::Insert, NO_MOD))]);
    }

    #[test]
    fn delete() {
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[3~");
        assert_eq!(events, [None, None, None, Some(KeyEvent::Special(Key::Delete, NO_MOD))]);
    }

    #[test]
    fn page_up() {
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[5~");
        assert_eq!(events, [None, None, None, Some(KeyEvent::Special(Key::PageUp, NO_MOD))]);
    }

    #[test]
    fn page_down() {
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[6~");
        assert_eq!(events, [None, None, None, Some(KeyEvent::Special(Key::PageDown, NO_MOD))]);
    }

    // ── Modifiers ───────────────────────────────────────────────────

    #[test]
    fn ctrl_arrow_up() {
        // ESC [ 1 ; 5 A — Ctrl+Up
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[1;5A");
        let ctrl = Modifier { alt: false, ctrl: true, shift: false };
        assert_eq!(*events.last().unwrap(), Some(KeyEvent::Special(Key::ArrowUp, ctrl)));
    }

    #[test]
    fn shift_delete() {
        // ESC [ 3 ; 2 ~ — Shift+Delete
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[3;2~");
        let shift = Modifier { alt: false, ctrl: false, shift: true };
        assert_eq!(*events.last().unwrap(), Some(KeyEvent::Special(Key::Delete, shift)));
    }

    #[test]
    fn ctrl_shift_arrow_right() {
        // ESC [ 1 ; 6 C — Ctrl+Shift+Right
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[1;6C");
        let ctrl_shift = Modifier { alt: false, ctrl: true, shift: true };
        assert_eq!(*events.last().unwrap(), Some(KeyEvent::Special(Key::ArrowRight, ctrl_shift)));
    }

    #[test]
    fn alt_arrow_left() {
        // ESC [ 1 ; 3 D — Alt+Left
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[1;3D");
        let alt = Modifier { alt: true, ctrl: false, shift: false };
        assert_eq!(*events.last().unwrap(), Some(KeyEvent::Special(Key::ArrowLeft, alt)));
    }

    #[test]
    fn ctrl_alt_home() {
        // ESC [ 1 ; 7 H — Ctrl+Alt+Home
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[1;7H");
        let ctrl_alt = Modifier { alt: true, ctrl: true, shift: false };
        assert_eq!(*events.last().unwrap(), Some(KeyEvent::Special(Key::Home, ctrl_alt)));
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
        let mut p = EscSeqParser::default();
        assert_eq!(p.parse(ascii::ESC), None);
        // A non-bracket byte after ESC → bare Esc, with the byte pushed back
        assert_eq!(p.parse(b'x'), Some(KeyEvent::Special(Key::Esc, NO_MOD)));
        assert_eq!(p.pending_byte(), Some(b'x'));
        assert_eq!(p.pending_byte(), None); // drained
    }

    #[test]
    fn bare_esc_pushback_drains_to_char() {
        // The full caller loop: ESC then 'x' produces Esc + Char('x')
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1bx");
        assert_eq!(events, [
            None,                                               // ESC consumed
            Some(KeyEvent::Special(Key::Esc, NO_MOD)),          // 'x' triggers bare Esc
            Some(KeyEvent::Char(b'x')),                         // pushback 'x' parsed
        ]);
    }

    // ── Unknown sequences ───────────────────────────────────────────

    #[test]
    fn unknown_csi_final_drops_silently() {
        // ESC [ Z (Shift+Tab) — valid sequence, not a key we support
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[Z");
        assert_eq!(events, [None, None, None]);
        assert_eq!(p.pending_byte(), None); // no pushback
    }

    #[test]
    fn unknown_tilde_key_param_drops_silently() {
        // ESC [ 1 5 ~ (F5) — valid sequence, key param 15 not in our table
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[15~");
        assert_eq!(*events.last().unwrap(), None);
        assert_eq!(p.pending_byte(), None);
    }

    #[test]
    fn delete_does_not_leak_tilde() {
        // ESC [ 3 ~ then 'a' — the tilde must not appear as Char('~')
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[3~a");
        assert_eq!(events, [
            None, None, None,
            Some(KeyEvent::Special(Key::Delete, NO_MOD)),
            Some(KeyEvent::Char(b'a')),
        ]);
    }

    // ── Abort (non-sequence byte mid-CSI) ───────────────────────────

    #[test]
    fn control_byte_mid_csi_aborts_and_pushes_back() {
        // ESC [ then a control byte (0x01 = Ctrl+A) — not part of the CSI
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, &[ascii::ESC, b'[', 0x01]);
        // The abort drops ESC [, pushback produces Char(0x01)
        assert_eq!(events, [None, None, None, Some(KeyEvent::Char(0x01))]);
    }

    // ── Intermediate bytes swallowed ────────────────────────────────

    #[test]
    fn intermediate_bytes_swallowed() {
        // ESC [ $ A — the '$' (0x24, an intermediate byte) is consumed
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[$A");
        assert_eq!(*events.last().unwrap(), Some(KeyEvent::Special(Key::ArrowUp, NO_MOD)));
    }

    // ── Timeout ─────────────────────────────────────────────────────

    #[test]
    fn timeout_in_normal_returns_none() {
        let mut p = EscSeqParser::default();
        assert_eq!(p.esc_seq_timeout(), None);
    }

    #[test]
    fn timeout_in_got_escape_returns_esc() {
        let mut p = EscSeqParser::default();
        let _ = p.parse(ascii::ESC);
        assert_eq!(p.esc_seq_timeout(), Some(KeyEvent::Special(Key::Esc, NO_MOD)));
        // Back to Normal
        assert_eq!(p.parse(b'a'), Some(KeyEvent::Char(b'a')));
    }

    #[test]
    fn timeout_in_key_param_discards() {
        let mut p = EscSeqParser::default();
        let _ = p.parse(ascii::ESC);
        let _ = p.parse(b'[');
        assert_eq!(p.esc_seq_timeout(), None);
        // Back to Normal
        assert_eq!(p.parse(b'a'), Some(KeyEvent::Char(b'a')));
    }

    #[test]
    fn timeout_in_modifier_param_discards() {
        let mut p = EscSeqParser::default();
        let _ = p.parse(ascii::ESC);
        let _ = p.parse(b'[');
        let _ = p.parse(b'1');
        let _ = p.parse(b';');
        assert_eq!(p.esc_seq_timeout(), None);
        assert_eq!(p.parse(b'a'), Some(KeyEvent::Char(b'a')));
    }

    // ── State reset ─────────────────────────────────────────────────

    #[test]
    fn state_resets_after_complete_sequence() {
        let mut p = EscSeqParser::default();
        let _ = parse_all(&mut p, b"\x1b[A");
        assert_eq!(p.parse(b'x'), Some(KeyEvent::Char(b'x')));
    }

    #[test]
    fn state_resets_after_unknown_final() {
        let mut p = EscSeqParser::default();
        let _ = parse_all(&mut p, b"\x1b[Z");
        assert_eq!(p.parse(b'a'), Some(KeyEvent::Char(b'a')));
    }

    #[test]
    fn consecutive_arrows() {
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[A\x1b[B");
        assert_eq!(events, [
            None, None, Some(KeyEvent::Special(Key::ArrowUp, NO_MOD)),
            None, None, Some(KeyEvent::Special(Key::ArrowDown, NO_MOD)),
        ]);
    }

    // ── Digit overflow (saturating) ─────────────────────────────────

    #[test]
    fn long_digit_run_saturates_and_drops() {
        // ESC [ 9 9 9 ~ — saturates to 255, which is not in the tilde table
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"\x1b[999~");
        assert_eq!(*events.last().unwrap(), None);
    }

    // ── Mixed input ─────────────────────────────────────────────────

    #[test]
    fn mixed_typing_and_sequences() {
        let mut p = EscSeqParser::default();
        let events = parse_all(&mut p, b"hi\x0d\x1b[A!");
        assert_eq!(events, [
            Some(KeyEvent::Char(b'h')),
            Some(KeyEvent::Char(b'i')),
            Some(KeyEvent::Special(Key::Enter, NO_MOD)),
            None, None, Some(KeyEvent::Special(Key::ArrowUp, NO_MOD)),
            Some(KeyEvent::Char(b'!')),
        ]);
    }
}
