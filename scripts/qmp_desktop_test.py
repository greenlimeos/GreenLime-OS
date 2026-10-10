#!/usr/bin/env python3
"""GreenLime desktop proof: Start menu open, About popup, dismiss.

Usage: python3 scripts/qmp_desktop_test.py
Moves the real cursor by relative events, left-clicks, screenshots each
stage. Asserts menu/popup pixels appear and vanish.
"""
import json
import os
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmp7.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime.iso"
SER = "dist/desktop-serial.log"


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


def wait_idle(path, settle=14, timeout=420):
    # Ticks flow forever, so "quiet" means: no non-tick output growth.
    # Even better: wait for the shell-done milestone, then settle.
    waited = 0
    while waited < timeout:
        time.sleep(2)
        waited += 2
        try:
            data = open(path, "rb").read().decode("utf-8", "replace")
        except FileNotFoundError:
            continue
        if "[shell] interactive ready" in data:
            time.sleep(20)  # let winsrv drain + settle
            return waited
    raise RuntimeError("shell never finished")


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
        time.sleep(20)
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(30)
        s.connect(QSOCK)
        qmp(s, {"execute": "qmp_capabilities"})

        def move(dx, dy):
            if dx:
                qmp(s, {"execute": "input-send-event", "arguments": {"events": [{"type": "rel", "data": {"axis": "x", "value": dx}}]}})
            if dy:
                qmp(s, {"execute": "input-send-event", "arguments": {"events": [{"type": "rel", "data": {"axis": "y", "value": dy}}]}})

        def click():
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [{"type": "btn", "data": {"button": "left", "down": True}}]}})
            time.sleep(1)
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [{"type": "btn", "data": {"button": "left", "down": False}}]}})
            time.sleep(2)

        def shot(path):
            qmp(s, {"execute": "screendump", "arguments": {"filename": path}})

        print("waiting for idle...", flush=True)
        wait_idle(SER)
        print("idle. cursor starts at screen center (640,400).", flush=True)
        # Start mark center = (28, 776): left-aligned slots ->
        # slot0 = 8..48. Cursor starts at (640, 400).
        move(-612, 376)
        time.sleep(2)
        click()  # open menu
        shot("dist/fb-menu.ppm")
        print("menu shot taken", flush=True)
        # About row center = (208, 487): left-docked panel (8,196,400,540),
        # app row 3 at y=462..512, full-width row x=24..392.
        move(180, -289)
        time.sleep(2)
        click()  # open About
        shot("dist/fb-about.ppm")
        print("about shot taken", flush=True)
        # Dismiss: click desktop at (900, 300) — right of the panel.
        move(692, -187)
        time.sleep(2)
        click()
        shot("dist/fb-dismissed.ppm")
        print("dismissed shot taken", flush=True)
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

    def region(path, x0, y0, x1, y1, thresh=120):
        with open(path, "rb") as f:
            assert f.readline().strip() == b"P6"
            dims = []
            while len(dims) < 3:
                line = f.readline()
                if line.startswith(b"#"):
                    continue
                dims += line.split()
            w = int(dims[0])
            px = f.read()
        n = 0
        for y in range(y0, y1):
            for x in range(x0, x1):
                o = (y * w + x) * 3
                if (px[o] + px[o + 1] + px[o + 2]) // 3 > thresh:
                    n += 1
        return n

    def count_color(path, box, rgb):
        """Exact-color pixel count inside box = (x0, y0, x1, y1), x1/y1 excluded."""
        with open(path, "rb") as f:
            assert f.readline().strip() == b"P6"
            dims = []
            while len(dims) < 3:
                line = f.readline()
                if line.startswith(b"#"):
                    continue
                dims += line.split()
            w = int(dims[0])
            px = f.read()
        x0, y0, x1, y1 = box
        n = 0
        for y in range(y0, y1):
            row = y * w * 3
            for x in range(x0, x1):
                o = row + x * 3
                if (px[o], px[o + 1], px[o + 2]) == rgb:
                    n += 1
        return n

    # Start-menu panel: exact MENU_BG (24, 27, 35) inside the panel rect
    # (8,196)-(408,736). The wallpaper's blue channel stays >= 42 and
    # window text never blends to (24,27,35), so only the panel matches.
    def panel_px(path):
        return count_color(path, (8, 196, 408, 736), (24, 27, 35))

    # Popup titlebar strip: exact TITLE_BG (38, 44, 58) at
    # (412,297)-(868,325) — only the About/Settings dialog draws there.
    def popup_px(path):
        return count_color(path, (412, 297, 868, 325), (38, 44, 58))

    m_menu = panel_px("dist/fb-menu.ppm")
    m_about = panel_px("dist/fb-about.ppm")
    m_gone = panel_px("dist/fb-dismissed.ppm")
    p_menu = popup_px("dist/fb-menu.ppm")
    p_about = popup_px("dist/fb-about.ppm")
    p_gone = popup_px("dist/fb-dismissed.ppm")
    print(f"menu panel px: menu={m_menu} about={m_about} dismissed={m_gone}")
    print(f"popup titlebar px: menu={p_menu} about={p_about} dismissed={p_gone}")
    ok = True
    if m_menu < 50000:
        print("FAIL: start menu did not open")
        ok = False
    else:
        print("OK: start menu opened")
    if m_about > 2000:
        print("FAIL: menu stayed open after launching About")
        ok = False
    else:
        print("OK: menu closed when About launched")
    if p_about < 5000:
        print("FAIL: About popup did not open")
        ok = False
    else:
        print("OK: About popup opened")
    if m_gone > 2000 or p_gone > 1000:
        print("FAIL: menu/popup did not dismiss")
        ok = False
    else:
        print("OK: menu + popup dismissed")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
