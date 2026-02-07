//! EASE Simple Shell

#![allow(dead_code)]

pub mod commands;
pub mod line_editor;

use crate::hal::ascii;
use crate::input::keyboard::{Keyboard, UartKeyboard};
use crate::shell::line_editor::LineDisplayAction;
use crate::{print, println};

use line_editor::LineEditor;

static PROMPT: &str = "ease> ";

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
            LineDisplayAction::None => {}
            LineDisplayAction::Echo(b) => print!("{}", b as char),
            LineDisplayAction::Enter => println!(),
            LineDisplayAction::Bell => print!("{}", ascii::BELL as char),
            LineDisplayAction::Backspace { s } => {
                print!("{}{} {}", ascii::BS as char, s, ascii::BS as char);
                // Move cursor back by length of str
                for _ in 0..s.len() {
                    print!("{}", ascii::BS as char);
                }
            }
            LineDisplayAction::Redraw { s, n } => {
                print!("{s}");
                let spaces = n.saturating_sub(s.len());
                for _ in 0..spaces {
                    print!(" ");
                }
                // Return the cursor back to after the new string
                for _ in 0..(s.len() + spaces).saturating_sub(1) {
                    print!("{}", ascii::BS as char);
                }
            }
            LineDisplayAction::RedrawLine { s, n } => {
                Self::handle_display(LineDisplayAction::ClearLine(n));
                print!("{s}");
            }
            LineDisplayAction::ClearLine(n) => {
                print!("{}", ascii::CR as char);
                for _ in 0..n + PROMPT.len() {
                    print!(" ");
                }
                // Move to start of line and re-prompt
                print!("{}{}", ascii::CR as char, PROMPT);
            }
            LineDisplayAction::CursorLeft => {
                print!("{}", ascii::BS as char);
            }
            LineDisplayAction::CursorRight(ch) => {
                // Line editor would not ask this if there was nowhere to go right
                print!("{}", ch as char);
            }
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

        match cmd {
            "help" => commands::help(),
            "clear" => commands::clear(),
            "echo" => commands::echo(args),
            "time" => commands::time(),
            _ => commands::unknown(cmd),
        }
    }
}
