# Adding Emoji Support

**Status: deferred.** Planning notes from a design conversation; pick this up when other priorities clear.

## Goal

Support typing and displaying ~60-100 curated emojis in greyscale on the console, with the design transferring cleanly to a future e-ink target.

---

## Key Decisions

### Where the data lives

- Font glyphs are `static const` arrays → `.rodata` → **FLASH** (`memory-qemu.x:60`). Not SRAM.
- RP2350 flash is 16 MB; ASCII font is currently ~2 KB; adding 100 emojis at 24×24 4-bit grey is ~30 KB. Trivial vs 16 MB.
- Per-character runtime cost is just the UTF-8 bytes carried transiently (3-4 bytes per glyph).
- **Watch out for the .data trap.** `memory-qemu.x` has `ASSERT(SIZEOF(.data) == 0, ...)`. Anything that initialises non-zero would land in .data and break the build (same issue we hit with `THREAD_ID = AtomicUsize::new(1)`). Font tables must be const-zero-equivalent or rodata-stable.

### Glyph size

**24×24 with 4-bit greyscale** is the sweet spot.

| Size | Visual feel on 300 DPI e-ink | Memory @ 4-bit grey |
|------|------------------------------|---------------------|
| 16×16 | Smaller than body text. Outline-only emoji possible, fine detail impossible. | 128 B/glyph |
| **24×24** | **Body-text adjacent. Integrates well.** | **288 B/glyph** |
| 32×32 | Standalone "icon" feel. More detail. | 512 B/glyph |

- Match emoji size to ~1.2-1.5× cap-height of body text so they read as distinct visual objects.
- 4-bit greyscale (16 levels) matches typical e-ink panel native depth → antialiasing instead of dithering.

### Source: monochrome-designed emoji set

Colour emojis lose meaning when desaturated. Use a set explicitly designed for monochrome.

- **Noto Emoji** (Google's black-and-white variant; distinct from Noto Color Emoji). SIL OFL. **Top recommendation.**
- **OpenMoji** black variant. CC BY-SA. Slightly more whimsical.
- **Twemoji**. MIT/CC-BY 4.0. Colour-first, requires flattening.

All three ship as SVG sources, render at any size with antialiasing.

### Framebuffer impact: none

Pixels are pixels. The framebuffer (640×480 RGBA, `__fb_size = 640*480*4`) doesn't change. What changes is the **console grid layout**:

| Cell size | Columns | Rows |
|-----------|---------|------|
| 8×8 (current) | 80 | 60 |
| 24×24 | 26 | 20 |

Looks chunky in QEMU; would be physically tiny on a 300 DPI panel. Optional yak: enlarge the QEMU framebuffer (e.g. 1280×800) for a less cramped development view.

Code changes confined to the console layer (`shell/console.rs`):
- `CELL_W` / `CELL_H` constants
- Cursor advance, line wrap, scroll height all derived from those constants
- Internal text buffer redimensioned
- Blit routine extended from 8×8 to 24×24 bitmaps

---

## Rendering Pipeline

```
UTF-8 bytes  →  Codepoint  →  Glyph index  →  Bitmap data  →  Framebuffer
              [decode]      [lookup]        [load]          [blit]
```

### Stage 1: UTF-8 decode

Use `&str.chars()`. Rust standard library handles this. (For byte-by-byte input from the keyboard, you'd want a stateful decoder; for printing buffers, `chars()` is enough.)

### Stage 2: Codepoint → Glyph index

**Two-tier dispatch** (recommended):

- ASCII (codepoint < 128): direct array index into ASCII block. O(1).
- Else: binary search a sorted `static [(u32, u16); N]` table of `(codepoint, glyph_index)` pairs. ~10 comparisons for 1000 entries.

ASCII path stays free; emoji pay log-cost only when they're used.

### Stage 3: Glyph index → bitmap data

Concatenated bitmap array, fixed size per glyph. For 24×24 4-bit grey: 288 bytes per glyph. `glyph_data[i * 288 .. (i+1) * 288]`.

No per-glyph metadata needed for v1. (Variable-width text would require an additional metadata table — defer.)

### Stage 4: Blit

For each glyph pixel:
1. Read nibble from glyph data (handle even/odd nibble within byte).
2. Expand 4-bit → 8-bit grey: `(nibble << 4) | nibble` (preserves [0..15] → [0..255]).
3. Replicate to RGBA: `[v, v, v, 0xFF]`.
4. Write 4 bytes at framebuffer pixel position.

For 24×24: 576 pixels × 4 bytes = 2.3 KB writes per glyph. Trivial on RP2350.

### Stage 5: Cursor advance

Bump cursor x by `CELL_W`, wrap at right edge.

### E-ink future-proofing

The 4-bit nibble in stage 4 step 2 maps **directly** to e-ink panel grey levels. When the e-ink driver lands, write a separate blit routine that skips the RGB expansion and writes nibbles straight to the panel. Same glyph data, different output target.

---

## Selection Strategy

**Quantity: 60-100 glyphs is plenty.** Resist exhaustiveness.

Rough category breakdown for a writing appliance (less chat-style, more reflective):

| Category | Count | Notes |
|----------|-------|-------|
| Faces | ~20 | Common emotions; thoughtful, reflective. Less LOL/skull. |
| Hands | ~6 | Thumbs up/down, wave, clap, ok, raised. |
| Hearts | ~3-5 | Red, broken, sparkling, two hearts. |
| Symbols | ~15-20 | Star, sparkle, fire, check, cross, arrows, sun, moon, weather, leaf. |
| Typographic ornaments | ~10 | ✓ ✗ ★ ☆ → ← ※ § ¶ ❦ — non-emoji Unicode, but same pipeline. |

**Start with 20.** Build the pipeline end-to-end, prove it works, then expand. Going from "no emojis" to "first emoji renders" is the hard architectural step; 20 → 200 is just data. Don't burn time hand-curating before the pipeline exists.

Pick the starter list with Evie — what does *she* actually use?

---

## Build Pipeline

A small offline tool (Python or Rust), run manually:

1. Read a list: `(codepoint, source_svg_path, label)`.
2. Render each SVG at 24×24 with antialiasing (rsvg-convert / Python+cairosvg).
3. Quantise to 4-bit grey (16 levels), pack nibbles.
4. Output a Rust file:
   - `pub(crate) static EMOJI_GLYPHS: [u8; N] = [...];`
   - `pub(crate) static EMOJI_LOOKUP: &[(u32, u16)] = &[...];` (sorted by codepoint)

Check the generated file into git. Keeps the build deterministic and fast; the curated list won't change frequently.

---

## Input UX

Layered by frequency. Hybrid approach.

### Layer 1: ASCII auto-replace (top ~15)

IM-classic. Type `:)` → 😊 the instant the closing char arrives.

```
:)  :(  :D  :P  :|  :/  ;)  :*  XD
<3  </3
\o/  o.O  ^_^  T_T
```

- Post-keystroke hook in the line editor: examine last N bytes; if they match a pattern, splice them out and insert the emoji codepoint.
- ~20 lines of code plus a small static table.
- **Escape**: `\:)` produces literal text. Or: backspace immediately after replacement reverts to literal.

### Layer 2: Colon shortcode with inline preview (rest of curated set)

GitHub/Slack style. Type `:smi` → display the buffered name lightly highlighted in the text. Tab cycles matches. Second `:` or Enter commits. Esc or non-alpha aborts (re-emits literal).

- Small state machine in the line editor: *normal* / *in-shortcode*.
- Linear scan of the name table (100 entries — nothing).
- **No popup.** Inline preview only. Steals no screen real estate, doesn't interrupt flow.

### Layer 3 (skip)

**Don't add a fullscreen emoji picker.** Antithetical to a writing appliance. The curated set is small enough that the writer already knows what's available.

### Discoverability

A single-page reference card (printable, or `:help` command). Critical — without it, the writer can't use the shortcuts.

---

## Implementation Order (when resumed)

1. Pick a curated 20-emoji starter list with Evie.
2. Write the offline conversion script (SVG → 4-bit nibble-packed, Rust output).
3. Add lookup function (two-tier: ASCII direct + binary search emoji).
4. Extend `shell/console.rs` blit to handle 24×24 4-bit grey glyphs.
5. Wire stage 2 + 3 + 4 — confirm one emoji renders on screen.
6. Add ASCII auto-replace to the line editor.
7. Add colon shortcode + inline preview + Tab completion.
8. Reference card / `:help`.
9. Expand the curated set.

---

## Open Questions to Resolve When Resuming

- **Mixed glyph sizes** (proportional text + 24×24 emoji)? Defer — v1 should be uniform cell size.
- **Multi-codepoint sequences** (skin-tone modifiers, ZWJ family emojis)? Defer — adds trie/special-case handling.
- **Alpha blending** for emoji on non-uniform backgrounds? Not needed for solid-bg console.
- **Resize QEMU framebuffer** for nicer development view? Optional yak.
- **E-ink panel target** (size, supplier)? Drives final glyph dimensions and the blit-to-panel routine.
- **Variable-width body text** (proportional fonts)? Big change — adds per-glyph metadata. Defer until you actually want it.

---

## Pragmatic Reminders

- The hard step is "no emoji → first emoji renders correctly." Everything after is data.
- Start with 20 emojis, not 200.
- Manual one-shot build script, not `build.rs` automation.
- Reference card matters as much as the implementation. Write it.
- Skip popups. Distraction-free is a feature.
