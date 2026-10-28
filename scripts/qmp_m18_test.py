#!/usr/bin/env python3
"""M18 suite — right-click menus (Files + Desktop), Notepad, .txt law.

Fresh 32 MB volume (the human's dist/disk.img is NEVER touched), then:

  DESKTOP: right-click (200,300)          -> [desktop] menu open
           item 0 New text file           -> /disk/Desktop/New file 1.txt
                                             + notepad task, caps=0x0008
           focus notepad, type:
             append hello from probe      -> buffer edit
             save                         -> [vfs] setsize -> 0, [vfs] wrote,
                                             [notepad] saved N bytes -> path
             quit                         -> saved-if-dirty, task exits (0)
           right-click again, item 1      -> [desktop] refresh

  FILES:   pin -> show / (ramfs root)
           right-click in the window body -> [files] menu open
           item 0 at root                 -> REFUSED (not a disk folder)
                                             + auto-browse [files] show /disk
           right-click again, item 0      -> /disk/New file 1.txt
                                             + a SECOND notepad (new id!)
                                             + [files] show /disk items=5
           double-click the .txt cell     -> [files] open txt + open_txt
                                             + third notepad opened line

Notepad is born with EXACTLY caps=0x0008 — every caps stamp is paired
to its own spawn line. Ids may legally recycle after an editor exits
(LIFO slot law); only concurrent editors must differ. Menu anchors
are PARSED, not hardcoded: the QMP abs round-trip can land 1 px off
the probe point (proved: (200,300) mapped to 199,299 — exact-string
markers starved there twice).
Usage: python3 scripts/qmp_m18_test.py [iso]
"""
import json
import os
import re
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmpM.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime-release.iso"
DISK = "dist/m18-disk.img"
SER = "dist/m18-serial.log"
SEED = "/tmp/m18-seed"

WIN_W, WIN_H = 1280, 800
FILES_PIN = (596, 776)  # proven by qmp_gle_dbl.py
DESK_RC = (200, 300)  # bare desktop (no windows at boot any more)
FILE_BODY = (500, 300)  # inside the Files window grid
# /disk grid: 4 place dirs sorted first (cells 0..3) -> first file = cell 4:
# x = 443 + 4*86 + 39 = 826, y = 198 + 37 = 235 (row 0, col 4).
TXT_CELL = (826, 235)

KEYNAME = {" ": ["spc"], ".": ["dot"], "/": ["slash"], "-": ["minus"]}


def qmp(s, cmd):
    s.sendall(json.dumps(cmd).encode() + b"\n")
    dec = json.JSONDecoder()
    buf = ""
    while True:
        chunk = s.recv(65536).decode()
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


def wait_menu(path, tag, timeout=420):
    """Wait for the right-click popup and return its REAL anchor (x, y).

    The popup opens at the cursor's mapped pixel and the QMP abs-pointer
    round-trip can land 1 px off the probe point — an exact-string marker
    starved there (chained suites proved it twice). Parse the newest
    `[{tag}] menu open at X,Y` line instead so item clicks stay on-row.
    """
    pat = re.compile(r"\[" + tag + r"\] menu open at (\d+),(\d+)")
    waited = 0
    while waited < timeout:
        time.sleep(2)
        waited += 2
        try:
            data = open(path, "rb").read().decode("utf-8", "replace")
        except OSError:
            continue
        hits = pat.findall(data)
        if hits:
            ax, ay = hits[-1]
            return int(ax), int(ay)
    raise RuntimeError(f"menu {tag!r} never appeared")


def build_disk():
    if os.path.exists(SEED):
        os.makedirs(SEED, exist_ok=True)
    for p in (DISK, QSOCK, SER):
        if os.path.exists(p):
            os.unlink(p)
    subprocess.run(
        "dd if=/dev/zero of=" + DISK + " bs=1M count=32 status=none && "
        "mformat -i " + DISK + " -F -v GREENLIME ::",
        shell=True,
        check=True,
    )
    print("fresh blank disk (the UI creates everything)", flush=True)


def main():
    build_disk()
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
        time.sleep(18)
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(60)
        s.connect(QSOCK)
        qmp(s, {"execute": "qmp_capabilities"})

        def hmp(cmd):
            return qmp(s, {"execute": "human-monitor-command",
                           "arguments": {"command-line": cmd}})

        def to_dev(px, dim):
            return px * 32767 // (dim - 1)

        def move(gx, gy):
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "abs", "data": {"axis": "x", "value": to_dev(gx, WIN_W)}},
                {"type": "abs", "data": {"axis": "y", "value": to_dev(gy, WIN_H)}},
            ]}})
            time.sleep(0.05)

        def btn(name, down):
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "btn", "data": {"button": name, "down": down}}]}})
            time.sleep(0.05)

        def click(gx, gy, gap=0.05):
            move(gx, gy)
            btn("left", True)
            time.sleep(gap)
            btn("left", False)
            time.sleep(gap)

        def rclick(gx, gy):
            move(gx, gy)
            btn("right", True)
            time.sleep(0.05)
            btn("right", False)
            time.sleep(0.12)

        def dbl_click(gx, gy):
            click(gx, gy)
            time.sleep(0.08)
            click(gx, gy)

        def type_text(cmd):
            move(640, 500)  # dead desktop zone keeps hover states quiet
            for ch in cmd:
                if ch.isalpha() or ch.isdigit():
                    ks = [ch.lower()]
                else:
                    ks = KEYNAME[ch]
                for k in ks:
                    hmp(f"sendkey {k}")
                    time.sleep(0.12)
            hmp("sendkey ret")

        def ser_text():
            return open(SER, "rb").read().decode("utf-8", "replace")

        def wins_opened_after(offset):
            """(id, x, y, w, h) of every [winsrv] win N open after `offset`."""
            out, pat = [], re.compile(
                r"\[winsrv\] win (\d+) open at (\d+),(\d+) (\d+)x(\d+)")
            for m in pat.finditer(ser_text()[offset:]):
                out.append(tuple(int(g) for g in m.groups()))
            return out

        # ---- 1. Desktop right-click -> New text file (Notepad) ----
        rclick(*DESK_RC)
        ax, ay = wait_menu(SER, "desktop")
        click(ax + 84, ay + 15)  # item 0 row (ay..ay+30)
        wait_marker(SER, "[vfs] created /disk/Desktop/New file 1.txt")
        time.sleep(4)
        data = ser_text()
        m = re.search(
            r"\[vfs\] open txt /disk/Desktop/New file 1\.txt -> task (\d+)",
            data)
        assert m, f"open txt task line missing\n{data[-1500:]}"
        id1 = int(m.group(1))
        wait_marker(SER, "[notepad] opened /disk/Desktop/New file 1.txt")
        wait_marker(SER, "[notepad] loaded 0 bytes")

        # step 1 window check + focus it (FRESH read: the [winsrv] win
        # opened line can land after the earlier snapshot was captured)
        data = ser_text()
        delta = wins_opened_after(data.find("open txt /disk/Desktop"))
        assert delta, "notepad window never opened"
        (wid, wx, wy, ww, wh) = delta[-1]
        click(wx + ww // 2, wy + 17)
        time.sleep(2)
        assert re.search(rf"\[winsrv\] focus win {wid}\b", ser_text()), \
            f"notepad win {wid} not focused"

        # ---- 2. type: append, save, quit ----
        type_text("append hello from probe")
        time.sleep(2)
        type_text("save")
        wait_marker(SER,
                    "[vfs] setsize /disk/Desktop/New file 1.txt -> 0")
        wait_marker(SER,
                    "[vfs] wrote 17 bytes -> /disk/Desktop/New file 1.txt")
        wait_marker(SER,
                    "[notepad] saved 17 bytes -> /disk/Desktop/New file 1.txt")
        time.sleep(2)
        type_text("quit")
        wait_marker(SER, "[notepad] quit - bye")
        wait_marker(SER, f"[sched] task {id1} exited (0)")

        # ---- 3. Desktop right-click -> Refresh ----
        rclick(*DESK_RC)
        ax, ay = wait_menu(SER, "desktop")  # fresh anchor for this popup
        click(ax + 84, ay + 45)  # item 1 row (ay+30..ay+60)
        wait_marker(SER, "[desktop] refresh")

        # ---- 4. Files: right-click in the window at ramfs root ----
        click(*FILES_PIN)
        wait_marker(SER, "[winsrv] launch Files")
        wait_marker(SER, "[files] show / items=4")
        rclick(*FILE_BODY)
        ax, ay = wait_menu(SER, "files")
        click(ax + 84, ay + 15)  # item 0 — but the browse dir is "/" (ramfs!)
        wait_marker(SER, "[files] new file refused (not a disk folder)")
        wait_marker(SER, "[files] show /disk items=4")  # auto-browse to /disk

        # ---- 5. right-click again: /disk is a disk dir now ----
        rclick(*FILE_BODY)
        ax, ay = wait_menu(SER, "files")  # fresh anchor for this popup
        click(ax + 84, ay + 15)
        wait_marker(SER, "[vfs] created /disk/New file 1.txt")
        time.sleep(4)
        wait_marker(SER, "[files] show /disk items=5")
        # NOTE: no id-distinctness assert here — the desktop editor quit
        # a step ago, so the slot law may legally hand its id right back.

        # ---- 6. .txt double-click binding (third id) ----
        time.sleep(2)
        dbl_click(*TXT_CELL)
        wait_marker(SER, "[files] open txt New file 1.txt")
        wait_marker(SER, "[vfs] open txt /disk/New file 1.txt -> task ",
                    timeout=60)
        time.sleep(3)
        wait_count(SER, "[notepad] opened /disk/New file 1.txt", 2)

        # ---- 7. the caps law — bind each stamp to ITS spawn line ----
        # The "task N stack ... caps=" stamp races other proof lines in
        # serial order, and freed slots recycle (init exits as task 3;
        # notepad can be born as task 3 over and over). So: pair every
        # notepad spawn with the nearest caps stamp AFTER it.
        data = ser_text()
        spawns = [int(m.group(1)) for m in re.finditer(
            r"\[sched\] spawned user task (\d+) \(notepad\)", data)]
        opens = [int(t) for t in re.findall(
            r"\[vfs\] open txt /disk(?:/\S*)?/New file \d\.txt -> task (\d+)",
            data)]
        assert len(spawns) >= 3, f"expected >=3 notepad spawns, saw {spawns}"
        assert len(opens) == len(spawns), \
            f"open txt ({opens}) / spawn ({spawns}) pairing broken"
        for sid in spawns:
            pos = data.find(f"spawned user task {sid} (notepad)")
            mm = re.search(
                rf"\[sched\] task {sid} stack \S+ caps=(0x[0-9a-fA-F]+)",
                data[pos:])
            assert mm, f"caps stamp missing after task {sid} spawn"
            assert int(mm.group(1), 16) == 0x0008, \
                f"notepad task {sid} caps {mm.group(1)} != CAP_DISK"
        # The dbl-click editor joins a LIVE editor (the menu one): only
        # dead slots recycle, so the pair must hold different slots.
        assert opens[2] != opens[1], \
            f"concurrent editors shared one slot: {opens}"

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
    spawns = [int(m.group(1)) for m in re.finditer(
        r"\[sched\] spawned user task (\d+) \(notepad\)", data)]
    opens = [int(t) for t in re.findall(
        r"\[vfs\] open txt /disk(?:/\S*)?/New file \d\.txt -> task (\d+)",
        data)]
    caps_ok = bool(spawns) and all(
        int(re.search(
            rf"\[sched\] task {sid} stack \S+ caps=(0x[0-9a-fA-F]+)",
            data[data.find(f"spawned user task {sid} (notepad)"):])
            .group(1), 16) == 0x0008
        for sid in spawns)
    checks = [
        ("desktop right-click opened the menu",
         "[desktop] menu open at " in data),
        ("desktop New text file created the file",
         "[vfs] created /disk/Desktop/New file 1.txt" in data),
        ("notepad spawned over OP_TXT_TAKE (task) + opened it",
         "[vfs] open txt /disk/Desktop/New file 1.txt -> task " in data
         and "[notepad] opened /disk/Desktop/New file 1.txt" in data),
        ("notepad window really opened",
         bool(wupd(data, "open txt /disk/Desktop"))),
        ("append reached the buffer + setsize/wrote proof",
         "[vfs] setsize /disk/Desktop/New file 1.txt -> 0" in data
         and "[vfs] wrote 17 bytes -> /disk/Desktop/New file 1.txt" in data),
        ("[notepad] saved proof line with the real path",
         "[notepad] saved 17 bytes -> /disk/Desktop/New file 1.txt" in data),
        ("quit closed notepad (task exited 0)",
         "[notepad] quit - bye" in data),
        ("desktop Refresh recomposes",
         "[desktop] refresh" in data),
        ("Files menu opened in-window",
         "[files] menu open at " in data),
        ("ramfs root refusal + auto-browse to /disk",
         "[files] new file refused (not a disk folder)" in data
         and "[files] show /disk items=4" in data),
        ("Files-menu new file + visible listing (items=5)",
         "[vfs] created /disk/New file 1.txt" in data
         and "[files] show /disk items=5" in data),
        (".txt double-click binding fires",
         "[files] open txt New file 1.txt" in data
         and "[vfs] open txt /disk/New file 1.txt -> task " in data),
        ("3 editors born (desktop, Files menu, dbl-click)",
         len(spawns) >= 3 and len(opens) == len(spawns)),
        ("every editor born with EXACTLY CAP_DISK (pair-to-spawn law)",
         caps_ok),
        ("concurrent editors hold distinct slots (recycle is for the dead)",
         len(opens) >= 3 and opens[2] != opens[1]),
        ("clean boot law: NO demo window at boot",
         "[sched] spawned user task 5 (demo)" not in data),
        ("net slid into slot 5", "[sched] spawned kernel task 5 (net)" in data),
    ]
    ok_all = True
    for label, val in checks:
        print(("OK  : " if val else "FAIL: ") + label, flush=True)
        ok_all &= bool(val)
    n_ok = sum(1 for _, v in checks if v)
    print(f"RESULT: {'PASS' if ok_all else 'FAIL'} ({n_ok}/{len(checks)})",
          flush=True)
    sys.exit(0 if ok_all else 1)


def wupd(data, after):
    """windows opened after the `after` marker (list of ids)."""
    i = data.find(after)
    if i < 0:
        return []
    return re.findall(r"\[winsrv\] win (\d+) open at ", data[i:])


def wait_count(path, marker, n, timeout=120):
    waited = 0
    while waited < timeout:
        time.sleep(2)
        waited += 2
        try:
            c = open(path, "rb").read().decode("utf-8", "replace").count(marker)
        except OSError:
            c = 0
        if c >= n:
            return waited
    raise RuntimeError(f"count of {marker!r} never reached {n}")


if __name__ == "__main__":
    main()
