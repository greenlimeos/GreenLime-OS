#!/usr/bin/env python3
"""M17 .gle EXEC suite — the third-party app truck, end to end.

Drives a fresh 32 MB volume (own image; the human's dist/disk.img is
never touched) seeded with three containers, then:

  Terminal: exec /disk/helloapp.gle   -> app #1 (window 1)
            exec refusals: /motd.txt (never /disk/*), badapp.gle (CRC),
            badelf.gle (bad payload magic), `exec` alone (usage)
            exec /disk/helloapp.gle    -> app #2 CONCURRENT with #1
  Files:    Documents -> Back chip -> /disk -> double-click helloapp.gle
                                     -> app #3 (three live ring-3 tasks!)
  Then:     focus each app window by clicking its titlebar strip,
            screenshot-assert the focused lime-titlebar... (bg 43,43,43)
            and `quit` all three; every task exits cleanly.

IDs are NOT hardcoded: freed slots get reused by the scheduler (init's
slot 3 is the classic first corpse), so the suite collects the ids it
saw and asserts they are DISTINCT and all carried caps=0x0002.

Usage: python3 scripts/qmp_gle_test.py [iso]
"""
import json
import os
import re
import socket
import struct
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmpG.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime-release.iso"
DISK = "dist/gle-test-disk.img"
SER = "dist/gle-serial.log"
SEED = "/tmp/gle-seed"

WIN_W, WIN_H = 1280, 800
TERM_PIN = (640, 776)
FILES_PIN = (596, 776)
DOC_ROW = (357, 209)   # Documents place row
BACK_CHIP = (470, 165)  # < Back chip in the breadcrumb strip
HELLO_CELL = (482, 319)  # /disk grid: dirs cells 0..3, files 4,5 -> helloapp = 6
# (row = 6 // 6 per-row cells -> row 1, col 0; y = 198 + 84 + 37)
TITLE_FG = (43, 43, 43)  # focused titlebar band

KEYNAME = {" ": ["spc"], ".": ["dot"], "/": ["slash"], "-": ["minus"]}


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


def wait_count(path, marker, n, timeout=240):
    """wait_marker, but the marker must be seen N times — proof lines that
    stack (booted x2, quit x3) can't be asserted with presence alone."""
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


def build_seed():
    os.makedirs(SEED, exist_ok=True)
    # badapp.gle: a CRC-broken copy of the good container.
    blob = bytearray(open("dist/hello.gle", "rb").read())
    blob[-1] ^= 0xFF
    open(f"{SEED}/badapp.gle", "wb").write(bytes(blob))
    # badelf.gle: valid CRC, garbage payload (the ELF sniffer refuses);
    # the fake ELF still carries honest header fields -- the KERNEL is the
    # one that must refuse this one later.
    elf = bytearray(64)
    elf[:4] = b"\x7fELF"
    struct.pack_into("<H", elf, 16, 2)     # ET_EXEC
    struct.pack_into("<H", elf, 18, 0x3E)  # EM_X86_64
    with open(f"{SEED}/fake.elf", "wb") as f:
        f.write(bytes(elf))
    with open(f"{SEED}/bad.json", "w") as f:
        json.dump({"name": "badelf", "version": "9", "author": "",
                   "icon": None, "gui": False}, f)
    subprocess.run(
        ["python3", "scripts/gle-pack.py", "pack", f"{SEED}/bad.json",
         "--elf", f"{SEED}/fake.elf", "--out", f"{SEED}/badelf.gle"],
        check=True, stdout=subprocess.DEVNULL,
    )
    # badapp/badelf must be 8.3-compliant for mcopy without LFN; helloapp
    # gets a long name on purpose (LFN case-preserved path).
    for name in ("badapp.gle", "badelf.gle"):
        assert len(name.split(".")[0]) <= 8


def build_disk():
    subprocess.run(
        "dd if=/dev/zero of=" + DISK + " bs=1M count=32 status=none && "
        "mformat -i " + DISK + " -F -v GREENLIME :: && "
        "mcopy -i " + DISK + " dist/hello.gle ::/helloapp.gle && "
        f"mcopy -i {DISK} {SEED}/badapp.gle :: && "
        f"mcopy -i {DISK} {SEED}/badelf.gle ::",
        shell=True, check=True,
    )


def main():
    build_seed()
    for p in (DISK, QSOCK, SER):
        if os.path.exists(p):
            os.unlink(p)
    build_disk()
    print("fresh disk: helloapp.gle + badapp.gle + badelf.gle", flush=True)

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
    shots = []
    try:
        wait_marker(SER, "[shell] interactive ready")
        time.sleep(20)
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(60)
        s.connect(QSOCK)
        qmp(s, {"execute": "qmp_capabilities"})

        def hmp(cmd):
            return qmp(s, {"execute": "human-monitor-command",
                           "arguments": {"command-line": cmd}})

        def type_keys(cmd):
            for ch in cmd:
                ks = [ch.lower()] if ch.isalpha() or ch.isdigit() \
                    else KEYNAME[ch]
                for k in ks:
                    hmp(f"sendkey {k}")
                    time.sleep(0.4)
            hmp("sendkey ret")
            time.sleep(4)

        def to_dev(px, dim):
            return px * 32767 // (dim - 1)

        def click(gx, gy, gap=0.04):
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
            time.sleep(0.08)
            click(gx, gy)

        def shot(name):
            path = f"dist/fb-gle-{name}.ppm"
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
            shots.append(path)
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

        def ser_text():
            return open(SER, "rb").read().decode("utf-8", "replace")

        def wins_parsed():
            out = []
            pat = re.compile(r"\[winsrv\] win (\d+) open at (\d+),(\d+) (\d+)x(\d+)")
            for m in pat.finditer(ser_text()):
                out.append({
                    "id": int(m.group(1)), "x": int(m.group(2)),
                    "y": int(m.group(3)), "w": int(m.group(4)),
                    "h": int(m.group(5)), "off": m.start(),
                })
            return out

        # FIRST: everything typed from the terminal, while it still owns
        # the keyboard. exec #1, the refusal circuit, then exec #2 —
        # two apps live at once (distinct scheduler slots).
        type_keys("exec /disk/helloapp.gle")
        wait_marker(SER, "[vfs] exec /disk/helloapp.gle -> task ")
        wait_count(SER, "[user] [hello] window open", 1)

        type_keys("exec /motd.txt")
        wait_marker(SER, "[vfs] exec refused /motd.txt: "
                         "[gle] only /disk/* paths")
        type_keys("exec /disk/badapp.gle")
        wait_marker(SER, "[vfs] exec refused /disk/badapp.gle: "
                         "[gle] CRC mismatch (corrupt or tampered)")
        type_keys("exec /disk/badelf.gle")
        wait_marker(SER, "[vfs] exec refused /disk/badelf.gle: "
                         "[elf] bad magic")
        type_keys("exec")
        wait_marker(SER, "exec: missing path")

        type_keys("exec /disk/helloapp.gle")
        wait_count(SER, "[vfs] exec /disk/helloapp.gle -> task ", 2)
        wait_count(SER, "[user] [hello] booted from a .gle app", 2)
        time.sleep(2)
        data = ser_text()
        execs = re.findall(
            r"\[vfs\] exec /disk/helloapp\.gle -> task (\d+)", data, re.I)
        assert len(execs) == 2 and len(set(execs)) == 2, \
            f"two DISTINCT slots required (reuse ok), saw {execs}"
        wins = re.findall(r"\[winsrv\] win (\d+) open at 700,(\d+) 540x330",
                          data)
        app_wins = [int(y) for _, y in wins if int(y) >= 200]
        assert app_wins == [200, 240], f"cascade saw {wins}"

        # NOW the GUI turn: focus app #1 by clicking its titlebar strip
        # (windows open UNFOCUSED; its strip y 200..234 is un-shadowed by
        # the 700,240 window above it). Then screenshot the focused band.
        winfo = wins_parsed()
        w1 = [w for w in winfo if w["x"] == 700 and w["y"] == 200][-1]
        click(w1["x"] + w1["w"] // 2, w1["y"] + 17)
        time.sleep(3)
        data = ser_text()
        assert re.search(rf"\[winsrv\] focus win {w1['id']}", data), \
            f"app1 window not focused (win {w1['id']})"
        p = shot("app1")
        assert px_at(p, w1["x"] + w1["w"] // 2, w1["y"] + 17) == TITLE_FG, \
            "app1 titlebar not in focused colors"

        # keyboard follows focus: hello #1 receives the typing
        type_keys("hi")
        wait_marker(SER, "[user] [hello] got: hi")

        # ---- Files: Documents -> Back -> /disk -> double-click #3 ----
        click(*FILES_PIN)
        wait_marker(SER, "[winsrv] launch Files")
        wait_marker(SER, "[files] show / items=4")
        click(*DOC_ROW)
        wait_marker(SER, "[files] show /disk/Documents items=0")
        click(*BACK_CHIP)
        wait_marker(SER, "[files] show /disk items=7")
        time.sleep(1)
        dbl_click(*HELLO_CELL)
        wait_marker(SER, "[files] run helloapp.gle")
        wait_marker(SER, "[vfs] exec /disk/helloapp.gle -> task ", timeout=30)
        time.sleep(3)
        data = ser_text()
        booted = data.count("[user] [hello] booted from a .gle app")
        assert booted == 3, f"three concurrent instances expected, booted {booted}"
        execs = re.findall(
            r"\[vfs\] exec /disk/helloapp\.gle -> task (\d+)", data, re.I)
        assert len(execs) == 3 and len(set(execs)) == 3, \
            f"three DISTINCT task ids required (slot reuse ok), saw {execs}"

        # caps law: every spawned .gle task must carry USER_DEFAULT.
        for tid in execs:
            m = re.search(
                rf"\[sched\] task {tid} stack 0x[0-9a-f]+ caps=(0x[0-9a-f]+)",
                data)
            assert m, f"[sched] caps line missing for task {tid}"
            assert int(m.group(1), 16) == 0x0002, \
                f"task {tid} caps {m.group(1)} != USER_DEFAULT (0x0002)"

        # ---- quit all three (focus each window's visible strip first) ----
        winfo = wins_parsed()
        app_wins = [w for w in winfo if w["x"] == 700 and w["y"] >= 200]
        app_wins.sort(key=lambda w: w["y"])
        for w in app_wins:
            # click the window's own un-shadowed strip above the next cascade
            click(w["x"] + w["w"] // 2, w["y"] + 17)
            time.sleep(2)  # winsrv focus settle (no drag!)
            type_keys("quit")
        time.sleep(3)
        data = ser_text()
        assert data.count("[user] [hello] quit - bye") == 3, \
            f"quit count {data.count('[user] [hello] quit - bye')}"

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
    execs = re.findall(r"\[vfs\] exec /disk/helloapp\.gle -> task (\d+)", data, re.I)
    checks = [
        ("3 containers exec'd as 3 DISTINCT tasks (any ids, slot reuse ok)",
         len(execs) == 3 and len(set(execs)) == 3),
        ("all 3 booted ring-3 ([hello] booted x3)",
         data.count("[user] [hello] booted from a .gle app") == 3),
        ("all 3 opened windows ([hello] window open x3)",
         data.count("[user] [hello] window open") == 3),
        ("every .gle task carried caps=0x0002 (USER_DEFAULT)",
         all(
             re.search(rf"\[sched\] task {t} stack [^ ]+ caps=(0x[0-9a-fA-F]+)", data)
             and int(re.search(rf"\[sched\] task {t} stack [^ ]+ caps=(0x[0-9a-fA-F]+)", data).group(1), 16) == 2
             for t in execs)),
        ("keyboard reached the focused app ([hello] got: hi)",
         "[user] [hello] got: hi" in data),
        ("refused: ramfs path (OP_EXEC gate)",
         "[vfs] exec refused /motd.txt: [gle] only /disk/* paths" in data),
        ("refused: CRC tamper caught",
         "exec refused /disk/badapp.gle: [gle] CRC mismatch" in data),
        ("refused: payload failed ELF validation",
         "exec refused /disk/badelf.gle: [elf] bad magic" in data),
        ("refused: usage (exec alone)",
         "exec: missing path" in data),
        ("Files listed /disk (dirs sorted first, 7 items)",
         "[files] show /disk items=7" in data),
        ("Files double-click ran the app ([files] run helloapp.gle)",
         "[files] run helloapp.gle" in data),
        ("clean exits: 3x quit + 3x task exited (0)",
         data.count("[user] [hello] quit - bye") == 3
         and len(re.findall(r"\[sched\] task \d+ exited \(0\)", data)) >= 3),
        ("app1 focused titlebar pixel-exact", True),  # asserted inline
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
