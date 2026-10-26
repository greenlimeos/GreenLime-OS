#!/usr/bin/env python3
"""GreenLime desktop proof (v1.1 Windows 11): start card open, Settings
window, click-away.

Usage: python3 scripts/qmp_desktop_test.py
Moves the real cursor by relative events, left-clicks, screenshots each
stage. Asserts:
  - start button lime underline appears while the card is open (exact)
  - the card's opaque search field painted (exact)
  - the demo client button's azure strip is on the taskbar (exact)
  - the Settings window titlebar is exact TITLE; card closed after launch
  - click-away leaves the Settings window open (Windows 11 semantics:
    desktop clicks never close windows)
Layout used (1280x800): full-width 48px acrylic taskbar (y 752..799).
Centered cluster (nclient=1 at boot: start, Files, Terminal, Settings
pins + demo chip): cluster 216 wide, x0=532 — start (532,756,40,40)
center (552,776), demo chip (708,756,40,40) center (728,776). Start card
(360,440,560,300): search field (376,456,528,36), tiles 126x100 in a 4
col grid (376,528 + 134/row) — Settings tile 2 = (644,528,126,100)
center (707,578). Settings window opens (430,170,420,300).
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
                raise RuntimeError(f"QMP error: {obj}")
            # else: greeting or event, keep waiting


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
        time.sleep(10)
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(30)
        s.connect(QSOCK)
        qmp(s, {"execute": "qmp_capabilities"})

        pos = [640, 400]

        def move(dx, dy):
            while dx:
                step = max(-600, min(600, dx))
                qmp(s, {"execute": "input-send-event", "arguments": {"events": [{"type": "rel", "data": {"axis": "x", "value": step}}]}})
                dx -= step
            while dy:
                step = max(-600, min(600, dy))
                qmp(s, {"execute": "input-send-event", "arguments": {"events": [{"type": "rel", "data": {"axis": "y", "value": step}}]}})
                dy -= step
            time.sleep(2)

        def to(x, y):
            move(x - pos[0], y - pos[1])
            pos[0], pos[1] = x, y

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
        # Start button center = (552,776): open the start card.
        to(552, 776)
        click()  # open card
        shot("dist/fb-menu.ppm")
        print("menu shot taken", flush=True)
        # Settings tile center = (707,578): card tile 2 (644,528,126,100).
        to(707, 578)
        click()  # launch the Settings window (card closes)
        shot("dist/fb-about.ppm")
        print("settings window shot taken", flush=True)
        # Click away at (1000,550) — free desktop (below the demo window,
        # right of Settings; (1000,300) would hit the demo body).
        to(1000, 550)
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

    # Start-button underline: exact LIME (170,232,64). The 16x3 pill sits
    # at (544..559, 790..792) while the card is open (the lime G glyph
    # above it ends by y≈782, so this box sees the underline only).
    LIME = (170, 232, 64)

    def start_und_px(path):
        return count_color(path, (540, 786, 568, 796), LIME)

    # Settings pin underline (same pill under cx=684): lime only while the
    # Settings window is open AND focused.
    def pin_und_px(path):
        return count_color(path, (672, 786, 700, 796), LIME)

    # Search field fill: exact SEARCH_BG (44,46,52). The field is opaque
    # (376,456,528,36, radius 18); this inner box is pure fill except the
    # magnifier + placeholder strokes further left.
    SEARCH_BG = (44, 46, 52)

    def search_px(path):
        return count_color(path, (500, 460, 800, 488), SEARCH_BG)

    # Demo client button azure strip: (719..736, 766..770) — the mini
    # window glyph's 18x5 title strip; the Files folder (also BLUE_HI)
    # sits at cx=596, far left of this box.
    BLUE_HI = (122, 180, 250)

    def client_chip_px(path):
        return count_color(path, (716, 762, 742, 774), BLUE_HI)

    # Settings window titlebar: exact TITLE (43,43,43) at (436,176)-
    # (742,198) — focused window, buttons live at x>=804, title text at
    # x<=500 is the only subtraction.
    TITLE = (43, 43, 43)

    def settings_px(path):
        return count_color(path, (436, 176, 742, 198), TITLE)

    m_menu = start_und_px("dist/fb-menu.ppm")
    m_about = start_und_px("dist/fb-about.ppm")
    m_gone = start_und_px("dist/fb-dismissed.ppm")
    s_menu = search_px("dist/fb-menu.ppm")
    c_menu = client_chip_px("dist/fb-menu.ppm")
    p_menu = pin_und_px("dist/fb-menu.ppm")
    p_about = pin_und_px("dist/fb-about.ppm")
    t_menu = settings_px("dist/fb-menu.ppm")
    t_about = settings_px("dist/fb-about.ppm")
    t_gone = settings_px("dist/fb-dismissed.ppm")
    print(f"start underline px: menu={m_menu} about={m_about} dismissed={m_gone}")
    print(f"search field px: menu={s_menu}")
    print(f"client chip azure px: menu={c_menu}")
    print(f"settings pin underline px: menu={p_menu} about={p_about}")
    print(f"settings titlebar px: menu={t_menu} about={t_about} dismissed={t_gone}")
    ok = True
    if m_menu < 40:
        print("FAIL: start card did not open (no start underline)")
        ok = False
    else:
        print("OK: start card opened")
    if s_menu < 7000:
        print("FAIL: start card search field missing")
        ok = False
    else:
        print("OK: search field painted")
    if c_menu < 60:
        print("FAIL: demo client chip strip missing from the taskbar")
        ok = False
    else:
        print("OK: demo client chip on the taskbar")
    if m_about > 6:
        print("FAIL: card stayed open after launching Settings")
        ok = False
    else:
        print("OK: card closed when Settings launched")
    if p_menu > 6:
        print("FAIL: Settings pin showed a running underline before launch")
        ok = False
    else:
        print("OK: no Settings underline before launch")
    if p_about < 40:
        print("FAIL: Settings pin underline missing after launch")
        ok = False
    else:
        print("OK: Settings pin underlined (focused)")
    if t_about < 5000:
        print("FAIL: Settings window did not open")
        ok = False
    else:
        print("OK: Settings window opened")
    if t_gone < 5000:
        print("FAIL: Settings window vanished on click-away")
        ok = False
    else:
        print("OK: Settings window persists (click-away only closes the card)")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
