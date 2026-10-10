#!/usr/bin/env python3
"""Minimal PPM (P6) -> PNG converter + terminal-region pixel stats.

Usage: python3 scripts/ppm2png.py input.ppm output.png
       python3 scripts/ppm2png.py --stats input.ppm [x0 y0 x1 y1]
"""
import struct
import sys
import zlib


def read_ppm(path):
    with open(path, "rb") as f:
        assert f.readline().strip() == b"P6", "not a P6 ppm"
        dims = []
        while len(dims) < 3:
            line = f.readline()
            if line.startswith(b"#"):
                continue
            dims += line.split()
        w, h = int(dims[0]), int(dims[1])
        _max = int(dims[2])
        px = f.read(w * h * 3)
    return w, h, px


def write_png(path, w, h, rgb):
    raw = b"".join(b"\x00" + rgb[y * w * 3:(y + 1) * w * 3] for y in range(h))

    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 6))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)


def main():
    if sys.argv[1] == "--stats":
        ppm = sys.argv[2]
        x0, y0, x1, y1 = (map(int, sys.argv[3:7])
                          if len(sys.argv) > 6 else (0, 0, 10**9, 10**9))
        w, h, px = read_ppm(ppm)
        x1, y1 = min(x1, w), min(y1, h)
        n = 0
        for y in range(y0, y1):
            for x in range(x0, x1):
                o = (y * w + x) * 3
                if px[o] + px[o + 1] + px[o + 2] > 90:  # >~30 avg: not BG
                    n += 1
        print(f"{ppm}: region({x0},{y0})-({x1},{y1}) non-bg px = {n}")
        return 0
    w, h, px = read_ppm(sys.argv[1])
    write_png(sys.argv[2], w, h, px)
    print(f"{sys.argv[1]} -> {sys.argv[2]} ({w}x{h})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
