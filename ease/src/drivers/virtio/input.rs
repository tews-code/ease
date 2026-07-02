//! Virtio input device

use alloc::boxed::Box;

use crate::board::virtio::keyboard;
use crate::kernel::sync::IrqSpinLock;

use super::queue::{VIRTQ_ENTRY_NUM, VirtioVirtq, virtq_init};
use super::{check_virtio, reset_and_handshake, set_driver_ok};

//Input device id
const VIRTIO_DEVICE_ID: u32 = 18;
// Event queue
const EVENTQ: usize = 0;

static KEYBOARD: IrqSpinLock<Option<Keyboard>> = IrqSpinLock::new(None);

// Events use the Linux evdev format
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

#[expect(dead_code)]
struct Keyboard {
    eventq: Box<VirtioVirtq>,
    events: Box<[Event; VIRTQ_ENTRY_NUM]>,
}

impl Keyboard {
    pub(super) fn new() -> Self {
        check_virtio(keyboard::BASE, VIRTIO_DEVICE_ID);
        let eventq = Self::reset();
        Self {
            eventq,
            events: Box::new([const { Event::new() }; VIRTQ_ENTRY_NUM]),
        }
    }

    pub(super) fn reset() -> Box<VirtioVirtq> {
        reset_and_handshake(keyboard::BASE);
        let vq = virtq_init(keyboard::BASE, EVENTQ);
        set_driver_ok(keyboard::BASE);
        vq
    }
}

pub(crate) fn virtio_keyboard_init() {
    let mut keyboard = KEYBOARD.lock();
    assert!(
        keyboard.is_none(),
        "virtio keyboard initialised more than once"
    );
    *keyboard = Some(Keyboard::new());
}
