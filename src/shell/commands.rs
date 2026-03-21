//! Shell commands

use core::fmt::Write;
use core::ops::ControlFlow;

use crate::kernel::timer;
use crate::shell::{Args, Console, ascii};

/// Clears the console screen.
pub fn clear(console: &mut Console) {
    let _ = write!(console, "{}", ascii::FF as char);
}

/// Prints arguments to the console.
pub fn echo(console: &mut Console, args: &Args) {
    for (i, arg) in args.positionals.as_slice().iter().enumerate() {
        if i > 0 {
            let _ = write!(console, " ");
        }
        let _ = write!(console, "{arg}");
    }
    let _ = writeln!(console);
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
pub fn ls(console: &mut Console, args: &Args) {
    crate::fs::fat16::with_volume(|vol| {
        let _ = vol
            .read_root_dir(|entry| {
                let name = entry.filename();
                if args.has_flag(b'l') {
                    let _ = writeln!(
                        console,
                        "{:>8}  {}",
                        entry.file_size,
                        name.as_str().unwrap_or("???")
                    );
                } else {
                    let _ = write!(console, "{}  ", name.as_str().unwrap_or("???"));
                }
                ControlFlow::<()>::Continue(())
            })
            .expect("ls command failed");

        if !args.has_flag(b'l') {
            let _ = writeln!(console);
        }
    });
}

/// Prints the current system tick count in milliseconds.
pub fn time(console: &mut Console) {
    let _ = writeln!(console, "{} [ms]", timer::ticks_ms());
}

/// Panics the system.
pub fn panic(_console: &mut Console) {
    panic!("user requested panic");
}

/// Prints an error message for an unrecognised command.
pub fn unknown(console: &mut Console, cmd: &str) {
    let _ = writeln!(console, "Unknown command: {}", cmd);
}
