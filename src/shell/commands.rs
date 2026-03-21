//! Shell commands

use core::fmt::Write;
use core::ops::ControlFlow;

use crate::fs::FsError;
use crate::kernel::timer;
use crate::shell::{Args, Console, ascii};

/// Reads file content to console
pub fn cat(console: &mut Console, args: &Args) {
    for filename in args.positionals.as_slice().iter() {
        crate::fs::volume::with_volume(|vol| match vol.open(filename) {
            Ok(entry) => match vol.read_file(&entry) {
                Ok(content) => match core::str::from_utf8(&content) {
                    Ok(text) => {
                        let _ = write!(console, "{}", text);
                    }
                    Err(_) => {
                        let _ = writeln!(console, "cat: file is not valid text");
                    }
                },
                Err(_) => {
                    let _ = writeln!(console, "cat: unable to read file");
                }
            },
            Err(_) => {
                let _ = write!(console, "cat: {}: No such file or directory", filename);
            }
        })
    }
}

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
    let _ = writeln!(console, "  cat   - Read file content to screen");
    let _ = writeln!(console, "  clear - Clear the screen");
    let _ = writeln!(console, "  echo  - Print arguments");
    let _ = writeln!(console, "  help  - Show this help");
    let _ = writeln!(console, "  ls    - List files in directory");
    let _ = writeln!(console, "  time  - Show system ticks");
    let _ = writeln!(console, "  touch - Create empty file");
    let _ = writeln!(console, "  rm    - Delete file");
}

/// Lists files in current directory
pub fn ls(console: &mut Console, args: &Args) {
    crate::fs::volume::with_volume(|vol| {
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

/// Creates an empty file
pub fn touch(console: &mut Console, args: &Args) {
    for filename in args.positionals.as_slice().iter() {
        crate::fs::volume::with_volume(|vol| match vol.create_empty_file(filename) {
            Ok(_) => {}
            Err(fs_error) => {
                let msg = match fs_error {
                    FsError::DirFull => "directory full",
                    FsError::InvalidName => "invalid file name",
                    _ => "device error",
                };
                let _ = writeln!(console, "touch: {}: {}", filename, msg);
            }
        });
    }
}

/// Deletes a file
pub fn rm(console: &mut Console, args: &Args) {
    for filename in args.positionals.as_slice().iter() {
        crate::fs::volume::with_volume(|vol| match vol.delete_file(filename) {
            Ok(()) => {}
            Err(fs_error) => {
                let msg = match fs_error {
                    FsError::InvalidName => "invalid file name",
                    FsError::NotFound => "file not found",
                    _ => "device error",
                };
                let _ = writeln!(console, "rm: {}: {}", filename, msg);
            }
        });
    }
}

/// Prints an error message for an unrecognised command.
pub fn unknown(console: &mut Console, cmd: &str) {
    let _ = writeln!(console, "Unknown command: {}", cmd);
}
