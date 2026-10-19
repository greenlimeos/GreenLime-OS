#!/usr/bin/env python3
"""Phase B proof: DHCP lease + ICMP echo to the gateway over virtio-net.

Usage: python3 scripts/qmp_net_test.py [iso]

Boots with `-device virtio-net-pci,netdev=n0 -netdev user,id=n0`
(QEMU user NAT: guest .15, gateway/DHCP/DNS at .2) and asserts:
  1. virtio-net probed with a MAC + TX self-test (serial)
  2. [net] dhcp: 10.0.2.15/24 gw 10.0.2.2  (full DORA against slirp)
  3. [net] ping: reply from 10.0.2.2       (ICMP echo through smoltcp)
"""
import os
import re
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmp-net.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime.iso"
SER = "dist/net-serial.log"


def read_serial():
    for _ in range(6):
        try:
            return open(SER, "rb").read().decode("utf-8", "replace")
        except OSError:
            time.sleep(0.5)
    return ""


def wait_for(marker, timeout=180):
    end = time.time() + timeout
    while time.time() < end:
        if marker in read_serial():
            return True
        time.sleep(2)
    return False


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
            "-device", "virtio-net-pci,netdev=n0",
            "-netdev", "user,id=n0",
            "-serial", "stdio",
            "-display", "none",
            "-qmp", f"unix:{QSOCK},server,nowait",
        ],
        stdout=ser,
        stderr=subprocess.DEVNULL,
    )
    checks = []

    def check(name, passed):
        checks.append((name, bool(passed)))
        print(f"{'OK  ' if passed else 'FAIL'}: {name}", flush=True)

    try:
        check("virtio-net probed with MAC",
              wait_for("virtio-net: 00:"))
        check("dhcp lease from slirp",
              wait_for("[net] dhcp: 10.0.2.15/24 gw 10.0.2.2"))
        serial = read_serial()
        m = re.search(r"\[net\] ping: reply from 10\.0\.2\.2 in (\d+)ms", serial)
        # The ping runs once at boot; if we attached after it printed,
        # the marker is already in the log — either way it must be there.
        if not m:
            wait_for("[net] ping: reply from 10.0.2.2", timeout=60)
            serial = read_serial()
            m = re.search(r"\[net\] ping: reply from 10\.0\.2\.2 in (\d+)ms", serial)
        check("icmp echo reply from gateway", m is not None)
        if m:
            print(f"rtt: {m.group(1)}ms", flush=True)
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.settimeout(10)
            s.connect(QSOCK)
            s.sendall(b'{"execute": "qmp_capabilities"}\n')
            time.sleep(1)
            s.sendall(b'{"execute": "quit"}\n')
            time.sleep(1)
            s.close()
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

    ok = all(p for _, p in checks)
    print(f"RESULT: {'PASS' if ok else 'FAIL'} ({sum(p for _, p in checks)}/{len(checks)})")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
