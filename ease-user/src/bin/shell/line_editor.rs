//! Line editor for shell

use alloc::vec::Vec;
use alloc::collections::VecDeque;
use ease_ulib::ascii;
use crate::vt_parse::{Key, KeyEvent};
#[cfg(test)]
use crate::vt_parse::Modifier;

pub const LINE_LEN: usize = 74; // 80 columns - 6 char prompt
const HISTORY_SIZE: usize = 10;

/// Result of processing a key event. The caller uses `line()` and `cursor()` to
/// get the current state after each result.
#[must_use]
pub enum EditResult {
    /// Append a byte to the end of the line
    Append(u8),
    /// Enter pressed
    Complete,
    /// Cursor has moved one character to the right
    CursorMoveRight,
    /// Cursor has moved one character to the left
    CursorMoveLeft,
    /// Line has been edited, changes need to be drawn
    LineEdit,
    /// Invalid action
    Reject,
}

/// Line editor with cursor movement, insert/delete, and command history.
pub struct LineEditor {
    line: Vec<u8>,
    saved_line: Vec<u8>,            // Stores current line when browsing history
    cursor: usize,                  // Cursor position
    history_index: Option<usize>,   // History row currently being used.
    history: VecDeque<Vec<u8>>,
}

impl LineEditor {
    /// Creates a new line editor with empty line and no history.
    pub const fn new() -> Self {
        Self {
            line: Vec::new(),
            saved_line: Vec::new(),
            cursor: 0,
            history_index: None,
            history: VecDeque::new(),
        }
    }

    /// Returns the current line contents as a byte slice.
    pub fn line(&self) -> &[u8] {
        self.line.as_slice()
    }

    /// Returns the current cursor position (byte offset into the line).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Clears the line and resets the cursor. Call after processing a completed command.
    pub fn reset(&mut self) {
        self.line.clear();
        self.cursor = 0;
    }

    /// Process a line
    ///
    /// Takes a KeyEvent and returns an EditResult
    pub fn process(&mut self, event: KeyEvent) -> EditResult {
        match event {
            KeyEvent::Char(ascii::TAB) => EditResult::Reject, // Discard tab
            KeyEvent::Char(ch) => {
                // Check if we've reached our limit
                if self.line.len() == LINE_LEN {
                    return EditResult::Reject;
                }
                if self.cursor == self.line.len() {
                    self.line.push(ch);
                    self.cursor += 1;
                    EditResult::Append(ch)
                } else {
                    self.line.insert(self.cursor, ch);
                    self.cursor += 1;
                    EditResult::LineEdit
                }
            }
            KeyEvent::Special(key, _modifier) => {
                match key {
                    Key::ArrowDown => {
                        let Some(index) = self.history_index else {
                            return EditResult::Reject;
                        };
                        if self.history.is_empty() {
                            return EditResult::Reject;
                        };
                        if index == 0 {
                            self.history_index = None;
                            self.line = core::mem::take(&mut self.saved_line);
                        } else {
                            // Find the newest entry
                            let Some(entry) = self.history.iter().rev().nth(index - 1)
                            else {
                                return EditResult::Reject;
                            };
                            self.history_index = Some(index - 1);
                            self.line = entry.clone();
                        }
                        self.cursor = self.line.len();
                        EditResult::LineEdit
                    }
                    Key::ArrowLeft => {
                        if self.cursor > 0 {
                            self.cursor -= 1;
                            EditResult::CursorMoveLeft
                        } else {
                            // Already at prompt, reject
                            EditResult::Reject
                        }
                    }
                    Key::ArrowRight => {
                        if self.cursor < self.line.len() {
                            self.cursor += 1;
                            EditResult::CursorMoveRight
                        } else {
                            // Already at end of line, reject
                            EditResult::Reject
                        }
                    }
                    Key::ArrowUp => {
                        if self.history.is_empty()
                            || self.history_index == Some(self.history.len() - 1)
                            {
                                return EditResult::Reject;
                            };
                        if self.history_index.is_none() {
                            // Store current line
                            self.saved_line = core::mem::take(&mut self.line);
                        };
                        // Update history index
                        let index = self.history_index.map_or(0, |i| i + 1);
                        self.history_index = Some(index);
                        // Find the newest entry
                        let Some(entry) = self.history.iter().rev().nth(index)
                        else {
                            return EditResult::Reject;
                        };
                        self.line = entry.clone();
                        self.cursor = self.line.len();
                        EditResult::LineEdit
                    }
                    Key::Backspace => {
                        if self.cursor > 0 {
                            // Remove the character at the cursor
                            self.line.remove(self.cursor - 1);
                            self.cursor -= 1;
                            EditResult::LineEdit
                        } else {
                            // Already at start, reject
                            EditResult::Reject
                        }
                    }
                    Key::Delete => {
                        if self.cursor < self.line.len() {
                            let _ = self.line.remove(self.cursor);
                            EditResult::LineEdit
                        } else {
                            EditResult::Reject
                        }
                    }
                    Key::End => {
                        if self.cursor < self.line.len() {
                            self.cursor = self.line.len();
                            EditResult::LineEdit
                        } else {
                            EditResult::Reject
                        }
                    }
                    Key::Enter => {
                        self.history.push_back(self.line.clone());
                        // Check if we are out of space - in which case forget the oldest entry
                        if self.history.len() > HISTORY_SIZE {
                            self.history.pop_front();
                        }
                        self.cursor = 0;
                        self.history_index = None;
                        self.saved_line.clear();

                        EditResult::Complete
                    }
                    Key::Esc => {
                        self.line.clear();
                        self.cursor = 0;
                        self.history_index = None;
                        EditResult::LineEdit
                    }
                    Key::Home => {
                        if self.cursor > 0 {
                            self.cursor = 0;
                            EditResult::LineEdit
                        } else {
                            EditResult::Reject
                        }
                    },
                    Key::Insert => EditResult::Reject,
                    Key::PageUp => EditResult::Reject,
                    Key::PageDown => EditResult::Reject,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn byte(ch: u8) -> KeyEvent {
        KeyEvent::Char(ch)
    }

    fn key(k: Key) -> KeyEvent {
        KeyEvent::Special(k, Modifier::default())
    }

    // Helper: type a string into the editor
    fn type_str(ed: &mut LineEditor, s: &str) {
        for b in s.bytes() {
            let _ = ed.process(byte(b));
        }
    }

    #[test]
    fn test_type_char() {
        let mut ed = LineEditor::new();
        let result = ed.process(byte(b'a'));
        assert!(matches!(result, EditResult::Append(b'a')));
        assert_eq!(ed.line(), b"a");
        assert_eq!(ed.cursor(), 1);
    }

    #[test]
    fn test_type_builds_line() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "hello");
        assert_eq!(ed.line(), b"hello");
        assert_eq!(ed.cursor(), 5);
    }

    #[test]
    fn test_tab_rejected() {
        let mut ed = LineEditor::new();
        let result = ed.process(byte(ascii::TAB));
        assert!(matches!(result, EditResult::Reject));
    }

    #[test]
    fn test_enter_completes() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "echo hi");
        assert_eq!(ed.line(), b"echo hi");
        let result = ed.process(key(Key::Enter));
        assert!(matches!(result, EditResult::Complete));
    }

    #[test]
    fn test_enter_preserves_line_until_reset() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "test");
        let _ = ed.process(key(Key::Enter));
        assert_eq!(ed.line(), b"test");
        assert_eq!(ed.cursor(), 0);
        ed.reset();
        assert!(ed.line().is_empty());
    }

    #[test]
    fn test_empty_enter() {
        let mut ed = LineEditor::new();
        let result = ed.process(key(Key::Enter));
        assert!(matches!(result, EditResult::Complete));
    }

    #[test]
    fn test_backspace_removes_char() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "abc");
        let result = ed.process(key(Key::Backspace));
        assert!(matches!(result, EditResult::LineEdit));
        assert_eq!(ed.line(), b"ab");
        assert_eq!(ed.cursor(), 2);
    }

    #[test]
    fn test_backspace_at_start_reject() {
        let mut ed = LineEditor::new();
        let result = ed.process(key(Key::Backspace));
        assert!(matches!(result, EditResult::Reject));
    }

    #[test]
    fn test_arrow_left_moves_cursor() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "abc");
        let result = ed.process(key(Key::ArrowLeft));
        assert!(matches!(result, EditResult::CursorMoveLeft));
        assert_eq!(ed.cursor(), 2);
        // Insert in middle
        let _ = ed.process(byte(b'X'));
        assert_eq!(ed.line(), b"abXc");
        assert_eq!(ed.cursor(), 3);
    }

    #[test]
    fn test_arrow_left_at_start_reject() {
        let mut ed = LineEditor::new();
        let result = ed.process(key(Key::ArrowLeft));
        assert!(matches!(result, EditResult::Reject));
        assert_eq!(ed.cursor(), 0);
    }

    #[test]
    fn test_arrow_right_at_end_reject() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "abc");
        let result = ed.process(key(Key::ArrowRight));
        assert!(matches!(result, EditResult::Reject));
        assert_eq!(ed.cursor(), 3);
    }

    #[test]
    fn test_arrow_right_moves_cursor() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "abc");
        let _ = ed.process(key(Key::ArrowLeft));
        let _ = ed.process(key(Key::ArrowLeft));
        assert_eq!(ed.cursor(), 1);
        let result = ed.process(key(Key::ArrowRight));
        assert!(matches!(result, EditResult::CursorMoveRight));
        assert_eq!(ed.cursor(), 2);
    }

    #[test]
    fn test_insert_in_middle() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "ac");
        let _ = ed.process(key(Key::ArrowLeft));
        let result = ed.process(byte(b'b'));
        assert!(matches!(result, EditResult::LineEdit));
        assert_eq!(ed.line(), b"abc");
    }

    #[test]
    fn test_backspace_in_middle() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "abcd");
        let _ = ed.process(key(Key::ArrowLeft));
        let _ = ed.process(key(Key::Backspace));
        assert_eq!(ed.line(), b"abd");
        assert_eq!(ed.cursor(), 2);
    }

    #[test]
    fn test_esc_clears_line() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "something");
        let result = ed.process(key(Key::Esc));
        assert!(matches!(result, EditResult::LineEdit));
        assert!(ed.line().is_empty());
        assert_eq!(ed.cursor(), 0);
    }

    #[test]
    fn test_esc_resets_history_browsing() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "first");
        let _ = ed.process(key(Key::Enter));
        ed.reset();
        let _ = ed.process(key(Key::ArrowUp)); // browsing history
        let _ = ed.process(key(Key::Esc)); // should reset
        // ArrowDown should reject (not browsing anymore)
        let result = ed.process(key(Key::ArrowDown));
        assert!(matches!(result, EditResult::Reject));
    }

    #[test]
    fn test_history_stores_on_enter() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "first");
        let _ = ed.process(key(Key::Enter));
        ed.reset();
        type_str(&mut ed, "second");
        let _ = ed.process(key(Key::Enter));
        ed.reset();
        // Arrow up should show "second" (most recent)
        let result = ed.process(key(Key::ArrowUp));
        assert!(matches!(result, EditResult::LineEdit));
        assert_eq!(ed.line(), b"second");
    }

    #[test]
    fn test_history_navigate_up_down() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "first");
        let _ = ed.process(key(Key::Enter));
        ed.reset();
        type_str(&mut ed, "second");
        let _ = ed.process(key(Key::Enter));
        ed.reset();
        // Up once -> "second"
        let _ = ed.process(key(Key::ArrowUp));
        assert_eq!(ed.line(), b"second");
        // Up again -> "first"
        let _ = ed.process(key(Key::ArrowUp));
        assert_eq!(ed.line(), b"first");
        // Down -> back to "second"
        let _ = ed.process(key(Key::ArrowDown));
        assert_eq!(ed.line(), b"second");
    }

    #[test]
    fn test_history_down_restores_current_line() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "old");
        let _ = ed.process(key(Key::Enter));
        ed.reset();
        type_str(&mut ed, "current");
        // Browse into history
        let _ = ed.process(key(Key::ArrowUp));
        assert_eq!(ed.line(), b"old");
        // Come back — should restore "current"
        let _ = ed.process(key(Key::ArrowDown));
        assert_eq!(ed.line(), b"current");
    }

    #[test]
    fn test_history_down_at_bottom_reject() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "test");
        let _ = ed.process(key(Key::Enter));
        ed.reset();
        // Not browsing history — down should reject
        let result = ed.process(key(Key::ArrowDown));
        assert!(matches!(result, EditResult::Reject));
    }

    #[test]
    fn test_history_up_empty_reject() {
        let mut ed = LineEditor::new();
        // No history — up should reject
        let result = ed.process(key(Key::ArrowUp));
        assert!(matches!(result, EditResult::Reject));
    }

    #[test]
    fn test_history_up_at_oldest_reject() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "only");
        let _ = ed.process(key(Key::Enter));
        ed.reset();
        let _ = ed.process(key(Key::ArrowUp)); // "only"
        // Already at oldest — should reject
        let result = ed.process(key(Key::ArrowUp));
        assert!(matches!(result, EditResult::Reject));
    }

    #[test]
    fn test_history_sets_cursor_to_end() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "long command");
        let _ = ed.process(key(Key::Enter));
        ed.reset();
        type_str(&mut ed, "hi");
        let _ = ed.process(key(Key::ArrowUp));
        assert_eq!(ed.line(), b"long command");
        assert_eq!(ed.cursor(), 12); // cursor at end of recalled line
    }

    // History keeps the newest HISTORY_SIZE lines: after one line more,
    // Up reaches back exactly HISTORY_SIZE lines, the oldest kept being the
    // second one entered, and the first is gone.
    #[test]
    fn test_history_keeps_newest_history_size_lines() {
        let mut ed = LineEditor::new();
        for i in 0..=HISTORY_SIZE {
            type_str(&mut ed, &format!("cmd{i}"));
            let _ = ed.process(key(Key::Enter));
            ed.reset();
        }
        for _ in 0..HISTORY_SIZE {
            assert!(matches!(ed.process(key(Key::ArrowUp)), EditResult::LineEdit));
        }
        assert_eq!(ed.line(), b"cmd1");
        assert!(matches!(ed.process(key(Key::ArrowUp)), EditResult::Reject));
    }

    #[test]
    fn test_full_line_rejects_input() {
        let mut ed = LineEditor::new();
        for _ in 0..LINE_LEN {
            let _ = ed.process(byte(b'x'));
        }
        assert_eq!(ed.line().len(), LINE_LEN);
        let result = ed.process(byte(b'y'));
        assert!(matches!(result, EditResult::Reject));
        assert_eq!(ed.line().len(), LINE_LEN);
    }
}
