//! Console driver
//!
//! Classic 80 columns x 30 rows dumb terminal

use crate::drivers::ramfb::{Colour, FrameBuffer};
use crate::shell::{ascii, font};

const ROWS: usize = 30;
const COLUMNS: usize = 80;

struct TextBuffer {
    cells: [[u8; COLUMNS]; ROWS],
    cx: usize,
    cy: usize,
}

impl TextBuffer {
    fn char_at(&self, row: usize, column: usize) -> u8 {
        self.cells[row][column]
    }

    fn cursor_left(&mut self) {
        if self.cx > 0 {
            self.cx -= 1;
        }
    }

    fn scroll(&mut self) {
        for row in 0..ROWS - 1 {
            self.cells[row] = self.cells[row + 1];
        }
        self.cx = 0;
        self.cy = ROWS - 1;
        self.cells[self.cy] = [b' '; COLUMNS];
    }

    fn clear(&mut self) {
        self.cells = [[b' '; COLUMNS]; ROWS];
        self.cx = 0;
        self.cy = 0;
    }

    fn carriage_return(&mut self) {
        self.cx = 0;
    }

    fn line_feed(&mut self) -> bool {
        if self.cy < ROWS - 1 {
            self.cy += 1;
            // Note - for ANSI VT-100 we do not set cx back to zero
        } else {
            self.scroll();
            return true; // Flag to tell TerminalEmulator we are scrolling
        }
        false
    }

    // Writeable char
    fn writeable_char(&mut self, ch: u8) -> (usize, usize, bool) {
        let old_cx = self.cx;
        let old_cy = self.cy;
        let mut scroll = false;

        self.cells[self.cy][self.cx] = ch; // Draw char overwriting cursor

        self.cx += 1;
        if self.cx >= COLUMNS {
            self.cx = 0;
            if self.cy < ROWS - 1 {
                self.cy += 1;
            } else {
                self.scroll();
                scroll = true; // Flag to tell TerminalEmulator we are scrolling
            }
        }
        (old_cy, old_cx, scroll)
    }
}

#[derive(Clone, Copy, Debug)]
enum RenderCommand {
    Clear,
    Scroll,
    WriteChar(usize, usize, u8),        // (row, column, ch)
    DrawCursor(usize, usize, u8, bool), // (row, column, ch, inverted)
}

struct TerminalEmulator {
    buffer: TextBuffer,
    cursor_visible: bool,
}

impl TerminalEmulator {
    #[cfg(all(test, feature = "test-shell"))]
    fn cursor_pos(&self) -> (usize, usize) {
        (self.buffer.cy, self.buffer.cx)
    }

    fn char_at_cursor(&self) -> u8 {
        self.buffer.char_at(self.buffer.cy, self.buffer.cx)
    }

    fn process(&mut self, ch: u8, mut emit: impl FnMut(RenderCommand)) {
        match ch {
            ascii::BELL => {}
            ascii::BS | ascii::DEL => {
                self.buffer.cursor_left();
            }
            ascii::CR => {
                self.buffer.carriage_return();
            }
            ascii::FF => {
                self.buffer.clear(); // Sets cursor to origin
                emit(RenderCommand::Clear);
            }
            ascii::LF => {
                let scroll = self.buffer.line_feed();
                if scroll {
                    emit(RenderCommand::Scroll);
                }
            }
            ascii::TAB => {}
            _ => {
                if ch.is_ascii_graphic() || ch == b' ' {
                    let (row, column, scroll) = self.buffer.writeable_char(ch);
                    emit(RenderCommand::WriteChar(row, column, ch));
                    if scroll {
                        emit(RenderCommand::Scroll);
                    }
                }
            }
        }
    }

    fn show_cursor(&mut self, mut emit: impl FnMut(RenderCommand)) {
        if !self.cursor_visible {
            self.cursor_visible = true;
            emit(RenderCommand::DrawCursor(
                self.buffer.cy,
                self.buffer.cx,
                self.char_at_cursor(),
                true,
            ))
        }
    }

    fn hide_cursor(&mut self, mut emit: impl FnMut(RenderCommand)) {
        if self.cursor_visible {
            self.cursor_visible = false;
            emit(RenderCommand::DrawCursor(
                self.buffer.cy,
                self.buffer.cx,
                self.char_at_cursor(),
                false,
            ))
        }
    }
}

/// 80x30 text console backed by a framebuffer. Handles character rendering,
/// cursor display, and scrolling.
pub struct Console {
    fb: FrameBuffer,
    emulator: TerminalEmulator,
    prev_cursor: usize,
    prev_line_len: usize,
    fg: Colour,
    bg: Colour,
}

impl Console {
    /// Creates a new console with green-on-black text and cursor enabled.
    #[allow(dead_code)]
    pub fn new(fb: FrameBuffer) -> Self {
        Self {
            fb,
            emulator: TerminalEmulator {
                buffer: TextBuffer {
                    cells: [[b' '; COLUMNS]; ROWS],
                    cx: 0,
                    cy: 0,
                },
                cursor_visible: true,
            },
            prev_cursor: 0,
            prev_line_len: 0,
            fg: Colour::GREEN,
            bg: Colour::BLACK,
        }
    }

    /// Writes a character to the terminal emulator and renders it. Does not manage cursor visibility.
    pub fn process_char(&mut self, ch: u8) {
        let fg = self.fg;
        let bg = self.bg;
        let fb = &mut self.fb;
        let emulator = &mut self.emulator;
        emulator.process(ch, |cmd| match cmd {
            RenderCommand::Clear => fb.fill(bg),
            RenderCommand::Scroll => fb.scroll(crate::shell::font::HEIGHT, bg),
            RenderCommand::WriteChar(row, column, ch) => {
                font::render_glyph(fb, column * font::WIDTH, row * font::HEIGHT, ch, fg, bg)
            }
            _ => {}
        });
    }

    /// Write a character using the font at the current cursor position
    ///
    /// # `ch` is a byte
    pub fn put_char(&mut self, ch: u8) {
        self.hide_cursor();
        self.process_char(ch);
        self.show_cursor();
        // Also echo to UART
        crate::drivers::uart::direct_write_byte(ch);
    }

    /// Hides the cursor by redrawing the character at the cursor position in normal colours.
    pub fn hide_cursor(&mut self) {
        let fg = self.fg;
        let bg = self.bg;
        let fb = &mut self.fb;
        self.emulator.hide_cursor(|cmd| {
            if let RenderCommand::DrawCursor(row, column, ch, inverted) = cmd {
                let (fg, bg) = if inverted { (bg, fg) } else { (fg, bg) };
                font::render_glyph(fb, column * font::WIDTH, row * font::HEIGHT, ch, fg, bg);
            }
        })
    }

    /// Shows the cursor by drawing the character at the cursor position in inverted colours.
    pub fn show_cursor(&mut self) {
        let fg = self.fg;
        let bg = self.bg;
        let fb = &mut self.fb;
        self.emulator.show_cursor(|cmd| {
            if let RenderCommand::DrawCursor(row, column, ch, inverted) = cmd {
                let (fg, bg) = if inverted { (bg, fg) } else { (fg, bg) };
                font::render_glyph(fb, column * font::WIDTH, row * font::HEIGHT, ch, fg, bg);
            }
        })
    }

    /// Redraws a line overwriting the previous content, starting at `cursor`
    #[allow(dead_code)]
    pub fn redraw_line(&mut self, line: &[u8], cursor: usize) {
        // Covers mid line insert and delete, Esc line clear and history recall by redrawing
        self.hide_cursor();
        for _ in 0..self.prev_cursor {
            self.process_char(ascii::BS);
        }
        for byte in line {
            self.process_char(*byte);
        }
        let new_len = line.len();
        let spaces = self.prev_line_len.saturating_sub(new_len);
        for _ in 0..spaces {
            self.process_char(b' ');
        }
        for _ in 0..(new_len + spaces - cursor) {
            self.process_char(ascii::BS);
        }
        self.show_cursor();
        self.prev_line_len = new_len;
        self.prev_cursor = cursor;
    }

    #[allow(dead_code)]
    pub fn reset_line(&mut self) {
        self.prev_cursor = 0;
        self.prev_line_len = 0;
    }
}

impl core::fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.hide_cursor();
        for byte in s.bytes() {
            if byte == b'\n' {
                self.process_char(b'\r');
                // Also print to UART (no locking)
                crate::drivers::uart::direct_write_byte(b'\r');
            }
            self.process_char(byte);
            // Also print to UART (no locking)
            crate::drivers::uart::direct_write_byte(byte);
        }
        self.show_cursor();

        Ok(())
    }
}

#[cfg(all(test, feature = "bench"))]
mod benchmarks {
    use crate::bench;
    use crate::drivers::ramfb::FrameBuffer;
    use crate::println;
    use crate::shell::ascii;
    use core::fmt::Write;

    use super::Console;

    const ITER_LARGE: u32 = 100;
    const ITER_SMALL: u32 = 10;

    // Baselines measured without IrqSpinLock overhead (owned Console).
    // Set at ~2x measured values for QEMU timing variance.
    const WRITE_STR_HELLO: u64 = 2_500_000;

    #[test_case]
    fn console_benchmarks() {
        use crate::drivers::ramfb::Colour;
        use crate::shell::font;

        println!();
        println!("====== CONSOLE ====== ");
        println!();

        // -- Regression check --
        {
            let fb = FrameBuffer::init();
            let mut console = Console::new(fb);
            console.put_char(ascii::FF);
            bench::check(
                "Console::write_str(\"hello\\n\")",
                WRITE_STR_HELLO,
                ITER_SMALL,
                || {
                    let _ = console.write_str("hello\n");
                },
            );
        }

        println!();

        // -- Print path profile --
        {
            let fb = FrameBuffer::init();
            let mut console = Console::new(fb);

            bench::run_avg("Console::put_char(ch)", ITER_SMALL, || {
                console.put_char(b'X');
            });
            console.put_char(ascii::FF);
            bench::run_avg("Console::show+hide_cursor", ITER_SMALL, || {
                console.show_cursor();
                console.hide_cursor();
            });
            console.put_char(ascii::FF);
            bench::run_avg("Console::write_char(ch)", ITER_SMALL, || {
                console
                    .write_char(b'X' as char)
                    .expect("should be able to write char");
            });
            console.put_char(ascii::FF);
            bench::run_avg("Console::write_char(LF)", ITER_SMALL, || {
                console
                    .write_char(ascii::LF as char)
                    .expect("should be able to write line feed");
            });
            console.put_char(ascii::FF);
            bench::run_avg("Console::write_str(\"hello\\n\")", ITER_SMALL, || {
                let _ = console.write_str("hello\n");
            });

            // FrameBuffer-level benchmarks
            let mut fb = console.fb;
            bench::run_avg("FrameBuffer::set_pixels(8px)", ITER_LARGE, || {
                fb.set_pixels(
                    0,
                    0,
                    &[
                        Colour::RED.as_raw(),
                        Colour::BLUE.as_raw(),
                        Colour::RED.as_raw(),
                        Colour::BLUE.as_raw(),
                        Colour::RED.as_raw(),
                        Colour::BLUE.as_raw(),
                        Colour::RED.as_raw(),
                        Colour::BLUE.as_raw(),
                    ],
                );
            });
            bench::run_avg("font::render_glyph", ITER_LARGE, || {
                font::render_glyph(&mut fb, 0, 0, b'X', Colour::WHITE, Colour::BLUE);
            });
        }

        println!();

        // -- Scroll path profile --
        {
            let fb = FrameBuffer::init();
            let mut console = Console::new(fb);

            // Position cursor at last row so each LF triggers a scroll
            console.put_char(ascii::FF);
            for _ in 0..29 {
                console.put_char(ascii::LF);
            }
            bench::run_avg("Console::write_char(LF) with scroll", ITER_LARGE, || {
                console.write_char(ascii::LF as char).unwrap();
            });
        }

        println!();
        println!("===================== ");
        println!();
    }
}

#[cfg(all(test, feature = "test-shell"))]
mod tests {
    use super::*;
    use crate::kernel::collection::StackVec;

    fn new_buffer() -> TextBuffer {
        TextBuffer {
            cells: [[b' '; COLUMNS]; ROWS],
            cx: 0,
            cy: 0,
        }
    }

    fn new_emulator() -> TerminalEmulator {
        TerminalEmulator {
            buffer: new_buffer(),
            cursor_visible: true,
        }
    }

    fn collect_cmds(emu: &mut TerminalEmulator, ch: u8) -> StackVec<RenderCommand, 8> {
        let mut cmds: StackVec<RenderCommand, 8> = StackVec::new();
        emu.process(ch, |cmd| {
            cmds.push(cmd).unwrap();
        });
        cmds
    }

    // =========================================================================
    // TextBuffer
    // =========================================================================

    #[test_case]
    fn text_buffer_clear_resets_cursor_and_cells() {
        let mut buf = new_buffer();
        buf.writeable_char(b'X');
        buf.writeable_char(b'Y');
        buf.clear();
        assert_eq!(buf.cx, 0);
        assert_eq!(buf.cy, 0);
        assert_eq!(buf.char_at(0, 0), b' ');
        assert_eq!(buf.char_at(0, 1), b' ');
    }

    #[test_case]
    fn text_buffer_writeable_char_advances_cursor() {
        let mut buf = new_buffer();
        let (row, col, scroll) = buf.writeable_char(b'A');
        assert_eq!((row, col, scroll), (0, 0, false));
        assert_eq!(buf.cx, 1);
        assert_eq!(buf.cy, 0);
        assert_eq!(buf.char_at(0, 0), b'A');
    }

    #[test_case]
    fn text_buffer_writeable_char_wraps_at_column_end() {
        let mut buf = new_buffer();
        buf.cx = COLUMNS - 1;
        let (row, col, scroll) = buf.writeable_char(b'Z');
        assert_eq!((row, col), (0, COLUMNS - 1));
        assert!(!scroll);
        assert_eq!(buf.cx, 0);
        assert_eq!(buf.cy, 1);
    }

    #[test_case]
    fn text_buffer_writeable_char_signals_scroll() {
        let mut buf = new_buffer();
        buf.cy = ROWS - 1;
        buf.cx = COLUMNS - 1;
        let (_, _, scroll) = buf.writeable_char(b'!');
        assert!(scroll);
    }

    #[test_case]
    fn text_buffer_cursor_left_moves_back() {
        let mut buf = new_buffer();
        buf.cx = 5;
        buf.cursor_left();
        assert_eq!(buf.cx, 4);
    }

    #[test_case]
    fn text_buffer_cursor_left_stops_at_zero() {
        let mut buf = new_buffer();
        buf.cursor_left();
        assert_eq!(buf.cx, 0);
    }

    #[test_case]
    fn text_buffer_carriage_return() {
        let mut buf = new_buffer();
        buf.cx = 40;
        buf.carriage_return();
        assert_eq!(buf.cx, 0);
    }

    #[test_case]
    fn text_buffer_line_feed_advances_row() {
        let mut buf = new_buffer();
        buf.cx = 10;
        let scroll = buf.line_feed();
        assert!(!scroll);
        assert_eq!(buf.cy, 1);
        assert_eq!(buf.cx, 10);
    }

    #[test_case]
    fn text_buffer_line_feed_at_bottom_signals_scroll() {
        let mut buf = new_buffer();
        buf.cy = ROWS - 1;
        let scroll = buf.line_feed();
        assert!(scroll);
        assert_eq!(buf.cy, ROWS - 1);
    }

    #[test_case]
    fn text_buffer_scroll_shifts_rows_up() {
        let mut buf = new_buffer();
        buf.cells[1][0] = b'A';
        buf.cells[2][0] = b'B';
        buf.scroll();
        assert_eq!(buf.char_at(0, 0), b'A');
        assert_eq!(buf.char_at(1, 0), b'B');
        assert_eq!(buf.char_at(ROWS - 1, 0), b' ');
    }

    #[test_case]
    fn text_buffer_scroll_resets_cursor() {
        let mut buf = new_buffer();
        buf.cx = 10;
        buf.cy = 5;
        buf.scroll();
        assert_eq!(buf.cx, 0);
        assert_eq!(buf.cy, ROWS - 1);
    }

    // =========================================================================
    // TerminalEmulator
    // =========================================================================

    #[test_case]
    fn emulator_printable_char_emits_write() {
        let mut emu = new_emulator();
        let cmds = collect_cmds(&mut emu, b'X');
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], RenderCommand::WriteChar(0, 0, b'X')));
        assert_eq!(emu.cursor_pos(), (0, 1));
    }

    #[test_case]
    fn emulator_bell_emits_nothing() {
        let mut emu = new_emulator();
        let cmds = collect_cmds(&mut emu, ascii::BELL);
        assert_eq!(cmds.len(), 0);
    }

    #[test_case]
    fn emulator_tab_emits_nothing() {
        let mut emu = new_emulator();
        let cmds = collect_cmds(&mut emu, ascii::TAB);
        assert_eq!(cmds.len(), 0);
    }

    #[test_case]
    fn emulator_bs_moves_cursor_no_emit() {
        let mut emu = new_emulator();
        collect_cmds(&mut emu, b'A');
        collect_cmds(&mut emu, b'B');
        let cmds = collect_cmds(&mut emu, ascii::BS);
        assert_eq!(cmds.len(), 0);
        assert_eq!(emu.cursor_pos(), (0, 1));
    }

    #[test_case]
    fn emulator_bs_preserves_cell() {
        let mut emu = new_emulator();
        collect_cmds(&mut emu, b'A');
        collect_cmds(&mut emu, ascii::BS);
        assert_eq!(emu.char_at_cursor(), b'A');
    }

    #[test_case]
    fn emulator_cr_moves_cursor_no_emit() {
        let mut emu = new_emulator();
        collect_cmds(&mut emu, b'H');
        collect_cmds(&mut emu, b'i');
        let cmds = collect_cmds(&mut emu, ascii::CR);
        assert_eq!(cmds.len(), 0);
        assert_eq!(emu.cursor_pos(), (0, 0));
    }

    #[test_case]
    fn emulator_lf_no_scroll() {
        let mut emu = new_emulator();
        let cmds = collect_cmds(&mut emu, ascii::LF);
        assert_eq!(cmds.len(), 0);
        assert_eq!(emu.cursor_pos(), (1, 0));
    }

    #[test_case]
    fn emulator_lf_at_bottom_emits_scroll() {
        let mut emu = new_emulator();
        // Move to last row
        for _ in 0..ROWS - 1 {
            collect_cmds(&mut emu, ascii::LF);
        }
        assert_eq!(emu.cursor_pos(), (ROWS - 1, 0));
        let cmds = collect_cmds(&mut emu, ascii::LF);
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], RenderCommand::Scroll));
        assert_eq!(emu.cursor_pos(), (ROWS - 1, 0));
    }

    #[test_case]
    fn emulator_ff_emits_clear() {
        let mut emu = new_emulator();
        collect_cmds(&mut emu, b'X');
        let cmds = collect_cmds(&mut emu, ascii::FF);
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], RenderCommand::Clear));
        assert_eq!(emu.cursor_pos(), (0, 0));
    }

    #[test_case]
    fn emulator_wrap_and_scroll() {
        let mut emu = new_emulator();
        emu.buffer.cy = ROWS - 1;
        emu.buffer.cx = COLUMNS - 1;
        let cmds = collect_cmds(&mut emu, b'!');
        assert_eq!(cmds.len(), 2);
        assert!(matches!(cmds[0], RenderCommand::WriteChar(_, _, b'!')));
        assert!(matches!(cmds[1], RenderCommand::Scroll));
    }

    #[test_case]
    fn emulator_string_builds_buffer() {
        let mut emu = new_emulator();
        for b in b"hello" {
            collect_cmds(&mut emu, *b);
        }
        assert_eq!(emu.buffer.char_at(0, 0), b'h');
        assert_eq!(emu.buffer.char_at(0, 1), b'e');
        assert_eq!(emu.buffer.char_at(0, 2), b'l');
        assert_eq!(emu.buffer.char_at(0, 3), b'l');
        assert_eq!(emu.buffer.char_at(0, 4), b'o');
        assert_eq!(emu.cursor_pos(), (0, 5));
    }

    #[test_case]
    fn emulator_cr_lf_moves_to_start_of_next_line() {
        let mut emu = new_emulator();
        for b in b"hello" {
            collect_cmds(&mut emu, *b);
        }
        assert_eq!(emu.cursor_pos(), (0, 5));
        collect_cmds(&mut emu, ascii::CR);
        collect_cmds(&mut emu, ascii::LF);
        assert_eq!(emu.cursor_pos(), (1, 0));
    }

    #[test_case]
    fn emulator_lf_alone_preserves_column() {
        let mut emu = new_emulator();
        for b in b"hello" {
            collect_cmds(&mut emu, *b);
        }
        collect_cmds(&mut emu, ascii::LF);
        // Column stays at 5 — correct VT-100 behaviour
        assert_eq!(emu.cursor_pos(), (1, 5));
    }
}
