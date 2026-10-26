#!/usr/bin/env python3
"""GreenLime right-taskbar proof (v1.1): icon glyphs removed, clock only.

The user wanted ALL icons stripped from the right side of the taskbar:
winsrv now draws ONLY the two-line clock (time over date) in a shrunken
clock zone TRAY_W=84 — wifi/volume/battery glyphs are gone, no hover chip
outside that zone. This probe asserts:

  - the OLD icon area (x 1100..1204 of the bar) paints ZERO bright
    pixels in every state (some = 0) — no leftover glyph pixels and the
    hover tint (dark blend) never brightens there
  - the clock is still present: bright pixels in the time-line box
    (1204..1262, 762..782) in every shot
  - hovering the clock zone lights the full chip shown in the left
    gutter (1184..1202, no text there: diff vs baseline >= 50% of 720)
  - after moving away, the gutter matches the baseline pixel-for-pixel
    (diff == 0); clock text itself is excluded from all diffs so a
    minute flip can never flake the run.

Shots are taken ~0.2s after the last pointer event (the 1s strip
refresh would otherwise re-compose the bar on its own); the gemstone
cursor baseline sits at (640,400), far from all checked regions.

Usage: python3 scripts/qmp_tray_hover_test.py [iso]
"""
import json
import os
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmp8.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime-release.iso"
SER = "dist/hover-serial.log"
W, H = 1280, 800
# Clock zone at 1280x800: tray_rect = (sw - 84 - 12, sh - 44 + 4, 84, 40)
GUTTER = (1184, 760, 1202, 799)   # left gutter of the clock zone, 18x40
OLD_ICONS = (1100, 760, 1204, 799)  # wifi/volume/battery area, now bare
CLOCK_LINE = (1204, 762, 1262, 782)  # "HH:MM AM" time text

FAILS = []


def qmp(sock, cmd):
    sock.sendall(json.dumps(cmd).encode() + b"\n")
    dec = json.JSONDecoder()
    buf = ""
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


def wait_idle(path, timeout=420):
    waited = 0
    while waited < timeout:
        time.sleep(2)
        waited += 2
        try:
            data = open(path, "rb").read().decode("utf-8", "replace")
        except FileNotFoundError:
            continue
        if "[shell] interactive ready" in data:
            time.sleep(20)
            return waited
    raise RuntimeError("shell never finished")


def load_ppm(path):
    with open(path, "rb") as f:
        assert f.readline().strip() == b"P6"
        dims = []
        while len(dims) < 3:
            line = f.readline()
            if line.startswith(b"#"):
                continue
            dims += line.split()
        assert (int(dims[0]), int(dims[1])) == (W, H), dims
        return f.read(W * H * 3)


def diff_px(a, b, box):
    x0, y0, x1, y1 = box
    n = 0
    for y in range(y0, y1):
        row = y * W * 3
        for x in range(x0, x1):
            o = row + x * 3
            if a[o] != b[o] or a[o + 1] != b[o + 1] or a[o + 2] != b[o + 2]:
                n += 1
    return n


def bright_px(im, box, thr=150):
    """Pixels whose lowest channel exceeds thr (bright glyph ink)."""
    x0, y0, x1, y1 = box
    n = 0
    for y in range(y0, y1):
        row = y * W * 3
        for x in range(x0, x1):
            o = row + x * 3
            if im[o] > thr and im[o + 1] > thr and im[o + 2] > thr:
                n += 1
    return n


def main():
    for p in (QSOCK, SER):
        try:
            os.unlink(p)
        except FileNotFoundError:
            pass
    ser = open(SER, "wb")
    qemu = subprocess.Popen(
        [
            "qemu-system-x86_64",
            "-cpu", "qemu64,+smep,+smap",
            "-cdrom", ISO,
            "-m", "512M",
            "-vga", "std",
            "-machine", "q35",
            "-no-reboot",
            "-boot", "order=d",
            "-drive", "file=dist/disk.img,if=virtio,format=raw",
            "-serial", "stdio",
            "-display", "none",
            "-qmp", f"unix:{QSOCK},server,nowait",
        ],
        stdout=ser,
        stderr=subprocess.DEVNULL,
    )
    try:
        time.sleep(10)
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(30)
        s.connect(QSOCK)
        qmp(s, {"execute": "qmp_capabilities"})

        pos = [640, 400]

        def shot(name):
            qmp(s, {"execute": "screendump", "arguments": {"filename": f"dist/{name}"}})

        def jump_then_shot(x, y, name, settle=0.2):
            """Exact relative deltas, short settle, screenshot FAST."""
            dx, dy = x - pos[0], y - pos[1]
            pos[0], pos[1] = x, y
            while dx:
                step = max(-600, min(600, dx))
                qmp(s, {"execute": "input-send-event", "arguments": {"events": [{"type": "rel", "data": {"axis": "x", "value": step}}]}})
                dx -= step
            while dy:
                step = max(-600, min(600, dy))
                qmp(s, {"execute": "input-send-event", "arguments": {"events": [{"type": "rel", "data": {"axis": "y", "value": step}}]}})
                dy -= step
            time.sleep(settle)
            shot(name)

        print("waiting for idle...", flush=True)
        wait_idle(SER)
        print("idle. cursor at center (640,400), no hover.", flush=True)
        shot("fb-tray-base.ppm")
        # Hover the clock zone (its center-ish), then leave to far left.
        jump_then_shot(1230, 776, "fb-tray-hover.ppm")
        jump_then_shot(200, 300, "fb-tray-after.ppm")

        base = load_ppm("dist/fb-tray-base.ppm")
        hov = load_ppm("dist/fb-tray-hover.ppm")
        after = load_ppm("dist/fb-tray-after.ppm")

        # 1. Icons gone: zero bright pixels in the old glyph area, in
        #    every state (glyph ink is pure white; hover tint is dark).
        for name, im in (("base", base), ("hover", hov), ("after", after)):
            b = bright_px(im, OLD_ICONS)
            if b != 0:
                FAILS.append(f"{name}: {b} bright px in old icon area (want 0)")
            else:
                print(f"OK: old icon area clean ({name}: 0 bright px)")
        # 2. Clock still there (presence only, minute-flip safe).
        for name, im in (("base", base), ("hover", hov), ("after", after)):
            b = bright_px(im, CLOCK_LINE)
            if b < 40:
                FAILS.append(f"{name}: clock too faint ({b} bright px, want >=40)")
            else:
                print(f"OK: clock text present ({name}: {b} bright px)")
        # 3. Hover chip covers the whole zone (gutter proof).
        area = (GUTTER[2] - GUTTER[0]) * (GUTTER[3] - GUTTER[1])
        dh = diff_px(base, hov, GUTTER)
        if dh * 100 < 50 * area:
            FAILS.append(f"hover covers only {dh * 100 // area}% of the zone gutter (want >=50%)")
        else:
            print(f"OK: hover chip lights the whole clock zone ({dh}/{area} gutter px)")
        # 4. Gutter pixel-exact after leaving (chip torn down fully).
        da = diff_px(base, after, GUTTER)
        if da != 0:
            FAILS.append(f"stale tint after leaving: {da} gutter px differ (want 0)")
        else:
            print("OK: zone fully resets after the cursor leaves (pixel-exact)")

        for f in FAILS:
            print(f"FAIL: {f}")
        print("ALL_GREEN" if not FAILS else "FAILED")
        sys.exit(0 if not FAILS else 1)
    finally:
        try:
            qmp(s, {"execute": "quit"})
        except Exception:
            pass
        time.sleep(1)
        qemu.terminate()
        try:
            qemu.wait(timeout=10)
        except Exception:
            qemu.kill()
        ser.close()


if __name__ == "__main__":
    main()
