//! Moss Simple Shell

use core::fmt::Write;

use crate::hal::Reader;
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
                        EditResult::CursorMove | EditResult::LineEdit | EditResult::Append => {
                            self.console
                                .redraw_line(self.line_editor.line(), self.line_editor.cursor());
                        }
                        EditResult::Complete => {
                            self.console.put_char(ascii::LF);
                            self.console.put_char(ascii::CR);
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

    fn execute(console: &mut Console, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }

        // Split on first space
        let (cmd, args) = match line.find(' ') {
            Some(pos) => (&line[..pos], line[pos + 1..].trim()),
            None => (line, ""),
        };

        match cmd {
            "clear" => commands::clear(console),
            "echo" => commands::echo(console, args),
            "help" => commands::help(console),
            "ls" => commands::ls(console),
            "time" => commands::time(console),
            "panic" => commands::panic(console),
            _ => commands::unknown(console, cmd),
        }
    }
}
