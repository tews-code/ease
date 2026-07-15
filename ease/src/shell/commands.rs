//! Shell commands

use alloc::string::String;

use core::fmt::Write;
use core::ops::ControlFlow;

use crate::fs::{DirHandle, FsError};
#[cfg(feature = "paint-stack")]
use crate::kernel::sched;
use crate::kernel::timer;
use crate::shell::{Args, Console, ascii};

/// Reads file content to console
#[allow(dead_code)]
pub fn cat(console: &mut Console, args: &Args) {
    let current_dir = DirHandle { start_cluster: 0 };
    for filename in args.positionals.as_slice().iter() {
        crate::fs::volume::with_volume(|vol| match vol.open(current_dir, filename) {
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
#[allow(dead_code)]
pub fn clear(console: &mut Console) {
    let _ = write!(console, "{}", ascii::FF as char);
}

/// Prints arguments to the console.
#[allow(dead_code)]
pub fn echo(console: &mut Console, args: &Args) {
    let _ = writeln!(console, "{}", args.rest);
}

/// Prints the list of available commands.
#[allow(dead_code)]
pub fn help(console: &mut Console) {
    let _ = writeln!(console, "Available commands:");
    let _ = writeln!(console, "  cat     - Read file content to screen");
    let _ = writeln!(console, "  clear   - Clear the screen");
    let _ = writeln!(console, "  echo    - Print arguments");
    let _ = writeln!(console, "  help    - Show this help");
    let _ = writeln!(console, "  hexdump - Raw file output");
    let _ = writeln!(console, "  ls      - List files in directory");
    #[cfg(feature = "paint-stack")]
    let _ = writeln!(console, "  stacks  - Print painted kernel thread stacks");
    let _ = writeln!(console, "  time    - Show system time since boot [ms]");
    let _ = writeln!(console, "  touch   - Create empty file");
    let _ = writeln!(console, "  rm      - Delete file");
    let _ = writeln!(console, "  write   - Write text to file");
}

/// Shows the raw file details in hex format
///
/// - Supports -C argument
#[allow(dead_code)]
pub fn hexdump(console: &mut Console, args: &Args) {
    // Open the file
    for filename in args.positionals.as_slice().iter() {
        let current_dir = DirHandle { start_cluster: 0 };
        crate::fs::volume::with_volume(|vol| match vol.open(current_dir, filename) {
            Ok(entry) => match vol.read_file(&entry) {
                Ok(content) => {
                    if args.has_flag(b'C') {
                        // byte-by-byte with ASCII column
                        // Loop through content 16 bytes at a time
                        for (i, chunk) in content.chunks(16).enumerate() {
                            let offset = i * 16;
                            // chunk is a &[u8], length 16 (or less for the last one)
                            // Print offset
                            let _ = write!(console, "{:08x}  ", offset);

                            // Print hex bytes
                            for (j, &byte) in chunk.iter().enumerate() {
                                let _ = write!(console, "{:02x} ", byte);
                                if j == 7 {
                                    let _ = write!(console, " ");
                                }
                            }

                            // Pad if chunk is shorter than 16 (last line)
                            for j in chunk.len()..16 {
                                let _ = write!(console, "   ");
                                if j == 7 {
                                    let _ = write!(console, " ");
                                }
                            }

                            // Print ASCII column
                            let _ = write!(console, " |");
                            for &byte in chunk {
                                let ch = if byte.is_ascii_graphic() || byte == b' ' {
                                    byte as char
                                } else {
                                    '.'
                                };
                                let _ = write!(console, "{}", ch);
                            }
                            let _ = writeln!(console, "|");
                        }
                    } else {
                        for (i, chunk) in content.chunks(16).enumerate() {
                            let offset = i * 16;
                            let _ = write!(console, "{:07x}", offset);
                            for word in chunk.chunks(2) {
                                if word.len() == 2 {
                                    let val = u16::from_le_bytes([word[0], word[1]]);
                                    let _ = write!(console, " {:04x}", val);
                                } else {
                                    let _ = write!(console, " {:02x}", word[0]);
                                }
                            }
                            let _ = writeln!(console);
                        }
                    }
                }
                Err(_) => {
                    let _ = writeln!(console, "hexdump: unable to read file");
                }
            },
            Err(_) => {
                let _ = write!(console, "hexdump: {}: No such file or directory", filename);
            }
        });
    }
}

/// Lists files in current directory
#[allow(dead_code)]
pub fn ls(console: &mut Console, args: &Args) {
    let current_dir = DirHandle { start_cluster: 0 };
    crate::fs::volume::with_volume(|vol| {
        let _ = vol
            .read_dir(current_dir, |entry| {
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

/// Prints the elapsed time since boot in milliseconds.
#[allow(dead_code)]
pub fn time(console: &mut Console) {
    let _ = writeln!(console, "{} [ms]", timer::elapsed_ms());
}

/// Panics the system.
#[allow(dead_code)]
pub fn panic(_console: &mut Console) {
    panic!("user requested panic");
}

/// Prints the live thread painted stack high watermark
#[allow(dead_code)]
#[cfg(feature = "paint-stack")]
pub fn stacks(_console: &mut Console) {
    sched::stacks();
}

/// Creates an empty file
#[allow(dead_code)]
pub fn touch(console: &mut Console, args: &Args) {
    for filename in args.positionals.as_slice().iter() {
        let current_dir = DirHandle { start_cluster: 0 };
        crate::fs::volume::with_volume(|vol| match vol.create_empty_file(current_dir, filename) {
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
#[allow(dead_code)]
pub fn rm(console: &mut Console, args: &Args) {
    let current_dir = DirHandle { start_cluster: 0 };
    for filename in args.positionals.as_slice().iter() {
        crate::fs::volume::with_volume(|vol| match vol.delete_file(current_dir, filename) {
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

/// Write text to file
#[allow(dead_code)]
pub fn write(console: &mut Console, args: &Args) {
    let current_dir = DirHandle { start_cluster: 0 };
    let rest = args.rest.trim();
    let (filename, content) = match rest.find(' ') {
        Some(pos) => (&rest[..pos], &rest[pos + 1..]),
        None => {
            let _ = writeln!(console, "write: usage: write FILENAME text...");
            return;
        }
    };
    let mut data = String::from(content);
    data.push('\n');
    crate::fs::volume::with_volume(|vol| {
        match vol.write_file(current_dir, filename, data.as_bytes()) {
            Ok(_) => {}
            Err(fs_error) => {
                let msg = match fs_error {
                    FsError::DiskFull => "disk full",
                    FsError::DirFull => "directory full",
                    FsError::InvalidName => "invalid file name",
                    _ => "device error",
                };
                let _ = writeln!(console, "write: {}: {}", filename, msg);
            }
        }
    });
}

/// Prints an error message for an unrecognised command.
#[allow(dead_code)]
pub fn unknown(console: &mut Console, cmd: &str) {
    let _ = writeln!(console, "Unknown command: {}", cmd);
}
