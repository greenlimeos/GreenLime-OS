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

## Open / accepted

- **GLS-007 (Medium) — mailbox spray suicides the shell.** Sender checks
  fail closed via `exit(3)`; anyone may send to port 4. Needs a kernel
  sender allowlist + skip-with-budget in the shell. Deferred.
- **GLS-009 (Info) — dev "capability bit" is decoration.** Console chosen
  by client-controlled bit 63; no handle table. Harmless today (consoles
  are world-writable by design); do not build auth on it.

## Audited and cleared

Separate per-task `cr3`, supervisor-only kernel half, SMEP+SMAP, NX on
stack/data, no `mmap` syscall; sound single-core syscall trampoline (IF
masked, DF-independent copies); `checked_add`+`USER_END` user copies under
STAC; kernel-stamped senders; clamped payloads/offsets; `checked` shell
number parsing; drop-on-full IRQ rings; FAT32 caps (`..` rejected, chain
cycles byte-capped); monotonic handles/ids (no ABA reuse).

## v1.0 landmines

- Syscall trampoline globals (`next_kstack_top`, `user_rsp_slot`) are
  safe only single-core, no preemption — SMP must make them per-CPU.
- `recent: Vec<App>` is currently write-only (menu no longer shows it).
