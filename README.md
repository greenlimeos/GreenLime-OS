# GreenLime OS 🟢🍋

A from-scratch hybrid kernel + OS in Rust. Modern, expandable, with strict
kernelspace / userspace separation. Long-term goal: compete with Windows 11.
Short-term goal: boot in QEMU, print, manage memory, isolate userspace.

> Status: v0.1.0 — boot skeleton. Boots via Limine, prints via serial +
> framebuffer, halts. All subsystems are stubbed but wired for growth.

## Quickstart (WSL on Windows 11)

You build and run **inside WSL** (Ubuntu). PowerShell can't build this.

```bash
# 1. Open Ubuntu (WSL) and cd to this project:
#    project lives at /mnt/c/Users/Denis/Desktop/GreenLime OS
cd "/mnt/c/Users/Denis/Desktop/GreenLime OS"

# 2. One-time setup (installs Rust nightly, QEMU, xorriso, Limine):
bash scripts/setup-wsl.sh

# 3. Build + make ISO + run in QEMU:
make run
```

Expected: QEMU window + serial output:
```
[boot] GreenLime OS v0.1.0
[kernel] hybrid core online: kernelspace | servers | userspace
...
```

Press `Ctrl+A, X` to exit QEMU.

## Layout

```
GreenLime OS/
  kernel/            # hybrid kernel (no_std, no_main) — kernelspace
    src/
      boot.rs        # Limine requests: framebuffer, memmap, hhdm, stack
      arch/          # arch isolation: x86_64 (gdt/idt/paging), aarch64 later
      mm/            # pmm (frame alloc), vmm, heap
      sched/         # scheduler, tasks, context switch
      ipc/           # hybrid IPC: channels, messages, ports (core primitive)
      syscall/       # userspace gate (syscall numbers in libs/greenlime-abi)
      servers/       # in-kernel hybrid services: vfs, dev, net (later user servers)
      drivers/       # serial, keyboard, display, virtio
      userspace/     # ELF loader, ring-3 jump, capabilities
      kprint.rs      # kernel console
  libs/
    greenlime-abi/   # shared kernel<->user ABI (syscalls, IPC structs) — no_std
  userland/
    init/            # PID 1, first userspace program (ELF)
    shell/           # future shell
  bootloader/
    limine.conf      # Limine menu entry
  docs/
    ARCHITECTURE.md  # read this first
    ROADMAP.md
    HYBRID_KERNEL.md
  scripts/
    setup-wsl.sh
    run-qemu.sh
```

Kernelspace vs userspace rule: `kernel/` can use `unsafe` + privileged
instructions. `libs/greenlime-abi` + `userland/*` must be safe-only and talk
to the kernel only via `syscall` / `ipc`. Never map kernel memory into user
page tables.

## Hybrid kernel model

* Microkernel-like IPC + servers, monolith-like speed (servers start in
  kernelspace, can migrate to userspace later).
* Core primitives: `ipc::Channel`, `syscall::dispatch`, `sched::Task`.
* Servers: `servers::vfs`, `servers::dev`, `servers::net` — today stubs,
  tomorrow isolated tasks.

See `docs/ARCHITECTURE.md` and `docs/HYBRID_KERNEL.md`.

## Roadmap

1. v0.1 boot (now): Limine -> serial+fb print -> halt ✅
2. v0.2 memory: PMM + paging + heap + memmap from Limine ✅
3. v0.3 interrupts: GDT/IDT, timer, keyboard IRQ ✅
4. v0.4 userspace: ring-3, ELF loader, syscalls (`write`, `exit`, `yield`) ✅
5. v0.5 IPC + servers: channels, VFS + dev server, `init` + shell ✅
6. v0.6 storage: PCI, virtio-blk, FAT32 (`ls`, `make disk`) ✅
7. v0.7 fonts: `greenlime-font` (TTF, glyph cache) + shell editing ✅
8. v0.8 GUI: window server, compositor, LimeDE desktop — Win11-style
   taskbar, wallpaper, full Start menu, multi-window focus/drag/resize ✅
9. v1.0 driver model, SMP, network, storage

See `docs/ROADMAP.md`.

## Commands

```bash
make build   # cargo build kernel
make iso     # build ISO at dist/GreenLime.iso
make run     # iso + qemu BIOS boot, serial to stdio
make run-uefi # UEFI boot with OVMF (if installed)
make run-release # optimized build (much faster under emulation)
make clean
```

## Requirements

* WSL2 + Ubuntu 22.04/24.04
* Rust nightly (`rust-toolchain.toml` pins it)
* `qemu-system-x86`, `xorriso`, `mtools`, `git`, `curl`

## License

MIT — see LICENSE.
