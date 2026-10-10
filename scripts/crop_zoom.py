#!/usr/bin/env python3
"""Crop + nearest-neighbor zoom a PPM region, output PNG (for reading text).

Usage: python3 scripts/crop_zoom.py in.ppm out.png x0 y0 x1 y1 [scale]
"""
import sys

sys.path.insert(0, "scripts")
from ppm2png import read_ppm, write_png  # noqa: E402

ppm, out, x0, y0, x1, y1 = (sys.argv[1], sys.argv[2], *map(int, sys.argv[3:7]))
scale = int(sys.argv[7]) if len(sys.argv) > 7 else 3
w, h, px = read_ppm(ppm)
cw, ch = x1 - x0, y1 - y0
buf = bytearray()
for y in range(ch):
    row = bytes(px[(y0 + y) * w * 3 + x0 * 3:(y0 + y) * w * 3 + x1 * 3])
    scaled = b"".join(row[i:i + 3] * scale for i in range(0, len(row), 3))
    buf += scaled * scale  # horizontal then vertical nearest-neighbor
write_png(out, cw * scale, ch * scale, bytes(buf))
print(f"{out}: {cw * scale}x{ch * scale}")
