//! Moss Simple Shell

use core::fmt::Write;

use crate::kernel::collection::StackVec;
use crate::shell::console::Console;
use crate::shell::keyboard::Keyboard;
use crate::shell::line_editor::EditResult;

pub mod commands;
pub mod console;
pub mod font;
pub mod keyboard;
pub mod line_editor;
pub mod vt_parse;

// ASCII chars that are used for console and serial control
pub mod ascii {
    pub const BELL: u8 = 0x07;
    pub const BS: u8 = 0x08;
    pub const TAB: u8 = 0x09;
    pub const LF: u8 = 0x0A;
    pub const FF: u8 = 0x0C;
    pub const CR: u8 = 0x0D;
    pub const DEL: u8 = 0x7F;
}

use line_editor::LineEditor;

static PROMPT: &str = "moss> ";

/// Interactive shell with line editing, command history, and framebuffer console.
pub struct Shell {
    console: Console,
    keyboard: Keyboard,
    line_editor: LineEditor,
}

/// Struct holding flags and positional arguments
pub struct Args<'a> {
    flags: StackVec<u8, 8>,
    positionals: StackVec<&'a str, 8>,
}

impl Args<'_> {
    pub fn has_flag(&self, flag: u8) -> bool {
        self.flags.as_slice().contains(&flag)
    }
}

impl Shell {
    /// Creates a new shell with the given framebuffer console.
    pub fn new(console: Console) -> Self {
        Self {
            console,
            keyboard: Keyboard::default(),
            line_editor: LineEditor::new(),
        }
    }

    /// Runs the shell main loop. Polls keyboard, processes input, dispatches commands. Never returns.
    pub fn run(&mut self) -> ! {
        loop {
            let _ = write!(self.console, "{PROMPT}");
            loop {
                if let Some(event) = self
                    .keyboard
                    .poll(|| crate::drivers::uart::UartReader.read_byte())
                {
                    // Send event to line editor
                    match self.line_editor.process(event) {
                        EditResult::CursorMove | EditResult::LineEdit => {
                            self.console
                                .redraw_line(self.line_editor.line(), self.line_editor.cursor());
                            Self::uart_redraw_line(
                                self.line_editor.line(),
                                self.line_editor.cursor(),
                            );
                        }
                        EditResult::Append => {
                            self.console
                                .redraw_line(self.line_editor.line(), self.line_editor.cursor());
                            // Echo last typed character to UART
                            if let Some(&ch) = self.line_editor.line().last() {
                                crate::drivers::uart::direct_write_byte(ch);
                            }
                        }
                        EditResult::Complete => {
                            self.console.put_char(ascii::CR);
                            self.console.put_char(ascii::LF);
                            let cmd = core::str::from_utf8(self.line_editor.line())
                                .expect("should be UTF-8");
                            Self::execute(&mut self.console, cmd);
                            self.line_editor.reset();
                            self.console.reset_line();
                            break;
                        }
                        EditResult::Reject => {
                            self.console.put_char(ascii::BELL);
                        }
                    }
                } else {
                    // Sleep until next tick
                    crate::hal::wait_for_interrupt();
                }
            }
        }
    }

    fn parse(rest: &str) -> Args<'_> {
        let mut flags = StackVec::<u8, 8>::new();
        let mut positionals = StackVec::<&str, 8>::new();
        for token in rest.split_whitespace() {
            if let Some(flag_chars) = token.strip_prefix('-') {
                for ch in flag_chars.bytes() {
                    let _ = flags.push(ch);
                }
            } else {
                // Positional argument
                let _ = positionals.push(token);
            }
        }
        Args { flags, positionals }
    }

    fn execute(console: &mut Console, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }

        let (cmd, rest) = match line.find(' ') {
            Some(pos) => (&line[..pos], line[pos + 1..].trim()),
            None => (line, ""),
        };

        let args = Self::parse(rest);

        match cmd {
            "cat" => commands::cat(console, &args),
            "clear" => commands::clear(console),
            "echo" => commands::echo(console, &args),
            "help" => commands::help(console),
            "ls" => commands::ls(console, &args),
            "panic" => commands::panic(console),
            "time" => commands::time(console),
            "touch" => commands::touch(console, &args),
            "rm" => commands::rm(console, &args),
            _ => commands::unknown(console, cmd),
        }
    }

    /// Redraws the current input line on the UART serial terminal.
    ///
    /// 1. CR — move cursor to start of line
    /// 2. Print prompt
    /// 3. Print line contents
    /// 4. Erase from cursor to end of line (ANSI escape `\x1b[K`)
    /// 5. Reposition cursor with backspaces
    fn uart_redraw_line(line: &[u8], cursor: usize) {
        use crate::drivers::uart::direct_write_byte;

        // 1. CR — move to start of line
        direct_write_byte(ascii::CR);
        // 2. Print prompt
        for &b in PROMPT.as_bytes() {
            direct_write_byte(b);
        }
        // 3. Print line contents
        for &b in line {
            direct_write_byte(b);
        }
        // 4. Erase to end of line
        direct_write_byte(0x1b);
        direct_write_byte(b'[');
        direct_write_byte(b'K');
        // 5. Reposition cursor
        for _ in 0..line.len().saturating_sub(cursor) {
            direct_write_byte(ascii::BS);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn parse_empty_input() {
        let args = Shell::parse("");
        assert_eq!(args.flags.len(), 0);
        assert_eq!(args.positionals.len(), 0);
    }

    #[test_case]
    fn parse_single_positional() {
        let args = Shell::parse("HELLO.TXT");
        assert_eq!(args.flags.len(), 0);
        assert_eq!(args.positionals.len(), 1);
        assert_eq!(args.positionals[0], "HELLO.TXT");
    }

    #[test_case]
    fn parse_multiple_positionals() {
        let args = Shell::parse("FOO.TXT BAR.TXT");
        assert_eq!(args.positionals.len(), 2);
        assert_eq!(args.positionals[0], "FOO.TXT");
        assert_eq!(args.positionals[1], "BAR.TXT");
    }

    #[test_case]
    fn parse_single_flag() {
        let args = Shell::parse("-l");
        assert_eq!(args.flags.len(), 1);
        assert!(args.has_flag(b'l'));
        assert_eq!(args.positionals.len(), 0);
    }

    #[test_case]
    fn parse_multiple_separate_flags() {
        let args = Shell::parse("-l -a");
        assert_eq!(args.flags.len(), 2);
        assert!(args.has_flag(b'l'));
        assert!(args.has_flag(b'a'));
    }

    #[test_case]
    fn parse_combined_flags() {
        let args = Shell::parse("-la");
        assert_eq!(args.flags.len(), 2);
        assert!(args.has_flag(b'l'));
        assert!(args.has_flag(b'a'));
    }

    #[test_case]
    fn parse_flags_and_positionals_mixed() {
        let args = Shell::parse("-l HELLO.TXT -a");
        assert!(args.has_flag(b'l'));
        assert!(args.has_flag(b'a'));
        assert_eq!(args.positionals.len(), 1);
        assert_eq!(args.positionals[0], "HELLO.TXT");
    }

    #[test_case]
    fn has_flag_returns_false_for_absent_flag() {
        let args = Shell::parse("-l");
        assert!(!args.has_flag(b'a'));
    }

    #[test_case]
    fn parse_whitespace_only() {
        let args = Shell::parse("   ");
        assert_eq!(args.flags.len(), 0);
        assert_eq!(args.positionals.len(), 0);
    }
}
