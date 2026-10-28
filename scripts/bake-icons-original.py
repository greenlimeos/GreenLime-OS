#!/usr/bin/env python3
"""Bake the ORIGINAL power-button glyph (settings returned to icons8).

Why this exists: the Start-card chrome came from the icons8 "fluency"
set. The settings glyph read like a Windows-logo twin — it was redrawn
as a gear here first, then per the user's pick it RENT back to the
fluency "gear" cog (baked by bake-icons.py; the swap is documented in
the README). What stays permanently original is the POWER button: a
hand-geometried ring + handle, so the license story for chrome is
"icons8 where it's colorful, ours where it's the power button".

Output law (mirrors bake-icons.py verbatim):
  - raw straight-alpha RGBA, row-major, 4 B/px, exact sizes
  - every color channel clamped <= 238 (cursor-uniqueness invariant:
    the pointer stays the only all->=239 blob on screen)
  - file: power_24.rgba

The palette is SAMPLED from the previous artwork (alpha-weighted mean
per vertical half), so the glyph keeps the set's color feel while the
shape is fully original. Pure stdlib: no PIL, no network, no PNG
decode — the geometry IS the source. The gear_test routine below is
the preserved template for any future original chrome icon.

Usage: python3 scripts/bake-icons-original.py
"""

import math
import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "assets", "icons")

# Fallback slate (the old fluency settings look like blue-slate anyway).
TOP_FLB, BOT_FLB = (121, 142, 168), (62, 79, 105)

CLAMP = 238  # cursor-uniqueness invariant: nothing reaches 239+


def sample_stops(size, src_name):
    """Alpha-weighted mean color of the top / bottom bands of the
    previous artwork — the two gradient stops for the redraw."""
    try:
        b = open(os.path.join(OUT, f"{src_name}_{size}.rgba"), "rb").read()
    except OSError:
        return TOP_FLB, BOT_FLB
    acc = {0: [0, 0, 0, 0.0], 1: [0, 0, 0, 0.0]}
    h = size
    for y in range(h):
        for x in range(size):
            i = 4 * (y * size + x)
            r, g, bl, a = b[i], b[i + 1], b[i + 2], b[i + 3]
            if a < 48:
                continue
            half = 0 if y < h * 0.42 else 1
            w = a / 255.0
            slot = acc[half]
            slot[0] += r * w
            slot[1] += g * w
            slot[2] += bl * w
            slot[3] += w
    tops = [tuple(int(slot[k] / slot[3]) if slot[3] > 0 else 0
                  for k in range(3)) for slot in acc.values()]
    if tops[0][2] == 0 and tops[1][2] == 0:
        return TOP_FLB, BOT_FLB
    return tops[0] or TOP_FLB, tops[1] or BOT_FLB


def draw(size, top, bot, test):
    """Per-pixel coverage with 3x3 supersampling; vertical 2-stop
    gradient, straight alpha, clamped."""
    c = size / 2.0 - 0.5
    ss = 3
    out = bytearray(size * size * 4)
    for py in range(size):
        for px in range(size):
            cov = 0
            for sy in range(ss):
                for sx in range(ss):
                    x = px + (sx + 0.5) / ss - c
                    y = py + (sy + 0.5) / ss - c
                    if test(x, y, size):
                        cov += 1
            a = cov * 255 // (ss * ss)
            i = 4 * (py * size + px)
            t = py / max(1.0, size - 1.0)
            for k in range(3):
                out[i + k] = min(CLAMP, round(top[k] + (bot[k] - top[k]) * t))
            out[i + 3] = a
    return bytes(out)


def gear_test(x, y, size):
    d = math.hypot(x, y)
    R = size / 2.0 - 1.0
    if d >= R:                       # tooth outer rim
        return False
    if d <= R * 0.36:                # the hole
        return False
    # 8 teeth: within the tooth's arc, the wheel grows to the rim.
    teeth, tw = 8, 0.30
    phase = math.pi / teeth
    ang = (math.atan2(y, x) + phase) % (2 * math.pi / teeth)
    return True if ang < tw * (2 * math.pi / teeth) else d < R * 0.72


def power_test(x, y, size):
    c = size / 2.0 - 0.5
    # Ring: center radius 0.34*size, half-thickness ~2.4px (24px-scaled),
    # with the classic gap around the top direction (-y).
    rc, half = size * 0.34, size * 0.10
    d = math.hypot(x, y)
    if rc - half <= d <= rc + half:
        dif = abs(((math.degrees(math.atan2(y, x)) + 90.0 + 180.0)
                   % 360.0) - 180.0)
        if dif > 52.0:               # 52 deg half-gap ≈ top opening
            return True
    # Handle: vertical stadium from above the ring's top to the center.
    hb = size * 0.075
    if abs(x) <= hb and y <= 0 and y >= -size * 0.38:
        return True
    return False


def main():
    # Power only since M18.1: Settings went BACK to icons8's fluency
    # "gear" cog (a baked blob from bake-icons.py, per the user's pick —
    # README documents the swap). This script keeps being the source of
    # truth for the original power button, and the gear_* drawing law
    # stays here as the template if another chrome icon needs original
    # art again.
    for name, sizes, fn in (
            ("power", (24,), power_test),
    ):
        for size in sizes:
            top, bot = sample_stops(size, "power")
            blob = draw(size, top, bot, fn)
            dst = os.path.join(OUT, f"{name}_{size}.rgba")
            open(dst, "wb").write(blob)
            nz = sum(1 for i in range(3, len(blob), 4) if blob[i] > 0)
            print(f"baked {name}_{size}.rgba  {size}x{size}  \
opaque px {nz}/{size*size}  stops {top}/{bot}", flush=True)


if __name__ == "__main__":
    main()
