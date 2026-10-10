#!/usr/bin/env python3
"""GreenLime proof: boot splash + virtio-tablet cursor sync (one boot).

Stage 1 (boot): right after "[arch] gdt loaded" the screen must show ONLY
the centered "GreenLime OS" splash — console diagnostics are serial-only.
Stage 2 (desktop): injected absolute tablet events must land the cursor at
the exact mapped position — top-left, bottom-right, center.

Usage: python3 scripts/qmp_tablet_test.py [release-iso]
"""
import json
import os
import re
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmp-tablet.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime.iso"
SER = "dist/tablet-serial.log"


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
                raise RuntimeError(f"QMP error: {cmd} -> {obj}")


def read_serial():
    # drvfs (WSL on /mnt/c) can throw transient ENODATA while QEMU has
    # the file open for append — retry, treat as empty.
    for _ in range(5):
        try:
            return open(SER, "rb").read().decode("utf-8", "replace")
        except FileNotFoundError:
            return ""
        except OSError:
            time.sleep(0.05)
    return ""


def wait_for(marker, timeout=420):
    end = time.time() + timeout
    while time.time() < end:
        if marker in read_serial():
            return True
        time.sleep(0.05)
    return False


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
        return w, h, f.read()


def splash_check(path):
    """Returns (title_px, dirty_px): bright title pixels at center, and
    any text-like pixel in the untouched top band (console dump would be)."""
    w, h, px = read_ppm(path)
    title = 0
    for y in range(300, 470):
        for x in range(400, 880):
            o = (y * w + x) * 3
            if px[o + 1] > 180:  # title strokes blend up to g=255
                title += 1
    dirty = 0
    for y in range(0, 100):  # watermark (950..,30) is dark: excluded by x
        for x in range(0, 900):
            o = (y * w + x) * 3
            if px[o + 1] > 120:  # console text (FG g up to 255)
                dirty += 1
    return title, dirty


def menu_px(path):
    """Start-menu panel pixels: exact MENU_BG (24,27,35) in the panel
    rect (320,176)-(960,736). Wallpaper blue stays >= 42, so it can never
    match; window text blends never land on the triple either."""
    w, h, px = read_ppm(path)
    n = 0
    for y in range(176, 736):
        row = y * w * 3
        for x in range(320, 960):
            o = row + x * 3
            if (px[o], px[o + 1], px[o + 2]) == (24, 27, 35):
                n += 1
    return n


def cursor_bbox(path):
    """Bounding box of near-white pixels (cursor is 240,240,240 — nothing
    else on the desktop reaches 239 in all channels)."""
    w, h, px = read_ppm(path)
    x0, y0, x1, y1 = 10**9, 10**9, -1, -1
    for y in range(h):
        row = y * w * 3
        for x in range(w):
            o = row + x * 3
            if px[o] >= 239 and px[o + 1] >= 239 and px[o + 2] >= 239:
                if x < x0:
                    x0 = x
                if x > x1:
                    x1 = x
                if y < y0:
                    y0 = y
                if y > y1:
                    y1 = y
    if x1 < 0:
        return None
    return (x0 + x1) // 2, (y0 + y1) // 2


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
            "-device", "virtio-tablet-pci",
            "-serial", "stdio",
            "-display", "none",
            "-qmp", f"unix:{QSOCK},server,nowait",
        ],
        stdout=ser,
        stderr=subprocess.DEVNULL,
    )
    ok = True
    try:
        # Connect ASAP: the splash shot must happen early in the boot.
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(30)
        end = time.time() + 60
        while True:
            try:
                s.connect(QSOCK)
                break
            except (FileNotFoundError, ConnectionRefusedError):
                if time.time() > end:
                    raise RuntimeError("QMP socket never appeared")
                time.sleep(0.1)
        qmp(s, {"execute": "qmp_capabilities"})

        def shot(path):
            qmp(s, {"execute": "screendump", "arguments": {"filename": path}})

        # --- Stage 1: splash ---
        if not wait_for("[arch] gdt loaded", timeout=300):
            raise RuntimeError("kernel never reached arch init")
        shot("dist/fb-splash.ppm")
        # Marker prints the instant the desktop covers the splash: read it
        # before waiting, or the sleep below would always report "raced".
        painted = "[winsrv] desktop painted" in read_serial()
        time.sleep(3)  # screendump is synchronous; give the file time
        title, dirty = splash_check("dist/fb-splash.ppm")
        print(f"splash: title_px={title} top_band_px={dirty} raced={painted}")
        if title < 300:
            print(f"FAIL: splash title missing (title_px={title})"
                  + (" — desktop won the race, rerun" if painted else ""))
            ok = False
        elif dirty > 0:
            print(f"FAIL: console text on screen (dirty_px={dirty})")
            ok = False
        else:
            print("OK: splash only, screen clean")

        # --- Stage 2: tablet absolute positioning ---
        if not wait_for("[shell] interactive ready", timeout=420):
            raise RuntimeError("shell never became ready")
        time.sleep(20)  # let winsrv drain + settle
        serial = read_serial()
        m = re.search(r"virtio-tablet: evt queue=(\d+) bufs=(\d+) "
                      r"x=(-?\d+)\.\.(-?\d+) y=(-?\d+)\.\.(-?\d+)", serial)
        if not m:
            print("FAIL: virtio-tablet never came online")
            try:
                qmp(s, {"execute": "quit"})
            except Exception:
                pass
            return finish(False)
        x0, x1, y0, y1 = (int(m.group(i)) for i in range(3, 7))
        print(f"tablet online: x={x0}..{x1} y={y0}..{y1}")

        def abs_move(vx, vy, tag, expect):
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "abs", "data": {"axis": "x", "value": vx}},
                {"type": "abs", "data": {"axis": "y", "value": vy}},
            ]}})
            time.sleep(4)
            path = f"dist/fb-tablet-{tag}.ppm"
            shot(path)
            time.sleep(2)
            cur = cursor_bbox(path)
            if cur is None:
                print(f"FAIL[{tag}]: cursor not visible")
                return False
            got = expect(cur)
            print(f"tablet[{tag}]: cursor center={cur} -> "
                  + ("OK" if got else "FAIL"))
            return got

        w, h = 1280, 800
        mid_x = x0 + (x1 - x0) // 2
        mid_y = y0 + (y1 - y0) // 2
        ok &= abs_move(x0, y0, "tl",
                       lambda c: c[0] <= 18 and c[1] <= 20)
        ok &= abs_move(x1, y1, "br",
                       lambda c: c[0] >= w - 24 and c[1] >= h - 24)
        ok &= abs_move(mid_x, mid_y, "mid",
                       lambda c: abs(c[0] - w // 2) <= 14
                       and abs(c[1] - h // 2) <= 14)

        # --- Stage 3: tablet click opens the Start menu, click-off closes it ---
        def to_dev(axis, px):
            lo, hi = (x0, x1) if axis == 0 else (y0, y1)
            dim = w if axis == 0 else h
            norm = px * 32767 // (dim - 1)
            return lo + norm * (hi - lo) // 32767

        def click_at(gx, gy, tag):
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "abs", "data": {"axis": "x", "value": to_dev(0, gx)}},
                {"type": "abs", "data": {"axis": "y", "value": to_dev(1, gy)}},
            ]}})
            time.sleep(3)
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "btn", "data": {"button": "left", "down": True}}]}})
            time.sleep(1)
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "btn", "data": {"button": "left", "down": False}}]}})
            time.sleep(3)
            shot(f"dist/fb-tablet-{tag}.ppm")
            time.sleep(2)

        # Start icon: centered slot group of 4 -> slot0 center (568, 776).
        click_at(568, 776, "menu")
        # Dismiss: click terminal at (200, 300) — left of the menu panel.
        click_at(200, 300, "dismiss")
        m_menu = menu_px("dist/fb-tablet-menu.ppm")
        m_gone = menu_px("dist/fb-tablet-dismiss.ppm")
        print(f"tablet click: menu_px={m_menu} dismissed_px={m_gone}")
        if m_menu < 50000:
            print("FAIL: tablet click did not open the Start menu")
            ok = False
        elif m_gone > 2000:
            print("FAIL: tablet click did not dismiss the menu")
            ok = False
        else:
            print("OK: tablet click opens + dismisses the menu")

        try:
            qmp(s, {"execute": "quit"})
        except Exception:
            pass
        s.close()
    finally:
        time.sleep(1)
        qemu.terminate()
        try:
            qemu.wait(timeout=10)
        except Exception:
            qemu.kill()
        ser.close()
    return finish(ok)


def finish(ok):
    if ok:
        print("PASS: splash + tablet sync verified")
    else:
        print("RESULT: FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
