#!/usr/bin/env python3
"""Draws the app icon and builds assets/lithe.icns.

Pure stdlib: writes a PNG by hand with zlib, then lets macOS's own sips and
iconutil produce the .icns. Keeps a design dependency out of the build.
"""
import os
import struct
import subprocess
import sys
import zlib

SIZE = 1024
BG = (0x14, 0x16, 0x1B)
SURFACE = (0x1E, 0x22, 0x2B)
CARET = (0x6E, 0xA8, 0xFE)
LINES = [(0x8F, 0xBF, 0x7F), (0xC8, 0xCC, 0xD4), (0x79, 0x81, 0x8F)]


def rounded_mask(size, radius):
    """Per-pixel coverage for a rounded square, 4x supersampled on corners."""
    mask = bytearray(size * size)
    for y in range(size):
        for x in range(size):
            dx = min(x, size - 1 - x)
            dy = min(y, size - 1 - y)
            if dx >= radius or dy >= radius:
                mask[y * size + x] = 255
                continue
            # dx/dy are distances to the nearest edge, so they say *how
            # close* a corner is but not *which* one. Pick the centre from
            # the coordinate itself.
            cx = radius if x < size // 2 else size - 1 - radius
            cy = radius if y < size // 2 else size - 1 - radius
            hits = 0
            for sy in range(4):
                for sx in range(4):
                    px = x + (sx + 0.5) / 4 - cx
                    py = y + (sy + 0.5) / 4 - cy
                    if px * px + py * py <= radius * radius:
                        hits += 1
            mask[y * size + x] = hits * 255 // 16
    return mask


def draw():
    size = SIZE
    px = bytearray(size * size * 4)
    mask = rounded_mask(size, int(size * 0.22))

    def blend(x, y, colour, alpha=255):
        if not (0 <= x < size and 0 <= y < size):
            return
        i = (y * size + x) * 4
        a = alpha * mask[y * size + x] // 255
        if a == 0:
            return
        for c in range(3):
            px[i + c] = (px[i + c] * (255 - a) + colour[c] * a) // 255
        px[i + 3] = max(px[i + 3], a)

    for y in range(size):
        for x in range(size):
            blend(x, y, BG)

    # An inset "document" plate, so the mark reads at small sizes.
    pad = int(size * 0.27)
    for y in range(pad, size - pad):
        for x in range(pad, size - pad):
            blend(x, y, SURFACE)

    # Three code lines of differing length, and a caret on the second.
    line_h = int(size * 0.07)
    gap = int(size * 0.075)
    left = int(size * 0.35)
    top = int(size * 0.335)
    widths = [0.20, 0.13, 0.26]
    for n, (colour, w) in enumerate(zip(LINES, widths)):
        y0 = top + n * (line_h + gap)
        x1 = left + int(size * w)
        for y in range(y0, y0 + line_h):
            for x in range(left, x1):
                blend(x, y, colour)
        if n == 1:
            cx = x1 + int(size * 0.035)
            for y in range(y0 - line_h // 2, y0 + line_h + line_h // 2):
                for x in range(cx, cx + int(size * 0.035)):
                    blend(x, y, CARET)
    return bytes(px)


def write_png(path, data):
    raw = b"".join(
        b"\x00" + data[y * SIZE * 4 : (y + 1) * SIZE * 4] for y in range(SIZE)
    )

    def chunk(tag, payload):
        body = tag + payload
        return struct.pack(">I", len(payload)) + body + struct.pack(
            ">I", zlib.crc32(body) & 0xFFFFFFFF
        )

    with open(path, "wb") as f:
        f.write(
            b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9))
            + chunk(b"IEND", b"")
        )


def main():
    here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    assets = os.path.join(here, "assets")
    os.makedirs(assets, exist_ok=True)
    base = os.path.join(assets, "icon-1024.png")
    write_png(base, draw())

    iconset = os.path.join(assets, "lithe.iconset")
    subprocess.run(["rm", "-rf", iconset], check=True)
    os.makedirs(iconset)
    for size in (16, 32, 64, 128, 256, 512, 1024):
        for scale, suffix in ((1, ""), (2, "@2x")):
            px = size * scale
            if px > 1024:
                continue
            name = f"icon_{size}x{size}{suffix}.png"
            subprocess.run(
                ["sips", "-z", str(px), str(px), base, "--out",
                 os.path.join(iconset, name)],
                check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
    out = os.path.join(assets, "lithe.icns")
    subprocess.run(["iconutil", "-c", "icns", iconset, "-o", out], check=True)
    subprocess.run(["rm", "-rf", iconset], check=True)
    print(out)


if __name__ == "__main__":
    sys.exit(main())
