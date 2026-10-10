#!/usr/bin/env python3
"""Mouse proof done right: wait for idle, screenshot cursor, move, compare."""
import json
import os
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmp6.sock"
ISO = "dist/GreenLime.iso"
SER = "dist/mous-idle-serial.log"


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


def shot(sock, path):
    qmp(sock, {"execute": "screendump", "arguments": {"filename": path}})


def white_near(path, cx, cy, r):
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
    for y in range(max(0, cy - r), cy + r):
        for x in range(max(0, cx - r), cx + r):
            o = (y * w + x) * 3
            if px[o] > 200 and px[o + 1] > 200 and px[o + 2] > 200:
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
        # Wait for serial quiet (script done, system idle).
        time.sleep(20)
        last, quiet, waited = 0, 0, 0
        while waited < 400:
            time.sleep(2)
            waited += 2
            size = os.path.getsize(SER)
            if size == last:
                quiet += 2
                if quiet >= 12:
                    break
            else:
                quiet = 0
                last = size
        print(f"idle after ~{20 + waited - quiet}s, serial={last} bytes", flush=True)
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(20)
        s.connect(QSOCK)
        qmp(s, {"execute": "qmp_capabilities"})
        shot(s, "dist/fb-idle.ppm")
        print("idle shot taken", flush=True)
        qmp(
            s,
            {
                "execute": "input-send-event",
                "arguments": {
                    "events": [{"type": "rel", "data": {"axis": "x", "value": 300}}]
                },
            },
        )
        time.sleep(6)
        shot(s, "dist/fb-moved.ppm")
        print("moved shot taken", flush=True)
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

    a = white_near("dist/fb-idle.ppm", 640, 400, 20)
    b = white_near("dist/fb-moved.ppm", 640, 400, 20)
    c = white_near("dist/fb-moved.ppm", 940, 400, 40)
    print(f"white near center before={a} after={b}, near target={c}")
    ok = True
    if a < 20:
        print("FAIL: no cursor at center when idle")
        ok = False
    else:
        print("OK: cursor visible at idle")
    if c < 20:
        print("FAIL: cursor did not reach target area")
        ok = False
    else:
        print("OK: cursor pixels near target")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
