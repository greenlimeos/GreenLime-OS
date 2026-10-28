# .gle — GreenLime apps (M17, v1.2 / M19, v2)

A `.gle` file is a **lunchbox for an app**. Inside it:

1. a **sticker** — the app's name, version, author (plain ASCII text),
2. a **button icon** — 28x28 raw pixels, baked ahead of time,
3. the **app itself** — a normal, static GreenLime program (ELF),
4. *(v2 only)* **any assets the app ships** — data files the kernel
   maps straight into the app's address space, byte for byte.

That's all it is. One flat file, no installer, no registry, no scripts.
Put it on the disk, double-click it in Files (or `exec` it in a
terminal), and GreenLime checks it, unpacks it, and starts it.

There are two generations of the box:

- **v1** — sticker + icon + ELF. Everything the M17 runtime needs.
  Still packed (when an app has no assets) and still runs, forever.
- **v2** — same sticker law, any number of **assets**, delivered as
  read-only memory the app reads with one `count()` call. This is the
  format for apps that carry real payload bytes (fonts, scripts,
  models, maps — the things Steam/Unity/EPIC-style launchers bundle).

The kernel **never** decodes PNGs, never resizes pixels, never executes
an asset, and never trusts the sticker for anything security-related.
The box is checked with a CRC (whole container AND every asset);
anything tampered or truncated is refused loudly, not crashed-into.

---

## Port your app in 3 steps

You need WSL with the toolchain (same as building the OS at all).

**1. Copy the template.** `userland/hello` IS the template — it's a
complete, working v2 app with two assets:

```
cp -r userland/hello userland/myapp
```

Change two things:
- `userland/myapp/Cargo.toml` — `name`, and the `[[bin]]` name
  (`greenlime-myapp`),
- `userland/myapp/app.json` — the sticker:

```json
{
  "name": "myapp",
  "version": "1.0",
  "author": "you",
  "icon": "icon.png",
  "gui": true,
  "assets": ["assets/proof.txt", "assets/blob0.bin"]
}
```

`name` is what the taskbar chip + serial log show. **ASCII only**, 1–24
characters. `version` ≤ 8 chars, `author` ≤ 32, `icon` may be `null`
(an iconless app is fine — remember to set `"icon": null`), and
`"gui": false` is allowed for a headless app that just logs.

**`assets` decides the format:** list it → packed **v2** (the paths'
positions become the app's asset indexes, in that order); leave it out
→ packed **v1**, exactly like before. Paths are relative to the
manifest. On disk, v2 stays under the same 1 MiB FAT32 **write** cap
per file; **reads** may be up to 8 MiB, which is the v2 container cap.

**2. Build it** (add the new crate to `members` in the root
`Cargo.toml` first — it's one line):

```
cargo build --release -p greenlime-myapp \
  --target targets/x86_64-greenlime-sse.json \
  -Zunstable-options -Zjson-target-spec
```

Your app talks to windows through the tiny dev crate
`greenlime-client` (`libs/greenlime-client`) — no syscalls, no ABI
knowledge needed:

```rust
use core::fmt::Write;
use greenlime_client::{assets, exit, log, Win};

let Some(win) = Win::open("My App") else { exit(0) };

// Assets: plain reads, always safe. count() is 0 for boot-baked or
// v1-container tasks — there is no error path, probing never faults.
let n = assets::count();
if n > 0 {
    if let Some(data) = assets::asset(0) {
        // data: &'static [u8] — the kernel never unmaps it.
        win.print(b"first asset right here\n");
    }
}
loop {
    let Some(keys) = win.keys() else { exit(0) }; // window closed
    // keys echo themselves into the window
}
```

The window contract (`greenlime_abi::app`): every ring-3 task owns one
**asset window** at `0x0000_0060_0000_0000` — one descriptor page
(magic `GLA1`, u16 version, u16 count, then `{u64 vaddr, u64 len}`
pairs) followed by the mapped blobs, ascending, page-aligned,
read-only, no-execute. v1 tasks and boot-baked tasks get an *empty*
page (count = 0), so asset-aware code runs unchanged everywhere.
A program may not link into the window: the ELF validator refuses any
segment that touches it, so an app can never shadow its own assets.

**3. Pack it.**

```
python3 scripts/gle-pack.py pack userland/myapp/app.json \
  --elf ~/.cache/greenlime-target/x86_64-greenlime-sse/release/greenlime-myapp \
  --out dist/myapp.gle
```

Now get it onto a disk and run it. From WSL (close the VM first —
never write a disk a running machine owns):

```
mcopy -i dist/disk.img dist/myapp.gle ::/Downloads/
```

Boot, open **Files → Downloads**, **double-click** the app. Or from
the terminal: `exec /disk/Downloads/myapp.gle`

Icon art: any 8-bit, non-interlaced PNG up to 256x256 (RGB, RGBA,
grayscale, or palette; transparent backgrounds welcome). The pack tool
resizes it to exactly 28x28, keeps alpha straight, and clamps alpha so
it can't collide with the system cursor. No idea for an icon?
`python3 scripts/gle-pack.py mkicon icon.png` writes a starter one.

Assets: any number of non-empty files, ≤ 32 entries total (ELF + icon
+ assets), the container ≤ 8 MiB. Asset *content* can be anything —
the kernel never inspects, indexes, or relocates it; it just maps it.

Handy commands:

```
python3 scripts/gle-pack.py inspect dist/myapp.gle   # sticker + entry table
python3 scripts/gle-pack.py extract dist/myapp.gle --dir out/  # unpack
bash scripts/gle-selftest.sh                         # tooling selftest (v1+v2)
```

---

## What the kernel does with a .gle (check order)

`exec` (terminal, admin-gated) and Files double-click both end in the
same kernel path — `vfs::exec_gle`:

1. path must be under `/disk/…` and a file (no directories, no ramfs),
2. read the container; refuse if missing. Read cap: 8 MiB (the size of
   the biggest legal v2 box),
3. peek magic `GLE1` + the u16 header version: **1** → the v1 walk,
   **2** → the v2 walk, anything else → `[gle] unknown header version`,
4. **shared laws:** unknown flag bits refused; sticker sane
   (name 1–24, version ≤ 8, ASCII; author ≤ 24 in v1, ≤ 32 in v2);
   whole-container CRC-32 over `[0..16) ++ [20..EOF)` — the *same
   field, same rule, both formats* — computed in two chunks so an
   8 MiB box never copies itself in the kernel heap,
5. **v1 laws:** icon width 0 or exactly 28; static lengths; nothing
   after the ELF payload,
6. **v2 laws**, in firing order (each refusal names itself):
   header fields sane (meta 9–4096 B, entries 1–32, file ≤ 8 MiB);
   meta starts `GLE2M1`, is ASCII everywhere; exactly one kind-0
   (ELF) entry, zero or one kind-1 (icon, exactly 28x28x4 RGBA);
   kind ≥ 3 → `[gle2] reserved kind`; every entry 4096-aligned,
   off-ascending, non-overlapping; gap + padding bytes zero
   (`[gle2] padding must be zero`); reserved record words zero
   (`[gle2] reserved words must be zero`); per-entry CRC-32 exact
   (icons and assets included); the last blob ends at EOF exactly,
7. the ELF payload passes the same structural validation boot images
   face (static `ET_EXEC` x86-64, no W+X segments, entry inside an
   executable segment, ≤ 32 program headers, user-half addresses —
   plus the new rule: no segment inside the asset window),
8. only then does it take a task slot and map the program into a
   fresh address space — and, for v2, build the asset window
   (descriptor page + all mapped assets) **before** the task's first
   instruction. If a frame runs out, the spawn dies loudly, like any
   other mapping.

Caps are **fixed by the kernel** at spawn — `USER_DEFAULT` (network
broker only, no kill/disk-mutation powers) — never taken from the
sticker, so a file that names itself `shell` gets nothing special.

Refusals are serial lines, never a panic (all four negative shapes
below are exercised by hand-built CRC-resigned containers):

```
[vfs] exec /disk/hello2.gle -> task 3 (hello v1.0, 2 asset(s))
[sched] task 3 asset window: 2 blob(s)
[user] [hello] assets: 2 blob(s) (v2 window)
[user] [hello] asset[0]: 241 B, head: GreenLime OS GLE
[vfs] exec refused /disk/badkind.gle: [gle2] reserved kind
[vfs] exec refused /disk/badres.gle: [gle2] reserved words must be zero
[vfs] exec refused /disk/badpad.gle: [gle2] padding must be zero
[vfs] exec refused /disk/badcrc.gle: [gle] CRC mismatch (corrupt or tampered)
```

New app windows open **unfocused** (they never steal your typing);
click the window to give it keys. Closing the window (or typing `quit`
in the hello template) ends the task and frees the slot for the next
app. Its asset window dies with the page tables — there is no
un-mapping bookkeeping to leak.

---

## Container layout (for tool authors)

Little-endian throughout. Both formats share the first 20 bytes: magic
`GLE1`, u16 version (1 or 2), u16 flags (bit0 = GUI), and the CRC at
[16..20) covering `[0..16) ++ [20..EOF)`.

### v1 — the classic lunchbox

| offset | size | field                                              |
| ------ | ---- | -------------------------------------------------- |
| 0      | 4    | magic `GLE1`                                       |
| 4      | 2    | u16 header version = 1                             |
| 6      | 2    | u16 flags — bit0 = GUI (app opens a window)        |
| 8      | 1    | u8 name_len (1–24)                                 |
| 9      | 1    | u8 ver_len (0–8)                                   |
| 10     | 1    | u8 author_len (0–24)                               |
| 11     | 1    | u8 icon_w (0 = none, else 28)                      |
| 12     | 4    | u32 elf_len (≥ 64, ELF-validated at exec)          |
| 16     | 4    | u32 CRC-32 over `[0..16) ++ [20..EOF)`             |
| 20     | …    | name ‖ version ‖ author ‖ icon RGBA ‖ ELF payload  |

Icon RGBA = straight (un-premultiplied) alpha, `icon_w × icon_w × 4`
bytes, alpha bytes ≤ 238, baked by the pack tool.

### v2 — the asset lunchbox

| offset           | size | field                                           |
| ---------------- | ---- | ----------------------------------------------- |
| 0                | 4    | magic `GLE1`                                    |
| 4                | 2    | u16 header version = **2**                      |
| 6                | 2    | u16 flags — bit0 = GUI                          |
| 8                | 4    | u32 meta_len (9–4096)                           |
| 12               | 4    | u32 entry_count (1–32)                          |
| 16               | 4    | u32 CRC-32 over `[0..16) ++ [20..EOF)`          |
| 20               | …    | **meta block** (see below)                      |
| 20+meta_len      | 32×n | **entry table**, n 32-byte records              |
| (4096-aligned …) | …    | blobs, one per entry, page-aligned ascending    |

Meta block: `GLE2M1`, then u8 name_len / ver_len / author_len, the
three ASCII strings back to back, then a freeform **ASCII** tail (the
pack tool writes `{"assets": [...]}` JSON there; the kernel reads only
the prefix + the three lengths and demands the whole tail be ASCII).

Every entry record (32 B):

| offset | size | field                                    |
| ------ | ---- | ---------------------------------------- |
| 0      | 4    | u32 len — exact blob byte length         |
| 4      | 4    | u32 off — absolute file offset, %4096==0 |
| 8      | 4    | u32 CRC-32 of the exact blob bytes       |
| 12     | 1    | u8 kind — 0 ELF (exactly one), 1 icon (zero or one, 3136 B), 2 asset; ≥ 3 refused |
| 13     | 1    | u8 flags — 0                             |
| 14     | 18   | reserved, all zero                       |

Layout law: entries off-ascending, non-overlapping; every byte between
records (table end → first blob, blob end → next off) is ZERO; the
last blob ends at EOF exactly. Whole container ≤ 8 MiB. The ELF
payload must be a static `ET_EXEC` x86-64 image built for
`targets/x86_64-greenlime-sse.json` — the same target this repo's
userland builds with.

---

## Sizes and caps (current, one line each)

- container: v1 ≤ 1 MiB, v2 ≤ 8 MiB (parse-time refusal)
- entries: ≤ 32 per v2 box (descriptor page could hold 254; the
  container cap binds first)
- disk: FAT32 **read** cap 8 MiB per file (chain walks bounded to
  match); **write** cap stays 1 MiB — nothing in the OS writes bigger
- asset window: descriptor page + blobs, 16 MiB shelf at
  `0x0000_0060_0000_0000` (every real payload fits; the ELF loader
  refuses anything else that wants the shelf)
- slots: three concurrent third-party apps (tasks 9–11); a fourth
  exec is refused with `[sched] no free task slot`

## On the record (not done, by design)

- No dynamic linking: every app carries its own code, whole. A
  `.so`-style loader is a different milestone with a different trust
  story.
- No sidecars/helper processes; no spawning apps from apps.
- No start-card tiles for installed apps (taskbar chips only).
- No GB-sized containers — 8 MiB is the box law until the disk layer
  grows an indexed read API worth defending.
