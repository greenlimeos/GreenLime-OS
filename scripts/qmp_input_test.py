#!/usr/bin/env python3
"""GreenLime input proof: sendkey + mouse events, verify on screen/serial.

Usage: python3 scripts/qmp_input_test.py
- Boots with QMP + HMP sockets, waits for the GUI + shell-done.
- HMP `sendkey a` -> serial must show 1e/9e (echoed into the terminal).
- QMP `input-send-event` rel x+120 -> cursor moves right (pixel check).
"""
import json
import os
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmp3.sock"
HSOCK = "/tmp/gl-hmp3.sock"
PPM1 = "dist/fb-key.ppm"
PPM2 = "dist/fb-mouse.ppm"
ISO = "dist/GreenLime.iso"
SER = "dist/input-serial.log"
WAIT = int(sys.argv[1]) if len(sys.argv) > 1 else 60


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
    time.sleep(1)
    try:
        out = s.recv(65536)
    except socket.timeout:
        out = b"(timeout)"
    s.close()
    return out


def shot(path):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(15)
    s.connect(QSOCK)
    qmp(s, {"execute": "qmp_capabilities"})
    qmp(s, {"execute": "screendump", "arguments": {"filename": path}})
    s.close()


def cursor_box(path):
    """Bounding box of cursor pixels (all channels >= 239 — the cursor is
    240,240,240; window title text is 232,238,246 and must NOT count)."""
    with open(path, "rb") as f:
        assert f.readline().strip() == b"P6"
        dims = []
        while len(dims) < 3:
            line = f.readline()
            if line.startswith(b"#"):
                continue
            dims += line.split()
        w, h = int(dims[0]), int(dims[1])
        px = f.read(w * h * 3)
    xs, ys = [], []
    for y in range(0, 700):
        for x in range(w):
            o = (y * w + x) * 3
            if px[o] >= 239 and px[o + 1] >= 239 and px[o + 2] >= 239:
                xs.append(x)
                ys.append(y)
    if not xs:
        return None
    return (min(xs), min(ys), max(xs), max(ys), len(xs))


def main():
    for p in (QSOCK, HSOCK, PPM1, PPM2, SER):
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
            "-monitor", f"unix:{HSOCK},server,nowait",
        ],
        stdout=ser,
        stderr=subprocess.DEVNULL,
    )
    try:
        print(f"waiting {WAIT}s for GUI idle...", flush=True)
        time.sleep(WAIT)
        print("HMP sendkey a:", hmp("sendkey a"), flush=True)
        time.sleep(4)
        shot(PPM1)
        print("keyboard screenshot taken", flush=True)
        # Mouse: move right 200px via QMP.
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(15)
        s.connect(QSOCK)
        qmp(s, {"execute": "qmp_capabilities"})
        try:
            qmp(
                s,
                {
                    "execute": "input-send-event",
                    "arguments": {
                        "events": [{"type": "rel", "data": {"axis": "x", "value": 200}}]
                    },
                },
            )
            print("mouse move sent", flush=True)
        except Exception as e:
            print("mouse inject unsupported:", e, flush=True)
        s.close()
        time.sleep(4)
        shot(PPM2)
        print("mouse screenshot taken", flush=True)
        try:
            qmp_quit = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            qmp_quit.settimeout(10)
            qmp_quit.connect(QSOCK)
            qmp(qmp_quit, {"execute": "qmp_capabilities"})
            qmp(qmp_quit, {"execute": "quit"})
            qmp_quit.close()
        except Exception:
            pass
    finally:
        time.sleep(1)
        qemu.terminate()
        try:
            qemu.wait(timeout=10)
        except Exception:
            qemu.kill()
        ser.close()

    b1 = cursor_box(PPM1)
    b2 = cursor_box(PPM2)
    print("cursor before:", b1)
    print("cursor after:", b2)
    ser_text = open(SER, "rb").read().decode("utf-8", "replace")
    print("serial saw 0x1e:", ("scancode 0x1e" in ser_text))
    ok = True
    if b1 is None or b2 is None:
        print("FAIL: cursor not found")
        ok = False
    elif b2[0] <= b1[0] + 50:
        print("FAIL: cursor did not move right")
        ok = False
    else:
        print(f"OK: cursor moved {b1[0]} -> {b2[0]}")
    if "scancode 0x1e" not in ser_text:
        print("FAIL: no 0x1e scancode")
        ok = False
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
