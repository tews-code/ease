#!/usr/bin/env python3
"""Generate resources/apple-uk.keymap from the layout table below.

This script is the editable source of truth for the keymap; the binary
file is its build artifact. Both are checked in. After editing LAYOUT,
rerun:  scripts/mkkeymap.py

File format (consumed by drivers/keyboard.rs):
  512 bytes = 128 evdev keycodes x 4 bytes each. The column is the
  modifier state read as bits, so the decoder needs no precedence rule:
    column = (shift as usize) | (alt as usize) << 1
    byte[code * 4 + 0] = plain
    byte[code * 4 + 1] = shift
    byte[code * 4 + 2] = alt/option
    byte[code * 4 + 3] = shift + alt/option
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
# Alt/option layer glyphs
INV_EXCL = 0xAD   # ¡
INV_QUES = 0xA8   # ¿
CENT = 0x9B       # ¢
YEN = 0x9D        # ¥
FLORIN = 0x9F     # ƒ
PARA = 0x14       # ¶
ORD_F = 0xA6      # ª
ORD_M = 0xA7      # º
NOT = 0xAA        # ¬
DEGREE = 0xF8     # °
DIVIDE = 0xF6     # ÷
LAQUO = 0xAE      # «
RAQUO = 0xAF      # »
AE = 0x91         # æ
AE_CAP = 0x92     # Æ
ARING = 0x86      # å
ARING_CAP = 0x8F  # Å
CCEDIL = 0x87     # ç
CCEDIL_CAP = 0x80 # Ç
SZLIG = 0xE1      # ß
PI = 0xE3         # π
MU = 0xE6         # µ
OMEGA = 0xEA      # Ω
INFINITY = 0xEC   # ∞
SQRT = 0xFB       # √

# Control bytes, chosen to match what the shell's UART path already
# expects from a serial terminal (see shell/keyboard.rs):
#   Enter -> CR, Backspace -> DEL (0x7F), Tab -> HT, Escape -> ESC.
ESC = 0x1B
TAB = 0x09
ENTER = 0x0D
BACKSPACE = 0x7F

KEYCODES_MAX = 128
BYTES_PER_CODE = 4   # plain, shift, alt, shift+alt

# (evdev keycode, plain, shifted[, alt[, shift_alt]]) — Apple British
# (ISO) layout. The trailing elements are optional; omitting one means
# "no mapping".
#
# The alt/option layer is limited by the console font: VGA8.F16 is code
# page 437, which predates the euro, so Option-2 (EUR) has no glyph and
# is deliberately absent. Also absent: the dead keys (Option-e/u/i/n)
# and macOS symbols with no CP437 equivalent (TM, bullet, en dash,
# ellipsis, (C), (R), dagger, approx, not-equal, <=, >=, oe, o-slash).
# Option-8 (bullet) would be CP437 0x07, which the console consumes as
# BEL, so only its shifted form (degree) is mapped.
#
# CALIBRATE: every alt entry below is a best guess at what arrives
# through KDE -> QEMU. KDE may also swallow some Option combinations as
# window-manager shortcuts before the guest sees them.
# Entries may be a 1-char string or an int (for control/CP437 bytes).
# Modifiers (shift 42/54, ctrl 29, alt 56/100, capslock 58) are
# deliberately absent: they are decoder state, not characters.
# Arrows/function keys are absent until the decoder emits special keys.
LAYOUT = [
    # -- top row --------------------------------------------------------
    (1,  ESC, ESC),        # Escape
    (2,  "1", "!", INV_EXCL),
    (3,  "2", "@"),        # Apple UK: @ on shift-2 (unlike PC-UK layout)
    (4,  "3", POUND, "#"),  # Apple UK: £ on shift-3, # on option-3
    (5,  "4", "$", CENT),
    (6,  "5", "%", INFINITY),
    (7,  "6", "^", SECTION),
    (8,  "7", "&", PARA),
    (9,  "8", "*", 0, DEGREE),  # option-8 is bullet = BEL, skipped
    (10, "9", "(", ORD_F),
    (11, "0", ")", ORD_M),
    (12, "-", "_"),
    (13, "=", "+"),
    (14, BACKSPACE, BACKSPACE),
    # -- letter rows ----------------------------------------------------
    (15, TAB, TAB),
    (16, "q", "Q"), (17, "w", "W"), (18, "e", "E"), (19, "r", "R"),
    (20, "t", "T"), (21, "y", "Y", YEN), (22, "u", "U"), (23, "i", "I"),
    (24, "o", "O"), (25, "p", "P", PI),
    (26, "[", "{"),
    (27, "]", "}"),
    (28, ENTER, ENTER),
    (30, "a", "A", ARING, ARING_CAP), (31, "s", "S", SZLIG),
    (32, "d", "D"), (33, "f", "F", FLORIN),
    (34, "g", "G"), (35, "h", "H"), (36, "j", "J"), (37, "k", "K"),
    (38, "l", "L", NOT),
    (39, ";", ":"),
    (40, "'", '"', AE, AE_CAP),
    (41, SECTION, PLUSMINUS),  # CALIBRATE: Apple top-left § key likely
                               # arrives as KEY_GRAVE (41) via QEMU
    (43, "\\", "|", LAQUO, RAQUO),
    (44, "z", "Z", OMEGA), (45, "x", "X"),
    (46, "c", "C", CCEDIL, CCEDIL_CAP), (47, "v", "V", SQRT),
    (48, "b", "B"), (49, "n", "N"), (50, "m", "M", MU),
    (51, ",", "<"),
    (52, ".", ">"),
    (53, "/", "?", DIVIDE, INV_QUES),
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
    table = bytearray(KEYCODES_MAX * BYTES_PER_CODE)
    seen = set()
    for entry in LAYOUT:
        assert 3 <= len(entry) <= 1 + BYTES_PER_CODE, (
            f"entry {entry!r}: want 3 to {1 + BYTES_PER_CODE} elements"
        )
        code = entry[0]
        assert 0 < code < KEYCODES_MAX, f"keycode {code} out of range"
        assert code not in seen, f"keycode {code} appears twice"
        seen.add(code)
        # Columns beyond those given are left unmapped (0).
        columns = list(entry[1:]) + [0] * (BYTES_PER_CODE - (len(entry) - 1))
        for column, value in enumerate(columns):
            table[code * BYTES_PER_CODE + column] = to_byte(value, code)

    out = Path(__file__).resolve().parent.parent / "resources" / "apple-uk.keymap"
    out.write_bytes(table)
    print(f"wrote {out} ({len(table)} bytes, {len(seen)} keycodes mapped)")


if __name__ == "__main__":
    main()
