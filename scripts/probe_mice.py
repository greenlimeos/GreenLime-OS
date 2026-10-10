#!/usr/bin/env python3
"""Probe: which pointer handler does QEMU pick for run-qemu.sh's device set?

query-mice lists every mouse-type handler in registration order; `current`
(head of list) is what qemu_input_is_absolute(con) consults when nothing is
console-bound — i.e. what -display gtk uses to choose grab-vs-passthrough.
No guest boot needed: handlers register at QEMU startup.
"""
import json
import os
import socket
import subprocess
import sys
import time

SOCK = "/tmp/gl-mice-probe.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime-release.iso"
EXTRA = sys.argv[2:]  # extra QEMU args (e.g. -nodefaults)


def qmp_connect(path, timeout=10):
    end = time.time() + timeout
    while True:
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.connect(path)
            return s
        except OSError:
            if time.time() > end:
                raise
            time.sleep(0.2)


def recv_obj(s):
    buf = b""
    while True:
        chunk = s.recv(65536)
        if not chunk:
            raise EOFError("qemu closed")
        buf += chunk
        if b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            return json.loads(line)


def qmp(s, obj):
    s.sendall((json.dumps(obj) + "\n").encode())
    while True:
        r = recv_obj(s)
        if "return" in r or "error" in r:
            return r


cmd = [
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
    "-display", "none",
    "-serial", "null",
    "-qmp", f"unix:{SOCK},server,nowait",
]
try:
    os.unlink(SOCK)
except FileNotFoundError:
    pass
p = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    s = qmp_connect(SOCK)
    greet = recv_obj(s)
    print("qmp:", greet.get("QMP", {}).get("version", {}).get("qemu", "?"))
    print("caps:", qmp(s, {"execute": "qmp_capabilities"}))
    r = qmp(s, {"execute": "query-mice"})
    mice = r.get("return", [])
    print(f"mice: {len(mice)} pointer handler(s)")
    for m in mice:
        print(
            f"  #{m['index']} name={m['name']!r} "
            f"absolute={m['absolute']} current={m['current']}"
        )
    cur = [m for m in mice if m["current"]]
    if cur:
        print("=> console pointer mode:",
              "ABSOLUTE (seamless, no grab)" if cur[0]["absolute"]
              else "RELATIVE (grab on click, host cursor hidden)")
    s.close()
finally:
    p.terminate()
    try:
        p.wait(timeout=5)
    except subprocess.TimeoutExpired:
        p.kill()
    try:
        os.unlink(SOCK)
    except FileNotFoundError:
        pass
