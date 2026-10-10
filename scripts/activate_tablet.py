#!/usr/bin/env python3
"""Make the virtio-tablet QEMU's console pointer (seamless host->guest cursor).

QEMU 10.2 registers its input handlers in machine-init order, so the PS/2
mouse always lands at the head of the handler list and wins
`qemu_input_is_absolute()`. -display gtk then behaves in RELATIVE mode:
first click grabs the pointer (host cursor blanked, title says "Press
Ctrl+Alt+G"), and only relative deltas go to the PS/2 mouse -- which our
guest discards because the tablet is present. Net effect: the host cursor
disappears and the guest cursor never moves.

Forcing the tablet to the head of the list makes the console pointer
absolute, so GTK stops grabbing and every motion is an absolute position
the guest maps 1:1 onto its own cursor.

Usage: activate_tablet.py <monitor-socket>   (HMP monitor, `server,nowait`)

Talks raw HMP: QEMU echoes every input char with backspace escapes, so we
send the command, then wait for the "(qemu)" prompt before reading output.

Daemonizes (double-fork) on startup so run-qemu.sh can background it
without owning its lifetime. Diagnostics go to
/tmp/greenlime-activate-tablet.log.
"""
import os
import re
import socket
import sys
import time

# HMP prompt. QEMU echoes keystrokes as "info mice[K[D..." garbage, so we
# only trust text captured *after* the command line has been echoed back.
PROMPT = "(qemu)"


def detach():
    """Double-fork so the caller never owns us.

    run-qemu.sh backgrounds this from a $( ) subshell; without this the
    subshell's teardown can kill us mid-flight and race QEMU's bind() on the
    monitor socket. Detaching makes us independent of the caller's lifetime.
    """
    if os.fork() > 0:
        os._exit(0)
    os.setsid()
    if os.fork() > 0:
        os._exit(0)
    # Fully decoupled: no inherited fds, no controlling terminal.
    devnull = os.open(os.devnull, os.O_RDWR)
    for fd in (0, 1, 2):
        os.dup2(devnull, fd)
    if devnull > 2:
        os.close(devnull)


def read_until(sock, marker, timeout=5.0):
    """Read from sock until marker appears; return text (marker excluded)."""
    buf = b""
    end = time.time() + timeout
    while time.time() < end:
        if marker.encode() in buf:
            return buf.decode(errors="replace")
        try:
            chunk = sock.recv(65536)
        except socket.timeout:
            continue
        if not chunk:
            break
        buf += chunk
    return buf.decode(errors="replace")


def main():
    # Detach BEFORE touching the socket so the caller's subshell teardown can
    # never race QEMU's bind() or kill us mid-handshake.
    detach()

    path = sys.argv[1]
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)

    # server,nowait: the socket appears once QEMU binds it during init.
    # QEMU self-cleans a stale socket file, so we never rm it ourselves.
    end = time.time() + 15.0
    while True:
        try:
            sock.connect(path)
            break
        except OSError:
            if time.time() > end:
                # Detached: no inherited stdout, so write to the serial log's
                # directory rather than print to a closed fd.
                with open("/tmp/greenlime-activate-tablet.log", "a") as fh:
                    fh.write(f"no monitor at {path} (giving up)\n")
                return 1
            time.sleep(0.2)

    read_until(sock, PROMPT)

    def hmp(cmd):
        sock.sendall((cmd + "\n").encode())
        # The echoed command + response, terminated by a fresh prompt.
        return read_until(sock, PROMPT)

    mice = hmp("info mice")
    # HMP (verified on QEMU 10.2.1 and 11.1.2) prints one line per pointer, e.g.:
    #     Mouse #3: QEMU Virtio Tablet (absolute)
    #   * Mouse #2: QEMU PS/2 Mouse
    # "*" marks current; only the tablet's line carries "(absolute)". So the
    # tablet is the first line whose index is followed by "absolute".
    m = re.search(r"Mouse #(\d+):.*?absolute", mice, re.IGNORECASE)
    if not m:
        with open("/tmp/greenlime-activate-tablet.log", "a") as fh:
            fh.write("no absolute pointer device found; "
                     "leaving console in relative (grab) mode\n")
        sock.close()
        return 1

    idx = m.group(1)
    hmp(f"mouse_set {idx}")
    after = hmp("info mice")
    # Deliberately NOT sending "quit": that HMP command shuts the whole VM
    # down. We only stop being a monitor client.
    sock.close()

    cur = re.search(r"\*\s*Mouse #(\d+)", after)
    with open("/tmp/greenlime-activate-tablet.log", "a") as fh:
        if cur and cur.group(1) == idx:
            fh.write(f"console pointer = tablet #{idx} (absolute, no grab)\n")
            return 0
        fh.write(f"mouse_set {idx} did not take:\n{after}\n")
    return 1


if __name__ == "__main__":
    sys.exit(main())
