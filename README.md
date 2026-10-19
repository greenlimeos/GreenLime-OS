# GreenLime OS 🟢🍋

A from-scratch operating system in Rust — custom x86-64 hybrid kernel,
composited GUI desktop (LimeDE), IPC, ring-3 userland, SMP, and a ring-3
network stack. Long-term goal: compete with Windows 11.

> Status: **v1.0** ✅ — the 1.0 line (made by GLM 5.3) brings SMP, ring-3
> networking (DHCP/ICMP/DNS/TCP) and the Fluent-dark desktop: wallpaper
> streamed by a userspace service, Files app, quick-settings flyout,
> rounded acrylic windows with soft shadows and animations. The earlier
> boot→GUI phases (v0.1–v0.9) are the groundwork the same tree builds on.
> Boots via Limine into a dark acrylic desktop over your own wallpaper:
> floating dock/tray pills, centered Start card with pinned tiles and
> quick-settings flyout, draggable/maximizable rounded windows, Files
> explorer, DHCP+ICMP+DNS+TCP networking.

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

# 2. One-time setup — downloads and installs everything automatically
#    (Rust nightly, QEMU, xorriso, mtools, Limine):
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

## Using the desktop

- **Mouse:** point, click, drag window titlebars (or the 16px grip), or
  double-click a titlebar to maximize. No pointer grab, no `Ctrl+Alt+G` —
  it just works.
- **Taskbar strip:** three floating pills — left **weather**, centered
  **dock** (start mark + one slot per window, focus = accent LED), right
  **tray** (Wi-Fi/speaker/battery glyphs + real RTC clock `HH:MM` +
  `Day N`).
- **Start menu:** click the start mark → 560×436 acrylic card: greeting
  header, 5 pinned tiles (Files / Terminal / Demo / Settings / About),
  search pill (type to filter, Enter launches, Esc closes). Clicks
  outside dismiss.
- **Quick settings:** click the tray → acrylic flyout with 2×2 toggles
  (Wi-Fi / Bluetooth / Night light / Battery saver) and click-to-set
  volume + brightness sliders; "All settings" opens Settings. One
  flyout at a time, outside-click dismiss.
- **Terminal:** launch from the menu (never opens at boot). Builtins:
  `help echo cat ps kill ls exit`, plus ring-3 network tools `ping` and
  `fetch` through the IPC net broker. Its ✕ closes it; reopening
  restores buffered text.
- **Files:** launch from the menu — the explorer window lists the
  filesystem with folder glyphs; click a directory to enter, `../` pops
  back.
- **Windows:** focus follows clicks, `WIN_CREATE` never steals focus,
  ✕ closes, rounded corners + soft shadows throughout. Windows, the
  menu, popups and the flyout all open with a 120 ms fade+rise.
- **Popups:** About and Settings dialogs, centered 460×210, X mark, any
  click dismisses.

## Architecture

```
Limine → kernel (ring 0) → servers (kernel tasks) → userland (ring 3) → LimeDE
.clients ──► desktop over IPC: windows, terminal, files, net broker, wallpaper
```

Hybrid kernel: microkernel-style keyed IPC + servers for structure,
monolith-style speed (servers start as kernel tasks and can migrate to
userspace later). Task map: kernel tasks `0=vfs, 1=dev, 2=winsrv, 6=net`;
user tasks `3=init, 4=shell, 5=demo, 7=fuzz, 8=wallpaper` (max 9).
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
  verified SMEP + SMAP, `SYSCALL`/`SYSRET` gate with IF masked.
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
  and the wallpaper service: `WIN_WALL_SETUP/ROWS/DONE` — task 8 streams
  decoded JPEG rows through the IPC gate and the kernel converts + blits
  (it never parses image bytes).
- **net** — ABI net broker on `PORT_NET` (tagged protocol over the same
  frozen IPC gate): NET_OPEN/SENDTO/RESOLVE/CLOSE for UDP,
  NET_CONNECT/SEND/RECV/CLOSE for TCP (polls to ESTABLISHED, streaming
  in 218-byte chunks, peer FIN → NET_EOF), NET_PING. smoltcp runs only
  in the kernel net task; leaky sockets, forged senders and duplicate
  ports all get refused.

## Drivers

- **serial**: COM1, unified console + log mirror.
- **input**: PS/2 keyboard (scancode ring, IRQ1) + PS/2 mouse bytes
  (drained and discarded whenever the tablet is present).
- **virtio-tablet** (modern MMIO caps transport): event queue 64 bufs,
  absolute `0..32767` → exact screen pixels, polled with buffer recycling.
  This is what makes the cursor track 1:1.
- **virtio-blk**: queue 256, up to 65536 sectors of 512 B, block
  read/write for the FAT32 disk.
- **virtio-net**: legacy-transport devices with MSI-X IRQ-driven RX/TX
  wake (entry 0 → vector 0x40, both queue vectors wired; INTx fallback
  via the q35 PIRQ router kept as backup).
- **rtc**: boot time + minute-of-day (feeds the tray clock, "Day N" and
  the start-card greeting).
- **pci**: full bus enumeration (host bridge, `1234:1111` display,
  virtio input/blk/net, ISA bridge, AHCI (detected), SMBus). AHCI driver
  proper is future work.
- **fb**: VBE framebuffer driver, `Framebuffer::from_raw`, handed over
  to winsrv at `[winsrv] desktop painted`.

## Filesystem

Read-only FAT32 (`kernel/src/fs/fat32.rs`): 512-byte sectors, no LFN,
8.3 uppercase compare, BPB validation (signature, `FAT32` magic, nonzero
geometry), `.`/`..`/deleted/volume-label entries skipped, directory reads
capped at 64 KiB, file reads capped at 1 MiB, FAT-chain cycles broken by
the byte cap. Test disk (`dist/disk.img`, 32 MiB) is made by
`scripts/make-disk.sh` via `make disk`. Files and shell read it.

## Userland

Ring 3, built static for the ELF loader, syscalls/IPC only:

- **init** — PID 1 (task 3): first user program, says hello, yields, exits.
- **shell** (task 4) — interactive shell: line editing with history,
  `help echo cat ps kill ls exit` + `ping <host>` / `fetch <url>` through
  the net broker, reads via dev `term0`, mirrors output to serial (the
  test harness' ground truth), checks reply senders and refuses forged
  messages.
- **demo** (task 5) — second window client: opens its window, writes,
  moves, closes, says bye; also does a ring-3 DNS resolve through the
  broker. Exercises the whole window protocol.
- **fuzz** (task 7) — unprivileged syscall fuzzer, paced by `Park` (it
  sleeps between bursts so the CPUs reach idle); its heartbeats
  (`[fuzz] iters=N`) and panic handler (payload + `file:line`) are the
  soak ground truth: the 1h release-ISO soak ran clean end to end.
- **wallpaper** (task 8) — embeds `assets/wallpapers/wallpaper.jpeg`,
  decodes it with zune-jpeg in its own 16 MiB heap, streams RGB rows to
  winsrv over the IPC gate. The kernel never parses image data — that's
  the trust boundary.

## LimeDE desktop

The winsrv compositor (`kernel/src/servers/winsrv.rs`; palette, acrylic,
geometry and animation math documented in `docs/GUI.md`):

- **Compositor**: back buffer + cached wallpaper rows (from the streamed
  decode), single-union damage rect per lap, compose order
  desktop → window shadows → windows → dock/weather/tray pills →
  start card → quick settings → popup; the cursor is drawn as an
  overlay, never baked into the back buffer. Idle = zero fb traffic.
- **Theme "Fluent dark"**: charcoal surfaces `BG (32,34,38)`, focused
  titlebar `(44,46,52)`, ink `FG (208,214,224)`, fluent accent
  `(76,194,255)`. Every panel/card/pill is rounded (`RAD 10/8/10`) and
  blends the wallpaper through it — acrylic (`PANEL_A=214`,
  `CARD_A=226`) — with 4-ring soft shadows. Windows, menu, popups and
  the quick flyout open with a 120 ms fade+rise animation.
- **Detector rule**: the cursor `(240,240,240)` is the only pixel on
  screen with all three channels ≥239 (tests locate it by that); every
  painted UI color keeps one channel below the line on purpose, and the
  wallpaper service clamps its own pixels the same way.
- **Input pipeline**: virtio-tablet owns the pointer; PS/2 bytes are
  swallowed when the tablet exists; focus follows clicks; titlebar drag,
  double-click maximize, 16px resize grip, ✕ close; scrollback-capped
  terminal text; Start search routes keys (Enter = launch first match).
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

- **Toolchain**: `cargo +nightly`, target `x86_64-unknown-none`,
  pinned by `rust-toolchain.toml`. Build cache lives in
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

Nine headless QMP tests, all green, plus the fuzz soak and pointer
utilities (`python3 scripts/<test>.py <iso>`):

| test | proves |
|---|---|
| `qmp_desktop_test.py` | Start card tiles (exact-color count), About popup, dismiss |
| `qmp_multitest.py` | 18-check multi-window proof: focus, drag, resize, close, EOF |
| `qmp_tablet_test.py` | splash band + absolute cursor corners + menu open/dismiss |
| `qmp_input_test.py` | keyboard scancodes reach the guest, cursor moves exactly |
| `qmp_type_test.py` | blind typing lands in the shell console |
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
| Rust | 1.101.0-nightly (via `cargo +nightly`) |
| xorriso | 1.5.8.pl02 (libburnia, built from source into `~/.local`) |
| Limine | 12.9.3 (binaries in `~/limine`, via `setup-wsl.sh`) |
| Host | Windows 11 with WSL2 (Ubuntu 22.04/24.04 runs inside WSL), plus `mtools`, `git`, `curl` |
| Guest | `-m 512M`, `-smp 2`, 1280×800×32 framebuffer, ~492 MiB usable RAM |

## Platform constraints

- **No SSE** on `x86_64-unknown-none` (only fxsr/x87): all float math is
  softfloat — hot paths (especially the compositor) must be integer-only.
- No 1 GiB pages; AHCI flagged but no driver yet; text-only windows (no
  pixels/cells client API yet); subset font.

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
  animations, quick settings

M1–M9 are the pre-1.0 groundwork (boot, memory, userspace, fonts, IPC,
storage, window server + compositor, identity hardening) — kept as
history in `docs/ROADMAP.md`.

See `docs/ROADMAP.md`. v1.0 ships verified in QEMU.

## License

MIT — see LICENSE.
