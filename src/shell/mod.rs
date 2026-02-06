//! EASE Simple Shell

pub mod commands;
pub mod line_editor;

use crate::shell::line_editor::LineDisplayAction;
use crate::{drivers::console::CONSOLE, input::keyboard::KeyEvent};
use crate::hal::Writer;
use crate::input::escape::Key;
use crate::input::keyboard::{Keyboard, UartKeyboard};
use crate::io::UartWriter;
use crate::kernel::collection::Vec;
use crate::{print, println};

use line_editor::LineEditor;

static PROMPT: &str = "ease> ";

pub const ASCII_BELL: u8 = 0x07;
pub const ASCII_BS: u8 = 0x08;

pub struct Shell {
    keyboard: UartKeyboard,
    line_editor: LineEditor,
}

impl Shell {
    pub const fn new() -> Self {
        Self {
            keyboard: UartKeyboard::new(),
            line_editor: LineEditor::new(),
        }
    }

    pub fn run(&mut self) -> ! {
        loop {
            print!("{PROMPT}");
            loop {
                if let Some(event) = self.keyboard.poll() {
                    // Send event to line editor
                    let (result, action) = self.line_editor.process(event);
                    // Handle display action from line editor
                    Self::handle_display(action);
                    // If the line is complete, execute and break
                    if let Some(line) = result {
                        Self::execute(line);
                        break;
                    }
                }
            }
        }
    }

    fn handle_display(action: LineDisplayAction) {
        match action {
            LineDisplayAction::None => {},
            LineDisplayAction::Echo(b) => print!("{}", b as char),
            LineDisplayAction::Enter => println!(),
            LineDisplayAction::Bell => CONSOLE.lock().write_char(ASCII_BELL), // Serial does not support bell. So console print only
            LineDisplayAction::Backspace { s } => {
                // Serial device
                UartWriter.write_byte(ASCII_BS); // Backspace - move cursor left
                UartWriter.write_byte(b' '); // Space - erase character
                UartWriter.write_byte(ASCII_BS); // Backspace - move cursor left again

                // Console
                let mut console = CONSOLE.lock();
                console.write_char(ASCII_BS);
                console.write_char(b' ');
                console.write_char(ASCII_BS);

                if !s.is_empty() {
                    Self::handle_display(LineDisplayAction::Redraw{ s })
                }
            },
            LineDisplayAction::Redraw { s } => {
                for &b in s {
                    print!("{}", b as char);
                }
                // Move cursor back to correct position
                for _ in 0..s.len() - 1 {
                    print!("\x08");  // Or use console.write_char
                }

            },
            LineDisplayAction::RedrawLine { s } => {
                Self::handle_display(LineDisplayAction::ClearLine(s.len()));
                print!("{PROMPT}");
                for &b in s {
                    print!("{}", b as char);
                }
            },
            LineDisplayAction::ClearLine(n) => {
                print!("\r{PROMPT}");
                for _ in 0..n { print!(" "); }
                // Move to start of line and re-prompt
                print!("\r");
            },
            LineDisplayAction::CursorLeft => {
                let ( cursor_x, _ ) = CONSOLE.lock().cursor_position();
                if cursor_x > PROMPT.len() {
                    // Serial device
                    UartWriter.write_byte(ASCII_BS);
                    // Console
                    let mut console = CONSOLE.lock();
                    console.write_char(ASCII_BS);
                }
            },
            LineDisplayAction::CursorRight(ch) => {
                // Line editor would not ask this if there was nowhere to go right
                UartWriter.write_byte(ch); // Rewrite ch to move cursor right
                let mut console = CONSOLE.lock();
                console.write_char(ch)
            },
        }
    }

    pub fn execute(line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }

        // Split on first space
        let (cmd, args) = match line.find(' ') {
            Some(pos) => (&line[..pos], line[pos + 1..].trim()),
            None => (line, ""),
        };

        let mut console = CONSOLE.lock();

        match cmd {
            "help" => commands::help(&mut console),
            "clear" => commands::clear(&mut console),
            "echo" => commands::echo(&mut console, args),
            "time" => commands::time(&mut console),
            _ => commands::unknown(&mut console, cmd),
        }
    }
}
