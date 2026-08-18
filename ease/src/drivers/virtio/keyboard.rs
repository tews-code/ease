//! Virtio keyboard driver
//!
//! Provides a virtio keyboard device with a worker thread that
//! decodes the events into ASCII values on a queue.
//! The keyboard is protected by a spinlock in case of multiple threads
//! simultaneously using the keyboard for input.
//!
//! Event interrupts are acknowledged and raised as a Completion
//!
//! See [crate::drivers::keyboard] for console keyboard driver

use super::{ack_interrupt, input, queue};
use crate::Order;
use crate::board;
use crate::drivers::keyboard::Decoder;
use crate::kernel::collection::StackVec;
use crate::kernel::sched;
use crate::kernel::sync::{Completion, IrqSpinLock};

static KEYBOARD: IrqSpinLock<Option<input::Device>> = IrqSpinLock::new(None);

static EVENTS_PENDING: Completion = Completion::new();

// Handle interrupt from trap
pub(crate) fn handle_interrupt() {
    ack_interrupt(board::virtio::keyboard::BASE);
    EVENTS_PENDING.signal();
}

pub(crate) fn init() {
    let mut keyboard = KEYBOARD.lock();
    assert!(
        keyboard.is_none(),
        "virtio keyboard initialised more than once"
    );
    *keyboard = Some(input::Device::new(board::virtio::keyboard::BASE));
    drop(keyboard);
    // Create a service thread that blocks on keyboard completion
    sched::Builder::new()
        .with_stack_class(Order::KB4)
        .spawn(keystroke_decode)
        .expect("could not launch key event thread");
}

/// Decodes keystrokes and turns them into ASCII characters on the ASCII_KEY_QUEUE
fn keystroke_decode() {
    let mut key_events = StackVec::<input::Event, { queue::VIRTQ_ENTRY_NUM * 2 }>::new(); // x 2 to cover both ev event and ev_syn after each event
    let mut decoder = Decoder::new();
    loop {
        EVENTS_PENDING.wait();
        with_keyboard(|kb| kb.drain_events(&mut key_events));

        // Decode events and deliver bytes to the shell's input queue
        for e in key_events.as_slice().iter() {
            decoder.feed(e)
        }
        key_events.clear();
    }
}

// Perform activity with keyboard lock
fn with_keyboard<R>(f: impl FnOnce(&mut input::Device) -> R) -> R {
    let mut guard = KEYBOARD.lock();
    let keyboard = guard.as_mut().expect("keyboard should be initialised");
    f(keyboard)
}

#[cfg(all(test, feature = "test-virtio"))]
mod test {
    use super::super::{VIRTIO_REG_DEVICE_STATUS, VIRTIO_STATUS_DRIVER_OK};
    use super::*;
    use crate::arch::mmio;

    #[test_case]
    fn keyboard_device_status_after_init() {
        // virtio_keyboard_init() already ran in main(); verify DRIVER_OK is set
        let status = mmio::read32(board::virtio::keyboard::BASE, VIRTIO_REG_DEVICE_STATUS);
        assert_eq!(status & VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_DRIVER_OK);
    }

    #[test_case]
    fn keyboard_eventq_starts_dry() {
        // Headless CI never types, so no event buffer should ever complete:
        // a drained event here means the device rejected or misread a posted
        // buffer during init.
        let mut events = StackVec::<input::Event, { queue::VIRTQ_ENTRY_NUM }>::new();
        with_keyboard(|kb| kb.drain_events(&mut events));
        assert!(events.is_empty());
    }
}
