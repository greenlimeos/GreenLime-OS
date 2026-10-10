#!/usr/bin/env python3
"""GreenLime framebuffer proof: boot headless, screendump via QMP, analyze.

Usage: python3 scripts/qmp_screendump.py
Boot ISO must exist (make iso). Prints non-bg pixel count + ASCII art of the
top-left text region so a human can verify glyphs render correctly.
"""
import json
import os
import socket
import subprocess
import sys
import time

SOCK = "/tmp/gl-qmp.sock"
PPM = "dist/fb.ppm"
ISO = "dist/GreenLime.iso"
WAIT = int(sys.argv[1]) if len(sys.argv) > 1 else 12


def qmp_exchange(sock, cmd):
    """Send one QMP command, return the first response containing 'return'."""
    sock.sendall(json.dumps(cmd).encode() + b"\n")
    dec = json.JSONDecoder()
    buf = ""
    # Drain any pending greeting/events first for the capabilities call.
    while True:
        chunk = sock.recv(65536).decode()
        if not chunk:
            raise RuntimeError("QMP closed")
        buf += chunk
        while True:
            buf = buf.lstrip()
            if not buf:
                break
            try:
                obj, idx = dec.raw_decode(buf)
            except json.JSONDecodeError:
                break
            buf = buf[idx:]
            if isinstance(obj, dict) and "return" in obj:
                return obj
            if isinstance(obj, dict) and "error" in obj:
                raise RuntimeError(f"QMP error: {obj}")
            # else: greeting or event, keep waiting


def main():
    try:
        os.unlink(SOCK)
    except FileNotFoundError:
        pass
    qemu = subprocess.Popen(
        [
            "qemu-system-x86_64",
            "-cpu", "qemu64,+smep,+smap",
            "-cdrom", ISO,
            "-m", "512M",
            "-vga", "std",
            "-machine", "q35",
            "-no-reboot",
            "-serial", "stdio",
            "-display", "none",
            "-qmp", f"unix:{SOCK},server,nowait",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        time.sleep(WAIT)  # Limine menu + kernel boot with margin
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(10)
        s.connect(SOCK)
        qmp_exchange(s, {"execute": "qmp_capabilities"})
        qmp_exchange(
            s, {"execute": "screendump", "arguments": {"filename": PPM}}
        )
        try:
            qmp_exchange(s, {"execute": "quit"})
        except Exception:
            pass
        s.close()
    finally:
        time.sleep(1)
        qemu.terminate()
        qemu.wait(timeout=10)

    with open(PPM, "rb") as f:
        magic = f.readline()
        assert magic.strip() == b"P6", f"bad magic {magic!r}"
        # Skip comments, read w h maxval.
        dims = []
        while len(dims) < 3:
            line = f.readline()
            if line.startswith(b"#"):
                continue
            dims += line.split()
        w, h, maxval = int(dims[0]), int(dims[1]), int(dims[2])
        px = f.read(w * h * 3)
    assert len(px) == w * h * 3, "short ppm"

    # "Content" = pixels differing from the top-left corner: the wallpaper
    # is a gradient, so a uniform (blank/frozen) screen fails this.
    ref = (px[0], px[1], px[2])
    nonbg = 0
    for i in range(0, len(px), 3):
        if (px[i], px[i + 1], px[i + 2]) != ref:
            nonbg += 1
    print(f"screen={w}x{h} non_ref_pixels={nonbg}")

    # ASCII art of top-left 640x96 at 1:4 sampling (160x24).
    print("--- screen top (each char = 4x4 px block) ---")
    for gy in range(0, 96, 4):
        row = []
        for gx in range(0, 640, 4):
            lit = 0
            for dy in range(4):
                for dx in range(4):
                    o = ((gy + dy) * w + (gx + dx)) * 3
                    r, g, b = px[o], px[o + 1], px[o + 2]
                    if (r + g + b) // 3 > 40:
                        lit += 1
            row.append("#" if lit >= 6 else ("." if lit >= 2 else " "))
        print("".join(row).rstrip())
    print("--- end ---")
    if nonbg < 1000:
        print("FAIL: screen looks empty", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
