# GreenLime GUI (v1.1 "Windows 11 dark", made by GLM 5.3): window server + desktop + Files + terminal.

## Theme (v1.1 rework: Windows 11 dark mica, brand lime accent)

Windows 11 dark surfaces + the brand lime accent (`winsrv.rs` palette
consts). The Win11 concept: a **full-width acrylic taskbar** flush with
the bottom edge, a **centered button cluster** (start logo, app pins,
running clients), a right **clock zone** with the two-line wall clock
(all icon glyphs removed — icons went away, the clock stayed), a **centered floating start card** with a search field and a
pinned-tile grid, and rounded soft-shadowed windows — every glass
surface alpha-blends the wallpaper rows beneath it (acrylic, no flat
fills). No bloat: no flyouts, no widgets, one card, one bar.

| role | color | role | color |
|---|---|---|---|
| window body | `(32,32,32)` | focused titlebar | `(43,43,43)` |
| unfocused titlebar | `(37,37,38)` | titlebar text / content text | `(222,226,230)` |
| frame (fc/unfc) | `(74,78,88)` / `(47,49,55)` | titlebar button glyph | `(200,204,210)` |
| button hover / close hover | `(58,60,66)` / `(196,44,30)` | hints / dim labels | `(150,154,162)` |
| bright glyphs (tray, terminal) | `(232,236,242)` | tiles / tray text | `(216,220,228)` |
| taskbar tint | `(20,22,27)` @205 | start card tint | `(18,20,26)` @216 |
| card / bar hairline | `(56,60,70)` / `(56,58,66)` | search field fill | `(44,46,52)` |
| hover lift / pressed fill | `(58,61,68)` / `(48,50,58)` | **accent (lime)** | `(170,232,64)` |
| running (unfocused) underline | `(146,150,158)` | client-chip azure strip | `(122,180,250)` |
| taskbar/menu/grid icons | **icons8 "fluency"** art, baked RGBA (`assets/icons`) | power button icon | icons8 `shutdown` (red) |
| avatar ink on lime | `(30,36,18)` | cursor | `(240,240,240)` |

Acrylic strengths (Q8): **BAR_A 205** (≈80%, taskbar), **PANEL_A 216**
(≈84%, start card). Radii: `RAD_WIN 8` (Win11 window corners — square
while maximized), `RAD_PANEL 12` (card), `RAD_CELL 6` (taskbar buttons
+ tray), `RAD_TILE 8` (start tiles). Shadows: `SHADOW_A=[110,70,40,22]`
(4 rings × 3 px, inner→outer). The Settings window keeps the
"GreenLime OS v1.1" info card (no separate About popup anymore).

Detector rule unchanged from v0.8: pixel tests count the cursor as the
**only pixel with all three channels ≥ 239** — every painted UI color
keeps at least one channel strictly below 239 (bright text
`WHITE (232,236,242)`, `BALL_WHITE (238,240,246)`, lime `g=232` keep
their red channel low on purpose), and the wallpaper task clamps every
decoded pixel to ≤238 the same way — the cursor (240,240,240) stays
the only all-channel blob on screen. Verified live on the wave
wallpaper: QA frames contain exactly one 12×14, 60-px blob at the
guest-cursor position.

* **Wallpaper** — the ring-3 pipeline (task 8 embeds
  `assets/wallpapers/wallpaper.png` — `build.rs` picks **`.png` →
  `.jpeg` → `.jpg`**, first match wins, and emits
  `GREENLIME_WALLPAPER` + `GREENLIME_WALL_FMT` to the crate — decodes
  with **zune-png 0.5.2** (PNG) or zune-jpeg (JPEG) in its own 16 MiB
  heap, streams rows via WIN_WALL_SETUP/ROWS/DONE; the kernel only
  converts + blits). **Drop-in rule**: replace the file with any
  ≤1600×900 `.png` (or baseline `.jpeg`) and rebuild — no code changes.
  **Asset budget rule (found live)**: the decode has to fit the 16 MiB
  userspace heap — a 6000×3375 photo does not. The shipped default is
  the generated dark-navy **blue wave ribbon** PNG, 1600×900 (~292 KiB),
  downscaled/clamped so no channel reaches 239 (see
  `scripts/…` history; users can simply swap the file later).
  PNG `decode_wall()` is lean: one exact-size `Vec`, 16-bit downshift,
  alpha-drop; a heap guard refuses politely (`exit 2`) if the needed
  `output + 3·w·h` exceeds ~13 MB instead of OOM-ing the task.
* **Taskbar (48px, full width)**: one `acrylic_round_clip` blend over
  the wallpaper (`BAR_TINT @205`, square — it is flush with the screen
  edges) + a 1px top hairline. The **centered cluster** holds
  `bar_cell_count(nclient) = 4 + min(nclient,4)` buttons at 44px pitch
  (40px cells, `RAD_CELL 6`): start logo, Files pin, Terminal pin,
  Settings pin, then one button per running client (max 4). Hover lifts
  with `HOVER @170`; the open surface (card / focused-open window)
  tints `ACTIVE` and gets a **16×3 rounded underline pill** at
  `(cx−8, sh−10)` — **lime when focused, soft gray when running**;
  this pill is the pixel contract. The start button is a brand-lime
  **G** glyph (`draw_start_g`, 22 px em, ink ends ≤y≈782 so the
  underline probes stay clean); pins draw the baked icons8 icons
  (`folder` / `console` / `settings`, 24px exact blits); a client button draws a mini rounded window with an
  18×5 azure strip. Empty bar space clicks just close the card.
* **Start card** — centered acrylic card `MENU_W 560 × MENU_H 300`
  floating `MENU_GAP 12` above the bar: `menu_rect = ((sw−560)/2,
  sh−48−12−300)` = **(360,440,560,300)** at 1280×800. Search field
  `(376,456,528,36)` radius 18 (`SEARCH_BG`, magnifier + placeholder
  when empty) — **live filtering shared with the card**: typing filters
  the tiles, Esc closes, Enter launches the first match. "Pinned"
  caption, then the filtered tile grid `tile_rect(gi) = (mx+16+134c,
  my+88+108r, 126,100)` (4 columns; centers `(439/573/707/841, 578)`):
  **Files, Terminal, Settings, Demo** (Demo: focuses the demo window
  while it is alive — duplicates are refused; once the demo task has
  **exited** it respawns it from the baked-in ELF and logs
  `[winsrv] launch Demo (respawned task 5)`, with the slot reap
  handing the demo right back its boot id — M15.1). Tiles hover-lift;
  icons draw at 28px — baked icons8 blobs (`folder` / `console` / Demo's
  `rocket`, exact-size); the Settings glyph is the fluency **gear** cog
  since v1.2.1 (the old `settings` art read Windows-logo adjacent — the
  swap was chosen against `scripts/probe-gear.py`'s shape maps). Footer:
  hairline, a
  lime avatar circle with a "G" + "GreenLime" label (inert), and the
  power button `power_rect = (mx+mw−60, my+MENU_H−54, 44,40)` =
  (860,686) — the ORIGINAL in-tree power glyph (`power_24.rgba`, baked
  by `scripts/bake-icons-original.py`; chrome must never be ambiguous
  about its origin). The old button only logged `[winsrv] power` and
  was kept open deliberately back then — it now powers off for REAL:
  click → `[winsrv] shutdown` → ACPI S5 (PM1a control at I/O 0x604,
  QEMU flavor), the VM exits cleanly.
  Clicks outside the card dismiss it; the left **Windows key (E0
  0x5B)** toggles it. The card flip prints on serial —
  `[winsrv] start card open` / `[winsrv] start card closed` — so a
  dead click vs. an actually-open card is always distinguishable in
  the log (test/debug affordance, M15.1).
* **Clock zone** — right, `TRAY_W 84` at `(sw−84−12, 760, 84,40)` =
  (1184,760): ONLY the two-line RTC wall clock (time over date,
  right-aligned) — the old wifi/volume/battery glyphs are gone. The
  hover chip covers the whole zone, **highlight only — no flyout by
  design** (the v1.1 quick-settings panel is gone; "no bloat").
* **Files v2** — a real explorer (see "Files v2 (M16)" section below);
  opens at `(280,110) 720×540` with sidebar places, breadcrumb, icon
  grid and footer action chips. Title stays `Files - <path>` (files
  navigation reads its own state; the click parser is geometry-driven,
  no title parsing). Opens ONLY from the pin/tile (never at boot).
* **Terminal**: unchanged contract — opens on demand (`menu_launch` →
  `ensure_terminal`), buffered early text, X closes, id 0 (`TERM_ID`).
* **Settings** — now a real system window (`SETTINGS_ID`), not a
  popup: opens at `(sw/2−210, 170) 420×300` = (430,170,420,300) with a
  static info card (Display/Uptime/Windows/v1.1 lines). Windows don't
  dismiss on click-away anymore — desktop clicks never close windows.
* **Animations** — every window/card opens with a **120 ms fade +
  rise** (`ANIM_TICKS=12`, `ANIM_RISE=14`, Q8 `anim_p`). The progress
  scales the shadow, the acrylic alpha and the rise offset. **Invariant:
  `ANIM_TAIL=180`** — one compose lap can be slower than the whole
  animation (a loaded debug VM laps at ~1 s), so the fade keeps its
  region invalidated for 1.8 s past expiry and the first post-deadline
  lap paints the final crisp `p=256` frame. Without it a slow lap
  freezes a half-faded ghost on screen (repro'd live on the Files
  window and the card).
* **Maximize** — double-click on a titlebar (within 25 ticks) toggles
  maximized (`(0,0,sw,sh-48)`, corners go square), serial proof
  `[winsrv] win N toggled`.
* **RTC date** — `rtc::snapshot()` returns `(h,min,sec,year:u16,month,
  day)`; `BOOT_DATE` packs `y<<16|m<<8|d`; `now_date(uptime_secs)`
  rolls over at day boundaries. Boot line (serial only):
  `[rtc] boot time 07:46, date 2026-10-24`.
* Splash (dark, `fb.rs`) unchanged — pre-GUI boot console.

## What runs where (v1.1, unchanged since v0.8)

```
init(3)──▶ spawn ──▶ shell(4) demo(5) net(6) fuzz(7) wallpaper(8)
wallpaper(8) ──include_bytes! greenlime-wallpaper──▶ zune-png|zune-jpeg (own 16MiB heap)
             ──WIN_WALL_SETUP/ROWS/DONE──▶ winsrv: convert+blit desktop
demo  ──rpc────▶ PORT_WINSRV ──WIN_CREATE──▶ client window (id ≥ 2)
shell ──term0──▶ DEV ──WIN_READ/WRITE──▶ terminal window (id 0)
vfs/dev/winsrv/net = kernel tasks 0–3(+6); all IPC over frozen ports
```

Boot order: vfs(0), dev(1), winsrv(2) → init(3), shell(4), demo(5),
net(6), fuzz(7), wallpaper(8) — `MAX_TASKS = 9`. The demo's window is
open at boot (`[winsrv] win 2 open at 700,120 540x330`): **nclient=1 →
the centered cluster is 5 buttons wide (216px), x0=532 — the start
button is (532,756,40,40), center (552,776), and the demo client
button (708,756,40,40), center (728,776), pitch 44**. `nclients()`
counts only true clients (excludes the Terminal `TERM_ID`, Files
`FILES_ID` and Settings `SETTINGS_ID` system windows) — launching the
terminal never shifts the demo button; pins keep their slots. Tests
rely on every number above.

## Geometry cheat sheet (1280×800)

* Taskbar: full width, `(0,752,1280,48)`; buttons span **756..795**
  (centers **y=776**); repaint = `strip_rect` (full width,
  `sh−48`), so the bar + underlines always repaint together.
* Cluster: `cluster_w = (n−1)·44 + 40`, `x0 = (1280−cluster_w)/2`;
  cell i = `(x0+44i, 756, 40, 40)`. At boot (nclient=1, n=5): start
  center **(552,776)**, Files 596, Terminal 640, Settings 684, demo
  client **728**. With ZERO client windows (demo closed), n=4 → the
  cluster re-centers: x0=554, start center **(574,776)**. Tests must
  compute cell centers from the formula, never from the boot constant —
  clicking the stale (552,776) after closing the demo lands in dead bar
  space (a silent no-op that closes the card); the first M15.1 headless
  runs died exactly there. Underline pill: `(cx−8, 790, 16, 3)`.
* Clock zone `(1184,760,84,40)` — clock only (icons removed), hover
  chip = whole zone, no flyout.
* Start card (360,440,560,300): search `(376,456,528,36)`; "Pinned" at
  +62; tiles `(+16+134c, +88+108r, 126,100)` → tile centers
  (439,578) Files, **(573,578)** Terminal, **(707,578)** Settings,
  (841,578) Demo; footer hairline at +232; power button
  (860,686,44,40).
* Windows: `TITLE_H 34` titlebar, body inset 8, radius 8 (square
  maximized); close `(x+w−26, y+4, 20,22)` (center `x+w−16, y+15`),
  maximize `(x+w−46, y+4, 20,22)`, grip 16px `(x+w−16, y+h−16)`.
* System windows: Terminal (150,100,980,540), Files (280,110,720,540),
  Settings (430,170,420,300); client windows keep the WIN_CREATE
  cascade geometry.

## Files v2 (M16 — the Windows-Explorer-style explorer)

The Files window is a **system window owned entirely by winsrv**
(`FILES_ID=1`, custom painter `draw_files` — not a userland WIN_BLIT
client). Sidebar places, breadcrumb, an icon grid, and a footer with
state-aware action chips. Everything below is the exact-pixel /
exact-geometry contract, at the default window rect (280,110,720,540).

**Palette additions (win `winsrv.rs` consts)**: places/sidebar card
`F_SIDEBAR (38,38,42)`, selection lift `F_SELECT_A = 56` (Q8) blended
in LIME — grid cell selection wash `(62,75,39)` over the body
`(32,32,32)`, sidebar active place row `(66,80,46)` over the card; Back
chip + footer chips fill `SEARCH_BG (44,46,52)` hover `HOVER`.

**Geometry (`files_geo(x,y,w,h)`, `bx=x+1, by=y+1+TITLE_H`,
`bw=w−2`)**:

* Sidebar card `(x+5, by+6, F_SIDE_W−10, bh−7)` = `(285,151,142,507)`
  at default, radius 8, fill `F_SIDEBAR`; place rows
  rows `(bx+9, by+14+36i, 134, 28)` for i=0..3 = Desktop /
  Documents / Downloads / Recycle Bin (`F_PLACE_PATHS`, pitch
  `F_ROW_H=36`); centers at the default rect: **(357,173 / 357,209 /
  357,245 / 357,281)**. Active row = exact LIME wash + a 3px lime
  left edge; hover = `HOVER@170`. Row glyphs `draw_desk` / `draw_folder` /
  `draw_dload` / `draw_bin` at 20px (the same azure folder glyph as the
  grid; desk = a little monitor, bin = a lidded trash can,
  dload = a tray with a down arrow).
* Breadcrumb strip `F_BREAD_H 40`: `< Back` chip `geo.back =
  (bx+158, by+9, 62, 22)` (**(439,154)** at default) — dimmed at the
  ramfs root (alpha 128, `SUB_FG` label; it IS clickable there but the
  root has no parent so the handler no-ops); the current path paints
  right of it in `SUB_FG`. Hairline under the whole strip.
* Grid origin `(grid_x, grid_y) = (bx+F_SIDE_W+10, by+F_BREAD_H+13)` =
  **(443,198)** at default; cells `F_CELL_W×F_CELL_H = 86×84` at
  pitch-0 tiling down the grid width (`grid_w = bw−F_SIDE_W−20 = 546`;
  cols = 546/86 = 6). Cell = icon (dir → icons8 `folder-invoices`
  30px, file → icons8 `documents` 28px, exact-size blits) + name under
  it, dirs first then names ASCII case-insensitive (the
  `list_details` order: `bool` desc for dirs).
  Single click selects (lime wash + name below), double-click (same
  cell ≤25 ticks) navigates into a dir; clicking empty grid space
  clears the selection. Selection wash probe `(451,206)` inside cell 0.
* Footer strip `F_FOOT_H 30` (hairline y=619 at default): left
  `"N items"` count in `SUB_FG`; when a file is selected a right-aligned
  `"name (size)"` line in `SUB_FG` (size via `fmt_size`: B → KiB → MiB →
  GiB); chips 20px tall, right-aligned with 6px gaps —
  **Delete `(805,624,56,20)`**, **Restore `(867,624,64,20)`**,
  **Empty `(937,624,56,20)`** (centers 833/899/965 at y 634). Chips are
  state-aware: bin view shows Restore (enabled only with a selection) +
  Empty; non-bin views with ≥1 disk file selected show Delete; ramfs
  views show none (root is read-only there) and clicking a place with
  no disk prints `[files] place unavailable (no disk)`.

**Behavior contract**:

* `files_open(path)` re-lists through `vfs::list_details` (names +
  dir flag + sizes), filters `.orig` sidecars in the bin view,
  sorts dirs-first, repopulates the window (console lines always empty
  — the painter is custom) and prints the serial proof
  **`[files] show <path> items=<n>`**. Every mutation refreshes the
  cache through `files_open(fpath)` — VFS is never painted per-lap.
* Sidebar rows navigate by place; breadcrumb Back pops to the parent;
  double-click enters dirs; Del key trashes the selection the same way
  the Delete chip does (`decode_key` arm keyed to the focused Files
  window).
* Delete = `vfs::trash(full_path)` → `**[files] trash <name> ok**` +
  `[vfs] trash <src> -> <binpath>` (refused prints `ok`→`refused` and
  keeps everything); bin view Delete is absent by design; ramfs-root
  delete prints `[files] delete refused (ramfs: root only reads)`.
* Restore = `vfs::untrash(name)` → `**[files] restore <name> ok**` +
  `[vfs] restore <name> -> <orig path>` then re-shows the bin; Empty =
  `vfs::empty_bin()` → `[vfs] bin emptied (N items)` +
  **`[files] bin empty n=N`**. The `.orig` sidecar scheme that makes
  all three work is `docs/STORAGE.md`.

**Taskbar pin geometry** (boot cluster, nclient=1 → start 552, Files
**(596,776)**, Terminal (640,776), Settings 684, demo client 728) —
same pin slots as before; the Files pin opens `files_open("/")` and
prints `[winsrv] launch Files` once (the pin focuses an already-open
Files instead of duplicating).

## The compositor (damage rects + back buffer + acrylic)

Back buffer Vec<u32>, `invalidate(rect)` unions one damage rect per
lap, layers compose bottom-to-top: wallpaper row-copy → window shadows
→ windows (rounded, scrim-faded during the anim) → **taskbar + tray**
→ start card. Cursor drawn on top, never composed; idle = zero fb
traffic. The Win11 additions keep the contract:

* `acrylic_round_clip(back, wall, r, rect, rad, tint, a)` — blends the
  **cached wallpaper rows** under the rounded rect with the tint at
  alpha a (integer blend, no f32): taskbar/card read the backdrop, not
  a flat fill.
* `chip_round_clip(back, r, rect, rad, tint, a)` — the same clip shape
  but blending over the **already-composed back buffer** (uses
  `read_pixel`): the hover lift / active tint layers sit on top of the
  surface's own acrylic instead of sampling the wallpaper.
* `fill_ring` — true annulus (tray battery, glass magnifier).
* `window_shadow(back, wall, r, wr, scale)` — 4 rings × 3 px around the
  rect (`SHADOW_A=[110,70,40,22]` inner→outer), alpha scaled by the
  anim progress; drawn *before* the surface so nothing shadows the
  chrome itself.
* `scrim_rect_clip` — the open-animation fade: blends the wallpaper
  back over the surface at `256−p` (the "materialize from glass" cue).
* **Hover invalidation**: every pointer move damages the full rect of
  every hovered widget at the old AND new cursor position
  (`hover_damage`: bar cell, full 168px tray, start tiles + power
  button, titlebar buttons). Hover chips paint inside the compose clip,
  so narrow cursor-following boxes would leave seams and stale tail
  patches on wide widgets — full-widget damage keeps hover flips
  single-present and exact.
* **Cursor = steady-on-screen (M15 rework)**: the sprite no longer gets
  a blanket undraw at every lap start (that left it invisible for a
  whole drain+present lap — harmless when laps were rare, a visible
  flicker under any message flood such as the M15 canvas stream).
  `pointer` undoes the save-under at the OLD spot before moving
  (`undraw_cursor` reads `mx/my` — call it first), `steady_cursor`
  undoes it only when the pending damage rect intersects the sprite
  zone, and `draw_cursor` re-saves the under-bg only when the sprite
  wasn't already drawn (`fresh = !mouse_drawn`) — re-reading pixels
  while the sprite itself is on the fb leaks the ink into `mouse_buf`
  and the next undraw ghost-replants a full 60-px arrow (the tablet
  pixel test rebuilt the ghost from screendump pixel histograms — it
  chained across moves and never erased itself).

## Windows, focus, Win11 chrome

Same rules, worth keeping:

* **Window = rect + title + text lines**, content renders statelessly,
  wraps at content width, scrollback 200. One unified chrome (title
  text left, flat − / □ / X buttons right) — no client-drawn strips;
  the window owns the whole surface.
* **Focus follows clicks**; new windows don't steal it; raise = top of
  z; **taskbar client buttons stay id-ordered** (not Vec z-order —
  `focus_window` reorders the Vec; tests and `click()` both walk id
  ASC, coordinates are contractual).
* **Drag** from titlebar (damage = old ∪ new), **resize** via the 16px
  bottom-right grip (min 200x120), serial logs `moved to` / `resized`.
* **Double-click titlebar** toggles max (25-tick window),
  `[winsrv] win N toggled`.
* **Close** = titlebar X (hover paints `CLOSE_HOVER (196,44,30)`):
  EOFs pending client reads, drops the client button. Serial:
  `[winsrv] win N closed`.
* Console rules (`push_text`): writes continue the open line, `\n`
  breaks; MAX_LINE 4096 + SCROLLBACK 200 both capped.

## App canvas (M15 — per-window graphics mode)

A client switches its **own** window from text lines into graphics
(pixels) with three additive ABI ops to `PORT_WINSRV`
(`libs/greenlime-abi/src/win.rs`):

* `WIN_BLIT_SETUP (10)`: `arg0` = window id, `arg1` = `w | h<<32`. Replies
  `REPLY_OK` with the **clamped** dims packed the same way (a canvas is
  clamped DOWN to the content viewport — ask "as big as my window" and
  trust the reply), or `REPLY_ERR` (not owner / no fb / over budget).
  The canvas starts as the window body color.
* `WIN_BLIT (11)`: `arg0` = window id, `arg1` = **first pixel index** of
  the run (row-major, `row * cw` — a raw row number is the classic
  off-by-mapping bug: everything lands on canvas rows 0..1 and damage
  rects collapse to two screen rows), payload = RGB triples (3 B/px),
  same run shape as `WIN_WALL_ROWS`. Fire-and-forget; owner-checked,
  bounds-checked (silent drop).
* `WIN_BLIT_END (12)`: frees the canvas and repaints the content viewport
  in text mode; the `REPLY_OK` reply is the client's sync point.

Semantics & budget: canvas = opaque XRGB anchored at the content
viewport's top-left, 1:1 (never scaled), indistinguishable from a themed
window (frame/shadow/taskbar unchanged). While a canvas exists it
REPLACES the text lines; keys still ring-buffer so the console phase
replays them. Total canvas pixels across all windows are budget-capped
at `CANVAS_BUDGET_PX = 1<<20` (4 MiB of the 16 MiB kernel heap, shared
with the 4 MiB back buffer + wallpaper cache); SETUP over budget or a
failed `try_reserve_exact` answers `REPLY_ERR` — never OOM inside the
compositor. Freed per window on END and on window destroy (budget
credit returned). Canvas pixels keep every channel < 239 (cursor
uniqueness — the demo clamps defensively, same rule as the wallpaper
task). Damage per run = the run's row strip clipped to the viewport;
blit into the back buffer via the M13 SSE2 span copy.

The demo's 40-frame animation (`userland/demo`: `canvas_intro`) is the
reference client: Park-paced (10 ticks/frame — ~7 fps release, ~1 fps
on the debug build under TCG), only dirty rows streamed (ball band +
wave band), zero heap. Proof strings on serial:
`[winsrv] canvas: win 2 512x256 (owner 5)`, `[demo] m15: canvas open
512x256 live`, per-frame `[demo] m15: frame N ball X,Y`,
`[winsrv] canvas: win 2 released (N msgs)`,
`[demo] m15: canvas released, text mode`.

## Interactive stdin (line discipline)

Unchanged, test-facing: set-1 scancodes → ASCII
(`winsrv::scancode_ascii`), canonical terminal mode (echo + backspace +
Enter-commit), blocking reads via dev's WIN_READ relay. **While the
start card is open, typing filters the card's search instead**
(lowercase graphic + space; backspace edits; **Enter launches the
first filtered tile** — `menu_enter`). Esc closes the card, the left
Windows key toggles it. The search query is the sliced buffer
`&search[..search_len]` — passing the raw 32-byte array hides every
tile (repro'd + fixed in M12, still true).

## Proved by test (not by reading)

All re-baselined for the v1.1 Win11 layout (one QEMU at a time —
disk.img is the lock):

* `scripts/qmp_desktop_test.py` — start button **(552,776)** opens the
  card: **exact `LIME (170,232,64)` underline ≥40 px inside
  (540,786)-(568,796)** (the 16px pill paints ~44); opaque search
  field `SEARCH_BG (44,46,52)` ≥7000 px in (500,460)-(800,488)
  (paints 8400); demo client button azure strip `(122,180,250)` ≥60 px
  in (716,762)-(742,774) (paints 90); Settings tile **(707,578)**
  opens the Settings window (card closes: start underline → 0,
  Settings pin underline → lime); **Settings titlebar `TITLE
  (43,43,43)` ≥5000 px in (436,176)-(742,198)**; click-away at
  (1000,550) leaves the window open.
* `scripts/qmp_multitest.py` — multi-window proof on the new chrome
  (start (552,776) → Terminal tile (573,578), demo button **(728,776)**
  refocus, dark `TITLE (43,43,43)` / `TITLE_UNFOC (37,37,38)` pixel
  asserts, drag/resize/close, X at (1164,95)).
* `scripts/qmp_tablet_test.py` — splash band + tablet abs clicks (tl/
  br/mid corners exact), start button click opens the card (lime
  underline ≥40), dismissed = 0.
* `scripts/qmp_m15_test.py` — the app-canvas lifecycle: SETUP → 40
  Park-paced frames → END (synced), screendump color asserts inside the
  demo window's content box (lime ball ≥250 px, azure wave ≥600 px,
  zero ≥239-px blobs), two in-flight samples ≥6 frames apart with ≥500
  px differing (the animation really moves), motion truth from serial
  frame markers, then text mode + click/type + X-close residuals.
  Then the **M15.1 respawn phase** (checks 15–21): after the X-close,
  start card → Demo tile respawns the exited demo — `[winsrv] launch
  Demo (respawned task 5)`, a fresh canvas again owned by 5, its own
  40 frames, X-close of the new window (win 3), second EOF + second
  reap. Its markers judge only lines that arrive after a byte-offset
  snapshot, since the boot run's identical-looking lines would
  otherwise fake passes. 21/21 on the release ISO.
* `scripts/qmp_type_test.py` — start (552,776) → Terminal tile
  (573,578) → `[winsrv] launch Terminal`; types `echo greenlime works`
  + `abd`/backspace/`c`; asserts mirror + screenshots.
* `scripts/qmp_net_shell.py` — start → Terminal tile on the RELEASE
  ISO; `ping google.com` + `fetch example.com` (HTTP/1.1 200, stream
  end + User-Agent proof in the pcap).
* `scripts/qmp_disk_test.py` — M14 phase 1 (echo>file / cat / mkdir /
  ls / rm + rm-refusal) and phase 2 **on the same disk.img after a
  full reboot** (fresh image first: `rm dist/disk.img && make disk`).
* `scripts/qmp_files_test.py` — M16 explorer flow on its OWN fresh
  32 MiB volume (`dist/files-test-disk.img`, dd+mformat+mcopy of
  `testdata/disk` — the human's `dist/disk.img` is never touched):
  boot creates the 4 places (`[vfs] places: 4 new`), Files pin → root
  (`items=4`), Documents browsed empty, the file is typed
  into the Terminal (`echo hi > /disk/Documents/note.txt`), re-browse →
  `items=1`, exact-pixel selection asserts (cell wash `(62,75,39)` at
  (451,206), Documents active row `(66,80,46)` at (298,209)), Delete
  chip → trash proofs + `items=0`, bin view (bin row lime even inside
  the bin — the M16 QA fix), Restore chip → file back home, Del-key
  trash round, Empty chip → `[vfs] bin emptied (1 items)` +
  `[files] bin empty n=1` + bin `items=0`. Counted serial checks +
  `RESULT: PASS` line; screendump pixel probes ride the same helpers
  as the desktop test.
* `scripts/qmp_net_test.py` / `qmp_smp_test.py` / `qmp_input_test.py`
  — ring-3 net machine proof, DHCP+ICMP, SMP, and the cursor/keyboard
  regressions (cursor = the only ≥239 blob, moves exactly).
* B3 suite: `runsuite_b3.sh` (desktop, multitest, smp, net, type) —
  serial mirror greps the `[m13]`/`[b3]` proofs; 0-warning kernel
  build.
* 1h fuzz soak on the release ISO: `RESULT: PASS — syscall fuzz 1h
  clean`, 0 faults, 0 panics, 20,758,528 iters.

## Performance notes (why it can feel slow)

* **Debug + TCG is ~50x slower than native.** Debug Rust (no inlining,
  fat frames, bounds checks) under QEMU software emulation — big
  acrylic compose laps take ~1 s there; **one lap can outlast the whole
  120 ms animation**, which is exactly why `ANIM_TAIL` exists. `make
  run-release` (opt-level z) is the verification target for anything
  timing-sensitive — frames compose in ~10 ms there.
* **Debug wallpaper decode takes ~12 s** (~24 ticks: PNG decode +
  stream), streaming 1502 messages; **release < 1 s**. Test scripts
  wait for `[shell] interactive ready` **+ 20 s settle** before the
  first click, or the wallpaper may still be streaming under the first
  screenshot.
* **Serial is 115200 baud (~11 KB/s)** — boot log floors it.
* **Wallpaper streaming is bandwidth-shaped**: 1280×800 = 1502 messages
  of ~2 KiB through the frozen IPC gate. Rows are cached once;
  composing damage never re-enters the streaming path. Acrylic helpers
  read that cache, not the framebuffer, so a hover repaint costs a few
  blended rows of wall + tint, not a decode.
* Damage/idle contract unchanged: one union rect per change, zero
  traffic when nothing happens.

## Gotchas met building v1.0–v1.1 (read before touching input/compositor/sched)

* **No SSE: per-pixel `f32` in compose is a time bomb.** Target has only
  `fxsr`/`x87`; every float op is a soft-float call. All compose paths
  are integer (`acrylic_round_clip`/`chip_round_clip`/`fill_round`/
  shadows included).
  *Update (v1.0.1 / M13): SSE2 is now legal for compose — but ONLY in the
  shipped shape. XMM is per-task state (FXSAVE/FXRSTOR in every context
  switch), userland builds hard-float (M15.1: the SSE-baked target spec
  `targets/x86_64-greenlime-sse.json`; pre-M15.1: RUSTFLAGS
  `+sse,+sse2,-soft-float` on the soft-float spec — that CLI-vs-ABI fight
  is what produced the rust#162235 warning family), and kernel SSE lives
  only in
  hand-written asm kernels (`arch/x86_64/simd.rs`) called from winsrv's
  task context (`copy_span` in the wallpaper blit, M13). Kernel Rust code
  stays integer-only (no `+sse`, no intrinsics — the `-sse,+soft-float`
  target spec rejects `target_feature`, rust#117938), so IRQ handlers can
  still never touch XMM. Never widen this; the blend kernels must stay
  bit-exact vs `blend_px` (compositor pixel contracts count on it).*
* **One compose lap can be slower than an animation → `ANIM_TAIL`.**
  Repro: Files window froze mid-fade (`p≈21`) for the remaining boot on
  the debug ISO; the clock/hover repaints updated other regions so the
  compositor looked alive while the ghost never resolved. The tail
  (extra invalidation past expiry) guarantees a post-deadline crisp
  lap. Any future anim must go through the same tail discipline.
* **Pass the SLICED search query** `&self.search[..search_len]` to
  renderers/filters — the raw `[u8;32]` buffer reads as a 32-NUL
  "query": tiles vanish while the placeholder looks fine. Fuzz-injected
  control bytes fall back to the "Search" placeholder (no tofu boxes on
  the field).
* **Client buttons are id ASC, never Vec z-order** — `focus_window`
  reorders the window Vec; both `click()` and the draw walk map
  `(i−4) → id` with TERM_ID/FILES_ID/SETTINGS_ID skipped, so the demo
  button never moves when the terminal focuses.
* **PNG decode in ring 3**: `zune-png` with `ZCursor`,
  `use_unsafe(false)`, `decode().to_rgb3()` — one exact-size Vec, no
  intermediate sample copies (the naive samples-copy chain OOM'd the
  16 MiB userspace heap: the lean rewrite + the `output + 3·wh ≤ 13 MB`
  guard + polite `exit 2` are the fix). JPEG path (zune-jpeg, RGB
  colorspace) unchanged.
* **Kernel stacks: 64 KiB per task (was 32).** ABI v6 messages are
  ~2 KiB; at `-O0` every frame that stages one costs it. Found live via
  `rip=0x0` fault loops; the fault-path dump (32 stack words + task
  table) is permanent. Never shrink kernel stacks without re-running
  the debug B3 suite.
* **Wallpaper decode lives in ring 3 on purpose** (trust boundary — a
  PNG bug kills task 8, not the kernel). **Heap budget rule**: decode
  must fit 16 MiB — drop a ≤1600×900 wallpaper first (see asset note
  above); user stack = 128 KiB for the -O0 decode frames.
* **QEMU pointer pick**: `run-qemu.sh` promotes the tablet with
  `activate_tablet.py` (`mouse_set`), else PS/2 wins and the guest
  cursor dead-ends in relative grab mode. Test scripts inject **rel**
  events in ≤600-step chunks for the PS/2 pointer or attach
  `virtio-tablet-pci` when they need absolute clicks.
* **Damage = one union rect**, fine at this scale; **text windows
  only**; subset font with lazy full-font loader still filed under
  performance.
* **Compose may write outside the damage rect — blit must not**; back
  buffer owns the pixels (Vec must outlive its view).
* **Boot focus policy**: WIN_CREATE never steals focus (demo boot
  window stays unfocused so the type test's blind typing lands in the
  shell console).
* **Cursor vs everything bright**: cursor = 240,240,240; every painted
  color keeps at least one channel <239 on purpose (`WHITE` b=242 has
  r=232, `BALL_WHITE` b=246 still has r=238) — or the test "finds"
  text as a cursor. The wallpaper task clamps its own pixels to ≤238
  too. M15 canvas clients clamp likewise (the demo clamps r down to
  238 defensively — a client that paints all-239+ breaks the tablet
  test, not just its own look).
* **M15 canvas payload = first-pixel index, not the row number.**
  `WIN_BLIT`'s `arg1` is `row * cw` (row-major over the whole canvas).
  Passing the raw row lands every run on canvas rows 0..1: colors on
  screen collapse (azure static, lime invisible) with all the damage
  crammed into two screen rows — caught by the m15 test's screendump
  counts, fixed by sending `y * cw`.
* **Cursor save-under discipline (M15 rework)**: undraw must happen
  before the position changes (`pointer`), before any blit that could
  overlap the zone (`steady_cursor`), and the re-save in `draw_cursor`
  must be skipped while the sprite is already drawn — a poisoned
  `mouse_buf` (containing the sprite's own ink) gunks a full 60-px
  ghost arrow at every future undraw, permanently. See the compositor
  section above.
* **Tablet px↔device truncation ±2px** — resize asserts tolerate it.
* **Icons are id-ordered, never z-ordered** (else buttons shuffle and
  break every coordinate test).
