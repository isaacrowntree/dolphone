#!/usr/bin/env python3
"""Draw the apps' menu icons.

FlipCTL's manifest wants a PNG 14 px wide whose alpha is the shape, optionally a
strip of 14 px frames that animates while the row is selected. These are drawn
here from ASCII art so they are reviewable in a diff and need no image editor:
`#` is ink, anything else is clear. The Dialer's handset shakes when selected,
Messages' bubble shows typing dots, and Settings' slider moves.

    python3 tools/make-icons.py      # writes apps/*/icon.png
"""

import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

HANDSET = [
    "..............",
    "..###.........",
    ".#####........",
    ".#####........",
    ".####.........",
    "..###.........",
    "..###.........",
    "...###........",
    "....###.......",
    ".....####.###.",
    "......#######.",
    ".......######.",
    ".........###..",
    "..............",
]

BUBBLE = [
    "..............",
    "..##########..",
    ".############.",
    ".############.",
    ".############.",
    ".############.",
    ".############.",
    ".############.",
    "..##########..",
    "...###........",
    "...##.........",
    "...#..........",
    "..............",
    "..............",
]

SLIDERS = [
    "..............",
    "..............",
    "...##.........",
    "############..",
    "...##.........",
    "..............",
    "........##....",
    "############..",
    "........##....",
    "..............",
    ".....##.......",
    "############..",
    ".....##.......",
    "..............",
]


def shift(art, dx, dy):
    h, w = len(art), len(art[0])
    out = []
    for y in range(h):
        row = ""
        for x in range(w):
            sx, sy = x - dx, y - dy
            row += art[sy][sx] if 0 <= sx < w and 0 <= sy < h else "."
        out.append(row)
    return out


def punch(art, holes):
    """Clear pixels: the dots inside the bubble."""
    rows = [list(r) for r in art]
    for x, y in holes:
        rows[y][x] = "."
    return ["".join(r) for r in rows]


def png(frames, path):
    """A strip of 14x14 frames, left to right, black ink with the shape in alpha."""
    w, h = 14 * len(frames), 14
    raw = bytearray()
    for y in range(h):
        raw.append(0)  # no filter
        for f in frames:
            for x in range(14):
                on = f[y][x] == "#"
                raw += bytes((0, 0, 0, 255 if on else 0))

    def chunk(kind, data):
        c = struct.pack(">I", len(data)) + kind + data
        return c + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    body = b"\x89PNG\r\n\x1a\n"
    body += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
    body += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    body += chunk(b"IEND", b"")
    path.write_bytes(body)
    print(f"{path.relative_to(ROOT)}: {len(frames)} frame(s)")


def main():
    handset = [HANDSET, shift(HANDSET, 1, 0), HANDSET, shift(HANDSET, -1, 0)]
    dots = [(3 + 3 * i, 5) for i in range(3)]
    bubble = [punch(BUBBLE, dots[:n] + [(x + 1, y) for x, y in dots[:n]]) for n in (1, 2, 3, 0)]
    sliders = [SLIDERS, shift_rows(SLIDERS, {2: 2, 3: 0, 4: 2}), shift_rows(SLIDERS, {6: -2, 8: -2}), shift_rows(SLIDERS, {10: 2, 12: 2})]
    png(handset, ROOT / "apps/dialer/icon.png")
    png(bubble, ROOT / "apps/messages/icon.png")
    png(sliders, ROOT / "apps/settings/icon.png")


def shift_rows(art, moves):
    """Move the knob rows sideways, keeping the track rows as they are."""
    out = list(art)
    for y, dx in moves.items():
        row = art[y]
        if "############" in row:
            continue
        out[y] = shift([row], dx, 0)[0]
    return out


if __name__ == "__main__":
    main()
