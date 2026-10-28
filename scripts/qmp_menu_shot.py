#!/usr/bin/env python3
"""One-shot start-menu screenshot: boot an ISO headless, toggle the start
card with the Windows key, screendump, quit.

Usage: python3 scripts/qmp_menu_shot.py [iso] [out.ppm] [wait_s]
Defaults: the release ISO, dist/menu-shot.ppm, 12 s boot wait. Unlike the
suites this grabs a DETERMINISTIC frame of one known-good ISO — handy for
eyeballing icon work without racing any rebuild.
"""
import json
import os
import socket
import subprocess
import sys
import time

SOCK = "/tmp/gl-qmp.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime-release.iso"
PPM = sys.argv[2] if len(sys.argv) > 2 else "dist/menu-shot.ppm"
WAIT = int(sys.argv[3]) if len(sys.argv) > 3 else 12


def qmp_exchange(sock, cmd):
    """Send one QMP command, return the first response containing 'return'."""
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
                raise RuntimeError(f"QMP error: {obj}")


def main():
    assert os.path.exists(ISO), f"{ISO} missing"
    try:
        os.unlink(SOCK)
    except FileNotFoundError:
        pass
    qemu = subprocess.Popen(
        [
            "qemu-system-x86_64",
            "-cpu", "qemu64,+smep,+smap",
            "-cdrom", ISO,
            "-m", "512M",
            "-vga", "std",
            "-machine", "q35",
            "-no-reboot",
            "-serial", "stdio",
            "-display", "none",
            "-qmp", f"unix:{SOCK},server,nowait",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        time.sleep(WAIT)  # Limine menu + kernel boot with margin
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(10)
        s.connect(SOCK)
        qmp_exchange(s, {"execute": "qmp_capabilities"})
        # Windows key taps the start card open (E0 0x5B path in winsrv).
        qmp_exchange(
            s,
            {
                "execute": "send-key",
                "arguments": {"keys": [{"type": "qcode", "data": "meta_l"}]},
            },
        )
        time.sleep(1.5)  # card open + 120 ms fade+rise, plus margin
        qmp_exchange(
            s, {"execute": "screendump", "arguments": {"filename": PPM}}
        )
        try:
            qmp_exchange(s, {"execute": "quit"})
        except Exception:
            pass
        s.close()
    finally:
        time.sleep(1)
        qemu.terminate()
        qemu.wait(timeout=10)
    assert os.path.exists(PPM) and os.path.getsize(PPM) > 1_000_000, \
        f"screendump missing/small: {PPM}"
    print(f"wrote {PPM} from {ISO}")


if __name__ == "__main__":
    main()
