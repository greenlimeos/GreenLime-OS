# GreenLime OS 🟢🍋

A from-scratch operating system in Rust — custom x86-64 hybrid kernel,
composited GUI desktop (LimeDE), IPC, ring-3 userland, SMP, and a ring-3
network stack. Long-term goal: compete with Windows 11.

![GreenLime OS v1.2.0 desktop in QEMU (1280×800): charcoal glass, the Demo
client window, and the Windows 11-style taskbar — lime start button,
Files/Terminal/Settings pins, one button per running client, RTC clock
tray](OS_screenshot.png)

> Status: **v1.2.0 ✓** — the 1.0 line (made by GLM 5.3) brought
> SMP, ring-3 networking (DHCP/ICMP/DNS/TCP) and the Fluent-dark desktop:
> wallpaper streamed by a userspace service, Files app, rounded acrylic
> windows with soft shadows and animations.
> v1.1 adds M13: real SSE2 for userland (per-task FXSAVE/FXRSTOR across
> every context switch) and a bit-exact SSE2 compositor — M14: a
> writable FAT32 (persisted writes survive full reboots) — Limine
> 12.9.4 — and a full **LimeDE rework** to a Windows-11-style charcoal
> glass desktop: a bottom 48px taskbar with a **centered cluster** (the
> lime start button, pinned tiles, one button per running client, and
> the cluster re-centers as windows open/close), a floating acrylic
> **start card** above the bar with live-search pinned tiles
> (**Files / Terminal / Settings / Demo** — the Demo tile relaunches
> the demo even after it exited, M15.1), a right two-line RTC clock
> tray, azure Files folder glyphs and a brand **lime** accent. Boots
> via Limine into the dark glass desktop over a `wallpaper.png` you can
> simply replace, with draggable/maximizable rounded windows, Files
> explorer and DHCP+ICMP+DNS+TCP networking. M15 adds an **app
> graphics API**: clients flip their own window into graphics mode
> (`WIN_BLIT_*`: owner-checked, budget-capped per-client canvases
> streamed as RGB runs) — the demo ships a 40-frame animated canvas
> (ball on a gradient + ridge wave) and the compositor keeps the mouse
> cursor steady on screen under any message flood. **M15.1** makes the
> start card's Demo tile a real launcher — demo alive → focus (never
> duplicates), demo exited → respawn from the baked-in ELF via a
> task-slot reap (the demo gets its boot task id right back) — and
> drops every userland build warning to zero via an SSE-baked JSON
> target spec (`targets/x86_64-greenlime-sse.json`). **v1.2 opens with
> M16**: the Files app is reworked into a real Windows-Explorer-style
> explorer — a sidebar of disk-backed places (Desktop / Documents /
> Downloads / Recycle Bin), a breadcrumb + Back chip, an icon grid,
> footer action chips (Delete / Restore / Empty), **real FAT32 long
> file names** (name truth on the disk byte-exactly, `(ssss)~N` 8.3
> aliases underneath), and a **persistent Recycle Bin** — trashed
> files keep an `.orig` sidecar recording their origin and survive
> full reboots. **M18** adds right-click context menus and Notepad, and
> **M20** closes the loop on networking: `fetch https://…` is REAL
> modern HTTPS — TLS 1.3 (rustls 0.23 + RustCrypto provider, vendored)
> with webpki root-store chain validation, kernel-timed certificate
> checks (`Rand`/`Now` syscalls, ABI v7), and scheme-gated redirects
> (http → https chases; the way down is refused).

> 🤖 **Made by GLM 5.3:** every line of this OS — kernel, servers,
> userland, docs — was written by **GLM 5.3** (the AI model) through
> [OpenCode](https://github.com/sst/opencode), with a human driving
> architecture, running every milestone in QEMU, and reviewing each
> change before it landed.

## Contents

- [Boot it in QEMU](#boot-it-in-qemu)
- [Using the desktop](#using-the-desktop)
- [Architecture](#architecture)
- [Kernel](#kernel)
- [System calls](#system-calls)
- [IPC](#ipc)
- [Servers](#servers)
- [Drivers](#drivers)
- [Filesystem](#filesystem)
- [Userland](#userland)
- [LimeDE desktop](#limede-desktop)
- [Fonts](#fonts)
- [Build system & scripts](#build-system--scripts)
- [Test suite](#test-suite)
- [Requirements](#requirements)
- [Platform constraints](#platform-constraints)
- [Security](#security)
- [Roadmap](#roadmap)
- [License](#license)

## Boot it in QEMU

This OS is made on **Windows 11**. You build and run it in the **WSL2**
terminal (Ubuntu runs *inside* WSL on your Windows machine — PowerShell
can't build this).

```bash
# 1. On Windows 11, open the WSL terminal (search "Ubuntu" in Start),
#    then download the source code:
git clone https://github.com/greenlimeos/GreenLime-OS.git
cd GreenLime-OS

# 2. One-time setup — installs everything automatically in one command
#    (the EXACT pinned Rust toolchain — nightly-2026-10-25 == 1.101.0-nightly,
#    never "latest" — plus QEMU, xorriso, mtools, Limine):
bash scripts/setup-wsl.sh

# 3. Build + make the ISO + boot it in QEMU (one command):
make run-release
```

What happens: everything compiles, `dist/GreenLime-release.iso` is baked,
and a QEMU window opens. You'll see the Limine menu → kernel boot log on
serial → the LimeDE desktop with your wallpaper streamed in by the
userspace service. Your host cursor stays visible and the guest cursor
tracks it 1:1 (virtio-tablet seamless pointer, no grab).

When you're done, close the QEMU window (or `Ctrl+C` the terminal) and the
full serial log prints at the end for copy-paste bug reports.

### Boot modes

| Command | What it does |
|---|---|
| `make run-release` | Optimized build + BIOS boot + GUI (the fast, recommended one) |
| `make run` | Debug build + BIOS boot + GUI (slower, better panic messages) |
| `make run-uefi` | UEFI boot with OVMF (if installed) |
| `make run-nographic` | No window, serial only. Exit with `Ctrl+A, X` |
| `make debug` | GDB stub on `:1234`, CPU frozen — attach with `gdb-multiarch` from a 2nd terminal |

### VirtualBox note

You can also boot the ISO in VirtualBox ("Other (64-bit)", BIOS boot) —
the kernel CPUID-gates SMEP/SMAP so it no longer triple-faults on
VirtualBox's CPU profile (proved by `scripts/qmp_vboxsim_test.py`).
Expect fewer built-ins there: VirtualBox exposes no virtio devices, so
**no `/disk` storage and no networking** (`net` fails closed,
`[net] no NIC claimed`). The desktop, Files (ramfs), terminal, and the
PS/2 pointer all work. For troubleshooting, enable the serial COM1 port
in the VM settings (Port Mode: *Raw File*, e.g. `C:\...\serial.log`) —
the serial mirror is the kernel's ground truth for what happened.

## Using the desktop

- **Mouse:** point, click, drag window titlebars (or the 16px grip), or
  double-click a titlebar to maximize. No pointer grab, no `Ctrl+Alt+G` —
  it just works. Hovering lifts and highlights the taskbar cells.
- **Taskbar:** full-width 48px bottom bar with a **centered cluster** of
  40px cells — the lime **start button**, the **Files / Terminal /
  Settings** pins (azure folder / lime `>_` glyphs), then one button
  per running client (mini azure window). Running/open state shows a
  lime underline under the cell. Clicking a client button refocuses
  its window; the cluster re-centers as clients open and close.
- **Start card:** click the start button (or the left Windows key) →
  floating acrylic card above the bar with a search field and pinned
  tiles **Files / Terminal / Settings / Demo**. Typing filters the
  tiles live; **Demo** focuses the demo window while it is alive and
  relaunches it from the baked-in ELF once the demo task exited
  (M15.1). Clicks outside the card dismiss it. The footer's red
  **power button** now powers the whole machine off for real — ACPI S5
  (QEMU's PM1a at I/O 0x604), so the VM closes cleanly
  (`[winsrv] shutdown`).
- **Tray:** right zone with the real RTC two-line clock (`HH:MM` over
  `YYYY-MM-DD`, day rollover handled) — clock only by design; hover
  shows a highlight, nothing opens.
- **Terminal:** launch from its start-card tile — it never opens at
  boot. Builtins: `help echo cat ps kill ls exit`, plus ring-3
  network tools `ping` and `fetch` through the IPC net broker —
  `fetch https://…` is REAL modern HTTPS (M20): TLS 1.3 with webpki
  root-store chain validation (`fetch http://…`/bare hosts stay plain
  HTTP; redirects chase http→https but never the way down). Its ✕
  closes it; reopening restores buffered text.
- **Files:** launch from its start-card tile — a real explorer (M16):
  a sidebar with the four disk-backed **places** (Desktop, Documents,
  Downloads, Recycle Bin), a `< Back` chip + path breadcrumb, an icon
  grid (dirs first, folders enter on double-click, files select on
  click, the **Del key** or the footer **Delete** chip moves them to
  the Recycle Bin), and a footer with `N items` + the selection's
  size. In the Recycle Bin, **Restore** puts a file back where it came
  from and **Empty** drains the bin — trash state lives on the disk
  (`.orig` sidecars), so it survives full reboots. **Right-click**
  (v1.2.1/M18): over the grid or the *bare desktop* opens a context
  menu — **New text file** (creates `New file N.txt`, first free name,
  in the browsed dir or `/disk/Desktop`, then opens it in **Notepad**)
  and **Refresh** (Files re-lists; desktop recomposites everything).
  A ramfs view politely refuses new files with an auto-browse to
  `/disk`.
- **Notepad (.txt, new in v1.2.1/M18):** double-clicking any `.txt` on
  the disk — or any New-text-file gesture — binds the file to the
  built-in editor: a fresh ring-3 task with exactly **CAP_DISK**
  (save-only powers; no net, no kill), spawned only on demand, never at
  boot. The window loads the file (4 KiB view cap), and the command
  line is `save | append <text> | clear | quit` — `quit` and the X
  button both save-when-dirty. Terminal law:
  `[notepad] opened <path>` / `[notepad] saved N bytes -> <path>`.
- **Apps (.gle, v1.2/M17 + v2/M19):** a `.gle` file is a self-contained
  app — sticker (name/version/author), 28x28 icon, and the compiled
  program in one flat container, CRC-checked by the kernel. **v2** adds
  bundled **assets**: the kernel maps them read-only into the app's
  address space (one descriptor page + the blobs, the
  `greenlime_abi::app` window contract) and the app reads them as
  plain memory — no installer, nothing copied. Files double-clicks
  (or `exec /disk/...`) run it: a fresh ring-3 task in its own
  address space, its own window on the taskbar, capped at
  USER_DEFAULT powers — never derived from the sticker. Port your app
  by copying `userland/hello` (the in-tree template), building for the
  userland target, and packing with the zero-dependency
  `scripts/gle-pack.py` (list `"assets"` in the manifest for a v2 box,
  leave them out for plain v1). The full dev guide (3 steps, format
  tables, kernel law, caps, limits) is **`docs/GLE.md`**.
- **Settings:** opens from its start-card tile as a floating acrylic
  info card (430,170,420,300) — drag and ✕ like any window.
- **Windows:** focus follows clicks, `WIN_CREATE` never steals focus,
  ✕ closes, rounded corners + soft shadows throughout. Windows and the
  start card open with a 120 ms fade+rise.
- **Wallpaper:** drop in your own `assets/wallpapers/wallpaper.png`
  (≤1600×900) — `build.rs` picks `.png` → `.jpeg` → `.jpg` at build
  time and the userspace service streams it in; no code changes (the
  repo ships a generated blue-wave default in the same spirit).
- **Icons:** baked from [icons8](https://icons8.com) ("fluency" style) —
  PNGs under `assets/icons-src/`, raw RGBA blobs under `assets/icons/`
  (rebuild with `python3 scripts/bake-icons.py`). Icons by icons8.
  Since v1.2.1 the **Settings** glyph is the fluency `gear` cog (the
  old `settings` art read Windows-logo adjacent — swap chosen with
  `scripts/probe-gear.py`'s shape maps), and the **power button** is
  original in-tree art via `scripts/bake-icons-original.py` — chrome
  must never be ambiguous about its origin.

## Architecture

```
Limine → kernel (ring 0) → servers (kernel tasks) → userland (ring 3) → LimeDE
.clients ──► desktop over IPC: windows, terminal, files, net broker, wallpaper
```

Hybrid kernel: microkernel-style keyed IPC + servers for structure,
monolith-style speed (servers start as kernel tasks and can migrate to
userspace later). Task map: kernel tasks `0=vfs, 1=dev, 2=winsrv, 5=net`; boot user tasks
`3=init, 4=shell` (fuzz 6, wallpaper 7); the Terminal / Files / Notepad,
the demo and third-party `.gle` apps spawn on demand at recycled slots
(max 12).
`kernel/` may use `unsafe` + privileged instructions; `libs/*` +
`userland/*` are safe-only and reach the kernel only through
syscalls/IPC. Kernel memory is never mapped into user page tables (SMP
rings keep their IRQ-stack PTEs mapped in every user CR3).

## Kernel

- **Boot** (`boot.rs`): Limine requests for framebuffer (1280×800×32),
  memory map, HHDM, stack. Paging comes up with `cr3` level 4.
- **SMP**: AP startup via Limine MP (`arch/x86_64/smp.rs`), LAPIC +
  IPIs, per-CPU TSS (own RSP0 + double-fault IST), per-CPU LAPIC timers
  (PIT-calibrated) with safe preemption (EOI-first, need_resched flag
  consumed at a safe point).
- **arch/x86_64**: GDT (kcode/kdata/ucode/udata + per-CPU TSS), IDT
  (breakpoint/UD/PF/GP/DF + timer/keyboard IRQs), masked PIC, PIT @100Hz,
  SMEP + SMAP enabled **when the CPU exposes them** (CPUID.(7:0)-
  gated; VirtualBox CPU profiles hide both — an unconditional CR4 write
  would #GP there → triple fault; found live and fixed, see
  `scripts/qmp_vboxsim_test.py`), `SYSCALL`/`SYSRET` gate with IF masked.
- **Memory**: PMM frame allocator (4 KiB frames, ~126k free on 512M),
  16 MiB static heap (`LockedHeap`), VMM with per-task user tables,
  W^X enforced (ELF refuses W+X), KASLR seed (per-task stack jitter).
  1 GiB pages unsupported (detected, refused).
- **Scheduler**: cooperative core, task states Ready/Blocked/Dead,
  block/wake on IPC ports, per-CPU run state via GS anchoring;
  `ps` snapshots.
- **Syscall gate**: SystemV trampoline, runs on the caller's private
  64 KiB kernel stack. Unknown numbers and rejected calls return
  `u64::MAX`. User buffers are range-checked *and page-probed*, copied
  under a STAC/SMAP window, then used from the kernel copy.
- **ELF loader** (`userspace/elf.rs`): static `ET_EXEC` x86-64 only,
  maps each `PT_LOAD` into a fresh address space, enforces user-half,
  image-bounds, and non-empty segment set. Images are embedded at build
  time via `build.rs`.
- **Console**: serial COM1 + framebuffer TTF console (framebuffer handed
  to winsrv once the desktop paints). Supervisor faults print a 32-word
  stack dump + task table before halting (forensics by design).

## System calls

ABI v6, frozen numbers (add, never reuse) — `libs/greenlime-abi/src/syscall.rs`:

| # | Name | Args | Returns |
|---|---|---|---|
| 0 | `Log` | ptr, len (≤4096) | bytes logged / `MAX` |
| 1 | `Exit` | code | never returns |
| 2 | `Yield` | — | 0 |
| 3 | `IpcSend` | port, msg ptr | 0 / `MAX` |
| 4 | `IpcRecv` | port (must be own), buf ptr | 0 / `MAX` |
| 5 | `Kill` | pid | 0 / error |
| 6 | `GetPid` | — | pid |
| 7 | `Park` | — | sleeps to the next tick (fuzzer pacing) |

## IPC

Port-based, fixed-size messages (`libs/greenlime-abi/src/ipc.rs`):

- **24 ports**: mailboxes `0–7` (task N owns port N), services
  `PORT_VFS=16`, `PORT_DEV=17`, `PORT_NET=18`, `PORT_WINSRV=19`.
  Queue depth 16 per port.
- **`Message`**: `kind u32`, `sender u32` (**kernel-stamped, unforgeable**),
  `arg0/arg1 u64`, `len u32`, `payload [u8; 2048]` (`MAX_PAYLOAD=2048`).
- Semantics: send blocks on full, recv blocks on empty; `try_recv` polling
  so one slow client can't wedge a server; every mutation wakes port
  waiters (spurious wakeups are harmless — waiters retry).

## Servers

All loop forever on their service port, replying to the kernel-stamped sender.

- **vfs** — ramfs (`/motd.txt`, `/hello.txt`), FAT32 `/disk`, live `/ps`
  task list. `OPEN/READ/WRITE/CLOSE` by owner-checked handle, `READDIR`,
  `PING`.
- **dev** — `serial0` (unified console) and `term0` (GUI terminal) as
  files. term0 writes forward to winsrv; term0 reads relay `WIN_READ`
  and block until a full line is typed.
- **winsrv** — desktop compositor (see LimeDE below). Protocol:
  `WIN_WRITE`, `WIN_READ`, `WIN_CREATE`, `WIN_CLIENT_*`, `WIN_DESTROY`,
  and the wallpaper service: `WIN_WALL_SETUP/ROWS/DONE` — task 8
  streams decoded PNG/JPEG rows through the IPC gate and the kernel
  converts + blits (it never parses image bytes).
- **net** — ABI net broker on `PORT_NET` (tagged protocol over the same
  frozen IPC gate): NET_OPEN/SENDTO/RESOLVE/CLOSE for UDP,
  NET_CONNECT/SEND/RECV/CLOSE for TCP (polls to ESTABLISHED, streaming
  in up-to-2048-byte chunks, peer FIN → NET_EOF), NET_PING. smoltcp runs
  only in the kernel net task; leaky sockets, forged senders and
  duplicate ports all get refused.

## Drivers

- **serial**: COM1, unified console + log mirror.
- **input**: PS/2 keyboard (scancode ring, IRQ1) + PS/2 mouse bytes
  (drained and discarded whenever the tablet is present).
- **virtio-tablet** (modern MMIO caps transport): event queue 64 bufs,
  absolute `0..32767` → exact screen pixels, polled with buffer recycling.
  This is what makes the cursor track 1:1.
- **virtio-blk**: queue 256, up to 65536 sectors of 512 B, block
  read/write for the FAT32 disk.
- **ahci** (v1.2, M19): a real SATA disk on the ICH9 (or any AHCI HBA at
  PCI BAR5) is claimed by `kernel/src/drivers/ahci.rs` — the driver
  takes over a firmware-owned port with the full engine stop dance,
  COMRESET, then IDENTIFY (LBA48 capacity from identify words 100/204)
  and serves block read/write via READ/WRITE DMA EXT (bounce-frame
  polling, no IRQs yet). `storage` prefers AHCI; virtio-blk stays the
  fallback when no SATA drive answers. Proof receipt:
  `scripts/qemu-ahci-test.sh` — SATA-only identify/claim/mount,
  AHCI-over-virtio preference, and empty-controller neutrality all pass.
- **virtio-net**: legacy-transport devices with MSI-X IRQ-driven RX/TX
  wake (entry 0 → vector 0x40, both queue vectors wired; INTx fallback
  via the q35 PIRQ router kept as backup).
- **rtc**: boot date + time and minute-of-day (feeds the tray two-line
  clock `HH:MM` / `YYYY-MM-DD`, with uptime-based day rollover).
- **pci**: full bus enumeration (host bridge, `1234:1111` display,
  virtio input/blk/net, ISA bridge, AHCI (claimed below by the ahci
  driver), SMBus).
- **fb**: VBE framebuffer driver, `Framebuffer::from_raw`, handed over
  to winsrv at `[winsrv] desktop painted`.

## Filesystem

FAT32 (`kernel/src/fs/fat32.rs`) — a **writable filesystem since M14
(v1.1)**, with **real long file names since M16**: 512-byte sectors,
BPB validation (signature, `FAT32` magic, nonzero geometry),
`.`/`..`/deleted/volume-label entries skipped, directory chains capped
at 64 KiB, file writes capped at 1 MiB, FAT-chain cycles broken by hard
bounds. **LFN (M16)**: names keep their typed case on disk — a UTF-16
entry chain above the 8.3 short entry (spec inventory: ordinals ascend
toward the short entry, the `0x40` flag on set 1 only, checksummed per
short entry), `~N` aliases fold under it for external tools, deletes
sweep the whole set; anything undecodable falls back to the uppercase
8.3 view. The write path (single vfs task, so `&mut self` is
race-free): first-fit FAT allocation with a resume cursor,
`EOC`-terminated chain building, file create/delete, `mkdir` with
`.`/`..` cluster, empty-dir-only deletes, zero-filled extends (holes
read as zeros), truncate with cluster release, and pwrite-at-offset.
Durability is simple and real: writes go straight through the polling
block driver, no cache to lose — `echo m14proof > /disk/m14.txt` then
`cat` in a fresh boot shows the file back. Single FAT mirror is kept
in sync (FAT[0]); FSInfo free-counts stay stale, which is legal. Test
disk (`dist/disk.img`, 32 MiB) is made by `scripts/make-disk.sh` via
`make disk` (delete the file to rebuild it — the script reuses an
existing image). Shell and Files app read it; the shell writes it
(admin-gated server-side — only init/shell hold the disk caps). At
boot, vfs **`ensure_places()`** creates the four explorer places
(Desktop / Documents / Downloads / Recycle Bin) on the volume when
they're missing (`[vfs] places: N new`), so Files navigation and the
bin exist for every later boot. The Recycle Bin scheme (payload +
`.orig` sidecar, name de-dup, restore-to-source, guard rails) is
detailed in `docs/STORAGE.md`.

## Userland

Ring 3, built static for the ELF loader, syscalls/IPC only:

- **init** — PID 1 (task 3): first user program, says hello, yields, exits.
- **shell** (task 4) — interactive shell: line editing with history,
  `help echo cat ps kill ls exit ping fetch rm mkdir` + `echo x > /disk/f`
  for file writes (M14), `ping <host>` / `fetch <url>` through
  the net broker — `fetch https://…` does TLS 1.3 with real webpki
  certificate validation via the `greenlime-tls` lane (M20: shell owns
  the TCP transport, the crate owns crypto+TLS+HTTP), reads via dev
  `term0`, mirrors output to serial (the
  test harness' ground truth), checks reply senders and refuses forged
  messages.
- **demo** (task 5) — second window client: opens its window, writes,
  moves, closes, says bye; also does a ring-3 DNS resolve through the
  broker. Exercises the whole window protocol.
- **fuzz** (task 7) — unprivileged syscall fuzzer, paced by `Park` (it
  sleeps between bursts so the CPUs reach idle); its heartbeats
  (`[fuzz] iters=N`) and panic handler (payload + `file:line`) are the
  soak ground truth: the 1h release-ISO soak ran clean end to end.
- **wallpaper** (task 8) — embeds `assets/wallpapers/wallpaper.png`
  (`build.rs` picks `.png` → `.jpeg` → `.jpg`, emitting
  `GREENLIME_WALLPAPER[+_FMT]`), decodes it with zune-png 0.5.2 (PNG)
  or zune-jpeg (JPEG) in its own 16 MiB heap, streams RGB rows to
  winsrv over the IPC gate. PNG decode is lean (one exact-size Vec,
  ready-to-blit conversion downscales 1600×900 → 1280×800) with a
  ~13 MB budget guard that exits politely instead of OOM-ing. The
  kernel never parses image data — that's the trust boundary.

## LimeDE desktop

The winsrv compositor (`kernel/src/servers/winsrv.rs`; palette, acrylic,
geometry and animation math documented in `docs/GUI.md`):

- **Compositor**: back buffer + cached wallpaper rows (from the streamed
  decode), single-union damage rect per lap, compose order
  desktop → window shadows → windows → taskbar (centered cluster +
  clock zone) → start card; the cursor is drawn as an
  overlay, never baked into the back buffer. Idle = zero fb traffic.
- **SSE2 compositor (v1.0.1)**: wallpaper copy, present blit and both
  blend paths run 4 px per vector through hand-written SSE2 kernels
  (`arch/x86_64/simd.rs`), bit-exact against the scalar reference
  (`blend_px`): `[m13] simd: SSE2 blend bit-exact on 20480 px`, release
  measured 7.8x faster blends (92.8x on the debug build). XMM0–15 are
  per-task state — FXSAVE/FXRSTOR in every context switch
  (`[m13] xmm: 256/256 sentinel bytes survived a task switch`) — while
  kernel Rust code stays integer-only, so IRQ handlers can never touch
  XMM and no IRQ path needs an FXSAVE.
- **Theme "LimeDE dark" (v1.1)**: charcoal glass with Windows-11
  weight: `BG (30,32,37)`, focused titlebar `(38,40,46)`, unfocused
  `(30,32,36)`, ink `FG (206,212,220)`, brand lime accent
  `(170,232,64)`, Files azure `(56,126,236)`. Every taskbar cell,
  start-card tile and window is rounded (`RAD 14/12/10`) and blends
  the wallpaper through it — acrylic layers at `PANEL_A=214`,
  `CARD_A=236`, `CHIP_A=238`) — with
  4-ring soft shadows. Windows and the start card
  open with a 120 ms fade+rise animation. No desktop watermark.
- **Detector rule**: the cursor `(240,240,240)` is the only pixel on
  screen with all three channels ≥239 (tests locate it by that); every
  painted UI color keeps one channel below the line on purpose, and the
  wallpaper service clamps its own pixels to ≤238 the same way.
- **Input pipeline**: virtio-tablet owns the pointer; PS/2 bytes are
  swallowed when the tablet exists; focus follows clicks; titlebar drag,
  double-click maximize, 16px resize grip, ✕ close; scrollback-capped
  terminal text; typing filters the start card's tiles live.
- **QEMU side**: QEMU registers its PS/2 mouse before our tablet, so a
  helper (`scripts/activate_tablet.py`, double-forked daemon) issues HMP
  `mouse_set` over a monitor socket to promote the tablet to absolute —
  no grab, 1:1 tracking. `-display gtk,show-cursor=on` keeps the host
  cursor visible (QEMU hides it in absolute mode by design).

## Fonts

`libs/greenlime-font` on top of `fontdue` (`hashbrown`, no default
features): TTF rasterization with glyph cache for the fb console and all
window text. Your font goes at `assets/fonts/default-font.ttf` (subset to
~600 glyphs via `pyftsubset`); full backup at
`assets/fonts/Roboto-full.ttf`. See `assets/fonts/README.md` for free OFL
options (DejaVu, Inter, Fira, Plex).

## Build system & scripts

- **Toolchain**: exact date-pinned nightly via `rust-toolchain.toml` —
  `nightly-2026-10-25`, i.e. rustc/cargo `1.101.0-nightly`
  (`12433c307 2026-10-24`). `scripts/setup-wsl.sh` installs exactly that
  build and verifies the compiler version before it prints done; plain
  `cargo` in this tree always compiles with it (no `+nightly` selectors —
  a channel override would bypass the pin). Target
  `x86_64-unknown-none`. Build cache lives in
  `~/.cache/greenlime-target` (not in the repo) — override with
  `make CARGO_TARGET_DIR=/path ...`.
- **ISO**: `xorriso` (BIOS El Torito + UEFI edd image) + `limine
  bios-install`. `make iso` → `dist/GreenLime.iso`,
  `make iso-release` → `dist/GreenLime-release.iso`.
- **QEMU** (`scripts/run-qemu.sh`): `-cpu qemu64,+smep,+smap`,
  `-machine q35`, `-m 512M`, `-vga std`, `-boot order=d`,
  `-device virtio-tablet-pci`, per-ISO serial logs
  (`dist/serial-<name>.log`, dumped on exit). `-smp 2` supported and
  tested; SMP is what boots by default. `make diag` collects logs.
- **Tablet helper**: `scripts/activate_tablet.py` (HMP `mouse_set`,
  detaches via double-fork, never sends `quit`).
- `scripts/make-disk.sh`, `scripts/collect-logs.sh`,
  `scripts/setup-wsl.sh`, `scripts/shots2png.sh`.

## Test suite

Ten headless QMP tests, all green, plus the fuzz soak and pointer
utilities (`python3 scripts/<test>.py <iso>`):

| test | proves |
|---|---|
| `qmp_desktop_test.py` | Win11 desktop proof: start card opens with the start-button underline, Settings window opens from its tile, click-away closes the card but leaves the window (exact-color counts) |
| `qmp_multitest.py` | 18-check multi-window proof: focus, taskbar demo-button refocus, drag, resize, close, EOF |
| `qmp_tablet_test.py` | splash band + absolute cursor corners + start card open/dismiss via the start button |
| `qmp_input_test.py` | keyboard scancodes reach the guest, cursor moves exactly |
| `qmp_type_test.py` | blind typing lands in the terminal opened from the Terminal start-card tile |
| `qmp_disk_test.py` | M14: shell writes FAT32 (`echo >`, `rm`, `mkdir`), non-empty-dir delete refused, and phase 2 re-reads the file after a full reboot on the same disk |
| `qmp_m15_test.py` | M15 + M15.1, 21 checks: the demo app's canvas animation — `WIN_BLIT_SETUP/BLIT/END` lifecycle on serial, screendump proves the lime ball + azure wave pixels inside the window (and that they move between frames), teardown returns the window to a working console — then the respawn: after X-close, the start card's Demo tile respawns the demo client on its reaped task slot (`launch Demo (respawned task 5)`, fresh `owner 5` canvas, second close/EOF/exit), with respawn-run markers judged inside a serial byte-offset window |
| `qmp_vboxsim_test.py` | boots with VirtualBox's story — `qemu64` without SMEP/SMAP AND zero virtio devices: CR4 CPUID-gating (no triple fault), net fail-closed, desktop still paints |
| `qmp_net_test.py` / `qmp_net_shell.py` | DHCP + ICMP + DNS + the ring-3 `ping`/`fetch` tools |
| `qmp_smp_test.py` | 2 cores online, IPIs answered, per-CPU state |
| `qmp_mouse_idle.py`, `verify_tablet_pointer.py`, `probe_mice.py`, `qmp_screendump.py` | pointer + utility layer |

Plus the **1h syscall-fuzz soak on the release ISO**: clean end to end,
0 faults, 0 panics. Pixel-exact color assertions;
`docs/GUI.md` lists what each test proves.

## Requirements

| Tool | Version used |
|---|---|
| QEMU | 11.1.2 (built from source into `~/.local`) |
| Rust | `nightly-2026-10-25` (= 1.101.0-nightly, exact pin in `rust-toolchain.toml`) |
| xorriso | 1.5.8.pl02 (libburnia, built from source into `~/.local`) |
| Limine | 12.9.4 (binaries in `~/limine`, via `setup-wsl.sh`) |
| Host | Windows 11 with WSL2 (Ubuntu 22.04/24.04 runs inside WSL), plus `mtools`, `git`, `curl` |
| Guest | `-m 512M`, `-smp 2`, 1280×800×32 framebuffer, ~492 MiB usable RAM |

## Platform constraints

- **Kernel stays soft-float by design** (`x86_64-unknown-none`): IRQ
  paths can never touch XMM. This constraint was never userland's: since
  M13 userland runs real SSE/SSE2 (per-task FXSAVE/FXRSTOR + SSE2
  compositor kernels), and since M15.1 it builds against the SSE-baked
  target spec `targets/x86_64-greenlime-sse.json` — zero build warnings,
  no CLI-vs-target flag fights.
- **Storage backends are AHCI first, virtio-blk second**: a SATA disk on
  the ICH9 (or any AHCI HBA at BAR5) is claimed by the ahci driver
  (`[drivers] storage: ahci ready`); with no SATA drive attached the
  controller is probed write-free and stays untouched, so virtio keeps
  the disk (suite truth). The writable FAT32 (M14: writes persist
  across full reboots) mounts over whichever backend answered.
  Story in `docs/STORAGE.md`; receipts from `scripts/qemu-ahci-test.sh`.
- No 1 GiB pages; windows are text + chrome, but any client may flip
  its own window into a budget-capped per-window graphics canvas
  (M15 `WIN_BLIT` — the demo's 40-frame animation is the proof);
  subset font.

## Security

Hardening campaigns across the 1.0 phases closed the internal AI-assisted
audit ledger GLS-001…012 (Critical down to Info — W^X, KASLR, capability
stamps on every server request, page-probed user pointers, fault
isolation, kstack budget). Two items deliberately deferred and documented
(the `Blocked → Ready` wake window; GLS-007). Full ledger in
`docs/SECURITY.md`. Hobby-grade nonetheless: no sandboxing or user model
yet — don't put secrets in it.

## Roadmap

**v1.0 starts at M10** — the 1.0 release is:

- [x] M10 SMP + network + hardening — 2 cores, DHCP/ICMP/DNS/TCP,
  W^X/KASLR/fuzz, 1h soak
- [x] M11 Fluent light + userspace wallpaper + Files app
- [x] M12 Fluent dark — charcoal acrylic faces, soft shadows,
  animations

**v1.0.1 adds M13** — SSE + fast compositor: SSE/SSE2 enabled for
userland (BSP + APs, `#XM` in the IDT), per-task FXSAVE/FXRSTOR in the
context switch (XMM survives switches and interrupts), and bit-exact
SSE2 blend/copy/blit kernels — full suite green with every contract
pixel count unchanged. Kernel SSE ships as asm kernels, not intrinsics:
the `x86_64-unknown-none` spec pairs `-sse,+soft-float` and LLVM rejects
`target_feature` on it (rust#117938), keeping IRQ paths integer-only by
construction.

**v1.1 ships M14 → M15.1** — storage, the big visual rework, app
graphics and a taskbar that launches programs:

- [x] M14 writable FAT32 — shell `echo >` / `rm` / `mkdir`; writes
  persist across full reboots (disk test 12/12 + 3/3 on the same image)
- [x] M14.5 LimeDE rework — full visual pass: charcoal glass, brand
  lime + azure, drop-in PNG wallpaper, RTC date tray; final chrome is
  the Windows-11-style centered taskbar + start card
- [x] M15 app graphics API — `WIN_BLIT_*` per-window canvases
  (owner-checked, budget-capped), the demo's 40-frame zero-heap
  animation, cursor steady under any message flood
- [x] M15.1 launcher + task-slot reap — the start card's Demo tile
  respawns the exited demo client (`[winsrv] launch Demo (respawned
  task 5)`, the demo's boot task id comes right back); freed task ids
  recycle kernel-wide; userland builds emit zero warnings on the
  SSE-baked target spec `targets/x86_64-greenlime-sse.json`

M1–M9 are the pre-1.0 groundwork (boot, memory, userspace, fonts, IPC,
storage, window server + compositor, identity hardening) — kept as
history in `docs/ROADMAP.md`.

See `docs/ROADMAP.md`. v1.0 through v1.2.0 (M20, real HTTPS) verified in QEMU.

## License

MIT — see LICENSE.
