#!/usr/bin/env python3
"""v1.0 Phase D proof: ring-3 TCP + ICMP through the net broker.

Boots the interactive desktop, opens the Terminal, then types:
  1. "ping google.com"  -> resolve + ICMP echo via broker
  2. "fetch example.com"  -> TCP:80, HTTP GET with
     User-Agent: GreenLime/1.0, streamed body, stream end + total

Asserts on the serial mirror; saves a wire capture (fetch.pcap) so the
User-Agent can beShown to actually leave the NIC in a real TCP segment.
"""
import json
import os
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmp-netsd.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime-release.iso"
SER = "dist/fetch-serial.log"
PCAP = "dist/fetch.pcap"

PING_KEYS = ["p", "i", "n", "g", "spc", "g", "o", "o", "g", "l", "e",
             "dot", "c", "o", "m", "ret"]
FETCH_KEYS = ["f", "e", "t", "c", "h", "spc", "e", "x", "a", "m", "p", "l",
              "e", "dot", "c", "o", "m", "ret"]


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
        except FileNotFoundError:
            continue
        if marker in data:
            return waited
    raise RuntimeError(f"marker {marker!r} never appeared")


def main():
    for p in (QSOCK, SER, PCAP):
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
            "-object", f"filter-dump,id=fd0,netdev=n0,file={PCAP}",
            "-device", "virtio-tablet-pci",
            "-serial", "stdio",
            "-display", "none",
            "-qmp", f"unix:{QSOCK},server,nowait",
        ],
        stdout=ser,
        stderr=subprocess.DEVNULL,
    )
    try:
        print("waiting for shell...", flush=True)
        wait_marker(SER, "[shell] interactive ready")
        time.sleep(20)  # let winsrv drain + settle (wallpaper streams late)
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(60)
        s.connect(QSOCK)
        qmp(s, {"execute": "qmp_capabilities"})

        def hmp(cmd):
            return qmp(s, {"execute": "human-monitor-command",
                           "arguments": {"command-line": cmd}})

        def type_keys(keys):
            for k in keys:
                hmp(f"sendkey {k}")
                time.sleep(0.6)

        def to_dev(px, dim):
            return px * 32767 // (dim - 1)

        def click_at(gx, gy):
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "abs", "data": {"axis": "x", "value": to_dev(gx, 1280)}},
                {"type": "abs", "data": {"axis": "y", "value": to_dev(gy, 800)}},
            ]}})
            time.sleep(2)
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "btn", "data": {"button": "left", "down": True}}]}})
            time.sleep(1)
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "btn", "data": {"button": "left", "down": False}}]}})
            time.sleep(2)

        def shot(path):
            qmp(s, {"execute": "screendump", "arguments": {"filename": path}})

        # Open the terminal from the start card (same spots as the
        # type test).
        click_at(552, 776)   # start button → card
        click_at(573, 578)   # Terminal tile (tile 1)
        wait_marker(SER, "[winsrv] launch Terminal")

        # 1. ping google.com
        print("typing: ping google.com", flush=True)
        type_keys(PING_KEYS)
        wait_marker(SER, "ping: reply from", 120)
        print("ping OK", flush=True)

        # 2. fetch example.com over real TCP
        print("typing: fetch example.com", flush=True)
        type_keys(FETCH_KEYS)
        wait_marker(SER, "fetch: stream end,", 180)

        data = open(SER, "rb").read().decode("utf-8", "replace")
        ok_status = "fetch: HTTP/1.1" in data
        ok_end = "fetch: stream end," in data
        ok_ping = "ping: reply from" in data
        print(f"ping-line={ok_ping} status={ok_status} streamend={ok_end}")
        # show the interesting lines for the report
        for line in data.splitlines():
            if any(m in line for m in ("ping:", "fetch:", "[net] connect",
                                       "[net] open: slot",
                                       "resolving")):
                print("  |", line.strip())
        shot("dist/fb-fetch.ppm")
        try:
            qmp(s, {"execute": "quit"})
        except Exception:
            pass
        s.close()
        if not (ok_status and ok_end and ok_ping):
            raise RuntimeError("net shell proof failed")
        print("RESULT: PASS"
              if ok_status and ok_end and ok_ping else "RESULT: FAIL")
    finally:
        try:
            qemu.terminate()
            qemu.wait(5)
        except Exception:
            qemu.kill()


if __name__ == "__main__":
    main()
