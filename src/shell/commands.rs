//! Shell commands

use core::fmt::Write;
use core::ops::ControlFlow;

use crate::kernel::timer;
use crate::shell::{Console, ascii};

/// Clears the console screen.
pub fn clear(console: &mut Console) {
    let _ = write!(console, "{}", ascii::FF as char);
}

/// Prints arguments to the console.
pub fn echo(console: &mut Console, args: &str) {
    let _ = writeln!(console, "{args}");
}

/// Prints the list of available commands.
pub fn help(console: &mut Console) {
    let _ = writeln!(console, "Available commands:");
    let _ = writeln!(console, "  clear - Clear the screen");
    let _ = writeln!(console, "  echo  - Print arguments");
    let _ = writeln!(console, "  help  - Show this help");
    let _ = writeln!(console, "  ls    - List files in directory");
    let _ = writeln!(console, "  time  - Show system ticks");
}

/// Lists files in current directory
pub fn ls(console: &mut Console) {
    crate::fs::fat16::with_volume(|vol| {
        let _ = vol
            .read_root_dir(|entry| {
                let name = entry.filename();
                let _ = writeln!(console, "{}", name.as_str().unwrap_or("???"));
                ControlFlow::<()>::Continue(())
            })
            .expect("ls command failed");
    });
}

/// Prints the current system tick count in milliseconds.
pub fn time(console: &mut Console) {
    let _ = writeln!(console, "{} [ms]", timer::ticks_ms());
}

/// Panics the system.
pub fn panic(_console: &mut Console) {
    unsafe {
        core::arch::asm!("unimp");
    }
}

/// Prints an error message for an unrecognised command.
pub fn unknown(console: &mut Console, cmd: &str) {
    let _ = writeln!(console, "Unknown command: {}", cmd);
}
