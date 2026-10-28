# GreenLime storage (M14 + M16 + M19, v1.2): PCI -> AHCI (else virtio-blk) -> FAT32 -> VFS.

## Stack

```
shell `echo x > /disk/f`, `rm`, `mkdir`, `ls /disk`, `cat` (M14: disk
  mutations OP_CREATE/DELETE/MKDIR/SETSIZE + WRITE; admin-gated)
Files app (M16): places / trash / restore / empty (kernel-internal)
  │ IPC (existing ports; no new syscalls)
  ▼
servers/vfs.rs                             routes /disk/... to FAT32, else ramfs//ps
  │ direct calls (same kernel task styles as before)
  ▼
fs/fat32.rs                                FAT32, 512B sectors; read (v0.7) +
                                           full write path (M14) + REAL LFN
                                           read+write (M16)
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

## FAT32 write path (M14; LFN since M16)

Same 512-byte sectors. Directory entries now carry real LFN sets (see
above); what the writer does and does not do:

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

## FAT32 long file names (M16)

Real spec-shaped LFN, both directions, replacing the "uppercase 8.3
only" reader/writer. Display names keep the case users typed
(`note.TXT` stays `note.txt`, `Recycle Bin` stays `Recycle Bin`);
the on-disk short entry stays a legal 8.3 alias so any external FAT
tool (mtools, Windows) still sees the file.

**On-disk layout (what the spec actually says)** — for a directory
chain of 32-byte slots:

```
slot n-2   LFN ord 1, 0x40 flag («first part»), chars  0..13
...        LFN ord 2..k-1 (unflagged), chars 13s..13s+13
slot n-1   LFN ord k (unflagged)           ← directly ABOVE the short
slot n     short 8.3 entry (attr real)     ← the entry itself
```

So ordinals ASCEND physically toward the short entry, the **0x40 flag
rides ord 1 only** (the slot furthest from the short entry), and the
slot directly above the short entry carries the HIGHEST ordinal,
unflagged. Set `s` (0-based) holds characters `[13s .. 13s+13)` and is
written at offset `offs[s]` with `flag|ord = (0x40 if s==0) | (s+1)`.
Checksums: every LFN slot carries `lfn_checksum(short_name11)` (the
classic sum-of-two-rotated-u8s); a set whose checksum doesn't match
the short entry is ignored byte for byte.

* **Decoder** (`lfn_of_slot`): walking UP from the short entry, reads
  LFN slots (attr `0x0F`) with matching checksum, requires ordinals to
  descend by exactly one as it walks out (`ord+1 == prev_ord`), stops
  at the flagged `0x40|1` slot, reassembles the UTF-16 units (5 + 6 +
  2 fields per slot, little-endian units), stops at the `0x0000`
  terminator, refuses any unit `> 0x7F` (the FS namespace is ASCII —
  a malformed/foreign set falls back to the uppercased short form
  instead of tofu). `entry_matches` takes that decoded truth first,
  then the plain 8.3 comparison.
* **Writer** (`create`/`mkdir`): `short_alias(dir, name)` produces the
  8.3 short entry — a name that is representable in 8.3 and
  unambiguous uses it directly; otherwise the classic fold (uppercase,
  illegal chars → `_`, first dot splits base/ext, base trimmed to 6,
  `SHORT~N` growing N=1..9 until free in the dir). LFN sets are
  reserved together with the short entry via `dir_run(dir, k+1)`
  (contiguous free slots; appends a fresh dir cluster when no run
  fits, re-establishing the `0x00` end-marker afterwards), then
  `write_entry_set` writes the LFN chain + short entry atomically with
  an `undo_set` (all touched slots → 0xE5) on partial failure.
* **Deletes** (`del_set`): from the short slot, walk up while
  `attr==0x0F` && checksum matches — E5 every slot of the set, then
  the short entry, so the allocator keeps seeing plain-free E5 runs
  (stale LFN parts wedged the first implementation).
* **Names on disk are byte-verified**: the whole engine was debugged
  by byte-inspecting real volumes with a FAT32 inspector script
  (header, FAT chains, dir slot dumps) — the bugs it caught: the
  original decoder walking sets BACKWARDS (rejected every real set →
  UI showed `RECYCL~1`/`DOCUME~1`/`M14.TXT`), the writer flagging 0x40
  on every slot, and `del_set` requiring the flag on every slot.
  Boot-time `ensure_places` rooted six duplicate place sets into the
  disk root (`DOCUME~1`…`DOCUME~6`) before the fixes; fresh volumes
  don't carry that cruft.
* Limits: names ≤ 13·LFN_MAX_SETS−1 chars, ASCII-only for LFN truth;
  128-slot-set sanity cap; everything else (stamps, folder semantics)
  unchanged from M14.

## Places + Recycle Bin (M16, on the real disk)

The boot-time `vfs::ensure_places()` creates the explorer's four
directories on the FAT32 volume (Desktop / Documents / Downloads /
`Recycle Bin` — `BIN_DISPLAY`), so they exist for every later boot:
`[vfs] places: N new` (N = however many were missing, 0 once grown
in). RAM is never the home of these paths — that is the whole point:
trashed files survive `rm dist/disk.img`-only reboots (proven by the
user by hand; the automated suite boots its own separate volume).

**Trash scheme** (`trash` / `untrash` / `empty_bin`, files only —
directories are not trashable yet, that's an M16.x pass):

* Trash: move semantics via read+put+delete inside `DISK.lock()` —
  `read_file` the source, `vol_put` the data into `/Recycle Bin/<name>`
  (name de-duplicated through `free_bin_name`: `note.txt`,
  `note.txt (2)`, … up to `(8)` → refuse), `vol_put` a sidecar
  `/Recycle Bin/<name>.orig` holding the FULL SOURCE DIR
  (`/disk/Documents`), then delete the original. Any mid-flight
  failure un-does every already-written piece (best effort, loud
  `[vfs] trash /a -> b` proof only on full success).
* The bin view keeps the sidecar ITSELF invisible (`winsrv` filters
  `.ends_with(".orig")` and never lists dirs the filter would confuse:
  `list_dir` names, sorted dirs-first by the Files window anyway).
* Restore: read both files, decode `.orig` as UTF-8 (`/disk…` prefix
  required, else fallback to the disk root), refuse when the target
  name still exists in that dir (`entry_info` — an occupied target is
  someone else's, never overwritten), put the file, delete BOTH the payload and
  the sidecar, printing `[vfs] restore <name> -> /disk/...`.
* Empty: `list_dir` the bin, delete every non-`.orig`, non-dir entry
  + its sidecar, count them, `[vfs] bin emptied (N items)`.
* Guard at the door: trashing the bin itself (or anything in it via
  the prefix check) is refused; disk-less boots fail closed.
* The Files UI wraps these: `[files] trash <name> ok|refused`,
  `[files] restore <name> ok|refused`, `[files] bin empty n=N`, plus
  the navigation proofs `[files] show <path> items=<n>`.
* `vol_put`/`vol_del` are the locked-volume helpers (they never
  re-lock); `disk_ok()` gates the Files UI's place clicks (a disk-less
  boot: places click → `[files] place unavailable (no disk)`).

## Test disk

`make disk` / `scripts/make-disk.sh` builds `dist/disk.img` (32 MB FAT32
via mtools) from `testdata/disk/`. QEMU gets `-drive file=dist/disk.img,
if=virtio,format=raw` plus `-boot order=d` (otherwise SeaBIOS tries the
blank-ish disk first and never reaches the CD).

## Deferred (explicit)

* AHCI: fully shipped (`kernel/src/drivers/ahci.rs`, M19) — the driver
  takes over a firmware-owned port (engine stop dance + COMRESET), reads
  capacity from IDENTIFY (LBA48, words 100/204), and serves block
  read/write with READ/WRITE DMA EXT over bounce frames. `storage` tries
  AHCI first, virtio-blk stays the fallback. One hard-won detail lives in
  the code comment: the PRDT entry carries the byte count (DBC) in dword
  **3** with dword 2 reserved — DBC in dword 2 delivers exactly byte 0 of
  the transfer (libata's `struct ahci_sg` is the reference). 3-case
  receipt: `scripts/qemu-ahci-test.sh` (sata-only, dual preference,
  empty-controller neutrality — all green).
* virtio-input (keyboard/mouse): PS/2 scancodes still cover v0.7; a real
  event queue belongs with the GUI input loop.
* ext2: FAT32 covers the test story; second FS when something needs it.
