# Console Module Review Discussion

Notes from code review of the console/renderer refactoring.

## Architecture Overview

After refactoring, the console module has four layers:

| Layer | Knows about | Doesn't know about |
|---|---|---|
| TextBuffer | cells, cx, cy | rendering, cursors, fonts |
| TerminalEmulator | control chars, when to scroll | pixels, colours, visibility |
| Console | cursor visibility, renderer | cell data, font details |
| FrameBufferRenderer | pixels, colours, font | terminal state |

## Review Items

### 1. Generic Console over Renderer (won't fix)

**Issue:** `Console<R: Renderer>` is stored in a static with concrete `FrameBufferRenderer`. The generic adds trait bounds everywhere without runtime polymorphism.

**Decision:** Valid point. Could remove the generic and make Console concrete. The Renderer trait can still exist for documenting the interface. Low priority — can revisit if it causes friction.

### 2. writeable_char leaving buffer in invalid state (fix)

**Issue:** After writing at last cell, cy becomes ROWS (out of bounds). Buffer is corrupt until caller calls scroll().

**Fix:** Don't increment cy past ROWS-1. Use `if self.cy < ROWS - 1 { self.cy += 1 } else { scroll = true }` instead.

### 3. Split scroll responsibility (fix)

**Issue:** Both `writeable_char` and `line_feed` detect scroll conditions but return flags instead of scrolling. Every caller must check and act.

**Fix:** Have TextBuffer scroll itself internally. Still return a bool so TerminalEmulator knows to emit `RenderCommand::Scroll`, but the buffer is always self-consistent.

### 4. Cursor state split across three layers (acknowledged)

**Issue:** TextBuffer owns cx/cy, Console owns cursor_visible, TerminalEmulator forwards cursor queries. Cursor state is smeared across three structs.

**Possible fix:** Move `cursor_visible` into TerminalEmulator. Add `hide_cursor`/`show_cursor` methods that emit RenderCommands via the closure pattern. Console becomes a thin command executor. Needs a new `RenderCommand::ShowCursor` variant for inverted drawing.

**Decision:** Bigger refactor. Do as a separate commit when current changes are stable.

### 5. RenderCommand doesn't carry defaults (won't fix)

**Issue:** Scroll doesn't carry blank row content, Clear doesn't carry background colour. Renderer must implicitly know conventions.

**Decision:** Intentional separation. The emulator thinks in characters and grid positions, the renderer thinks in pixels and colours. "Blank = space, default colours" is a universal terminal convention. Adding parameters would over-engineer a dumb terminal.

### 6. BS and DEL treated identically (acknowledged)

**Issue:** BS (0x08) and DEL (0x7F) both just move cursor left. DEL conventionally erases.

**Decision:** In practice, DEL never reaches the terminal emulator — the keyboard escape parser maps it to `Key::Backspace` and the line editor/shell handle erase semantics. The terminal only sees BS as "move left". Note as future consideration for raw terminal mode.

### 7. No guard against non-ASCII bytes (acknowledged)

**Issue:** Bytes above 0x7F fall into catch-all and get written as characters. VGA8 font has CP437 glyphs for 128-255, so no crash, but not UTF-8.

**Possible fix:** Filter with `ch.is_ascii_graphic() || ch == b' '` in the catch-all arm. Low priority since all input currently comes from ASCII shell.

## Design Principles Discussed

### Module design approach

1. **Start messy, then extract** (recommended) — write everything in one struct, get it working, then extract structs when groups of fields/methods naturally cluster
2. **Data first** — fields accessed together belong together; fields that change for different reasons belong in separate structs
3. **Don't split until you feel pain** — signs: methods only use subset of fields, same parameters passed everywhere, bug fix in one area breaks another, can't test without unrelated setup

### Shell display performance

Per-character `print!` loops for line clearing are slow (each call: lock + cursor hide/show + unlock). Better approach: build byte sequence in a stack buffer and `print!` once. Stack buffer under 1KB for 256-char max line.

## Key Refactoring Changes Made

- Extracted `Renderer` trait and `FrameBufferRenderer` into `src/drivers/render.rs`
- Separated `TextBuffer` from `Console` (pure data, no rendering)
- Created `TerminalEmulator` with closure-based `process()` method
- Moved cursor management to entry points only (`write_str`, `put_char`)
- Added `cursor_left()` to TextBuffer for non-destructive cursor movement
- Replaced Console's `clear()`/`scroll()` public methods with `put_char(ascii::FF)` in tests
- Added 22 unit tests covering TextBuffer and TerminalEmulator
- Updated benchmarks and baselines for new architecture
