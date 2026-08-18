//! Console keyboard driver
//!
//! Decodes duplicate key press and shift tracking.
//! See [read_byte] as the API

use super::virtio::input::Event;
use crate::kernel::collection::{Bitmap, SpscRingBuf, bitmap_words_for};
use crate::kernel::sync::Completion;

static ASCII_KEY_QUEUE: SpscRingBuf<u8, ASCII_QUEUE_LEN> = SpscRingBuf::new();

pub(crate) static ASCII_KEY_PENDING: Completion = Completion::new();

// Number of ASCII keys in queue (max burst size, more are dropped)
const ASCII_QUEUE_LEN: usize = 64;
const KEY_CODES_MAX: usize = 768;
const ASCII_CODES_MAX: usize = 128;
const KEY_MAP: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/resources/apple-uk.keymap"
));
const BYTES_PER_CODE: usize = 2;

// Critical key codes
const LEFT_SHIFT: u16 = 42;
const RIGHT_SHIFT: u16 = 54;
const BACKSPACE: u16 = 14;

/// Read a decoded ASCII byte from virtio keyboard
///
/// Returns None if no key presses have been decoded
pub(crate) fn read_byte() -> Option<u8> {
    ASCII_KEY_QUEUE.pop()
}

fn ascii_code(key_code: u16, shift: bool) -> u8 {
    assert!(usize::from(key_code) < ASCII_CODES_MAX);
    let offset = key_code as usize * BYTES_PER_CODE;
    KEY_MAP[offset + shift as usize]
}

enum EventKind {
    KeyPressed(u16),
    KeyReleased(u16),
    KeyHeld(u16),
    Syn,
    Unknown(Event),
}

#[derive(Debug, PartialEq, Eq)]
enum Emit {
    Char(u8),
    Quiet,
    Unknown(u16),
}

pub(crate) struct Decoder {
    pressed_keys: Bitmap<KEY_CODES_MAX, { bitmap_words_for(KEY_CODES_MAX) }>,
    held_shift: bool,
}

impl Decoder {
    pub(crate) const fn new() -> Self {
        Self {
            pressed_keys: Bitmap::new(),
            held_shift: false,
        }
    }

    #[expect(dead_code)]
    fn get_pressed_keys(&self) -> &Bitmap<KEY_CODES_MAX, { bitmap_words_for(KEY_CODES_MAX) }> {
        &self.pressed_keys
    }

    fn emit(&self, k: u16, shift: bool) -> Emit {
        if usize::from(k) >= ASCII_CODES_MAX {
            return Emit::Unknown(k);
        }
        let ascii = ascii_code(k, shift);
        if ascii == 0 {
            Emit::Unknown(k)
        } else {
            Emit::Char(ascii)
        }
    }

    fn decode(&mut self, event: &Event) -> Emit {
        let key_press = match (event.event_type, event.value) {
            (0, 0) => EventKind::Syn,
            (1, 0) => EventKind::KeyReleased(event.code),
            (1, 1) => EventKind::KeyPressed(event.code),
            (1, 2) => EventKind::KeyHeld(event.code),
            _ => EventKind::Unknown(*event),
        };
        match key_press {
            EventKind::Syn => Emit::Quiet, // Filter syn by ignoring it
            EventKind::KeyPressed(k) | EventKind::KeyHeld(k) => {
                // We do not handle keycodes above KEY_CODES_MAX
                if usize::from(k) >= KEY_CODES_MAX {
                    return Emit::Unknown(k);
                }
                if k == LEFT_SHIFT || k == RIGHT_SHIFT {
                    self.held_shift = true;
                    Emit::Quiet
                } else {
                    // Check if this is a repeated key
                    if self.pressed_keys.get(k.into()) {
                        // Only repeat if backspace or delete
                        if k == BACKSPACE {
                            Emit::Char(ascii_code(k, self.held_shift))
                        } else {
                            Emit::Quiet
                        }
                    } else {
                        // Set the pressed keys record
                        self.pressed_keys.set(k.into());
                        // Return the ascii code
                        self.emit(k, self.held_shift)
                    }
                }
            }
            EventKind::KeyReleased(k) => {
                if usize::from(k) >= KEY_CODES_MAX {
                    return Emit::Unknown(k);
                }
                if k == LEFT_SHIFT || k == RIGHT_SHIFT {
                    self.held_shift = false;
                    Emit::Quiet
                } else {
                    self.pressed_keys.clear(k as usize);
                    Emit::Quiet
                }
            }
            EventKind::Unknown(e) => Emit::Unknown(e.code),
        }
    }
    // Feeds the SPSC ASCII queue with valid ASCII bytes
    pub(crate) fn feed(&mut self, e: &Event) {
        match self.decode(e) {
            Emit::Quiet => {}
            Emit::Char(ch) => {
                // Every decoded byte queues (incl. CR, DEL, CP437 highs).
                // If the queue is full the keystroke is dropped: blocking
                // here would back-pressure into the device ring instead.
                let _ = ASCII_KEY_QUEUE.push(ch);
                // Let the blocked thread know
                ASCII_KEY_PENDING.signal();
            }
            Emit::Unknown(k) => {
                println!("Unknown key code {}", k);
            }
        }
    }
}

#[cfg(all(test, feature = "test-virtio"))]
mod test {
    use super::*;

    // ---- Decoder tests: fabricated events, no device involved ----------
    // Event field meanings: type (0=EV_SYN, 1=EV_KEY), code (which key),
    // value (0=release, 1=press, 2=repeat).

    fn ev(event_type: u16, code: u16, value: u32) -> Event {
        Event {
            event_type,
            code,
            value,
        }
    }

    #[test_case]
    fn decoder_plain_letter() {
        let mut d = Decoder::new();
        assert_eq!(d.decode(&ev(1, 30, 1)), Emit::Char(b'a'));
    }

    #[test_case]
    fn decoder_shift_uppercases_until_released() {
        let mut d = Decoder::new();
        assert_eq!(d.decode(&ev(1, 42, 1)), Emit::Quiet); // left shift down
        assert_eq!(d.decode(&ev(1, 30, 1)), Emit::Char(b'A'));
        assert_eq!(d.decode(&ev(1, 30, 0)), Emit::Quiet); // release a
        assert_eq!(d.decode(&ev(1, 42, 0)), Emit::Quiet); // shift up
        // Regression: shift release must actually clear held_shift
        assert_eq!(d.decode(&ev(1, 30, 1)), Emit::Char(b'a'));
    }

    #[test_case]
    fn decoder_pound_calibration() {
        // Pins mkkeymap.py, the binary keymap, and the decoder to each
        // other: shift-3 on Apple UK is pound, CP437 byte 0x9C.
        let mut d = Decoder::new();
        assert_eq!(d.decode(&ev(1, 42, 1)), Emit::Quiet);
        assert_eq!(d.decode(&ev(1, 4, 1)), Emit::Char(0x9C));
    }

    #[test_case]
    fn decoder_letters_do_not_repeat() {
        // No auto-repeat policy: neither host-style duplicate presses
        // (Fedora/KDE) nor proper value-2 repeats emit for letters.
        let mut d = Decoder::new();
        assert_eq!(d.decode(&ev(1, 30, 1)), Emit::Char(b'a'));
        assert_eq!(d.decode(&ev(1, 30, 1)), Emit::Quiet);
        assert_eq!(d.decode(&ev(1, 30, 2)), Emit::Quiet);
    }

    #[test_case]
    fn decoder_backspace_repeats() {
        // The erasing key is allowlisted for repeat, via both repeat forms.
        let mut d = Decoder::new();
        assert_eq!(d.decode(&ev(1, 14, 1)), Emit::Char(0x7F));
        assert_eq!(d.decode(&ev(1, 14, 1)), Emit::Char(0x7F));
        assert_eq!(d.decode(&ev(1, 14, 2)), Emit::Char(0x7F));
    }

    #[test_case]
    fn decoder_unmapped_modifier_is_unknown() {
        // Ctrl has a zero keymap entry: surfaced as Unknown during
        // calibration rather than emitting a NUL byte.
        let mut d = Decoder::new();
        assert_eq!(d.decode(&ev(1, 29, 1)), Emit::Unknown(29));
    }

    #[test_case]
    fn decoder_syn_and_release_are_quiet() {
        let mut d = Decoder::new();
        assert_eq!(d.decode(&ev(0, 0, 0)), Emit::Quiet);
        assert_eq!(d.decode(&ev(1, 30, 1)), Emit::Char(b'a'));
        assert_eq!(d.decode(&ev(1, 30, 0)), Emit::Quiet);
    }

    #[test_case]
    fn decoder_media_key_tracked_but_unknown() {
        // In bitmap range but beyond the keymap: state is tracked, no
        // byte emitted, release stays quiet.
        let mut d = Decoder::new();
        assert_eq!(d.decode(&ev(1, 200, 1)), Emit::Unknown(200));
        assert_eq!(d.decode(&ev(1, 200, 0)), Emit::Quiet);
    }

    #[test_case]
    fn decoder_survives_hostile_events() {
        // Events come from the device; nothing it sends may panic the
        // kernel. Oversized codes on press AND release paths, unknown
        // event types, unknown values.
        let mut d = Decoder::new();
        assert_eq!(d.decode(&ev(1, 800, 1)), Emit::Unknown(800));
        assert_eq!(d.decode(&ev(1, 800, 0)), Emit::Unknown(800));
        assert_eq!(d.decode(&ev(4, 4, 5)), Emit::Unknown(4));
        assert_eq!(d.decode(&ev(1, 30, 7)), Emit::Unknown(30));
    }
}
