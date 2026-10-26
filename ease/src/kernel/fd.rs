//! File descriptors
//!
//! File descriptors are held in a static arena, and each process holds a handle to its slot.

use ease_abi::fds::{STDERR, STDIN, STDOUT};

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

#[derive(Debug)]
#[allow(dead_code)]
pub(super) enum Error {
    /// When the file descriptor within the table range finds a `None` in the slot
    StaleFileDescriptor,
    /// A file descriptor that isn't within the range bounded by `MAX_OPEN` concurrent file descriptor indices
    UnknownFileDescriptor,
    /// The process has too many open files, see [MAX_OPEN]. The rejected entry is returned to the caller to be dropped outside the lock
    TooManyOpenFiles(Entry),
}

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
/// Acquire a new fd table slot for a new process and returns a handle.
/// Takes a lock on FILE_DESCRIPTORS.
///
/// # Panics
/// Panics if there is no available file descriptor table slot. This should
/// never happen as each process has its slot.
pub(super) fn acquire() -> Handle {
    FILE_DESCRIPTORS
        .lock()
        .add(Table::new())
        .expect("there should be a free slot for each process")
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
pub(super) enum Entry {
    ConsoleInput,
    ConsoleOutput,
    DebugConsole,
    File(FileHandle),
}
impl Entry {
    /// Returns what Kind the Entry is.
    /// The kind is copy and does not enforce file close on a file
    /// descriptor being dropped.
    pub(super) fn kind(&self) -> Kind {
        match self {
            Entry::ConsoleInput => Kind::ConsoleInput,
            Entry::ConsoleOutput => Kind::ConsoleOutput,
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
    ConsoleInput,
    ConsoleOutput,
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
        self.0[STDIN] = Some(Entry::ConsoleInput);
        self.0[STDOUT] = Some(Entry::ConsoleOutput);
        self.0[STDERR] = Some(Entry::DebugConsole);
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
            Err(Error::TooManyOpenFiles(entry))
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

// Host tests: ValidFd and Table are plain safe Rust, so validation bounds
// and slot bookkeeping are checked natively. The FILE_DESCRIPTORS static
// and its free functions are left to the QEMU tests (they need the lock
// and run inside spawn/teardown). Entry::File is not built here: a
// FileHandle needs a mounted volume.
#[cfg(all(test, not(target_os = "none"), feature = "test-io"))]
mod host_tests {
    use super::*;

    fn valid(n: usize) -> ValidFd {
        ValidFd::validate(Untrusted::new(n)).expect("fd should be in range")
    }

    fn kind_at(table: &Table, n: usize) -> Option<Kind> {
        table.get(valid(n)).ok().map(Entry::kind)
    }

    #[test]
    fn validate_accepts_every_in_range_fd() {
        for n in 0..MAX_OPEN {
            assert_eq!(valid(n).get(), n);
        }
    }

    #[test]
    fn validate_rejects_out_of_range_fds() {
        for n in [MAX_OPEN, MAX_OPEN + 1, usize::MAX] {
            assert!(
                matches!(
                    ValidFd::validate(Untrusted::new(n)),
                    Err(Error::UnknownFileDescriptor)
                ),
                "fd {n} must be refused"
            );
        }
    }

    #[test]
    fn new_table_is_empty() {
        let table = Table::new();
        assert!(table.is_empty());
        for n in 0..MAX_OPEN {
            assert!(matches!(
                table.get(valid(n)),
                Err(Error::StaleFileDescriptor)
            ));
        }
    }

    #[test]
    fn standard_fds_are_console_input_output_and_debug() {
        let mut table = Table::new();
        table.add_standard_fds();
        assert!(!table.is_empty());
        assert_eq!(kind_at(&table, STDIN), Some(Kind::ConsoleInput));
        assert_eq!(kind_at(&table, STDOUT), Some(Kind::ConsoleOutput));
        assert_eq!(kind_at(&table, STDERR), Some(Kind::DebugConsole));
        for n in 3..MAX_OPEN {
            assert_eq!(kind_at(&table, n), None, "fd {n} should be empty");
        }
    }

    #[test]
    #[should_panic(expected = "previous process teardown")]
    fn standard_fds_twice_panics() {
        let mut table = Table::new();
        table.add_standard_fds();
        table.add_standard_fds();
    }

    #[test]
    fn close_returns_entry_and_empties_slot() {
        let mut table = Table::new();
        table.add_standard_fds();
        let closed = table.close(valid(STDOUT)).expect("stdout is open");
        assert_eq!(closed.kind(), Kind::ConsoleOutput);
        assert_eq!(kind_at(&table, STDOUT), None);
        // Closing again is a stale fd, not a panic
        assert!(matches!(
            table.close(valid(STDOUT)),
            Err(Error::StaleFileDescriptor)
        ));
        // Neighbours are untouched
        assert_eq!(kind_at(&table, STDIN), Some(Kind::ConsoleInput));
        assert_eq!(kind_at(&table, STDERR), Some(Kind::DebugConsole));
    }

    #[test]
    fn open_fills_lowest_free_slot_then_refuses_when_full() {
        let mut table = Table::new();
        table.add_standard_fds();
        assert_eq!(table.open(Entry::ConsoleOutput).ok(), Some(3));
        // A freed low slot is reused before higher ones
        let _ = table.close(valid(1)).expect("fd 1 is open");
        assert_eq!(table.open(Entry::DebugConsole).ok(), Some(1));
        for n in 4..MAX_OPEN {
            assert_eq!(table.open(Entry::ConsoleOutput).ok(), Some(n));
        }
        // Full: the entry comes back to the caller rather than being dropped
        assert!(matches!(
            table.open(Entry::ConsoleInput),
            Err(Error::TooManyOpenFiles(Entry::ConsoleInput))
        ));
    }
}
