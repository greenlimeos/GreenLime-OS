#!/usr/bin/env python3
"""M20 TLS suite — real modern HTTPS, end to end.

Drives the release ISO headless (QMP keys + clicks), the same wiring the
M19 v2 suite used. The shell's terminal is opened by clicking the Taskbar
pin, then four fetch commands run:

  1. https       fetch https://example.com        -> TLS1.3 200 + body head + close_notify
  2. redirect    fetch http://google.com          -> 301 -> chase -> TLS hop 200
                                                   (http->https upgrade only)
                                                   (example.com:80 has moved between
                                                   301 and direct-200 fronts; google:80
                                                   always redirects)
  3. big chain   fetch https://www.google.com      -> TLS1.3 200 (real webpki chain)
  4. negative    fetch https://self-signed.badssl.com
                                                   -> refused by the verifier
                                                      (invalid peer certificate)

Four proves the validation is REAL: a self-signed chain must bounce with
rustls' actual verifier reason, quoted on the console — not a 200.

Usage: python3 scripts/qmp_m20_tls_test.py [iso]
"""
import json
import os
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmpM20.sock"
ISO = (sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime-release.iso")
SER = "dist/m20-serial.log"
DISK = "dist/disk.img"
# Boot a COPY: a `make run-release` the human runs mid-suite must not
# rewrite the image under the running VM.
QISO = "dist/m20-iso.iso"

WIN_W, WIN_H = 1280, 800
# Taskbar pin geometry (same math the M19 suite documented): the Terminal
# button spans 642..682 -> center 662; bar top = 800-48+4, CELL 40 ->
# center y 776.
TERM_PIN = (662, 776)

KEYNAME = {" ": ["spc"], ".": ["dot"], "/": ["slash"], ":": ["shift-semicolon"],
           "-": ["minus"], "\n": []}


def die(msg):
    raise SystemExit("m20-test: " + msg)


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


def wait_marker_after(path, marker, offset, timeout=420):
    """Same, but only counts bytes past `offset` — used when the same
    proof line must appear a SECOND time (redirect hops)."""
    waited = 0
    while waited < timeout:
        time.sleep(2)
        waited += 2
        try:
            data = open(path, "rb").read().decode("utf-8", "replace")
        except OSError:
            data = ""
        if marker in data[offset:]:
            return waited
    raise RuntimeError(f"marker {marker!r} never appeared past {offset}")


def main():
    for p in (QSOCK, SER, QISO):
        if os.path.exists(p):
            os.unlink(p)
    import shutil
    shutil.copyfile(ISO, QISO)
    print(f"iso: {ISO} (booting copy {QISO}), disk: {DISK}", flush=True)

    ser = open(SER, "wb")
    qemu = subprocess.Popen(
        [
            "qemu-system-x86_64",
            "-cpu", "qemu64,+smep,+smap",
            "-cdrom", QISO,
            "-m", "512M",
            "-vga", "std",
            "-machine", "q35",
            "-no-reboot",
            "-boot", "order=d",
            "-drive", f"file={DISK},if=virtio,format=raw",
            # NAT NIC: 10.0.2.x so the guest can reach the real web.
            "-device", "virtio-net-pci,netdev=n0",
            "-netdev", "user,id=n0",
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

        click(*TERM_PIN)
        wait_marker(SER, "[winsrv] terminal open")
        time.sleep(1)

        def snap():
            return len(open(SER, "rb").read())

        # -- 1: https example.com -------------------------------------
        # Console writes split multi-part lines (each out() is one
        # [dev:serial0]-prefixed write), so markers MUST be single-write
        # lines: proof lines, or kernel [net] lines.
        type_keys("fetch https://example.com")
        wait_marker(SER, "[net] resolve: example.com", 120)
        print("net up: DNS resolved example.com", flush=True)
        wait_marker(SER, "fetch: TLS1.3 HTTP/1.1 200 OK", 300)
        wait_marker(SER, "Example Domain", 120)
        wait_marker(SER, "close_notify", 60)
        print("PASS https example.com: TLS1.3 200, body streamed, close_notify", flush=True)

        # -- 2: http -> https redirect (upgrade chase) -----------------
        # NOTE: the chase finishes within milliseconds of the 301 proof,
        # so all hop-2 markers share `off` — re-snapping between events
        # loses to the 2 s poll granularity (live-found harness bug).
        # example.com's port-80 fronts have flip-flopped (301/200);
        # google.com:80 always redirects — pinned there.
        off = snap()
        type_keys("fetch http://google.com")
        wait_marker_after(SER, "fetch: HTTP/1.1 301", off, 240)
        wait_marker_after(SER, "[net] resolve: www.google.com", off, 120)
        wait_marker_after(SER, "fetch: HTTP/1.1 200 OK", off, 240)
        print("PASS http hop: 301 chased, hop-2 fetched", flush=True)

        # -- 3: a longer real chain: www.google.com --------------------
        # The proof line already carries the close kind; Google is a
        # tcp-fin closer (live-observed repeatedly), only example.com
        # sends close_notify (proven in case 1).
        off = snap()
        type_keys("fetch https://www.google.com")
        wait_marker(SER, "[net] resolve: www.google.com", 60)
        wait_marker_after(SER, "fetch: TLS1.3 HTTP/1.1 200 OK", off, 300)
        print("PASS https www.google.com: TLS1.3 200", flush=True)

        # -- 4: TLS-lane redirect: google.com -> www.google.com --------
        off = snap()
        type_keys("fetch https://google.com")
        wait_marker_after(SER, "fetch: TLS1.3 HTTP/1.1 301", off, 240)
        wait_marker_after(SER, "[net] resolve: www.google.com", off, 120)
        wait_marker_after(SER, "fetch: TLS1.3 HTTP/1.1 200 OK", off, 240)
        print("PASS tls redirect: 301 chased INSIDE the https lane", flush=True)

        # -- 5: negative — a self-signed chain MUST bounce --------------
        off = snap()
        type_keys("fetch https://self-signed.badssl.com")
        wait_marker_after(SER, "fetch: https refused:", off, 300)
        wait_marker_after(SER, "certificate", off, 30)
        print("PASS negative: self-signed chain refused by the real verifier", flush=True)

        print("M20-QMP: ALL GREEN", flush=True)
    finally:
        qemu.terminate()
        try:
            qemu.wait(5)
        except Exception:
            qemu.kill()


if __name__ == "__main__":
    main()
