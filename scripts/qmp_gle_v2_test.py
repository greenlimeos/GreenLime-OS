#!/usr/bin/env python3
"""M19 .gle v2 suite — the asset truck, end to end.

Drives a fresh 64 MB volume (own image; the human's dist/disk.img is
never touched) seeded with the v2 hello container, then:

  Terminal: exec /disk/hello2.gle   -> the app logs the asset window:
            kernel maps the descriptor page + 2 blobs read-only,
            hello prints "[hello] assets: 2 blob(s)" and the head of
            asset[0] — the container's own bytes, no installer.
  Negatives (hand-patched containers, CRC fixed so the SPECIFIC law
  fires): kind=3 -> "[gle2] reserved kind"; nonzero reserved dword ->
  "[gle2] reserved words must be zero"; nonzero gap padding ->
  "[gle2] padding must be zero"; CRC-broken copy -> v1-style CRC
  refusal. Each refusal is a serial line, never a panic.

Usage: python3 scripts/qmp_gle_v2_test.py [iso]
"""
import json
import os
import socket
import struct
import subprocess
import sys
import time
import zlib

QSOCK = "/tmp/gl-qmpG2.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime-release.iso"
DISK = "dist/gle-v2-disk.img"
SER = "dist/gle-v2-serial.log"
GLE = "dist/hello.gle"

WIN_W, WIN_H = 1280, 800
# Taskbar pins (0 clients): cluster x0 = (1280 - (3*44+40)) / 2 = 554;
# button 2 (Terminal) spans 642..682 -> center 662; bar top = 800-48+4,
# CELL 40 -> center y 776. (bar_cell order: 0 start, 1 Files, 2 Terminal
# = menu_launch(App::Terminal), 3 Settings.)
TERM_PIN = (662, 776)

KEYNAME = {" ": ["spc"], ".": ["dot"], "/": ["slash"], "2": ["2"], "0": ["0"],
           "-": ["minus"], "\n": []}


def die(msg):
    raise SystemExit("gle-v2-test: " + msg)


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


def outer_crc_bytes(b):
    """The container CRC law: over [0..16) ++ [20..EOF) — used by the
    tamper suite to re-sign a hand-patched container so the SPECIFIC
    law fires (not just the CRC gate)."""
    return zlib.crc32(bytes(b[:16]) + bytes(b[20:])) & 0xFFFFFFFF


def struct_sites(blob):
    """(meta_len, entry_count, table_off) of the v2 container."""
    (meta_len,) = struct.unpack_from("<I", blob, 8)
    (entry_count,) = struct.unpack_from("<I", blob, 12)
    return meta_len, entry_count, 20 + meta_len


def build_negatives():
    """Hand-patched copies of the v2 container, each re-signed so exactly
    ONE kernel law breaks. (The packer never emits these — build_seed the
    kernel-side refusals the selftest can't reach.)"""
    neg = {}

    base = bytearray(open(GLE, "rb").read())

    # kind=3 on the first entry (the ELF record's kind byte)
    b = bytearray(base)
    ml, ec, toff = struct_sites(b)
    b[toff + 12] = 3
    struct.pack_into("<I", b, 16, outer_crc_bytes(b))
    neg["badkind.gle"] = bytes(b)

    # nonzero reserved dword (first u64 slot of entry 1)
    b = bytearray(base)
    b[toff + 32 + 16] = 0xDB
    struct.pack_into("<I", b, 16, outer_crc_bytes(b))
    neg["badres.gle"] = bytes(b)

    # nonzero gap padding: the first byte after the entry table
    # (table_end .. first blob off) must be zero
    b = bytearray(base)
    first_off = struct.unpack_from("<I", bytearray(base), toff + 4)[0]
    b[first_off - 1] = 0x5A
    struct.pack_into("<I", b, 16, outer_crc_bytes(b))
    neg["badpad.gle"] = bytes(b)

    # CRC-broken v2 (plain byte flip, no re-sign)
    b = bytearray(base)
    b[-1] ^= 0xFF
    neg["badcrc.gle"] = bytes(b)
    return neg


def build_disk():
    subprocess.run(
        "dd if=/dev/zero of=" + DISK + " bs=1M count=64 status=none && "
        "mformat -i " + DISK + " -F -v GREENLIME :: && "
        "mcopy -i " + DISK + " " + GLE + " ::/hello2.gle",
        shell=True, check=True,
    )
    for name, data in build_negatives().items():
        with open("/tmp/" + name, "wb") as f:
            f.write(data)
        subprocess.run(
            f"mcopy -i {DISK} /tmp/{name} ::/{name}",
            shell=True, check=True,
        )


def main():
    if not os.path.exists(GLE):
        die(f"{GLE} missing — run `make hello-gle` first")
    head = open(GLE, "rb").read(6)
    if head[0:4] != b"GLE1" or struct.unpack_from("<H", head, 4)[0] != 2:
        die(f"{GLE} is not a v2 container (got {head!r})")

    for p in (DISK, QSOCK, SER):
        if os.path.exists(p):
            os.unlink(p)
    build_disk()
    print(f"fresh 64M disk: {GLE} as hello2.gle + 4 negatives", flush=True)

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

        def type_keys(cmd):
            for ch in cmd:
                ks = [ch.lower()] if ch.isalpha() or ch.isdigit() \
                    else KEYNAME.get(ch)
                if ks is None:
                    die(f"no sendkey plan for char {ch!r}")
                for k in ks:
                    hmp(f"sendkey {k}")
                    time.sleep(0.25)
            hmp("sendkey ret")
            time.sleep(4)

        def to_dev(px, dim):
            return px * 32767 // (dim - 1)

        def move(gx, gy):
            # M18's proven shape: the MOVE is its own event batch, pressed
            # separately — batching move+button into one call is not what
            # the compositor's input path sees as a click.
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

        # The desktop is clean at boot (terminal opens ON DEMAND, never
        # focused): click the Terminal pin so the window exists and owns
        # the keyboard, then type.
        click(*TERM_PIN)
        wait_marker(SER, "[winsrv] terminal open")
        time.sleep(1)
        type_keys("exec /disk/hello2.gle")
        wait_marker(SER, "[vfs] exec /disk/hello2.gle -> task ")
        wait_marker(SER, "asset window: 2 blob(s)")
        wait_marker(SER, "[user] [hello] assets: 2 blob(s) (v2 window)")
        # the container's proof.txt head: the first 16 asset bytes are
        # exactly "GreenLime OS GLE" — the app read the MAPPED bytes.
        wait_marker(SER, "asset[0]: 241 B, head: GreenLime OS GLE")
        print("PASS v2 exec: task spawned, window delivered, app read asset bytes", flush=True)

        # the negatives: every one refuses with its exact law line
        for name, law in (
            ("badkind.gle", "reserved kind"),
            ("badres.gle", "reserved words must be zero"),
            ("badpad.gle", "padding must be zero"),
            ("badcrc.gle", "CRC mismatch (corrupt or tampered)"),
        ):
            type_keys(f"exec /disk/{name}")
            wait_marker(SER, f"[vfs] exec refused /disk/{name}: [gle2] {law}")
            print(f"PASS refusal {name}: [gle2] {law}", flush=True)
        # badcrc keeps the v1-shaped refusal text (the container CRC law
        # reads the same slot in both formats).
        print("GLE-V2-QMP: ALL GREEN", flush=True)
    finally:
        qemu.terminate()
        try:
            qemu.wait(5)
        except Exception:
            qemu.kill()


if __name__ == "__main__":
    main()
