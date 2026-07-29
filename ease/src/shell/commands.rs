//! Shell commands

use alloc::string::String;

use core::fmt::Write;
use core::ops::ControlFlow;

use crate::fs::file;
use crate::fs::{Dir, FsError};
#[cfg(feature = "paint-stack")]
use crate::kernel::sched;
use crate::kernel::timer;
use crate::shell::{Args, Console, ascii};

/// Reads file content to console
#[allow(dead_code)]
pub fn cat(console: &mut Console, args: &Args) {
    let dir = Dir::Root;
    for filename in args.positionals.as_slice().iter() {
        let mut file = match file::open(file::Access::Read, dir, filename) {
            Ok(file_handle) => file_handle,
            Err(fs_error) => {
                let _ = writeln!(
                    console,
                    "cat: {}: could not open file - {:?}",
                    filename, fs_error
                );
                break;
            }
        };
        let mut buf = [0u8; 256];
        let mut remainder_byte_count = 0;
        loop {
            let num_bytes = match file::read_at(&mut file, &mut buf[remainder_byte_count..]) {
                Ok(num_bytes) => num_bytes,
                Err(fs_error) => {
                    let _ = writeln!(
                        console,
                        "cat: {}: Error reading file {:?}",
                        filename, fs_error
                    );
                    break;
                }
            };
            if num_bytes == 0 {
                break;
            }
            // Print out the bytes that have arrived so far
            let filled_bytes = remainder_byte_count + num_bytes;
            match core::str::from_utf8(&buf[..filled_bytes]) {
                Ok(text) => {
                    let _ = write!(console, "{}", text);
                    remainder_byte_count = 0;
                }
                Err(e) => {
                    if e.error_len().is_some() {
                        let _ = writeln!(console, "cat: file is not valid text");
                        break;
                    } else {
                        let valid_bytes = e.valid_up_to();
                        let valid_text = core::str::from_utf8(&buf[..valid_bytes])
                            .expect("already validated that these bytes");
                        let _ = write!(console, "{}", valid_text);
                        remainder_byte_count = filled_bytes - valid_bytes;
                        // Copy the remainder to the beginning for the next read
                        buf.copy_within(valid_bytes..filled_bytes, 0);
                    }
                }
            }
        }
        file::close(&file).expect("should not have a file error closing a file with Access::Read");
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
    const READ_BUF_SIZE: usize = 383;
    const LINE_LEN: usize = 16;

    /// Print one line: the offset, `byte_len` bytes of `line`, and (for `-C`)
    /// the ASCII column. Columns past `byte_len` are padded so a short final
    /// line still aligns.
    fn emit(
        console: &mut Console,
        args: &Args,
        file_offset: usize,
        line: &[u8; LINE_LEN],
        byte_len: usize,
    ) {
        if args.has_flag(b'C') {
            // byte-by-byte with ASCII column
            let _ = write!(console, "{:08x}  ", file_offset);

            // Print hex bytes, padding the columns past the real data.
            for (j, _) in line.iter().enumerate().take(LINE_LEN) {
                if j < byte_len {
                    let _ = write!(console, "{:02x} ", line[j]);
                } else {
                    let _ = write!(console, "   ");
                }
                if j == 7 {
                    let _ = write!(console, " ");
                }
            }

            // Print ASCII column for the real bytes only.
            let _ = write!(console, " |");
            for &byte in &line[..byte_len] {
                let ch = if byte.is_ascii_graphic() || byte == b' ' {
                    byte as char
                } else {
                    '.'
                };
                let _ = write!(console, "{}", ch);
            }
            let _ = writeln!(console, "|");
        } else {
            let _ = write!(console, "{:07x}", file_offset);
            for word in line[..byte_len].chunks(2) {
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

    // Open the file
    for filename in args.positionals.as_slice().iter() {
        let dir = Dir::Root; // For now all files in root
        let mut file = match crate::fs::file::open(file::Access::Read, dir, filename) {
            Ok(file) => file,
            Err(fs_error) => {
                let _ = write!(
                    console,
                    "hexdump: {}: Error opening file - {:?}.",
                    filename, fs_error
                );
                break;
            }
        };
        // The 16-byte line accumulator lives across reads, so a line that
        // straddles a read boundary is stitched back together.
        let mut line = [0u8; LINE_LEN]; // The line currently being assembled.
        let mut line_len = 0; // How many of those 16 slots are filled so far (0–16).
        let mut file_offset = 0; // Absolute offset of the line being built (i.e. bytes already emitted).
        // Read buffer is separate to the line accumulator.
        let mut read_buf = [0u8; READ_BUF_SIZE];
        loop {
            let num_bytes_read = match file::read_at(&mut file, &mut read_buf) {
                Ok(num_bytes_read) => num_bytes_read,
                Err(fs_error) => {
                    let _ = writeln!(
                        console,
                        "hexdump: {}: Unable to read file - {:?}.",
                        filename, fs_error
                    );
                    file::close(&file)
                        .expect("should not have an error closing file with Access::Read access");
                    break;
                }
            };
            if num_bytes_read == 0 {
                // End of file
                file::close(&file)
                    .expect("should not have an error closing file with Access::Read access");
                break;
            }
            // Pour the bytes just read into the line, flushing whenever it fills.
            let mut buf_pos = 0;
            loop {
                let bytes = (LINE_LEN - line_len).min(num_bytes_read - buf_pos);
                line[line_len..line_len + bytes]
                    .copy_from_slice(&read_buf[buf_pos..buf_pos + bytes]);
                // Advance cursors.
                buf_pos += bytes;
                line_len += bytes;
                if line_len == LINE_LEN {
                    emit(console, args, file_offset, &line, LINE_LEN);
                    file_offset += LINE_LEN;
                    line_len = 0;
                }
                if buf_pos == num_bytes_read {
                    break;
                }
            }
        }
        // Flush the final short line, if the file didn't end on a 16-byte boundary.
        if line_len > 0 {
            emit(console, args, file_offset, &line, line_len);
        }
    }
}

/// Lists files in current directory
#[allow(dead_code)]
pub fn ls(console: &mut Console, args: &Args) {
    let current_dir = Dir::Root;
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
        let current_dir = Dir::Root;
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
    let current_dir = Dir::Root;
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
    let current_dir = Dir::Root;
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
