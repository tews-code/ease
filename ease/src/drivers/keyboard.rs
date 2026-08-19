//! Console keyboard driver
//!
//! Decodes duplicate key press and shift tracking.
//! See [read_byte] as the API

use super::virtio::input;
use super::virtio::input::evdev;
use crate::kernel::collection::{Bitmap, SpscRingBuf, bitmap_words_for};
use crate::kernel::sync::Completion;
use crate::syscall;

// Number of ASCII or special keys in queue (max burst size, more are dropped)
const KEY_QUEUE_LEN: usize = 64;
// Limits on codes for validation
const EVDEV_CODES_MAX: usize = 768;
const KEYMAP_CODES_MAX: usize = 128;
const CTRL_MODIFIER: u8 = 0x1F;
// Keyboard map for Mac UK keyboard
const KEY_MAP_BYTES_PER_CODE: usize = 4; // Four columns for key, shift-key and opt-key and both
const KEY_MAP: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/resources/apple-uk.keymap"
));
const _: () = assert!(KEY_MAP.len() == KEYMAP_CODES_MAX * KEY_MAP_BYTES_PER_CODE);

static KEY_QUEUE: SpscRingBuf<usize, KEY_QUEUE_LEN> = SpscRingBuf::new();
pub(crate) static KEY_PENDING: Completion = Completion::new();

/// Read a decoded key from virtio keyboard
///
/// Returns None if no key presses have been decoded
/// Special keys are encoded above 0xFF see [crate::syscall::special_key]
pub(crate) fn read_key() -> Option<usize> {
    KEY_QUEUE.pop()
}

fn ascii_code(key_code: u16, shift: bool, opt: bool, caps_lock: bool) -> u8 {
    assert!(usize::from(key_code) < KEYMAP_CODES_MAX);
    let offset = key_code as usize * KEY_MAP_BYTES_PER_CODE;
    // First check if the unmodified key is a letter
    let letter = KEY_MAP[offset].is_ascii_lowercase();
    let shift = if letter { shift != caps_lock } else { shift };
    KEY_MAP[offset + ((shift as usize) | ((opt as usize) << 1))]
}

fn ctrl_code(ascii_code: u8, ctrl: bool) -> u8 {
    if ctrl {
        ascii_code & CTRL_MODIFIER
    } else {
        ascii_code
    }
}

fn special_code(key_code: u16) -> Option<usize> {
    match key_code {
        input::key_code::UP_ARROW => Some(syscall::special_key::UP_ARROW),
        _ => None,
    }
}

enum EventKind {
    KeyPressed(u16),
    KeyReleased(u16),
    KeyHeld(u16),
    Syn,
    Unknown(input::Event),
}

#[derive(Debug, PartialEq, Eq)]
enum Emit {
    Char(usize), // We emit usize instead of u8 to fit into a0 for syscall; Special keys are also sent this way
    Quiet,
    UnknownKeyCode(u16),
    UnknownEvent(input::Event),
}

pub(crate) struct Decoder {
    pressed_keys: Bitmap<EVDEV_CODES_MAX, { bitmap_words_for(EVDEV_CODES_MAX) }>,
    caps_lock: bool,
}

impl Decoder {
    pub(crate) const fn new() -> Self {
        Self {
            pressed_keys: Bitmap::new(),
            caps_lock: false,
        }
    }

    fn held_shift(&self) -> bool {
        self.pressed_keys.get(input::key_code::LEFT_SHIFT as usize)
            || self.pressed_keys.get(input::key_code::RIGHT_SHIFT as usize)
    }

    fn held_opt(&self) -> bool {
        self.pressed_keys.get(input::key_code::LEFT_OPT as usize)
            || self.pressed_keys.get(input::key_code::RIGHT_OPT as usize)
    }

    fn held_ctrl(&self) -> bool {
        self.pressed_keys.get(input::key_code::CONTROL as usize)
    }

    fn decode_event(&mut self, event: &input::Event) -> Emit {
        let event = match (event.event_type, event.value) {
            (evdev::EV_SYN, _) => EventKind::Syn,
            (evdev::EV_KEY, evdev::VALUE_RELEASE) => EventKind::KeyReleased(event.code),
            (evdev::EV_KEY, evdev::VALUE_PRESS) => EventKind::KeyPressed(event.code),
            (evdev::EV_KEY, evdev::VALUE_HOLD) => EventKind::KeyHeld(event.code),
            _ => EventKind::Unknown(*event),
        };
        match event {
            EventKind::Syn => Emit::Quiet, // Filter syn by ignoring it
            EventKind::KeyPressed(key_code) if key_code == input::key_code::CAPS_LOCK => {
                let prev_caps_locked = self.pressed_keys.get(key_code as usize);
                self.pressed_keys.set(key_code as usize);
                if !prev_caps_locked {
                    self.caps_lock = !self.caps_lock;
                }
                Emit::Quiet
            }
            EventKind::KeyPressed(key_code) | EventKind::KeyHeld(key_code) => {
                match key_code {
                    input::key_code::LEFT_SHIFT
                    | input::key_code::RIGHT_SHIFT
                    | input::key_code::LEFT_OPT
                    | input::key_code::RIGHT_OPT
                    | input::key_code::CONTROL => {
                        self.pressed_keys.set(key_code as usize);
                        Emit::Quiet
                    }
                    key_code => {
                        // Held-key state covers the whole evdev range, so a key
                        // with no character still gets tracked.
                        if key_code as usize >= EVDEV_CODES_MAX {
                            return Emit::UnknownKeyCode(key_code);
                        }
                        // If this is a duplicated key, only emit if it is backspace / del
                        if self.pressed_keys.get(key_code as usize)
                            && key_code != input::key_code::BACKSPACE
                        {
                            return Emit::Quiet;
                        }
                        // Record this key
                        self.pressed_keys.set(key_code.into());
                        // The keymap only covers the low codes
                        if key_code as usize >= KEYMAP_CODES_MAX {
                            return Emit::UnknownKeyCode(key_code);
                        }
                        // See if this is an ASCII code
                        let ascii_code = ascii_code(
                            key_code,
                            self.held_shift(),
                            self.held_opt(),
                            self.caps_lock,
                        );
                        // Add ctrl modifier
                        let ascii_code = ctrl_code(ascii_code, self.held_ctrl());
                        if ascii_code != 0 {
                            return Emit::Char(ascii_code as usize);
                        }
                        // Not a character, try to match as a special key
                        if let Some(special_key) = special_code(key_code) {
                            Emit::Char(special_key)
                        } else {
                            Emit::UnknownKeyCode(key_code)
                        }
                    }
                }
            }
            EventKind::KeyReleased(key_code) => {
                match key_code {
                    input::key_code::LEFT_SHIFT
                    | input::key_code::RIGHT_SHIFT
                    | input::key_code::LEFT_OPT
                    | input::key_code::RIGHT_OPT
                    | input::key_code::CONTROL => {
                        self.pressed_keys.clear(key_code as usize);
                        Emit::Quiet
                    }
                    key_code => {
                        // Mirrors the press path: the bitmap bounds this, not
                        // the keymap, so unmapped keys release cleanly.
                        if key_code as usize >= EVDEV_CODES_MAX {
                            return Emit::UnknownKeyCode(key_code);
                        }
                        // Clear this key
                        self.pressed_keys.clear(key_code.into());
                        Emit::Quiet
                    }
                }
            }
            EventKind::Unknown(e) => Emit::UnknownEvent(e),
        }
    }
    /// Feeds the SPSC key queue
    ///
    /// Key codes are sent as their usize value
    /// Special codes are sent as values above 0xFF, see [crate::syscall::special_key]
    pub(crate) fn feed(&mut self, e: &input::Event) {
        match self.decode_event(e) {
            Emit::Quiet => {}
            Emit::Char(val) => {
                // Every decoded byte queues (incl. CR, DEL, CP437 highs).
                // If the queue is full the keystroke is dropped: blocking
                // here would back-pressure into the device ring instead.
                let _ = KEY_QUEUE.push(val);
                // Let the blocked thread know
                KEY_PENDING.signal();
            }
            Emit::UnknownKeyCode(k) => {
                println!("Unknown key code {}", k);
            }
            Emit::UnknownEvent(e) => {
                println!("Unknown event {:?}", e);
            }
        }
    }
}

#[cfg(all(test, feature = "test-virtio"))]
mod test {
    use super::*;
    use crate::drivers::virtio::input::Event;

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
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'a' as usize));
    }

    #[test_case]
    fn decoder_shift_uppercases_until_released() {
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(1, 42, 1)), Emit::Quiet); // left shift down
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'A' as usize));
        assert_eq!(d.decode_event(&ev(1, 30, 0)), Emit::Quiet); // release a
        assert_eq!(d.decode_event(&ev(1, 42, 0)), Emit::Quiet); // shift up
        // Regression: shift release must actually clear held_shift
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'a' as usize));
    }

    #[test_case]
    fn decoder_pound_calibration() {
        // Pins mkkeymap.py, the binary keymap, and the decoder to each
        // other: shift-3 on Apple UK is pound, CP437 byte 0x9C.
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(1, 42, 1)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 4, 1)), Emit::Char(0x9C));
    }

    #[test_case]
    fn decoder_letters_do_not_repeat() {
        // No auto-repeat policy: neither host-style duplicate presses
        // (Fedora/KDE) nor proper value-2 repeats emit for letters.
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'a' as usize));
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 30, 2)), Emit::Quiet);
    }

    #[test_case]
    fn decoder_backspace_repeats() {
        // The erasing key is allowlisted for repeat, via both repeat forms.
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(1, 14, 1)), Emit::Char(0x7F));
        assert_eq!(d.decode_event(&ev(1, 14, 1)), Emit::Char(0x7F));
        assert_eq!(d.decode_event(&ev(1, 14, 2)), Emit::Char(0x7F));
    }

    #[test_case]
    fn decoder_caps_lock_latches_across_release() {
        // Caps lock is a toggle: it stays active after the key is
        // released, and a second press-release cycle turns it off.
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(1, 58, 1)), Emit::Quiet); // caps down
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'A' as usize));
        assert_eq!(d.decode_event(&ev(1, 30, 0)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 58, 0)), Emit::Quiet); // caps up
        // Still latched after release
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'A' as usize));
        assert_eq!(d.decode_event(&ev(1, 30, 0)), Emit::Quiet);
        // Second cycle unlatches
        assert_eq!(d.decode_event(&ev(1, 58, 1)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 58, 0)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'a' as usize));
    }

    #[test_case]
    fn decoder_caps_lock_repeat_toggles_once() {
        // Host-style duplicate presses and value-2 repeats of the caps
        // key must not re-toggle while it is held down.
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(1, 58, 1)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 58, 1)), Emit::Quiet); // duplicate press
        assert_eq!(d.decode_event(&ev(1, 58, 2)), Emit::Quiet); // repeat
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'A' as usize));
    }

    #[test_case]
    fn decoder_caps_lock_only_affects_letters() {
        // Shift cancels caps for letters (XOR), while non-letter keys
        // ignore caps entirely: plain 3 stays 3, shift-3 stays pound.
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(1, 58, 1)), Emit::Quiet); // caps down
        assert_eq!(d.decode_event(&ev(1, 4, 1)), Emit::Char(b'3' as usize));
        assert_eq!(d.decode_event(&ev(1, 4, 0)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 42, 1)), Emit::Quiet); // shift down
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'a' as usize));
        assert_eq!(d.decode_event(&ev(1, 4, 1)), Emit::Char(0x9C));
    }

    #[test_case]
    fn decoder_ctrl_masks_to_control_byte() {
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(1, 29, 1)), Emit::Quiet); // ctrl down
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(0x01)); // ctrl-a
        assert_eq!(d.decode_event(&ev(1, 30, 0)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 29, 0)), Emit::Quiet); // ctrl up
        // Regression: ctrl release must actually clear the modifier
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'a' as usize));
    }

    #[test_case]
    fn decoder_opt_layer() {
        // Pins the third and fourth keymap columns: opt-a is å (CP437
        // 0x86), shift-opt-a is Å (0x8F), and either opt key counts.
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(1, 56, 1)), Emit::Quiet); // left opt down
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(0x86));
        assert_eq!(d.decode_event(&ev(1, 30, 0)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 42, 1)), Emit::Quiet); // shift down
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(0x8F));
        assert_eq!(d.decode_event(&ev(1, 30, 0)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 42, 0)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 56, 0)), Emit::Quiet); // left opt up
        assert_eq!(d.decode_event(&ev(1, 100, 1)), Emit::Quiet); // right opt down
        assert_eq!(d.decode_event(&ev(1, 50, 1)), Emit::Char(0xE6)); // opt-m is µ
    }

    #[test_case]
    fn decoder_unmapped_opt_combo_is_unknown() {
        // Option-2 (euro) is deliberately absent from the keymap: no
        // CP437 glyph. A zero entry must surface as unknown, not NUL.
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(1, 56, 1)), Emit::Quiet); // left opt down
        assert_eq!(d.decode_event(&ev(1, 3, 1)), Emit::UnknownKeyCode(3));
    }

    #[test_case]
    fn decoder_up_arrow_is_special_key() {
        // Arrows have no keymap byte; they emit an encoded special key
        // above 0xFF instead (see syscall::special_key).
        let mut d = Decoder::new();
        assert_eq!(
            d.decode_event(&ev(1, 103, 1)),
            Emit::Char(syscall::special_key::UP_ARROW)
        );
        assert_eq!(d.decode_event(&ev(1, 103, 0)), Emit::Quiet);
    }

    #[test_case]
    fn decoder_syn_and_release_are_quiet() {
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(0, 0, 0)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'a' as usize));
        assert_eq!(d.decode_event(&ev(1, 30, 0)), Emit::Quiet);
    }

    #[test_case]
    fn decoder_media_key_tracked_but_unknown() {
        // In bitmap range but beyond the keymap: state is tracked, no
        // byte emitted, release stays quiet.
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(1, 200, 1)), Emit::UnknownKeyCode(200));
        assert_eq!(d.decode_event(&ev(1, 200, 0)), Emit::Quiet);
    }

    #[test_case]
    fn decoder_survives_hostile_events() {
        // Events come from the device; nothing it sends may panic the
        // kernel. Oversized codes on press AND release paths, unknown
        // event types, unknown values.
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(1, 800, 1)), Emit::UnknownKeyCode(800));
        assert_eq!(d.decode_event(&ev(1, 800, 0)), Emit::UnknownKeyCode(800));
        assert_eq!(
            d.decode_event(&ev(4, 4, 5)),
            Emit::UnknownEvent(ev(4, 4, 5))
        );
        assert_eq!(
            d.decode_event(&ev(1, 30, 7)),
            Emit::UnknownEvent(ev(1, 30, 7))
        );
    }
}
