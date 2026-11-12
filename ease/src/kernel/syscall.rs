//! Handle syscalls

use crate::arch::{trap, umode};
use crate::drivers::uart;
use crate::fs;
use crate::kernel::alloc;
use crate::kernel::console_input;
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
        syscall::EXIT => {
            frame.a0 = thread::ExitReason::Exit as usize;
            frame.set_up_for_divert_to_kernel(umode::user_thread_exit as *const () as usize);
            umode::EcallResult::Diverted
        }
        syscall::ALLOC | syscall::CHDIR | syscall::READ | syscall::WRITE => {
            kernel::trap::divert_work_to_kernel(
                frame,
                frame.mepc + 4,
                kernel::trap::Work::Syscall(frame.syscall()),
            );
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
/// Checks if the user thread should be exited and performs exit call where appropriate
/// i.e. in looping calls.
///
/// Interrupts are enabled.
///
/// # Panics #
/// Panics if the system call number is unknown
pub(crate) fn handle_diverted_user_thread(frame: &mut trap::Frame, syscall: usize) {
    // Safety: We have arrived from mret and do not have any interrupts disabled tokens held
    unsafe {
        interrupts::with_interrupts_enabled(|| {
            match syscall {
                syscall::ALLOC => alloc(frame),
                syscall::CHDIR => chdir(frame),
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

/// Read from given file descriptor into the provided buffer
fn read(frame: &mut trap::Frame) {
    // READ holds nothing across its waits, so exit-on-the-spot is legal here
    sched::exit_user_thread_if_needs_exit();
    // Get the untrusted file descriptor from the frame.
    let fd = Untrusted::new(frame.a0);
    // Validate
    let fd = match fd::ValidFd::validate(fd) {
        Ok(fd) => fd,
        Err(_) => {
            frame.a0 = ease_abi::Error::BadFd.into();
            frame.a1 = 0;
            return;
        }
    };
    // Validate the file descriptor kind
    let Ok(syscall_context) = SyscallContext::current() else {
        frame.a0 = ease_abi::Error::Interrupted.into();
        frame.a1 = 0;
        return;
    };
    if !matches!(
        fd::kind(syscall_context.fds(), fd),
        Some(fd::Kind::ConsoleInput)
    ) {
        // Wrong file descriptor, set up frame for error number and return
        frame.a0 = ease_abi::Error::BadFd.into();
        frame.a1 = 0;
        return;
    }
    // Build the user buffer and validate it
    let user_buf = usermem::UserBuf::new(frame.a1, frame.a2, usermem::Transfer::ToUser);
    let Some(validated_user_buf) = syscall_context.validate_buf(user_buf) else {
        frame.a0 = ease_abi::Error::BadBuffer.into();
        frame.a1 = 0;
        return;
    };
    // Kernel buffer to receive the bytes
    let mut buf = [0u8; READ_BUF_LEN];
    // Check the requested timeout. If it is u32::MAX the caller
    // doesn't want a timeout.
    // We trust frame.a3 as the timeout is valid across the entire u32 range.
    let bytes_read = if frame.a3 as u32 == u32::MAX {
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
            frame.a3 as u32,
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
    frame.a0 = 0;
    frame.a1 = bytes_copied;
}

/// Write from the given buffer to the given file descriptor
fn write(frame: &mut trap::Frame) {
    // No wait loop, so no need to call [sched::exit_user_thread_if_needs_exit].
    // Get the (untrusted) file descriptor from the frame
    let fd = Untrusted::new(frame.a0);
    let fd = match fd::ValidFd::validate(fd) {
        Ok(fd) => fd,
        Err(_) => {
            frame.a0 = ease_abi::Error::BadFd.into();
            frame.a1 = 0;
            return;
        }
    };
    // Check what kind the fd is indexing
    let Ok(syscall_context) = SyscallContext::current() else {
        // The process is not Alive - syscall has been interrupted
        frame.a0 = ease_abi::Error::Interrupted.into();
        frame.a1 = 0;
        return;
    };
    if !matches!(
        fd::kind(syscall_context.fds(), fd),
        Some(fd::Kind::ConsoleOutput | fd::Kind::DebugConsole)
    ) {
        // Wrong file descriptor, set up frame for error number and return
        frame.a0 = ease_abi::Error::BadFd.into();
        frame.a1 = 0;
        return;
    }
    // Build the user buffer and validate it
    let user_buf = usermem::UserBuf::new(frame.a1, frame.a2, usermem::Transfer::FromUser);
    if let Some(validated_user_buf) = syscall_context.validate_buf(user_buf) {
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
        frame.a0 = 0;
        frame.a1 = bytes_written;
    } else {
        frame.a0 = ease_abi::Error::BadBuffer.into();
        frame.a1 = 0;
    }
}

impl From<alloc::Error> for ease_abi::Error {
    fn from(value: alloc::Error) -> Self {
        match value {
            alloc::Error::NotEnoughMemory => ease_abi::Error::NotEnoughHeapMemory,
            alloc::Error::HeapSizeNotSupported => ease_abi::Error::HeapSizeNotSupported,
        }
    }
}

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

impl From<process::Error> for ease_abi::Error {
    fn from(_value: process::Error) -> Self {
        ease_abi::Error::Interrupted
    }
}

impl From<fs::Error> for ease_abi::Error {
    fn from(value: fs::Error) -> Self {
        match value {
            fs::Error::DirNameHasExtension => ease_abi::Error::DirNameHasExtension,
            fs::Error::NotADirectory => ease_abi::Error::NotADirectory,
            fs::Error::NotFound => ease_abi::Error::NotFound,
            fs::Error::InvalidName => ease_abi::Error::InvalidName,
            fs::Error::DirectoryInvalid => ease_abi::Error::DirectoryInvalid,
            _ => ease_abi::Error::FileSystemError,
        }
    }
}

/// Allocate heap memory for a user process.
///
/// A process gets at most one heap; additional requests are refused
fn alloc(frame: &mut trap::Frame) {
    // Nothing is held yet, so exiting is safe
    sched::exit_user_thread_if_needs_exit();
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

/// Change directory for a process.
fn chdir(frame: &mut trap::Frame) {
    // Nothing is held yet, so exiting is safe
    sched::exit_user_thread_if_needs_exit();
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
fn try_chdir(base_addr: Untrusted<usize>, size: Untrusted<usize>) -> Result<(), ease_abi::Error> {
    // Get the syscall context
    let context = SyscallContext::current()?;
    // Check the given path isn't too long
    let path_text_len = size.raw();
    if path_text_len > ease_abi::MAX_PATH_TEXT {
        return Err(ease_abi::Error::InvalidName);
    };
    // Check if the path isn't zero chars
    if path_text_len == 0 {
        return Err(ease_abi::Error::DirectoryInvalid);
    }
    // Reconstitute the user buffer and validate
    let user_buf =
        usermem::UserBuf::new(base_addr.raw(), path_text_len, usermem::Transfer::FromUser);
    let validated_user_buf = context
        .validate_buf(user_buf)
        .ok_or(ease_abi::Error::BadBuffer)?;
    let new_dir_path_buf = &mut [0u8; ease_abi::MAX_PATH_TEXT];
    let new_dir_path_text_len = validated_user_buf.copy_from_user(new_dir_path_buf);
    if new_dir_path_text_len != path_text_len {
        return Err(ease_abi::Error::BadBuffer);
    }
    // Convert to a UTF8 string
    let Ok(new_dir_path) = str::from_utf8(&new_dir_path_buf[..new_dir_path_text_len]) else {
        return Err(ease_abi::Error::InvalidName);
    };
    // Call the filesystem with the given path
    let mut current_cwd = context.cwd();
    fs::file::change_directory(&mut current_cwd, new_dir_path)?;
    // Update the syscall context on success
    context.set_cwd(current_cwd)?;
    Ok(())
}
