# GreenLime GUI (v1.0 "Fluent dark", made by GLM 5.3): window server + desktop + Files + terminal.

## Theme (v1.0 · M12 rework: dark Fluent acrylic over a user wallpaper)

Charcoal surfaces + fluent cyan-blue accent (`winsrv.rs` palette consts).
The Windows-11 mica concept: everything floats on the wallpaper as
translucent, rounded, soft-shadowed glass.

| role | color | role | color |
|---|---|---|---|
| window body | `(32,34,38)` | window text | `(208,214,224)` |
| focused titlebar | `(44,46,52)` | unfocused titlebar | `(33,35,40)` |
| titlebar text | `(214,220,232)` | frame (fc/unfc) | `(74,79,90)` / `(48,52,60)` |
| close X well / hover | `(56,58,66)` / `(196,43,28)` | hover fill | `(62,66,76)` |
| pressed / open pill | `(50,58,74)` | accent (cyan-blue) | `(76,194,255)` |
| accent fills (LED/strips) | `(44,78,110)` | dock/tray pill tint | `(38,41,48)` |
| card tint (menu/popups) | `(34,37,44)` | tiles/search/quick rows | `(52,56,64)` |
| menu text / hints | `(226,230,238)` / `(126,136,154)` | watermark | `(148,166,196)` |
| icon wells / tray ink | `(60,64,74)` / `(198,204,216)` | folder amber | `(206,160,78)`+`(240,216,140)` |
| pill hairline / card hairline | `(66,72,84)` / `(62,68,80)` | cursor | `(240,240,240)` |

Acrylic strengths (Q8): **PANEL_A 214** (≈84%, dock/tray/weather pills),
**CARD_A 226** (≈88%, start card/popups/quick settings). Radii:
`RAD_WIN 10`, `RAD_PANEL 10`, `RAD_TILE 8`.

Detector rule unchanged from v0.8: pixel tests count the cursor by
"≥239 in **all** channels"; every painted UI pixel stays <239 per
channel (bright text like `MENU_FG (226,230,238)` and the accent
`(76,194,255)` have one channel low on purpose), and the wallpaper task
clamps its own pixels the same way — the cursor (240,240,240) stays the
only all-channel blob on screen. Verified live: every QA frame contains
exactly one 9×14, 60-px blob at the guest-cursor position.

* **Wallpaper** — still the ring-3 pipeline (task 8 embeds
  `assets/wallpapers/wallpaper.jpeg`, decodes with zune-jpeg in its own
  16 MiB heap, streams rows via WIN_WALL_SETUP/ROWS/DONE; kernel only
  converts + blits). **Asset format rule (found live)**: the decode has
  to fit the 16 MiB userspace heap — a 6000×3375 photo does not.
  `assets/wallpapers/wallpaper.jpeg` was therefore transcoded to
  **1600×900 baseline JPEG q92 (~330 KiB)** from the user's original
  `wallpaper.jpg` (kept untouched at 6000×3375 — never embed that
  directly). Every dark surface composites the decoded rows as its
  backdrop instead of a flat fill.
* **Taskbar strip (48px)**: three floating pills with dead margins —
  left **weather** ("18° Tokyo"), centered **dock** (Start + one icon
  per window, focus = accent LED), right **tray** (wifi arcs / speaker /
  battery strokes + RTC clock + "Day N"). All pills are
  `acrylic_round_clip` blends over the wallpaper with 4-ring shadows.
* **Start menu** — `MENU_W 560 × MENU_H 436` centered acrylic card
  (360,304): accent-disc + greeting header ("Good Evening, User!" from
  the RTC minute), "Pinned" row, **5 tiles** (Files / Terminal / Demo /
  Settings / About — rounded `MENU_BG2` wells, icon glyph + label, hover
  fill, they fade in at 2× the card rate), bottom search pill (stadium,
  keys filter while open, Enter launches the first match).
* **Quick settings flyout** (new) — tray click opens `Popup::Quick`:
  240×280 acrylic panel anchored right-above-tray (1028,468 at
  1280×800): 2×2 toggle tiles (Wi-Fi / Bluetooth / Night light / Battery
  saver — accent fill when ON), click-to-set **volume + brightness
  sliders** (rows at +164/+208, state kept in `qs_volume`/`qs_bright`),
  "All settings" footer opens Settings. One flyout at a time; outside
  click dismisses; opens/closes animate like everything else.
* **Files** — explorer (id 1, `FILES_ID` reserved; demo client = id 2),
  opens at (230,140) 540×420: dark body, `vfs::list_names` rows with
  folder glyphs, click navigation with `../` pop, dead-path guard.
* **Terminal**: unchanged contract — opens on demand, buffered early
  text, X closes, id 0 (`TERM_ID`).
* **Popups (About/Settings)** — centered dialog (410,295) 460×210,
  dark acrylic card + `TITLE_BG` strip (rows +1..+30), X mark, any
  click dismisses.
* **Animations (new)** — every window/menu/popup/flyout opens with a
  **120 ms fade + rise** (`ANIM_TICKS=12`, `ANIM_RISE=14`, Q8
  `anim_p`). The progress scales the shadow, the scrim alpha and the
  rise offset. **Invariant: `ANIM_TAIL=180`** — one compose lap can be
  slower than the whole animation (a loaded debug VM laps at ~1 s), so
  the fade keeps its region invalidated for 1.8 s past expiry and the
  first post-deadline lap paints the final crisp `p=256` frame. Without
  it a slow lap freezes a half-faded ghost on screen (repro'd live on
  the Files window and the menu).
* **Maximize (new)** — double-click on a titlebar (within 25 ticks)
  toggles maximized (`(0,0,sw,sh-48)`), serial proof
  `[winsrv] win N toggled`.
* Splash (dark, `fb.rs`) unchanged — pre-GUI boot console.

## What runs where (v1.0, unchanged since v0.8)

```
init(3)──▶ spawn ──▶ shell(4) demo(5) net(6) fuzz(7) wallpaper(8)
wallpaper(8) ──include_bytes! jpeg──▶ zune-jpeg (own 16MiB heap)
             ──WIN_WALL_SETUP/ROWS/DONE──▶ winsrv: convert+blit desktop
demo  ──rpc────▶ PORT_WINSRV ──WIN_CREATE──▶ client window (id ≥ 2)
shell ──term0──▶ DEV ──WIN_READ/WRITE──▶ terminal window (id 0)
vfs/dev/winsrv/net = kernel tasks 0–3(+6); all IPC over frozen ports
```

Boot order: vfs(0), dev(1), winsrv(2) → init(3), shell(4), demo(5),
net(6), fuzz(7), wallpaper(8) — `MAX_TASKS = 9`. The demo's window is
open at boot, so the dock has 2 slots: **Start center = (616,776)**
(dock pill w = 16 + n·48 − 8 + 16, pill_x = (sw−w)/2; slot i center =
pill_x + 36 + i·48 at y 776). Tests rely on every number above.

## Geometry cheat sheet (1280×800)

* strip_y = sh−48+2; strip repaint = `strip_rect` (full width,
  `sh−48−SHADOW_PAD`) so pill shadows' overhang repaints with it.
* Start card at ((sw−560)/2, sh−48−12−436) = (360,304); tile grid from
  +100: pitch x 100, y 106, tile 92×100 → row-0 centers Files (430,450),
  Terminal (530,450), Settings (630,450), Demo (730,450), About
  (830,450); search pill (380,676) 520×44, stadium radius 22.
* Quick settings: `popup_rect(Quick)` = (sw−12−240, sh−48−12−280) =
  (1028,468); tiles 2×2 at +40/+104; slider rows at +164/+208 (band
  208×30, track 176×14 inside); footer at +QS_H−34 (208×24).
* Popup (About/Settings) (410,295) 460×210; `TITLE_BG` strip rows
  +1..+30 (test counts (44,46,52) inside (410,295)-(842,325)).

## The compositor (damage rects + back buffer + acrylic)

Back buffer Vec<u32>, `invalidate(rect)` unions one damage rect per
lap, layers compose bottom-to-top: wallpaper row-copy → window shadows
→ windows (rounded, scrim-faded during the anim) → dock/weather/tray
pills → start card → quick flyout → popup. Cursor drawn on top, never
composed; idle = zero fb traffic. The Fluent-dark (M12) additions keep the contract:

* `acrylic_round_clip(back, wall, r, rect, rad, tint, a)` — blends the
  **cached wallpaper rows** under the rounded rect with the tint at
  alpha a (integer blend, no f32): panels/cards read the backdrop, not
  a flat fill. The compose borrow is field-split (`let Self { back,
  wall, … } = self`) so helpers take `&[u32]` wall + `&mut` back
  concurrently.
* `window_shadow(back, wall, r, wr, scale)` — 4 rings × 3 px around the
  rect (`SHADOW_A=[120,76,42,24]` inner→outer), alpha scaled by the
  anim progress; drawn *before* the surface so nothing shadows the
  chrome itself.
* `scrim_rect_clip` — the open-animation fade: blends the wallpaper
  back over the surface at `256−p` (the "materialize from glass" cue).

## Windows, focus, LimeDE chrome

Same rules, worth keeping:

* **Window = rect + title + text lines**, content renders statelessly,
  wraps at content width, scrollback 200.
* **Focus follows clicks**; new windows don't steal it; raise = top of
  z; **dock icons stay id-ordered** (test coordinates are contractual).
* **Drag** from titlebar (damage = old ∪ new), **resize** via the 16px
  bottom-right grip (min 200x120), serial logs `moved to` / `resized`.
* **Double-click titlebar** toggles max (25-tick window),
  `[winsrv] win N toggled`.
* **Close** = titlebar X: EOFs pending client reads, drops the dock
  icon. Serial: `[winsrv] win N closed`.
* Console rules (`push_text`): writes continue the open line, `\n`
  breaks; MAX_LINE 4096 + SCROLLBACK 200 both capped.

## Interactive stdin (line discipline)

Unchanged, test-facing: set-1 scancodes → ASCII
(`winsrv::scancode_ascii`), canonical terminal mode (echo + backspace +
Enter-commit), blocking reads via dev's WIN_READ relay. **While the
Start menu is open, typing filters its search instead** (lowercase
graphic + space; Enter launches the first match). The search query is
the sliced buffer `&search[..search_len]` — passing the raw 32-byte
array hides every tile (repro'd + fixed in M12).

## Proved by test (not by reading)

* `scripts/qmp_desktop_test.py` — Start slot **(616,776)** opens the
  centered card: **exact `MENU_BG2 (52,56,64)` tile count in
  (384,404)-(896,516) ≥ 30000**, About tile **(830,450)** opens its
  popup (`TITLE_BG (44,46,52)` strip (410,295)-(842,325) ≥ 5000), menu
  count drops to 0, click outside at (1000,300) dismisses both.
* `scripts/qmp_multitest.py` — multi-window proof on the dark chrome
  (demo id 2, Start (616,776) with demo open, demo dock slot (688,776)
  with 3 slots, dark `TITLE_BG (44,46,52)` / `TITLE_BG2 (33,35,40)`
  pixel asserts) — **18/18**.
* `scripts/qmp_tablet_test.py` — splash band + tablet abs clicks
  (tl/br/mid corners), menu tile count (52,56,64) ≥ threshold on the
  clicked card, dismissed = 0.
* `scripts/qmp_type_test.py` — Start (616,776) → Terminal tile
  (530,450) → `[winsrv] launch Terminal`; types `echo greenlime works`
  + `abd`/backspace/`c`; asserts mirror + screenshots.
* `scripts/qmp_net_shell.py` / `qmp_net_test.py` / `qmp_smp_test.py` /
  `qmp_input_test.py` — ring-3 net machine proof, DHCP+ICMP, SMP, and
  the cursor/keyboard regressions (cursor = the only ≥239 blob,
  moves exactly).
* 1h fuzz soak on the release ISO: `RESULT: PASS — syscall fuzz 1h
  clean`, 0 faults, 0 panics, 20,758,528 iters.

## Performance notes (why it can feel slow)

* **Debug + TCG is ~50x slower than native.** Debug Rust (no inlining,
  fat frames, bounds checks) under QEMU software emulation — big acrylic
  compose laps take ~1 s there; **one lap can outlast the whole 120 ms
  animation**, which is exactly why `ANIM_TAIL` exists. `make
  run-release` (opt-level z) is the verification target for anything
  timing-sensitive — frames compose in ~10 ms there.
* **Serial is 115200 baud (~11 KB/s)** — boot log floors it.
* **Wallpaper streaming is bandwidth-shaped**: 1280×800 = 1502 messages
  of ~2 KiB through the frozen IPC gate. Rows are cached once;
  composing damage never re-enters the streaming path. Acrylic helpers
  read that cache, not the framebuffer, so a hover repaint costs a few
  blended rows of wall + tint, not a decode.
* Damage/idle contract unchanged: one union rect per change, zero
  traffic when nothing happens.

## Gotchas met building v1.0 (read before touching input/compositor/sched)

* **No SSE: per-pixel `f32` in compose is a time bomb.** Target has only
  `fxsr`/`x87`; every float op is a soft-float call. The v0.8 wallpaper
  draft painted ~350s per screen at `-O0`. All compose paths are integer
  (`acrylic_round_clip`/`fill_round`/shadows included).
* **One compose lap can be slower than an animation → `ANIM_TAIL`.**
  Repro: Files window froze mid-fade (`p≈21`) for the remaining boot on
  the debug ISO; the clock/hover repaints updated other regions so the
  compositor looked alive while the ghost never resolved. The tail
  (extra invalidation past expiry) guarantees a post-deadline crisp
  lap. Any future anim must go through the same tail discipline.
* **Pass the SLICED search query** `&self.search[..search_len]` to
  renderers/filters — the raw `[u8;32]` buffer reads as a 32-NUL
  "query": tiles vanish while the placeholder looks fine. (Caught by
  the desktop test's exact-count tile probe.)
* **Kernel stacks: 64 KiB per task (was 32).** ABI v6 messages are
  ~2 KiB; at `-O0` every frame that stages one costs it. Found live via
  `rip=0x0` fault loops; the fault-path dump (32 stack words + task
  table) is permanent. Never shrink kernel stacks without re-running
  the debug B3 suite.
* **Wallpaper decode lives in ring 3 on purpose** (trust boundary — a
  jpeg bug kills task 8, not the kernel). **Heap budget rule**: decode
  must fit 16 MiB — transcode the user's wallpaper to ≤1600×900 first
  (see asset note above); user stack = 128 KiB for the -O0 decode
  frames.
* **QEMU pointer pick**: `run-qemu.sh` promotes the tablet with
  `activate_tablet.py` (`mouse_set`), else PS/2 wins and the guest
  cursor dead-ends in relative grab mode.
* **Damage = one union rect**, fine at this scale; **text windows
  only**; subset font with lazy full-font loader still filed under
  performance.
* **Compose may write outside the damage rect — blit must not**; back
  buffer owns the pixels (Vec must outlive its view).
* **Boot focus policy**: WIN_CREATE never steals focus (demo/Files boot
  windows stay unfocused so the type test's blind typing lands in the
  shell console).
* **Cursor vs everything bright**: cursor = 240,240,240; every painted
  color keeps one channel <239 — dark theme included (bright inks and
  the accent are capped on a channel on purpose) — or the test "finds"
  text as a cursor. The wallpaper task clamps its own pixels too.
* **Tablet px↔device truncation ±2px** — resize asserts tolerate it.
* **Icons are id-ordered, never z-ordered** (else icons shuffle and
  break every coordinate test).
