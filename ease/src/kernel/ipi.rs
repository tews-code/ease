//! Inter processor interrupts
//!
//! We use atomic bitmaps to carry details of the requested IPI as
//! mailboxes. Memory ordering is enforced by the IPI mailbox.

use crate::arch::{self, csr::mie};
use crate::board;
use crate::drivers::clint;
use crate::kernel::collection::{AtomicBitmap, Bitmap, bitmap_words_for};
use crate::kernel::percpu;

const MAILBOX_SIZE: usize = 2;
// Mailbox values
pub(crate) const RESCHEDULE: usize = 0; // Call the scheduler reschedule function
pub(crate) const FENCEI: usize = 1; // Call the instruction fence (code has been loaded)

// Type alias to avoid having to specify the const generic parameters multiple times
type Mailbox = AtomicBitmap<MAILBOX_SIZE, { bitmap_words_for(MAILBOX_SIZE) }>;

// Mailboxes for bits set by the other HART before it calls IPI
static MAILBOX: [Mailbox; board::HARTS_MAX] = [const { Mailbox::new() }; board::HARTS_MAX];

pub fn init() {
    mie::enable_bits(mie::MSIE);
}

pub fn send(reason: usize) {
    assert!(reason < MAILBOX_SIZE, "Unknown IPI reason");
    let that_hart_id = percpu::that_hart_id();
    // If the other HART is offline these IPIs are ignored which is fine since no other threads are running
    if percpu::other_online() {
        MAILBOX[that_hart_id].set(reason);
        clint::set_msip(that_hart_id);
    }
}

pub fn clear_self() {
    clint::clear_msip()
}
/// Drain my own flags and return a snapshot
pub fn drain() -> Bitmap<MAILBOX_SIZE, { bitmap_words_for(MAILBOX_SIZE) }> {
    MAILBOX[arch::hart_id()].drain()
}
