//! File descriptors
//!
//! File descriptors are held directly in the Process Control Block.
//! This means that we can not support reference counting on file descriptors
//! which has the implication that duplicate file descriptors are not
//! possible (e.g. fork a child with the same files open, or two processes
//! coordinating on the same file read are not supported)

use crate::fs::FileHandle;

pub(super) const MAX: usize = 8; // We let each process open up to MAX files

#[derive(Debug)]
pub(super) enum Error {
    NotAUserProcess,
    NotEnoughSlots(Kind),
    StaleFileDescriptor,
    UnknownFileDescriptor,
}

#[derive(Debug)]
pub(super) enum Kind {
    Keyboard,
    Console,
    DebugConsole,
    File(FileHandle),
}

pub(super) struct Table([Option<Kind>; MAX]);

impl Table {
    pub(super) const fn new() -> Self {
        Self([const { None }; MAX])
    }
    /// Add the base file descriptors for a new process
    ///
    /// Panics - panics if there are any previous entries in the FD array
    pub(super) fn new_process(&mut self) {
        assert!(
            self.is_empty(),
            "previous process teardown didn't clear file descriptor table"
        );
        // A new process traditionally starts with three file descriptors
        self.0[0] = Some(Kind::Keyboard);
        self.0[1] = Some(Kind::Console);
        self.0[2] = Some(Kind::DebugConsole);
    }
    /// Add an open file
    ///
    /// Takes the `fd::Kind` and returns the file descriptor (`usize`) on success
    pub(super) fn open(&mut self, fdk: Kind) -> Result<usize, Error> {
        // Find an empty slot and insert
        if let Some((i, slot)) = self.0.iter_mut().enumerate().find(|(_, s)| s.is_none()) {
            *slot = Some(fdk);
            Ok(i)
        } else {
            Err(Error::NotEnoughSlots(fdk))
        }
    }
    /// Close a file descriptor entry
    ///
    /// Takes the file descriptor and returns the taken `fd::Kind` on success
    pub(super) fn close(&mut self, fd: usize) -> Result<Kind, Error> {
        if fd >= MAX {
            return Err(Error::UnknownFileDescriptor);
        }
        if let Some(fd_kind) = self.0[fd].take() {
            Ok(fd_kind)
        } else {
            Err(Error::StaleFileDescriptor)
        }
    }
    /// Take the entire file descriptor array
    pub(super) fn take_all(&mut self) -> [Option<Kind>; MAX] {
        core::mem::take(&mut self.0)
    }
    /// Check if there are any entries in the file descriptor table
    pub(super) fn is_empty(&self) -> bool {
        self.0.iter().all(Option::is_none)
    }
    /// Close all open file descriptors ignoring errors
    ///
    /// This is called during process close to ensure any files open for writing are closed with
    /// meta data updated
    pub(super) fn close_all(fds: [Option<Kind>; MAX]) {
        for fd in fds.into_iter().flatten() {
            if let Kind::File(of) = fd {
                let _ = of.close();
            }
        }
    }
}
