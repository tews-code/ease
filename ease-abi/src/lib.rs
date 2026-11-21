//! System Calls
//!
//! Shared system call definitions and syscall error codes.
//!
//! We take inspiration from the RISCV SBI approach on syscall responses:
//!  `a0` holds 0 on success or a syscall error code on failure
//!  `a1` holds the value on success or zero on error
//!  `a2` additionally holds a return value on success or zero on error for some calls.
//!
//! Also holds
//! - Error codes
//! - File descriptor common values
//! - the maximum heap size
//! - common ASCII codes

#![no_std]

use core::ops::BitOr;

pub mod syscall {
    /// Exit the user process, exiting all threads and releasing all resources
    pub const EXIT: usize = 0;
    /// Read from the given file descriptor
    pub const READ: usize = 1;
    /// Write to the given file descriptor
    pub const WRITE: usize = 2;
    /// Request memory from the kernel for the entire process; must only be called once before the first allocation.
    /// The kernel allocator will round up to a suitable region and return the base address in `a1` and the region size in `a2`.
    pub const ALLOC: usize = 3;
    /// Change directory
    pub const CHDIR: usize = 4;
    /// Open a file
    pub const OPEN: usize = 5;
    /// Close a file
    pub const CLOSE: usize = 6;

    // Test-only syscalls live at 100+ so real syscall growth never
    // collides. The kernel only wires them up in test builds; a
    // production program issuing one gets the unknown-syscall panic.
    pub const TEST_MUTEX_BLOCK: usize = 100;
}

/// Syscall error codes
#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    // Skip 0 as it indicates success
    /// Item not found
    NotFound = 1,
    /// Buffer is invalid, for example outside of the process memory range
    BadBuffer = 2,
    /// File descriptor is invalid
    BadFd = 3,
    /// Attempt to write but zero bytes written; Produced by `ulib` not kernel.
    WriteZero = 4,
    /// Syscall refused
    Interrupted = 5,
    /// The process already has a heap (second call)
    HeapAlreadyAllocated = 6,
    /// The requested size is zero or over the maximum
    HeapSizeNotSupported = 7,
    /// Not enough memory to satisfy the heap request
    NotEnoughHeapMemory = 8,
    /// An object already exists
    AlreadyExists = 9,
    /// A general file system error
    FileSystemError = 10,
    /// A directory was given when a file was expected
    DirectoryInsteadOfFile = 11,
    /// The given directory is not valid
    DirectoryInvalid = 12,
    /// The change could not be made as the directory is the current working directory
    DirectoryIsCurrent = 13,
    /// The directory is not empty
    DirectoryNotEmpty = 14,
    /// The directory is full
    DirFull = 15,
    /// The directory name is invalid as it has an extension
    DirNameHasExtension = 16,
    /// The disk is full
    DiskFull = 17,
    /// The file is in use already
    FileInUse = 18,
    /// The given name is not a directory
    NotADirectory = 19,
    /// The name is invalid
    InvalidName = 20,
    /// There are too many open files for this process. See [MAX_OPEN_FILES].
    TooManyOpenFiles = 21,
    /// Invalid file access bits combination
    InvalidFileAccess = 22,
}

/// Convert an error number into the Error enum.
///
/// If the error is successfully mapped, returns `Ok` holding the error variant.
/// If the error is not mapped, returns `Err` holding the unmapped value.
impl TryFrom<usize> for Error {
    type Error = usize; // when decoding fails, hand back the raw number so it can be printed
    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            v if v == Error::NotFound as usize => Ok(Error::NotFound),
            v if v == Error::BadBuffer as usize => Ok(Error::BadBuffer),
            v if v == Error::BadFd as usize => Ok(Error::BadFd),
            v if v == Error::WriteZero as usize => Ok(Error::WriteZero),
            v if v == Error::Interrupted as usize => Ok(Error::Interrupted),
            v if v == Error::HeapAlreadyAllocated as usize => Ok(Error::HeapAlreadyAllocated),
            v if v == Error::HeapSizeNotSupported as usize => Ok(Error::HeapSizeNotSupported),
            v if v == Error::NotEnoughHeapMemory as usize => Ok(Error::NotEnoughHeapMemory),
            v if v == Error::AlreadyExists as usize => Ok(Error::AlreadyExists),
            v if v == Error::FileSystemError as usize => Ok(Error::FileSystemError),
            v if v == Error::DirectoryInsteadOfFile as usize => Ok(Error::DirectoryInsteadOfFile),
            v if v == Error::DirectoryInvalid as usize => Ok(Error::DirectoryInvalid),
            v if v == Error::DirectoryIsCurrent as usize => Ok(Error::DirectoryIsCurrent),
            v if v == Error::DirectoryNotEmpty as usize => Ok(Error::DirectoryNotEmpty),
            v if v == Error::DirFull as usize => Ok(Error::DirFull),
            v if v == Error::DirNameHasExtension as usize => Ok(Error::DirNameHasExtension),
            v if v == Error::DiskFull as usize => Ok(Error::DiskFull),
            v if v == Error::FileInUse as usize => Ok(Error::FileInUse),
            v if v == Error::NotADirectory as usize => Ok(Error::NotADirectory),
            v if v == Error::InvalidName as usize => Ok(Error::InvalidName),
            v if v == Error::TooManyOpenFiles as usize => Ok(Error::TooManyOpenFiles),
            v if v == Error::InvalidFileAccess as usize => Ok(Error::InvalidFileAccess),
            _ => Err(value),
        }
    }
}
/// Convert an Error into a number.
impl From<Error> for usize {
    fn from(value: Error) -> Self {
        value as usize
    }
}

/// A user program can request at most this much heap memory
pub const MAX_HEAP_SIZE: usize = 4 * 1024 * 1024;

/// A user program can open at most this many files concurrently
pub const MAX_OPEN_FILES: usize = 8;

/// A user program directory path has at most this number of characters (bytes)
pub const MAX_PATH_TEXT: usize = 128;

/// ASCII chars that are used for console and serial control
pub mod ascii {
    pub const BELL: u8 = 0x07;
    pub const BS: u8 = 0x08;
    pub const TAB: u8 = 0x09;
    pub const LF: u8 = 0x0A;
    pub const FF: u8 = 0x0C;
    pub const CR: u8 = 0x0D;
    pub const DEL: u8 = 0x7F;
    pub const ESC: u8 = 0x1B;
}

/// Standard file descriptor values
pub mod fds {
    pub const STDIN: usize = 0;
    pub const STDOUT: usize = 1;
    pub const STDERR: usize = 2;
}

/// Access types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileAccess(usize);

impl FileAccess {
    /// Read file access
    pub const READ: Self = Self(1 << 0);
    /// Write file access
    pub const WRITE: Self = Self(1 << 1);
    /// Ability to create a file, must be combined with WRITE access
    pub const CREATE: Self = Self(1 << 2);
    /// Append a file, must be combined with WRITE access
    pub const APPEND: Self = Self(1 << 3);
    /// Truncate a file, must be combined with WRITE access
    pub const TRUNCATE: Self = Self(1 << 4);

    // Masks
    /// All valid bits
    const ALL: usize =
        Self::READ.0 | Self::WRITE.0 | Self::CREATE.0 | Self::APPEND.0 | Self::TRUNCATE.0;
    /// READ or WRITE bit set
    const READ_OR_WRITE: usize = Self::READ.0 | Self::WRITE.0;
    /// Flags that need an underlying WRITE
    const FLAGS_NEEDING_WRITE: usize = Self::CREATE.0 | Self::APPEND.0 | Self::TRUNCATE.0;

    /// Get the bits as a raw usize
    pub fn raw(&self) -> usize {
        self.0
    }

    /// Check if the FileAccess contains the given flag
    pub fn contains(&self, flag: FileAccess) -> bool {
        self.0 & flag.0 == flag.0
    }
}

impl TryFrom<usize> for FileAccess {
    type Error = Error;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        // Check the bits are valid
        if value & !Self::ALL != 0 {
            return Err(Error::InvalidFileAccess);
        };
        // Check that the bits include either read or write
        if value & Self::READ_OR_WRITE == 0 {
            return Err(Error::InvalidFileAccess);
        }
        // Check if the flags are attached to a WRITE
        if value & Self::FLAGS_NEEDING_WRITE != 0 && value & Self::WRITE.0 == 0 {
            return Err(Error::InvalidFileAccess);
        }
        Ok(Self(value))
    }
}

impl BitOr for FileAccess {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: FileAccess = FileAccess::READ;
    const W: FileAccess = FileAccess::WRITE;
    const C: FileAccess = FileAccess::CREATE;
    const A: FileAccess = FileAccess::APPEND;
    const T: FileAccess = FileAccess::TRUNCATE;

    fn accepted(access: FileAccess) -> bool {
        FileAccess::try_from(access.raw()) == Ok(access)
    }

    fn refused(raw: usize) -> bool {
        FileAccess::try_from(raw) == Err(Error::InvalidFileAccess)
    }

    #[test]
    fn each_constant_is_one_distinct_bit() {
        let all = [R, W, C, A, T];
        for (i, a) in all.iter().enumerate() {
            assert_eq!(a.raw().count_ones(), 1, "constant {i} is not a single bit");
            for b in &all[i + 1..] {
                assert_ne!(a.raw(), b.raw());
            }
        }
    }

    #[test]
    fn bitor_combines_and_raw_round_trips() {
        let rw = R | W;
        assert_eq!(rw.raw(), R.raw() | W.raw());
        assert_eq!(FileAccess::try_from(rw.raw()), Ok(rw));
        // Joining a flag with itself changes nothing
        assert_eq!(R | R, R);
    }

    #[test]
    fn plain_access_is_accepted() {
        assert!(accepted(R), "read");
        assert!(accepted(W), "write");
        assert!(accepted(R | W), "read and write");
    }

    #[test]
    fn flags_with_write_are_accepted() {
        assert!(accepted(W | C), "write, create");
        assert!(accepted(W | A), "write, append");
        assert!(accepted(W | T), "write, truncate");
        assert!(accepted(W | C | T), "save: write, create, truncate");
        assert!(accepted(W | C | A), "log: write, create, append");
        assert!(accepted(R | W | C), "read, write, create");
        assert!(accepted(R | W | C | A | T), "everything");
    }

    #[test]
    fn no_access_at_all_is_refused() {
        assert!(refused(0), "zero");
    }

    #[test]
    fn flags_without_write_are_refused() {
        // A flag alone: neither read nor write
        assert!(refused(C.raw()), "create alone");
        assert!(refused(A.raw()), "append alone");
        assert!(refused(T.raw()), "truncate alone");
        // A flag with read only: still no write
        assert!(refused((R | C).raw()), "read, create");
        assert!(refused((R | A).raw()), "read, append");
        assert!(refused((R | T).raw()), "read, truncate");
        assert!(refused((R | C | A | T).raw()), "read and every flag");
    }

    #[test]
    fn unknown_bits_are_refused() {
        // The first bit past the known ones, alone and with valid access
        let unknown = 1 << 5;
        assert!(refused(unknown));
        assert!(refused(unknown | R.raw()));
        assert!(refused(unknown | (R | W).raw()));
        assert!(refused(1 << (usize::BITS - 1)), "top bit");
        assert!(refused(usize::MAX), "every bit");
    }

    #[test]
    fn every_five_bit_value_follows_the_three_rules() {
        // Exhaustive over the known bits, against the rules stated plainly:
        // at least one of read/write, and any flag needs write.
        for raw in 0..(1usize << 5) {
            let read = raw & R.raw() != 0;
            let write = raw & W.raw() != 0;
            let flag = raw & (C.raw() | A.raw() | T.raw()) != 0;
            let valid = (read || write) && (!flag || write);
            assert_eq!(
                FileAccess::try_from(raw).is_ok(),
                valid,
                "raw {raw:#07b}: read={read} write={write} flag={flag}"
            );
        }
    }

    #[test]
    fn contains_means_all_the_bits_not_any() {
        let rw = R | W;
        assert!(rw.contains(R));
        assert!(rw.contains(W));
        assert!(rw.contains(rw));
        assert!(!rw.contains(C));
        // Read alone has one of the two bits of read-write, not both
        assert!(!R.contains(rw), "read does not contain read-and-write");
        assert!(!(W | C).contains(W | T), "shares write, lacks truncate");
        assert!((W | C | T).contains(W | T));
    }
}
