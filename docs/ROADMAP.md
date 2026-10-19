# GreenLime OS — Roadmap (made by GLM 5.3)

Every line of this OS was written by **GLM 5.3** (the AI model) with a
human driving review and running every milestone in QEMU.

Version framing: **v1.0 starts at M10 — SMP + net + hardening.** The
phases built *for v1.0* are M10 (SMP + ring-3 networking + hardening),
M11 (Fluent light: userspace wallpaper service + Files app) and M12
(Fluent dark — the shipping look). The earlier `v0.1`–`v0.9` sections
are the pre-1.0 groundwork (boot → memory → userspace → fonts → IPC →
storage → window-server/compositor → identity hardening), kept here as
history. The `v1.0 · M##` tags below are the 1.0 build order.

Honest scope: Windows 11 = ~50M+ LOC, thousands of drivers, decades of compat.
You beat it by starting tiny and never rewriting. Each milestone is bootable in QEMU.

## v0.1 — Boot skeleton ✅ BOOTED
- [x] Workspace + arch folders
- [x] Limine boot, serial + framebuffer print, halt
- [ ] `make run` shows GreenLime banner in QEMU
- Exit criteria: ISO boots in BIOS + UEFI, serial log clean.

## v0.2 — Memory ✅ BOOTED
- [x] PMM bitmap from Limine memmap (`mm/pmm.rs`)
- [x] Paging: higher-half kernel, `vmm.rs` map/unmap
- [x] Kernel heap + `#[global_allocator]` (`mm/heap.rs`)
- [x] `kprintln!` with heap (alloc tests)
- Exit: `mm::test()` prints free frames, heap alloc works, no page faults.

## v0.3 — Interrupts ✅ BOOTED
- [x] GDT with kernel/user segments + TSS (`arch/x86_64/gdt.rs`)
- [x] IDT: exceptions (page fault dump), timer (PIT/APIC), keyboard (`arch/x86_64/idt.rs`)
- [x] `arch::sti/cli`, IRQ EOI, tick counter
- Exit: timer ticks, Ctrl keys print scancodes, page fault prints CR2.

## v0.4 — Userspace (kernelspace/userspace split real) ✅ BOOTED
- [x] ELF loader (`userspace/elf.rs`), user address spaces
- [x] `iretq` to ring 3, user stack, `init` runs
- [x] Syscalls: `log, exit, yield` (`syscall/`, ABI v0.1 frozen)
- [x] User page tables isolate kernel (SMAP/SMEP on)
- Exit: `userland/init` prints from ring 3, `exit` returns to kernel.

## v0.5 — TTF fonts ✅ DONE
- [x] Real font system: `libs/greenlime-font` (TTF load, glyph cache, fb text)
- [x] Kernel fb console mirrors serial (`drivers/fb.rs`, `assets/fonts/`)
- Exit: TTF console on screen in QEMU, pixel-verified.

## v0.6 — Hybrid IPC + servers + shell ✅ DONE
- [x] `ipc::Channel` (bounded queue, blocking send/recv), capabilities
- [x] Syscalls `ipc_send/recv`, scheduler blocks on channels
- [x] Servers: `vfs` (ramfs), `dev` (serial/fb as files), scheduler as server client
- [x] `shell` in userspace: `help, echo, ps, kill`
- Exit: shell runs `echo hi` via `vfs` + `ipc`, no kernel panic.

## v0.7 — Storage + drivers ✅ DONE
- [x] VirtIO-blk (legacy PCI transport, polling), FAT32 read
- [x] Driver model: `Driver::probe/init/handle_irq` trait (+ PCI scanner)
- [x] AHCI: detected + reported (full driver later); virtio-input shipped in v0.8
- Exit: `ls` lists disk files in QEMU `-drive` image.

## v0.8 — GUI: window server + compositor + LimeDE desktop ✅ DONE
- [x] Window server task + terminal window + taskbar + clock (`servers/winsrv.rs`)
- [x] Shell output routed to window (`term0` device, WIN_WRITE protocol)
- [x] Keyboard scancodes to status line (lock-free rings, QMP-verified `0x1e`)
- [x] PS/2 mouse cursor, moves on input (pixel-verified +300 x)
- [x] Boot splash: only centered "GreenLime OS" on screen, diagnostics stay serial (`fb::splash`/`wake_console`)
- [x] virtio-tablet (modern MMIO cap transport): guest cursor = host pointer exactly, clicks too (corner/center pixel-verified)
- [x] Interactive stdin: set-1 line discipline, canonical reads, shell REPL (`WIN_READ` via dev `OP_READ`)
- [x] Second client (multi-window + focus policy): `userland/demo` + WIN_CREATE/CLIENT_WRITE/CLIENT_READ/DESTROY; focus follows clicks, keys route to the focused window (`qmp_multitest.py`)
- [x] Double buffering + damage rects: back buffer + compose(damage)/blit compositor, layers in z-order, cursor never composed (`servers/winsrv.rs`)
- [x] LimeDE chrome: taskbar app buttons (id order, focused = bright), titlebar drag-move, corner-grip resize, titlebar X close
- [x] Visual rework: no green — dark neutral + blue accent palette, procedural wallpaper (integer one-shot render), Win11 centered taskbar + tray, full Start menu (search filter / pinned tiles / recent), Settings + About popups (`qmp_desktop_test.py` exact-color asserts)
- Exit: ✅ met — boot to GUI, open terminal, type `echo` (`qmp_type_test.py` 5/5); multi-window proof 15/15 (`qmp_multitest.py`); tablet/splash, desktop, input regressions all green.

## v0.9 — Own identity + hardening ✅ DONE
- [x] G start mark replaces the four-square logo; left-aligned taskbar, no Search slot
- [x] Launcher menu: left-docked vertical app rows (no centered grid, no Recent, no "All apps")
- [x] Terminal opens on demand from the launcher (never at boot), buffered early text, X button closes it; popups get X marks
- [x] Terminal line-length cap (heap-exhaustion guard)
- [x] Toolchain: QEMU 11.1.2 + xorriso 1.5.8.pl02 (source-built), full suite re-verified green
- [x] Security fixes (audit GLS-001…010: relay check, kill gate, handle/window owners, fault isolation, ELF + disk hardening)
- Exit: ✅ met — clean desktop at boot, suite 7/7 on the new toolchain, findings closed (GLS-007 deferred, documented in `docs/SECURITY.md`).

## v1.0 · M10 — SMP + net + hardening
- [x] AP startup (Limine MP): APs print + park, BSP fail-soft (`arch/x86_64/smp.rs`, `qmp_smp_test.py` 4/4)
- [x] Per-CPU TSS (own RSP0 + double-fault IST per CPU, IPI-ready sti+hlt idle)
- [x] LAPIC + first IPI: enable, per-AP IPI, counted answers (`arch/x86_64/lapic.rs`, `qmp_smp_test.py` 6/6)
- [x] Per-CPU scheduler state (GS-anchored CpuLocal, atomic claim,
  no-resurrect yield), per-CPU syscall trampoline (GS-relative slots)
- [x] LAPIC timer per CPU (PIT-calibrated) + safe preemption
  (EOI-first, APs hlt-idle; **need_resched design**: the tick never
  switches context from IRQ — it runs the double-claim watchdog and
  raises the per-CPU flag; the current task consumes it at its next
  safe point, syscall return or voluntary yield, switching through the
  proven cooperative machinery — `sched::preempt_if_pending`).
   **SMP stability campaign** (post-mouse-freeze bug triage): four
  real scheduler-corruption bugs found live and fixed — (1) IRQ/IOAPIC
  page missing from every user CR3 (fault handlers under user tables
  wedged writing 0xfec00000); (2) `Vec<Task>` heap reallocs dangling
  `saved_rsp`/gs raw pointers handed out mid-claim (fixed with
  `reserve_exact(MAX_TASKS)` + static `USER_RSP_SAVE` array); (3) the
  context switch ran with interrupts open — a tick landing on the asm's
  first instruction parked a torn mid-switch context (now
  `pushfq/cli/…/popfq` atomic + per-CPU `switching` flag covering the
  pre-cli window); (4) with QEMU's IOAPIC routing the PIT vector
  reaches APs too, so one CPU could park from *two* tick sources
  interleaved — stale parked-scheduler-frame replay left a task stuck
  `Running` while its owner CPU moved on (watchdog: `Task.owner`
  cross-CPU checked every tick; PIT counts + prints on the BSP only).
  The old tick-park preemption path was the proven corruptor of all of
  the above and is now deleted by construction (need_resched instead).
  Known tradeoff: a ring-3 CPU burner that makes zero syscalls and
  never yields is not preempted; every real task here passes a syscall
  or yield boundary many times per ms.
  Proof: release `-smp 2` matrix 4/4 clean + B3 suite 5/5
  (`dist/serial-GreenLime-release.log`: 16-tick interactive desktop,
  zero fault lines).
- [x] Driver model: registry + IRQ mapping + report (`drivers::register`,
  `qmp_smp_test.py` 7/7 green, zero behavior change)
- [x] VirtIO-net (legacy transport, polled queues) + smoltcp 0.14: DHCP
  lease + ICMP echo to the gateway, headless-proven (`qmp_net_test.py`).
  No sockets API yet; NET-01 (parse panics halt) accepted for now.
- [x] IRQ-driven RX/TX wake: MSI-X lucky path — entry 0 → vector 0x40
  (LAPIC MSI window), config + both queue vectors wired to entry 0,
  `irq #N` proof lines during DHCP/ping (`dist/net-serial.log`). Gotcha
  documented in-stone in `pci.rs`: MSI-X table regions reject accesses
  narrower than 4 bytes (QEMU `valid.min_access_size = 4`), so vec_ctrl
  must be written as its full dword or the mask bit sticks at 1 and all
  interrupts land in the pending-bit path. INTx fallback via the q35
  PIRQ router (00:02.0 pin A → IOAPIC GSI 22) kept, armed only if the
  MSI-X entry read-back fails.
- [x] DNS A-query + external ICMP echo (google.com, end to end):
  hand-rolled minimal resolver over a UDP socket (RD-flagged A query,
  first `A` IN record parsed from the answer section, compression-pointer
  names skipped without chasing) via slirp's 10.0.2.3 proxy — then one
  echo to the resolved address (`[net] dns: google.com -> <ip>` +
  `[net] ping: reply from <ip> in Nms`, repeatable, zero faults; B3
  5/5 + `-smp 2` matrix 4/4 on the same build). Gotchas: the DNS header
  counts field is five u16 fields (qd/an/ns/ar) — writing three after
  flags (an off-by-2) shifts the whole message; slirp silently DISCARDS
  malformed queries with no rcode reply, so the only refactor-proof
  evidence is a wire capture (`-object filter-dump`). Resolver server
  comes from DHCP option 6 with 10.0.2.3 as fallback. Known limits:
  rcode != 0 and CNAME-only chains fall through as "no answer" (loud),
  no negative caching, still single-shot per boot.
- [x] Phase C — ABI v4 net broker: userland sockets via IPC, not new
  syscall numbers. `PORT_NET` = 18 + tagged protocol
  (`libs/greenlime-abi/src/net.rs`, ABI_VERSION 3→4): NET_OPEN
  (UDP-first, TCP reserved for Phase D), NET_SENDTO, NET_RESOLVE,
  NET_CLOSE, NET_REPLY; incoming datagrams are PUSHED to the owner's
  mailbox as NET_DATAGRAM (blocking recv = blocking on your own
  mailbox — the broker never blocks, queue-full drops). The net task
  owns smoltcp exclusively; after boot checks it drains port 18 each
  lap: poll → service → push datagrams → yield. Handles are owner-
  stamped (SENDTO/CLOSE verify), duplicate local ports refused (first-
  match wins steals mail), no-NIC boots fail CLOSED with fast NET_ERR
  replies (found live: a blocked demo wedged the desktop test boot).
  Design note: no new Syscall numbers — sockets ride the frozen IPC
  gate like windows/vfs, keeping the fuzz surface small.
  Proof: `[user] [demo] net: google.com -> 142.251.14.101` — ring-3
  demo hand-builds the DNS query, broker moves the bytes, demo parses
  the A record; agrees byte-for-byte with the kernel's own resolve;
  repeatable, zero faults, B3 5/5 + release matrix 4/4. Live-found
  bug: the boot resolver's UDP socket also bound (any,49100) and was
  never retired — smoltcp delivers a datagram to the FIRST matching
  socket and stops, so the stale boot socket ate every reply addressed
  to a client (demo blocked on a reply it never saw; fixed by removing
  the boot socket after resolve). Diagram cap: 240 B per datagram
  (MAX_PAYLOAD); bigger chunks are a TCP story.
- [x] Phase D — TCP + ring-3 shell tools through the same broker
  (`scripts/qmp_net_shell.py` drives a REAL interactive terminal: types
  `ping google.com` + `fetch example.com` via QMP sendkey). Broker arms:
  NET_CONNECT (polls to ESTABLISHED, 8 s bound, abort+err on RST/deadline),
  NET_SEND (stream, polls out backpressure instead of failing), NET_RECV
  (pull model: request parks in the broker, the drain loop answers with
  240 B chunks; peer FIN + drained → NET_EOF), NET_CLOSE (abort; no
  FIN-flush state machine this phase — a drained stream already saw its
  EOF), NET_PING (one ICMP echo through the broker-owned icmp socket,
  clients serialize). Client shell types: ephemeral ports 49200+,
  GET with `User-Agent: GreenLime/1.0 (ring-3 fetch)` — **the OS is now
  countable**: the UA leaves the NIC inside a real TCP segment
  (`filter-dump` proof: 9 frames — SYN / SYN-ACK / ACK+GET / ACK /
  985 B response + FIN / closing ACK).
  Proof lines: `[dev:serial0] ping: google.com -> 142.251.14.138`,
  `ping: reply from 142.251.14.138 in 80ms`, `fetch: HTTP/1.1 200 OK`,
  `fetch: stream end, 931 bytes total`; B3 5/5 + release matrix 4/4 on
  the same build. Live-found bugs: (1) error sentinel split-brain — the
  broker replied `REPLY_ERR` (0xFFFFFFFF) where the ABI documents
  u64::MAX, so a shell error decoded as IP 0.0.255.255 + RTT
  4294967295ms (unified: u64::MAX everywhere); (2) the broker resolve
  arm called `send(len, dst)` and never copied the query into the
  returned tx window — the wire saw 28 perfect-length ZERO bytes and
  slirp answered nothing (silent like every malformed-DNS drop; only
  the pcap caught it).
- [x] Capabilities enforced, W^X, KASLR seed, fuzz syscalls
  - Caps (`sched::caps`, spawn-time-only, no escalation API): kernel
    servers ALL; init/shell CAP_KILL|CAP_NET; demo CAP_NET; fuzz none —
    enforced at `Kill` and every net-broker op (kernel-stamped sender).
  - W^X: ELF loader refuses W+X segments; boot PTE audit proves kernel
    text R+X / data RW+NX (`[mm] wx:` line; first build FAILED it via a
    walk bug — leaf PTE descended as a table, MMIO-backed phys paged the
    kernel; both bug classes found live by the fuzzer).
  - KASLR: splitmix64 boot seed feeds per-task stack-top jitter (0–511
    pages × task id, `[sched] task N stack … caps=…` lines) + smoltcp.
  - GLS-011 fixed: user-range validation was arithmetic-only; unmapped
    in-range pointers paged the kernel under STAC. `user_range_ok` now
    probes every page in the caller's tables before the copy.
  - Fuzz: `greenlime-fuzz` task 7 (unprivileged) — random syscall
    numbers, junk pointer chaos, IPC self-traffic (flow-controlled — an
    ungated recv parked it forever, found live), Denied kills; PACED via
    `Syscall::Park` (ABI v5, 8th number): after each 64-iteration burst
    the task sleeps to the next tick, so the schedulers reach their idle
    `hlt` and the virtual CPU stops pegging at 100% (first build ALWAYS
    ran it flat — GUI lag + ~80 serial lines/s; found live, of course).
    The tick converts Parked → Ready inside tick_preempt's existing
    try_lock scope — no new IRQ-path locking.
- Exit: ✅ met — two cores print ticks (`qmp_smp_test` 4/4), ping works
  (`qmp_net_test` + phase-D `qmp_net_shell`), B3 suite 5/5 on the
  hardening build, and the syscall-fuzz soak ran **1h clean**:
  0 `[FAULT]`, 0 `[PANIC]`, 108,033,536 iterations complete (fuzzer
  alive start to finish, single continuous run).

## v1.0 · M11 — Fluent light + userspace wallpaper + Files app
- [x] **Userspace wallpaper service** (task 8): `userland/wallpaper`
  embeds the user's `assets/wallpapers/wallpaper.jpeg`, decodes it with
  zune-jpeg 0.5 in its own 16 MiB static heap, and streams RGB to winsrv
  via WIN_WALL_SETUP/ROWS/DONE (ABI v6 message, ~2 KiB payload).
  Kernel never parses image data — winsrv just converts + blits (trust
  boundary). Placeholder JPEG (1×1) auto-written by build.rs when the
  asset is missing. Proof: `[wall] wallpaper streamed (1502 msgs)` +
  `[winsrv] wallpaper: task 8 decoded …` on every boot.
- [x] **ABI v6** (`libs/greenlime-abi`): MAX_PAYLOAD 240 → 2048
  (Message ≈ 2.1 KiB — rows/payload stream in raw now), MAX_TASKS 8 → 9
  (wallpaper task 8), WIN_WALL_* klasses; all other numbers frozen, gate
  unchanged (still 8 syscalls).
- [x] **RTC driver** (`drivers/rtc.rs`): boot time + minute-of-day —
  feeds the clock tray (`18:32`), "Day N" count and the start-card
  greeting (`[rtc] boot time HH:MM` proof line).
- [x] **GUI rework "Fluent light"** (`winsrv.rs`): light palette,
  floating dock/weather/tray pills inside the 48px strip, centered start
  card 560×436 with greeting + 5 pinned tiles + search pill, rounded
  pill/card caps via integer `fill_round`. Geometry is contractual and
  documented in `docs/GUI.md` (Start center (616,776) at boot, tiles
  row-0 y 450, popup (410,295) 460×210).
- [x] **Files app**: the explorer (id 1, `FILES_ID` reserved — demo
  always id 2): title carries the path, body lists `vfs::list_names`
  with folder glyphs, click navigation with `../` pop, dead-path guard.
  Replaces the old Recent row (deleted).
- [x] **Two-phase park (PendingPark) soak fix**: `park_current` sets
  PendingPark + a per-CPU handshake; scheduler_loop promotes
  PendingPark → Parked at lap top (context provably saved). The M10
  pre-switch exposure (tick converting a half-saved Parked task to
  Ready → double-run at ~2034s soak) is closed by construction.
  *Deferred follow-up:* the `Blocked → Ready` wake path has the same
  theoretical pre-switch window (documented in `SECURITY.md`); it did
  not fire in the M11 kernelling (see below) and stays deferred.
- [x] **Kernel stack right-sizing + forensics (debug-campaign finding)**:
  the first M11 debug boots showed deterministic `rip=0x0` fault loops.
  Root cause (gbd + fault-path stack dumps): net's broker call chain used
  **32,576/32,768 bytes** of its 32 KiB kstack (ABI v6 2 KiB `Message`s
  staged per frame at `-O0`) and its next deep call slopped past the slot
  bottom into the *neighbouring task's* saved switch context — demo then
  resumed with a zeroed return address. Fix: `KSTACK_SIZE` 32 → 64 KiB
  (9 tasks ≈ 576 KiB extra .bss, trivial); the dump (32 stack words +
  task table before the halt) is kept permanent. Also fixed en route:
  the fuzzer's seed line (`pid * const` overflowed u64 in DEBUG builds —
  wrapping_mul; its panic handler now prints payload + `file:line`
  instead of a fixed string).
- [x] **Debug suite unblocked (was blocked on the M10 hardened build)**: B3 5/5 on the debug ISO
  (desktop / multi / smp / net incl. DHCP+ICMP/ type), dbg-loop 5/5
  boots fault-free with demo window + wallpaper streaming + fuzz
  heartbeats, hardening smoke (wx / kaslr / caps lines) green.
- [x] Exit: **1h paced fuzz soak re-run on the release ISO** with
  the two-phase park in place (the M10 108M-iteration clean stamp predates
  it); + qmp_desktop/tablet coords re-verified after the wallpaper asset
  lands (cursor-uniqueness vs real wallpaper pixels).
  ✅ Met — soak `RESULT: PASS — syscall fuzz 1h clean` (0 `[FAULT]`,
  0 `[PANIC]`, 20,758,528 iterations, single continuous run), and the
  desktop/tablet/input suites all green over the **real JPEG wallpaper**
  (cursor predicate: every frame's only all-≥239 blob is the 60-px
  9×14 arrow at the expected position).

## v1.0 · M12 — Fluent dark
- [x] **Palette "Fluent dark"** (`winsrv.rs`): charcoal surfaces
  (`BG(32,34,38)`, title `44,46,52`, menus `38,41,48`, cards `34,37,44`),
  light ink `FG(208,214,224)`, Fluent blue accent `ACCENT(76,194,255)` +
  `ACCENT_FILL(44,78,110)`. Every functional surface, glyph, chrome and
  proof string preserved; geometry and fonts untouched (all QMP
  coordinate probes still contractual, values re-ported to dark).
- [x] **Acrylic engine**: `acrylic_round_clip` blends `wall` under every
  card/pill/flyout at per-pixel alpha (`PANEL_A=214`, `CARD_A=226`),
  integer-only (`-O0`-safe), damages recomposed per lap. Rounded windows
  (`RAD_WIN=10`), tiles (`RAD_TILE=8`, stadium search pill `r=h/2`).
- [x] **Soft shadows**: 4-ring 3px `window_shadow` around every floating
  surface (alpha ramp `SHADOW_A=[120,76,42,24]`), scaled by the open
  animation progress; `strip_rect` includes `SHADOW_PAD=12` overhang so
  partial repaints never leave stale rings.
- [x] **Open animations**: 120 ms fade+rise (`ANIM_TICKS=12`,
  `ANIM_RISE=14`, Q8 `anim_p`) on windows, menu and popups, plus the
  **`ANIM_TAIL=180` invariant**: one compose lap can take longer than
  the whole animation (a loaded debug VM laps at ~1 s), so tick_anim
  keeps animating surfaces invalidated for 1.8 s past expiry — the first
  post-deadline lap paints the final crisp `p=256` frame. Without the
  tail a slow lap freezes the half-faded ghost on screen forever
  (repro'd: Files window + menu frozen at p≈21 for the rest of boot).
- [x] **Quick settings flyout** (`Popup::Quick`, 240×280 anchored
  right-above-tray): 2×2 toggle tiles (Wi-Fi/BT/Night/Saver, accent when
  on), click-to-set volume/brightness sliders, "All settings" footer.
  One flyout at a time, outside-click dismiss, tray-clock pose proof.
- [x] **Start-menu search slice fix** (QA-campaign regression caught by
  `qmp_desktop_test.py`): compose passed the raw 32-byte `search` buffer
  to `draw_menu` instead of `&search[..search_len]` — with zero typed
  chars the "query" was 32 NUL bytes, so the filter hid every tile and
  the placeholder read as tofu. Slice restored (M11 semantics); the
  render fallback (non-printable query ⇒ placeholder) stays as defense.
- [x] **Dbl-click title maximize**: double-click on a titlebar within
  25 ticks toggles maximized (`(0,0,sw,sh-48)`),
  `[winsrv] win N toggled` proof.
- [x] **Suite re-run after the dark rework**: desktop OK (menu=41,116
  tiles · About popup 11,552 · dismiss 0), multitest 18/18, tablet PASS
  (tl/br/mid cursor + menu open 62,598 / dismiss 0), input PASS (cursor
  640→840, sc 0x1e), smp/net/type rc=0, dbg-loop 5/5 boots fault-free,
  hardening smoke green on release, 1h soak PASS pre-suite.

## After 1.0
* aarch64 port (`arch/aarch64/`), UEFI self-host, package manager, POSIX shim,
  Linux/Wine-like compat, signed updates.

## How to work
1. One milestone at a time. Don't start GUI before userspace.
2. Each PR: code + QEMU log + `docs/` update.
3. ABI breaks only with major version bump + migration note.
