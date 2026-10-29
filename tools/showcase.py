#!/usr/bin/env python3
"""Turn the panel apps' rendered frames into pictures for the README.

The render tests write the exact 256x144 greyscale frame the panel receives.
This scales each one up (nearest neighbour, so pixels stay pixels) and tints it
the way the Flipper One's orange-backlit LCD shows it: white is the backlight,
black is ink. Standard library only.

    DOLPHONE_RENDER=1 cargo test          # in each apps/<app>
    python3 tools/showcase.py            # docs/screens/*.png -> docs/screens/lcd/*.png
"""

import struct
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCALE = 3
# Backlight and ink, as the photo of the device shows them.
LIGHT = (0xFF, 0x8A, 0x00)
INK = (0x1E, 0x10, 0x00)


def read_gray_png(path):
    data = path.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", path
    pos, idat, w = 8, b"", 0
    while pos < len(data):
        n = struct.unpack(">I", data[pos:pos + 4])[0]
        kind, body = data[pos + 4:pos + 8], data[pos + 8:pos + 8 + n]
        if kind == b"IHDR":
            w, h, depth, ctype = struct.unpack(">IIBB", body[:10])
            if depth != 8 or ctype != 0:
                return None  # not one of ours
        elif kind == b"IDAT":
            idat += body
        pos += 12 + n
    raw = zlib.decompress(idat)
    rows, prev, i = [], bytearray(w), 0
    for _ in range(h):
        f = raw[i]
        line = bytearray(raw[i + 1:i + 1 + w])
        i += 1 + w
        for x in range(w):
            a = line[x - 1] if x else 0
            b, c = prev[x], prev[x - 1] if x else 0
            if f == 1:
                line[x] = (line[x] + a) & 255
            elif f == 2:
                line[x] = (line[x] + b) & 255
            elif f == 3:
                line[x] = (line[x] + (a + b) // 2) & 255
            elif f == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[x] = (line[x] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        rows.append(line)
        prev = line
    return w, h, rows


def tint(v):
    t = v / 255
    return bytes(round(INK[k] + (LIGHT[k] - INK[k]) * t) for k in range(3))


def write_rgb_png(path, w, h, rows):
    raw = bytearray()
    for row in rows:
        raw.append(0)
        raw += row

    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)

    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def main():
    src = ROOT / "docs/screens"
    out = src / "lcd"
    out.mkdir(exist_ok=True)
    palette = [tint(v) for v in range(256)]
    for p in sorted(src.glob("*.png")):
        img = read_gray_png(p)
        if img is None:
            continue
        w, h, rows = img
        big = []
        for row in rows:
            line = b"".join(palette[v] * SCALE for v in row)
            big.extend([line] * SCALE)
        write_rgb_png(out / p.name, w * SCALE, h * SCALE, big)
        print(f"{out.relative_to(ROOT) / p.name}")


if __name__ == "__main__":
    sys.exit(main())
