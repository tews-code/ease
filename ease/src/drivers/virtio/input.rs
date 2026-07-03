//! Virtio input device

use alloc::boxed::Box;
use core::mem;
use core::ptr::{read_volatile, write_volatile};

use crate::Order;
use crate::board::virtio::keyboard;
use crate::kernel::collection::StackVec;
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

static KEYBOARD: IrqSpinLock<Option<Keyboard>> = IrqSpinLock::new(None);

static EVENTS_PENDING: Completion = Completion::new();

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

fn keyboard_service() {
    let mut key_events = StackVec::<Event, { VIRTQ_ENTRY_NUM * 2 }>::new();
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

        // Print the key events
        for e in key_events.as_slice().iter() {
            println!("{:?}", e);
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
        .with_stack_class(Order::KB2)
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
