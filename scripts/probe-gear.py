#!/usr/bin/env python3
"""Probe icons8 fluency gear candidates: fetch, decode, ASCII-visualize.

The fluency 'settings' glyph read like the Windows logo (four panes);
pick a proper cog instead. This prints each candidate's alpha map as
ASCII so the pick is an actual eyeball decision, not a slug guess.
Run from the repo root: python3 scripts/probe-gear.py
"""

import importlib.util
import os
import subprocess

_here = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location(
    "bakeoidx", os.path.join(_here, "bake-icons.py"))
bake = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(bake)
read_png = bake.read_png

CDN = "https://img.icons8.com/fluency/{size}/{slug}.png"
CANDIDATES = ["gears", "gear", "cog", "settings-gear", "mechanical-gear",
              "decorative-gear", "servicemark"]

RAMP = " .:-=+*#%@"


def ascii_map(size, w, h, rgba):
    for y in range(0, h, max(1, h // 30)):
        row = ""
        for x in range(0, w, max(1, size // 30)):
            a = rgba[4 * (y * size + x) + 3]
            row += RAMP[min(9, a * 10 // 256)]
        print("      " + row)


def main():
    for slug in CANDIDATES:
        dst = f"/tmp/gear-probe-{slug}.png"
        code = subprocess.run(
            ["curl", "-s", "-o", dst, "-w", "%{http_code}",
             CDN.format(size=28, slug=slug)],
            stdout=subprocess.PIPE).stdout.decode()
        if code != "200" or not os.path.exists(dst) \
                or os.path.getsize(dst) < 400:
            print(f"{slug:18s} HTTP {code} — MISSING", flush=True)
            if os.path.exists(dst) and os.path.getsize(dst) <= 400:
                os.unlink(dst)
            continue
        try:
            size, _, rgba = read_png(dst)
            if size != 28:
                print(f"{slug:18s} got {size}px, need 28", flush=True)
                continue
            print(f"{slug:18s} OK:", flush=True)
            ascii_map(28, 28, 28, rgba)
        except Exception as e:  # noqa: BLE001 probe may fail hard
            print(f"{slug:18s} decode failed: {e}", flush=True)
        print(flush=True)


if __name__ == "__main__":
    main()
