//! User library for EASE processes

#![cfg_attr(not(test), no_std)]

pub use ease_abi::ascii;
pub use ease_abi::fds;

mod heap;
mod sync;
#[cfg(target_arch = "riscv32")]
mod syscall;

#[cfg(target_arch = "riscv32")]
pub use syscall::{_start, Writer, exit, put_char, read, write, write_all};

// Host test builds link std, which brings its own panic handler
#[cfg(target_arch = "riscv32")]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Attempt to print the info
    println!("USER PANIC: {}", info.message());
    syscall::exit();
}

// PRINT MACROS

/// Print formatted string .
/// Printing may be interleaved
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        use $crate::Writer;
        let _ = write!(Writer, $($arg)*);
    }}
}

/// Print with newline
/// Printing may be interleaved
#[macro_export]
macro_rules! println {
    () => { {
        $crate::print!("\n");
    }};
    ($($arg:tt)*) => {{
        $crate::print!("{}\n", format_args!($($arg)*));
    }}
}
