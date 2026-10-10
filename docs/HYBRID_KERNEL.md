# GreenLime OS — Hybrid kernel explained

## Why hybrid?

* Pure monolith (Linux/Windows): fast, but a bad driver takes down everything.
* Pure microkernel (seL4/Minix): safe, but lots of IPC overhead early.
* Hybrid (GreenLime, like Windows NT/XNU): start with servers in kernelspace
  for speed + simplicity, move them to userspace when IPC + scheduler are solid.

You get to v0.5 fast without painting yourself into a corner.

## Kernelspace (ring 0, `kernel/`)

Always resident, privileged:
* `arch`: GDT/IDT/paging, context switch, `in/out`, `cli/sti`
* `mm`: frame allocator, address spaces, heap
* `sched`: tasks, ready queues, timer preemption
* `ipc::core`: channels, ports, capability checks (must be fast + correct)
* `syscall::dispatch`: the ONLY gate from ring 3
* `servers/*`: VFS, dev, net — in-kernel threads for now
* `drivers/*`: interrupt handlers, MMIO, DMA

Kernelspace can use `unsafe`. Every `unsafe` needs a `// SAFETY:` comment.

## Userspace (ring 3, `userland/`, `libs/`)

Unprivileged ELF programs:
* `userland/init` (pid 1): mounts, spawns shell, reaps zombies
* `userland/shell`: builtin commands, spawns apps via IPC to VFS
* Future: `windowserver`, `netd` (migrated out of kernel), apps

Userspace can ONLY:
```rust
greenlime_abi::syscall::syscall(nr, a0, a1, a2) // -> Result<usize, Error>
greenlime_abi::ipc::send(recv, &msg)
greenlime_abi::ipc::recv(port, &mut msg)
```
No `x86_64` privileged instructions, no raw pointers to kernel memory.

## IPC — the backbone

```rust
// libs/greenlime-abi/src/ipc.rs
pub struct Message { pub kind: u32, pub caps: [u32; 4], pub payload: [u8; 256] }
```

* `Channel`: MPSC bounded queue in kernel, sender = anyone with cap, receiver = server.
* `Port`: named endpoint (`"vfs"`, `"dev/fb0"`), looked up via `dev` server.
* `Capability`: (object_id, rights bitmask). Checked on every send/map.
* Sync: `send` blocks if full, `recv` blocks if empty — scheduler sleeps task.

Servers handle messages, never raw syscalls from random tasks:

```
shell --ipc_send("vfs:open /bin/echo")--> vfs server --> fs driver --> reply
```

## Migration path (no rewrite)

1. v0.5: `servers::vfs::handle(msg)` is a kernel function.
2. v0.7: same logic compiled as userspace ELF, registered as `"vfs"` port.
3. Kernel `syscall::dispatch` routes to port, not function — user code unchanged.

Keep `greenlime-abi` stable and migration is config, not code.

## Security rules

1. User pointers validated: `vmm::check_user_slice(ptr, len)` before copy.
2. `copy_from_user` / `copy_to_user` only, never deref user ptr directly.
3. Per-process page tables: kernel mapped `GLOBAL|NO_USER`, user `USER|NO_EXEC` as needed.
4. Capabilities, not UIDs, for objects. UID 0 does not bypass checks.
5. All syscall numbers + error codes live in ABI crate. Kernel never invents new ones silently.
