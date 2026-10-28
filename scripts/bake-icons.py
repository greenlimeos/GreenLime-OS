#!/usr/bin/env python3
"""Bake the kernel's UI icons (M16.x) from icons8 ("fluency" style).

The kernel never decodes PNGs at runtime — this script is the only
PNG consumer: it downloads the pinned icons8 art once into
assets/icons-src/, decodes it with the Python stdlib (zlib), clamps
every color channel to <= 238 (the cursor-uniqueness invariant: the
pointer must stay the only all-channels->=239 blob on screen), and
writes raw straight-alpha RGBA blobs (row-major, 4 B/px) into
assets/icons/ for kernel/include_bytes! embedding at the EXACT sizes
the painters want — no runtime scaling anywhere.

Sources are committed too, so rebuilding the blobs needs no network:
    python3 scripts/bake-icons.py

Icons: https://icons8.com (fluency set) — free license, attribution
kept in README ("Icons by icons8").
"""

import os
import struct
import subprocess
import sys
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "assets", "icons-src")
OUT = os.path.join(ROOT, "assets", "icons")
CDN = "https://img.icons8.com/fluency/{size}/{slug}.png"

# name -> (slug, [sizes]); every size gets its own exact-size blob.
ICONS = {
    "folder": ("folder-invoices", [24, 28, 30]),   # Files pin/tile/grid dir
    "console": ("console", [24, 28]),               # Terminal pin/tile
    "settings": ("gear", [24, 28]),                 # Settings pin/tile
    # ^ the fluency "settings" glyph read like the Windows logo (four
    # panes); "gear" is a plain single cog at the same flavor (M18.1,
    # verified by scripts/probe-gear.py's ASCII shape maps).
    "monitor": ("monitor", [20]),                   # Files place: Desktop
    "docsfold": ("documents-folder", [20]),         # Files place: Documents
    "dload": ("download", [20]),                    # Files place: Downloads
    "bin": ("recycle-bin", [20]),                   # Files place: Recycle Bin
    "page": ("documents", [28]),                    # Files grid: file
    "rocket": ("rocket", [28]),                     # Demo tile
    # "power" left out deliberately (M18.1): the Start-card button is
    # ORIGINAL in-tree art — scripts/bake-icons-original.py draws it, so
    # bake-icons.py must never clobber it back to the icons8 shutdown.
}


def fetch(slug, size, name):
    os.makedirs(SRC, exist_ok=True)
    dst = os.path.join(SRC, f"{name}_{size}.png")
    url = CDN.format(size=size, slug=slug)
    if os.path.exists(dst) and os.path.getsize(dst) > 400:
        return dst  # committed source: offline reproducible
    subprocess.run(["curl", "-s", "-o", dst, "-w", "%{http_code}",
                    url], check=False, stdout=subprocess.PIPE)
    if not os.path.exists(dst) or os.path.getsize(dst) < 400:
        sys.exit(f"fetch failed for {url}")
    return dst


def read_png(path):
    """Minimal stdlib PNG decode: 8-bit, non-interlaced, color types
    0/2/3/4/6 -> (w, h, bytes as RGBA)."""
    data = open(path, "rb").read()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", f"{path}: not a PNG"
    pos, w, h, depth, ctype, interlace = 8, 0, 0, 0, 0, 0
    plte, trns, idat = None, None, b""
    while pos < len(data):
        ln = struct.unpack(">I", data[pos:pos + 4])[0]
        kind = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + ln]
        pos += 12 + ln
        if kind == b"IHDR":
            w, h, depth, ctype, _, _, interlace = struct.unpack(">IIBBBBB", body)
        elif kind == b"PLTE":
            plte = body
        elif kind == b"TRNS":
            trns = body
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
    assert depth == 8 and interlace == 0, f"{path}: depth/interlace unsupported"
    channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[ctype]
    raw = zlib.decompress(idat)
    bpp = channels  # 8-bit depth: one byte per channel
    stride = w * bpp
    # reverse per-row filters
    out = bytearray()
    prev = bytearray(stride)
    o = 0
    for _ in range(h):
        f = raw[o]
        o += 1
        line = bytearray(raw[o:o + stride])
        o += stride
        if f == 1:
            for i in range(bpp, stride):
                line[i] = (line[i] + line[i - bpp]) & 0xFF
        elif f == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif f == 3:
            for i in range(stride):
                a = line[i - bpp] if i >= bpp else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 0xFF
        elif f == 4:
            for i in range(stride):
                a = line[i - bpp] if i >= bpp else 0
                b = prev[i]
                c = prev[i - bpp] if i >= bpp else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 0xFF
        out += line
        prev = line
    # to RGBA
    rgba = bytearray(w * h * 4)
    if ctype == 6:
        rgba = out
    elif ctype == 2:
        for i in range(w * h):
            rgba[4 * i:4 * i + 3] = out[3 * i:3 * i + 3]
            rgba[4 * i + 3] = 255
    elif ctype == 0:
        for i in range(w * h):
            g = out[i]
            rgba[4 * i:4 * i + 3] = bytes((g, g, g))
            rgba[4 * i + 3] = 255
    elif ctype == 4:
        for i in range(w * h):
            g = out[2 * i]
            rgba[4 * i:4 * i + 3] = bytes((g, g, g))
            rgba[4 * i + 3] = out[2 * i + 1]
    elif ctype == 3:
        n = len(plte or b"") // 3
        for i in range(w * h):
            idx = out[i]
            assert idx < n, f"{path}: palette index out of range"
            rgba[4 * i:4 * i + 3] = plte[3 * idx:3 * idx + 3]
            alpha = trns[idx] if (trns and idx < len(trns)) else 255
            rgba[4 * i + 3] = alpha
    return w, h, bytes(rgba)


def main():
    for name, (slug, sizes) in ICONS.items():
        for size in sizes:
            png = fetch(slug, size, name)
            w, h, rgba = read_png(png)
            assert (w, h) == (size, size), f"{name}: got {w}x{h}, want {size}"
            b = bytearray(rgba)
            # Cursor-uniqueness clamp: no baked channel may reach 239+
            # (the pointer stays the only all->=239 blob on screen).
            for i in range(0, len(b), 4):
                if b[i] > 238: b[i] = 238
                if b[i + 1] > 238: b[i + 1] = 238
                if b[i + 2] > 238: b[i + 2] = 238
            os.makedirs(OUT, exist_ok=True)
            dst = os.path.join(OUT, f"{name}_{size}.rgba")
            open(dst, "wb").write(bytes(b))
            nz = sum(1 for i in range(3, len(b), 4) if b[i] > 0)
            print(f"baked {name}_{size}.rgba  {w}x{h}  opaque px {nz}/{w*h}")


if __name__ == "__main__":
    main()
