#!/usr/bin/env python3
"""M16 Files app proof (v1.2 Windows-Explorer rework).

Usage: python3 scripts/qmp_files_test.py [iso]

Builds its OWN fresh disk (dist/files-test-disk.img: dd + mformat +
mcopy of testdata/disk — the user's dist/disk.img is never touched),
boots, and drives the explorer UI with QMP:

  - boot on a fresh volume -> "[vfs] places: 4 new"
  - Files taskbar pin -> window at ramfs root ("[files] show / items=4")
  - Documents place tile -> "[files] show /disk/Documents items=0"
  - Terminal pin -> "echo hi > /disk/Documents/note.txt" (typed; sendkey
    names carry no shift, so the shell receives an all-lowercase path —
    the LFN compare folds ASCII case, the write still hits Documents, and
    the kernel prints the stored path: /disk/documents/note.txt)
  - re-browse Documents -> items=1; select note.txt:
      exact lime selection wash (62,75,39) + lime active band on the
      Documents sidebar row (66,80,46)
  - footer Delete chip -> "[files] trash note.txt ok" + "[vfs] trash
      /disk/Documents/note.txt -> /disk/note.txt" + Documents items=0
  - Recycle Bin tile -> active lime band on the bin row; items=1
      (the ".orig" sidecar stays hidden)
  - select note.txt -> Restore chip -> "[files] restore note.txt ok"
      + "[vfs] restore note.txt -> /disk/Documents/note.txt" (bin
      shows items=0) -> re-open Documents (items=1)
  - select note.txt again -> QMP sendkey Delete (Del key path)
  - Recycle Bin -> items=1 -> Empty chip -> "[vfs] bin emptied
      (1 items)" + "[files] bin empty n=1" + bin items=0

Geometry mirrored from kernel files_geo (window pinned at the default
280,110,720,540): place rows start x=290 y=159 pitch 36 (rows 28 tall),
grid origin (443,198) with 86x84 cell pitch, footer chips right-aligned
at y 624..644 — Empty at x 937+, Restore 867, Delete 805 (6px gaps).
"""
import json
import os
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmpF.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime-release.iso"
DISK = "dist/files-test-disk.img"
SER = "dist/files-serial.log"

WIN_W, WIN_H = 1280, 800
# Files window (default 280,110,720,540) child geometry.
DOC_ROW = (357, 209)    # sidebar "Documents" row center
BIN_ROW = (357, 281)    # sidebar "Recycle Bin" row center
NOTE_CELL = (482, 235)  # grid cell 0 center
SEL_PX = (451, 206)     # inside cell 0, off the glyph + corners
ROW_PX = (298, 209)     # inside Documents row (mid rows)
BIN_ROW_PX = (298, 285)  # inside Recycle Bin row
CHIP_DEL = (833, 634)   # footer Delete chip center
CHIP_RESTORE = (899, 634)
CHIP_EMPTY = (965, 634)

# Exact blends, mirroring blend_px(): (base*(256-a) + tint*a) >> 8.
# a = F_SELECT_A (56), tint = LIME (170,232,64).
LIME_CELL = ((32 * 200 + 170 * 56) >> 8, (32 * 200 + 232 * 56) >> 8, (32 * 200 + 64 * 56) >> 8)   # 62,75,39
LIME_ROW = ((38 * 200 + 170 * 56) >> 8, (38 * 200 + 232 * 56) >> 8, (42 * 200 + 64 * 56) >> 8)    # 66,80,46

KEYNAME = {
    " ": ["spc"],
    ".": ["dot"],
    "/": ["slash"],
    "-": ["minus"],
    ">": ["shift-dot"],
    "=": ["equal"],
}


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


def build_disk():
    """Fresh 32MB FAT32 volume seeded like make-disk.sh (hello.txt +
    docs/note.txt). No places yet: the boot itself creates them."""
    subprocess.run(
        "dd if=/dev/zero of=" + DISK + " bs=1M count=32 status=none && "
        "mformat -i " + DISK + " -F -v GREENLIME :: && "
        "mcopy -s -i " + DISK + " testdata/disk/* ::",
        shell=True,
        check=True,
    )


def main():
    if os.path.exists(DISK):
        os.unlink(DISK)
    build_disk()
    print("fresh test disk built (no places yet)", flush=True)

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
    shot_files = []
    try:
        wait_marker(SER, "[shell] interactive ready")
        time.sleep(20)  # let wallpaper/demo settle (tablet input lands late)
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(60)
        s.connect(QSOCK)
        qmp(s, {"execute": "qmp_capabilities"})

        def hmp(cmd):
            return qmp(s, {"execute": "human-monitor-command",
                           "arguments": {"command-line": cmd}})

        def type_keys(cmd):
            for ch in cmd:
                ks = [ch.lower()] if ch.isalpha() or ch.isdigit() else KEYNAME[ch]
                for k in ks:
                    hmp(f"sendkey {k}")
                    time.sleep(0.4)
            hmp("sendkey ret")
            time.sleep(8)

        def to_dev(px, dim):
            return px * 32767 // (dim - 1)

        def click(gx, gy):
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "abs", "data": {"axis": "x", "value": to_dev(gx, WIN_W)}},
                {"type": "abs", "data": {"axis": "y", "value": to_dev(gy, WIN_H)}},
            ]}})
            time.sleep(2)
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "btn", "data": {"button": "left", "down": True}}]}})
            time.sleep(1)
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "btn", "data": {"button": "left", "down": False}}]}})
            time.sleep(2)

        def shot(name):
            path = f"dist/fb-files-{name}.ppm"
            try:
                os.unlink(path)
            except FileNotFoundError:
                pass
            qmp(s, {"execute": "screendump",
                    "arguments": {"filename": path, "format": "ppm"}})
            for _ in range(50):
                time.sleep(0.3)
                if os.path.exists(path) and os.path.getsize(path) > 3_000_000:
                    break
            shot_files.append(path)
            return path

        def px_at(path, x, y):
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
            o = (y * w + x) * 3
            return (px[o], px[o + 1], px[o + 2])

        # ---- open Files from the taskbar pin ----
        click(596, 776)  # Files pin (bar_cell 1)
        wait_marker(SER, "[winsrv] launch Files")
        wait_marker(SER, "[files] show / items=4")
        time.sleep(2)

        # ---- browse Documents (empty on the fresh disk) ----
        click(*DOC_ROW)
        wait_marker(SER, "[files] show /disk/Documents items=0")

        # ---- create a file through the tablet-typed terminal ----
        click(640, 776)  # Terminal pin
        wait_marker(SER, "[winsrv] launch Terminal")
        time.sleep(2)  # launch + focus settle
        type_keys("echo hi > /disk/Documents/note.txt")
        wait_marker(SER, "[vfs] wrote 3 bytes -> /disk/documents/note.txt")

        # The Terminal window (150,100,980,540) covers the Files window,
        # so sidebar clicks would land in the terminal. The Files taskbar
        # pin only FOCUSES an already-open window (no re-list), raising
        # it back to the top of the z order.
        click(596, 776)  # Files pin -> focus, no re-list
        time.sleep(2)

        # ---- re-browse via the place tile (cache refresh) + select ----
        click(*DOC_ROW)
        wait_marker(SER, "[files] show /disk/Documents items=1")
        time.sleep(1)
        click(*NOTE_CELL)  # select note.txt -> lime wash + active row
        time.sleep(2)
        shot("doc-sel")
        p = shot_files[-1]
        assert px_at(p, *SEL_PX) == LIME_CELL, (
            f"selection wash {px_at(p, *SEL_PX)} != {LIME_CELL}")
        assert px_at(p, *ROW_PX) == LIME_ROW, (
            f"Documents active band {px_at(p, *ROW_PX)} != {LIME_ROW}")

        # ---- Delete chip -> recycle bin ----
        click(*CHIP_DEL)
        wait_marker(SER, "[files] trash note.txt ok")
        wait_marker(SER, "[vfs] trash /disk/Documents/note.txt -> /disk/note.txt")
        wait_marker(SER, "[files] show /disk/Documents items=0")  # second one

        # ---- bin view: restore ----
        click(*BIN_ROW)
        wait_marker(SER, "[files] show /disk/Recycle Bin items=1")
        time.sleep(2)
        p = shot("bin-active")
        assert px_at(p, *BIN_ROW_PX) == LIME_ROW, (
            f"bin active band {px_at(p, *BIN_ROW_PX)} != {LIME_ROW}")
        click(*NOTE_CELL)  # select note.txt in the bin
        time.sleep(2)
        click(*CHIP_RESTORE)
        wait_marker(SER, "[files] restore note.txt ok")
        wait_marker(SER, "[vfs] restore note.txt -> /disk/Documents/note.txt")
        wait_marker(SER, "[files] show /disk/Recycle Bin items=0")
        click(*DOC_ROW)
        wait_marker(SER, "[files] show /disk/Documents items=1")  # third

        # ---- Del key round ----
        click(*NOTE_CELL)
        time.sleep(2)
        hmp("sendkey delete")
        time.sleep(4)
        data_mid = open(SER, "rb").read().decode("utf-8", "replace")
        assert data_mid.count("[files] trash note.txt ok") == 2, (
            f"Del key trash count {data_mid.count('[files] trash note.txt ok')}")

        # ---- bin view: empty ----
        click(*BIN_ROW)
        wait_marker(SER, "[files] show /disk/Recycle Bin items=1")  # second
        click(*CHIP_EMPTY)
        wait_marker(SER, "[vfs] bin emptied (1 items)")
        wait_marker(SER, "[files] bin empty n=1")
        wait_marker(SER, "[files] show /disk/Recycle Bin items=0")  # after empty
        shot("bin-empty")
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
        ("boot created the 4 places (fresh disk)",
         "[vfs] places: 4 new" in data),
        ("Files opened at the ramfs root (4 items)",
         data.count("[files] show / items=4") == 1),
        ("Documents emptied at every trash (initial + 2× trash refresh)",
         data.count("[files] show /disk/Documents items=0") == 3),
        ("shell wrote note.txt (3 bytes; typed path is lowercase — "
         "sendkey names carry no shift — and LFN compare is "
         "case-insensitive)",
         "[vfs] wrote 3 bytes -> /disk/documents/note.txt" in data),
        ("Documents shows the file (write re-browse + post-restore)",
         data.count("[files] show /disk/Documents items=1") == 2),
        ("the bin ate it twice, both times to the same name (vfs proof)",
         data.count("[vfs] trash /disk/Documents/note.txt -> /disk/note.txt") == 2),
        ("kernel [files] trash proof",
         data.count("[files] trash note.txt ok") == 2),
        ("bin view lost the .orig sidecar (items=1 twice + final 0)",
         data.count("[files] show /disk/Recycle Bin items=1") == 2
         and data.count("[files] show /disk/Recycle Bin items=0") == 2),
        ("Restore proof (vfs path back to source dir)",
         data.count("[vfs] restore note.txt -> /disk/Documents/note.txt") == 1
         and "[files] restore note.txt ok" in data),
        ("Empty proof (bin drained once)",
         data.count("[vfs] bin emptied (1 items)") == 1
         and "[files] bin empty n=1" in data),
        ("selection wash exact", True),  # asserted inline above
    ]
    ok_all = True
    for label, val in checks:
        print(("OK  : " if val else "FAIL: ") + label, flush=True)
        ok_all &= bool(val)
    print("RESULT: " + ("PASS" if ok_all else "FAIL") + f" ({sum(1 for _, v in checks if v)}/{len(checks)})", flush=True)
    sys.exit(0 if ok_all else 1)


if __name__ == "__main__":
    main()
