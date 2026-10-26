# GreenLime OS — Roadmap (made by GLM 5.3)

Every line of this OS was written by **GLM 5.3** (the AI model) with a
human driving review and running every milestone in QEMU.

Version framing: **v1.0 starts at M10 — SMP + net + hardening.** The
phases built *for v1.0* are M10 (SMP + ring-3 networking + hardening),
M11 (Fluent light: userspace wallpaper service + Files app) and M12
(Fluent dark). The earlier `v0.1`–`v0.9` sections
are the pre-1.0 groundwork (boot → memory → userspace → fonts → IPC →
storage → window-server/compositor → identity hardening), kept here as
history. The `v1.0 · M##` tags below are the 1.0 build order; **v1.1**
adds M13 (SSE2 + fast compositor), M14 (writable FAT32) and the M14.5
LimeDE rework — the current shipping look.

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

## v1.1 — Applications, speed, real storage (planned)

Scope picked for 1.1: three phases, in this order (the build-wide one first,
everything after stacks on it).

### v1.1 · M13 — SSE + fast compositor ✅ DONE
- [x] SSE/SSE2 enabled at boot on the BSP + every AP (CR4.OSFXSR +
  OSXMMEXCPT, CR0.MP set / EM clear — `arch::init_ap_features`, now also
  called from kmain), `#XM` handler in the IDT (ring-3 task dies like
  #UD/#PF, supervisor halts loud).
- [x] FXSAVE/FXRSTOR in the context switch: `switch_context_asm` now takes
  a per-context 512-byte FX area (16-aligned; `FX_SAVE[task]` +
  `SCHED_FX[cpu]`), seeded once at boot from a `fninit` + default-MXCSR
  template. Proof: `[m13] xmm: 256/256 sentinel bytes survived a task
  switch` — winsrv loads distinct sentinels into XMM0–15, yields to the
  scheduler (other tasks run), and every byte comes back.
- [x] Build split: **userland** builds with `-C target-feature=
  +sse,+sse2,-soft-float` (Makefile `UE_RUSTFLAGS` + relocation-model,
  since env RUSTFLAGS overrides `.cargo/config`); the **kernel stays
  built without `+sse`** — IRQ handlers can never touch XMM, which is why
  the IRQ path needs no FXSAVE and user XMM state survives interrupts.
  Toolchain landmine (documented, stays avoided): the
  `x86_64-unknown-none` spec pairs `-sse,+soft-float`, and LLVM rejects
  `#[target_feature(enable="sse2")]` (+ `-soft-float` alone breaks core/
  compiler_builtins float returns — "SSE register return with SSE
  disabled", rust#117938). Kernel kernels therefore ship as **hand-written
  SSE2 asm** (`arch/x86_64/simd.rs`), not intrinsics; every Rust path
  stays integer-only.
  Found live along the way: userland entered ring 3 with rsp % 16 == 0 —
  once codegen emitted `movaps` spills, every user task #GP'd (soft-float
  never noticed). Fixed: initial user rsp reserves the SysV return-slot
  (stack_top − 8).
- [x] Compositor hot paths vectorized (bit-exact vs the scalar reference
  `blend_px` — 4 px/vector: acrylic `blend_const_span`, scrim/shadow
  `blend_over_span`, wallpaper copy + present blit `copy_span`).
  Proof: `[m13] simd: SSE2 blend bit-exact on 20480 px`, `… scalar 1436
  Mcyc vs sse2 15 Mcyc -> 92.8x` (debug build; release 111 vs 14 Mcyc =
  7.8x — scalar is auto-tuned by -O2 there).
- Exit: ✅ met — B3 suite 5/5 (desktop contract pixel counts unchanged:
  menu 41,116 / About 11,552 over the SSE2 blend), tablet PASS (62,598
  menu tiles), input PASS (cursor 640→840, 60-px blob rule), release ISO
  desktop green with wallpaper streams, zero `[FAULT]` lines.
- Portability addendum (live-found on VirtualBox "Other (64-bit)"):
  `init_ap_features` wrote CR4 with **SMEP+SMAP unconditionally** —
  VirtualBox's default CPU profile exposes neither bit, the CPU raised
  #GP on `mov cr4, eax` (`0f 22 e0`, eax=PAE|OSFXSR|OSXMM|SMEP|SMAP),
  and with no IDT loaded yet that escalated to a triple fault →
  **Guru Meditation 1155 (VINF_EM_TRIPLE_FAULT)**, black screen at
  Limine handoff. Fix (both call sites — BSP `kmain` + every AP):
  CPUID.(7:0)-gate the two bits exactly like `paging::init` already
  did, **max-leaf guarded** (some hypervisors return vendor-string
  fragments in EBX for unimplemented leaves) and kept
  OSXMMEXCPT/OSFXSR unconditional (SSE2 mandatory in long mode).
  New regression test: **`scripts/qmp_vboxsim_test.py`** — boots
  QEMU with the VirtualBox story (`-cpu qemu64` without the feature
  flags AND zero virtio devices) and proves
  `[arch] protections: smep=false smap=false`, desktop painted, shell
  interactive, demo window, wallpaper streamed, no `[FAULT]`, net
  fail-closed. QEMU's `+smep,+smap` suites kept green (B3 5/5 after the
  fix; the gate is a no-op there).

### v1.1 · M14 — Writable filesystem
- [x] FAT32 write path: FAT chain alloc/free (first-fit + resume cursor,
  `EOC` on alloc so partial ops leave a legal chain end), dir entry
  create/delete with terminator-correct slot reuse (E5 as-is; a consumed
  `0x00` marker is rewritten a slot later; the chain's last slot being
  consumed triggers a dir-cluster append), mkdir with `.`/`..`+terminator,
  empty-dir-only deletes, extend (pre-zeroed) / truncate (`set_size`)
  with excess-cluster release, pwrite-at-offset with hole zero-fill.
  Single FAT mirror kept in sync; FSInfo counts intentionally stale
  (legal).
- [x] vfs WRITE (now real: pwrite at arg1 offset with extend + zero-fill
  holes; reply arg0 = bytes written) + new additive ops OP_CREATE=7,
  OP_DELETE=8, OP_MKDIR=9, OP_SETSIZE=10 (v1.0 clients unaffected). Disk
  mutations are caps-gated: `CAP_DISK = 1<<3` folded into USER_ADMIN
  (init + shell only; reads stay open). Shell: `echo text > /disk/f`
  (delete-then-create overwrite, one atomic proof line), `rm`,
  `mkdir`, atomic `cat: not found`. Files app saving: deferred (needs a
  text-buffer UI; shell covers disk mutations today) — see 1.2 notes.
- [x] virtio-blk `write_block` (T_OUT header, data descriptor
  device-read; same bounce frame shape as read).
- Exit: ✅ met — `scripts/qmp_disk_test.py`: shell writes
  `m14proof` to `/disk/m14.txt` via `echo >`, reads it back in the same
  boot, then (after a full QEMU reboot on the same `dist/disk.img`) a
  read-only second phase still sees the file + `mkdir`'d dir. Also
  proves: `rm` deletes a file (gone on `cat`), refuses a NON-EMPTY dir
  (`/disk/docs` intact), 12/12 checks on the debug ISO and 12/12 + 3/3 on
  the release ISO. Kernel proofs: `[vfs] created|deleted|mkdir … ok`,
  `[vfs] wrote N bytes -> /disk/…`; shell proofs: `wrote N bytes -> …`,
  `rm/mkdir: ok|err …`. All prior suites stay green (B3 5/5, tablet,
  input, release desktop pixel contracts unchanged).
- Found live along the way: u16 fat32 dir-entry packing panics on first
  boot use (u32→2-byte field `copy_from_slice`) — fixed by `(f>>16) as
  u16` splits; the QMP disk test caught it before any reviewer could.

## v1.1 · M14.5 — LimeDE desktop rework ✅ DONE (ChromeOS charcoal glass)

User directive: make the GUI match a posted ChromeOS dark reference
"exactly" — individual floating shelf chips (not one merged pill), an
azure Files folder, no desktop watermark, bright tray glyphs, and the
blue wave wallpaper. The user also ruled: **no procedural wallpaper
engine** — a `wallpaper.png` is simply dropped in later; the repo ships
a generated default asset in the same spirit.

- [x] **Palette "LimeDE dark"** (`winsrv.rs`): `BG(30,32,37)`,
  `TITLE_BG(38,40,46)` / `TITLE_BG2(30,32,36)` focused/unfocused
  titlebars, `FRAME(64,68,78)`, `HOVER(52,56,64)`, `ACTIVE(56,60,70)`,
  brand **`LIME(170,232,64)`** (the `ACCENT` cyan-blue is fully
  renamed/deleted), `LIME_FILL(44,74,22)`, **`AZURE(56,126,236)` +
  `AZURE_HI(96,158,244)`** (Files folder, client chips), chip surface
  `MENU_BG(24,26,30)`, panel card `CARD(28,30,36)`, quick OFF tiles
  `MENU_BG2(46,49,56)`, inks `MENU_FG(230,233,240)` / `INK(206,212,220)`
  / `SUB_FG(122,128,142)`, tray glyphs `WHITE(232,238,247)`.
  Acrylics: `PANEL_A=214`, `CARD_A=236`, new per-chip
  **`CHIP_A=238`**; radii `RAD_TILE 14`, `RAD_PANEL 12`. Every
  functional surface, glyph, chrome, serial proof string and the
  "made by GLM 5.3" attribution preserved. Desktop watermark deleted (About/Settings popup copies
  keep the version line); Settings popup shows "Shelf chip shelf,
  48px strip".
- [x] **Chip shelf** (48px strip, no shared pill): per-chip
  `acrylic_round_clip` chips — launcher G (x=16), wide search stadium
  (64..210), Files pin (218, azure folder), Terminal pin (266, lime
  `>_`), then one chip per running client at 48px pitch
  (`x=314+48k`, max `SHELF_MAX_CLIENTS=4`). Hover lifts a chip with a
  `HOVER@150` `chip_round_clip` tint over the composed back buffer;
  launcher/search tint `ACTIVE@230` while the panel shows; open/running
  state = a 3px lime underline. Client chips map **id ASC** (never Vec
  z-order; `nclients()` excludes the Terminal/Files system windows, so
  the demo chip never moves when the terminal opens).
- [x] **All apps panel** replaces the centered Start card: left
  acrylic panel (16,241,200,500), "All apps" header, rows
  `(28,289+54i,176,50)` in order **Settings / Terminal / Files /
  Demo**, footer icon buttons (About center (46,671), Settings gear
  center (46,711)) with `fill_ring` annuli. `APPS` reordered,
  `recent` fields deleted.
- [x] **Live search, one query**: the `[u8;32]` search buffer is
  shared shelf chip ↔ panel; typing/backspace invalidates both
  surfaces; non-UTF8 (fuzz) queries fall back to the "Search"
  placeholder; row order is filtered-length-driven.
- [x] **Bright tray + date**: pill (1060,753,196,46), chevron/wifi/
  volume/battery glyphs in `WHITE`; tray shows a two-line RTC clock
  `HH:MM` over `YYYY-MM-DD` — `rtc::snapshot()` gained
  `(year:u16,month,day)`, `BOOT_DATE` packs `y<<16|m<<8|d`, and
  `now_date(uptime_secs)` rolls the day over (`[rtc] boot time HH:MM,
  date YYYY-MM-DD` boot line).
- [x] **PNG wallpaper drop-in**: `userland/wallpaper/build.rs` picks
  the asset `.png` → `.jpeg` → `.jpg` (notifies on both dir + file
  changes) and emits `GREENLIME_WALLPAPER` + `GREENLIME_WALL_FMT`;
  `zune-png 0.5.2` added (lean decode: one exact-size Vec,
  16-bit downshift, alpha-drop, direct RGB3 conversion — the naive
  samples-copy chain OOM'd the 16 MiB userspace heap before the
  rewrite); a heap guard (`output + 3·w·h ≤ 13MB`) exits politely
  instead of dying. Default asset: generated dark-navy blue wave
  `wallpaper.png` (1600×900, ~292 KiB), channels clamped ≤238.
  Shader rules unchanged: wallpaper pixels still never allow an
  all-channel ≥239 blob (cursor uniqueness).
- [x] **All pixel tests re-baselined** to the new chrome with NEW
  contracts: desktop — launcher lime underline ≥40 px in
  (22,786)-(50,794) (paints 54), demo client azure strip ≥100 in
  (316,760)-(356,798) (paints 112), popup `TITLE_BG` strip ≥8000 in
  (412,297)-(858,323) (paints 10862), dismiss → 0/0; tablet/test/net/
  disk/multi re-pointed to launcher (36,776) → rows (116, 314+54i);
  multitest demo chip (334,776) + `TITLE_BG(38,40,46)`/
  `TITLE_BG2(30,32,36)` counts — 18/18. Full proof: desktop 5/5,
  tablet 4/4 stages, type 5/5, multi 18/18, disk 12/12 + 3/3 (fresh
  image, same-image phase 2), net shell PASS (release), B3 suite 5/5,
  release desktop pixel counts identical to debug.
- Exit: ✅ met — the desktop reads like the reference (floating chips,
  azure Files, bright tray, blue wave + lime accents), every suite
  green, both ISOs build 0-warning.
- Note: M15 (below) stacks on this rework.

### v1.1 · M15 — App graphics API ✓ DONE
- [x] `WIN_BLIT`: a client sends RGB pixel runs (3 B/px triples, same run
  shape as the wallpaper service) into its own window; winsrv keeps a
  per-client canvas. Additive ABI kinds: `WIN_BLIT_SETUP=10`, `WIN_BLIT=11`,
  `WIN_BLIT_END=12` — RGB triples, not RGBA (the compositor is XRGB anyway,
  and 3 B/px reuses the proven `WIN_WALL_ROWS` payload shape).
- [x] Compositor composes client canvases under the theme: the canvas is
  anchored at the content viewport's top-left, 1:1 opaque, clipped to the
  viewport (rounded frame, shadow ring, taskbar untouched — and the canvas
  REPLACES the window's text lines while present, text mode returns on
  `WIN_BLIT_END`, synced by its `REPLY_OK`). Owner-checked (kernel-stamped
  sender must be the `WIN_CREATE` owner), bounds-checked runs
  (out-of-range drops silently), budget-capped at `CANVAS_BUDGET_PX = 1<<20`
  px total across all windows (4 MiB of the kernel heap; `try_reserve_exact`
  failure answers `REPLY_ERR` instead of OOM-ing the compositor).
- [x] Demo app draws live custom graphics in its window: 40 Park-paced
  frames (10 ticks/frame — ~7 fps measured on the release build, ~1 s per
  frame on the debug build under TCG) of navy gradient + azure ridge wave
  + a bouncing lime ball, streaming only dirty rows — zero heap (a 1536 B
  row buffer + a 256 B dirty mask on the stack, one reused `Message`)
  Proof strings:
  `[winsrv] canvas: win 2 512x256 (owner 5)`, `[demo] m15: canvas open
  512x256 live`, per-frame `[demo] m15: frame N ball X,Y` (the headless
  test's motion ground truth), `[winsrv] canvas: win 2 released (N msgs)`,
  `[demo] m15: canvas released, text mode`.
- [x] Exit: met — `scripts/qmp_m15_test.py` (new headless proof): 15/15 on
  the release AND debug ISO — canvas lifecycle on serial, screendump color
  asserts inside the content box (lime ball ≥250 px, azure wave ≥600 px,
  zero all-channels ≥239 px = the cursor-uniqueness predicate), two
  in-flight samples ≥6 frames apart with ≥500 px differing inside the
  window, ball motion between samples ≥5 px, then teardown → the window is
  a console again (click focuses it, keys land as `[demo] got: ok`) and X
  closes it (`win 2 closed` + demo EOF). All prior suites stay green:
  multitest 18/18 (canvas coexists with the console demo), desktop, tablet
  ×3 boots, type, disk 12/12 + 3/3, input, tray hover 8/8, net PASS, smp
  7/7.
- [x] Compositor polish this milestone forced: winsrv no longer blanks the
  mouse cursor for a WHOLE lap (the old blanket lap-start undraw flickered
  visibly under the canvas message flood). Now `pointer` undoes the
  sprite's save-under at the old spot on a move, `steady_cursor` undoes it
  only when the pending damage intersects the sprite zone, and
  `draw_cursor` saves the under-bg only when the sprite wasn't already
  drawn (a re-save over a drawn sprite leaks its own ink into `mouse_buf`;
  the next undraw ghost-replants it — the tablet pixel test caught this).

### v1.1 · M15.1 — Start-card launcher + task-slot reap ✓ DONE
- [x] The start card's **Demo tile is a real launcher** now (was: focus
  only, one-shot demo forever). Semantics: demo window alive → focus it
  (no duplicates); demo **exited** → winsrv spawns a fresh demo user
  task straight from the baked-in ELF (`sched::spawn_user_task("demo",
  DEMO_ELF)`) and prints `[winsrv] launch Demo (respawned task 5)` —
  confirmed on glass by the human run (window pops back at the same
  spot, canvas re-runs, second X-close + exit). Verified headless by
  `qmp_m15_test.py`, now **21 checks** (phase 9: launch + task-5
  respawn + fresh `owner 5` canvas + 40 new frames + X-close + second
  EOF/exit), with **windowed markers** (serial byte-offset snapshot:
  the respawn run's lines are judged only among lines that arrive
  after it — boot's identical-looking lines can no longer fake passes).
- [x] **Task-slot reap** (the kernel prerequisite, `sched/mod.rs`):
  task ids were monotonic with `MAX_TASKS = 9` slots — every id burns
  at boot, so `task 5 exited` used to be the slot's death; a respawn
  asserted `too many tasks`. Now a *reaper* owns slot recycling:
  `take_id()` pops the free list (only under `TASKS.lock()`), else
  takes `NEXT_ID`; `exit_current`/`kill_task` push the Dead task's id
  back through `release_id` (self-guarded against double release).
  The slot trio is **id-addressed** — `KSTACKS[id]`, `FX_SAVE[id]`
  (seeded once at boot for all slots, so a reused slot inherits a
  clean FPU image for free), mailbox port = id — so `kstack_top_for`
  replaced the allocation cursor and reuse costs nothing. Its
  `install()` keeps the invariant `tasks[id].id == id` everywhere
  (fresh ids append, reused ids replace in place). **LIFO** lifo pop
  → the respawned demo comes back at its boot id: the serial proof
  reads `spawned user task 5 (demo)` exactly like boot.
- [x] **Port hygiene on reuse**: a reused id inherits its predecessor's
  mailbox port, so `ipc::reset_port(id)` clears queued self-messages
  and wakes anyone parked on the port. Lock discipline: the port reset
  never runs while `TASKS` is held (its wake nests the table) — spawn
  is two-phase (id pick under lock → drop → ELF load + maps →
  re-lock for `install`), exits release under the already-held lock.
- [x] **Zero-warning userland** (was the milestone's second ask): every
  warning traced to ONE root cause — userland was compiled against the
  stock soft-float `x86_64-unknown-none` target while RUSTFLAGS forced
  `+sse,+sse2,-soft-float`: the CLI-vs-spec fight tripped the
  rust#162235 ABI-mismatch trio on every crate and put `core`,
  `zune-png` (`#[target_feature(enable="ssse3")]` etc.) and
  `simd-adler32` (stdarch) on the future-incompat list (rust#117938 —
  SSE is illegal on a soft-float target; updating crates was a dead
  end, both are newest on crates.io). Fix = a **JSON target spec**,
  `targets/x86_64-greenlime-sse.json` — stock spec verbatim, but SSE/SSE2
  baked INTO the target ("features": "-mmx,+sse,+sse2", no
  `rustc-abi: "softfloat"`): Makefile drives it via `UE_TARGET` +
  `-Zjson-target-spec`, `kernel/build.rs` locates the ELFs through
  `GREENLIME_UE_DIR` (cargo keys a JSON-target build dir by the file
  stem). Result: userland builds emit **zero warnings** on debug and
  release. The kernel **stays** on `x86_64-unknown-none` by design
  (M13 trust shape: IRQ paths can never touch XMM), so its single
  `core v0.0.0` future-incompat note remains — expected, documented
  here once. Also modernized: `greenlime-fuzz`'s panic hook uses
  `info.message().as_str()` (stable 1.81) instead of deprecated
  `payload()` ("never returns anything useful"), falling back to
  `(formatted)` since alloc-free panics can't materialize args.
- Note: the taskbar cluster centers on `(n−1)·44+40` with n = 4 +
  clients — with the demo window CLOSED the start button sits at
  (574,776), not the boot-time (552,776). Tests must mirror the
  formula (GUI.md), and the start card now prints
  `[winsrv] start card open` / `[winsrv] start card closed` so dead
  clicks vs. open cards are distinguishable in serial forever.

## After 1.1 — 1.2 candidates (no promises, rough order)
- **M16 · `.gle` app format + EXEC** — the third-party app truck: a
  lunchbox file (ELF payload + sticker header: name / version / icon /
  requested caps), a `gle-pack` CLI, a `greenlime-app-template` repo, a
  `greenlime-client` dev crate (open window / blit frame / read input),
  `docs/GLE.md`, and the kernel-side EXEC syscall so a dropped
  `name.gle` runs and shows up in the start card. Kernel keeps parsing
  plain ELF — it unwraps the container, checks the caps, then execs.
- **virtio-gpu 2D driver** — QEMU's real paravirtual display device:
  dirty-rect flushes instead of full-frame presents, a hardware cursor
  plane (retires the hand-composited cursor), EDID / runtime resolution.
- **"GLite"** — our own SSE2 software-3D pipeline (vertex transform +
  rasterizer) exposed to apps like `WIN_BLIT` is: a real rotating cube.
- **virgl / OpenGL passthrough** — long-term; needs a Mesa-class guest
  driver stack. Written down so nobody accidentalizes it.
- **Browser ladder** — M17 mini browser (fetch → small HTML → layout →
  `WIN_BLIT` canvas, http-only at first) → QuickJS / JavaScriptCore port
  → WebKit-adapter engine port (Haiku's road). Chrome/Brave/Firefox
  *themselves* need a full GreenPOSIX floor first (mmap, threads+futex,
  dynamic linking, gigabytes) — that's the M18–M20 class of work.

## After 1.0
* aarch64 port (`arch/aarch64/`), UEFI self-host, package manager, POSIX shim,
  Linux/Wine-like compat, signed updates.

## How to work
1. One milestone at a time. Don't start GUI before userspace.
2. Each PR: code + QEMU log + `docs/` update.
3. ABI breaks only with major version bump + migration note.
