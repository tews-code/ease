//! Built in shell for EASE
//!
//! This simple shell does not support line wrapping and assumes 80 columns of text for the screen width.
//! All reading is from STDIN and all output is to STDOUT.

// Host test builds (`cargo test --bin shell` on the host target) link std
// and the test harness, which brings its own `main`
#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

extern crate alloc;
mod args;
#[cfg(target_os = "none")]
mod commands;
mod line_editor;
mod vt_parse;

use crate::line_editor::{EditResult, LineEditor};
#[cfg(target_os = "none")]
use crate::vt_parse::{KeyEvent, vt_out};
use alloc::vec::Vec;
use ease_ulib::{ascii, fds};
#[cfg(target_os = "none")]
use ease_ulib::{print, println, put_char, read, write_all};

/// Terminal is assumed to have an 80 character width
const WIDTH: usize = 80;
/// The prompt printed for the EASE shell
const PROMPT: &str = "ease> ";
/// The usable length of a line, taking the prompt into account
const LINE_LEN: usize = WIDTH - PROMPT.len();
/// The number of bytes needed to redraw a line (in the worst case).
#[cfg(target_os = "none")]
const REDRAW_LINE_LEN: usize =
    1 + PROMPT.len() + LINE_LEN + vt_out::ERASE_TO_END_OF_LINE.len() + LINE_LEN; // CR, prompt, line, erase, backspace to prompt
/// Timeout in milliseconds before Esc is treated as a single Esc and not the start of a sequence
const ESC_TIMEOUT_MS: usize = 50;

/// Matches EditResults
#[cfg(target_os = "none")]
fn handle_key(write_buf: &mut Vec<u8>, line_editor: &mut LineEditor, key_event: KeyEvent) {
    match line_editor.process(key_event) {
        EditResult::Append(ch) => {
            let _ = put_char(ch);
        }
        EditResult::Complete => {
            let _ = put_char(ascii::CR);
            let _ = put_char(ascii::LF);
            if let Ok(cmd) = core::str::from_utf8(line_editor.line()) {
                execute(cmd);
            } else {
                println!("Invalid string.")
            }
            print!("{}", PROMPT);
            line_editor.reset();
        }
        EditResult::CursorMoveRight => {
            let _ = write_all(fds::STDOUT, vt_out::CURSOR_RIGHT);
        }
        EditResult::CursorMoveLeft => {
            let _ = put_char(ascii::BS);
        }
        EditResult::LineEdit => {
            let line = line_editor.line();
            let cursor_pos = line_editor.cursor();
            redraw_line(write_buf, line, cursor_pos);
        }
        EditResult::Reject => {}
    }
}

/// Redraw a line using VT sequences
#[cfg(target_os = "none")]
fn redraw_line(write_buf: &mut Vec<u8>, line: &[u8], cursor_pos: usize) {
    write_buf.clear();
    // 1. CR - move to the start of the line
    write_buf.push(ascii::CR);
    // 2. Re-print the prompt
    write_buf.extend_from_slice(PROMPT.as_bytes());
    // 3. Print the new line contents
    write_buf.extend_from_slice(line);
    // 4. Erase to end of line
    write_buf.extend_from_slice(vt_out::ERASE_TO_END_OF_LINE);
    // 5. Backspace to get cursor to its original position
    let backspace_count = line.len().saturating_sub(cursor_pos);
    write_buf.extend(core::iter::repeat_n(ascii::BS, backspace_count));
    // 6. Write this in one syscall
    let _ = write_all(fds::STDOUT, write_buf.as_slice());
}

/// Execute the given command
#[cfg(target_os = "none")]
fn execute(line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }

    let (cmd, rest) = match line.find(' ') {
        Some(pos) => (&line[..pos], line[pos + 1..].trim()),
        None => (line, ""),
    };

    match cmd {
        // "cat" => commands::cat(console, *wd, rest),
        "cd" => commands::cd(rest),
        "clear" => commands::clear(),
        // "date" => commands::date(console),
        "echo" => commands::echo(rest),
        "help" => commands::help(),
        // "hexdump" => commands::hexdump(console, *wd, rest),
        // "ls" => commands::ls(console, *wd, rest),
        // "mkdir" => commands::mkdir(console, *wd, rest),
        "panic" => commands::panic(),
        // "rm" => commands::rm(console, *wd, rest),
        // "rmdir" => commands::rmdir(console, *wd, rest),
        // #[cfg(feature = "paint-stack")]
        // "stacks" => commands::stacks(console),
        // "time" => {
        //     // Time another command: `rest` is the command line to run, so
        //     // re-enter the dispatcher around a wall-clock reading. Reuses
        //     // the whole dispatch (any command, its own args, wd changes).
        //     let start_ms = crate::kernel::timer::elapsed_ms();
        //     Self::execute(console, wd, rest);
        //     let elapsed_ms = crate::kernel::timer::elapsed_ms() - start_ms;
        //     let _ = writeln!(console, "time: {} ms", elapsed_ms);
        // }
        // "truncate" => commands::truncate(console, *wd, rest),
        // "touch" => commands::touch(console, *wd, rest),
        // "write" => commands::write(console, *wd, rest),
        _ => commands::unknown(cmd),
    }
}

/// The EASE built-in shell
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn main() {
    const READ_BUF_SIZE: usize = 8;
    // We use the heap for the shell
    ease_ulib::heap_init(8192).expect("should have heap space free");
    let mut editor = line_editor::LineEditor::new();
    let mut parser = vt_parse::EscSeqParser::new();
    let mut read_buf = [0u8; READ_BUF_SIZE];
    let mut write_buf = Vec::<u8>::with_capacity(REDRAW_LINE_LEN);
    print!("{}", PROMPT);
    loop {
        // Read from STDIN and block until at least one byte is available. If we are mid
        // sequence add a timeout
        let timeout = if parser.is_mid_sequence() {
            ESC_TIMEOUT_MS as u32
        } else {
            u32::MAX
        };
        let Ok(bytes_read) = read(fds::STDIN, &mut read_buf, timeout) else {
            continue;
        };
        // Check if this was a timeout
        if bytes_read == 0
            && let Some(key_event) = parser.esc_seq_timeout()
        {
            handle_key(&mut write_buf, &mut editor, key_event)
        }
        for &byte in &read_buf[..bytes_read] {
            if let Some(key_event) = parser.parse(byte) {
                handle_key(&mut write_buf, &mut editor, key_event);
            }
            while let Some(pushback_byte) = parser.pending_byte() {
                if let Some(key_event) = parser.parse(pushback_byte) {
                    handle_key(&mut write_buf, &mut editor, key_event);
                }
            }
        }
    }
}
