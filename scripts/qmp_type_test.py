#!/usr/bin/env python3
"""v1.1 interactive stdin proof: launch the terminal from the start card,
then type into it via QMP sendkey.

Usage: python3 scripts/qmp_type_test.py [iso]

The terminal no longer opens at boot — it opens on demand from the
launcher, so this test clicks the Start dock slot (616,776) + the
Terminal tile (530,450) first (also proving the launch flow), then types
two commands:
  1. "echo greenlime works"      -> output must appear (round trip works)
  2. "echo abd" + backspace + "c" -> "abc" (backspace edits the line)

Asserts on the serial mirror (ground truth for what the shell ran) and
saves screenshots before/after for pixel proof.
"""
import json
import os
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmp8.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime.iso"
SER = "dist/type-serial.log"

KEYS_CMD1 = ["e", "c", "h", "o", "spc", "g", "r", "e", "e", "n", "l", "i",
             "m", "e", "spc", "w", "o", "r", "k", "s", "ret"]
KEYS_CMD2 = ["e", "c", "h", "o", "spc", "a", "b", "d", "backspace", "c", "ret"]


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


def wait_marker(path, marker, timeout=420):
    waited = 0
    while waited < timeout:
        time.sleep(2)
        waited += 2
        try:
            data = open(path, "rb").read().decode("utf-8", "replace")
        except FileNotFoundError:
            continue
        if marker in data:
            return waited
    raise RuntimeError(f"marker {marker!r} never appeared")


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
    try:
        print("waiting for shell...", flush=True)
        wait_marker(SER, "[shell] interactive ready")
        print("shell ready, boot took ~time below", flush=True)
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(60)
        s.connect(QSOCK)
        qmp(s, {"execute": "qmp_capabilities"})

        def hmp(cmd):
            return qmp(s, {"execute": "human-monitor-command",
                           "arguments": {"command-line": cmd}})

        def type_keys(keys):
            for k in keys:
                hmp(f"sendkey {k}")
                time.sleep(0.6)

        def to_dev(px, dim):
            # virtio-tablet exposes 0..32767 over the screen dimension.
            return px * 32767 // (dim - 1)

        def click_at(gx, gy):
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "abs", "data": {"axis": "x", "value": to_dev(gx, 1280)}},
                {"type": "abs", "data": {"axis": "y", "value": to_dev(gy, 800)}},
            ]}})
            time.sleep(2)
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "btn", "data": {"button": "left", "down": True}}]}})
            time.sleep(1)
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "btn", "data": {"button": "left", "down": False}}]}})
            time.sleep(2)

        def shot(path):
            qmp(s, {"execute": "screendump", "arguments": {"filename": path}})

        # Terminal opens on demand: Start dock slot (616,776) — the demo
        # window is open at boot, so the pill has 2 windows (n=2: w=120,
        # pill_x=580, slot0 center 616) — then the Terminal pinned tile
        # (530,450) in the centered start card.
        click_at(616, 776)
        click_at(530, 450)
        wait_marker(SER, "[winsrv] launch Terminal")

        shot("dist/fb-before-type.ppm")
        type_keys(KEYS_CMD1)
        time.sleep(8)  # let the command run + mirror out
        type_keys(KEYS_CMD2)
        time.sleep(8)
        shot("dist/fb-after-type.ppm")
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

    data = open(SER, "rb").read().decode("utf-8", "replace")
    checks = [
        ("echo round trip", "greenlime works" in data),
        ("backspace edit -> abc", "abc" in data),
        ("backspaced char never ran", "abd" not in data),
        ("prompt shown", "$" in data),
        ("kbd scancodes seen", "[kbd] scancode 0x1e" in data),
    ]
    ok = True
    for name, passed in checks:
        print(f"{'OK' if passed else 'FAIL'}: {name}")
        ok = ok and passed
    if not ok:
        print("\n--- serial tail ---")
        print("\n".join(data.splitlines()[-40:]))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
