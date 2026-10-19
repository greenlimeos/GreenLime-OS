#!/usr/bin/env python3
"""Phase A step 1 proof: APs start, print, and park — one headless boot.

Usage: python3 scripts/qmp_smp_test.py [iso]

Boots with -smp 4 and asserts:
  1. [smp] 4 cpus, bsp lapic 0          (MP response parsed)
  2. three "[smp] cpu N online" lines    (every AP checked in)
  3. [smp] 3/3 APs online               (bounded wait, no TIMEOUT)
  4. three "[smp] cpu N woke (ipi" lines  (first inter-processor interrupt)
  5. [smp] 3/3 APs woke (ipi)            (BSP counted every answer)
  6. [shell] interactive ready           (BSP proceeded normally)
  7. one "[sched] cpu N running tasks" per cpu (APs schedule real tasks)
"""
import os
import re
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmp-smp.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime.iso"
SER = "dist/smp-serial.log"


def read_serial():
    for _ in range(6):
        try:
            return open(SER, "rb").read().decode("utf-8", "replace")
        except OSError:
            time.sleep(0.5)
    return ""


def wait_for(marker, timeout=420):
    end = time.time() + timeout
    while time.time() < end:
        if marker in read_serial():
            return True
        time.sleep(2)
    return False


def main():
    try:
        os.unlink(QSOCK)
    except FileNotFoundError:
        pass
    try:
        os.unlink(SER)
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
            "-smp", "4",
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
    checks = []

    def check(name, passed):
        checks.append((name, bool(passed)))
        print(f"{'OK  ' if passed else 'FAIL'}: {name}", flush=True)

    try:
        check("MP response parsed (4 cpus)",
              wait_for("[smp] 4 cpus, bsp lapic 0"))
        check("all 3 APs checked in",
              wait_for("[smp] 3/3 APs online"))
        serial = read_serial()
        n_online = len(re.findall(r"\[smp\] cpu \d+ online", serial))
        print(f"ap online markers: {n_online}", flush=True)
        check("three online markers, no TIMEOUT",
              n_online == 3 and "TIMEOUT" not in serial)
        check("BSP reached interactive shell",
              wait_for("[shell] interactive ready"))
        time.sleep(10)  # let every CPU pick up tasks at least once
        serial = read_serial()
        cpus_running = sorted(set(re.findall(r"\[sched\] cpu (\d) running tasks", serial)))
        print(f"cpus running tasks: {cpus_running}", flush=True)
        check("all 4 CPUs ran tasks",
              cpus_running == ["0", "1", "2", "3"])
        check("all 3 APs woke via IPI",
              wait_for("[smp] 3/3 APs woke (ipi)"))
        serial = read_serial()
        n_woke = len(re.findall(r"\[smp\] cpu \d+ woke \(ipi", serial))
        print(f"ipi woke markers: {n_woke}", flush=True)
        check("three woke markers, no delivery failures",
              n_woke == 3 and "not delivered" not in serial)
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
    if not ok:
        print("--- serial tail ---")
        print("\n".join(read_serial().splitlines()[-25:]))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
