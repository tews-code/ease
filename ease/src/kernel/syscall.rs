//! Handle syscalls

use crate::arch::{trap, umode};
use crate::drivers::uart;
use crate::fs;
use crate::kernel::alloc;
use crate::kernel::console_input;
use crate::kernel::sync::Arc;
use crate::kernel::validate::{Untrusted, Validate};
use crate::kernel::{self, fd, interrupts};
use crate::sched::process::{self, SyscallContext};
use crate::sched::{self, thread, usermem};
use ease_abi::syscall;
#[cfg(feature = "profile")]
use ease_macros::profile;

/// Size of the user write buffer. Bytes are copied one at a time with interrupts off for up to this buf length.
const WRITE_BUF_LEN: usize = 128;
const READ_BUF_LEN: usize = 128;

/// Handle U-mode ecalls
///
/// We only expect ecalls from u-mode.
/// Interrupts are disabled; these need to be a quick response or
/// for blocking calls we use the percpu::resume_* stash and [divert_work_to_kernel].
#[inline(never)]
#[cold]
#[cfg_attr(feature = "profile", profile)]
pub(crate) fn handle_ecall(frame: &mut trap::Frame) -> umode::EcallResult {
    match frame.syscall() {
        syscall::ALLOC | syscall::CHDIR | syscall::OPEN | syscall::READ | syscall::WRITE => {
            kernel::trap::divert_work_to_kernel(
                frame,
                frame.mepc + 4,
                kernel::trap::Work::Syscall(frame.syscall()),
            );
            umode::EcallResult::Diverted
        }
        syscall::EXIT => {
            frame.a0 = thread::ExitReason::Exit as usize;
            frame.set_up_for_divert_to_kernel(umode::user_thread_exit as *const () as usize);
            umode::EcallResult::Diverted
        }
        // Test-only: park holding a kernel mutex, for the boundary-kill test.
        #[cfg(all(test, feature = "test-sched"))]
        syscall::TEST_MUTEX_BLOCK => {
            kernel::trap::divert_work_to_kernel(
                frame,
                frame.mepc + 4,
                kernel::trap::Work::Syscall(frame.syscall()),
            );
            umode::EcallResult::Diverted
        }
        _ => {
            // Advance mepc
            frame.mepc += 4;
            crate::dprint!("user ecall code {}", frame.syscall());
            umode::EcallResult::Completed
        }
    }
}

/// Handles diverted system calls for user threads that were not immediately handled,
/// e.g. as they may block, or take a scheduler lock.
///
/// Interrupts are enabled.
///
/// # Panics #
/// Panics if the system call number is unknown
pub(crate) fn handle_diverted_user_thread(frame: &mut trap::Frame, syscall: usize) {
    // Safety: We have arrived from mret and do not have any interrupts disabled tokens held
    unsafe {
        interrupts::with_interrupts_enabled(|| {
            // If this is a condemned thread, exit before entering a (possibly blocking) syscall
            sched::exit_user_thread_if_needs_exit();
            // Call the syscall function
            match syscall {
                syscall::ALLOC => alloc(frame),
                syscall::CHDIR => chdir(frame),
                syscall::OPEN => open(frame),
                syscall::READ => read(frame),
                syscall::WRITE => write(frame),

                // Test-only: the LOCK-HOLDING exemplar of the interruptible-wait
                // discipline. The guard lives in an inner scope: on Err(Interrupted)
                // we leave the scope by normal control flow, the guard drops (mutex
                // freed), and only THEN does the thread exit. Calling exit inside
                // the scope would leak the guard — exit diverges, Drop never runs.
                #[cfg(all(test, feature = "test-sched"))]
                syscall::TEST_MUTEX_BLOCK => test_mutex_block(frame),
                _ => panic!("unexpected blocking syscall: {}", syscall),
            }
        })
    }
}

/// Translate from allocation error to ABI error
impl From<alloc::Error> for ease_abi::Error {
    fn from(value: alloc::Error) -> Self {
        match value {
            alloc::Error::NotEnoughMemory => ease_abi::Error::NotEnoughHeapMemory,
            alloc::Error::HeapSizeNotSupported => ease_abi::Error::HeapSizeNotSupported,
        }
    }
}

/// Translate from heap error to ABI error
impl From<process::HeapError> for ease_abi::Error {
    fn from(value: process::HeapError) -> Self {
        match value {
            process::HeapError::ProcessNotInAliveState => ease_abi::Error::Interrupted,
            process::HeapError::ProcessAlreadyHasHeapInstalled => {
                ease_abi::Error::HeapAlreadyAllocated
            }
        }
    }
}

/// Translate from process error to ABI error
impl From<process::Error> for ease_abi::Error {
    fn from(_value: process::Error) -> Self {
        ease_abi::Error::Interrupted
    }
}

/// Translate from file system error to ABI error
impl From<fs::Error> for ease_abi::Error {
    fn from(value: fs::Error) -> Self {
        match value {
            fs::Error::DirNameHasExtension => ease_abi::Error::DirNameHasExtension,
            fs::Error::NotADirectory => ease_abi::Error::NotADirectory,
            fs::Error::NotFound => ease_abi::Error::NotFound,
            fs::Error::InvalidName => ease_abi::Error::InvalidName,
            fs::Error::DirectoryInvalid => ease_abi::Error::DirectoryInvalid,
            fs::Error::TooManyOpenFiles => ease_abi::Error::TooManyOpenFiles,
            fs::Error::DirectoryInsteadOfFile => ease_abi::Error::DirectoryInsteadOfFile,
            fs::Error::OpeningForReadButWriteInProgress => ease_abi::Error::FileIsBusy,
            _ => ease_abi::Error::FileSystemError,
        }
    }
}

/// Translate from file descriptor error to ABI error
impl From<fd::Error> for ease_abi::Error {
    fn from(value: fd::Error) -> Self {
        match value {
            fd::Error::StaleFileDescriptor => ease_abi::Error::BadFd,
            fd::Error::TooManyOpenFiles(_) => ease_abi::Error::TooManyOpenFiles,
            fd::Error::UnknownFileDescriptor => ease_abi::Error::BadFd,
        }
    }
}

/// Translate from a user-provided file access to a filesystem file access
impl TryFrom<ease_abi::FileAccess> for fs::file::Access {
    type Error = ease_abi::Error;

    fn try_from(value: ease_abi::FileAccess) -> Result<Self, Self::Error> {
        if value == ease_abi::FileAccess::READ {
            Ok(fs::file::Access::Read)
        } else {
            Err(ease_abi::Error::NotSupported)
        }
    }
}

/// Allocate heap memory for a user process.
///
/// A process gets at most one heap; additional requests are refused
fn alloc(frame: &mut trap::Frame) {
    match try_alloc(Untrusted::new(frame.a0)) {
        Ok((base, size)) => {
            frame.a0 = 0;
            frame.a1 = base;
            frame.a2 = size;
        }
        Err(e) => {
            frame.a0 = e.into();
            frame.a1 = 0;
            frame.a2 = 0;
        }
    }
}

/// Allocate heap memory for a user process.
///
/// A process gets at most one heap; additional requests are refused
fn try_alloc(size: Untrusted<usize>) -> Result<(usize, usize), ease_abi::Error> {
    // Validate the untrusted size and convert to an order
    let order = usermem::ValidOrder::validate(size)?;
    // Allocate outside of a lock
    let heap = usermem::Map::allocate_heap(order)?;
    let base = heap.base_addr();
    let size = heap.size();
    // Install the heap into the user mem map
    sched::install_user_heap(heap)?;
    Ok((base, size))
}

#[cfg(all(test, feature = "test-sched"))]
fn test_mutex_block(frame: &mut trap::Frame) {
    use crate::kernel::sched::test_support;
    {
        let _guard = test_support::TEST_MUTEX.lock();
        while test_support::TEST_MUTEX_SIGNAL.wait_interruptible().is_ok() {
            // Spurious signal: keep holding and keep waiting.
        }
        // Err(Interrupted): fall out of the scope, dropping _guard.
    }
    sched::exit_current_user_thread(thread::ExitReason::Fault);
}

/// Copy a path from a user buffer, returning errors if the path is not valid
fn copy_path_from_user<'a>(
    context: &SyscallContext,
    base_addr: Untrusted<usize>,
    size: Untrusted<usize>,
    new_path_buf: &'a mut [u8; ease_abi::MAX_PATH_TEXT],
) -> Result<&'a str, ease_abi::Error> {
    // Check the given path isn't too long
    let path_text_len = size.raw();
    if path_text_len > ease_abi::MAX_PATH_TEXT {
        return Err(ease_abi::Error::InvalidPath);
    };
    // Check if the path isn't zero chars
    if path_text_len == 0 {
        return Err(ease_abi::Error::InvalidPath);
    }
    // Reconstitute the user buffer and validate
    let user_buf =
        usermem::UserBuf::new(base_addr.raw(), path_text_len, usermem::Transfer::FromUser);
    let validated_user_buf = context
        .validate_buf(user_buf)
        .ok_or(ease_abi::Error::BadBuffer)?;
    let user_path_text_len = validated_user_buf.copy_from_user(new_path_buf);
    if user_path_text_len != path_text_len {
        return Err(ease_abi::Error::BadBuffer);
    }
    // Convert to a UTF8 string
    let Ok(user_path) = str::from_utf8(&new_path_buf[..user_path_text_len]) else {
        return Err(ease_abi::Error::InvalidPath);
    };
    Ok(user_path)
}

/// Change directory for a process.
fn chdir(frame: &mut trap::Frame) {
    match try_chdir(Untrusted::new(frame.a0), Untrusted::new(frame.a1)) {
        Ok(()) => {
            frame.a0 = 0;
            frame.a1 = 0;
        }
        Err(e) => {
            frame.a0 = e.into();
            frame.a1 = 0;
        }
    }
}

/// Change directory for a process
fn try_chdir(
    path_buf_addr: Untrusted<usize>,
    path_buf_size: Untrusted<usize>,
) -> Result<(), ease_abi::Error> {
    // Get the syscall context
    let context = SyscallContext::current()?;
    // Copy the path
    let mut new_path_buf = [0u8; ease_abi::MAX_PATH_TEXT];
    let new_dir_path =
        copy_path_from_user(&context, path_buf_addr, path_buf_size, &mut new_path_buf)?;
    // Call the filesystem with the given path
    let mut current_cwd = context.cwd();
    fs::file::change_directory(&mut current_cwd, new_dir_path)?;
    // Update the syscall context on success
    context.set_cwd(current_cwd)?;
    Ok(())
}

/// Open a file
fn open(frame: &mut trap::Frame) {
    match try_open(
        Untrusted::new(frame.a0),
        Untrusted::new(frame.a1),
        Untrusted::new(frame.a2),
    ) {
        Ok(fd) => {
            frame.a0 = 0;
            frame.a1 = fd;
        }
        Err(e) => {
            frame.a0 = e.into();
            frame.a1 = 0;
        }
    }
}

/// Call the file system open on the given path
fn try_open(
    path_buf_addr: Untrusted<usize>,
    path_buf_size: Untrusted<usize>,
    access_type: Untrusted<usize>,
) -> Result<usize, ease_abi::Error> {
    // Get the syscall context
    let context = SyscallContext::current()?;
    // Copy the path
    let mut path_buf = [0u8; ease_abi::MAX_PATH_TEXT];
    let path = copy_path_from_user(&context, path_buf_addr, path_buf_size, &mut path_buf)?;
    // Check the access type
    let requested_access = ease_abi::FileAccess::validate(access_type)?;
    let fs_access = fs::file::Access::try_from(requested_access)?;
    // Open the file
    let file = fs::file::open(fs_access, context.cwd(), path)?;
    // Record this in the file descriptor table (using ARC)
    let shared_file = Arc::new(file);
    let entry = fd::Entry::File(shared_file);
    let fd = fd::open(context.fds(), entry)?;
    Ok(fd)
}

/// Read from given file descriptor into the provided buffer
/// See [try_read]
fn read(frame: &mut trap::Frame) {
    match try_read(
        Untrusted::new(frame.a0),
        Untrusted::new(frame.a1),
        Untrusted::new(frame.a2),
        Untrusted::new(frame.a3),
    ) {
        Ok(bytes_read) => {
            frame.a0 = 0;
            frame.a1 = bytes_read;
        }
        Err(e) => {
            frame.a0 = e.into();
            frame.a1 = 0;
        }
    }
}

/// Read from given file descriptor into the provided buffer
fn try_read(
    fd: Untrusted<usize>,
    user_buf_addr: Untrusted<usize>,
    user_buf_size: Untrusted<usize>,
    timeout_ms: Untrusted<usize>,
) -> Result<usize, ease_abi::Error> {
    // Validate
    let fd = fd::ValidFd::validate(fd)?;
    // Validate the file descriptor kind
    let context = SyscallContext::current()?;
    if !matches!(fd::kind(context.fds(), fd), Some(fd::Kind::ConsoleInput)) {
        // Wrong file descriptor, return
        return Err(ease_abi::Error::BadFd);
    }
    // Build the user buffer and validate it
    let user_buf = usermem::UserBuf::new(
        user_buf_addr.raw(),
        user_buf_size.raw(),
        usermem::Transfer::ToUser,
    );
    let validated_user_buf = context
        .validate_buf(user_buf)
        .ok_or(ease_abi::Error::BadBuffer)?;
    // Kernel buffer to receive the bytes
    let mut buf = [0u8; READ_BUF_LEN];
    // Check the requested timeout. If it is u32::MAX the caller
    // doesn't want a timeout.
    // We trust the Unsafe timeout as it is by definition is valid across the entire u32 range.
    let timeout = timeout_ms.raw() as u32;
    let bytes_read = if timeout == u32::MAX {
        // Read the bytes via console input; we sleep until at least a byte is available
        match console_input::wait_and_read_interruptible(
            &mut buf[..validated_user_buf.len().min(READ_BUF_LEN)],
        ) {
            Ok(bytes_read) => bytes_read,
            Err(kernel::sync::Interrupted) => {
                sched::exit_current_user_thread(thread::ExitReason::Fault)
            }
        }
    } else {
        // Read the bytes via console input; we sleep until at least a byte is available or timeout is reached
        match console_input::wait_and_read_interruptible_until(
            &mut buf[..validated_user_buf.len().min(READ_BUF_LEN)],
            timeout,
        ) {
            Ok(bytes_read) => bytes_read,
            Err(kernel::sync::Interrupted) => {
                sched::exit_current_user_thread(thread::ExitReason::Fault)
            }
        }
    };
    // Safely copy from the kernel buffer into the user buffer
    let bytes_copied = validated_user_buf.copy_to_user(&buf[..bytes_read]);
    // Return the number of bytes read
    Ok(bytes_copied)
}

/// Write from given user buffer to the given file descriptor
/// See [try_write]
fn write(frame: &mut trap::Frame) {
    match try_write(
        Untrusted::new(frame.a0),
        Untrusted::new(frame.a1),
        Untrusted::new(frame.a2),
    ) {
        Ok(bytes_written) => {
            frame.a0 = 0;
            frame.a1 = bytes_written;
        }
        Err(e) => {
            frame.a0 = e.into();
            frame.a1 = 0;
        }
    }
}

/// Write from the given buffer to the given file descriptor
///
/// Returns the number of bytes written
fn try_write(
    fd: Untrusted<usize>,
    user_buf_addr: Untrusted<usize>,
    user_buf_size: Untrusted<usize>,
) -> Result<usize, ease_abi::Error> {
    // Validate the file descriptor
    let fd = fd::ValidFd::validate(fd)?;
    // Check what kind the fd is indexing
    let context = SyscallContext::current()?;
    if !matches!(
        fd::kind(context.fds(), fd),
        Some(fd::Kind::ConsoleOutput | fd::Kind::DebugConsole)
    ) {
        // Wrong file descriptor, return
        return Err(ease_abi::Error::BadFd);
    }
    // Build the user buffer and validate it
    let user_buf = usermem::UserBuf::new(
        user_buf_addr.raw(),
        user_buf_size.raw(),
        usermem::Transfer::FromUser,
    );
    let validated_user_buf = context
        .validate_buf(user_buf)
        .ok_or(ease_abi::Error::BadBuffer)?;
    // Kernel buffer to receive the bytes
    let mut buf = [0u8; WRITE_BUF_LEN];
    // Safely copy into the kernel buffer
    let bytes_copied = validated_user_buf.copy_from_user(&mut buf);
    // Send the bytes via UART; we sleep on the wait queue until there
    // is some free space in the queue
    let bytes_written = match uart::wait_and_print_interruptible(&buf[..bytes_copied]) {
        Ok(bytes_written) => bytes_written,
        Err(kernel::sync::Interrupted) => {
            sched::exit_current_user_thread(thread::ExitReason::Fault)
        }
    };
    // Return the number of bytes written
    Ok(bytes_written)
}
