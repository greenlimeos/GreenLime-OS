#!/usr/bin/env python3
"""M14 (v1.1) disk write persistence proof: launch the terminal from the
start card, then type disk commands into it via QMP sendkey.

Usage: python3 scripts/qmp_disk_test.py [phase] [iso]

phase 1 (default): write + read back in the same boot
  1. "echo m14proof > /disk/m14.txt"
  2. "cat /disk/m14.txt"          -> "m14proof" appears
  3. "mkdir /disk/m14dir"         -> mkdir proof
  4. "ls /disk"                   -> m14.txt + m14dir/ listed

phase 2: read only — run AFTER phase 1 and AFTER the previous QEMU fully
exited, on the SAME dist/disk.img. Nothing is written; seeing
"m14proof" again proves the file survived a full reboot.

Asserts on the serial mirror (ground truth for what the shell ran) and
the kernel's own [vfs] proof lines.
"""
import json
import os
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmpD.sock"
PHASE = int(sys.argv[1]) if len(sys.argv) > 1 else 1
ISO = sys.argv[2] if len(sys.argv) > 2 else "dist/GreenLime.iso"
SER = f"dist/disk-serial-p{PHASE}.log"

# QMP sendkey names for the few non-letter chars we type.
NAME = {
    " ": ["spc"],
    ".": ["dot"],
    "/": ["slash"],
    "-": ["minus"],
    ">": ["shift-dot"],
}


def keys_of(cmd):
    ks = []
    for ch in cmd:
        if ch.isalpha():
            ks.append(ch.lower())
        elif ch.isdigit():
            ks.append(ch)
        else:
            ks.extend(NAME.get(ch, []))
    if None in ks or "" in ks:
        raise ValueError(f"untypeable char in {cmd!r}")
    ks.append("ret")
    return ks


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
        except OSError:       # transient drvfs errors incl. mid-write reads
            continue
        if marker in data:
            return waited
    raise RuntimeError(f"marker {marker!r} never appeared")


def run(iso, cmds):
    """Boot, open the terminal, type each command. Returns serial text."""
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
            "-cdrom", iso,
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
        time.sleep(20)  # let winsrv drain + settle (wallpaper streams late)
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

        # Terminal opens on demand: start button (552,776) opens the
        # start card, then the Terminal tile — tile 1 = (510,528,126,100),
        # center (573,578).
        click_at(552, 776)
        click_at(573, 578)
        wait_marker(SER, "[winsrv] launch Terminal")

        for i, cmd in enumerate(cmds):
            print(f"typing: {cmd}", flush=True)
            type_keys(keys_of(cmd))
            time.sleep(9 if i == 0 else 7)
        qmp(s, {"execute": "quit"})
        s.close()
    finally:
        time.sleep(1)
        qemu.terminate()
        try:
            qemu.wait(timeout=10)
        except Exception:
            qemu.kill()
        ser.close()
    return open(SER, "rb").read().decode("utf-8", "replace")


def main():
    ok_all = True
    if PHASE == 1:
        data = run(ISO, [
            "echo m14proof > /disk/m14.txt",
            "cat /disk/m14.txt",
            "mkdir /disk/m14dir",
            "ls /disk",
            "echo removeme > /disk/m14d.txt",
            "rm /disk/m14d.txt",
            "rm /disk/docs",
            "cat /disk/m14d.txt",
        ])
        checks = [
            ("vfs created file (kernel proof)",
             "[vfs] created /disk/m14.txt" in data),
            ("shell wrote 9 bytes (8 + newline)",
             "wrote 9 bytes -> /disk/m14.txt" in data),
            ("vfs write proof (kernel side)",
             "[vfs] wrote 9 bytes -> /disk/m14.txt" in data),
            ("cat reads it back (same boot)",
             "m14proof" in data),
            ("vfs mkdir proof", "[vfs] mkdir /disk/m14dir ok" in data),
            ("shell mkdir ok", "mkdir: ok /disk/m14dir" in data),
            ("ls shows the new dir", "M14DIR/" in data),
            ("ls still shows the old files", "HELLO.TXT" in data),
            ("vfs deleted file (kernel proof)",
             "[vfs] deleted /disk/m14d.txt" in data),
            ("shell rm ok", "rm: ok /disk/m14d.txt" in data),
            ("rm refuses a non-empty dir", "rm: err /disk/docs" in data),
            ("deleted file gone (cat)",
             "cat: not found: /disk/m14d.txt" in data),
        ]
    else:
        data = run(ISO, [
            "cat /disk/m14.txt",
            "ls /disk",
        ])
        checks = [
            ("file survived a full reboot (cat)",
             "m14proof" in data),
            ("dir survived a full reboot (ls)", "M14DIR/" in data),
            ("no write happened in phase 2",
             "[vfs] created /disk/m14.txt" not in data),
        ]
    for name, passed in checks:
        print(f"{'OK' if passed else 'FAIL'}: {name}")
        ok_all = ok_all and passed
    if not ok_all:
        print("\n--- serial tail ---")
        print("\n".join(data.splitlines()[-45:]))
    return 0 if ok_all else 1


if __name__ == "__main__":
    sys.exit(main())
