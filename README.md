# GreenLime OS 🟢🍋

A from-scratch operating system in Rust — custom x86-64 hybrid kernel,
composited GUI desktop (LimeDE), IPC, and ring-3 userland. Long-term goal:
compete with Windows 11.

> Status: **v0.8** — boots via Limine into the LimeDE desktop: Win11-style
> centered taskbar, full Start menu with search, wallpaper, draggable
> windows, terminal + demo apps, interactive shell.

> 🤖 **Built with AI:** every line of this OS — kernel, servers, userland,
> docs — was written with AI assistance using
> [OpenCode](https://github.com/sst/opencode) powered by **Muse Spark**,
> with a human driving architecture, testing every milestone in QEMU, and
> reviewing each change.

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
serial → the LimeDE desktop. Your host cursor stays visible and the guest
cursor tracks it 1:1 (virtio-tablet seamless pointer, no grab).

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

- **Mouse:** point, click, drag window titlebars (or the 16px grip). No
  pointer grab, no `Ctrl+Alt+G` — it just works.
- **Taskbar:** centered Start, Search, and one slot per window; accent
  underline marks the focused app; right-side tray shows an uptime clock
  (`HH:MM` + `Day N`).
- **Start menu:** click Start → 640×560 panel with search box (type to
  filter, Enter launches, Esc closes), 4 pinned tiles, Recent list (last 3,
  deduplicated), footer avatar. Clicks outside the panel dismiss it.
- **Terminal:** click it, type `help`. Builtins:
  `help echo cat ps kill ls exit`. Full line editing.
- **Windows:** focus follows clicks, `WIN_CREATE` never steals focus,
  ✕ button closes. The demo app opens a second window by itself.
- **Popups:** About and Settings dialogs, centered 460×210.

## Architecture

```
Limine → kernel (ring 0) → servers (kernel tasks) → userland (ring 3) → LimeDE
```

Hybrid kernel: microkernel-style IPC + servers for structure, monolith-style
speed (servers start as kernel tasks and can migrate to userspace later).
Fixed task map: kernel tasks `0=vfs, 1=dev, 2=winsrv`; user tasks
`3=init, 4=shell, 5=demo`. Max 8 tasks. Strict rule: `kernel/` may use
`unsafe` + privileged instructions; `libs/*` + `userland/*` are safe-only
and reach the kernel only through syscalls/IPC. Kernel memory is never
mapped into user page tables.

## Kernel

- **Boot** (`boot.rs`): Limine requests for framebuffer (1280×800×32),
  memory map, HHDM, stack. Paging comes up with `cr3` level 4.
- **arch/x86_64**: GDT (kcode/kdata/ucode/udata + TSS), IDT
  (breakpoint/UD/PF/GP/DF + timer/keyboard IRQs), masked PIC, PIT @100Hz,
  verified SMEP + SMAP, `SYSCALL`/`SYSRET` gate with IF masked.
- **Memory**: PMM frame allocator (4 KiB frames, ~126k free on 512M),
  16 MiB static heap (`LockedHeap`), VMM with per-task user tables.
  1 GiB pages unsupported (detected, refused).
- **Scheduler**: cooperative, task states Ready/Blocked(port)/Dead,
  block/wake on IPC ports, `ps` snapshots.
- **Syscall gate**: SystemV trampoline (`num, a0, a1, a2`), runs on the
  caller's private kernel stack. Unknown numbers and rejected calls return
  `u64::MAX`. User buffers are range-checked (`checked_add`, lower-half
  only, length caps), copied under a STAC/SMAP window, then used from the
  kernel copy.
- **ELF loader** (`userspace/elf.rs`): static `ET_EXEC` x86-64 only,
  maps each `PT_LOAD` into a fresh address space, enforces user-half,
  image-bounds, and non-empty segment set. Images are embedded at build
  time via `build.rs`.
- **Console**: serial COM1 + framebuffer TTF console (framebuffer handed
  to winsrv once the desktop paints).

## System calls

ABI v3, frozen numbers (add, never reuse) — `libs/greenlime-abi/src/syscall.rs`:

| # | Name | Args | Returns |
|---|---|---|---|
| 0 | `Log` | ptr, len (≤4096) | bytes logged / `MAX` |
| 1 | `Exit` | code | never returns |
| 2 | `Yield` | — | 0 |
| 3 | `IpcSend` | port, msg ptr | 0 / `MAX` |
| 4 | `IpcRecv` | port (must be own), buf ptr | 0 / `MAX` |
| 5 | `Kill` | pid | 0 / error |
| 6 | `GetPid` | — | pid |

## IPC

Port-based, fixed-size messages (`libs/greenlime-abi/src/ipc.rs`):

- **24 ports**: mailboxes `0–7` (task N owns port N), services
  `PORT_VFS=16`, `PORT_DEV=17`, `PORT_WINSRV=19`. Queue depth 16 per port.
- **`Message`**: `kind u32`, `sender u32` (**kernel-stamped, unforgeable**),
  `arg0/arg1 u64`, `len u32`, `payload [u8; 240]` (`MAX_PAYLOAD=240`).
- Semantics: send blocks on full, recv blocks on empty; `try_recv` polling
  so one slow client can't wedge a server; every mutation wakes port
  waiters (spurious wakeups are harmless — waiters retry).

## Servers

All loop forever on their service port, replying to the kernel-stamped sender.

- **vfs** — ramfs (`/motd.txt`, `/hello.txt`), FAT32 `/disk`, live `/ps`
  task list. `OPEN path → handle`, `READ/WRITE/CLOSE` by u64 handle,
  `READDIR`, `PING` (magic `0x50494E47`). Offsets clamped, reads chunked
  at 200 bytes, unknown paths/closes get `REPLY_ERR`.
- **dev** — `serial0` (unified console) and `term0` (GUI terminal) as
  files. term0 writes forward to winsrv fire-and-forget; term0 reads relay
  `WIN_READ` and block until a full line is typed.
- **winsrv** — window server + compositor (see LimeDE below). Protocol:
  `WIN_WRITE` (terminal text), `WIN_READ` (canonical terminal input),
  `WIN_CREATE` (payload = title → window id), `WIN_CLIENT_WRITE`,
  `WIN_CLIENT_READ` (per-window key stream), `WIN_DESTROY`.

## Drivers

- **serial**: COM1, unified console + log mirror.
- **input**: PS/2 keyboard (scancode ring, IRQ1) + PS/2 mouse bytes
  (drained and discarded whenever the tablet is present).
- **virtio-tablet** (modern, MMIO caps walk): event queue 64 bufs,
  absolute `0..32767` → exact screen pixels, polled with buffer recycling.
  This is what makes the cursor track 1:1.
- **virtio-blk**: queue 256, up to 65536 sectors of 512 B, block
  read/write for the FAT32 disk.
- **pci**: full bus enumeration (host bridge, `1234:1111` display,
  `8086:10d3` network, virtio input/blk, ISA bridge, AHCI (detected),
  SMBus). AHCI driver proper is future work.
- **fb**: VBE framebuffer driver, `Framebuffer::from_raw`, handed over
  to winsrv at `[winsrv] desktop painted`.

## Filesystem

Read-only FAT32 (`kernel/src/fs/fat32.rs`): 512-byte sectors, no LFN,
8.3 uppercase compare, BPB validation (signature, `FAT32` magic, nonzero
geometry), `.`/`..`/deleted/volume-label entries skipped, directory reads
capped at 64 KiB, file reads capped at 1 MiB, FAT-chain cycles broken by
the byte cap. Test disk (`dist/disk.img`, 32 MiB) is made by
`scripts/make-disk.sh` via `make disk`. Shell reads it with `cat`/`ls`.

## Userland

Ring 3, built static for the ELF loader, syscalls/IPC only:

- **init** — PID 1 (task 3): first user program, says hello, yields, exits.
- **shell** (task 4) — v0.8 interactive shell: line editing with history
  buffer, `help echo cat ps kill ls exit`, reads via dev `term0`,
  mirrors output to serial (the harness ground truth), checks reply
  senders and refuses forged messages.
- **demo** (task 5) — second window client: opens its window, writes,
  moves, closes, says bye. Exercises the whole window protocol.

## LimeDE desktop

The winsrv compositor (`kernel/src/servers/winsrv.rs`, palette + geometry
documented in `docs/GUI.md`):

- **Compositor**: 4 MiB heap back buffer + 4 MiB cached wallpaper (of the
  16 MiB heap), single-union damage rect, compose order
  desktop → windows → taskbar → menu → popup; the cursor is drawn as an
  overlay, never baked into the back buffer.
- **Wallpaper**: rendered once at takeover by integer-only fixed-point
  math (Q8/Q16 gradient + 4 radial glows) — no floats, because this CPU
  target has no SSE (see constraints).
- **Theme** (no green — deliberately un-hacker): accent `(76,194,255)`,
  focused titlebar `(38,44,58)`, body `(16,18,24)`, taskbar `(23,26,34)`,
  menu `(24,27,35)`, tiles `(34,38,49)`, console `(214,222,235)` on
  `(10,14,28)`.
- **Input pipeline**: virtio-tablet owns the pointer; PS/2 bytes are
  swallowed when the tablet exists; focus follows clicks; titlebar drag,
  16px resize grip, ✕ close; at most 8 windows; scrollback-capped
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
  (`dist/serial-<name>.log`, dumped on exit). `make diag` collects logs.
- **Tablet helper**: `scripts/activate_tablet.py` (HMP `mouse_set`,
  detaches via double-fork, never sends `quit`).
- `scripts/make-disk.sh`, `scripts/collect-logs.sh`,
  `scripts/setup-wsl.sh`, `scripts/shots2png.sh`.

## Test suite

Five headless QMP tests, all passing, plus pointer + input proofs
(`python3 scripts/<test>.py <iso>`):
`qmp_desktop_test`, `qmp_tablet_test`, `qmp_multitest`, `qmp_input_test`
(cursor 640→840 on +200px, serial sees scancode), `qmp_type_test`,
`qmp_mouse_idle`, `verify_tablet_pointer` (tablet `current=True,
absolute=True`), `probe_mice`, `qmp_screendump`. Pixel-exact color
assertions; `docs/GUI.md` lists what each test proves.

## Requirements

| Tool | Version used |
|---|---|
| QEMU | 10.2.1 (Debian `1:10.2.1+ds-1ubuntu3.2`) |
| Rust | 1.101.0-nightly (via `cargo +nightly`) |
| xorriso | 1.5.6 (libburnia) |
| Limine | 12.9.3 (binaries in `~/limine`, via `setup-wsl.sh`) |
| Host | Windows 11 with WSL2 (Ubuntu 22.04/24.04 runs inside WSL), plus `mtools`, `git`, `curl` |
| Guest | `-m 512M`, 1280×800×32 framebuffer, ~492 MiB usable RAM |

## Platform constraints

- **No SSE** on `x86_64-unknown-none` (only fxsr/x87): all float math is
  softfloat — hot paths (especially the compositor) must be integer-only.
- No 1 GiB pages, no SMP, no networking, no AHCI driver yet, text-only
  windows (no pixels/cells client API yet), subset font.

## Security

v0.8 passed an internal AI-assisted audit: 9 findings from Critical
(shell command injection via an unchecked server relay) down to Info,
plus cleared components (FAT32 parser, sender stamping, payload clamping).
Fixed in order of severity with rebuild + test verification. Hobby-grade
nonetheless: no sandboxing or user model yet — don't put secrets in it.

## Roadmap

- [x] v0.1 boot — Limine → serial + framebuffer print
- [x] v0.2 memory — PMM, paging, heap
- [x] v0.3 interrupts — GDT/IDT, timer, keyboard
- [x] v0.4 userspace — ring-3, ELF loader, syscalls
- [x] v0.5 IPC + servers — VFS + dev, `init` + shell
- [x] v0.6 storage — PCI, virtio-blk, FAT32 (`ls`, `make disk`)
- [x] v0.7 fonts — TTF rasterization + shell editing
- [x] v0.8 GUI — LimeDE: taskbar, Start menu, wallpaper, compositor,
  multi-window focus/drag/close
- [ ] v0.9 security fixes + hardening
- [ ] v1.0 driver model, SMP, network, storage

See `docs/ROADMAP.md`. Milestones ship one at a time, each verified in QEMU.

## License

MIT — see LICENSE.
