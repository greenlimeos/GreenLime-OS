#!/usr/bin/env python3
"""M17 .gle double-click probe — mirrors the USER's own scenario, minus the
place-folder exotica this probe can't pre-create on a fresh volume (the
human's Downloads copy was proven live in their own session; twice):

fresh disk seeded with an 8.3-style ``hello.gle`` (short-entry FAT name,
stored as HELLO.GLE) at the disk root, Files browsed Documents -> Back to
/disk, then a real double-click on the file cell. The kernel must log
``[files] run ...`` + ``[vfs] exec ... -> task N`` + the app's own
``[user] [hello] booted``.

Usage: python3 scripts/qmp_gle_dbl.py [iso]
"""
import json
import os
import re
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmpD.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime-release.iso"
DISK = "dist/gle-dbl-disk.img"
SER = "dist/gle-dbl-serial.log"

WIN_W, WIN_H = 1280, 800
FILES_PIN = (596, 776)
DOC_ROW = (357, 209)   # Documents place row (empty on a fresh place's disk)
BACK_CHIP = (470, 165)  # < Back chip -> parent "/disk"
# /disk grid: boot dirs sorted first (Desktop, Documents, Downloads,
# Recycle Bin = cells 0..3), then hello.gle = cell 4 (row 0, col 4):
# x = 443 + 4*86 + 39 = 826, y = 198 + 37 = 235.
HELLO_CELL = (826, 235)


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
        except OSError:
            continue
        if marker in data:
            return waited
    raise RuntimeError(f"marker {marker!r} never appeared")


def main():
    if os.path.exists(DISK):
        os.unlink(DISK)
    subprocess.run(
        "dd if=/dev/zero of=" + DISK + " bs=1M count=32 status=none && "
        "mformat -i " + DISK + " -F -v GREENLIME :: && "
        "mcopy -i " + DISK + " dist/hello.gle ::",
        shell=True,
        check=True,
    )
    print("fresh disk seeded with 8.3-style hello.gle", flush=True)

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
            "-drive", f"file={DISK},if=virtio,format=raw",
            "-device", "virtio-tablet-pci",
            "-serial", "stdio",
            "-display", "none",
            "-qmp", f"unix:{QSOCK},server,nowait",
        ],
        stdout=ser,
        stderr=subprocess.DEVNULL,
    )
    try:
        wait_marker(SER, "[shell] interactive ready")
        time.sleep(20)  # tablet input lands late
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(60)
        s.connect(QSOCK)
        qmp(s, {"execute": "qmp_capabilities"})

        def hmp(cmd):
            return qmp(s, {"execute": "human-monitor-command",
                           "arguments": {"command-line": cmd}})

        def to_dev(px, dim):
            return px * 32767 // (dim - 1)

        def click(gx, gy, gap=0.04):
            """Real double-click class click: press/release fast enough to
            hit the kernel's dbl window (press, gap, release, gap)."""
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "abs", "data": {"axis": "x", "value": to_dev(gx, WIN_W)}},
                {"type": "abs", "data": {"axis": "y", "value": to_dev(gy, WIN_H)}},
            ]}})
            time.sleep(gap)
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "btn", "data": {"button": "left", "down": True}}]}})
            time.sleep(gap)
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "btn", "data": {"button": "left", "down": False}}]}})
            time.sleep(gap)

        def dbl_click(gx, gy):
            click(gx, gy)
            time.sleep(0.08)  # ~8 ticks of the 45-tick window
            click(gx, gy)

        # Files -> Documents -> Back to /disk -> double-click hello.gle
        # (an 8.3 short-entry file at the disk root — the exact scenario
        # that used to do nothing).
        click(*FILES_PIN)
        time.sleep(2)
        wait_marker(SER, "[winsrv] launch Files")
        wait_marker(SER, "[files] show / items=4")
        click(*DOC_ROW)
        wait_marker(SER, "[files] show /disk/Documents items=0")
        click(*BACK_CHIP)
        wait_marker(SER, "[files] show /disk items=5")
        time.sleep(1)
        dbl_click(*HELLO_CELL)
        wait_marker(SER, "[vfs] exec /disk/", timeout=120)
        wait_marker(SER, "[user] [hello] booted from a .gle app", timeout=120)

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
    m = re.search(r"\[files\] run (\S*hello\S*)", data, re.I)
    run_name = m.group(1) if m else "(none)"
    execm = re.search(r"\[vfs\] exec /disk/(\S+) -> task (\d+)", data, re.I)
    winm = re.search(r"\[winsrv\] win (\d+) open at", data)
    checks = [
        ("Files listed /disk after Documents + Back (items=5)",
         "[files] show /disk items=5" in data),
        ("double-click fired the .gle arm ([files] run ...)",
         "[files] run " in data),
        ("the run line names hello (stored case: " + run_name + ")",
         m is not None and "hello" in run_name.lower()),
        ("vfs exec'd it from /disk (task "
         + (execm.group(2) if execm else "?") + ", path "
         + (execm.group(1) if execm else "?") + ")",
         execm is not None),
        ("the app really booted ring-3",
         "[user] [hello] booted from a .gle app" in data),
        ("its window opened (win " + (winm.group(1) if winm else "?") + ")",
         "[user] [hello] window open" in data),
    ]
    ok_all = True
    for label, val in checks:
        print(("OK  : " if val else "FAIL: ") + label, flush=True)
        ok_all &= bool(val)
    print("RESULT: " + ("PASS" if ok_all else "FAIL")
          + f" ({sum(1 for _, v in checks if v)}/{len(checks)})", flush=True)
    sys.exit(0 if ok_all else 1)


if __name__ == "__main__":
    main()
