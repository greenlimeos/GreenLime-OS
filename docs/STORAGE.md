# GreenLime storage (M14, v1.1): PCI -> virtio-blk -> FAT32 -> VFS.

## Stack

```
shell `echo x > /disk/f`, `rm`, `mkdir`, `ls /disk`, `cat` (M14: disk
  mutations OP_CREATE/DELETE/MKDIR/SETSIZE + WRITE; admin-gated)
  │ IPC (existing ports; no new syscalls)
  ▼
servers/vfs.rs                             routes /disk/... to FAT32, else ramfs//ps
  │ direct calls (same kernel task styles as before)
  ▼
fs/fat32.rs                                FAT32, 512B sectors, no LFN;
                                           read (since v0.7) + full write
                                           path (M14)
  │ drivers::storage::read_block / write_block(lba, [u8; 512])
  ▼
drivers/virtio_blk.rs                      legacy PCI transport, 1 queue,
                                           1 outstanding request, POLLING;
                                           VIRTIO_BLK_T_IN / T_OUT headers
  │ ports 0xCF8/0xCFC + BAR0 I/O
  ▼
drivers/pci.rs                             config-space scanner + class names
```

## Why legacy virtio (not modern)?

Legacy (port I/O, single PFN queue) is ~350 lines and QEMU still provides
it on transitional `virtio-blk-pci` devices. Modern (capabilities, MSI-X)
is the upgrade path when input/net devices need it too.

## Why polling, not interrupts?

One outstanding request + a bounded spin with a loud timeout panic beats
an IRQ path for bring-up (no APIC routing, no reentrancy). Interrupts move
in with network/input drivers.

## DMA notes

* All device-visible memory is single PMM frames (< 4 GiB, 32-bit fields).
* Queue: 2+ contiguous frames (retry loop), zeroed before handing over.
* Bounce frame per I/O: [header 16][data 512][status 1].
* Fences around notify/poll; volatile ring indices (device writes concurrently).

## FAT32 write path (M14)

Same 512-byte sectors, no LFN (8.3 uppercase). What the writer does and
does not do:

* First-fit FAT allocation with a resume cursor (`scan_hint`); every
  fresh cluster is marked `EOC` on allocation, so even a partial
  operation leaves a legal chain end.
* Directory mutations are terminator-correct: E5 slots are reused as-is;
  consuming a `0x00` end-of-dir marker (mid-chain) rewrites the marker
  one slot later; consuming the last slot of the chain appends a fresh
  zeroed cluster to the dir chain first (fresh cluster = new marker).
* `mkdir` builds one cluster with `.`, `..` and the terminator; `..`
  points at the parent's cluster, or 0 when the parent is the root
  (root is not a cluster — classic FAT32 convention; our own reader
  skips dot entries by name anyway).
* Deletes: `rm` frees the data chain (files) or requires the dir to hold
  only `.`/`..` (empty dir check), then marks the entry `E5`. Root can't
  be deleted; a non-empty dir refuses and stays intact.
* Writes: pwrite at an offset (OP_WRITE arg1). Past-EOF writes extend the
  file with fresh clusters; the gap between the old size and the offset
  is zero-filled (holes read as zeros). Whole sectors skip the read;
  partial sectors are read-modify-write. `OP_SETSIZE` extends
  (pre-zeroed) or truncates (excess clusters freed, chain re-pointed at
  `EOC`).
* Durability: writes go straight through the polling block driver — no
  page cache. When the ioctl returns, the disk is written; a full QEMU
  reboot later re-reads the same bytes (`scripts/qmp_disk_test.py`
  phases 1→2 prove this on the same image).
* Explicitly NOT maintained: the second FAT copy (FAT[1]) and the
  FSInfo-sector free counts. Stale FSInfo is legal (clients rescan);
  a future release can mirror both for external-tool strictness.
* File cap: 1 MiB (matches the read cap). Stamps are fixed
  2000-01-01 00:00 — no RTC dependency inside the FS.

## Test disk

`make disk` / `scripts/make-disk.sh` builds `dist/disk.img` (32 MB FAT32
via mtools) from `testdata/disk/`. QEMU gets `-drive file=dist/disk.img,
if=virtio,format=raw` plus `-boot order=d` (otherwise SeaBIOS tries the
blank-ish disk first and never reaches the CD).

## Deferred (explicit)

* AHCI full driver: controller is detected + reported by the PCI scan;
  read path stays virtio-blk until real-hardware work starts.
* virtio-input (keyboard/mouse): PS/2 scancodes still cover v0.7; a real
  event queue belongs with the GUI input loop.
* ext2: FAT32 covers the test story; second FS when something needs it.
