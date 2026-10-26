#!/usr/bin/env python3
# Low-RAM boot proof (VirtualBox OS V4 profile: 64 MiB, qemu64, no virtio).
# Usage: python3 scripts/qmp_lowram_test.py [iso]   (kept in scripts/ for reuse)

import json
import os
import socket
import subprocess
import sys
import time
import tempfile

ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime-release.iso"
SER = os.path.join(tempfile.gettempdir(), "lowram-serial.log")


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


def main():
    for p in (SER,):
        try:
            os.unlink(p)
        except FileNotFoundError:
            pass
    ser = open(SER, "wb")
    # Same VirtualBox profile as qmp_vboxsim_test.py, but RamSize=64M to match
    # the failing VM (RamSize 0x4000000).
    qemu = subprocess.Popen(
        [
            "qemu-system-x86_64",
            "-cpu", "qemu64",
            "-cdrom", ISO,
            "-m", "64M",
            "-vga", "std",
            "-machine", "q35",
            "-no-reboot",
            "-boot", "order=d",
            "-serial", "stdio",
            "-display", "none",
            # no QMP socket: target is serial-marker reading only
        ],
        stdout=ser,
        stderr=subprocess.DEVNULL,
    )
    try:
        waited = 0
        ready = False
        while waited < 300:
            time.sleep(2)
            waited += 2
            try:
                data = open(SER, "rb").read().decode("utf-8", "replace")
            except FileNotFoundError:
                continue
            if "interactive ready" in data:
                ready = True
                time.sleep(5)
                break
    finally:
        time.sleep(1)
        qemu.terminate()
        try:
            qemu.wait(timeout=10)
            terminated = True
        except Exception:
            qemu.kill()
            terminated = False
        ser.close()

    data = open(SER, "rb").read().decode("utf-8", "replace")
    tail = data.strip().splitlines()
    if ready:
        stuck = "last line before ready: " + (tail[-1] if tail else "?")
    else:
        stuck = "DID NOT reach [shell] interactive ready; last line: " + (tail[-1] if tail else "<none>")
    checks = [
        ("boot reached interactive ready", ready),
        ("no kernel faults", "[FAULT]" not in data and "[PANIC]" not in data),
        ("no console-init wedge", "[console] COM1 init failed" not in data
         ),
        ("desktop painted (or serial-only fallback noted)",
         "[winsrv] desktop painted" in data),
        ("wallpaper streamed", "[wall] wallpaper streamed" in data),
    ]
    ok = True
    for name, passed in checks:
        print(f"{'OK' if passed else 'FAIL'}: {name}")
        ok = ok and passed
    print(stuck)
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
