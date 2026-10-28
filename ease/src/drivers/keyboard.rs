//! Console keyboard driver
//!
//! Decodes duplicate key press and shift tracking.
//! Keys are bytes for characters or byte slice for escape sequences.
//! See [Reader::read_bytes] as the API

use super::virtio::input;
use super::virtio::input::evdev;
use crate::kernel::collection::{Bitmap, bitmap_words_for, spsc};
use crate::kernel::console_input;

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

/// Queue to pass decoded bytes from the virtio keyboard decoder thread to the reader.
/// We use statics as the keyboard is permanently attached to the hardware
pub(crate) mod queue {
    use crate::kernel::collection::spsc;
    use crate::kernel::sync::TryLock;
    /// Number of bytes in queue (max burst size, more are dropped)
    pub(super) const LEN: usize = 64;
    /// SPSC queue to Send the bytes from the keyboard
    pub(super) static KEYBOARD: spsc::Queue<u8, LEN> = spsc::Queue::new();
    /// Holds the static key queue producer
    pub(super) static PRODUCER: TryLock<spsc::Producer<'static, u8, LEN>> =
        // Safety: The producer is only created once right here
        TryLock::new(unsafe { KEYBOARD.producer_unchecked() });
}

/// Reader that holds the consumer for the KEYBOARD queue
pub(crate) struct Reader {
    consumer: spsc::Consumer<'static, u8, { queue::LEN }>,
}

impl Reader {
    /// Create a new [Reader] which holds the consumer side of the SPSC queue
    ///
    /// # Safety
    /// The caller must ensure that there is only one Reader as there must be only
    /// one consumer
    pub(crate) const unsafe fn new() -> Self {
        Self {
            // Safety: This is the only place the KEYBOARD queue consumer is created
            consumer: unsafe { queue::KEYBOARD.consumer_unchecked() },
        }
    }

    /// Pop bytes from the KEYBOARD queue into the given buffer until the buffer
    /// is full or the queue is empty.
    ///
    /// Returns the number of bytes read into the buffer.
    /// An empty buffer always returns immediately with zero bytes read.
    pub(crate) fn read_bytes(&mut self, buf: &mut [u8]) -> usize {
        let mut bytes_read: usize = 0;
        while bytes_read < buf.len()
            && let Some(byte) = self.consumer.pop()
        {
            buf[bytes_read] = byte;
            bytes_read += 1;
        }
        bytes_read
    }
}

/// Test support: queue a decoded key exactly as the decoder would, so a
/// thread blocked in READ wakes without a virtio keyboard event.
///
/// This is a second user of the PRODUCER (but is safe due to TryLock as nothing sends keyboard events during testing)
#[cfg(test)]
pub(crate) fn inject_key(bytes: &[u8]) {
    // Check if there is enough space in the queue before loading the sequence
    // If not, simply drop the entire key
    let mut producer = queue::PRODUCER
        .try_lock()
        .expect("should be the only user of the PRODUCER lock");
    if queue::KEYBOARD.free() >= bytes.len() {
        for byte in bytes.iter() {
            let _ = producer.push(*byte); // We know there is enough space
        }
        console_input::notify();
    }
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

fn escape_sequence(key_code: u16) -> Option<&'static [u8]> {
    match key_code {
        input::key_code::UP_ARROW => Some(b"\x1b[A"),
        input::key_code::DOWN_ARROW => Some(b"\x1b[B"),
        input::key_code::RIGHT_ARROW => Some(b"\x1b[C"),
        input::key_code::LEFT_ARROW => Some(b"\x1b[D"),
        input::key_code::HOME => Some(b"\x1b[H"),
        input::key_code::END => Some(b"\x1b[F"),
        input::key_code::INSERT => Some(b"\x1b[2~"),
        input::key_code::DELETE => Some(b"\x1b[3~"),
        input::key_code::PAGE_UP => Some(b"\x1b[5~"),
        input::key_code::PAGE_DOWN => Some(b"\x1b[6~"),
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
    Char(u8),                      // For byte characters
    EscapeSequence(&'static [u8]), // Escape sequence as a slice
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
                            return Emit::Char(ascii_code);
                        }
                        // Not a character, try to match as a special key
                        if let Some(sequence) = escape_sequence(key_code) {
                            return Emit::EscapeSequence(sequence);
                        }
                        // Unknown key
                        Emit::UnknownKeyCode(key_code)
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
    /// Characters are pushed as a byte. Escape sequences are pushed as a byte array if
    /// there is enough space on the queue, otherwise dropped.
    pub(crate) fn feed(&mut self, e: &input::Event) {
        match self.decode_event(e) {
            Emit::Quiet => {}
            Emit::Char(val) => {
                // If the queue is full the keystroke is dropped: blocking
                // here would back-pressure into the device ring instead.
                let _ = queue::PRODUCER
                    .try_lock()
                    .expect("should be the only user of the PRODUCER lock")
                    .push(val);
                // Let the blocked thread know
                console_input::notify();
            }
            Emit::EscapeSequence(sequence) => {
                // Check if there is enough space in the queue before loading the sequence
                // If not, simply drop the entire key
                let mut producer = queue::PRODUCER
                    .try_lock()
                    .expect("should be the only user of the PRODUCER lock");
                if queue::KEYBOARD.free() >= sequence.len() {
                    for byte in sequence.iter() {
                        let _ = producer.push(*byte); // We know there is enough space
                    }
                    console_input::notify();
                }
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
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'a'));
    }

    #[test_case]
    fn decoder_shift_uppercases_until_released() {
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(1, 42, 1)), Emit::Quiet); // left shift down
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'A'));
        assert_eq!(d.decode_event(&ev(1, 30, 0)), Emit::Quiet); // release a
        assert_eq!(d.decode_event(&ev(1, 42, 0)), Emit::Quiet); // shift up
        // Regression: shift release must actually clear held_shift
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'a'));
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
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'a'));
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
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'A'));
        assert_eq!(d.decode_event(&ev(1, 30, 0)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 58, 0)), Emit::Quiet); // caps up
        // Still latched after release
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'A'));
        assert_eq!(d.decode_event(&ev(1, 30, 0)), Emit::Quiet);
        // Second cycle unlatches
        assert_eq!(d.decode_event(&ev(1, 58, 1)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 58, 0)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'a'));
    }

    #[test_case]
    fn decoder_caps_lock_repeat_toggles_once() {
        // Host-style duplicate presses and value-2 repeats of the caps
        // key must not re-toggle while it is held down.
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(1, 58, 1)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 58, 1)), Emit::Quiet); // duplicate press
        assert_eq!(d.decode_event(&ev(1, 58, 2)), Emit::Quiet); // repeat
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'A'));
    }

    #[test_case]
    fn decoder_caps_lock_only_affects_letters() {
        // Shift cancels caps for letters (XOR), while non-letter keys
        // ignore caps entirely: plain 3 stays 3, shift-3 stays pound.
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(1, 58, 1)), Emit::Quiet); // caps down
        assert_eq!(d.decode_event(&ev(1, 4, 1)), Emit::Char(b'3'));
        assert_eq!(d.decode_event(&ev(1, 4, 0)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 42, 1)), Emit::Quiet); // shift down
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'a'));
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
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'a'));
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
    fn decoder_up_arrow_is_escape_sequence() {
        // Arrows have no keymap byte; they emit the xterm escape sequence
        // a terminal would send over the UART, and release stays quiet.
        let mut d = Decoder::new();
        assert_eq!(
            d.decode_event(&ev(1, 103, 1)),
            Emit::EscapeSequence(b"\x1b[A")
        );
        assert_eq!(d.decode_event(&ev(1, 103, 0)), Emit::Quiet);
    }

    #[test_case]
    fn decoder_navigation_keys_match_xterm() {
        // Pins every evdev code in the table to its xterm sequence. The
        // codes are the raw Linux numbers, not the key_code constants, so
        // a constant with the wrong value (evdev doesn't number the
        // arrows in order) fails here.
        let table: [(u16, &[u8]); 10] = [
            (102, b"\x1b[H"),  // Home
            (103, b"\x1b[A"),  // Up
            (104, b"\x1b[5~"), // Page Up
            (105, b"\x1b[D"),  // Left
            (106, b"\x1b[C"),  // Right
            (107, b"\x1b[F"),  // End
            (108, b"\x1b[B"),  // Down
            (109, b"\x1b[6~"), // Page Down
            (110, b"\x1b[2~"), // Insert
            (111, b"\x1b[3~"), // Delete
        ];
        let mut d = Decoder::new();
        for (code, sequence) in table {
            assert_eq!(
                d.decode_event(&ev(1, code, 1)),
                Emit::EscapeSequence(sequence)
            );
            assert_eq!(d.decode_event(&ev(1, code, 0)), Emit::Quiet);
        }
    }

    #[test_case]
    fn feed_drops_a_sequence_that_does_not_fit_whole() {
        // A sequence must reach the queue whole or not at all: a partial
        // ESC [ would make the reader's parser swallow the next key.
        // Fill the queue to 2 free bytes, feed Up (3 bytes), and check
        // nothing went in; then a 1-byte key still fits.
        let mut d = Decoder::new();
        let free = queue::KEYBOARD.free();
        assert!(
            free >= 2,
            "keyboard queue should be near empty at test start"
        );
        inject_key(&[b'a'; queue::LEN][..free - 2]);
        assert_eq!(queue::KEYBOARD.free(), 2);
        d.feed(&ev(1, 103, 1));
        assert_eq!(
            queue::KEYBOARD.free(),
            2,
            "a partial escape sequence was queued"
        );
        d.feed(&ev(1, 103, 0));
        d.feed(&ev(1, 30, 1)); // 'a' fits in the space Up couldn't use
        assert_eq!(queue::KEYBOARD.free(), 1);
        d.feed(&ev(1, 30, 0));
        // Drain through console input so later tests start empty
        let mut buf = [0u8; queue::LEN];
        while queue::KEYBOARD.free() < queue::LEN {
            crate::kernel::console_input::wait_and_read_interruptible(&mut buf)
                .expect("a kernel test thread is never interrupted");
        }
    }

    #[test_case]
    fn decoder_syn_and_release_are_quiet() {
        let mut d = Decoder::new();
        assert_eq!(d.decode_event(&ev(0, 0, 0)), Emit::Quiet);
        assert_eq!(d.decode_event(&ev(1, 30, 1)), Emit::Char(b'a'));
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
