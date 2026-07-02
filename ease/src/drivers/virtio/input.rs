//! Virtio input device

use alloc::boxed::Box;

use super::queue::{VirtioVirtq, virtq_init};
use super::{check_virtio, reset_and_handshake, set_driver_ok};
use crate::board::virtio::keyboard;
use crate::kernel::sync::IrqSpinLock;

//Input device id
const VIRTIO_DEVICE_ID: u32 = 18;
// Event queue
const EVENTQ: usize = 0;

static KEYBOARD: IrqSpinLock<Option<VirtioKeyboard>> = IrqSpinLock::new(None);

pub(crate) fn virtio_keyboard_init() {
    let mut keyboard = KEYBOARD.lock();
    assert!(
        keyboard.is_none(),
        "virtio keyboard initialised more than once"
    );
    *keyboard = Some(VirtioKeyboard::new());
}

#[expect(dead_code)]
struct VirtioKeyboard {
    eventq: Box<VirtioVirtq>,
}

impl VirtioKeyboard {
    pub(super) fn new() -> Self {
        check_virtio(keyboard::BASE, VIRTIO_DEVICE_ID);
        let eventq = Self::reset();
        Self { eventq }
    }

    pub(super) fn reset() -> Box<VirtioVirtq> {
        reset_and_handshake(keyboard::BASE);
        let vq = virtq_init(keyboard::BASE, EVENTQ);
        set_driver_ok(keyboard::BASE);
        vq
    }
}
