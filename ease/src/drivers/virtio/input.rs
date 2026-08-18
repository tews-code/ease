//! Virtio input device receiving Linux-style evdev events
//!
//! EASE uses the virtio input device for input as this is more similar
//! to the RP2350 USB interface than UART.
//!
//! All events are pushed into a caller-provided StackVec in
//! [Device::drain_events].
//!
//! For virtio keyboard driver see [super::keyboard]
//! For console keyboard driver see [super::super::keyboard]

use super::queue::{
    VIRTQ_DESC_F_WRITE, VIRTQ_ENTRY_NUM, VirtioVirtq, VirtqDesc, virtq_init, virtq_notify,
    virtq_publish,
};
use super::{check_virtio, reset_and_handshake, set_driver_ok};
use crate::kernel::collection::StackVec;
use alloc::boxed::Box;
use core::mem;
use core::ptr::{read_volatile, write_volatile};

// Virtio input events use the Linux evdev format
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub(crate) struct Event {
    pub(crate) event_type: u16,
    pub(crate) code: u16,
    pub(crate) value: u32,
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

// The virtio device DMAs exactly one 8-byte evdev event
const _: () = assert!(mem::size_of::<Event>() == 8);

// The input device
pub(crate) struct Device {
    device_base_address: usize,
    eventq: Box<VirtioVirtq>,
    events: Box<[Event; VIRTQ_ENTRY_NUM]>,
}

impl Device {
    // Input device id is given as 18
    const VIRTIO_DEVICE_ID: u32 = 18;
    // Event queue index
    const EVENTQ_IDX: usize = 0;
    #[expect(dead_code)]
    const STATUSQ_IDX: usize = 1; // For setting the caps-lock LED etc

    /// New input device set up with virtio queues in place
    pub(crate) fn new(device_base_address: usize) -> Self {
        check_virtio(device_base_address, Self::VIRTIO_DEVICE_ID);
        let mut events = Box::new([const { Event::new() }; VIRTQ_ENTRY_NUM]);
        reset_and_handshake(device_base_address);
        let mut eventq = virtq_init(device_base_address, Self::EVENTQ_IDX);
        Self::post_event_buffers(&mut events, &mut eventq);
        set_driver_ok(device_base_address);
        virtq_notify(device_base_address, &eventq);
        Self {
            device_base_address,
            eventq,
            events,
        }
    }

    /// Drain all events from the event queue
    ///
    /// Pushes the events into the caller-provided StackVec
    pub(crate) fn drain_events<const N: usize>(&mut self, out: &mut StackVec<Event, N>) {
        loop {
            // Check if we have space in the StackVec to store this
            if out.is_full() {
                break;
            }
            let Some(e) = self.eventq.pop_used() else {
                break;
            };
            assert!(
                (e.id as usize) < VIRTQ_ENTRY_NUM,
                "id is larger than virtio ring buffer"
            );
            // Read the key event from the events buffer
            let key_event = unsafe { read_volatile(&raw const self.events[e.id as usize]) };
            out.push(key_event)
                .expect("there should be space in the Vec");
            virtq_publish(&mut self.eventq, e.id as u16);
        }
        virtq_notify(self.device_base_address, &self.eventq);
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
