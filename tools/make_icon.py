"""Writes the printer icon (printer-icons): an e-paper tablet showing a recipe.

Usage: python3 tools/make_icon.py crates/ipp-http/assets/icon.png [size]
Only the standard library is used (zlib + struct for PNG).
"""
import struct
import sys
import zlib


def rounded_rect(x, y, x0, y0, x1, y1, r):
    if not (x0 <= x < x1 and y0 <= y < y1):
        return False
    cx = min(max(x, x0 + r), x1 - 1 - r)
    cy = min(max(y, y0 + r), y1 - 1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def pixel(x, y, s):
    """Gray level and alpha at (x, y) for an s×s icon, drawn on a 128 grid."""
    u, v = x * 128 / s, y * 128 / s
    if not rounded_rect(u, v, 22, 6, 106, 122, 10):
        return 0, 0  # transparent
    if not rounded_rect(u, v, 30, 14, 98, 106, 3):
        return 40, 255  # dark bezel
    # Paper-white screen with a title bar and text lines, like a recipe.
    lines = [(34, 37, 38, 90), (42, 45, 38, 78), (50, 53, 38, 88),
             (62, 65, 38, 84), (70, 73, 38, 90), (78, 81, 38, 70), (90, 93, 38, 82)]
    if 20 <= v < 28 and 38 <= u < 72:
        return 60, 255
    for y0, y1, x0, x1 in lines:
        if y0 <= v < y1 and x0 <= u < x1:
            return 120, 255
    return 235, 255


def main():
    out = sys.argv[1]
    size = int(sys.argv[2]) if len(sys.argv) > 2 else 128
    raw = bytearray()
    for y in range(size):
        raw.append(0)  # filter: none
        for x in range(size):
            raw.extend(pixel(x, y, size))

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 4, 0, 0, 0))  # gray + alpha
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    with open(out, "wb") as f:
        f.write(png)


main()
