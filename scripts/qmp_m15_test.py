#!/usr/bin/env python3
"""GreenLime M15 app-graphics proof: the demo client's canvas animation.

Usage: python3 scripts/qmp_m15_test.py [iso]

The demo switches its own window (id 2, at 700,120 540x330 — content
viewport (708,162)-(1232,444)) into graphics mode: WIN_BLIT_SETUP
allocates a 512x256 canvas, 40 frames of WIN_BLIT rows stream ring-3 ->
winsrv (navy gradient, azure ridge wave, lime ball in free flight), then
WIN_BLIT_END hands the window back to text mode.

Checks (serial unless marked "screen"):
  1. canvas allocated:      [winsrv] canvas: win 2 512x256 (owner 5)
  2. animation ran:         [demo] m15: frame 39 ball X,Y
  3. teardown synced:       [winsrv] canvas: win 2 released (N msgs)
  4. screen 1: lime ball >= 250 px + azure wave >= 600 px in the
     content box; zero all-channels >=239 px (the cursor predicate)
  5. screen 2, later frame: same colors again AND the pixels actually
     moved (>= 500 px differ inside the content box)
  6. motion truth from serial: ball coords moved >= 5 px between the
     two screen samples
  7. after release the window is a console again: click focuses it
     (focus win 2) and typed keys land ([demo] got: ok)
  8. X closes the window (win 2 closed) + demo sees EOF (window
     closed, bye) + the old titlebar is gone (screen)
  9. M15.1 start-card respawn: clicking the Demo tile after the demo
      task exited boots a fresh demo user task on the REAPED slot (the
      id orbits back to 5 - freelist LIFO): launch line + "spawned
      user task 5 (demo)" + a new canvas owned by 5 + a second 40-
      frame animation; the new window closes via its own X and the
      demo exits again (slot eligible for the next click).
"""
import json
import os
import re
import socket
import subprocess
import sys
import time

QSOCK = "/tmp/gl-qmp15.sock"
HSOCK = "/tmp/gl-hmp15.sock"
ISO = sys.argv[1] if len(sys.argv) > 1 else "dist/GreenLime.iso"
SER = "dist/m15-serial.log"

# Demo window (700,120,540,330): content viewport drawn below the 34px
# titlebar + 8px insets. Canary colors live INSIDE the canvas.
CONTENT = (708, 162, 1232, 444)
LIME = (170, 232, 64)
AZURE = (56, 126, 236)
TITLE = (43, 43, 43)


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


def hmp(cmd):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(10)
    s.connect(HSOCK)
    banner = b""
    while b"(qemu)" not in banner:
        banner += s.recv(4096)
    s.sendall(cmd.encode() + b"\n")
    time.sleep(0.5)
    try:
        out = s.recv(65536)
    except socket.timeout:
        out = b"(timeout)"
    s.close()
    return out


def read_serial():
    # WSL drvfs can throw transient ENODATA on files QEMU is writing.
    for _ in range(6):
        try:
            return open(SER, "rb").read().decode("utf-8", "replace")
        except OSError:
            time.sleep(0.3)
    return ""


def wait_for(marker, timeout=90):
    end = time.time() + timeout
    while time.time() < end:
        if marker in read_serial():
            return True
        time.sleep(0.4)
    return False


def serial_tail(base):
    """Serial bytes after byte offset `base`, decoded.

    Phase 9's marker window: the respawn run shares EVERY interesting
    marker with the boot run ("spawned user task 5 (demo)", "(owner 5)",
    "win 2 closed", 40 frames) — full-history matching would credit boot's
    lines to the respawn and fake passes. Everything phase 9 judges must
    arrive AFTER `base`."""
    for _ in range(6):
        try:
            return open(SER, "rb").read()[base:].decode("utf-8", "replace")
        except OSError:
            time.sleep(0.3)
    return ""


def wait_for_after(marker, base, timeout=90):
    end = time.time() + timeout
    while time.time() < end:
        if marker in serial_tail(base):
            return True
        time.sleep(0.4)
    return False


def last_frame():
    """(frame_idx, bx, by) of the newest [demo] m15: frame line, or None."""
    m = None
    for m in re.finditer(r"\[demo\] m15: frame (\d+) ball (-?\d+),(-?\d+)",
                         read_serial()):
        pass
    if not m:
        return None
    return int(m.group(1)), int(m.group(2)), int(m.group(3))


def released():
    return "[winsrv] canvas: win 2 released" in read_serial()


def read_ppm(path):
    for _ in range(6):
        try:
            with open(path, "rb") as f:
                assert f.readline().strip() == b"P6"
                dims = []
                while len(dims) < 3:
                    line = f.readline()
                    if line.startswith(b"#"):
                        continue
                    dims += line.split()
                w, h = int(dims[0]), int(dims[1])
                return w, h, f.read(w * h * 3)
        except OSError:
            time.sleep(0.5)
    raise RuntimeError(f"cannot read {path}")


def scan(path, box):
    """Per-pixel results inside box = (x0,y0,x1,y1), x1/y1 excluded."""
    w, h, px = read_ppm(path)
    x0, y0, x1, y1 = box
    lime = azure = blob = 0
    for y in range(y0, y1):
        row = y * w * 3
        for x in range(x0, x1):
            o = row + x * 3
            r, g, b = px[o], px[o + 1], px[o + 2]
            if (r, g, b) == LIME:
                lime += 1
            elif (r, g, b) == AZURE:
                azure += 1
            if r >= 239 and g >= 239 and b >= 239:
                blob += 1
    return lime, azure, blob


def diff_px(path_a, path_b, box):
    w, h, a = read_ppm(path_a)
    _, _, b = read_ppm(path_b)
    x0, y0, x1, y1 = box
    n = 0
    for y in range(y0, y1):
        row = y * w * 3
        for x in range(x0, x1):
            o = row + x * 3
            if (a[o], a[o + 1], a[o + 2]) != (b[o], b[o + 1], b[o + 2]):
                n += 1
    return n


def main():
    for p in (QSOCK, HSOCK, SER):
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
            "-device", "virtio-tablet-pci",
            "-serial", "stdio",
            "-display", "none",
            "-qmp", f"unix:{QSOCK},server,nowait",
            "-monitor", f"unix:{HSOCK},server,nowait",
        ],
        stdout=ser,
        stderr=subprocess.DEVNULL,
    )
    checks = []

    def check(name, passed):
        checks.append((name, bool(passed)))
        print(f"{'OK  ' if passed else 'FAIL'}: {name}", flush=True)

    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(30)
        end = time.time() + 60
        while True:
            try:
                s.connect(QSOCK)
                break
            except (FileNotFoundError, ConnectionRefusedError):
                if time.time() > end:
                    raise RuntimeError("QMP socket never appeared")
                time.sleep(0.1)
        qmp(s, {"execute": "qmp_capabilities"})

        def shot(path):
            qmp(s, {"execute": "screendump", "arguments": {"filename": path}})
            time.sleep(1.2)  # screendump is sync; give drvfs the time

        # Tablet ranges (needed for absolute clicks).
        m = re.search(
            r"virtio-tablet: evt queue=(\d+) bufs=(\d+) "
            r"x=(-?\d+)\.\.(-?\d+) y=(-?\d+)\.\.(-?\d+)",
            read_serial() if wait_for("virtio-tablet: evt queue", 420) else "",
        )
        if not m:
            raise RuntimeError("virtio-tablet never came online")
        x0, x1, y0, y1 = (int(m.group(i)) for i in range(3, 7))
        w, h = 1280, 800

        def abs_xy(gx, gy):
            vx = x0 + gx * 32767 // (w - 1) * (x1 - x0) // 32767
            vy = y0 + gy * 32767 // (h - 1) * (y1 - y0) // 32767
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "abs", "data": {"axis": "x", "value": vx}},
                {"type": "abs", "data": {"axis": "y", "value": vy}},
            ]}})

        def btn(down):
            qmp(s, {"execute": "input-send-event", "arguments": {"events": [
                {"type": "btn", "data": {"button": "left", "down": down}}]}})
            time.sleep(0.6)

        def click_at(gx, gy):
            abs_xy(gx, gy)
            time.sleep(1.0)
            btn(True)
            btn(False)
            time.sleep(1.0)

        def type_keys(keys, gap=0.7):
            for k in keys:
                hmp(f"sendkey {k if k != ' ' else 'spc'}")
                time.sleep(gap)

        # --- 1: canvas allocation on serial -------------------------------
        check("canvas allocated ([winsrv] canvas: win 2 512x256)",
              wait_for("[winsrv] canvas: win 2 512x256", timeout=120))

        # The animation is Park-paced (~10 fps): catch it mid-flight for
        # the two screen samples. Samples are anchored to frame markers
        # so the pair is separated by several real frames. Only AFTER the
        # pixels are sampled do we wait for the end-of-life markers.
        def catch_frame(target, allow_release=False, timeout=90):
            """Newest frame marker >= target, or None if released first."""
            end = time.time() + timeout
            while time.time() < end:
                nf = last_frame()
                if nf and nf[0] >= target:
                    return nf
                if allow_release and released():
                    return last_frame()
                time.sleep(0.15)
            return last_frame()

        fr1 = catch_frame(4)
        shot("dist/fb-m15-live1.ppm")
        c1 = fr1 or (-1, 0, 0)
        fr2 = catch_frame(c1[0] + 6, allow_release=True)
        shot("dist/fb-m15-live2.ppm")
        c2 = fr2 or (c1[0] + 6, c1[1] + 35, c1[2])  # sentinel if never seen

        # --- 2/3: the full lifecycle completed on serial ------------------
        check("animation ran to frame 39",
              wait_for("[demo] m15: frame 39 ball", timeout=120))
        check("canvas released (sync teardown)",
              wait_for("[winsrv] canvas: win 2 released", timeout=60))

        # --- 4/5: canvas pixels on screen (both samples) ------------------
        l1, a1, b1 = scan("dist/fb-m15-live1.ppm", CONTENT)
        l2, a2, b2 = scan("dist/fb-m15-live2.ppm", CONTENT)
        print(f"screen: shot1 lime={l1} azure={a1} blob={b1}", flush=True)
        print(f"screen: shot2 lime={l2} azure={a2} blob={b2}", flush=True)
        check("screen: lime ball on the canvas", l1 >= 250)
        check("screen: azure wave on the canvas", a1 >= 600)
        check("screen: shot2 lime ball again", l2 >= 250)
        check("screen: shot2 azure wave again", a2 >= 600)
        check("cursor predicate holds (no >=239 blob in the canvas)",
              b1 == 0 and b2 == 0)
        d = diff_px("dist/fb-m15-live1.ppm", "dist/fb-m15-live2.ppm", CONTENT)
        print(f"screen: moved/differing px between frames = {d}", flush=True)
        check("graphics really animate (>= 500 px differ)", d >= 500)

        # --- 6: motion truth from the demo's own serial -------------------
        moved = ((c2[1] - c1[1]) ** 2 + (c2[2] - c1[2]) ** 2) ** 0.5
        print(f"ball: {c1} -> {c2} (moved {moved:.1f} px)", flush=True)
        check("ball moved between samples (>= 5 px)", moved >= 5.0)

        # --- 7: text mode returns after the release -----------------------
        click_at(970, 300)  # demo window content
        check("click focuses demo (focus win 2)",
              wait_for("[winsrv] focus win 2", timeout=30))
        type_keys(["o", "k", "ret"])
        check("console mode again ([demo] got: ok)",
              wait_for("[demo] got: ok", timeout=45))

        # --- 8: X closes the window; demo sees the EOF --------------------
        # close button = (700+540-26, 124, 20, 22) -> center (1224,135)
        click_at(1224, 135)
        check("X closed the window (win 2 closed)",
              wait_for("[winsrv] win 2 closed", timeout=30))
        check("demo saw the EOF (window closed, bye)",
              wait_for("[demo] window closed, bye", timeout=30))
        shot("dist/fb-m15-closed.ppm")
        t_gone = 0
        wpx, hpx, px = read_ppm("dist/fb-m15-closed.ppm")
        for y in range(126, 146):
            for x in range(710, 1230):
                o = (y * wpx + x) * 3
                if (px[o], px[o + 1], px[o + 2]) == TITLE:
                    t_gone += 1
        print(f"screen: old titlebar TITLE px after close = {t_gone}",
              flush=True)
        check("titlebar + canvas gone after close", t_gone < 50)

        # --- 9 (M15.1): the start-card Demo tile is a real launcher --------
        # The old demo task exited above; its slot was reaped back to the
        # free list. Clicking the Demo tile must boot a NEW demo user task,
        # which reuses task id 5 (freelist LIFO) — so the fresh proofs read
        # exactly like boot: "spawned user task 5 (demo)" + a canvas whose
        # owner is 5. Then the new window closes via its own X button.
        #
        # Marker discipline: `base` snapshots the serial BEFORE any phase-9
        # interaction; every check below judges only its own window
        # (serial_tail / wait_for_after). Full-history matching would give
        # the BOOT run's lines credit for the respawn — first 17/21 runs
        # died exactly there.
        base = len(open(SER, "rb").read())
        # TASKBAR geometry mirrors winsrv's bar_cluster_w: the centered
        # cluster is (n-1)*44 + 40 px wide with 40 px cells. The demo window
        # is CLOSED here -> zero client windows -> n = 4 (start + 3 pins),
        # so the cluster re-centers and the start button MOVES: x0 = 554,
        # center (574,776). The boot-time (552,776) is only right while a
        # client chip widens the cluster — clicking it lands in dead bar
        # space (closes the card, prints nothing), and the tile click then
        # hits the wallpaper: total silence, as measured. Interactive
        # clicks always hit the real button, so this never showed on
        # glass.
        n = 4
        x0 = (1280 - ((n - 1) * 44 + 40)) // 2  # 554
        start_center = (x0 + 20, 800 - 48 + 4 + 20)  # (574, 776)
        click_at(*start_center)  # start button: open the card
        click_at(841, 578)  # Demo tile (tile 3 = (778,528,126,100))
        check("Demo tile respawned the client (launch + task 5)",
              wait_for_after("[winsrv] start card open", base, timeout=15)
              and wait_for_after("[winsrv] launch Demo", base, timeout=30)
              and wait_for_after("spawned user task 5 (demo)", base, timeout=60))
        check("fresh canvas live again (owner 5)",
              wait_for_after("[demo] m15: canvas open", base, timeout=120)
              and wait_for_after("(owner 5)", base, timeout=30))
        # Full 40-frame run: wait for its release line, then count ONLY
        # fresh frame lines (boot's 40 are outside the window).
        wait_for_after("released (", base, timeout=90)
        tail = serial_tail(base)
        nframes = tail.count("[demo] m15: frame")
        print(f"respawn: frames drawn on serial = {nframes}", flush=True)
        check("second run drew its own 40 frames", nframes >= 40)
        # The new window (ids keep growing: boot's demo was 2) closes via
        # its own X button — geometry identical, position from the log.
        m = None
        for m in re.finditer(r"\[winsrv\] win (\d+) open at (\d+),(\d+)", tail):
            pass
        if m is None:
            check("new demo window opened", False)
        else:
            wid = m.group(1)
            wx, wy = int(m.group(2)), int(m.group(3))
            click_at(wx + 524, wy + 15)  # same X-button offset as before
            check(
                f"respawned demo closed via X (win {wid} closed)",
                wait_for_after(f"win {wid} closed", base, timeout=30),
            )
            check("demo saw the second EOF (second window closed, bye)",
                  (tail2 := serial_tail(base)).count("window closed, bye") >= 1)
            check("task slot reaped again (second task 5 exited)",
                  wait_for_after("task 5 exited", base, timeout=30))

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

    ok = True
    for _, passed in checks:
        ok = ok and passed
    if not ok:
        print("\n--- serial tail ---")
        print("\n".join(read_serial().splitlines()[-40:]))
    print(f"\nRESULT: {'PASS' if ok else 'FAIL'} ({sum(p for _, p in checks)}"
          f"/{len(checks)})")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
