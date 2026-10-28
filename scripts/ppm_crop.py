#!/usr/bin/env python3
"""Crop regions from test screendumps (PPM) into one preview PNG so the
baked icons8 icons can be eyeballed on real QEMU output (no PIL in WSL,
so this is a hand-rolled PPM reader + PNG writer)."""

import struct
import sys
import zlib


def read_ppm(path):
    with open(path, "rb") as f:
        assert f.readline().strip() == b"P6"
        dims = []
        while len(dims) < 3:
            line = f.readline()
            if line.startswith(b"#"):
                continue
            dims += line.split()
        w, h = int(dims[0]), int(dims[1])
        px = f.read()
    return w, h, px


def png_write(path, w, h, rgb):
    rows = b"".join(b"\x00" + rgb[y * w * 3:(y + 1) * w * 3] for y in range(h))
    comp = zlib.compress(rows, 9)

    def chunk(kind, body):
        c = struct.pack(">I", len(body)) + kind + body
        return c + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)

    hdr = struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)
    open(path, "wb").write(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", hdr)
        + chunk(b"IDAT", comp)
        + chunk(b"IEND", b"")
    )


def crop(w, h, px, x0, y0, cw, ch):
    rows = []
    for y in range(y0, y0 + ch):
        s = (y * w + x0) * 3
        rows.append(px[s:s + cw * 3])
    return b"".join(rows)


def main():
    src = sys.argv[1]  # full-frame PPM screendump (1280x800)
    (x, y, w, h) = map(int, sys.argv[2:6])
    out = sys.argv[6]
    W, H, px = read_ppm(src)
    assert x + w <= W and y + h <= H, \
        f"crop {x},{y} {w}x{h} outside {W}x{H} shot"
    png_write(out, w, h, crop(W, H, px, x, y, w, h))
    print(f"wrote {out} ({w}x{h} crop of the full {W}x{H} shot)")


if __name__ == "__main__":
    main()
