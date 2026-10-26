#!/usr/bin/env python3
"""VirtualBox boot simulation: same CPU story (no +smep,+smap) and the
same device story (no virtio at all — VBox "Other (64-bit)" exposes
none of our virtio models). Boots the ISO headless, waits for the
desktop, prints the proof lines, exits 0/1.

Usage: python3 scripts/qmp_vboxsim_test.py [iso]   (kept in scripts/ for reuse)
"""
import json
import os
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmp-vboxsim.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime-release.iso"
SER = "dist/vboxsim-serial.log"


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
    for p in (QSOCK, SER):
        try:
            os.unlink(p)
        except FileNotFoundError:
            pass
    ser = open(SER, "wb")
    # VirtualBox profile: plain qemu64 CPU (no SMEP/SMAP exposed), NO
    # virtio-blk / virtio-net / virtio-tablet — everything else mirrors
    # the run-qemu.sh defaults (q35, 512M, std vga, -boot order=d).
    qemu = subprocess.Popen(
        [
            "qemu-system-x86_64",
            "-cpu", "qemu64",
            "-cdrom", ISO,
            "-m", "512M",
            "-vga", "std",
            "-machine", "q35",
            "-no-reboot",
            "-boot", "order=d",
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
        s.settimeout(60)
        s.connect(QSOCK)
        qmp(s, {"execute": "qmp_capabilities"})

        waited = 0
        ready = False
        while waited < 300:
            time.sleep(2)
            waited += 2
            try:
                data = open(SER, "rb").read().decode("utf-8", "replace")
            except FileNotFoundError:
                continue
            if "[shell] interactive ready" in data:
                ready = True
                time.sleep(25)  # settle (wallpaper streams late in debug)
                break
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
        ("arch: smep/smap gracefully OFF (CPUID-gated)",
         "[arch] protections: smep=false smap=false" in data),
        ("desktop painted", "[winsrv] desktop painted" in data),
        ("shell interactive", "[shell] interactive ready" in data),
        ("demo client window open", "[winsrv] win 2 open" in data),
        ("wallpaper streamed (ring-3 task)",
         "[wall] wallpaper streamed" in data),
        ("no kernel faults", "[FAULT]" not in data and "[PANIC]" not in data),
    ]
    ok = True
    for name, passed in checks:
        print(f"{'OK' if passed else 'FAIL'}: {name}")
        ok = ok and passed
    if not ready:
        print("FAIL: shell never became interactive")
        ok = False
    # Show the interesting boot lines for the report.
    for line in data.splitlines():
        if any(m in line for m in ("[arch]", "[smp]", "[winsrv] desktop",
                                   "wallpaper: ", "[vbox", "[ahci] AHCI",
                                   "SMBus", "no NIC", "interrupts on")):
            print("  |", line.strip())
    if not ok:
        print("\n--- serial tail ---")
        print("\n".join(data.splitlines()[-40:]))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
