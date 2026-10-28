//! VFS server (kernel task): static ramfs + FAT32 disk + dynamic /ps.
//! Clients: OPEN path -> handle, READ/WRITE/CLOSE by handle, PING,
//! READDIR, and (M14, v1.1) disk mutations: CREATE / DELETE / MKDIR by
//! path, WRITE (pwrite at arg1 offset, hole-fill + extend) and SETSIZE
//! by handle. Disk mutations are admin-gated (caps::USER_ADMIN) and only
//! touch /disk/*; reads stay open to every task.
//! (M17, v1.2) OP_EXEC runs a third-party .gle app from the disk: the
//! container (sticker + icon + static ELF) is validated and then spawned
//! as a fresh ring-3 task — see exec_gle and the GLE section below.
//! Paths: `/disk/...` = FAT32 disk, `/ps` = task list, else ramfs.
//! Replies go to the request's (kernel-stamped) sender mailbox.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

use greenlime_abi::ipc::{Message, PORT_VFS, REPLY_ERR, REPLY_OK};
use greenlime_abi::vfs::*;

use crate::fs::fat32::Fat32;
use crate::{ipc, kprintln, sched};

struct RamFile {
    path: &'static str,
    data: &'static [u8],
}

static FILES: &[RamFile] = &[
    RamFile {
        path: "/motd.txt",
        data: b"Welcome to GreenLime OS - hybrid IPC online.\n",
    },
    RamFile {
        path: "/hello.txt",
        data: b"hi\n",
    },
];

enum OpenTarget {
    Ram(usize),
    DiskFile(String),
    Ps,
}

struct OpenFile {
    handle: u64,
    target: OpenTarget,
    /// Opener's kernel-stamped task id (GLS-004). Handles are sequential
    /// and guessable, so every op re-checks sender == owner.
    owner: u32,
}

static OPEN: Mutex<Vec<OpenFile>> = Mutex::new(Vec::new());
static NEXT_HANDLE: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(1);
/// FAT32 volume (if a disk was claimed at boot).
static DISK: Mutex<Option<Fat32>> = Mutex::new(None);

// ---------------- M18: the built-in .txt editor (Notepad) ----------------

/// Notepad intake: (task id, path) pairs. THE WINSRV KERNEL TASK
/// registers the file here and spawns notepad next; the task pulls its
/// own path over OP_TXT_TAKE. Keyed by SENDER, so two concurrent
/// editors can never swap files. Entries live until taken.
static PENDING_TXT: Mutex<Vec<(u32, String)>> = Mutex::new(Vec::new());

fn reply(to: u32, kind: u32, arg0: u64, payload: &[u8]) {
    let mut m = Message::new(kind);
    m.arg0 = arg0;
    m.set_payload(payload);
    ipc::send(to, m, sched::current_id().unwrap_or(u32::MAX));
}

fn err(to: u32) {
    reply(to, REPLY_ERR, 0, &[]);
}

/// M14: disk mutations need the admin pair (init + shell). Reads stay
/// open to every task.
fn admin(task: u32) -> bool {
    sched::caps_of(task) & sched::caps::USER_ADMIN == sched::caps::USER_ADMIN
}

/// M18: disk WRITE rights widen to CAP_DISK bearers (the notepad task
/// saves its file). Reads were always open to every task; only the
/// mutation gates widen, and CAP_DISK is granted once at spawn.
fn has_disk(task: u32) -> bool {
    admin(task) || sched::caps_of(task) & sched::caps::CAP_DISK != 0
}

/// Print-safe path for proof lines: ASCII graphics + spaces, cut at 64.
fn safe_path(p: &str) -> alloc::string::String {
    p.chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(64)
        .collect()
}

// ---------------- M16: Files app support (places + trash bin) ----------------

/// The Recycle Bin lives IN the disk so a trashed file survives reboots.
pub const BIN_DISPLAY: &str = "Recycle Bin";

/// The Places sidebar shows these four disk folders first. Order is the
/// contract: Desktop, Documents, Downloads, Recycle Bin.
pub const PLACES: &[&str] = &["Desktop", "Documents", "Downloads", "Recycle Bin"];

/// Best-effort mkdir of every place at boot (idempotent by design: mkdir
/// of an existing dir fails, which is fine). Returns (created, has-disk).
fn ensure_places() -> (usize, bool) {
    let mut disk = DISK.lock();
    let Some(vol) = disk.as_mut() else {
        return (0, false);
    };
    let mut made = 0;
    for name in PLACES {
        let sub = alloc::format!("/{}", name);
        if vol.mkdir(&sub) {
            made += 1;
        }
    }
    (made, true)
}

/// Disk present? (drives the Files sidebar's greyed-out states.)
pub fn disk_ok() -> bool {
    DISK.lock().is_some()
}

/// Root listing entries with kind + size: ramfs files, /ps, /disk.
fn root_detail() -> Vec<(alloc::string::String, bool, u64)> {
    let mut out = Vec::new();
    for f in FILES {
        let name = f.path.strip_prefix('/').unwrap_or(f.path);
        out.push((alloc::string::String::from(name), false, f.data.len() as u64));
    }
    out.push((alloc::string::String::from("ps"), false, 0));
    if DISK.lock().is_some() {
        out.push((alloc::string::String::from("disk"), true, 0));
    }
    out
}

/// Kind + size listing for the Files window: (display name, is_dir,
/// size in bytes; dirs report 0). None = not a directory / unmounted.
pub fn list_details(path: &str) -> Option<Vec<(alloc::string::String, bool, u64)>> {
    if path == "/" || path.is_empty() {
        return Some(root_detail());
    }
    let sub = path.strip_prefix("/disk")?;
    let sub = if sub.is_empty() { "/" } else { sub };
    let disk = DISK.lock();
    let vol = disk.as_ref()?;
    vol.list_detail(sub)
}

// ---------------- M17: .gle apps (the third-party app truck) ----------------
//
// A .gle file is a flat lunchbox: a 20-byte header (magic, version, flags,
// sticker lengths, icon size, ELF length, CRC32), the ASCII sticker
// strings, an optional 28x28 straight-alpha RGBA icon (pre-clamped <=238
// by the pack tool — the kernel never decodes PNGs or normalizes pixels),
// then the static ET_EXEC ELF payload. `exec_gle` unwraps it, validates
// everything, picks a task slot and jumps in; the app then registers with
// winsrv like any client (WIN_CREATE) and shows up on the taskbar.
//
// Layout (little-endian; the pack tool writes exactly this):
//   [0..4)    magic "GLE1"
//   [4..6)    u16 header version (1)
//   [6..8)    u16 flags — bit0 GUI (app opens a window)
//   [8]       u8 name_len    (1..=GLE_NAME_MAX bytes, ASCII)
//   [9]       u8 ver_len     (0..=GLE_VER_MAX)
//   [10]      u8 author_len  (0..=GLE_AUTHOR_MAX)
//   [11]      u8 icon_w      (0 = none, else exactly GLE_ICON_W)
//   [12..16)  u32 elf_len    (>= 64, ELF-validated before spawn)
//   [16..20)  u32 crc32      (over [0..16) ++ [20..EOF): everything BUT itself)
//   [20..)    name || version || author || icon RGBA || ELF payload
//
// Any failed check short-circuits to a reason string — a dropped-in bad
// file is refused with a serial line, never a kernel panic.

const GLE_HDR: usize = 20;
const GLE_MAGIC: [u8; 4] = *b"GLE1";
const GLE_VER: u16 = 1;
const GLE_FLAG_GUI: u16 = 1;
pub const GLE_NAME_MAX: usize = 24;
pub const GLE_VER_MAX: usize = 8;
pub const GLE_AUTHOR_MAX: usize = 24;
pub const GLE_ICON_W: usize = 28;
/// Container cap: an easy refusal beats an out-of-frames surprise.
pub const GLE_MAX: usize = 1024 * 1024;

/// A parsed, validated .gle sticker (borrows the container bytes).
/// author/gui/icon are validated but not printed — #[allow] keeps the
/// zero-warning build while the struct stays the one place the law lists.
#[allow(dead_code)]
pub struct GleApp<'a> {
    pub name: &'a str,
    pub version: &'a str,
    pub author: &'a str,
    pub gui: bool,
    pub icon: Option<&'a [u8]>, // straight-alpha RGBA, icon_w x icon_w x 4
    pub elf: &'a [u8],
}

fn gle_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn gle_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// CRC-32 (IEEE, the zlib poly 0xEDB8_8320) — matches what zlib and the
/// Python pack tool emit. A 1 MiB container costs ~8 M ops once: fine.
/// v2 (M19) re-factored to a two-call chunked form ([0..16) then
/// [20..EOF']): an 8 MiB container must never mirror itself in the
/// 16 MiB kernel heap.
fn crc32_raw(st: u32, data: &[u8]) -> u32 {
    let mut crc = st;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    crc
}

fn crc32(data: &[u8]) -> u32 {
    !crc32_raw(0xFFFF_FFFF, data)
}

/// Parse + validate a .gle container — any format. Peek version from the
/// header and dispatch: v1 keeps its law, v2 (M19) lands below. Borrows
/// and serves refusal strings; allocation is only the v2 borrow list.
/// The union view: the sticker facts either way, assets only for v2.
pub enum GleParsed<'a> {
    V1(GleApp<'a>),
    V2(Gle2App<'a>),
}

pub fn parse_gle(b: &[u8]) -> Result<GleParsed<'_>, &'static str> {
    if b.len() < 6 {
        return Err("[gle] truncated header");
    }
    if b[0..4] != GLE_MAGIC {
        return Err("[gle] bad magic");
    }
    match gle_u16(b, 4) {
        1 => parse_gle_v1(b).map(GleParsed::V1),
        2 => parse_gle2(b).map(GleParsed::V2),
        _ => Err("[gle] unknown header version"),
    }
}

/// v1 lunchbox: header + sticker + icon + ELF payload, exactly its own
/// length law. (Version/magic checks stay as belt laws next to the
/// dispatcher.)
fn parse_gle_v1(b: &[u8]) -> Result<GleApp<'_>, &'static str> {
    if b.len() < GLE_HDR {
        return Err("[gle] truncated header");
    }
    if b[0..4] != GLE_MAGIC {
        return Err("[gle] bad magic");
    }
    if gle_u16(b, 4) != GLE_VER {
        return Err("[gle] unknown header version");
    }
    let flags = gle_u16(b, 6);
    if flags & !GLE_FLAG_GUI != 0 {
        return Err("[gle] unknown flag bits");
    }
    let nl = b[8] as usize;
    let vl = b[9] as usize;
    let al = b[10] as usize;
    if nl == 0 || nl > GLE_NAME_MAX {
        return Err("[gle] bad name length");
    }
    if vl > GLE_VER_MAX {
        return Err("[gle] bad version length");
    }
    if al > GLE_AUTHOR_MAX {
        return Err("[gle] bad author length");
    }
    let iw = b[11] as usize;
    if iw != 0 && iw != GLE_ICON_W {
        return Err("[gle] unsupported icon size");
    }
    let elf_len = gle_u32(b, 12) as usize;
    if elf_len == 0 {
        return Err("[gle] empty payload");
    }
    // Exact-size container: header + strings + icon + ELF, no trail bytes.
    let strings = nl + vl + al;
    let icon = iw * iw * 4;
    let total = GLE_HDR + strings + icon + elf_len;
    if total != b.len() {
        return Err("[gle] length mismatch");
    }
    if total > GLE_MAX {
        return Err("[gle] container beyond GLE_MAX");
    }
    let want = gle_u32(b, 16);
    // Chunked container CRC (same law as v1: over [0..16) ++ [20..EOF))
    // — two direct calls, no second container-sized copy.
    let mut st = crc32_raw(0xFFFF_FFFF, &b[..16]);
    st = crc32_raw(st, &b[20..]);
    let got = !st;
    if got != want {
        return Err("[gle] CRC mismatch (corrupt or tampered)");
    }
    let sticker = &b[GLE_HDR..GLE_HDR + strings];
    if !sticker.is_ascii() {
        return Err("[gle] sticker must be ASCII (OS namespace is ASCII)");
    }
    let (name_b, rest) = sticker.split_at(nl);
    let (ver_b, author_b) = rest.split_at(vl);
    const NOT_UTF8: &str = "[gle] sticker is not UTF-8";
    fn as_str<'a>(x: &'a [u8], _why: &'static str) -> Result<&'a str, &'static str> {
        core::str::from_utf8(x).map_err(|_| _why)
    }
    Ok(GleApp {
        name: as_str(name_b, NOT_UTF8)?,
        version: as_str(ver_b, NOT_UTF8)?,
        author: as_str(author_b, NOT_UTF8)?,
        gui: flags & GLE_FLAG_GUI != 0,
        icon: if iw == 0 {
            None
        } else {
            Some(&b[GLE_HDR + strings..GLE_HDR + strings + icon])
        },
        elf: &b[total - elf_len..],
    })
}

// ---------------- M19: .gle v2 — the container grows up ----------------
//
// The lunchbox law holds (one flat file, whole-file CRC, no installer,
// no trusted sticker), so v1 files keep running untouched. What changes:
//
//   [0..4)    magic "GLE1"                 (same — the version field tells)
//   [4..6)    u16 version      = 2
//   [6..8)    u16 flags        (bit0 GUI; any other set bit refused)
//   [8..12)   u32 meta_len     (9..=GLE2_META_MAX)
//   [12..16)  u32 entry_count  (1..=GLE2_ENTRY_MAX)
//   [16..20)  u32 crc32        (over [0..16) ++ [20..EOF): the v1 slot law)
//   [20..20+meta_len)   meta block: magic "GLE2M1", then u8 nl / vl / al,
//                       ASCII name || version || author, then a freeform
//                       ASCII tail (the pack tool writes JSON there; the
//                       kernel reads ONLY the prefix + the three lengths
//                       and still demands the tail is ASCII).
//   [table: entry_count x 32 bytes, right after the meta block:
//     [0..4)   u32 len    exact blob byte length
//     [4..8)   u32 off    absolute file offset (EVERY entry 4096-aligned)
//     [8..12)  u32 crc32  over the exact blob bytes
//     [12]     u8  kind   0 = main ELF (exactly one), 1 = icon 28x28
//                         straight-alpha RGBA (zero or one), 2 = asset;
//                         3+ = reserved: refused
//     [13]     u8  flags  must be 0
//     [14..16) u16, [16..24) u64, [24..32) u64 — reserved, must be 0
//   blobs follow the table, page-aligned ascending; every gap byte
//   (table end -> first blob, entry end -> next off) is ZERO; the last
//   blob ends at the file length exactly.
//
// Assets (kind 2) are NOT copied anywhere: exec maps them read-only into
// the task's asset window (greenlime_abi::app contract, built in
// elf::map_assets) and the client reads them as plain memory.
//
// The entry-count cap (32) also caps assets below the descriptor-page
// capacity (254), and the container cap (8 MiB) keeps every aligned blob
// inside the 16 MiB window: 8 MiB + 32 gaps of < 4 KiB can't reach it.
// Those two invariants live here; map_assets only dies loudly on true OOM.

pub const GLE2_ENTRY_MAX: usize = 32;
pub const GLE2_META_MAX: usize = 4096;
/// v2 sticker: the taskbar chip law holds (name <= 24, version <= 8);
/// the author field gets one extra byte band: 32.
pub const GLE2_AUTHOR_MAX: usize = 32;
/// v2 container cap (was 1 MiB in v1 — assets want room).
pub const GLE2_MAX: usize = 8 * 1024 * 1024;
const GLE2_META_MAGIC: [u8; 6] = *b"GLE2M1";
const GLE2_KIND_ELF: u8 = 0;
const GLE2_KIND_ICON: u8 = 1;
const GLE2_KIND_ASSET: u8 = 2;

/// A parsed, validated v2 container (borrows the file bytes). `assets`
/// is file order (off ascending) and is exactly what spawn maps.
#[allow(dead_code)]
pub struct Gle2App<'a> {
    pub name: &'a str,
    pub version: &'a str,
    pub author: &'a str,
    pub gui: bool,
    pub icon: Option<&'a [u8]>,
    pub elf: &'a [u8],
    pub assets: Vec<&'a [u8]>,
}

/// Parse + validate a v2 container. Laws in firing order (every refusal
/// short-circuits to a reason string before kernel state moves):
/// header fields, whole-file CRC, meta (prefix, lengths, ASCII, sizes),
/// then the entry walk (alignment, ordering, zero padding, kinds,
/// per-entry CRC), then the shape laws (exactly one ELF, exact total).
pub fn parse_gle2(b: &[u8]) -> Result<Gle2App<'_>, &'static str> {
    if b.len() < GLE_HDR {
        return Err("[gle] truncated header");
    }
    let flags = gle_u16(b, 6);
    if flags & !GLE_FLAG_GUI != 0 {
        return Err("[gle2] unknown flag bits");
    }
    let meta_len = gle_u32(b, 8) as usize;
    if meta_len < 9 || meta_len > GLE2_META_MAX {
        return Err("[gle2] bad meta length");
    }
    let count = gle_u32(b, 12) as usize;
    if count == 0 || count > GLE2_ENTRY_MAX {
        return Err("[gle2] bad entry count");
    }
    if b.len() > GLE2_MAX {
        return Err("[gle2] container beyond GLE2_MAX");
    }
    // Whole-container CRC — v1's slot law ([16..20) over [0..16) ++
    // [20..EOF)) — in two direct chunks: an 8 MiB container must not
    // mirror itself in the heap.
    let want = gle_u32(b, 16);
    let mut st = crc32_raw(0xFFFF_FFFF, &b[..16]);
    st = crc32_raw(st, &b[20..]);
    let got = !st;
    if got != want {
        return Err("[gle] CRC mismatch (corrupt or tampered)");
    }
    let table_off = GLE_HDR + meta_len;
    let table_end = table_off
        .checked_add(count * 32)
        .filter(|&e| e <= b.len())
        .ok_or("[gle2] truncated entry table")?;

    // Meta block: fixed prefix + birth-sticker, ASCII everywhere.
    let m = &b[GLE_HDR..table_off];
    if m[0..6] != GLE2_META_MAGIC {
        return Err("[gle2] unknown meta format");
    }
    let nl = m[6] as usize;
    let vl = m[7] as usize;
    let al = m[8] as usize;
    if nl == 0 || nl > GLE_NAME_MAX {
        return Err("[gle] bad name length");
    }
    if vl > GLE_VER_MAX {
        return Err("[gle] bad version length");
    }
    if al > GLE2_AUTHOR_MAX {
        return Err("[gle] bad author length");
    }
    let strings = nl + vl + al;
    if 9 + strings > meta_len {
        return Err("[gle2] truncated sticker");
    }
    let sticker = &m[9..9 + strings];
    if !sticker.is_ascii() {
        return Err("[gle] sticker must be ASCII (OS namespace is ASCII)");
    }
    if !m[9 + strings..].is_ascii() {
        return Err("[gle2] meta tail must be ASCII");
    }
    const NOT_UTF8: &str = "[gle] sticker is not UTF-8";
    fn as_str<'a>(x: &'a [u8], why: &'static str) -> Result<&'a str, &'static str> {
        core::str::from_utf8(x).map_err(|_| why)
    }
    let (name_b, rest) = sticker.split_at(nl);
    let (ver_b, author_b) = rest.split_at(vl);
    let name = as_str(name_b, NOT_UTF8)?;
    let version = as_str(ver_b, NOT_UTF8)?;
    let author = as_str(author_b, NOT_UTF8)?;

    // Entry walk. prev_end starts at the table end, so the first check
    // also covers "entries may not overlap the table".
    let mut prev_end = table_end;
    let mut elf: Option<(usize, usize)> = None; // (off, len)
    let mut icon: Option<&[u8]> = None;
    let mut assets: Vec<&[u8]> = Vec::new();
    for i in 0..count {
        let e = table_off + i * 32;
        let eb = &b[e..e + 32];
        let len = gle_u32(eb, 0) as usize;
        let off = gle_u32(eb, 4) as usize;
        let ecrc = gle_u32(eb, 8);
        let kind = eb[12];
        if eb[13] != 0 {
            return Err("[gle2] unknown entry flags");
        }
        // u16 pad + the two reserved u64s stay zero, or no deal.
        if eb[14] != 0 || eb[15] != 0 || b[e + 16..e + 32].iter().any(|&x| x != 0) {
            return Err("[gle2] reserved words must be zero");
        }
        if kind > GLE2_KIND_ASSET {
            return Err("[gle2] reserved kind");
        }
        let blob_end = off
            .checked_add(len)
            .filter(|&x| x <= b.len())
            .ok_or("[gle2] entry beyond the file end")?;
        if (off & (4096 - 1)) != 0 {
            return Err("[gle2] entries must be page-aligned");
        }
        if off < prev_end {
            return Err("[gle2] entries must be off-ascending, non-overlapping");
        }
        if b[prev_end..off].iter().any(|&x| x != 0) {
            return Err("[gle2] padding must be zero");
        }
        if crc32(&b[off..blob_end]) != ecrc {
            return Err("[gle2] entry CRC mismatch");
        }
        match kind {
            GLE2_KIND_ELF => {
                if len == 0 {
                    return Err("[gle2] empty payload");
                }
                if elf.is_some() {
                    return Err("[gle2] second main ELF");
                }
                elf = Some((off, len));
            }
            GLE2_KIND_ICON => {
                if len != GLE_ICON_W * GLE_ICON_W * 4 {
                    return Err("[gle2] icon must be 28x28 RGBA");
                }
                if icon.is_some() {
                    return Err("[gle2] two icons");
                }
                icon = Some(&b[off..blob_end]);
            }
            _ => {
                // kind == 2: asset. Zero-length would map zero pages and
                // hand the client a nonsense descriptor: refuse.
                if len == 0 {
                    return Err("[gle2] empty asset");
                }
                assets.push(&b[off..blob_end]);
            }
        }
        prev_end = blob_end;
    }
    let (elf_off, elf_len) = elf.ok_or("[gle2] no main ELF")?;
    if prev_end != b.len() {
        return Err("[gle2] length mismatch");
    }
    Ok(Gle2App {
        name,
        version,
        author,
        gui: flags & GLE_FLAG_GUI != 0,
        icon,
        elf: &b[elf_off..elf_off + elf_len],
        assets,
    })
}

/// Kernel-internal .gle exec: load the container from the disk, validate
/// it, and spawn the task. `winsrv` (Files double-click) calls this
/// directly — kernel tasks share the kernel — while the `OP_EXEC` IPC arm
/// funnels userland `exec` here after its admin gate.
pub fn exec_gle(path: &str) -> Result<u32, &'static str> {
    if !path.starts_with("/disk/") {
        return Err("[gle] only /disk/* paths");
    }
    let sub = path.strip_prefix("/disk").unwrap_or(path);
    if sub.ends_with('/') {
        return Err("[gle] not a file");
    }
    let bytes = {
        let disk = DISK.lock();
        let vol = disk.as_ref().ok_or("[gle] no disk mounted")?;
        vol.read_file(sub).ok_or("[gle] file not found")?
    };
    // The task table wants 'static names: leak the one bounded bit worth
    // carrying (ASCII, <=24 bytes in either format, one string per live
    // task). Caps are fixed HERE — USER_DEFAULT (net; no kill / wall /
    // disk mutations) — and never derived from the sticker, so a payload
    // cannot borrow boot-task privileges by calling itself "shell".
    match parse_gle(&bytes)? {
        GleParsed::V1(a) => {
            let leaked: &'static str =
                alloc::boxed::Box::leak(alloc::format!("{}", a.name).into_boxed_str());
            let id = sched::spawn_user_task_with(leaked, a.elf, sched::caps::USER_DEFAULT)?;
            kprintln!(
                "[vfs] exec {} -> task {} ({} v{})",
                safe_path(path),
                id,
                a.name,
                a.version
            );
            Ok(id)
        }
        GleParsed::V2(a) => {
            let leaked: &'static str =
                alloc::boxed::Box::leak(alloc::format!("{}", a.name).into_boxed_str());
            let id =
                sched::spawn_user_task_ex(leaked, a.elf, sched::caps::USER_DEFAULT, &a.assets)?;
            kprintln!(
                "[vfs] exec {} -> task {} ({} v{}, {} asset(s))",
                safe_path(path),
                id,
                a.name,
                a.version,
                a.assets.len()
            );
            Ok(id)
        }
    }
}

// ---------------- M18: the .txt editor binding (Notepad) ----------------

/// File-size cap for notepad intake (the editor reads + saves a 4 KiB
/// window; the 64 KiB line keeps the intake shell-law sized).
pub const TXT_MAX: usize = 64 * 1024;

/// Kernel-internal notepad spawn — the .txt twin of exec_gle: validate
/// a disk FILE, spawn the notepad task with CALLER-FIXED caps (CAP_DISK;
/// it cannot net or kill), then register (task, path). Order matters:
/// falsify-first so a failed spawn never leaves a phantom ticket.
/// winsrv calls this for right-click "New text file" + .txt
/// double-click; nothing spawns notepad at boot.
pub fn open_txt(path: &str) -> Result<u32, &'static str> {
    if !path.starts_with("/disk/") {
        return Err("[txt] only /disk/* paths");
    }
    let sub = path.strip_prefix("/disk").unwrap_or(path);
    if sub.ends_with('/') || sub.is_empty() {
        return Err("[txt] not a file");
    }
    let (is_dir, size) = {
        let disk = DISK.lock();
        let vol = disk.as_ref().ok_or("[txt] no disk mounted")?;
        vol.entry_info(sub).ok_or("[txt] file not found")?
    };
    if is_dir {
        return Err("[txt] not a file");
    }
    if size > TXT_MAX as u64 {
        return Err("[txt] file beyond the 64 KiB notepad law");
    }
    let id = sched::spawn_user_task_with(
        "notepad",
        crate::userspace::NOTEPAD_ELF,
        sched::caps::CAP_DISK,
    )?;
    {
        let mut pend = PENDING_TXT.lock();
        // Reincarnation law (live burst evidence): task ids recycle from
        // the LIFO freelist, so an entry already bearing THIS id is a
        // dead earlier incarnation's spare ticket — erase it here and
        // let exactly one ticket exist per spawn, held by the task that
        // may legally pop it.
        pend.retain(|(t, _)| *t != id);
        pend.push((id, alloc::string::String::from(path)));
    }
    kprintln!("[vfs] open txt {} -> task {}", safe_path(path), id);
    Ok(id)
}

/// Fresh-name probe + create inside ONE volume lock: `New file N.txt`,
/// N = 1..99, first free wins. Returns the full /disk path. Bags refuse:
/// a working file inside the Recycle Bin contradicts trash/restore.
pub fn new_txt_in(dir: &str) -> Option<String> {
    if !dir.starts_with("/disk") {
        kprintln!("[vfs] new file refused {} (disk only)", safe_path(dir));
        return None;
    }
    if dir.trim_end_matches('/').ends_with(BIN_DISPLAY) {
        kprintln!("[vfs] new file refused (Recycle Bin is for trash)");
        return None;
    }
    let mut disk = DISK.lock();
    let Some(vol) = disk.as_mut() else {
        kprintln!("[vfs] new file refused (no disk)");
        return None;
    };
    for n in 1..=99 {
        let name = alloc::format!("New file {}.txt", n);
        let full = alloc::format!("{}/{}", dir.trim_end_matches('/'), name);
        let sub = full.strip_prefix("/disk").unwrap_or(full.as_str());
        if vol.entry_info(sub).is_some() {
            continue;
        }
        if vol.create(sub) {
            kprintln!("[vfs] created {}", safe_path(&full));
            return Some(full);
        }
        kprintln!("[vfs] create refused (FAT busy?)");
        return None;
    }
    kprintln!("[vfs] new file refused (99 names taken)");
    None
}

// Vol helpers below take an ALREADY-LOCKED volume — the public entry
// points above/below lock DISK exactly once (spin::Mutex is not
// re-entrant, so no helper may lock again).

/// Write `data` to a NEW file (create + pwrite-0). True on success.
fn vol_put(vol: &mut Fat32, sub: &str, data: &[u8]) -> bool {
    if !vol.create(sub) {
        return false;
    }
    data.is_empty() || vol.write_at(sub, 0, data).is_some()
}

/// Delete a volume path (best-effort; dirs must be empty).
fn vol_del(vol: &mut Fat32, sub: &str) -> bool {
    vol.delete(sub)
}

/// Free a display name inside the bin: `name`, then `name (2)`… `(8)`.
/// Uses the volume-backed existence probe so a trashed-out-of-band name
/// is still honored.
fn free_bin_name(vol: &mut Fat32, name: &str) -> Option<alloc::string::String> {
    let bin_prefix = alloc::format!("/{}/", BIN_DISPLAY);
    for suffix in ["", " (2)", " (3)", " (4)", " (5)", " (6)", " (7)", " (8)"] {
        let cand = alloc::format!("{}{}", name, suffix);
        let sub = alloc::format!("{}{}", bin_prefix, cand);
        if vol.entry_info(&sub).is_none() {
            return Some(cand);
        }
    }
    None
}

/// Move a disk FILE into the Recycle Bin: copy bytes, remember the
/// source dir in a `<name>.orig` sidecar, delete the original. Returns
/// Some(display path) on success. Dirs and bin-internals refused;
/// collisions resolved by the `name (n)` scheme. All-or-nothing: any
/// step failing rolls the whole move back and returns None.
pub fn trash(path: &str) -> Option<alloc::string::String> {
    let sub = path.strip_prefix("/disk")?;
    if sub.is_empty() || !sub.starts_with('/') || sub.ends_with('/') {
        return None;
    }
    let bin_prefix = alloc::format!("/{}/", BIN_DISPLAY);
    if sub.starts_with(&bin_prefix) || sub.trim_start_matches('/') == BIN_DISPLAY {
        return None; // no trashing the bin itself
    }
    let mut disk = DISK.lock();
    let Some(vol) = disk.as_mut() else {
        return None;
    };
    let Some((false, _)) = vol.entry_info(sub) else {
        return None; // missing, or a dir (dirs get their own M16.x pass)
    };
    let Some(data) = vol.read_file(sub) else {
        return None;
    };
    let Some(name) = sub.rsplit('/').next() else {
        return None;
    };
    if name.is_empty() {
        return None;
    }
    let parent_sub = sub[..sub.len() - name.len()]
        .trim_end_matches('/')
        .to_string();
    let parent_full = if parent_sub.is_empty() {
        alloc::string::String::from("/disk")
    } else {
        alloc::format!("/disk{}", parent_sub)
    };
    let Some(display) = free_bin_name(vol, name) else {
        return None;
    };
    let bin_sub = alloc::format!("/{}/", BIN_DISPLAY);
    let dest_sub = alloc::format!("{}{}", bin_sub, display);
    let orig_sub = alloc::format!("{}{}.orig", bin_sub, display);
    if !vol_put(vol, &dest_sub, &data) {
        return None;
    }
    if !vol_put(vol, &orig_sub, parent_full.as_bytes()) {
        let _ = vol_del(vol, &dest_sub);
        return None;
    }
    if !vol_del(vol, sub) {
        let _ = vol_del(vol, &dest_sub);
        let _ = vol_del(vol, &orig_sub);
        return None;
    }
    kprintln!(
        "[vfs] trash /disk{} -> /disk/{}",
        safe_path(sub),
        safe_path(&display)
    );
    Some(alloc::format!("/disk/{}", display))
}

/// Restore a bin item: `<name>.orig` remembers the source dir; when the
/// original dir is gone the item lands on the /disk root. Returns
/// Some(new path); refuses (None) on a live collision at the
/// destination instead of clobbering anything.
pub fn untrash(display: &str) -> Option<alloc::string::String> {
    if display.is_empty() || display.contains('/') || display.ends_with('/') {
        return None;
    }
    let bin_sub = alloc::format!("/{}/", BIN_DISPLAY);
    let src_sub = alloc::format!("{}{}", bin_sub, display);
    let orig_sub = alloc::format!("{}{}.orig", bin_sub, display);
    let mut disk = DISK.lock();
    let Some(vol) = disk.as_mut() else {
        return None;
    };
    let Some(data) = vol.read_file(&src_sub) else {
        return None;
    };
    let Some(orig) = vol.read_file(&orig_sub) else {
        return None;
    };
    let parent = core::str::from_utf8(&orig).ok()?.trim().to_string();
    let parent_sub = if parent.len() >= 6 && parent.starts_with("/disk") {
        parent.strip_prefix("/disk").unwrap_or("/").to_string()
    } else {
        alloc::string::String::new() // fallback: the disk root
    };
    let dest_sub = alloc::format!(
        "{}/{}",
        parent_sub.trim_end_matches('/'),
        display
    );
    if vol.entry_info(&dest_sub).is_some() {
        return None; // occupied outside our knowledge: refuse
    }
    if !vol_put(vol, &dest_sub, &data) {
        return None;
    }
    let _ = vol_del(vol, &orig_sub);
    let _ = vol_del(vol, &src_sub);
    let full = alloc::format!("/disk{}", dest_sub);
    kprintln!(
        "[vfs] restore {} -> {}",
        safe_path(display),
        safe_path(&full)
    );
    Some(full)
}

/// Empty the Recycle Bin: every live item + its sidecar deleted
/// (best-effort). Returns the number of items removed.
pub fn empty_bin() -> usize {
    let mut disk = DISK.lock();
    let Some(vol) = disk.as_mut() else {
        return 0;
    };
    let Some(items) = vol.list_dir(alloc::format!("/{}", BIN_DISPLAY).as_str()) else {
        return 0;
    };
    let bin_sub = alloc::format!("/{}/", BIN_DISPLAY);
    let mut n = 0;
    for it in items {
        let is_dir = it.ends_with('/');
        let name = it.trim_end_matches('/');
        if is_dir || name.is_empty() || name.ends_with(".orig") {
            continue;
        }
        let item_sub = alloc::format!("{}{}", bin_sub, name);
        let orig_sub = alloc::format!("{}{}.orig", bin_sub, name);
        let _ = vol_del(vol, &item_sub);
        let _ = vol_del(vol, &orig_sub);
        n += 1;
    }
    kprintln!("[vfs] bin emptied ({} items)", n);
    n
}

/// Root listing: ramfs files + disk/ + ps.
fn root_listing() -> Vec<u8> {
    let mut out = String::new();
    for f in FILES {
        let name = f.path.strip_prefix('/').unwrap_or(f.path);
        out.push_str(name);
        out.push('\n');
    }
    out.push_str("ps\n");
    // Only advertise the disk if it actually mounted.
    if DISK.lock().is_some() {
        out.push_str("disk/\n");
    }
    out.into_bytes()
}

fn full_listing(path: &str) -> Option<Vec<u8>> {
    if path == "/" || path.is_empty() {
        Some(root_listing())
    } else if path == "/ps" {
        let mut out = String::from("PID NAME STATE\n");
        for (id, name, state) in sched::snapshot() {
            out.push_str(&alloc::format!("{} {} {:?}\n", id, name, state));
        }
        Some(out.into_bytes())
    } else if let Some(sub) = path.strip_prefix("/disk") {
        let sub = if sub.is_empty() { "/" } else { sub };
        let disk = DISK.lock();
        let vol = disk.as_ref()?;
        let names = vol.list_dir(sub)?;
        let mut out = String::new();
        for n in names {
            out.push_str(&n);
            out.push('\n');
        }
        Some(out.into_bytes())
    } else {
        None
    }
}

fn list_target(path: &str, offset: usize) -> Option<Vec<u8>> {
    let full = full_listing(path)?;
    let off = offset.min(full.len());
    let end = (off + 200).min(full.len());
    Some(full[off..end].to_vec())
}

/// Names of a directory (for the Files window): one line per entry,
/// directories ending with '/'. None = not a directory / unmounted.
/// (v0.x names-only listing; the M16 Files window moved to list_details,
/// but the API stays for compatibility.)
#[allow(dead_code)]
pub fn list_names(path: &str) -> Option<Vec<String>> {
    let full = full_listing(path)?;
    let text = core::str::from_utf8(&full).ok()?;
    let mut names = Vec::new();
    for l in text.split('\n') {
        if !l.is_empty() {
            names.push(String::from(l));
        }
    }
    Some(names)
}

fn handle(req: Message) {
    let from = req.sender;
    match req.kind {
        OP_OPEN => {
            let path = core::str::from_utf8(req.payload_bytes()).unwrap_or("");
            let target = if path == "/ps" {
                Some(OpenTarget::Ps)
            } else if let Some(sub) = path.strip_prefix("/disk") {
                let sub = if sub.is_empty() { "/" } else { sub };
                // Validate readability now (fail fast on missing files).
                let ok = {
                    let disk = DISK.lock();
                    match disk.as_ref() {
                        Some(vol) => vol.read_file(sub).is_some(),
                        None => false,
                    }
                };
                if ok {
                    Some(OpenTarget::DiskFile(String::from(sub)))
                } else {
                    None
                }
            } else if FILES.iter().any(|f| f.path == path) {
                Some(OpenTarget::Ram(
                    FILES.iter().position(|f| f.path == path).unwrap(),
                ))
            } else {
                None
            };
            match target {
                Some(t) => {
                    let h = NEXT_HANDLE.fetch_add(
                        1,
                        core::sync::atomic::Ordering::SeqCst,
                    );
                    OPEN.lock().push(OpenFile {
                        handle: h,
                        target: t,
                        owner: from,
                    });
                    reply(from, REPLY_OK, h, &[]);
                }
                None => err(from),
            }
        }
        OP_READ => {
            let data: Option<Vec<u8>> = {
                let open = OPEN.lock();
                // GLS-004: handles are guessable (1, 2, 3, …) — only the
                // opener may read through one.
                let entry = match open
                    .iter()
                    .find(|o| o.handle == req.arg0 && o.owner == from)
                {
                    Some(e) => e,
                    None => {
                        drop(open);
                        err(from);
                        return;
                    }
                };
                match &entry.target {
                    OpenTarget::Ram(i) => {
                        let d = FILES[*i].data;
                        let off = (req.arg1 as usize).min(d.len());
                        let end = (off + 200).min(d.len());
                        Some(d[off..end].to_vec())
                    }
                    OpenTarget::DiskFile(path) => {
                        let disk = DISK.lock();
                        let vol = match disk.as_ref() {
                            Some(v) => v,
                            None => {
                                drop(open);
                                err(from);
                                return;
                            }
                        };
                        match vol.read_file(path) {
                            Some(full) => {
                                let off = (req.arg1 as usize).min(full.len());
                                let end = (off + 200).min(full.len());
                                Some(full[off..end].to_vec())
                            }
                            None => None,
                        }
                    }
                    OpenTarget::Ps => {
                        drop(open);
                        // Regenerate (live task states), then slice.
                        let mut out = String::from("PID NAME STATE\n");
                        for (id, name, state) in sched::snapshot() {
                            out.push_str(&alloc::format!("{} {} {:?}\n", id, name, state));
                        }
                        let full = out.into_bytes();
                        let off = (req.arg1 as usize).min(full.len());
                        let end = (off + 200).min(full.len());
                        Some(full[off..end].to_vec())
                    }
                }
            };
            // NOTE: OPEN guard dropped above in every arm before replying.
            match data {
                Some(d) => reply(from, REPLY_OK, d.len() as u64, &d),
                None => err(from),
            }
        }
        OP_READDIR => {
            let path = core::str::from_utf8(req.payload_bytes()).unwrap_or("");
            match list_target(path, req.arg1 as usize) {
                Some(d) => reply(from, REPLY_OK, d.len() as u64, &d),
                None => err(from),
            }
        }
        OP_CLOSE => {
            let mut open = OPEN.lock();
            let n = open.len();
            // GLS-004: only the opener may close its handle.
            open.retain(|o| !(o.handle == req.arg0 && o.owner == from));
            if open.len() < n {
                reply(from, REPLY_OK, 0, &[]);
            } else {
                err(from);
            }
        }
        OP_PING => reply(from, REPLY_OK, PING_MAGIC, &[]),
        // M17: run a third-party .gle app. Admin-gated like disk mutations
        // (init + shell exec from the shell; winsrv double-click uses the
        // direct internal path instead). Reply arg0 = new task id.
        OP_EXEC => {
            let path =
                core::str::from_utf8(req.payload_bytes()).unwrap_or("").trim();
            if !admin(from) {
                kprintln!(
                    "[vfs] exec refused {} (admin only)",
                    safe_path(path)
                );
                err(from);
            } else {
                match exec_gle(path) {
                    Ok(id) => reply(from, REPLY_OK, id as u64, &[]),
                    Err(reason) => {
                        kprintln!(
                            "[vfs] exec refused {}: {}",
                            safe_path(path),
                            reason
                        );
                        err(from);
                    }
                }
            }
        }
        OP_WRITE => {
            // M14: pwrite the payload at arg1 offset. Extends the file
            // (new clusters appended) and zero-fills holes. Disk files
            // only — ramfs is &'static data, /ps is generated. Reply
            // arg0 = bytes written.
            let payload = req.payload_bytes();
            let n = payload.len() as u64;
            let res: Option<(u64, String)> = {
                let open = OPEN.lock();
                // GLS-004: handles are guessable — only the opener may
                // write through one.
                let entry = match open
                    .iter()
                    .find(|o| o.handle == req.arg0 && o.owner == from)
                {
                    Some(e) => e,
                    None => {
                        drop(open);
                        err(from);
                        return;
                    }
                };
                if !has_disk(from) {
                    drop(open);
                    err(from);
                    return;
                }
                match &entry.target {
                    OpenTarget::DiskFile(path) => {
                        let mut disk = DISK.lock();
                        let vol = match disk.as_mut() {
                            Some(v) => v,
                            None => {
                                drop(open);
                                err(from);
                                return;
                            }
                        };
                        vol.write_at(path, req.arg1, payload)
                            .map(|_sz| (n, alloc::format!("/disk{}", path)))
                    }
                    _ => None,
                }
            };
            match res {
                Some((n, path)) => {
                    kprintln!("[vfs] wrote {} bytes -> {}", n, safe_path(&path));
                    reply(from, REPLY_OK, n, &[]);
                }
                None => err(from),
            }
        }
        OP_SETSIZE => {
            // M14: truncate/extend the file at handle to arg1 bytes.
            // Extends are pre-zeroed; excess clusters are freed. Reply
            // arg0 = resulting size.
            let res: Option<(u64, String)> = {
                let open = OPEN.lock();
                let entry = match open
                    .iter()
                    .find(|o| o.handle == req.arg0 && o.owner == from)
                {
                    Some(e) => e,
                    None => {
                        drop(open);
                        err(from);
                        return;
                    }
                };
                if !has_disk(from) {
                    drop(open);
                    err(from);
                    return;
                }
                match &entry.target {
                    OpenTarget::DiskFile(path) => {
                        let mut disk = DISK.lock();
                        let vol = match disk.as_mut() {
                            Some(v) => v,
                            None => {
                                drop(open);
                                err(from);
                                return;
                            }
                        };
                        vol.set_size(path, req.arg1)
                            .map(|sz| (sz, alloc::format!("/disk{}", path)))
                    }
                    _ => None,
                }
            };
            match res {
                Some((sz, path)) => {
                    kprintln!("[vfs] setsize {} -> {}", safe_path(&path), sz);
                    reply(from, REPLY_OK, sz, &[]);
                }
                None => err(from),
            }
        }
        OP_CREATE | OP_DELETE | OP_MKDIR => {
            // M14 disk mutations: payload = path, /disk/* only, admin
            // gate (init/shell). Ramfs files are &'static constants —
            // mutating them would make boot state lie.
            let path = core::str::from_utf8(req.payload_bytes()).unwrap_or("");
            let sub = path.strip_prefix("/disk").unwrap_or("");
            let sub = if sub.is_empty() { "/" } else { sub };
            if !admin(from) || sub.len() < 2 || !sub.starts_with('/') {
                err(from);
                return;
            }
            let mut disk = DISK.lock();
            let vol = match disk.as_mut() {
                Some(v) => v,
                None => {
                    err(from);
                    return;
                }
            };
            let ok = match req.kind {
                OP_CREATE => vol.create(sub),
                OP_DELETE => vol.delete(sub),
                _ => vol.mkdir(sub),
            };
            let full = alloc::format!("/disk{}", sub);
            drop(disk);
            if ok {
                match req.kind {
                    OP_CREATE => kprintln!("[vfs] created {}", safe_path(&full)),
                    OP_DELETE => kprintln!("[vfs] deleted {}", safe_path(&full)),
                    _ => kprintln!("[vfs] mkdir {} ok", safe_path(&full)),
                }
                reply(from, REPLY_OK, 0, &[]);
            } else {
                err(from);
            }
        }
        // M18: notepad collects the .txt path winsrv registered for it.
        // CAP_DISK gate (the intake is the editor's whole job) and keyed
        // by sender: no ticket, no file, no lie.
        OP_TXT_TAKE => {
            if !has_disk(from) {
                err(from);
                return;
            }
            let mut pend = PENDING_TXT.lock();
            let idx = pend.iter().position(|(t, _)| *t == from);
            let Some(i) = idx else {
                drop(pend);
                err(from);
                return;
            };
            let (_, path) = pend.remove(i);
            drop(pend);
            reply(from, REPLY_OK, path.len() as u64, path.as_bytes());
        }
        _ => err(from),
    }
}

pub fn init() {
    // Mount the disk now (reads BPB via the block driver).
    match Fat32::open() {
        Some(vol) => {
            *DISK.lock() = Some(vol);
            kprintln!("[servers] vfs: ramfs + FAT32 disk + /ps");
            // M16: the Files places exist from first boot (Desktop /
            // Documents / Downloads / Recycle Bin). Idempotent; the
            // count tells the log how many were actually new.
            let (made, _) = ensure_places();
            if made > 0 {
                kprintln!("[vfs] places: {} new (Desktop, Documents, Downloads, bin)", made);
            } else {
                kprintln!("[vfs] places ready");
            }
        }
        None => kprintln!("[servers] vfs: ramfs + /ps (no disk)"),
    }
}

/// Kernel task entry. Loops forever serving PORT_VFS.
pub unsafe extern "C" fn server_main() -> ! {
    loop {
        let req = ipc::recv(PORT_VFS);
        handle(req);
    }
}
