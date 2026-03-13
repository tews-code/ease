//! EASE Simple Shell

#![allow(dead_code)]

pub mod commands;
pub mod line_editor;

use crate::drivers::uart::UartReader;
use crate::hal::ascii;
use crate::input::keyboard::{Keyboard, KeyboardInput};
use crate::kernel::collection::StackVec;
use crate::shell::line_editor::{LINE_LEN, LineDisplayAction};
use crate::{print, println};

use line_editor::LineEditor;

const DISPLAY_BUF_SIZE: usize = LINE_LEN * 4;

static PROMPT: &str = "ease> ";

pub struct Shell {
    keyboard: KeyboardInput<UartReader>,
    line_editor: LineEditor,
}

impl Shell {
    pub const fn new() -> Self {
        Self {
            keyboard: KeyboardInput::new(UartReader),
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
                } else {
                    // Sleep until next tick
                    unsafe {
                        core::arch::asm!("wfi");
                    }
                }
            }
        }
    }

    fn fill_bs(buf: &mut StackVec<u8, DISPLAY_BUF_SIZE>, count: usize) {
        for _ in 0..count {
            let _ = buf.push(ascii::BS);
        }
    }

    fn fill_spaces(buf: &mut StackVec<u8, DISPLAY_BUF_SIZE>, count: usize) {
        for _ in 0..count {
            let _ = buf.push(b' ');
        }
    }

    fn fill_str(buf: &mut StackVec<u8, DISPLAY_BUF_SIZE>, s: &str) {
        for b in s.bytes() {
            let _ = buf.push(b);
        }
    }

    fn fill_clear_line(buf: &mut StackVec<u8, DISPLAY_BUF_SIZE>, n: usize, c: usize) {
        Self::fill_bs(buf, c);
        Self::fill_spaces(buf, n);
        Self::fill_bs(buf, n);
    }

    fn handle_display(action: LineDisplayAction) {
        match action {
            LineDisplayAction::None => {}
            LineDisplayAction::Echo(b) => print!("{}", b as char),
            LineDisplayAction::Enter => println!(),
            LineDisplayAction::Bell => print!("{}", ascii::BELL as char),
            LineDisplayAction::Backspace { s } => {
                let mut buf: StackVec<u8, DISPLAY_BUF_SIZE> = StackVec::new();
                let _ = buf.push(ascii::BS);
                Self::fill_str(&mut buf, s);
                let _ = buf.push(b' ');
                Self::fill_bs(&mut buf, s.len() + 1);
                print!("{}", buf.as_str().expect("should be valid UTF-8"));
            }
            LineDisplayAction::Redraw { s, n } => {
                let mut buf: StackVec<u8, DISPLAY_BUF_SIZE> = StackVec::new();
                Self::fill_str(&mut buf, s);
                let spaces = n.saturating_sub(s.len());
                Self::fill_spaces(&mut buf, spaces);
                Self::fill_bs(&mut buf, s.len() - 1 + spaces);
                print!("{}", buf.as_str().expect("should be valid UTF-8"));
            }
            LineDisplayAction::ClearLine { n, c } => {
                let mut buf: StackVec<u8, DISPLAY_BUF_SIZE> = StackVec::new();
                Self::fill_clear_line(&mut buf, n, c);
                print!("{}", buf.as_str().expect("should be valid UTF-8"));
            }
            LineDisplayAction::RedrawLine { s, n, c } => {
                let mut buf: StackVec<u8, DISPLAY_BUF_SIZE> = StackVec::new();
                Self::fill_clear_line(&mut buf, n, c);
                Self::fill_str(&mut buf, s);
                let spaces = n.saturating_sub(s.len());
                Self::fill_spaces(&mut buf, spaces);
                Self::fill_bs(&mut buf, spaces);
                print!("{}", buf.as_str().expect("should be valid UTF-8"));
            }
            LineDisplayAction::CursorLeft => {
                print!("{}", ascii::BS as char);
            }
            LineDisplayAction::CursorRight(ch) => {
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
