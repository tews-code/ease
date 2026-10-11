//! File descriptors
//!
//! File descriptors are held in a static arena, and each process holds a handle to its slot.

use crate::fs::FileHandle;
use crate::kernel::collection::Arena;
use crate::kernel::sched::process;
use crate::kernel::sync::IrqSpinLock;
use crate::kernel::validate::{Untrusted, Validate};

pub(super) const MAX_OPEN: usize = 8; // We let each process open up to MAX_OPEN concurrent files

/// Static arena of file descriptor tables. We use IrqSpinLock to safely provide interior mutability.
/// Handles to the table entries are held in the PCB's `fds` struct member.
static FILE_DESCRIPTORS: IrqSpinLock<Arena<Table, { process::MAX_COUNT }>> =
    IrqSpinLock::new(Arena::new());

/// Alias of the file descriptor table handle
pub(super) type Handle = crate::kernel::collection::Handle<Table>;

/// Newtype for a validated file descriptor
/// The ValidFd is in range of the file descriptor table
#[derive(Debug, Clone, Copy)]
pub(super) struct ValidFd(usize);

impl ValidFd {
    pub(super) fn get(self) -> usize {
        self.0
    }
}

impl Validate<usize> for ValidFd {
    type Error = Error;
    fn validate(val: Untrusted<usize>) -> Result<Self, Self::Error> {
        let val = val.raw();
        if val >= MAX_OPEN {
            Err(Error::UnknownFileDescriptor)
        } else {
            Ok(ValidFd(val))
        }
    }
}
/// Create a new fd table for a new process and returns a handle.
/// Takes a lock on FILE_DESCRIPTORS.
///
/// # Panics
/// Panics if there is no slot free - the table is sized by the number of processes so this should never
/// occur.
pub(super) fn new() -> Handle {
    FILE_DESCRIPTORS
        .lock()
        .add(Table::new())
        .expect("number of processes must match number of slots in FILE_DESCRIPTORS arena")
}

/// Set up the standard file descriptors.
/// Takes a lock on FILE_DESCRIPTORS.
///
/// # Panics
/// Panics if the handle is stale.
pub(super) fn add_standard(fds: Handle) {
    let mut guard = FILE_DESCRIPTORS.lock();
    let fd_table = guard.get_mut(fds).expect("handles should never be stale");
    fd_table.add_standard_fds();
}

/// Take the file descriptors for a process.
/// Takes a lock on FILE_DESCRIPTORS.
///
/// # Panics
/// Panics if the handle is stale.
pub(super) fn take(fds: Handle) -> Table {
    FILE_DESCRIPTORS
        .lock()
        .take(fds)
        .expect("handles should never be stale")
}

/// Open a file descriptor. Returns the file descriptor on Ok.
/// Takes a lock on FILE_DESCRIPTORS.
///
/// # Panics
/// Panics if the handle is stale.
pub(super) fn open(fds: Handle, entry: Entry) -> Result<usize, Error> {
    FILE_DESCRIPTORS
        .lock()
        .get_mut(fds)
        .expect("handles should never be stale")
        .open(entry)
}

/// Closes the given file descriptor, returns the `fd::Entry` that was closed on Ok.
/// Takes a lock on FILE_DESCRIPTORS.
///
/// # Panics
/// Panics if the handle is stale.
pub(super) fn close(fds: Handle, fd: ValidFd) -> Result<Entry, Error> {
    FILE_DESCRIPTORS
        .lock()
        .get_mut(fds)
        .expect("handles should never be stale")
        .close(fd)
}

/// Returns the Kind of the given file descriptor, or `None` if the table
/// entry is `None`.
/// Takes a lock on FILE_DESCRIPTORS.
///
/// # Panics
/// Panics if the handle is stale.
pub(super) fn kind(fds: Handle, fd: ValidFd) -> Option<Kind> {
    FILE_DESCRIPTORS
        .lock()
        .get(fds)
        .expect("handles should never be stale")
        .slot(fd)
        .as_ref()
        .map(|f| f.kind())
}

#[derive(Debug)]
#[allow(dead_code)]
pub(super) enum Error {
    /// When the `MAX_OPEN` concurrent file descriptor limit is reached
    NotEnoughSlots(Entry),
    /// When the file descriptor within the table range finds a `None` in the slot
    StaleFileDescriptor,
    /// A file descriptor that isn't within the range bounded by `MAX_OPEN` concurrent file descriptor indices
    UnknownFileDescriptor,
}

#[derive(Debug)]
pub(super) enum Entry {
    Keyboard,
    Console,
    DebugConsole,
    File(FileHandle),
}
impl Entry {
    /// Returns what Kind the Entry is.
    /// The kind is copy and does not enforce file close on a file
    /// descriptor being dropped.
    pub(super) fn kind(&self) -> Kind {
        match self {
            Entry::Keyboard => Kind::Keyboard,
            Entry::Console => Kind::Console,
            Entry::DebugConsole => Kind::DebugConsole,
            Entry::File(_) => Kind::File,
        }
    }
}

/// The file descriptor kind tag - does not hold
/// a file handle. Used to identify the slot.
/// Note that this is Copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Keyboard,
    Console,
    DebugConsole,
    File,
}

#[derive(Default)]
#[must_use]
pub(crate) struct Table([Option<Entry>; MAX_OPEN]);

impl Table {
    pub(super) const fn new() -> Self {
        Self([const { None }; MAX_OPEN])
    }

    /// Add the base file descriptors for a new process
    ///
    /// Panics - panics if there are any previous entries in the FD array
    pub(super) fn add_standard_fds(&mut self) {
        assert!(
            self.is_empty(),
            "previous process teardown didn't clear file descriptor table"
        );
        // A new process traditionally starts with three file descriptors
        self.0[0] = Some(Entry::Keyboard);
        self.0[1] = Some(Entry::Console);
        self.0[2] = Some(Entry::DebugConsole);
    }

    /// Add an open file
    ///
    /// Takes the `fd::Entry` and returns the file descriptor (`usize`) on success
    pub(super) fn open(&mut self, entry: Entry) -> Result<usize, Error> {
        // Find an empty slot and insert
        if let Some((i, slot)) = self.0.iter_mut().enumerate().find(|(_, s)| s.is_none()) {
            *slot = Some(entry);
            Ok(i)
        } else {
            Err(Error::NotEnoughSlots(entry))
        }
    }

    /// Close a file descriptor entry
    ///
    /// Takes the file descriptor and returns the taken `fd::Entry` on success
    pub(super) fn close(&mut self, fd: ValidFd) -> Result<Entry, Error> {
        if let Some(entry) = self.slot_mut(fd).take() {
            Ok(entry)
        } else {
            Err(Error::StaleFileDescriptor)
        }
    }

    /// Check if there are any entries in the file descriptor table
    pub(super) fn is_empty(&self) -> bool {
        self.0.iter().all(Option::is_none)
    }

    /// Helper function to return a shared reference to a valid slot.
    fn slot(&self, fd: ValidFd) -> &Option<Entry> {
        &self.0[fd.get()] // Direct index is as we know ValidFd is in range
    }

    /// Helper function to return an exclusive mutable reference to a valid slot.
    fn slot_mut(&mut self, fd: ValidFd) -> &mut Option<Entry> {
        &mut self.0[fd.get()] // Direct index is as we know ValidFd is in range
    }

    /// Get the &[Entry] of a file descriptor
    ///
    /// Returns [Error::StaleFileDescriptor] if the fd is not found
    pub(super) fn get(&self, fd: ValidFd) -> Result<&Entry, Error> {
        self.slot(fd).as_ref().ok_or(Error::StaleFileDescriptor)
    }
}
