# GreenLime storage (v0.7): PCI -> virtio-blk -> FAT32 -> VFS.

## Stack

```
shell `ls /disk`, `cat /disk/f`          (OP_READDIR / OPEN+READ, no new syscalls)
  │ IPC (existing ports)
  ▼
servers/vfs.rs                             routes /disk/... to FAT32, else ramfs//ps
  │ direct calls (same kernel task styles as before)
  ▼
fs/fat32.rs                                read-only FAT32, 512B sectors, no LFN
  │ drivers::storage::read_block(lba, &mut [u8; 512])
  ▼
drivers/virtio_blk.rs                      legacy PCI transport, 1 queue,
                                           1 outstanding request, POLLING
  │ ports 0xCF8/0xCFC + BAR0 I/O
  ▼
drivers/pci.rs                             config-space scanner + class names
```

## Why legacy virtio (not modern)?

Legacy (port I/O, single PFN queue) is ~350 lines and QEMU still provides
it on transitional `virtio-blk-pci` devices. Modern (capabilities, MSI-X)
is the v0.8+ upgrade path when input/net devices need it too.

## Why polling, not interrupts?

One outstanding request + a bounded spin with a loud timeout panic beats
an IRQ path for bring-up (no APIC routing, no reentrancy). Interrupts move
in with network/input drivers.

## DMA notes

* All device-visible memory is single PMM frames (< 4 GiB, 32-bit fields).
* Queue: 2+ contiguous frames (retry loop), zeroed before handing over.
* Bounce frame per I/O: [header 16][data 512][status 1].
* Fences around notify/poll; volatile ring indices (device writes concurrently).

## FAT32 limits (v0.7)

Read-only. 512-byte sectors. No LFN (8.3 uppercase). No write support
(journaling + allocation tables are a v1.x project). Empty files/dirs
(cluster 0) handled explicitly (no LBA underflow).

## Test disk

`make disk` / `scripts/make-disk.sh` builds `dist/disk.img` (32 MB FAT32
via mtools) from `testdata/disk/`. QEMU gets `-drive file=dist/disk.img,
if=virtio,format=raw` plus `-boot order=d` (otherwise SeaBIOS tries the
blank-ish disk first and never reaches the CD).

## Deferred (explicit)

* AHCI full driver: controller is detected + reported by the PCI scan;
  read path stays virtio-blk until real-hardware work starts.
* virtio-input (keyboard/mouse): PS/2 scancodes still cover v0.7; a real
  event queue belongs with the v0.8 GUI input loop.
* ext2: FAT32 covers the test story; second FS when something needs it.
