#!/usr/bin/env python3
"""Generate resources/apple-uk.keymap from the layout table below.

This script is the editable source of truth for the keymap; the binary
file is its build artifact. Both are checked in. After editing LAYOUT,
rerun:  scripts/mkkeymap.py

File format (consumed by drivers/virtio/input.rs):
  256 bytes = 128 evdev keycodes x 2 bytes each:
    byte[code * 2]     = character when pressed plain
    byte[code * 2 + 1] = character when pressed with shift held
  0 means "no mapping" (modifiers, arrows, unused codes).

Character encoding matches the console font (resources/VGA8.F16), which
is a classic VGA font using code page 437 — NOT ASCII/Latin-1 for
anything above 0x7F. Hence the CP437 constants below.

CALIBRATION NOTE: the guest never sees the physical Apple keyboard.
KDE translates physical keys to keysyms, QEMU maps those to positional
US-style evdev codes. The Apple-specific corners (the section-sign key
top-left, the grave key next to left shift) are best-guess placements —
verify each against the C3 event printer and edit LAYOUT to match what
actually arrives.
"""

from pathlib import Path

# CP437 code points for non-ASCII glyphs (match VGA8.F16 rendering).
POUND = 0x9C     # £
SECTION = 0x15   # §
PLUSMINUS = 0xF1 # ±

# Control bytes, chosen to match what the shell's UART path already
# expects from a serial terminal (see shell/keyboard.rs):
#   Enter -> CR, Backspace -> DEL (0x7F), Tab -> HT, Escape -> ESC.
ESC = 0x1B
TAB = 0x09
ENTER = 0x0D
BACKSPACE = 0x7F

KEYCODES_MAX = 128

# (evdev keycode, plain, shifted) — Apple British (ISO) layout.
# Entries may be a 1-char string or an int (for control/CP437 bytes).
# Modifiers (shift 42/54, ctrl 29, alt 56, capslock 58) are deliberately
# absent: they are decoder state, not characters. Arrows/function keys
# are absent until the decoder learns VT escape sequences.
LAYOUT = [
    # -- top row --------------------------------------------------------
    (1,  ESC, ESC),        # Escape
    (2,  "1", "!"),
    (3,  "2", "@"),        # Apple UK: @ on shift-2 (unlike PC-UK layout)
    (4,  "3", POUND),      # Apple UK: £ on shift-3
    (5,  "4", "$"),
    (6,  "5", "%"),
    (7,  "6", "^"),
    (8,  "7", "&"),
    (9,  "8", "*"),
    (10, "9", "("),
    (11, "0", ")"),
    (12, "-", "_"),
    (13, "=", "+"),
    (14, BACKSPACE, BACKSPACE),
    # -- letter rows ----------------------------------------------------
    (15, TAB, TAB),
    (16, "q", "Q"), (17, "w", "W"), (18, "e", "E"), (19, "r", "R"),
    (20, "t", "T"), (21, "y", "Y"), (22, "u", "U"), (23, "i", "I"),
    (24, "o", "O"), (25, "p", "P"),
    (26, "[", "{"),
    (27, "]", "}"),
    (28, ENTER, ENTER),
    (30, "a", "A"), (31, "s", "S"), (32, "d", "D"), (33, "f", "F"),
    (34, "g", "G"), (35, "h", "H"), (36, "j", "J"), (37, "k", "K"),
    (38, "l", "L"),
    (39, ";", ":"),
    (40, "'", '"'),
    (41, SECTION, PLUSMINUS),  # CALIBRATE: Apple top-left § key likely
                               # arrives as KEY_GRAVE (41) via QEMU
    (43, "\\", "|"),
    (44, "z", "Z"), (45, "x", "X"), (46, "c", "C"), (47, "v", "V"),
    (48, "b", "B"), (49, "n", "N"), (50, "m", "M"),
    (51, ",", "<"),
    (52, ".", ">"),
    (53, "/", "?"),
    (57, " ", " "),
    # -- ISO extra key --------------------------------------------------
    (86, "`", "~"),        # CALIBRATE: Apple ISO grave/tilde key next to
                           # left shift usually arrives as KEY_102ND (86)
    # -- keypad (full-size keyboards; harmless if absent) ----------------
    (55, "*", "*"),
    (71, "7", "7"), (72, "8", "8"), (73, "9", "9"), (74, "-", "-"),
    (75, "4", "4"), (76, "5", "5"), (77, "6", "6"), (78, "+", "+"),
    (79, "1", "1"), (80, "2", "2"), (81, "3", "3"),
    (82, "0", "0"), (83, ".", "."),
    (96, ENTER, ENTER),    # keypad enter
    (98, "/", "/"),
]


def to_byte(value, code):
    if isinstance(value, str):
        assert len(value) == 1, f"keycode {code}: string must be one char"
        value = ord(value)
        assert value < 0x80, (
            f"keycode {code}: non-ASCII char must be given as a CP437 int"
        )
    assert 0 <= value <= 0xFF, f"keycode {code}: byte out of range"
    return value


def main():
    table = bytearray(KEYCODES_MAX * 2)
    seen = set()
    for code, plain, shifted in LAYOUT:
        assert 0 < code < KEYCODES_MAX, f"keycode {code} out of range"
        assert code not in seen, f"keycode {code} appears twice"
        seen.add(code)
        table[code * 2] = to_byte(plain, code)
        table[code * 2 + 1] = to_byte(shifted, code)

    out = Path(__file__).resolve().parent.parent / "resources" / "apple-uk.keymap"
    out.write_bytes(table)
    print(f"wrote {out} ({len(table)} bytes, {len(seen)} keycodes mapped)")


if __name__ == "__main__":
    main()
