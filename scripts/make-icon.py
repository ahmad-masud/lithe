#!/usr/bin/env python3
"""Draws the app icon and packs it for this platform.

    make-icon.py            # the format this platform wants
    make-icon.py --icns     # assets/lithe.icns, for the Mac bundle
    make-icon.py --ico      # assets/lithe.ico, for the Windows .exe

Pure stdlib: writes a PNG by hand with zlib. The .icns is handed to macOS's
own sips and iconutil; the .ico is packed here, because Windows has no
equivalent pair of tools and the format is a directory of bitmaps. Keeps a
design dependency out of the build either way.
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


def png_bytes(data, size):
    raw = b"".join(
        b"\x00" + data[y * size * 4 : (y + 1) * size * 4] for y in range(size)
    )

    def chunk(tag, payload):
        body = tag + payload
        return struct.pack(">I", len(payload)) + body + struct.pack(
            ">I", zlib.crc32(body) & 0xFFFFFFFF
        )

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def write_png(path, data):
    with open(path, "wb") as f:
        f.write(png_bytes(data, SIZE))


def scaled(data, size):
    """`data` at `size`, by averaging each source box into one pixel.

    A box filter, not a nearest-neighbour pick: at 16x16 the caret is two
    source pixels wide, and dropping rather than averaging them is what
    makes a small icon flicker between having one and not.
    """
    out = bytearray(size * size * 4)
    edges = [x * SIZE // size for x in range(size + 1)]
    for y in range(size):
        y0, y1 = edges[y], max(edges[y + 1], edges[y] + 1)
        for x in range(size):
            x0, x1 = edges[x], max(edges[x + 1], edges[x] + 1)
            totals = [0, 0, 0, 0]
            for sy in range(y0, y1):
                row = sy * SIZE * 4
                for sx in range(x0, x1):
                    i = row + sx * 4
                    for c in range(4):
                        totals[c] += data[i + c]
            count = (y1 - y0) * (x1 - x0)
            i = (y * size + x) * 4
            for c in range(4):
                out[i + c] = totals[c] // count
    return bytes(out)


def dib(data, size):
    """One .ico entry as a bottom-up 32-bit DIB.

    The header claims twice the height because an icon is historically two
    bitmaps, colour over mask. The mask is left empty: with an alpha channel
    nothing reads it, but it has to be there for the size arithmetic to add
    up.
    """
    header = struct.pack(
        "<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, size * size * 4, 0, 0, 0, 0
    )
    pixels = bytearray()
    for y in range(size - 1, -1, -1):
        for x in range(size):
            i = (y * size + x) * 4
            r, g, b, a = data[i : i + 4]
            pixels += bytes((b, g, r, a))
    mask_row = ((size + 31) // 32) * 4
    return header + bytes(pixels) + bytes(mask_row * size)


def write_ico(path, base):
    """Packs the sizes Windows actually asks for into one .ico.

    Explorer, the taskbar and Alt-Tab each pick a different one, and a file
    holding only the largest is scaled down by the shell at display time —
    which is what blurry application icons are.
    """
    sizes = (16, 24, 32, 48, 64, 128, 256)
    # 256 goes in as a PNG, which is what the format gained it for: as a DIB
    # it alone would be a quarter of a megabyte.
    images = []
    for size in sizes:
        pixels = scaled(base, size)
        images.append(png_bytes(pixels, size) if size == 256 else dib(pixels, size))

    offset = 6 + 16 * len(sizes)
    directory = b""
    for size, image in zip(sizes, images):
        directory += struct.pack(
            "<BBBBHHII",
            size % 256,  # 256 is written as 0; the field is one byte.
            size % 256,
            0,
            0,
            1,
            32,
            len(image),
            offset,
        )
        offset += len(image)

    with open(path, "wb") as f:
        f.write(struct.pack("<HHH", 0, 1, len(sizes)) + directory + b"".join(images))
    return path


def build_icns(assets, base_png):
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
                ["sips", "-z", str(px), str(px), base_png, "--out",
                 os.path.join(iconset, name)],
                check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
    out = os.path.join(assets, "lithe.icns")
    subprocess.run(["iconutil", "-c", "icns", iconset, "-o", out], check=True)
    subprocess.run(["rm", "-rf", iconset], check=True)
    return out


def main():
    here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    assets = os.path.join(here, "assets")
    os.makedirs(assets, exist_ok=True)
    base_png = os.path.join(assets, "icon-1024.png")
    pixels = draw()
    write_png(base_png, pixels)

    asked = [a.lstrip("-") for a in sys.argv[1:]]
    if not asked:
        asked = ["icns" if sys.platform == "darwin" else "ico"]
    for kind in asked:
        if kind == "icns":
            print(build_icns(assets, base_png))
        elif kind == "ico":
            print(write_ico(os.path.join(assets, "lithe.ico"), pixels))
        else:
            print(f"unknown format {kind!r}; expected icns or ico", file=sys.stderr)
            return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
