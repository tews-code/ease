//! Line editor for EASE shell

use crate::hal::ascii;
use crate::input::escape::Key;
use crate::input::keyboard::KeyEvent;
use crate::kernel::collection::Vec;

pub const LINE_LEN: usize = 256;
const HISTORY_SIZE: usize = 30;

// Enum returned by the line editor to instruct display
#[expect(dead_code)]
pub enum LineDisplayAction<'a> {
    None,
    Backspace { s: &'a str },
    Bell,
    Echo(u8), // Simple append at end
    Enter,
    Redraw { s: &'a str, n: usize }, //Blank `n` chars at current position, redraw `s` at current position, reposition cursor
    RedrawLine { s: &'a str, n: usize, c: usize }, // Redraw string `s` from start of line, leave cursor at end. `n` is current line size, `c` is current cursor position
    ClearLine { n: usize, c: usize }, // Clear line sharing the number of chars in that line `n` and the cursor starting position within that line `c`
    CursorLeft,
    CursorRight(u8),
}

pub struct LineEditor {
    pub line: Vec<u8, LINE_LEN>,
    saved_line: Vec<u8, LINE_LEN>, // Stores current line when browsing history
    cursor: usize,                 // Cursor position
    history_index: Option<usize>,  // History row currently being used.
    history: Vec<Vec<u8, LINE_LEN>, HISTORY_SIZE>,
}

impl LineEditor {
    pub const fn new() -> Self {
        Self {
            line: Vec::new(),
            saved_line: Vec::new(),
            cursor: 0,
            history_index: None,
            history: Vec::new(),
        }
    }

    /// Process a line
    ///
    /// - Takes a KeyEvent from your keyboard module
    /// - Returns Some(line) when Enter is pressed
    /// - Returns None otherwise (still editing)
    pub fn process(&mut self, event: KeyEvent) -> (Option<&str>, LineDisplayAction<'_>) {
        match event {
            KeyEvent::Byte(ascii::TAB) => (None, LineDisplayAction::Bell), // Discard tab
            KeyEvent::Byte(ch) => {
                if self.line.is_full() {
                    return (None, LineDisplayAction::Bell);
                };
                let at_end = self.cursor == self.line.len();
                if at_end {
                    // Adding a single char at end
                    let _ = self.line.push(ch);
                    self.cursor += 1;
                    (None, LineDisplayAction::Echo(ch))
                } else {
                    // Print the char and reprint rest of string
                    let _ = self.line.insert(self.cursor, ch);
                    self.cursor += 1;
                    (
                        None,
                        LineDisplayAction::Redraw {
                            s: str::from_utf8(
                                &self.line.as_slice()[self.cursor - 1..self.line.len()],
                            )
                            .expect("should be utf-8"),
                            n: 0, // No underlining chars to blank
                        },
                    )
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
                        self.history_index = None; // Not browsing history
                        self.saved_line.clear();
                        // Return the line (using command history store)
                        (
                            Some(
                                self.history[0]
                                    .as_str()
                                    .expect("should only have UTF-8-valid bytes"),
                            ),
                            LineDisplayAction::Enter,
                        )
                    }
                    Key::Backspace => {
                        if self.cursor > 0 {
                            // Remove the character at the cursor
                            self.line.remove(self.cursor - 1);
                            self.cursor -= 1;
                            let s =
                                str::from_utf8(&self.line.as_slice()[self.cursor..self.line.len()])
                                    .expect("should be valid UTF-8");
                            (None, LineDisplayAction::Backspace { s })
                        } else {
                            // Already at start, bell
                            (None, LineDisplayAction::Bell)
                        }
                    }
                    Key::Esc => {
                        let prev_len = self.line.len();
                        self.line.clear();
                        self.cursor = 0;
                        self.history_index = None;
                        (
                            None,
                            LineDisplayAction::ClearLine {
                                n: prev_len,
                                c: self.cursor,
                            },
                        )
                    }
                    Key::ArrowUp => {
                        if self.history.is_empty()
                            || self.history_index == Some(self.history.len() - 1)
                        {
                            return (None, LineDisplayAction::Bell);
                        };
                        if self.history_index.is_none() {
                            // Store current line
                            self.saved_line = self.line;
                        };
                        let prev_len = match self.history_index {
                            None => self.line.len(),
                            Some(i) => self.history[i].len(),
                        };
                        // Update history index
                        let index = self.history_index.map_or(0, |i| i + 1);
                        self.history_index = Some(index);
                        self.line = self.history[index];
                        let c = self.cursor;
                        self.cursor = self.line.len();
                        (
                            None,
                            LineDisplayAction::RedrawLine {
                                s: str::from_utf8(self.line.as_slice()).expect("should be UTF-8"),
                                n: prev_len,
                                c,
                            },
                        )
                    }
                    Key::ArrowDown => {
                        let Some(index) = self.history_index else {
                            return (None, LineDisplayAction::Bell);
                        };
                        if self.history.is_empty() {
                            return (None, LineDisplayAction::Bell);
                        };
                        let prev_len = self.line.len();
                        if index == 0 {
                            self.history_index = None;
                            self.line = self.saved_line;
                        } else {
                            self.history_index = Some(index - 1);
                            self.line = self.history[index - 1];
                        };
                        let c = self.cursor;
                        self.cursor = self.line.len();
                        (
                            None,
                            LineDisplayAction::RedrawLine {
                                s: str::from_utf8(self.line.as_slice()).expect("should be UTF-8"),
                                n: prev_len,
                                c,
                            },
                        )
                    }
                    Key::ArrowRight => {
                        if self.cursor < self.line.len() {
                            self.cursor += 1;
                            (
                                None,
                                LineDisplayAction::CursorRight(self.line[self.cursor - 1]),
                            )
                        } else {
                            // Already at end of line, bell
                            (None, LineDisplayAction::Bell)
                        }
                    }
                    Key::ArrowLeft => {
                        if self.cursor > 0 {
                            self.cursor -= 1;
                            (None, LineDisplayAction::CursorLeft)
                        } else {
                            // Already at prompt, bell
                            (None, LineDisplayAction::Bell)
                        }
                    }
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
    fn test_tab_rejected() {
        let mut ed = LineEditor::new();
        let (result, action) = ed.process(byte(ascii::TAB));
        assert!(result.is_none());
        assert!(matches!(action, LineDisplayAction::Bell));
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
        // Insert in middle — should produce Redraw with n=0
        let (_, action) = ed.process(byte(b'X'));
        assert!(matches!(action, LineDisplayAction::Redraw { n: 0, .. }));
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
        ed.process(key(Key::ArrowLeft));
        ed.process(key(Key::ArrowLeft));
        let (_, action) = ed.process(key(Key::ArrowRight));
        assert!(matches!(action, LineDisplayAction::CursorRight(b'b')));
    }

    #[test_case]
    fn test_insert_in_middle() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "ac");
        ed.process(key(Key::ArrowLeft));
        ed.process(byte(b'b'));
        assert_eq!(ed.line.as_str(), Ok("abc"));
    }

    #[test_case]
    fn test_backspace_in_middle() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "abcd");
        ed.process(key(Key::ArrowLeft));
        ed.process(key(Key::Backspace));
        assert_eq!(ed.line.as_str(), Ok("abd"));
    }

    #[test_case]
    fn test_esc_clears_line() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "something");
        let (_, action) = ed.process(key(Key::Esc));
        assert!(matches!(
            action,
            LineDisplayAction::ClearLine { n: 9, c: 0 }
        )); // "something" = 9 chars
        assert_eq!(ed.line.len(), 0);
    }

    #[test_case]
    fn test_esc_resets_history_browsing() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "first");
        ed.process(key(Key::Enter));
        ed.process(key(Key::ArrowUp)); // browsing history
        ed.process(key(Key::Esc)); // should reset
        // ArrowDown should bell (not browsing anymore)
        let (_, action) = ed.process(key(Key::ArrowDown));
        assert!(matches!(action, LineDisplayAction::Bell));
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
        // Up once -> "second"
        ed.process(key(Key::ArrowUp));
        assert_eq!(ed.line.as_str(), Ok("second"));
        // Up again -> "first"
        ed.process(key(Key::ArrowUp));
        assert_eq!(ed.line.as_str(), Ok("first"));
        // Down -> back to "second"
        ed.process(key(Key::ArrowDown));
        assert_eq!(ed.line.as_str(), Ok("second"));
    }

    #[test_case]
    fn test_history_down_restores_current_line() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "old");
        ed.process(key(Key::Enter));
        type_str(&mut ed, "current");
        // Browse into history
        ed.process(key(Key::ArrowUp));
        assert_eq!(ed.line.as_str(), Ok("old"));
        // Come back — should restore "current"
        ed.process(key(Key::ArrowDown));
        assert_eq!(ed.line.as_str(), Ok("current"));
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
    fn test_history_up_empty_bells() {
        let mut ed = LineEditor::new();
        // No history — up should bell
        let (_, action) = ed.process(key(Key::ArrowUp));
        assert!(matches!(action, LineDisplayAction::Bell));
    }

    #[test_case]
    fn test_history_up_at_oldest_bells() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "only");
        ed.process(key(Key::Enter));
        ed.process(key(Key::ArrowUp)); // "only"
        // Already at oldest — should bell
        let (_, action) = ed.process(key(Key::ArrowUp));
        assert!(matches!(action, LineDisplayAction::Bell));
    }

    #[test_case]
    fn test_history_redrawline_has_prev_len() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "long command");
        ed.process(key(Key::Enter));
        type_str(&mut ed, "hi");
        // ArrowUp from "hi" (2 chars) to "long command" — n should be 2
        let (_, action) = ed.process(key(Key::ArrowUp));
        assert!(matches!(action, LineDisplayAction::RedrawLine { n: 2, .. }));
    }

    #[test_case]
    fn test_full_line_rejects_input() {
        let mut ed = LineEditor::new();
        for _ in 0..LINE_LEN {
            ed.process(byte(b'x'));
        }
        assert!(ed.line.is_full());
        let (_, action) = ed.process(byte(b'y'));
        assert!(matches!(action, LineDisplayAction::Bell));
        assert_eq!(ed.line.len(), LINE_LEN);
    }
}
