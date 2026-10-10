#!/usr/bin/env python3
"""1:1 pixel dump of a dist/fb.ppm region (exact glyph shapes)."""
import sys

PPM = "dist/fb.ppm"
X0, Y0, W, H = (int(a) for a in sys.argv[1:5])
SHADES = " .:-=+*#%@"

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

for gy in range(Y0, min(Y0 + H, h)):
    row = []
    for gx in range(X0, min(X0 + W, w)):
        o = (gy * w + gx) * 3
        v = (px[o] + px[o + 1] + px[o + 2]) // 3
        row.append(SHADES[min(v * len(SHADES) // 256, len(SHADES) - 1)])
    print("".join(row).rstrip())
