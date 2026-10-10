#!/usr/bin/env python3
"""Verify activate_tablet.py flips QEMU's console pointer to the tablet.

Spawns QEMU exactly like run-qemu.sh's GUI mode but headless, runs the
helper against the HMP monitor, then reads query-mice over QMP.
"""
import json
import os
import socket
import subprocess
import sys
import time

MON = "/tmp/gl-verify-mon.sock"
QSOCK = "/tmp/gl-verify-qmp.sock"
ISO = "dist/GreenLime-release.iso"


def recv_obj(s):
    buf = b""
    while b"\n" not in buf:
        chunk = s.recv(65536)
        if not chunk:
            raise EOFError
        buf += chunk
    return json.loads(buf.split(b"\n", 1)[0])


def qmp_cmd(s, obj):
    s.sendall((json.dumps(obj) + "\n").encode())
    while True:
        r = recv_obj(s)
        if "return" in r or "error" in r:
            return r


for p in (MON, QSOCK):
    try:
        os.unlink(p)
    except FileNotFoundError:
        pass

qemu = subprocess.Popen(
    [
        "qemu-system-x86_64", "-machine", "q35", "-m", "256M",
        "-cpu", "qemu64,+smep,+smap",
        "-cdrom", ISO,
        "-drive", "file=dist/disk.img,if=virtio,format=raw",
        "-device", "virtio-tablet-pci",
        "-display", "none", "-serial", "null",
        "-monitor", f"unix:{MON},server,nowait",
        "-qmp", f"unix:{QSOCK},server,nowait",
    ],
    stdout=open("/tmp/gl-verify-qemu-stdout.txt", "wb"),
    stderr=open("/tmp/gl-verify-qemu-stderr.txt", "wb"),
)
time.sleep(2)
if qemu.poll() is not None:
    print("QEMU EXITED:", (qemu.stderr.read() or b"").decode(errors="replace"))
    sys.exit(1)
ok = False
launched = time.time()
try:
    # Helper daemonizes (double-fork) so run-qemu.sh can background it
    # without owning its lifetime, so it returns immediately. Poll for the
    # effect instead of trusting its exit status.
    subprocess.run(
        [sys.executable, "scripts/activate_tablet.py", MON],
        capture_output=True, text=True, timeout=30,
    )

    # The helper having connected proves QEMU bound the HMP socket, so the
    # QMP socket is up too; a short bounded wait is enough.
    s = None
    last = None
    for _ in range(50):
        if qemu.poll() is not None:
            break
        try:
            s = socket.socket(socket.AF_UNIX)
            s.connect(QSOCK)
            break
        except OSError as e:
            last = e
            s = None
            time.sleep(0.2)
    if s is None:
        rc = qemu.poll()
        if rc is not None:
            print(f"FAIL: qemu exited rc={rc} after {time.time()-launched:.1f}s")
            err = b""
            try:
                err = qemu.stderr.read() or b""
            except Exception:
                pass
            if err:
                print("QEMU STDERR:", err.decode(errors="replace"))
        else:
            print(f"FAIL: QMP socket never appeared (last={last!r})")
        sys.exit(1)
    s.settimeout(5)
    recv_obj(s)  # greeting
    qmp_cmd(s, {"execute": "qmp_capabilities"})

    mice = None
    deadline = time.time() + 15
    while time.time() < deadline:
        mice = qmp_cmd(s, {"execute": "query-mice"})["return"]
        cur = [m for m in mice if m["current"]]
        # The helper runs mouse_set concurrently; wait for it to land.
        if cur and cur[0]["absolute"]:
            break
        time.sleep(0.5)

    for m in mice:
        print(f"  #{m['index']} {m['name']!r} "
              f"absolute={m['absolute']} current={m['current']}")
    cur = [m for m in mice if m["current"]]
    if cur and cur[0]["absolute"]:
        print("PASS: console pointer is the tablet (absolute, no grab)")
        ok = True
    else:
        print("FAIL: console pointer still relative -> GTK would grab")
    s.close()
finally:
    qemu.terminate()
    try:
        qemu.wait(timeout=5)
    except subprocess.TimeoutExpired:
        qemu.kill()
    for p in (MON, QSOCK):
        try:
            os.unlink(p)
        except FileNotFoundError:
            pass
sys.exit(0 if ok else 1)
