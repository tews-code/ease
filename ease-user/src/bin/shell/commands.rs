//! Shell commands

// use alloc::string::String;

// use core::fmt::Write;
// use core::ops::ControlFlow;

// use crate::fs::file;
// use crate::fs::{Dir, Error};
// #[cfg(feature = "paint-stack")]
// use crate::kernel::sched;
// use crate::kernel::timer;

use crate::args::Args;
use crate::vt_parse::vt_out;
use ease_ulib::{chdir, fds, println, write_all};

// /// Reads file content to console
// #[allow(dead_code)]
// pub fn cat(console: &mut Console, dir: Dir, arg_str: &str) {
//     let args = match Args::parse(arg_str, &[], &[]) {
//         Ok(args) => args,
//         Err(e) => {
//             println!("cat: invalid arguments - {:?}", e);
//             return;
//         }
//     };
//     for filename in args.positionals.as_slice().iter() {
//         let file = match file::open(file::Access::Read, dir, filename) {
//             Ok(file_handle) => file_handle,
//             Err(fs_error) => {
//                 let _ = writeln!(
//                     console,
//                     "cat: {}: could not open file - {:?}",
//                     filename, fs_error
//                 );
//                 break;
//             }
//         };
//         let mut buf = [0u8; 256];
//         let mut remainder_byte_count = 0;
//         loop {
//             let num_bytes = match file::read_at(&file, &mut buf[remainder_byte_count..]) {
//                 Ok(num_bytes) => num_bytes,
//                 Err(fs_error) => {
//                     let _ = writeln!(
//                         console,
//                         "cat: {}: Error reading file {:?}",
//                         filename, fs_error
//                     );
//                     break;
//                 }
//             };
//             if num_bytes == 0 {
//                 break;
//             }
//             // Print out the bytes that have arrived so far
//             let filled_bytes = remainder_byte_count + num_bytes;
//             match core::str::from_utf8(&buf[..filled_bytes]) {
//                 Ok(text) => {
//                     let _ = write!(console, "{}", text);
//                     remainder_byte_count = 0;
//                 }
//                 Err(e) => {
//                     if e.error_len().is_some() {
//                         println!("cat: file is not valid text");
//                         break;
//                     } else {
//                         let valid_bytes = e.valid_up_to();
//                         let valid_text = core::str::from_utf8(&buf[..valid_bytes])
//                             .expect("already validated that these bytes");
//                         let _ = write!(console, "{}", valid_text);
//                         remainder_byte_count = filled_bytes - valid_bytes;
//                         // Copy the remainder to the beginning for the next read
//                         buf.copy_within(valid_bytes..filled_bytes, 0);
//                     }
//                 }
//             }
//         }
//     }
// }

/// Clears the screen.
pub fn clear() {
    let _ = write_all(fds::STDOUT, vt_out::CURSOR_HOME);
    let _ = write_all(fds::STDOUT, vt_out::CLEAR_SCREEN);
}

/// Change working directory
pub fn cd(arg_str: &str) {
    let mut args = match Args::parse(arg_str, &[], &[]) {
        Ok(args) => args,
        Err(e) => {
            println!("cd: invalid arguments - {:?}", e);
            return;
        }
    };
    if args.positionals.len() > 1 {
        println!("cd: too many arguments");
        return;
    }
    if let Some(dirname) = args.positionals.pop() {
        match chdir(dirname) {
            Ok(_) => {}
            Err(e) => {
                println!("cd: error changing directory - {:?}", e);
            }
        }
    } else {
        println!("cd: need directory name");
    }
}

/// Prints arguments to the console.
pub fn echo(arg_str: &str) {
    println!("{}", arg_str);
}

/// Prints the list of available commands.
pub fn help() {
    println!("Available commands:");
    println!("  cd       - Change directory");
    // println!("  cat      - Read file content to screen");
    println!("  clear    - Clear the screen");
    println!("  echo     - Print arguments");
    println!("  help     - Show this help");
    // println!("  hexdump  - Raw file output");
    // println!("  ls       - List files in directory");
    // #[cfg(feature = "paint-stack")]
    // println!("  stacks   - Print painted kernel thread stacks");
    // println!("  date     - Show system time since boot [ms]");
    // println!("  time     - Time another command [ms]");
    // println!("  truncate - Truncate file");
    // println!("  touch    - Create empty file");
    // println!("  rm       - Delete file");
    // println!("  write    - Write text to file");
}

// /// Shows the raw file details in hex format
// ///
// /// - Supports -C argument
// /// - Supports -s bytes for absolute byte seek
// #[allow(dead_code)]
// pub fn hexdump(console: &mut Console, dir: Dir, arg_str: &str) {
//     // Print one line: the offset, `byte_len` bytes of `line`, and (for `-C`)
//     // the ASCII column. Columns past `byte_len` are padded so a short final
//     // line still aligns.
//     fn emit(
//         console: &mut Console,
//         args: &Args,
//         file_offset: usize,
//         line: &[u8; LINE_LEN],
//         byte_len: usize,
//     ) {
//         if args.has_flag("C") {
//             // byte-by-byte with ASCII column
//             let _ = write!(console, "{:08x}  ", file_offset);
//
//             // Print hex bytes, padding the columns past the real data.
//             for (j, _) in line.iter().enumerate().take(LINE_LEN) {
//                 if j < byte_len {
//                     let _ = write!(console, "{:02x} ", line[j]);
//                 } else {
//                     let _ = write!(console, "   ");
//                 }
//                 if j == 7 {
//                     let _ = write!(console, " ");
//                 }
//             }
//
//             // Print ASCII column for the real bytes only.
//             let _ = write!(console, " |");
//             for &byte in &line[..byte_len] {
//                 let ch = if byte.is_ascii_graphic() || byte == b' ' {
//                     byte as char
//                 } else {
//                     '.'
//                 };
//                 let _ = write!(console, "{}", ch);
//             }
//             println!("|");
//         } else {
//             let _ = write!(console, "{:07x}", file_offset);
//             for word in line[..byte_len].chunks(2) {
//                 if word.len() == 2 {
//                     let val = u16::from_le_bytes([word[0], word[1]]);
//                     let _ = write!(console, " {:04x}", val);
//                 } else {
//                     let _ = write!(console, " {:02x}", word[0]);
//                 }
//             }
//             let _ = writeln!(console);
//         }
//     }
//
//     let args = match Args::parse(arg_str, &["C"], &["s"]) {
//         Ok(args) => args,
//         Err(e) => {
//             println!("hexdump: invalid arguments - {:?}", e);
//             return;
//         }
//     };
//     const READ_BUF_SIZE: usize = 383;
//     const LINE_LEN: usize = 16;
//     // Seek as needed
//     let seek_bytes = if let Some(seek_str) = args.flag_value("s") {
//         match seek_str.parse::<u32>() {
//             Ok(b) => b,
//             Err(e) => {
//                 println!("hexdump: invalid argument value - {:?}", e);
//                 return;
//             }
//         }
//     } else {
//         0
//     };
//
//     // Open the file
//     for filename in args.positionals.as_slice().iter() {
//         let file = match crate::fs::file::open(file::Access::Read, dir, filename) {
//             Ok(file) => file,
//             Err(fs_error) => {
//                 let _ = write!(
//                     console,
//                     "hexdump: {}: Error opening file - {:?}.",
//                     filename, fs_error
//                 );
//                 break;
//             }
//         };
//         // Seek to the desired byte position
//         match file::lseek(&file, seek_bytes) {
//             Ok(_) => {}
//             Err(e) => {
//                 let _ = write!(console, "hexdump: {}: Error seeking - {:?}.", filename, e);
//                 break;
//             }
//         }
//         // The 16-byte line accumulator lives across reads, so a line that
//         // straddles a read boundary is stitched back together.
//         let mut line = [0u8; LINE_LEN]; // The line currently being assembled.
//         let mut line_len = 0; // How many of those 16 slots are filled so far (0–16).
//         let mut file_offset = 0; // Absolute offset of the line being built (i.e. bytes already emitted).
//         // Read buffer is separate to the line accumulator.
//         let mut read_buf = [0u8; READ_BUF_SIZE];
//         loop {
//             let num_bytes_read = match file::read_at(&file, &mut read_buf) {
//                 Ok(num_bytes_read) => num_bytes_read,
//                 Err(fs_error) => {
//                     let _ = writeln!(
//                         console,
//                         "hexdump: {}: Unable to read file - {:?}.",
//                         filename, fs_error
//                     );
//                     break;
//                 }
//             };
//             if num_bytes_read == 0 {
//                 // End of file
//                 break;
//             }
//             // Pour the bytes just read into the line, flushing whenever it fills.
//             let mut buf_pos = 0;
//             loop {
//                 let bytes = (LINE_LEN - line_len).min(num_bytes_read - buf_pos);
//                 line[line_len..line_len + bytes]
//                     .copy_from_slice(&read_buf[buf_pos..buf_pos + bytes]);
//                 // Advance cursors.
//                 buf_pos += bytes;
//                 line_len += bytes;
//                 if line_len == LINE_LEN {
//                     emit(console, &args, file_offset, &line, LINE_LEN);
//                     file_offset += LINE_LEN;
//                     line_len = 0;
//                 }
//                 if buf_pos == num_bytes_read {
//                     break;
//                 }
//             }
//         }
//         // Flush the final short line, if the file didn't end on a 16-byte boundary.
//         if line_len > 0 {
//             emit(console, &args, file_offset, &line, line_len);
//         }
//     }
// }
/*
/// Lists files in current directory
#[allow(dead_code)]
pub fn ls(console: &mut Console, dir: Dir, arg_str: &str) {
    let args = match Args::parse(arg_str, &["l"], &[]) {
        Ok(args) => args,
        Err(e) => {
            println!("ls: invalid arguments - {:?}", e);
            return;
        }
    };
    crate::fs::volume::with_volume(|vol| {
        let _ = vol
            .read_dir(dir, |entry| {
                let name = entry.filename();
                if args.has_flag("l") {
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

        if !args.has_flag("l") {
            let _ = writeln!(console);
        }
    });
}*/

// /// Creates a subdirectory
// #[allow(dead_code)]
// pub fn mkdir(console: &mut Console, dir: Dir, arg_str: &str) {
//     let args = match Args::parse(arg_str, &[], &[]) {
//         Ok(args) => args,
//         Err(e) => {
//             println!("mkdir: invalid arguments - {:?}", e);
//             return;
//         }
//     };
//     for dirname in args.positionals.as_slice().iter() {
//         match file::mkdir(dir, dirname) {
//             Ok(()) => {}
//             Err(fs_error) => {
//                 let msg = match fs_error {
//                     fs::Error::DuplicateDirName => "directory already exists",
//                     fs::Error::InvalidName => "invalid directory name",
//                     _ => "device error",
//                 };
//                 println!("mkdir: {}: {}", dirname, msg);
//             }
//         }
//     }
// }

/// Panics the system.
pub fn panic() {
    panic!("user requested panic");
}

// /// Deletes a file
// #[allow(dead_code)]
// pub fn rm(console: &mut Console, dir: Dir, arg_str: &str) {
//     let args = match Args::parse(arg_str, &[], &[]) {
//         Ok(args) => args,
//         Err(e) => {
//             println!("rm: invalid arguments - {:?}", e);
//             return;
//         }
//     };
//     for filename in args.positionals.as_slice().iter() {
//         match file::rm(dir, filename) {
//             Ok(()) => {}
//             Err(fs_error) => {
//                 let msg = match fs_error {
//                     fs::Error::InvalidName => "invalid file name",
//                     fs::Error::NotFound => "file not found",
//                     _ => "device error",
//                 };
//                 println!("rm: {}: {}", filename, msg);
//             }
//         }
//     }
// }

// /// Deletes an empty directory
// #[allow(dead_code)]
// pub fn rmdir(console: &mut Console, dir: Dir, arg_str: &str) {
//     let args = match Args::parse(arg_str, &[], &[]) {
//         Ok(args) => args,
//         Err(e) => {
//             println!("rmdir: invalid arguments - {:?}", e);
//             return;
//         }
//     };
//     for dirname in args.positionals.as_slice().iter() {
//         match file::rmdir(dir, dirname) {
//             Ok(()) => {}
//             Err(fs_error) => {
//                 let msg = match fs_error {
//                     fs::Error::DirectoryNotEmpty => "directory not empty",
//                     fs::Error::DirectoryIsCurrent => "cannot remove '.' or '..'",
//                     fs::Error::NotADirectory => "not a directory",
//                     fs::Error::InvalidName => "invalid directory name",
//                     fs::Error::NotFound => "directory not found",
//                     _ => "device error",
//                 };
//                 println!("rmdir: {}: {}", dirname, msg);
//             }
//         }
//     }
// }
/*
/// Prints the live thread painted stack high watermark
#[cfg(feature = "paint-stack")]
#[allow(dead_code)]
pub fn stacks(_console: &mut Console) {
    sched::stacks();
}*/

// /// Prints the elapsed time since boot in milliseconds.
// #[allow(dead_code)]
// pub fn date(console: &mut Console) {
//     println!("{} [ms]", timer::elapsed_ms());
// }

// /// Creates an empty file
// #[allow(dead_code)]
// pub fn touch(console: &mut Console, dir: Dir, arg_str: &str) {
//     let args = match Args::parse(arg_str, &[], &[]) {
//         Ok(args) => args,
//         Err(e) => {
//             println!("touch: invalid arguments - {:?}", e);
//             return;
//         }
//     };
//     for filename in args.positionals.as_slice().iter() {
//         crate::fs::volume::with_volume(|vol| match vol.create_empty_file(dir, filename) {
//             Ok(_) => {}
//             Err(fs_error) => {
//                 let msg = match fs_error {
//                     fs::Error::DirFull => "directory full",
//                     fs::Error::InvalidName => "invalid file name",
//                     _ => "device error",
//                 };
//                 println!("touch: {}: {}", filename, msg);
//             }
//         });
//     }
// }

// /// Truncate a file to zero bytes
// #[allow(dead_code)]
// pub fn truncate(console: &mut Console, dir: Dir, arg_str: &str) {
//     let args = match Args::parse(arg_str, &[], &[]) {
//         Ok(args) => args,
//         Err(e) => {
//             println!("truncate: invalid arguments - {:?}", e);
//             return;
//         }
//     };
//     for filename in args.positionals.as_slice().iter() {
//         match file::truncate(dir, filename) {
//             Ok(()) => {}
//             Err(fs_error) => {
//                 let msg = match fs_error {
//                     fs::Error::InvalidName => "invalid file name",
//                     fs::Error::NotFound => "file not found",
//                     _ => "device error",
//                 };
//                 println!("truncate: {}: {}", filename, msg);
//             }
//         }
//     }
// }

// /// Write text to file
// #[allow(dead_code)]
// pub fn write(console: &mut Console, dir: Dir, arg_str: &str) {
//     let args = match Args::parse(arg_str, &[], &[]) {
//         Ok(args) => args,
//         Err(e) => {
//             println!("write: invalid arguments - {:?}", e);
//             return;
//         }
//     };
//     let rest = args.rest.trim();
//     let (filename, content) = match rest.find(' ') {
//         Some(pos) => (&rest[..pos], &rest[pos + 1..]),
//         None => {
//             println!("write: usage: write FILENAME text...");
//             return;
//         }
//     };
//     let mut data = String::from(content);
//     data.push('\n');
//     // Ensure the file exists
//     if let Err(e) = file::touch(dir, filename) {
//         println!("write: {}: error on touch: {:?}", filename, e);
//         return;
//     }
//     // Truncate the file if necessary
//     if let Err(e) = file::truncate(dir, filename) {
//         println!("write: {}: error on truncate: {:?}", filename, e);
//         return;
//     }
//     // Open the file for writing
//     // Because we truncated the position is zero
//     let file = match file::open(file::Access::Write, dir, filename) {
//         Ok(file) => file,
//         Err(e) => {
//             println!("write: {}: error on open: {:?}", filename, e);
//             return;
//         }
//     };
//     // Write by looping over buffer
//     let write_buf_len = 5;
//     for chunk in data.as_bytes().chunks(write_buf_len) {
//         if let Err(e) = file::write_at(&file, chunk) {
//             println!("write: {}: error on write: {:?}", filename, e);
//             return;
//         }
//     }
//     if let Err(e) = file.close() {
//         println!("write: {}: error on close: {:?}", filename, e);
//     }
// }

/// Prints an error message for an unrecognised command.
pub fn unknown(cmd: &str) {
    println!("Unknown command: {}", cmd);
}
