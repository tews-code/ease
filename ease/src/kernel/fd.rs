//! File descriptors
//
// File descriptors are held directly in the Process Control Block.
// This means that we can not support reference counting on file descriptors
// which has the implication that duplicate file descriptors are not
// possible (e.g. fork a child with the same files open, or two processes
// coordinating on the same file read are not supported)

use crate::fs::FileHandle;

pub(super) const MAX: usize = 8; // We let each process open up to FDS_MAX files

pub(super) enum Error {
    NotAUserProcess,
    NotEnoughSlots(Kind),
    StaleFileDescriptor,
    UnknownFileDescriptor,
}

pub(super) enum Kind {
    Keyboard,
    Console,
    DebugConsole,
    File(FileHandle),
}

pub(super) struct FdsTable([Option<Kind>; MAX]);

impl FdsTable {
    pub(super) const fn new() -> Self {
        Self([const { None }; MAX])
    }
    /// Add the base file descriptors for a new process
    ///
    /// Panics - panics if there are any previous entries in the FD array
    pub(super) fn new_fds(&mut self) {
        assert!(
            self.0.iter().all(Option::is_none),
            "previous proc teardown didn't clear file descriptor table"
        );
        // A new process must have at least three slots (for 0, 1 & 2)
        self.0[0] = Some(Kind::Keyboard);
        self.0[1] = Some(Kind::Console);
        self.0[2] = Some(Kind::DebugConsole);
    }

    pub(super) fn open_fd(&mut self, fdk: Kind) -> Result<usize, Error> {
        // Find an empty slot and insert
        if let Some((i, slot)) = self.0.iter_mut().enumerate().find(|(_, s)| s.is_none()) {
            *slot = Some(fdk);
            Ok(i)
        } else {
            Err(Error::NotEnoughSlots(fdk))
        }
    }

    pub(super) fn close_fd(&mut self, fd: usize) -> Result<Kind, Error> {
        if fd >= MAX {
            return Err(Error::UnknownFileDescriptor);
        }
        if let Some(fd_kind) = self.0[fd].take() {
            Ok(fd_kind)
        } else {
            Err(Error::StaleFileDescriptor)
        }
    }
}
