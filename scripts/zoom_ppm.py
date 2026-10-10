#!/usr/bin/env python3
"""Zoom ASCII art of dist/fb.ppm region to read actual glyphs."""
import sys

PPM = "dist/fb.ppm"
# Region: x0,y0,w,h in pixels; S = sample step (2 = each char is 2x2 px).
X0, Y0, W, H, S = int(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), 2

with open(PPM, "rb") as f:
    assert f.readline().strip() == b"P6"
    dims = []
    while len(dims) < 3:
        line = f.readline()
        if line.startswith(b"#"):
            continue
        dims += line.split()
    w, h = int(dims[0]), int(dims[1])
    px = f.read(w * h * 3)

for gy in range(Y0, Y0 + H, S):
    row = []
    for gx in range(X0, X0 + W, S):
        lit = 0
        for dy in range(S):
            for dx in range(S):
                o = ((gy + dy) * w + (gx + dx)) * 3
                if (px[o] + px[o + 1] + px[o + 2]) // 3 > 60:
                    lit += 1
        row.append("#" if lit >= 3 else ("." if lit >= 1 else " "))
    print("".join(row).rstrip())
