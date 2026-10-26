#!/usr/bin/env python3
"""GreenLime multi-window proof: second client, focus routing, taskbar
buttons, window drag + resize + close — one boot, virtio-tablet pointer.

Usage: python3 scripts/qmp_multitest.py [iso]

Geometry (asserted from the server's serial log first):
  demo window opens at 700,120 540x330 (id 2). v1.1 Windows 11 taskbar:
  full-width 48px bar, centered cluster — start (532,756,40,40) center
  (552,776), Terminal tile 1 in the start card (510,528,126,100) center
  (573,578); the demo client button sits at (708,756,40,40), center
  (728,776) — nclients excludes the Terminal/Files/Settings system
  windows, so it never shifts.

Checks (serial unless marked "screen"):
  1. demo client spawned + window created
  2. screen: demo titlebar pixels at the expected geometry
  3. keys land in demo when focused:      [demo] got: hi
  4. launch Terminal from the start card, then click back and forth:
     focus win 0 + focusok on serial AND demo's input count stays 1
  5. taskbar button refocuses demo:       focus win 2 + [demo] got: q
  6. titlebar drag moves the window:      win 2 moved to 600,80 (+ screen)
  7. corner grip resizes:                 win 2 resized 580x370
  8. titlebar X closes it:                win 2 closed + demo says bye (+ screen)
"""
import json
import os
import re
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmp5.sock"
HSOCK = "/tmp/gl-hmp5.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime.iso"
SER = "dist/multi-serial.log"

# Focused titlebar (43,43,43) / unfocused (37,37,38) — the v1.1
# Windows 11 dark theme.
TITLE_BG = (43, 43, 43)
TITLE_BG2 = (37, 37, 38)


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


def hmp(cmd):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(10)
    s.connect(HSOCK)
    banner = b""
    while b"(qemu)" not in banner:
        banner += s.recv(4096)
    s.sendall(cmd.encode() + b"\n")
    time.sleep(0.5)
    try:
        out = s.recv(65536)
    except socket.timeout:
        out = b"(timeout)"
    s.close()
    return out


def read_serial():
    # WSL drvfs can throw transient ENODATA on files QEMU is writing.
    for _ in range(6):
        try:
            return open(SER, "rb").read().decode("utf-8", "replace")
        except OSError:
            time.sleep(0.5)
    return ""


def wait_for(marker, timeout=90):
    end = time.time() + timeout
    while time.time() < end:
        if marker in read_serial():
            return True
        time.sleep(1.5)
    return False


def wait_for_match(pattern, timeout=90):
    end = time.time() + timeout
    while time.time() < end:
        m = re.search(pattern, read_serial())
        if m:
            return m
        time.sleep(1.5)
    return None


def read_ppm(path):
    for _ in range(6):
        try:
            with open(path, "rb") as f:
                assert f.readline().strip() == b"P6"
                dims = []
                while len(dims) < 3:
                    line = f.readline()
                    if line.startswith(b"#"):
                        continue
                    dims += line.split()
                w, h = int(dims[0]), int(dims[1])
                return w, h, f.read(w * h * 3)
        except OSError:
            time.sleep(0.5)
    raise RuntimeError(f"cannot read {path}")


def color_px(path, box, rgb=TITLE_BG):
    """Exact-color pixel count inside box = (x0, y0, x1, y1), x1/y1 excluded."""
    w, h, px = read_ppm(path)
    x0, y0, x1, y1 = box
    n = 0
    for y in range(y0, y1):
        for x in range(x0, x1):
            o = (y * w + x) * 3
            if (px[o], px[o + 1], px[o + 2]) == rgb:
                n += 1
    return n


def main():
    for p in (QSOCK, HSOCK, SER):
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
            "-monitor", f"unix:{HSOCK},server,nowait",
        ],
        stdout=ser,
        stderr=subprocess.DEVNULL,
    )
    checks = []

    def check(name, passed):
        checks.append((name, bool(passed)))
        print(f"{'OK  ' if passed else 'FAIL'}: {name}", flush=True)

    try:
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
            time.sleep(1.5)  # screendump is sync; give drvfs the time

        # Tablet ranges (parse like the tablet test; QEMU is 0..32767).
        if not wait_for("[demo] window open", timeout=420):
            raise RuntimeError("demo client never opened its window")
        wait_for("[shell] interactive ready", timeout=420)
        time.sleep(15)  # let winsrv drain + settle
        serial = read_serial()
        check("demo window created",
              "[demo] window open" in serial)
        m = re.search(r"\[winsrv\] win 2 open at (\d+),(\d+) (\d+)x(\d+)",
                      serial)
        check("server logged geometry 700,120 540x330",
              m and m.group(0).endswith("700,120 540x330"))
        m = re.search(r"virtio-tablet: evt queue=(\d+) bufs=(\d+) "
                      r"x=(-?\d+)\.\.(-?\d+) y=(-?\d+)\.\.(-?\d+)", serial)
        if not m:
            raise RuntimeError("virtio-tablet never came online")
        x0, x1, y0, y1 = (int(m.group(i)) for i in range(3, 7))

        # --- Screen: demo window is on the desktop ------------------------
        # Boot: clean desktop (terminal opens on demand, never at boot),
        # so nothing is focused and the demo titlebar is the dark one.
        shot("dist/fb-multi-open.ppm")
        n_open = color_px("dist/fb-multi-open.ppm", (710, 126, 1230, 146),
                          TITLE_BG2)
        print(f"screen: demo titlebar px={n_open}", flush=True)
        check("screen: demo titlebar visible (unfocused)", n_open > 3500)

        w, h = 1280, 800

        def abs_xy(gx, gy):
            vx = x0 + gx * 32767 // (w - 1) * (x1 - x0) // 32767
            vy = y0 + gy * 32767 // (h - 1) * (y1 - y0) // 32767
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "abs", "data": {"axis": "x", "value": vx}},
                {"type": "abs", "data": {"axis": "y", "value": vy}},
            ]}})

        def btn(down):
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "btn", "data": {"button": "left", "down": down}}]}})
            time.sleep(0.6)

        def click_at(gx, gy):
            abs_xy(gx, gy)
            time.sleep(1.2)
            btn(True)
            btn(False)
            time.sleep(1.2)

        def drag(x_from, y_from, x_to, y_to):
            abs_xy(x_from, y_from)
            time.sleep(1.2)
            btn(True)
            abs_xy(x_to, y_to)
            time.sleep(1.2)
            btn(False)
            time.sleep(1.2)

        def type_keys(keys, gap=0.7):
            for k in keys:
                # HMP sendkey names: space is "spc", not " ".
                hmp(f"sendkey {k if k != ' ' else 'spc'}")
                time.sleep(gap)

        # --- Focus demo, type into it -------------------------------------
        click_at(970, 290)  # demo window content
        type_keys(["h", "i", "ret"])
        check("keys -> demo window ([demo] got: hi)",
              wait_for("[demo] got: hi", timeout=45))

        # --- Launch Terminal from the start card, then click back and
        # forth. Start button (552,776) opens the card; the Terminal tile
        # (510,528,126,100) launches it on demand (never open at boot).
        click_at(552, 776)
        click_at(573, 578)
        check("menu launched the terminal",
              wait_for("[winsrv] launch Terminal", timeout=30))
        check("terminal opened + focused (focus win 0)",
              wait_for("[winsrv] focus win 0", timeout=30))
        # --- Click demo, then terminal: keys must follow focus ------------
        click_at(970, 290)  # demo window content
        check("click focuses demo (focus win 2)",
              wait_for("[winsrv] focus win 2", timeout=30))
        click_at(400, 400)  # terminal content
        check("click focuses terminal (focus win 0)",
              wait_for("[winsrv] focus win 0", timeout=30))
        type_keys(list("echo focusok") + ["ret"])
        check("keys -> terminal (focusok echoed+ran)",
              wait_for("focusok", timeout=45))
        time.sleep(3)
        n1 = read_serial().count("[demo] got: ")
        check("demo input count still 1 (no key leak)", n1 == 1)

        # --- Taskbar button refocuses the demo ---------------------------
        # The demo client button: first (only) client → (708,756,40,40),
        # center (728,776). nclients excludes the Terminal/Files/Settings
        # system windows, so foregrounding the terminal does not shift it.
        click_at(728, 776)
        check("taskbar button focuses demo (focus win 2)",
              wait_for("[winsrv] focus win 2", timeout=30))
        type_keys(["q", "ret"])
        check("keys -> demo again ([demo] got: q)",
              wait_for("[demo] got: q", timeout=45))

        # --- Drag the titlebar --------------------------------------------
        drag(900, 135, 800, 95)  # dx=200, dy=15 -> window at 600,80
        check("titlebar drag moved window (moved to 600,80)",
              wait_for("[winsrv] win 2 moved to 600,80", timeout=30))
        shot("dist/fb-multi-moved.ppm")
        n_moved = color_px("dist/fb-multi-moved.ppm", (605, 84, 1135, 100),
                           TITLE_BG)
        print(f"screen: titlebar at new pos px={n_moved}", flush=True)
        check("screen: titlebar at new position", n_moved > 3500)

        # --- Resize via the corner grip -----------------------------------
        drag(1132, 402, 1180, 450)  # (600+540-16, 80+330-16) grip center
        # Tablet scaling truncates guest px <-> device units: allow +/-2.
        m = wait_for_match(r"\[winsrv\] win 2 resized (\d+)x(\d+)", timeout=30)
        check("grip resize (resized ~580x370)",
              m and abs(int(m.group(1)) - 580) <= 2
              and abs(int(m.group(2)) - 370) <= 2)

        # --- Close via the titlebar X -------------------------------------
        click_at(1164, 95)  # close btn = (600+580-26, 84, 20, 22) center
        check("X closed the window (win 2 closed)",
              wait_for("[winsrv] win 2 closed", timeout=30))
        check("client saw the EOF (window closed, bye)",
              wait_for("[demo] window closed, bye", timeout=30))
        shot("dist/fb-multi-closed.ppm")
        n_gone = color_px("dist/fb-multi-closed.ppm", (605, 84, 1135, 100),
                          TITLE_BG)
        print(f"screen: titlebar gone px={n_gone}", flush=True)
        check("screen: titlebar gone after close", n_gone < 300)

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

    ok = True
    for _, passed in checks:
        ok = ok and passed
    if not ok:
        print("\n--- serial tail ---")
        print("\n".join(read_serial().splitlines()[-40:]))
    print(f"\nRESULT: {'PASS' if ok else 'FAIL'} ({sum(p for _, p in checks)}"
          f"/{len(checks)})")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
