//! Handle syscalls

use crate::arch::{trap, umode};
use crate::drivers::{keyboard, uart};
use crate::kernel::{self, interrupts, sync};
use crate::sched::{self, SyscallContext, thread, usermem};
use ease_abi::{fds, syscall};
#[cfg(feature = "profile")]
use ease_macros::profile;

/// Size of the user write buffer. Bytes are copied one at a time with interrupts off for up to this buf length.
const WRITE_BUF_LEN: usize = 128;

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
        syscall::GET_CHAR => {
            kernel::trap::divert_work_to_kernel(
                frame,
                frame.mepc + 4,
                kernel::trap::Work::Syscall(frame.syscall()),
            );
            umode::EcallResult::Diverted
        }
        syscall::WRITE => {
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
                syscall::GET_CHAR => get_char(frame),
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

fn get_char(frame: &mut trap::Frame) {
    // GET_CHAR holds nothing across its waits, so exit-on-the-spot is legal here
    sched::exit_user_thread_if_needs_exit();
    // Block using a wait queue, with a closure to evalute the key press
    let mut key = None;
    let result = keyboard::queue::KEYS_PENDING.wait_with_interruptible(
        &keyboard::queue::CONSUMER,
        |consumer| {
            key = consumer
                .as_mut()
                .expect("decoded key queue must be initialised")
                .pop();
            key.is_some()
        },
    );
    match result {
        Ok(guard) => drop(guard),
        Err(sync::Interrupted) => sched::exit_current_user_thread(thread::ExitReason::Fault),
    }
    // Return the key in the frame's value field
    frame.a0 = 0;
    frame.a1 = key
        .expect("wait should only have returned on a valid key popped")
        .code();
}

fn write(frame: &mut trap::Frame) {
    // No wait loop, so no need to call [sched::exit_user_thread_if_needs_exit].
    // Check if the fd is valid
    if frame.a0 != fds::STDOUT && frame.a0 != fds::STDERR {
        // Wrong file descriptor, set up frame for error number and return
        frame.a0 = ease_abi::Error::BadFd.into();
        frame.a1 = 0;
        return;
    }
    // Create the syscall context to allow the syscall's lifetime to protect access to the user slice memory
    let syscall_context = SyscallContext::current();
    // Build the user buffer and validate it
    let user_buf = usermem::UserBuf::new(frame.a1, frame.a2, usermem::Transfer::FromUser);
    if let Some(validated_user_buf) = syscall_context.validate(user_buf) {
        // Kernel buffer to receive the bytes
        let mut buf = [0u8; WRITE_BUF_LEN];
        // Safely copy into the kernel buffer
        let bytes_copied = validated_user_buf.copy_from_user(&mut buf);
        // Send the bytes via UART
        let bytes_written =
            uart::with_uart_writer(|writer| writer.write_bytes(&buf[..bytes_copied]));
        // Return the number of bytes written
        frame.a0 = 0;
        frame.a1 = bytes_written;
    } else {
        frame.a0 = ease_abi::Error::BadBuffer.into();
        frame.a1 = 0;
    }
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
