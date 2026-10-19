# GreenLime Security (GLS findings)

Threat model: a malicious or buggy **ring-3 task**. Host/QEMU trusted.
Method: code-traced chains + live headless proofs where noted. Hobby-grade:
no sandboxing or user model yet — don't put secrets in it.

## Fixed

- **GLS-001 (Critical) — shell command injection via dev relay.** Any task
  could plant a forged "reply" in dev's mailbox; dev forwarded it as
  trusted typed input and the shell executed it (`kill 2` kills winsrv).
  Fixed: the relay discards until `sender == 2` (`servers/dev.rs`).
- **GLS-002 (High) — unauthenticated Kill.** Any task could kill kernel
  servers (permanent DoS, shell-typable). Fixed: pids < 3 are immortal
  in `syscall_dispatch` (`syscall/mod.rs`).
- **GLS-003 (High) — any fault halted the machine.** Unmapped user
  pointers, UD2, `cli` in ring 3 — all halted via the fault handlers.
  Fixed: ring-3 #PF/#UD/#GP retires the task (`exit_current`), supervisor
  faults still halt. Proven live: a UD2 in ring 3 prints
  `[FAULT] UD from ring 3 — killing task 5`, the task exits (132), the
  machine keeps ticking.
- **GLS-004 (Medium) — VFS handle hijacking.** Handles were global +
  sequential; any task could read/close another's. Fixed: `owner`
  recorded at OPEN, enforced on READ/CLOSE (`servers/vfs.rs`).
- **GLS-005 (Medium) — unbounded terminal line → heap OOM.** Fixed in
  v0.9: `MAX_LINE` (4096) cap in `push_text` (`servers/winsrv.rs`).
- **GLS-006 (Medium) — reader/window hijacking.** `WIN_READ`/`WIN_CLIENT_READ`
  slots were first-come-first-served (keystroke theft + relay wedge);
  any client could write/destroy any window. Fixed: terminal reads only
  from DEV (`sender == 1`); client windows record their creating `owner`,
  enforced on read/write/destroy (`servers/winsrv.rs`). IPC still can't
  kill the terminal (only its X button can).
- **GLS-008 (Low) — ELF loader gaps.** Unbounded phdr table, `vaddr+memsz`
  wraparound, unvalidated entry (trusted blindly by `jump_to_user`).
  Fixed: table bounds + 32-phdr cap, `checked_add`, entry must lie in an
  executable segment (`userspace/elf.rs`).
- **GLS-010 (Low) — disk errors halted.** I/O timeout / bad status
  panicked. Fixed: `read_block` returns false; LBA-capacity guard was
  already there (`drivers/virtio_blk.rs`).

## v1.0 · M11 hardening line

- **CAP_WALL + wallpaper streaming trust boundary.** The wallpaper
  (task 8) streams decoded RGB rows to winsrv via the frozen IPC gate
  with a new capability bit `CAP_WALL = 1<<2` (spawn-time grant, same
  no-escalation rule). The trust split matters more than the cap: the
  kernel **never parses image data** — zune-jpeg runs in ring 3 against
  ring-3 memory. A hostile JPEG kills at worst its own task (retired by
  the GLS-003 fault isolation); winsrv validates only shapes it must
  (target size at SETUP, row bounds at ROWS — clamped, palette-clamped
  so wallpaper pixels can't fake the cursor detector, see `GUI.md`).
- **PendingPark two-phase (the M10 soak fault, now fixed).** The tick
  could convert a task from Parked → Ready while its context was still
  mid-save — a claimer then ran a half-written frame (double fault at
  ~2034 s). `park_current` now parks as `PendingPark` + a per-CPU
  handshake (`smp::park_pending`); scheduler_loop promotes
  PendingPark → Parked at lap top, i.e. only after the switch completed.
  **Deferred (same family, not reproduced live):** the
  `Blocked → Ready` wake in `wake_port` has the same pre-switch
  exposure; a waker racing the switch could Ready a task whose context
  is still saving. The M11 debug campaign produced deterministic ring-0
  faults in this area and traced them ALL to the kstack overflow below —
  the wake window itself never fired and stays deferred with the
  two-phase patch as the ready design if it ever does.
- **GLS-012 (High, fixed) — 32 KiB kernel stack marginality.** ABI v6
  messages (~2 KiB) are staged per frame; at `-O0` net's broker chain
  reached 32,576/32,768 bytes and the NEXT deep call wrote past the slot
  bottom into the neighbouring task's saved switch context (the
  deterministic `rip=0x0` debug fault loop; in release `-Oz` the same
  code shows no fault, which is exactly what made it appear
  timing-dependent). Fixed: `KSTACK_SIZE` = 64 KiB for every task
  (uniform budget, ~576 KiB .bss total) — and the class of bug is now
  *visible* instead of silent: every supervisor page fault prints 32
  top-of-stack words + the full task table (`[FAULT] stk[i]`,
  `[faultdump]`) before halting. Lesson: with by-value IPC structs this
  size, per-frame staging cost is a *scheduler* concern; re-run the
  debug suite after any change that deepens a syscall chain.
- **Fuzz panic detail.** The fuzz task's `#[panic_handler]` used to log
  a fixed string — the first M11 debug campaign misread its arithmetic
  seed overflow (`pid * const`, wrapping intended) as scheduler
  damage. It now prints the payload + `file:line` (`[FUZZ-PANIC] … @
  main.rs:134`), and the seed line uses `wrapping_mul` (the wrap was
  the intended seed mixing). Any future fuzz death is self-diagnosing.

## v1.0 · M10 hardening line (SMP + net)

- **GLS-011 (High) — in-range-but-unmapped user pointers faulted in
  kernel mode.** `valid_user_range` verified arithmetic only (bounds +
  `checked_add`); a pointer inside the user half but on an unmapped
  page paged the kernel under STAC — a supervisor #PF, machine halt.
  Found live on the first fuzz boot. Fixed: `vmm::user_range_ok` walks
  the caller's page tables and every overlapped page must be present +
  user-accessible (+ writable for IPC recv), all before the STAC
  window. sys_log + IPC send/recv probe; a bad pointer is a clean
  `u64::MAX`. Walk lesson (found live): never descend through a 4 KiB
  leaf PTE — its phys page is data, and following it dereferences
  MMIO-backed entries (IOAPIC window in user CR3s) as a table.

- **Capabilities.** Spawn-time-only grants (`sched::caps`): kernel
  servers hold ALL; init/shell get CAP_KILL|CAP_NET; demo gets CAP_NET;
  the fuzzer gets nothing (the Denied paths are part of its surface).
  No runtime grant API exists, so there is no escalation path.
  Enforcement: `Kill` refuses without CAP_KILL (pids < 3 remain
  immortal per GLS-002); the net broker refuses every port-18 op from a
  sender without CAP_NET (kernel-stamped sender is the truth).
- **W^X.** User half: the ELF loader refuses any segment that is both
  writable and executable (no legitimate RWE section in a static
  binary). Boot-time audit of the kernel's own mapping walks the ACTUAL
  PTEs: text must be R+X, a data page RW+NX — verifies Limine's mapping
  instead of assuming it (`[mm] wx:` line at boot).
- **KASLR seed.** TSC folded through splitmix64 at boot
  (`mm::kaslr_seed`, fingerprint-only log line). Feeds per-task user
  stack-top jitter (0–511 pages, mixed with task id) — the fixed
  stack-top assumption no longer holds — and the smoltcp seed (DHCP
  xids / ISNs). NOT crypto entropy.
- **Syscall fuzz.** `greenlime-fuzz` (task 7, unprivileged by design)
  hammers random syscall numbers, junk pointers (mapped / unmapped /
  boundary / kernel-half), IPC send+recv with pointer chaos, and Denied
  kills; self-mailbox traffic is flow-controlled (outstanding counter)
  so the fuzzer can never wedge on the blocking recv. It is PACED
  (`Syscall::Park`, ABI v5): 64-iteration bursts, then sleep to the next
  tick — the scheduler's idle `hlt` gets to run instead of QEMU being
  pegged at 100%, and the serial heartbeat drops from 80 lines/s to
  ~2/min. One-hour soak (paced build): zero `[FAULT]`, zero `[PANIC]`,
  fuzzer alive throughout.

## Open / accepted

- **GLS-007 (Medium) — mailbox spray suicides the shell.** Sender checks
  fail closed via `exit(3)`; anyone may send to port 4. Needs a kernel
  sender allowlist + skip-with-budget in the shell. Deferred.
- **GLS-009 (Info) — dev "capability bit" is decoration.** Console chosen
  by client-controlled bit 63; no handle table. Harmless today (consoles
  are world-writable by design); do not build auth on it.
- **NET-01 (Medium, accepted for now) — hostile packets halt the machine.**
  The net stack runs under `panic = "abort"`: any smoltcp parse path that
  panics on a malformed frame (DHCP/ARP/ICMP from the virtual wire) kills
  the whole OS instead of dropping the packet. Mitigate before trusting:
  fuzz the parsers headlessly, then convert hot paths to `Result` so bad
  input drops instead of halting.

## Audited and cleared

Separate per-task `cr3`, supervisor-only kernel half, SMEP+SMAP, NX on
stack/data (boot-audited, `[mm] wx:`), no `mmap` syscall; page-probed
user copies under STAC (`user_range_ok`, GLS-011); sound single-core
syscall trampoline (IF masked, DF-independent copies); `checked_add`+
`USER_END` bounds as the first gate; kernel-stamped senders; clamped
payloads/offsets; `checked` shell number parsing; drop-on-full IRQ
rings; FAT32 caps (`..` rejected, chain cycles byte-capped); monotonic
handles/ids (no ABA reuse); spawn-time-only capabilities.

## v1.0 landmines

- Syscall trampoline globals (`next_kstack_top`, `user_rsp_slot`) are
  safe only single-core, no preemption — SMP must make them per-CPU.
- `recent: Vec<App>` is currently write-only (menu no longer shows it).
