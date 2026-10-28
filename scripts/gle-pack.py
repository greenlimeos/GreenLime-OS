#!/usr/bin/env python3
# gle-pack.py — the .gle packer (M17 v1, M19 v2). ZERO dependencies:
# Python 3 stdlib only.
#
# A .gle file is a flat lunchbox for a GreenLime app.
#
# v1 — the classic lunchbox (still packed when the manifest lists no
# "assets", and still 100% supported by the kernel):
#
#   [0..4)    magic "GLE1"
#   [4..6)    u16 little-endian header version (1)
#   [6..8)    u16 flags — bit0 = GUI (the app opens a window)
#   [8]       u8  name_len   (1..24, ASCII)
#   [9]       u8  ver_len    (0..8)
#   [10]      u8  author_len (0..24)
#   [11]      u8  icon_w     (0 = no icon, else exactly 28)
#   [12..16)  u32 elf_len
#   [16..20)  u32 crc32 over [0..16) ++ [20..EOF)   (everything but itself)
#   [20..)    name || version || author || icon RGBA (28*28*4, straight
#             alpha, alpha bytes pre-clamped <= 238) || static ELF payload
#
# v2 — the modern container (packed when the manifest lists "assets"):
# bundles the app with named-order asset blobs that the KERNEL maps
# read-only into the task's asset window at exec (no installer, nothing
# is unpacked at runtime — the app reads them as plain memory):
#
#   [0..4)    magic "GLE1"        (same; the version field tells)
#   [4..6)    u16 version = 2
#   [6..8)    u16 flags           (bit0 = GUI)
#   [8..12)   u32 meta_len        (9..4096)
#   [12..16)  u32 entry_count     (1..32)
#   [16..20)  u32 crc32 over [0..16) ++ [20..EOF)   (the v1 slot law)
#   [20..20+meta_len)  meta: "GLE2M1", u8 nl/vl/al, ASCII
#             name || version || author (author now <= 32), then a
#             freeform ASCII tail (JSON: {"assets": [...]}).
#   [table: entry_count x 32 bytes, right after the meta block:
#     [0..4)   u32 len    exact blob byte length
#     [4..8)   u32 off    byte offset, every entry 4096-aligned
#     [8..12)  u32 crc32  over the exact blob bytes
#     [12]     u8  kind   0 = main ELF (exactly one), 1 = icon 28x28
#                         straight-alpha RGBA (zero or one), 2 = asset
#     [13]     u8  flags  0
#     [14..32) 22 reserved bytes, 0]
#   blobs, page-aligned ascending; every gap byte is ZERO; the last one
#   ends at EOF exactly. Whole-container cap: 8 MiB.
#
# The KERNEL never decodes PNGs and never resizes pixels: the pack tool
# bakes the icon to exactly 28x28 raw RGBA before it goes in the box.
#
# Usage:
#   python3 scripts/gle-pack.py mkicon [out.png]           placeholder icon
#   python3 scripts/gle-pack.py pack app.json --elf ELF --out OUT.gle
#   python3 scripts/gle-pack.py inspect app.gle            show the sticker
#   python3 scripts/gle-pack.py extract app.gle --dir DIR  unpack pieces
#
# Exit code 0 = success, 1 = refused (with a plain-English reason).

import json
import os
import struct
import sys
import zlib

GLE_HDR = 20
GLE_MAGIC = b"GLE1"
GLE_VER = 1
GLE_FLAG_GUI = 1
GLE_NAME_MAX = 24
GLE_VER_MAX = 8
GLE_AUTHOR_MAX = 24
GLE_ICON_W = 28
GLE_ICON_CLAMP = 238  # kernel invariant: cursor pixels stay unique
GLE_MAX = 1024 * 1024

# --- v2 (M19): every number in step with the kernel's parse law ---
GLE2_VER = 2
GLE2_AUTHOR_MAX = 32
GLE2_META_MAGIC = b"GLE2M1"
GLE2_META_MAX = 4096
GLE2_ENTRY_MAX = 32
GLE2_KIND_ELF = 0
GLE2_KIND_ICON = 1
GLE2_KIND_ASSET = 2
GLE2_ENTRY_REC = 32
GLE2_PAGE = 4096
GLE2_MAX = 8 * 1024 * 1024

PNG_SIG = b"\x89PNG\r\n\x1a\n"


def die(msg):
    raise SystemExit("gle-pack: " + msg)


# ---------------------------------------------------------------- PNG writing

def png_chunks(tag, data):
    out = bytearray()
    out += struct.pack(">I", len(data))
    out += tag
    out += data
    out += struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    return bytes(out)


def write_png(path, w, h, rgba):
    """RGBA bytes -> 8-bit non-interlaced RGBA PNG (type 6)."""
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
    raw = bytearray()
    for y in range(h):
        raw.append(0)  # filter type 0 (None) per scanline
        o = y * w * 4
        raw += rgba[o : o + w * 4]
    png = PNG_SIG
    png += png_chunks(b"IHDR", ihdr)
    png += png_chunks(b"IDAT", zlib.compress(bytes(raw), 9))
    png += png_chunks(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)


# ---------------------------------------------------------------- PNG reading

class Png:
    __slots__ = ("w", "h", "rgba")


def read_png(path):
    """Minimal stdlib PNG decoder: bit depth 8, color types 0/2/3/4/6,
    non-interlaced. Enough for any icon a .gle author will feed us."""
    with open(path, "rb") as f:
        b = f.read()
    if b[:8] != PNG_SIG:
        die(f"{path}: not a PNG")
    pos = 8
    ihdr = None
    plte = None
    trns = None
    idat = bytearray()
    while pos + 8 <= len(b):
        (ln,) = struct.unpack_from(">I", b, pos)
        tag = b[pos + 4 : pos + 8]
        data = b[pos + 8 : pos + 8 + ln]
        if tag == b"IHDR":
            ihdr = data
        elif tag == b"PLTE":
            plte = data
        elif tag == b"tRNS":
            trns = data
        elif tag == b"IDAT":
            idat += data
        elif tag == b"IEND":
            break
        pos += 12 + ln
    if ihdr is None:
        die(f"{path}: no IHDR")
    w, h, depth, ctype, _comp, _filt, interlace = struct.unpack(">IIBBBBB", ihdr)
    if interlace != 0:
        die(f"{path}: interlaced PNGs unsupported (save without interlacing)")
    if depth != 8:
        die(f"{path}: only 8-bit PNGs supported")
    if w == 0 or h == 0 or w > 256 or h > 256:
        die(f"{path}: icon must be <= 256x256")
    chans = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}.get(ctype)
    if chans is None:
        die(f"{path}: unsupported PNG color type {ctype}")
    raw = zlib.decompress(bytes(idat))
    stride = w * chans
    if len(raw) != (stride + 1) * h:
        die(f"{path}: truncated pixel data")
    rows = []
    prev = bytearray(stride)
    for y in range(h):
        ft = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1 : (y + 1) * (stride + 1)])
        if ft == 1:  # Sub
            for i in range(chans, stride):
                line[i] = (line[i] + line[i - chans]) & 0xFF
        elif ft == 2:  # Up
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif ft == 3:  # Average
            for i in range(stride):
                a = line[i - chans] if i >= chans else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 0xFF
        elif ft == 4:  # Paeth
            for i in range(stride):
                a = line[i - chans] if i >= chans else 0
                bb = prev[i]
                c = prev[i - chans] if i >= chans else 0
                p = a + bb - c
                pa, pb, pc = abs(p - a), abs(p - bb), abs(p - c)
                pr = a if (pa <= pb and pa <= pc) else (bb if pb <= pc else c)
                line[i] = (line[i] + pr) & 0xFF
        rows.append(bytes(line))
        prev = line
    # Expand to straight-alpha RGBA.
    out = bytearray(w * h * 4)
    for y in range(h):
        line = rows[y]
        for x in range(w):
            i = x * chans
            o = (y * w + x) * 4
            if ctype == 0:
                g = line[i]
                out[o : o + 4] = bytes((g, g, g, 255))
            elif ctype == 2:
                out[o : o + 3] = line[i : i + 3]
                out[o + 3] = 255
            elif ctype == 3:
                idx = line[i]
                if plte is None or idx * 3 + 3 > len(plte):
                    die(f"{path}: bad palette entry")
                out[o : o + 3] = plte[idx * 3 : idx * 3 + 3]
                out[o + 3] = (
                    trns[idx] if trns is not None and idx < len(trns) else 255
                )
            elif ctype == 4:
                g = line[i]
                out[o : o + 4] = bytes((g, g, g, line[i + 1]))
            else:  # 6
                out[o : o + 4] = line[i : i + 4]
    png = Png()
    png.w, png.h, png.rgba = w, h, bytes(out)
    return png


def resize_nearest(png, w, h):
    """Nearest-neighbor scale to exactly w x h (kernel gives us no scaler)."""
    if png.w == w and png.h == h:
        return png.rgba
    out = bytearray(w * h * 4)
    for y in range(h):
        sy = min(png.h - 1, y * png.h // h)
        for x in range(w):
            sx = min(png.w - 1, x * png.w // w)
            o = (y * w + x) * 4
            s = (sy * png.w + sx) * 4
            out[o : o + 4] = png.rgba[s : s + 4]
    return bytes(out)


def clamp_alpha(rgba):
    """Straight alpha stays straight; the ALPHA bytes are clamped <= 238 so
    no packed app icon can collide with the system cursor's alpha trick."""
    b = bytearray(rgba)
    for i in range(3, len(b), 4):
        if b[i] > GLE_ICON_CLAMP:
            b[i] = GLE_ICON_CLAMP
    return bytes(b)


# ---------------------------------------------------------------- containers

def build_container(name, version, author, gui, icon_rgba, elf):
    """All the byte law from docs/GLE.md lives right here (v1)."""
    name_b = _sticker_b(name, GLE_NAME_MAX, "name", require=True)
    ver_b = _sticker_b(version, GLE_VER_MAX, "version")
    auth_b = _sticker_b(author, GLE_AUTHOR_MAX, "author")
    nl, vl, al = len(name_b), len(ver_b), len(auth_b)
    flags = GLE_FLAG_GUI if gui else 0
    iw = GLE_ICON_W if icon_rgba else 0
    elf_len = len(elf)
    total = GLE_HDR + nl + vl + al + len(icon_rgba or b"") + elf_len
    if total > GLE_MAX:
        die(f"packed size {total} bytes exceeds the kernel's 1 MiB GLE_MAX")
    hdr = bytearray()
    hdr += GLE_MAGIC
    hdr += struct.pack("<HH", GLE_VER, flags)
    hdr += bytes((nl, vl, al, iw))
    hdr += struct.pack("<I", elf_len)
    assert len(hdr) == 16
    # CRC law (docs/GLE.md): header-prefix + stickers + icon + ELF —
    # i.e. everything AFTER the CRC field itself.
    crc = (
        zlib.crc32(
            bytes(hdr[:16])
            + name_b
            + ver_b
            + auth_b
            + bytes(icon_rgba or b"")
            + elf
        )
        & 0xFFFFFFFF
    )
    # CRC field lives at [16..20): it covers header-before-crc + icon + elf.
    out = bytearray(hdr[:16])
    out += struct.pack("<I", crc)
    out += name_b
    out += ver_b
    out += auth_b
    out += icon_rgba or b""
    out += elf
    return bytes(out)


def _page_align(n):
    return (n + GLE2_PAGE - 1) & ~(GLE2_PAGE - 1)


def build_container_v2(name, version, author, gui, icon_rgba, elf, asset_paths):
    """The v2 law (docs/GLE.md): meta block + 32-byte entry records +
    page-aligned, gap-zeroed blobs. Every blob gets its own CRC; the
    whole container keeps the v1-slot CRC law."""
    name_b = _sticker_b(name, GLE_NAME_MAX, "name", require=True)
    ver_b = _sticker_b(version, GLE_VER_MAX, "version")
    auth_b = _sticker_b(author, GLE2_AUTHOR_MAX, "author")
    nl, vl, al = len(name_b), len(ver_b), len(auth_b)
    flags = GLE_FLAG_GUI if gui else 0

    if not asset_paths:
        die("v2 pack needs at least one asset (leave \"assets\" out for v1)")
    assets = []
    for p in asset_paths:
        if not os.path.exists(p):
            die(f"asset not found: {p}")
        with open(p, "rb") as f:
            data = f.read()
        if not data:
            die(f"{p}: empty asset (zero-page blobs serve nobody)")
        assets.append(data)

    # Entries: main ELF first, then the icon, then assets in manifest
    # order. The ORDER is the descriptor contract: the app fetches
    # assets by index, so the manifest order is the app's ABI.
    if len(elf) < 64:
        die("v2: the main ELF is suspiciously small (<64 B)")
    entries = [(GLE2_KIND_ELF, "elf", elf)]
    if icon_rgba:
        entries.append((GLE2_KIND_ICON, "icon", icon_rgba))
    for i, data in enumerate(assets):
        entries.append((GLE2_KIND_ASSET, os.path.basename(asset_paths[i]), data))
    if len(entries) > GLE2_ENTRY_MAX:
        die(f"{len(entries)} entries, the container law caps at {GLE2_ENTRY_MAX}")

    # Meta: fixed prefix + birth-sticker + freeform ASCII tail (JSON).
    # (The kernel reads ONLY the prefix + the three sticker lengths and
    # checks the tail is ASCII — the tool's JSON is the readable record.)
    tail_obj = {"assets": [nm for k, nm, _ in entries if k == GLE2_KIND_ASSET]}
    tail = json.dumps(tail_obj, separators=(",", ":"), ensure_ascii=True)
    meta = (
        GLE2_META_MAGIC
        + bytes((nl, vl, al))
        + name_b
        + ver_b
        + auth_b
        + tail.encode("ascii")
    )
    if len(meta) > GLE2_META_MAX:
        die(f"meta block is {len(meta)} bytes, max {GLE2_META_MAX}")

    table_off = GLE_HDR + len(meta)
    table_end = table_off + len(entries) * GLE2_ENTRY_REC
    # Blobs: page-aligned ascending; gaps and tail padding stay zero.
    off = _page_align(table_end)
    recs = []
    for kind, _name, data in entries:
        blob_end = off + len(data)
        recs.append((kind, len(data), off, zlib.crc32(data) & 0xFFFFFFFF))
        off = _page_align(blob_end)
    total = recs[-1][2] + recs[-1][1]
    if total > GLE2_MAX:
        die(f"packed size {total} bytes exceeds the kernel's 8 MiB GLE2_MAX")

    out = bytearray(total)  # zeros everywhere = legal gaps and padding
    out[:4] = GLE_MAGIC
    struct.pack_into("<HH", out, 4, GLE2_VER, flags)
    struct.pack_into("<II", out, 8, len(meta), len(entries))
    out[GLE_HDR : GLE_HDR + len(meta)] = meta
    for i, (kind, ln, o, ecrc) in enumerate(recs):
        e = table_off + i * GLE2_ENTRY_REC
        # <IIIBBHQII = the exact 32-byte record: len, off, entry-CRC,
        # kind, flags=0, u16 reserved, u64 reserved, u32 x2 reserved.
        struct.pack_into("<IIIBBHQII", out, e, ln, o, ecrc, kind, 0, 0, 0, 0, 0)
        data = entries[i][2]
        out[o : o + ln] = data
    # The v1 slot law: CRC over [0..16) ++ [20..EOF) — everything but itself.
    crc = zlib.crc32(bytes(out[:16]) + bytes(out[20:])) & 0xFFFFFFFF
    struct.pack_into("<I", out, 16, crc)
    return bytes(out)


def _sticker_b(s, cap, what, require=False):
    if s is None or (require and not s):
        if require:
            die(f"manifest: .gle apps need a {what}")
        return b""
    try:
        b = s.encode("ascii")
    except UnicodeEncodeError:
        die(f"manifest: {what} must be ASCII (the OS text namespace is ASCII)")
    if len(b) > cap:
        die(f"manifest: {what} is {len(b)} bytes, max {cap}")
    return b


def verify_elf(path):
    with open(path, "rb") as f:
        elf = f.read()
    if len(elf) < 64:
        die(f"{path}: not an ELF (too small)")
    if elf[:4] != b"\x7fELF":
        die(f"{path}: not an ELF (bad magic)")
    if struct.unpack_from("<H", elf, 16)[0] != 2:
        die(f"{path}: not a static ET_EXEC (e_type {struct.unpack_from('<H', elf, 16)[0]}; build with the GreenLime no_std target)")
    if struct.unpack_from("<H", elf, 18)[0] != 0x3E:
        die(f"{path}: not x86-64")
    return elf


# ---------------------------------------------------------------- commands

def cmd_mkicon(argv):
    out = argv[0] if argv else "icon.png"
    # Placeholder: a lime rounded square with a lighter "hello" dot grid —
    # replace-friendly; real art can be dropped in any time.
    w = h = GLE_ICON_W
    bg = (127, 191, 64, 238)
    lite = (170, 224, 128, 238)
    dark = (84, 140, 40, 255)
    px = bytearray(w * h * 4)
    for y in range(h):
        for x in range(w):
            o = (y * w + x) * 4
            edge = x in (0, w - 1) or y in (0, h - 1)
            if edge:
                px[o : o + 4] = bytes(bg)
            elif (x // 2) % 2 == 0 and (y // 2) % 2 == 0:
                px[o : o + 4] = bytes(lite)
            else:
                px[o : o + 4] = bytes(dark)
    write_png(out, w, h, clamp_alpha(bytes(px)))
    print(f"gle-pack: wrote placeholder icon {out} ({w}x{h} RGBA)")
    return 0


def cmd_pack(argv):
    manifest_path = argv[0]
    opts = {"--elf": None, "--out": None}
    i = 1
    while i < len(argv):
        if argv[i] in opts:
            if i + 1 >= len(argv):
                die(f"{argv[i]} needs a value")
            opts[argv[i]] = argv[i + 1]
            i += 2
        else:
            die(f"unknown argument {argv[i]}")
    try:
        with open(manifest_path, "rb") as f:
            m = json.load(f)
    except OSError as e:
        die(f"manifest: {e}")
    except ValueError as e:
        die(f"manifest: bad JSON: {e}")
    name = m.get("name")
    version = str(m.get("version", ""))
    author = str(m.get("author", ""))
    gui = bool(m.get("gui", True))
    icon_path = m.get("icon")
    icon_rgba = b""
    if icon_path:
        icon_rel = os.path.join(os.path.dirname(manifest_path) or ".", icon_path)
        png = read_png(icon_rel)
        if png.w != GLE_ICON_W or png.h != GLE_ICON_W:
            print(
                f"gle-pack: resizing icon {icon_rel} "
                f"{png.w}x{png.h} -> {GLE_ICON_W}x{GLE_ICON_W} (nearest)",
            )
        icon_rgba = clamp_alpha(resize_nearest(png, GLE_ICON_W, GLE_ICON_W))
    if not opts["--elf"]:
        die("pack needs --elf <path to built greenlime-...-app ELF>")
    elf = verify_elf(opts["--elf"])

    # v2 when the manifest lists assets; a manifest without "assets"
    # stays a v1 lunchbox (old docs/tests stay truthful).
    raw_assets = m.get("assets")
    if raw_assets is not None and not isinstance(raw_assets, list):
        die('manifest: "assets" must be a list of file paths')
    asset_paths = [
        os.path.join(os.path.dirname(manifest_path) or ".", p)
        if not os.path.isabs(p)
        else p
        for p in (raw_assets or [])
    ]
    if asset_paths:
        blob = build_container_v2(name, version, author, gui, icon_rgba, elf, asset_paths)
        fmt = "v2"
        extra = f" assets={len(asset_paths)} (manifest order = descriptor order)"
    else:
        blob = build_container(name, version, author, gui, icon_rgba, elf)
        fmt = "v1"
        extra = ""
    out = opts["--out"] or f"dist/{_sticker_b(name, GLE_NAME_MAX, 'name', True).decode()}.gle"
    out_dir = os.path.dirname(out)
    if out_dir:
        os.makedirs(out_dir, exist_ok=True)
    with open(out, "wb") as f:
        f.write(blob)
    print(
        f"gle-pack: packed {fmt} {out} ({len(blob)} bytes) app={name!r} "
        f"v={version!r} author={author!r} gui={gui} icon={bool(icon_rgba)} "
        f"elf={len(elf)} B{extra}"
    )
    return 0


def parse_container(path):
    with open(path, "rb") as f:
        b = f.read()
    if len(b) < GLE_HDR:
        die(f"{path}: truncated header")
    if b[:4] != GLE_MAGIC:
        die(f"{path}: bad magic")
    (ver,) = struct.unpack_from("<H", b, 4)
    if ver == 2:
        return parse_container_v2(path, b)
    if ver != 1:
        die(f"{path}: unknown header version {ver}")
    flags, = struct.unpack_from("<H", b, 6)
    nl, vl, al, iw = b[8], b[9], b[10], b[11]
    (elf_len,) = struct.unpack_from("<I", b, 12)
    (want,) = struct.unpack_from("<I", b, 16)
    strings = nl + vl + al
    icon = iw * iw * 4
    total = GLE_HDR + strings + icon + elf_len
    if total != len(b):
        die(f"{path}: length mismatch (header says {total}, file is {len(b)})")
    got = zlib.crc32(b[:16] + b[20:]) & 0xFFFFFFFF
    app = {
        "ver": ver,
        "gui": bool(flags & GLE_FLAG_GUI),
        "name": b[GLE_HDR : GLE_HDR + nl].decode("ascii", "replace"),
        "version": b[GLE_HDR + nl : GLE_HDR + nl + vl].decode("ascii", "replace"),
        "author": b[GLE_HDR + strings - al : GLE_HDR + strings].decode(
            "ascii", "replace"
        ),
        "icon_w": iw,
        "elf_len": elf_len,
        "crc_ok": got == want,
        "bytes": len(b),
        "elf": b[total - elf_len :],
    }
    if iw:
        app["icon_rgba"] = b[GLE_HDR + strings : GLE_HDR + strings + icon]
    return app


def parse_container_v2(path, b):
    """Same house keys as v1 (so extract keeps working) + v2 extras."""
    ver, flags = struct.unpack_from("<HH", b, 4)
    (meta_len,) = struct.unpack_from("<I", b, 8)
    (entry_count,) = struct.unpack_from("<I", b, 12)
    (want,) = struct.unpack_from("<I", b, 16)
    got = zlib.crc32(b[:16] + b[20:]) & 0xFFFFFFFF
    if meta_len < 9 or meta_len > GLE2_META_MAX:
        die(f"{path}: bad meta length {meta_len}")
    if entry_count < 1 or entry_count > GLE2_ENTRY_MAX:
        die(f"{path}: bad entry count {entry_count}")
    table_off = GLE_HDR + meta_len
    table_end = table_off + entry_count * GLE2_ENTRY_REC
    if table_end > len(b):
        die(f"{path}: truncated entry table")
    m = b[GLE_HDR:table_off]
    if m[:6] != GLE2_META_MAGIC:
        die(f"{path}: unknown meta format (no GLE2M1 prefix)")
    nl, vl, al = m[6], m[7], m[8]
    strings = nl + vl + al
    if 9 + strings > meta_len:
        die(f"{path}: truncated sticker")
    try:
        name = m[9 : 9 + nl].decode("ascii")
        version = m[9 + nl : 9 + nl + vl].decode("ascii")
        author = m[9 + nl + vl : 9 + strings].decode("ascii")
        tail = json.loads(m[9 + strings :].decode("ascii"))
    except UnicodeDecodeError:
        die(f"{path}: meta must be ASCII")
    except ValueError:
        die(f"{path}: meta tail is not JSON (tool wrote it; kernel ignores it)")
    icon_w = 0
    icon_rgba = b""
    elf = b""
    assets = []
    entries = []
    for i in range(entry_count):
        e = table_off + i * GLE2_ENTRY_REC
        (ln, off, ecrc) = struct.unpack_from("<III", b, e)
        kind, eflags = b[e + 12], b[e + 13]
        if e + 32 > len(b) or eflags != 0:
            die(f"{path}: bad entry record {i}")
        if off + ln > len(b) or (off & (GLE2_PAGE - 1)) != 0:
            die(f"{path}: entry {i} offset/length breaks the layout law")
        blob = b[off : off + ln]
        crc_ok = zlib.crc32(blob) & 0xFFFFFFFF == ecrc
        if kind == GLE2_KIND_ELF:
            elf = blob
        elif kind == GLE2_KIND_ICON:
            icon_w, icon_rgba = GLE_ICON_W, blob
        elif kind == GLE2_KIND_ASSET:
            assets.append(blob)
        entries.append(
            {
                "kind": kind,
                "len": ln,
                "off": off,
                "crc_ok": crc_ok,
            }
        )
    app = {
        "ver": ver,
        "gui": bool(flags & GLE_FLAG_GUI),
        "name": name,
        "version": version,
        "author": author,
        "icon_w": icon_w,
        "elf_len": len(elf),
        "crc_ok": got == want,
        "bytes": len(b),
        "elf": elf,
        "entries": entries,
        "assets": assets,
        "tail": tail,
    }
    if icon_w:
        app["icon_rgba"] = icon_rgba
    return app


KIND_NAMES = {GLE2_KIND_ELF: "elf", GLE2_KIND_ICON: "icon", GLE2_KIND_ASSET: "asset"}


def cmd_inspect(argv):
    app = parse_container(argv[0])
    cap = "8 MiB (v2)" if app["ver"] == 2 else "1 MiB (v1)"
    print(f"path           {argv[0]}")
    print(f"header version {app['ver']}")
    print(f"gui flag       {app['gui']} (opens a window)")
    print(f"name           {app['name']!r}")
    print(f"version        {app['version']!r}")
    print(f"author         {app['author']!r}")
    if app["ver"] == 2:
        for i, e in enumerate(app["entries"]):
            kname = KIND_NAMES.get(e["kind"], f"reserved({e['kind']})")
            print(
                f"entry {i}        {kname:6} len={e['len']:<8} off={e['off']:<8} "
                f"crc={'OK' if e['crc_ok'] else 'MISMATCH'}"
            )
        print(f"tail           {json.dumps(app.get('tail'))}")
    else:
        print(
            f"icon           {'%dx%d RGBA' % (app['icon_w'], app['icon_w']) if app['icon_w'] else 'none'}"
        )
    print(
        f"elf            {app['elf_len']} bytes "
        f"({'static ET_EXEC, x86-64' if _elf_ok(app['elf']) else 'NOT ELF -> kernel will refuse'})"
    )
    print(f"crc32          {'OK' if app['crc_ok'] else 'MISMATCH (corrupt)'}")
    print(f"total          {app['bytes']} bytes (kernel cap {cap})")
    # Exit non-zero on corruption so QA scripts can assert the refusal.
    ok = app["crc_ok"] and all(e["crc_ok"] for e in app.get("entries", []))
    return 0 if ok else 1


def _elf_ok(elf):
    return (
        len(elf) >= 64
        and elf[:4] == b"\x7fELF"
        and struct.unpack_from("<H", elf, 16)[0] == 2
        and struct.unpack_from("<H", elf, 18)[0] == 0x3E
    )


def cmd_extract(argv):
    src = argv[0]
    d = "."
    i = 1
    while i < len(argv):
        if argv[i] == "--dir":
            if i + 1 >= len(argv):
                die("--dir needs a value")
            d = argv[i + 1]
            i += 2
        else:
            die(f"unknown argument {argv[i]}")
    app = parse_container(src)
    if not app["crc_ok"]:
        die(f"{src}: CRC mismatch — refusing to extract a corrupt container")
    os.makedirs(d, exist_ok=True)
    sticker = {
        "name": app["name"],
        "version": app["version"],
        "author": app["author"],
        "gui": app["gui"],
        "icon": "icon.png" if app["icon_w"] else None,
    }
    if app["ver"] == 2:
        sticker["assets"] = [
            f"assets/asset{idx}.bin" for idx in range(len(app["assets"]))
        ]
        sticker["tail"] = app.get("tail")
        sticker["format"] = "GLE v2"
    else:
        sticker["format"] = "GLE v1"
    with open(os.path.join(d, "sticker.json"), "w") as f:
        json.dump(sticker, f, indent=2)
    print(f"gle-pack: wrote {os.path.join(d, 'sticker.json')} ({sticker['format']})")
    if app["icon_w"]:
        write_png(
            os.path.join(d, "icon.png"),
            app["icon_w"],
            app["icon_w"],
            bytes(app["icon_rgba"]),
        )
        print(f"gle-pack: wrote {os.path.join(d, 'icon.png')}")
    if app["ver"] == 2:
        a_dir = os.path.join(d, "assets")
        os.makedirs(a_dir, exist_ok=True)
        for idx, blob in enumerate(app["assets"]):
            p = os.path.join(a_dir, f"asset{idx}.bin")
            with open(p, "wb") as f:
                f.write(blob)
            print(f"gle-pack: wrote {p} ({len(blob)} B)")
    with open(os.path.join(d, "payload.elf"), "wb") as f:
        f.write(app["elf"])
    print(f"gle-pack: wrote {os.path.join(d, 'payload.elf')} ({app['elf_len']} B)")
    return 0


def main():
    cmds = {
        "mkicon": cmd_mkicon,
        "pack": cmd_pack,
        "inspect": cmd_inspect,
        "extract": cmd_extract,
    }
    if len(sys.argv) < 2 or sys.argv[1] in ("-h", "--help"):
        print(__doc__)
        return 0
    fn = cmds.get(sys.argv[1])
    if fn is None:
        die(f"unknown command {sys.argv[1]!r} (try pack / inspect / extract / mkicon)")
    return fn(sys.argv[2:])


if __name__ == "__main__":
    sys.exit(main() or 0)
