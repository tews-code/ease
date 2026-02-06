//! Line editor for EASE shell

use crate::input::escape::Key;
use crate::input::keyboard::KeyEvent;
use crate::kernel::collection::Vec;

pub const LINE_LEN: usize = 256;
const HISTORY_SIZE: usize = 30;

// Enum returned by the line editor to instruct display
pub enum LineDisplayAction<'a> {
    None,
    Backspace { s: &'a [u8] },
    Bell,
    Echo(u8),                   // Simple append at end
    Enter,
    Redraw { s: &'a [u8]},      // Redraw from position to end, reposition cursor
    RedrawLine { s: &'a [u8]},  // Redraw from start, reposition cursor
    ClearLine(usize),           // Clear line sharing the number of chars in that line
    CursorLeft,
    CursorRight(u8),
}

pub struct LineEditor {
    pub line: Vec<u8, LINE_LEN>,
    cursor: usize,      // Cursor position
    history_row: usize, // History row currently being used.
    history: Vec<Vec<u8, LINE_LEN>, HISTORY_SIZE>,
}

impl LineEditor {
    pub const fn new() -> Self {
        Self {
            line: Vec::new(),
            cursor: 0,
            history_row: 0,
            history: Vec::new(),
        }
    }

    /// Process a line
    ///
    /// - Takes a KeyEvent from your keyboard module
    /// - Returns Some(line) when Enter is pressed
    /// - Returns None otherwise (still editing)
    pub fn process(&mut self, event: KeyEvent) -> (Option<&str>, LineDisplayAction) {
        match event {
            KeyEvent::Byte(ch) => {
                if self.line.is_full() {
                    return (None, LineDisplayAction::None);
                };
                let at_end = self.cursor == self.line.len();
                if at_end {
                    let _ = self.line.push(ch);
                    self.cursor += 1;
                    (None, LineDisplayAction::Echo(ch))
                } else {
                    let _ = self.line.insert(self.cursor, ch);
                    self.cursor += 1;
                    (None,
                     LineDisplayAction::Redraw {
                         s: &self.line.as_slice()[self.cursor - 1..self.line.len()]
                     })
                }
            }
            KeyEvent::Special(key) => {
                match key {
                    Key::Enter => {
                        // Insert this command as most recent the command history
                        if self.history.is_full() {
                            let _ = self.history.pop();
                        }
                        let _ = self.history.insert(0, self.line);
                        self.line.clear();
                        self.cursor = 0;
                        // Set history current row to 0
                        self.history_row = 0;
                        // Return the line (using command history store)
                        (Some(
                            self.history[0]
                                .as_str()
                                .expect("should only have UTF-8-valid bytes"),
                        ), LineDisplayAction::Enter)
                    },
                    Key::Backspace => {
                        if self.cursor > 0 {
                            // Remove the character at the cursor
                            self.line.remove(self.cursor - 1);
                            self.cursor -= 1;
                            (None, LineDisplayAction::Backspace {
                                s: &self.line.as_slice()[self.cursor..self.line.len()]
                            })
                        } else {
                            // Already at start, bell
                            (None, LineDisplayAction::Bell)
                        }
                    },
                    Key::Esc => {
                        self.line.clear();
                        self.cursor = 0;
                        self.history_row = 0;
                        (None, LineDisplayAction::ClearLine(self.line.len()))
                    }
                    Key::ArrowUp => {
                       if self.history_row == 0 {
                           // Save current line and step into history
                           let _ = self.history.insert(0, self.line);
                           self.history_row = 1;
                       } else if self.history_row + 1 < self.history.len() {
                           // Move further back in history
                           self.history_row += 1;
                       } else {
                           // Already at oldest entry
                            if self.history.is_empty() {
                                return (None, LineDisplayAction::Bell);
                            };
                       }
                        self.line = self.history[self.history_row];
                        self.cursor = self.line.len();
                        (None, LineDisplayAction::RedrawLine {
                            s: &self.line.as_slice()
                        })
                    },
                    Key::ArrowDown => {
                        if self.history_row == 0 {
                            (None, LineDisplayAction::Bell)
                        } else {
                            self.history_row -= 1;
                            self.line = self.history[self.history_row];
                            self.cursor = self.line.len();
                            (None, LineDisplayAction::RedrawLine {
                                s: &self.line.as_slice()
                            })
                        }
                    },
                    Key::ArrowRight => {
                        if self.cursor < self.line.len() {
                            self.cursor += 1;
                            (None, LineDisplayAction::CursorRight(self.line[self.cursor-1]))
                        } else {
                            // Already at end of line, bell
                            (None, LineDisplayAction::Bell)
                        }
                    },
                    Key::ArrowLeft => {
                        if self.cursor > 0 {
                            self.cursor -= 1;
                            (None, LineDisplayAction::CursorLeft)
                        } else {
                            // Already at prompt, bell
                            (None, LineDisplayAction::Bell)
                        }
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn byte(ch: u8) -> KeyEvent {
        KeyEvent::Byte(ch)
    }

    fn key(k: Key) -> KeyEvent {
        KeyEvent::Special(k)
    }

    // Helper: type a string into the editor
    fn type_str(ed: &mut LineEditor, s: &str) {
        for b in s.bytes() {
            ed.process(byte(b));
        }
    }

    #[test_case]
    fn test_type_char_echoes() {
        let mut ed = LineEditor::new();
        let (result, action) = ed.process(byte(b'a'));
        assert!(result.is_none());
        assert!(matches!(action, LineDisplayAction::Echo(b'a')));
    }

    #[test_case]
    fn test_type_builds_line() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "hello");
        assert_eq!(ed.line.as_str(), Ok("hello"));
    }

    #[test_case]
    fn test_enter_returns_line() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "echo hi");
        let (result, action) = ed.process(key(Key::Enter));
        assert!(matches!(action, LineDisplayAction::Enter));
        assert_eq!(result, Some("echo hi"));
    }

    #[test_case]
    fn test_enter_clears_line() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "test");
        ed.process(key(Key::Enter));
        // Line should be cleared after Enter
        assert_eq!(ed.line.len(), 0);
    }

    #[test_case]
    fn test_empty_enter() {
        let mut ed = LineEditor::new();
        let (result, action) = ed.process(key(Key::Enter));
        assert!(matches!(action, LineDisplayAction::Enter));
        assert_eq!(result, Some(""));
    }

    #[test_case]
    fn test_backspace_removes_char() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "abc");
        let (result, action) = ed.process(key(Key::Backspace));
        assert!(result.is_none());
        assert!(matches!(action, LineDisplayAction::Backspace { .. }));
        assert_eq!(ed.line.as_str(), Ok("ab"));
    }

    #[test_case]
    fn test_backspace_at_start_bells() {
        let mut ed = LineEditor::new();
        let (result, action) = ed.process(key(Key::Backspace));
        assert!(result.is_none());
        assert!(matches!(action, LineDisplayAction::Bell));
    }

    #[test_case]
    fn test_arrow_left_moves_cursor() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "abc");
        let (_, action) = ed.process(key(Key::ArrowLeft));
        assert!(matches!(action, LineDisplayAction::CursorLeft));
        // Now insert — should go into middle position
        let (_, action) = ed.process(byte(b'X'));
        assert!(matches!(action, LineDisplayAction::Redraw { .. }));
        assert_eq!(ed.line.as_str(), Ok("abXc"));
    }

    #[test_case]
    fn test_arrow_left_at_start_bells() {
        let mut ed = LineEditor::new();
        let (_, action) = ed.process(key(Key::ArrowLeft));
        assert!(matches!(action, LineDisplayAction::Bell));
    }

    #[test_case]
    fn test_arrow_right_at_end_bells() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "abc");
        let (_, action) = ed.process(key(Key::ArrowRight));
        assert!(matches!(action, LineDisplayAction::Bell));
    }

    #[test_case]
    fn test_arrow_right_moves_cursor() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "abc");
        // Move left twice
        ed.process(key(Key::ArrowLeft));
        ed.process(key(Key::ArrowLeft));
        // Move right once
        let (_, action) = ed.process(key(Key::ArrowRight));
        assert!(matches!(action, LineDisplayAction::CursorRight(b'b')));
    }

    #[test_case]
    fn test_insert_in_middle() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "ac");
        ed.process(key(Key::ArrowLeft)); // cursor before 'c'
        ed.process(byte(b'b')); // insert 'b' between a and c
        assert_eq!(ed.line.as_str(), Ok("abc"));
    }

    #[test_case]
    fn test_backspace_in_middle() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "abcd");
        ed.process(key(Key::ArrowLeft)); // cursor before 'd'
        ed.process(key(Key::Backspace)); // delete 'c'
        assert_eq!(ed.line.as_str(), Ok("abd"));
    }

    #[test_case]
    fn test_esc_clears_line() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "something");
        let (_, action) = ed.process(key(Key::Esc));
        assert!(matches!(action, LineDisplayAction::ClearLine(_)));
        assert_eq!(ed.line.len(), 0);
    }

    #[test_case]
    fn test_history_stores_on_enter() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "first");
        ed.process(key(Key::Enter));
        type_str(&mut ed, "second");
        ed.process(key(Key::Enter));
        // Arrow up should show "second" (most recent)
        let (_, action) = ed.process(key(Key::ArrowUp));
        assert!(matches!(action, LineDisplayAction::RedrawLine { .. }));
        assert_eq!(ed.line.as_str(), Ok("second"));
    }

    #[test_case]
    fn test_history_navigate_up_down() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "first");
        ed.process(key(Key::Enter));
        type_str(&mut ed, "second");
        ed.process(key(Key::Enter));
        // Up once → "second"
        ed.process(key(Key::ArrowUp));
        assert_eq!(ed.line.as_str(), Ok("second"));
        // Up again → "first"
        ed.process(key(Key::ArrowUp));
        assert_eq!(ed.line.as_str(), Ok("first"));
        // Down → back to "second"
        ed.process(key(Key::ArrowDown));
        assert_eq!(ed.line.as_str(), Ok("second"));
    }

    #[test_case]
    fn test_history_down_at_bottom_bells() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "test");
        ed.process(key(Key::Enter));
        // Not browsing history — down should bell
        let (_, action) = ed.process(key(Key::ArrowDown));
        assert!(matches!(action, LineDisplayAction::Bell));
    }

    #[test_case]
    fn test_full_line_rejects_input() {
        let mut ed = LineEditor::new();
        // Fill the line buffer
        for _ in 0..LINE_LEN {
            ed.process(byte(b'x'));
        }
        assert!(ed.line.is_full());
        // One more should be rejected silently
        let (_, action) = ed.process(byte(b'y'));
        assert!(matches!(action, LineDisplayAction::None));
        assert_eq!(ed.line.len(), LINE_LEN);
    }
}
