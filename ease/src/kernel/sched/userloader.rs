//! Loads user programs
//!
//! Handles binary blobs and functions loaded from flash
//! Only one user program can be loaded at a time.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch;
use crate::kernel::sched::usermem;
use crate::kernel::sync::Mutex;
use crate::kernel::{ipi, panic, percpu};
use crate::user;

// All programs are currently forced to have the same layout by linker script
unsafe extern "C" {
    static mut __user_text_start: u8;
    static __user_text_end: u8;
    static __user_text_lma: u8;

    static mut __user_data_start: u8;
    static __user_data_end: u8;
    static __user_data_lma: u8;

    static mut __user_bss_start: u8;
    static __user_bss_end: u8;
}

/// RISCV requires that a "fence.i" is called after code is loaded.
/// This atomic allows the calling HART to spin until the fence is completed by the other HART
/// Note that the IPI mailbox only sends the signal that a fence is needed, but not when the fence is completed.
pub(crate) static FENCE_ACK: AtomicBool = AtomicBool::new(false);
/// We serialise loading user programs using this mutex. The mutex is purely for serialisation and does not hold any data
static LOAD_LOCK: Mutex<()> = Mutex::new(());

/// User program loader errors
#[derive(Debug)]
pub(crate) enum Error {
    MismatchedBlobSize,
    NotEnoughMemory,
}

/// Newtype to hold the address of the entry to a user program
#[derive(Debug, Clone, Copy)]
pub(crate) struct UserEntry(usize);

impl UserEntry {
    pub(crate) fn from_fn(f: extern "C" fn()) -> Self {
        Self(f as usize)
    }
    /// Build an entry from a raw address. Used by tests to address a blob
    /// image's `_start` (the window base), which has no fn item to name.
    #[allow(dead_code)]
    pub(crate) const fn from_addr(addr: usize) -> Self {
        Self(addr)
    }
    pub(crate) const fn addr(&self) -> usize {
        self.0
    }
}

pub(crate) struct LoadedImage {
    pub(super) user_mem_map: usermem::Map,
    pub(super) entry: UserEntry,
    pub(super) entry_ra: usize, // The value stored into the `ra` slot in the forged trap frame.
}

// Segment address / size pairs
struct Segment {
    addr: Option<*const u8>,
    size: usize,
}

/// Program segment plan
struct Plan {
    text: Segment,
    data: Segment,
    bss: Segment,
}

impl Plan {
    /// Create a program map to load the image
    fn from_image(image: user::Image) -> Result<Self, Error> {
        match image {
            user::Image::Flash(_) => Ok(Self {
                text: Segment {
                    addr: Some(&raw const __user_text_lma),
                    size: &raw const __user_text_end as usize
                        - &raw const __user_text_start as usize,
                },
                data: Segment {
                    addr: Some(&raw const __user_data_lma),
                    size: &raw const __user_data_end as usize
                        - &raw const __user_data_start as usize,
                },
                bss: Segment {
                    addr: None,
                    size: &raw const __user_bss_end as usize - &raw const __user_bss_start as usize,
                },
            }),
            user::Image::Blob(blob_bytes) => {
                let text_size =
                    &raw const __user_text_end as usize - &raw const __user_text_start as usize;
                let data_size =
                    &raw const __user_data_end as usize - &raw const __user_data_start as usize;
                // Check if the blob is the right size
                if blob_bytes.len() != text_size + data_size {
                    return Err(Error::MismatchedBlobSize);
                }
                Ok(Self {
                    text: Segment {
                        addr: Some(blob_bytes.as_ptr()),
                        size: text_size,
                    },
                    data: Segment {
                        addr: Some(blob_bytes[text_size..].as_ptr()),
                        size: data_size,
                    },
                    bss: Segment {
                        addr: None,
                        size: &raw const __user_bss_end as usize
                            - &raw const __user_bss_start as usize,
                    },
                })
            }
        }
    }
    /// Loads a user image into PSRAM and zeros BSS
    ///
    /// # Panics #
    /// Panics if the user memory map has not been set up with the required segments before the loader is called
    fn copy_user_image(&self, user_mem_map: &usermem::Map) {
        assert!(
            user_mem_map
                .region(usermem::Role::Text)
                .expect("must have a .text segment")
                .size()
                == self.text.size
        );
        assert!(
            user_mem_map
                .region(usermem::Role::Data)
                .expect("must have a .data segment")
                .size()
                == self.data.size
        );
        assert!(
            user_mem_map
                .region(usermem::Role::Bss)
                .expect("must have a .bss segment")
                .size()
                == self.bss.size
        );
        // Safety: Linker script sets up symbols to an aligned writeable region
        unsafe {
            // Copy the user .text to PSRAM
            core::ptr::copy_nonoverlapping(
                self.text.addr.expect("must have a text segment"),
                user_mem_map
                    .region(usermem::Role::Text)
                    .expect("must have a ready text memory region")
                    .base()
                    .as_ptr(),
                self.text.size,
            );
            // Copy the user .data to PSRAM
            core::ptr::copy_nonoverlapping(
                self.data.addr.expect("must have a data segment"),
                user_mem_map
                    .region(usermem::Role::Data)
                    .expect("must have a ready data memory region")
                    .base()
                    .as_ptr(),
                self.data.size,
            );
            // Zero the user .bss in PSRAM
            core::ptr::write_bytes(
                user_mem_map
                    .region(usermem::Role::Bss)
                    .expect("must have a ready bss memory region")
                    .base()
                    .as_ptr(),
                0,
                self.bss.size,
            );
        }
    }
}
/// Loads a user program binary into memory
///
/// Returns the loaded image with the user memory map and entry point.
/// Loading is serialised with a mutex, so that one program is loaded at a time.
/// The required fence call is also handled across the HARTs by this loader.
pub(crate) fn load_user_image(image: user::Image) -> Result<LoadedImage, Error> {
    // To enforce serialisation on user load we use the mutex
    let busy_loading = LOAD_LOCK.lock();
    // Create a load plan for this program
    let load_plan = Plan::from_image(image)?;
    // Create the user memory map for this program
    let mut user_mem_map = usermem::Map::new();
    user_mem_map
        .try_for_process()
        .map_err(|_| Error::NotEnoughMemory)?;
    // Now load from the load plan source into the user memory map destination
    load_plan.copy_user_image(&user_mem_map);
    // Set the user entry address
    let entry = match image {
        user::Image::Blob(_) => UserEntry(
            // The linker script puts the _start entry point at the beginning of .text
            user_mem_map
                .region(usermem::Role::Text)
                .expect("must have a ready text memory region")
                .base()
                .addr()
                .into(),
        ),
        user::Image::Flash(f) => UserEntry::from_fn(f),
    };
    // Fence both HARTs so the new instructions are visible
    arch::fence_i();
    if percpu::other_online() {
        // Clear the FENCE_ACK AtomicBool
        FENCE_ACK.store(false, Ordering::Relaxed); // We are not ordering memory off this (which has been handled by the mailbox)
        // Set the IPI reason
        ipi::send(ipi::FENCEI);
        // Now spin until the other HART has run the fence
        while !FENCE_ACK.load(Ordering::Relaxed) {
            // Make sure we aren't in a PANIC situation
            if panic::STOP.load(Ordering::Relaxed) {
                break;
            }
            core::hint::spin_loop();
        }
    }
    drop(busy_loading);
    Ok(LoadedImage {
        user_mem_map,
        entry,
        entry_ra: match image {
            user::Image::Flash(_) => user::user_exit as *const () as usize,
            user::Image::Blob(_) => 0, // Any attempt to return via `ra` will fault on accessing address zero which is PMP protected
        },
    })
}
