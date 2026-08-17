//! Virtio input device

use alloc::boxed::Box;
use core::mem;
use core::ptr::{read_volatile, write_volatile};

use crate::Order;
use crate::board::virtio::keyboard;
use crate::kernel::collection::{Bitmap, SpscRingBuf, StackVec, bitmap_words_for};
use crate::kernel::sched;
use crate::kernel::sync::{Completion, IrqSpinLock};

use super::queue::{
    VIRTQ_DESC_F_WRITE, VIRTQ_ENTRY_NUM, VirtioVirtq, VirtqDesc, virtq_init, virtq_notify,
    virtq_publish,
};
use super::{ack_interrupt, check_virtio, reset_and_handshake, set_driver_ok};

//Input device id
const VIRTIO_DEVICE_ID: u32 = 18;
// Event queue
const EVENTQ: usize = 0;
// Number of ascii keys in queue (max burst size, more are dropped)
const ASCII_QUEUE_LEN: usize = 64;

static KEYBOARD: IrqSpinLock<Option<Keyboard>> = IrqSpinLock::new(None);
static EVENTS_PENDING: Completion = Completion::new();
static ASCII_KEY_QUEUE: SpscRingBuf<u8, ASCII_QUEUE_LEN> = SpscRingBuf::new();
pub(crate) static ASCII_KEY_PENDING: Completion = Completion::new();

// Events use the Linux evdev format
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct Event {
    event_type: u16,
    code: u16,
    value: u32,
}

impl Event {
    const fn new() -> Self {
        Self {
            event_type: 0,
            code: 0,
            value: 0,
        }
    }
}

// The device DMAs exactly one 8-byte evdev event into each buffer; the
// descriptors are sized from this struct, so any drift in its layout makes
// every event decode as garbage with no error anywhere.
const _: () = assert!(mem::size_of::<Event>() == 8);

pub(crate) struct Keyboard {
    eventq: Box<VirtioVirtq>,
    events: Box<[Event; VIRTQ_ENTRY_NUM]>,
}

impl Keyboard {
    pub(super) fn new() -> Self {
        check_virtio(keyboard::BASE, VIRTIO_DEVICE_ID);
        let mut events = Box::new([const { Event::new() }; VIRTQ_ENTRY_NUM]);
        reset_and_handshake(keyboard::BASE);
        let mut eventq = virtq_init(keyboard::BASE, EVENTQ);
        Self::post_event_buffers(&mut events, &mut eventq);
        set_driver_ok(keyboard::BASE);
        virtq_notify(keyboard::BASE, &eventq);
        Self { eventq, events }
    }

    // Set up descriptors
    fn post_event_buffers(events: &mut [Event; VIRTQ_ENTRY_NUM], vq: &mut VirtioVirtq) {
        for (i, event) in events.iter().enumerate() {
            let addr = (&raw const *event).addr();

            // Descriptor: event header
            unsafe {
                write_volatile(
                    &raw mut vq.descs[i],
                    VirtqDesc {
                        addr: addr as u64,
                        len: mem::size_of::<Event>() as u32,
                        flags: VIRTQ_DESC_F_WRITE as u16,
                        next: 0, // Single event - no chain
                    },
                )
            };

            virtq_publish(vq, i as u16);
        }
    }
}

// Decodes duplicate keys and shift tracking
const KEY_CODES_MAX: usize = 768;
const ASCII_CODES_MAX: usize = 128;
const KEY_MAP: &[u8] = include_bytes!("../../../resources/apple-uk.keymap");
const BYTES_PER_CODE: usize = 2;

// Critical key codes
const LEFT_SHIFT: u16 = 42;
const RIGHT_SHIFT: u16 = 54;
const BACKSPACE: u16 = 14;

fn ascii_code(key_code: u16, shift: bool) -> u8 {
    assert!(usize::from(key_code) < ASCII_CODES_MAX);
    let offset = key_code as usize * BYTES_PER_CODE;
    KEY_MAP[offset + shift as usize]
}

enum EventType {
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

struct Decoder {
    pressed_keys: Bitmap<KEY_CODES_MAX, { bitmap_words_for(KEY_CODES_MAX) }>,
    held_shift: bool,
}

impl Decoder {
    const fn new() -> Self {
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
            (0, 0) => EventType::Syn,
            (1, 0) => EventType::KeyReleased(event.code),
            (1, 1) => EventType::KeyPressed(event.code),
            (1, 2) => EventType::KeyHeld(event.code),
            _ => EventType::Unknown(*event),
        };
        match key_press {
            EventType::Syn => Emit::Quiet, // Filter syn by ignoring it
            EventType::KeyPressed(k) | EventType::KeyHeld(k) => {
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
            EventType::KeyReleased(k) => {
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
            EventType::Unknown(e) => Emit::Unknown(e.code),
        }
    }
}

/// Decodes keystrokes and turns them into ASCII characters on the ASCII_KEY_QUEUE
fn keyboard_service() {
    let mut key_events = StackVec::<Event, { VIRTQ_ENTRY_NUM * 2 }>::new();
    let mut decoder = Decoder::new();
    loop {
        EVENTS_PENDING.wait();
        with_keyboard(|keyboard| {
            loop {
                // Check if we have space in the vec to store this
                if key_events.is_full() {
                    break;
                }
                let Some(e) = keyboard.eventq.pop_used() else {
                    break;
                };
                assert!(
                    (e.id as usize) < VIRTQ_ENTRY_NUM,
                    "id is larger than virtio ring buffer"
                );

                // Read the key event from the events buffer
                let key_event = unsafe { read_volatile(&raw const keyboard.events[e.id as usize]) };
                key_events
                    .push(key_event)
                    .expect("there should be space in the vec");
                virtq_publish(&mut keyboard.eventq, e.id as u16);
            }
            virtq_notify(keyboard::BASE, &keyboard.eventq);
        });

        // Decode events and deliver bytes to the shell's input queue
        for e in key_events.as_slice().iter() {
            match decoder.decode(e) {
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
        key_events.clear();
    }
}

pub(crate) fn virtio_keyboard_init() {
    let mut keyboard = KEYBOARD.lock();
    assert!(
        keyboard.is_none(),
        "virtio keyboard initialised more than once"
    );
    *keyboard = Some(Keyboard::new());
    drop(keyboard);
    // Create a service thread that blocks on keyboard completion
    sched::Builder::new()
        .with_stack_class(Order::KB4)
        .spawn(keyboard_service)
        .expect("could not launch key event thread");
}

// Handle interrupt from trap
pub(crate) fn handle_virtio_interrupt() {
    ack_interrupt(keyboard::BASE);
    EVENTS_PENDING.signal();
}

// Perform activity with keyboard lock
pub(crate) fn with_keyboard<R>(f: impl FnOnce(&mut Keyboard) -> R) -> R {
    let mut guard = KEYBOARD.lock();
    let keyboard = guard.as_mut().expect("keyboard should be initialised");
    f(keyboard)
}

// Read byte from virtio input keyboard
pub(crate) fn read_byte() -> Option<u8> {
    ASCII_KEY_QUEUE.pop()
}

#[cfg(all(test, feature = "test-virtio"))]
mod test {
    use super::super::{VIRTIO_REG_DEVICE_STATUS, VIRTIO_STATUS_DRIVER_OK};
    use super::*;
    use crate::arch::mmio;

    #[test_case]
    fn keyboard_device_status_after_init() {
        // virtio_keyboard_init() already ran in main(); verify DRIVER_OK is set
        let status = mmio::read32(keyboard::BASE, VIRTIO_REG_DEVICE_STATUS);
        assert_eq!(status & VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_DRIVER_OK);
    }

    #[test_case]
    fn keyboard_eventq_starts_dry() {
        // Headless CI never types, so no event buffer should ever complete:
        // a used entry here means the device rejected or misread a posted
        // buffer during init.
        with_keyboard(|kb| {
            assert!(kb.eventq.pop_used().is_none());
        });
    }

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
