//! Virtio input device

use alloc::boxed::Box;
use core::mem;
use core::ptr::write_volatile;

use crate::board::virtio::keyboard;
use crate::kernel::sync::IrqSpinLock;

use super::queue::{
    VIRTQ_DESC_F_WRITE, VIRTQ_ENTRY_NUM, VirtioVirtq, VirtqDesc, virtq_init, virtq_notify,
    virtq_publish,
};
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

pub(crate) fn virtio_keyboard_init() {
    let mut keyboard = KEYBOARD.lock();
    assert!(
        keyboard.is_none(),
        "virtio keyboard initialised more than once"
    );
    *keyboard = Some(Keyboard::new());
}
