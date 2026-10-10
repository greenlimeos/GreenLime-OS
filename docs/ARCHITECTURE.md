# GreenLime OS — Architecture
# Read this before writing code.

## 1. Goals

* From-scratch hybrid kernel in Rust (`#![no_std]`, `#![no_main]`).
* Strict kernelspace / userspace split from day one.
* Expandable: new arch, driver, server, or syscall = new file, not rewrite.
* Boots in QEMU via Limine (BIOS + UEFI). Real hardware later.
* Long-term: GUI, SMP, network, storage. Short-term: small bootable core.

Being honest: competing with Windows 11 is a 10-year, millions-of-lines
project (drivers + apps + compat). This layout lets you grow toward it
without rewrites.

## 2. Hybrid model

```
 +---------------- userspace (ring 3) ----------------+
 | init (pid 1) | shell | gui compositor | apps ...   |  <- ELF, no privileged insns
 +------------------ syscall / IPC -------------------+
 +---------------- kernelspace (ring 0) --------------+
 | syscall dispatch | scheduler | vmm/pmm | ipc core  |  <- always resident
 | servers: vfs/dev/net (in-kernel now, movable later) |
 | drivers: serial/uart, fb, kbd, virtio              |
 +----------------------------------------------------+
```

* Monolith speed now: servers live in kernelspace as modules (`kernel/src/servers/`).
* Microkernel path later: same server code moves to userspace tasks, talks via
  `kernel/src/ipc/` channels. `libs/greenlime-abi` stays stable so user code
  doesn't break.
* Rule: userspace can ONLY talk via `greenlime-abi::Syscall` + `IpcMessage`.
  No direct hardware access, no kernel memory mapping.

## 3. Folder contract

```
kernel/src/main.rs    -> _start -> kmain -> init order only. No logic.
kernel/src/boot.rs    -> Limine requests + boot info. ONLY file that knows Limine.
kernel/src/arch/      -> per-CPU arch. x86_64 now, aarch64 later.
  arch/mod.rs         -> re-export current arch, pub fn init(), halt(), interrupts on/off
  arch/x86_64/mod.rs  -> gdt/idt/paging init stubs
  arch/x86_64/gdt.rs  -> GDT + TSS (ring0/ring3 segments)
  arch/x86_64/idt.rs  -> IDT, exceptions, IRQs, syscall entry (MSR_LSTAR)
  arch/x86_64/paging.rs -> page table helpers (uses x86_64 crate)
kernel/src/mm/        -> memory. pmm.rs (bitmap frame alloc from Limine memmap),
                         vmm.rs (address spaces), heap.rs (linked-list alloc)
kernel/src/sched/     -> cooperative -> preemptive scheduler, Task, context switch
kernel/src/ipc/       -> hybrid backbone: Channel, Message, Port, capability handles
kernel/src/syscall/   -> syscall table, dispatch(user_regs) -> server call
kernel/src/servers/   -> vfs/dev/net stubs. Each server = init() + handle(msg)
kernel/src/drivers/   -> serial.rs (COM1, always first), fb console, pci scan,
                         virtio-blk, storage bring-up, ps/2 input rings.
                         See docs/STORAGE.md, docs/GUI.md.
kernel/src/servers/   -> vfs (ramfs/disk), dev (serial0/term0), winsrv
                         (terminal + taskbar, owns fb after takeover).
kernel/src/userspace/ -> ELF loader, address space switch, ring-3 jump (iretq)
kernel/src/kprint.rs  -> kprint!/kprintln! (serial + fb). Only print path.
libs/greenlime-abi/   -> shared ABI: syscall numbers, error codes, IPC wire format.
                         no_std, no kernel imports. Versioned (ABI v0.1).
libs/greenlime-font/  -> TTF font system: load, glyph cache, fb text (no_std).
                         Used by kernel console (v0.5) and GUI later.
assets/fonts/         -> default-font.ttf embedded at build (see docs/FONTS.md).
userland/init/        -> first user program. Tests syscall path. Keep tiny.
```

Add new code by:
* new arch -> `kernel/src/arch/<arch>/` + 3 lines in `arch/mod.rs`
* new driver -> `kernel/src/drivers/<name>.rs` + 1 line in `drivers/mod.rs`
* new server -> `kernel/src/servers/<name>.rs` + 1 line in `servers/mod.rs`
* new syscall -> number in `libs/greenlime-abi`, handler in `kernel/src/syscall/`

## 4. Boot flow (v0.1)

```
Limine (BIOS/UEFI) -> _start (main.rs) -> boot::init()
 -> arch::init() (GDT/IDT stubs) -> mm::init() (parse memmap later)
 -> drivers::init() (serial) -> kprintln!("GreenLime...")
 -> boot::boot_info() (framebuffer res, memmap entries, hhdm offset)
 -> halt loop
```

Limine gives us: framebuffer, memory map, HHDM offset, bootloader info.
We do NOT parse multiboot, we do NOT write our own bootloader yet.
Custom bootloader is a v2.0 project — Limine gets you to kernel work fast.

## 5. Memory plan (v0.2 next)

* Use `MemoryMapRequest` + `HhdmRequest` from `boot.rs`.
* PMM: bitmap over usable entries, 4KiB frames.
* VMM: higher-half kernel (`0xFFFFFFFF80000000+`), per-process lower half.
* Heap: 1MB bump -> linked-list alloc, then `#[global_allocator]`.
* Never identity-map all memory into userspace. Kernel pages: `PRESENT|WRITABLE|NO_EXEC` for data, user pages: `USER_ACCESSIBLE`.

## 6. Userspace plan (v0.4)

* ELF64 loader in `userspace/elf.rs` (parse, map LOAD segments, build stack).
* `arch::jump_to_user(entry, stack)`: `iretq` to ring3, `RFLAGS.IF=1`, user CS/SS from GDT.
* Syscall entry: `EFER.SCE=1`, `LSTAR=entry`, `STAR` selectors. Numbers in ABI.
  Start with: `0=log, 1=exit, 2=yield, 3=ipc_send, 4=ipc_recv`.
* Capabilities: each Task holds handle table, checked in `syscall::dispatch`.

## 7. What NOT to do

* No `std`, no `println!` in kernel — use `kprint!`.
* No hardware access from `userland/` or `libs/`.
* No arch-specific code outside `arch/` (use traits).
* No big `main.rs` — init order only.
* No GUI in kernel (framebuffer console only). GUI = userspace compositor later.
