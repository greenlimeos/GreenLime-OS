# GreenLime OS — Roadmap to competing with Windows

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

## v0.8 — GUI: window server + compositor + LimeDE desktop (IN PROGRESS)
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

## v1.0 — SMP + net + hardening
- [ ] AP startup (Limine MP), per-CPU scheduler, IPIs
- [ ] VirtIO-net, TCP/IP (smoltcp), DNS/DHCP
- [ ] Capabilities enforced, W^X, KASLR seed, fuzz syscalls
- Exit: two cores print ticks, ping works, syscall fuzz 1h clean.

## After v1.0
* aarch64 port (`arch/aarch64/`), UEFI self-host, package manager, POSIX shim,
  Linux/Wine-like compat, signed updates.

## How to work
1. One milestone at a time. Don't start GUI before userspace.
2. Each PR: code + QEMU log + `docs/` update.
3. ABI breaks only with major version bump + migration note.
