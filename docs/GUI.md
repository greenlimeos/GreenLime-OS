# GreenLime GUI (v0.9): window server + desktop + terminal + taskbar.

## Theme (v0.9 rework: no green, GreenLime's own chrome)

Dark neutral + blue accent everywhere (`winsrv.rs` palette consts):

| role | color | role | color |
|---|---|---|---|
| accent / focus underline | `(76,194,255)` | focused titlebar | `(38,44,58)` |
| window body / text | `(16,18,24)` / `(214,222,235)` | unfocused titlebar | `(26,30,40)` |
| taskbar | `(23,26,34)` | Start panel | `(24,27,35)` |
| rows / search / popup body | `(34,38,49)` | hover | `(46,52,66)` |

* **Wallpaper**: deep-blue vertical gradient + 4 soft radial glows, rendered
  **once** into a cached `Vec<u32>` (`render_wallpaper`, Q16 fixed-point) and
  row-copied into the damage during compose. See the soft-float gotcha below
  before ever making this per-pixel float.
* **Taskbar**: 48px, *left-aligned* slots [G start mark,
  one icon per window in id order], accent underline under the focused app,
  hover fill, right-aligned two-line uptime tray (no key display).
* **Start menu**: 400x540 launcher panel docked bottom-left above the
  taskbar — search box (keys go to the search filter whenever the menu is
  open; Enter launches the first match), vertical app rows with icon +
  name + chevron (Terminal/Demo/Settings/About), account footer. Only
  clicks outside the panel (or empty taskbar / Esc) dismiss it.
  Deliberately not a centered grid.
* **Terminal**: the shell console opens on demand from the launcher —
  never at boot. Early shell output is buffered and shown when it opens;
  the serial mirror stays live regardless. The terminal has an X button
  like every window (its IPC handle stays protected); closing it just
  hides it — reopening restores the buffered text.
* **Popups**: `Popup::{About, Settings}` — centered dialog with an X
  mark, any click dismisses; Settings shows display size / uptime /
  window count / build.
* Splash + console palette match: title `(243,245,248)`, console
  `(214,222,235)` on `(10,14,28)` — no green anywhere.

## What runs where (v0.9 demo)

```
shell ──write──▶ DEV "term0" ──WIN_WRITE──▶ winsrv (kernel task)
shell ──read───▶ DEV "term0" ──WIN_READ───▶ winsrv (blocks until Enter)
demo  ──rpc────▶ PORT_WINSRV ──WIN_CREATE──▶ new client window (id ≥ 1)
demo  ──write──▶ PORT_WINSRV ──WIN_CLIENT_WRITE──▶ that window's text
demo  ──read───▶ PORT_WINSRV ──WIN_CLIENT_READ───▶ keys (while focused)
                                             │ compose(damage) into back
                                             │ buffer, blit damage, cursor
                                             ▼
                                        framebuffer (takeover from kprint)
PS/2 kbd ──IRQ1──▶ scancode ring ──poll──▶ focus routing ──▶ line discipline
                                                     (terminal) or raw ring
                                                     (client window)
PS/2 mouse ─IRQ12─▶ byte ring ──poll──▶ pointer() — click/drag (no tablet)
virtio-tablet ─DMA─▶ eventq (64 bufs) ──poll──▶ pointer() — abs = host pointer
```

Boot starts under a splash: `fb::splash("GreenLime OS", …)` centers the
title, then sets `quiet` so kprint mirrors serial only (every diagnostic
still lands on the serial log). winsrv's first paint covers the splash;
the panic path calls `fb::wake_console()` so a panic is never hidden.

The window server lives in-kernel for v0.9, exactly like vfs/dev did in
v0.6: same IPC protocol (`PORT_WINSRV`), so it migrates to userspace later
without changing clients.

## The compositor (damage rects + back buffer)

* **Back buffer**: a same-size XRGB buffer on the heap (4 MiB at
  1280x800); *all* drawing targets it. `present()` blits only the damage.
* **Damage**: every state change (`invalidate(rect)`) unions into one
  damage rect; each lap composes **only the layers intersecting it**,
  clipped fill-by-fill, then blits. Keystroke = one line-height band;
  window move = old ∪ new rect; clock tick = taskbar band.
* **Layers, bottom-to-top**: wallpaper desktop (+watermark) → windows in
  z-order → taskbar → Start menu → popup. Writes may spill outside the
  damage rect (harmless: only the rect is blitted, and any later compose
  recomputes from state, so the back buffer can never propagate an error).
* **The on-screen cursor is never in the back buffer.** Each lap: undraw
  (restore save-under) → handle input (state only) → compose+blit →
  draw cursor on top. Compositing can no longer stomp the cursor (the
  pre-compositor code needed three hand-ordered undraw calls per lap).
* **Idle**: no messages, no input, no damage, clock fresh → zero fb
  traffic, zero allocator churn (same fast path as before).

## Windows, focus, LimeDE chrome

* **Window = rect + title + text lines.** Content renders statelessly:
  lines wrap at the content width, the tail that fits is shown, scroll
  beyond it. Resize reflows for free. Scrollback 200 lines.
* **Terminal (id 0)** keeps the line discipline (canonical stdin). Keys
  are routed by keyboard focus: terminal → line buffer/echo/ring; client
  window → its raw ring (WIN_CLIENT_READ).
* **Focus follows clicks** (window body, titlebar, or its taskbar
  icon); focused titlebar is `(38,44,58)` with a 2px accent underline,
  unfocused `(26,30,40)`; the taskbar icon shows the same accent
  underline. New windows do **not** steal focus (a boot-time client must
  not hijack the console). Raise = move to top of z; taskbar icons stay
  in creation-id order so they never shuffle.
* **Drag**: press the titlebar and move — the window follows the cursor
  live (damage = old ∪ new). **Resize**: the bottom-right 16px grip,
  min 200x120. Release logs `moved to x,y` / `resized WxH` to serial.
* **Close**: the titlebar X (every window, red on hover) destroys
  the window, EOFs any pending WIN_CLIENT_READ (client sees
  `REPLY_ERR`), and drops its taskbar icon. `WIN_DESTROY` is the
  message-side equivalent.
* **Taskbar**: left-aligned [Start | one icon per window, id
  order] + right-aligned uptime tray (HH:MM over "Day N"). Clicking
  Start toggles-opens the menu; clicking an icon focuses that
  window (closing the menu first); empty taskbar space closes the menu.

## Interactive stdin (the v0.9 line discipline)

* **Decode**: set-1 scancodes → ASCII in `winsrv::scancode_ascii`
  (US layout, shift + caps, E0-prefixed keys swallowed, break codes
  only release modifiers).
* **Canonical mode** (terminal only): typed chars land in `line_buf` and
  echo immediately (append to the open line; backspace pops a char).
  Enter commits `line_buf + '\n'` into the read ring and wakes a waiting
  reader. Client windows get the same decode, raw (Enter = `'\n'`).
* **Blocking read**: dev's `OP_READ` on a `term0` handle relays
  `WIN_READ` to winsrv and blocks on its own mailbox; winsrv holds the
  request until Enter supplies data, then replies with the bytes. The
  shell just does a blocking read on its console fd — no polling.
* **Console rules for writes**: `WIN_WRITE`/`WIN_CLIENT_WRITE` continue
  the current line; only `\n` starts a new one (otherwise `"$ "` +
  `"help"` would render on two rows).
* **Serial mirror**: interactive shell duplicates all output to `serial0`,
  and the demo client logs `[demo] got: …` lines, so tests assert on
  what actually ran, not on pixel guesses.

## Deliberate simplifications (v0.9 demo scope)

* **Pointer: virtio-tablet when present, PS/2 fallback.** The event model
  (polled by the server) is transport-agnostic; with a tablet attached the
  server drains PS/2 mouse bytes (QEMU feeds both) and tracks the absolute
  device instead: 0..32767 → exact pixel, so the guest cursor lands where
  the real one is. Relative PS/2 can't do that — no shared coordinate
  system — which is why the cursor used to drift from the host pointer.
* **QEMU must be told to use the tablet as its console pointer.** QEMU
  registers its PS/2 mouse before the tablet, so PS/2 wins QEMU's internal
  "current pointer" pick and `-display gtk` runs in *relative* mode: first
  click grabs the pointer (host cursor vanishes, title shows "Press
  Ctrl+Alt+G to release grab") and QEMU feeds only relative deltas to the
  PS/2 mouse — which the server drains and discards. Net effect: host
  cursor disappears and the guest cursor never moves. `run-qemu.sh` opens
  an HMP monitor socket and runs `scripts/activate_tablet.py`, which issues
  `mouse_set` to promote the tablet to *current*, making the pointer
  absolute (no grab, 1:1 tracking). Failure is non-fatal: without it the
  guest still works, just with the old grab behaviour. The guest cannot
  fix this itself — QEMU's pointer pick happens before any guest code runs.
* **Absolute mode hides the host cursor — by QEMU's design.** QEMU's
  `ui/gtk.c gd_update_cursor()` sets a blank cursor whenever
  `qemu_input_is_absolute()` is true, so once the tablet is current the
  Windows cursor vanishes on hover (nothing is grabbed; it is just
  hidden to avoid double cursors). The guest still draws its own cursor
  in the framebuffer, tracking 1:1. `run-qemu.sh` passes
  `-display gtk,show-cursor=on`, which overrides the blank cursor so the
  host cursor stays visible alongside the guest one.
* **Damage = one union rect**, not a list (a window move composes a
  bounding box). Fine at this scale; a rect list is performance work.
* **Text windows only.** Pixels/cells client APIs, minimize, and z-click
  cycling are future work; the compositor already renders arbitrary rects.
* **Subset font.** Same console subset as v0.5 (all ASCII + friends); the
  lazy full-font loader is filed under performance work.

## Proved by test (not by reading)

* `scripts/qmp_screendump.py [secs]` — screenshot the display any time.
* `scripts/qmp_input_test.py` — HMP `sendkey a` shows `scancode 0x1e` on
  serial, and a PS/2 relative +200px move shifts the cursor 640→840
  (bbox threshold ≥239: cursor is 240,240,240; every UI text color
  keeps one channel below 239 so titles/menus must not count).
* `scripts/qmp_mouse_idle.py` — waits for serial-idle, screenshots the
  cursor, injects +300 x, screenshots again; asserts pixels moved.
* `scripts/probe_mice.py` — QMP `query-mice` over run-qemu.sh's exact argv.
  Without the helper: `PS/2 Mouse current=True, absolute=False` (relative,
  grab mode). With `activate_tablet.py`: `Virtio Tablet current=True,
  absolute=True`. This is the cursor-grab bug, caught in one line.
* `scripts/verify_tablet_pointer.py` — runs the helper against a live
  headless QEMU, then independently QMP-queries `query-mice` to assert the
  tablet ended up `current=True, absolute=True`. Exits non-zero otherwise.
* `scripts/qmp_type_test.py` — launches the terminal from the menu
  (Start + Terminal row via absolute clicks), then types
  `echo greenlime works` and an editing session (`abd` + backspace + `c`)
  via HMP `sendkey`; asserts the mirrored output on serial
  (`greenlime works`, `abc`, no `abd`) and saves before/after screenshots.
* `scripts/qmp_desktop_test.py` — clicks Start (28,776) → menu opens
  (exact `MENU_BG` count in the panel rect ≥50000), About row (208,487)
  → popup opens (`TITLE_BG` in its titlebar ≥5000, menu count drops to
  0), click away → both dismiss (counts back to 0).
* `scripts/qmp_tablet_test.py` — one boot proves splash + tablet: shot
  right after `[arch] gdt loaded` shows only the centered title (clean top
  band), then abs injections at min/max/mid land the cursor at
  (4,6)/(1279,799)/(643,405) and a tablet click opens + dismisses the
  Start menu (menu_px=220625, identical to the PS/2 path).
* `scripts/qmp_multitest.py` — the multi-window proof in one boot (15
  checks): demo window geometry from the server's own log + titlebar
  pixels; keys land in the focused window (`[demo] got: hi` vs terminal
  `focusok`, with a no-leak count check); taskbar icon refocuses; a
  tablet drag moves the window to an exact logged position (pixel check
  at the new spot); the grip resizes (±2px tablet truncation tolerance);
  the X closes it (client sees the EOF, titlebar pixels gone).
* `scripts/zoom11_ppm.py`, `scripts/zoom_ppm.py`, `scripts/ppm2png.py` —
  read actual pixels.

## Performance notes (why it can feel slow)

* **Debug + TCG is ~50x slower than native.** Debug Rust (no inlining,
  fat frames, bounds checks) under QEMU software emulation makes every
  millisecond of guest work cost real seconds. `make run-release` builds
  with optimizations + LTO: 4.8 MB ISO and roughly 3x faster wall-clock
  to idle.
* **Serial is 115200 baud (~11 KB/s).** The full boot log is ~15 KB, so
  serial alone floors boot at ~1.5 s even natively.
* **Damage keeps repaints small.** A keystroke composes one text band +
  blits it; the idle path still touches nothing. A full-screen drag is
  the expensive case (compose window-sized rects per pointer report).
* **Spurious IRQs need guards.** Both IRQ handlers check OBF (+AUX for the
  mouse) before consuming port 0x60; otherwise stale bytes flood the rings.
* **Cap per-lap drains.** A babbling device must not wedge the server loop;
  mouse bytes are capped at 64/lap.
* **Cursor vs damage bookkeeping.** The cursor is not composed: undraw
  runs at lap start, draw runs after present. Invariant: at lap ends the
  save-under pixels match the screen exactly.
* **fb takeover.** `drivers::fb::takeover()` moves the framebuffer +
  warm font cache out of the kprint console; serial keeps working.
  kprint mirroring becomes a no-op automatically (console is None).

## Gotchas met in v0.8 (read before touching input/compositor)

* **No SSE: per-pixel `f32` in compose is a time bomb.** The
  `x86_64-unknown-none` target only has `fxsr`/`x87` (no `sse`), so every
  float op lowers to a `compiler_builtins` soft-float call. The first
  wallpaper draft computed its gradient+glows per damage rect in `f32`:
  full-screen paint took ~350s at `-O0`, each present() stalled dev's
  `WIN_WRITE` send into winsrv's mailbox, which blocked dev's reply to
  shell — boot wedged with no `[shell] interactive ready`. Fixed by
  rendering the wallpaper **once** in Q16 integer math
  (`render_wallpaper`) and memcpy-ing rows per damage. Keep all compose
  paths integer; `fontdue` f32 work happens once per glyph (cache).
* **PIC cascade bit.** Master mask must clear IRQ2 (0xF8, not 0xFC) or
  slave IRQs (mouse IRQ12) never reach the CPU. Silent, no faults.
* **QEMU's virtio-input has no legacy interface.** It's a post-1.0
  device: `disable-modern`/`disable-legacy` are no-ops and only
  `1AF4:1052` + MMIO capability regions exist (the legacy probe would
  only ever find `bar0=0`). QEMU 10's `virtio_pci_cap` also grew a
  `cap_len` byte at +2, so `cfg_type` sits at +3 and `bar` at +4
  (offset/length remain 8/12 — parsing them first masked the shift).
  BAR4 is *prefetchable* 64-bit: type bits `11`, not just `10`.
* **Limine maps the framebuffer but not general MMIO.** The tablet BARs
  at `0xfe000000` translate to nothing (identity *and* HHDM) — they need
  `vmm::map_mmio_page` (boot-time identity map, `NO_CACHE`).
* **QEMU drops whole input reports when the avail ring is empty**
  (`virtio_input_send` pops at SYN_REPORT), so all 64 event buffers stay
  stocked and are recycled inside the same drain loop.
* **Splash vs panic.** `fb::splash` clears + quiets the console; without
  `wake_console()` in the panic handler a panic would print into serial
  only and look like a hang on screen.
* **Compose may write outside the damage rect — blit must not.** Layer
  draws spill (glyphs overhang, fills are clipped per-rect but text
  isn't); correctness comes from blitting only the damage and from every
  compose recomputing from state. Don't "optimize" by blitting the union
  of touched pixels.
* **Back buffer = heap Vec<u32>; the Framebuffer is a view.** The view
  pointer stays valid when WinSrv moves (Vec owns its data), but the
  Vec must be kept alive (`#[allow(dead_code)]` field) or the view
  dangles.
* **Taskbar icons must be id-ordered, not z-ordered.** Following the
  z-order makes icons shuffle on every focus click (and breaks test
  coordinates); destroyed ids skip a slot without leaving a gap.
* **Boot focus policy.** WIN_CREATE does not focus the new window: a
  boot-time client (demo) would otherwise swallow the type-test's blind
  keyboard input. Click/taskbar focuses.
* **Cursor vs white text.** The cursor is 240,240,240 — pixel detectors
  must threshold ≥239 in **all** channels, and every UI text color
  (titles `232,238,246`, menu `230,236,244`, console `214,222,235`) must
  keep at least one channel <239 or the detector "finds" text as cursor.
* **Tablet px↔device truncation.** Guest px → device units → guest px
  loses up to 2px; resize asserts use a ±2 tolerance (a drag to
  (1180,450) logs `579x369`).
