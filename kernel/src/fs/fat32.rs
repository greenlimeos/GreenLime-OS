//! FAT32 filesystem (512-byte sectors, no LFN).
//! v0.x-v1.0: read-only (load files and list directories off a disk).
//! M14 (v1.1): full write path — FAT chain alloc/free, 8.3 dir entry
//! create/delete, mkdir, file write/extend/truncate. Write semantics:
//! * single FAT mirror maintained (FAT[0] only — clients that care about
//!   the second copy rescan; stale FSInfo free-counts stay legal),
//! * timestamps are fixed 2000-01-01 00:00 (valid non-zero DOS stamps,
//!   no RTC dependency inside the FS),
//! * whole write paths go straight through the polling block driver:
//!   no cache, so a write is durable when the syscall returns.
//! Only Fat32 ever calls storage::write_* — always from the single vfs
//! task behind DISK: Mutex, so &mut self methods are race-free.

use alloc::string::String;
use alloc::vec::Vec;

const SECTOR: usize = 512;
const EOC_MIN: u32 = 0x0FFF_FFF8;
const EOC: u32 = 0x0FFF_FFFF;

/// Hard cap for one file, matching the v1.0 read cap.
const FILE_MAX: usize = 1024 * 1024;
/// Hard cap for one directory chain, matching the v1.0 scan cap.
const DIR_MAX: usize = 64 * 1024;
/// Path depth cap (create/mkdir/delete): cheap loop guard.
const DEPTH_MAX: usize = 16;

/// Fixed creation/mod stamp: 2000-01-01 00:00 = (2000-1980)<<9 | 1<<5 | 1.
const STAMP_DATE: u16 = 0x2821;
const STAMP_TIME: u16 = 0;

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

pub struct Fat32 {
    sectors_per_cluster: u8,
    fat_start: u32, // lba
    fat_sectors: u32,
    data_start: u32, // lba of cluster 2
    root_cluster: u32,
    /// First-fit resume cursor: next cluster to check for free space.
    scan_hint: u32,
}

/// A usable 32-byte slot inside a dir chain, from find_slot().
struct DirSlot {
    /// Byte offset of the slot inside the dir chain.
    off: u64,
    /// The slot was an end-of-directory 0x00 marker before we used it.
    was_term: bool,
    /// That marker was the LAST slot of the chain (we pre-extended the
    /// chain so a fresh 0x00 terminator now sits at off + 32).
    consumed_last: bool,
}

impl Fat32 {
    /// Open the volume (reads + validates the BPB). None if not FAT32.
    pub fn open() -> Option<Self> {
        let mut sec = [0u8; SECTOR];
        if !crate::drivers::storage::read_block(0, &mut sec) {
            return None;
        }
        if sec[510] != 0x55 || sec[511] != 0xAA {
            return None;
        }
        if &sec[82..90] != b"FAT32   " {
            return None;
        }
        let bps = le16(&sec, 11);
        if bps != 512 {
            return None; // supports 512-byte sectors only
        }
        let spc = sec[13];
        let reserved = le16(&sec, 14) as u32;
        let fats = sec[16] as u32;
        let fatsz = le32(&sec, 36);
        let root = le32(&sec, 44);
        if spc == 0 || fats == 0 || fatsz == 0 || root < 2 {
            return None;
        }
        Some(Self {
            sectors_per_cluster: spc,
            fat_start: reserved,
            fat_sectors: fats * fatsz,
            data_start: reserved + fats * fatsz,
            root_cluster: root,
            scan_hint: 2,
        })
    }

    // ---------------- sector + FAT primitives ----------------

    fn read_sector(&self, lba: u64, buf: &mut [u8; SECTOR]) -> bool {
        crate::drivers::storage::read_block(lba, buf)
    }

    fn write_sector(&self, lba: u64, buf: &[u8; SECTOR]) -> bool {
        crate::drivers::storage::write_block(lba, buf)
    }

    fn cluster_lba(&self, c: u32) -> u64 {
        self.data_start as u64 + (c - 2) as u64 * self.sectors_per_cluster as u64
    }

    fn cluster_bytes(&self) -> usize {
        self.sectors_per_cluster as usize * SECTOR
    }

    fn fat_entry(&self, cluster: u32) -> Option<u32> {
        let off = cluster as u64 * 4;
        let sec = self.fat_start as u64 + off / SECTOR as u64;
        let o = (off % SECTOR as u64) as usize;
        // Entry may straddle sectors; read two and stitch (simple + rare).
        let mut a = [0u8; SECTOR];
        let mut b = [0u8; SECTOR];
        if !self.read_sector(sec, &mut a) {
            return None;
        }
        let mut raw = [0u8; 4];
        for i in 0..4 {
            let (s, k) = if o + i < SECTOR {
                (&a, o + i)
            } else {
                if !self.read_sector(sec + 1, &mut b) {
                    return None;
                }
                (&b, o + i - SECTOR)
            };
            raw[i] = s[k];
        }
        Some(u32::from_le_bytes(raw) & 0x0FFF_FFFF)
    }

    /// Set one FAT entry (read-modify-write of the single sector that
    /// holds it: cluster*4 is a 4-aligned offset, and 512 % 4 == 0, so a
    /// 32-bit entry never straddles sectors). M14 keeps only the first
    /// FAT copy in sync.
    fn fat_set_entry(&mut self, cluster: u32, value: u32) -> bool {
        let off = cluster as u64 * 4;
        let sec = self.fat_start as u64 + off / SECTOR as u64;
        let o = (off % SECTOR as u64) as usize;
        let mut s = [0u8; SECTOR];
        if !self.read_sector(sec, &mut s) {
            return false;
        }
        s[o..o + 4].copy_from_slice(&(value & 0x0FFF_FFFF).to_le_bytes());
        self.write_sector(sec, &s)
    }

    /// First-fit free-cluster scan from `scan_hint` (wrapping once round
    /// the FAT). Marks the winner EOC in place and bumps the cursor.
    /// None = volume full or FAT unreadable.
    fn alloc_one(&mut self) -> Option<u32> {
        let slots = self.fat_sectors as u64 * (SECTOR as u64 / 4);
        if slots <= 2 {
            return None;
        }
        let span = slots - 2;
        let start = (self.scan_hint.max(2) as u64 - 2) % span;
        for step in 0..span {
            let c = ((start + step) % span + 2) as u32;
            if self.fat_entry(c) == Some(0) && self.fat_set_entry(c, EOC) {
                self.scan_hint = c + 1;
                return Some(c);
            }
        }
        None
    }

    /// Free a whole chain (writes 0 over every entry). Bounded so a
    /// corrupt (looping) FAT can't hang the disk; best-effort per entry.
    fn free_chain(&mut self, first: u32) {
        if first < 2 {
            return;
        }
        let mut c = first;
        for _ in 0..(FILE_MAX / SECTOR + 2) {
            let next = match self.fat_entry(c) {
                Some(n) => n,
                None => return,
            };
            let _ = self.fat_set_entry(c, 0);
            if next >= EOC_MIN || next < 2 {
                return;
            }
            c = next;
        }
    }

    /// Collect up to `limit` cluster numbers of a chain. first < 2 = an
    /// empty (cluster-less) file: Some(empty). None = unreadable chain.
    fn chain_of(&self, first: u32, limit: usize) -> Option<Vec<u32>> {
        let mut out = Vec::new();
        if first < 2 {
            return Some(out);
        }
        let mut c = first;
        for _ in 0..(FILE_MAX / SECTOR + 2) {
            if out.len() < limit {
                out.push(c);
            } else {
                break;
            }
            let n = self.fat_entry(c)?;
            if n >= EOC_MIN {
                break;
            }
            if n < 2 {
                return None; // corrupt link
            }
            c = n;
        }
        Some(out)
    }

    /// Map a byte index inside a cluster chain to a physical LBA.
    fn chain_pos_lba(&self, chain: &[u32], pos: u64) -> Option<u64> {
        let cb = self.cluster_bytes() as u64;
        let c = *chain.get((pos / cb) as usize)?;
        Some(self.cluster_lba(c) + (pos % cb) / SECTOR as u64)
    }

    fn read_chain(&self, start: u32, max: usize) -> Option<Vec<u8>> {
        if start < 2 {
            return None; // cluster 0/1 = empty dir, handled by callers
        }
        let mut out = Vec::new();
        let mut c = start;
        let mut sec = [0u8; SECTOR];
        loop {
            for _ in 0..self.sectors_per_cluster {
                if !self.read_sector(self.cluster_lba(c), &mut sec) {
                    return None;
                }
                let room = max.saturating_sub(out.len()).min(SECTOR);
                out.extend_from_slice(&sec[..room]);
                if out.len() >= max {
                    return Some(out);
                }
            }
            let n = self.fat_entry(c)?;
            if n >= EOC_MIN {
                return Some(out);
            }
            if n < 2 {
                return None;
            }
            c = n;
        }
    }

    /// Zero every sector of one cluster (fresh allocations, mkdir).
    fn zero_cluster(&mut self, c: u32) -> Option<()> {
        let z = [0u8; SECTOR];
        for s in 0..self.sectors_per_cluster as u64 {
            if !self.write_sector(self.cluster_lba(c) + s, &z) {
                return None;
            }
        }
        Some(())
    }

    /// Zero-fill [from, to) bytes of a file/cluster chain (holes created
    /// by writing past the end, and truncation-extend ranges).
    fn fill_zeros(&mut self, chain: &[u32], from: u64, to: u64) -> bool {
        if to <= from {
            return true;
        }
        let mut p = from / SECTOR as u64 * SECTOR as u64;
        while p < to {
            let sec_end = p - p % SECTOR as u64 + SECTOR as u64;
            let a = (from.max(p)) as usize - (p as usize);
            let b = (to.min(sec_end)) as usize - (p as usize);
            let lba = match self.chain_pos_lba(chain, p) {
                Some(l) => l,
                None => return false,
            };
            if a == 0 && b == SECTOR {
                let z = [0u8; SECTOR];
                if !self.write_sector(lba, &z) {
                    return false;
                }
            } else {
                let mut s = [0u8; SECTOR];
                if !self.read_sector(lba, &mut s) {
                    return false;
                }
                s[a..b].fill(0);
                if !self.write_sector(lba, &s) {
                    return false;
                }
            }
            p = sec_end;
        }
        true
    }

    /// Write `data` at byte `off` inside a cluster chain (the chain must
    /// already cover the whole range). Full sectors skip the read.
    fn write_span(&mut self, chain: &[u32], off: u64, data: &[u8]) -> bool {
        let end = off + data.len() as u64;
        let first_sec = off / SECTOR as u64;
        let last_sec = (end - 1) / SECTOR as u64;
        let mut di = 0usize; // cursor into data
        for s in first_sec..=last_sec {
            let a = if s == first_sec {
                (off % SECTOR as u64) as usize
            } else {
                0
            };
            let b = if s == last_sec {
                ((end - 1) % SECTOR as u64) as usize + 1
            } else {
                SECTOR
            };
            let lba = match self.chain_pos_lba(chain, s * SECTOR as u64) {
                Some(l) => l,
                None => return false,
            };
            if a == 0 && b == SECTOR {
                let mut buf = [0u8; SECTOR];
                buf.copy_from_slice(&data[di..di + SECTOR]);
                if !self.write_sector(lba, &buf) {
                    return false;
                }
            } else {
                let mut buf = [0u8; SECTOR];
                if !self.read_sector(lba, &mut buf) {
                    return false;
                }
                buf[a..b].copy_from_slice(&data[di..di + (b - a)]);
                if !self.write_sector(lba, &buf) {
                    return false;
                }
            }
            di += b - a;
        }
        true
    }

    // ---------------- names + walks ----------------

    fn split(path: &str) -> Vec<&str> {
        path.split('/').filter(|s| !s.is_empty()).collect()
    }

    /// Build an on-disk 8.3 short name. ASCII; single dot; base 1..=8,
    /// ext 0..=3 with no nested dot; '.'/'..' rejected. Names are
    /// uppercased (the v0.x reader is case-insensitive on lookup too).
    fn to_83(name: &str) -> Option<[u8; 11]> {
        const SPECIAL: &[u8] = b"!#$%&()@^_`{}~'-";
        if name.is_empty() {
            return None;
        }
        let up: String = name.chars().map(|c| c.to_ascii_uppercase()).collect();
        let bytes = up.as_bytes();
        if bytes.iter().any(|&b| b < 0x21 || b > 0x7E) {
            return None; // printable ASCII (no spaces) only
        }
        let (base, ext) = match bytes.iter().position(|&b| b == b'.') {
            Some(i) => (&bytes[..i], &bytes[i + 1..]),
            None => (bytes, &[][..]),
        };
        if base.is_empty() || base.len() > 8 || ext.len() > 3 {
            return None;
        }
        if ext.iter().any(|&b| b == b'.') {
            return None;
        }
        if base == b"." || base == b".." {
            return None;
        }
        let ok = |s: &[u8]| {
            s.iter()
                .all(|&b| b.is_ascii_alphanumeric() || SPECIAL.contains(&b))
        };
        if !ok(base) || !ok(ext) {
            return None;
        }
        let mut out = [b' '; 11];
        out[..base.len()].copy_from_slice(base);
        out[8..8 + ext.len()].copy_from_slice(ext);
        Some(out)
    }

    /// 8.3 uppercase compare (ASCII only — test disk is ASCII).
    fn name_eq(raw_name: &[u8; 8], raw_ext: &[u8; 3], want: &str) -> bool {
        let mut want_up = [0u8; 13];
        let mut wn = 0usize;
        for &b in want.as_bytes() {
            if wn >= want_up.len() {
                return false;
            }
            want_up[wn] = b.to_ascii_uppercase();
            wn += 1;
        }
        let want = &want_up[..wn];
        // Split WANT at the dot.
        let (wbase, wext) = match want.iter().position(|&b| b == b'.') {
            Some(i) => (&want[..i], &want[i + 1..]),
            None => (want, &[][..]),
        };
        if wbase.len() > 8 || wext.len() > 3 {
            return false;
        }
        let mut have_base = [b' '; 8];
        let mut have_ext = [b' '; 3];
        have_base[..wbase.len()].copy_from_slice(wbase);
        have_ext[..wext.len()].copy_from_slice(wext);
        raw_name == &have_base && raw_ext == &have_ext
    }

    /// Walk to a path with full bookkeeping: returns the cluster of the
    /// dir CONTAINING the final component (root's cluster for "/"), the
    /// entry when the final component exists, and the byte offset of its
    /// 32-byte slot inside that dir chain. When the final component is
    /// missing, (dir, None, None) is returned with dir still valid — so
    /// create/mkdir get the parent from the same call family.
    fn locate(
        &self,
        path: &str,
    ) -> Option<(u32, Option<(bool, u32, u32)>, Option<u64>)> {
        let mut dir = self.root_cluster;
        let parts = Self::split(path);
        if parts.is_empty() {
            return Some((dir, None, None)); // root itself
        }
        if parts.len() > DEPTH_MAX {
            return None;
        }
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            let raw = if dir >= 2 {
                self.read_chain(dir, DIR_MAX)?
            } else {
                // Inside an empty (cluster-less) dir nothing exists.
                return Some((dir, None, None));
            };
            let mut o = 0usize;
            let mut found: Option<(bool, u32, u32, u64)> = None;
            while o + 32 <= raw.len() {
                let e = &raw[o..o + 32];
                o += 32;
                if e[0] == 0x00 {
                    break; // end of directory
                }
                if e[0] == 0xE5 || e[11] == 0x0F || e[11] & 0x08 != 0 {
                    continue; // deleted / LFN / volume label
                }
                let mut name = [0u8; 8];
                let mut ext = [0u8; 3];
                name.copy_from_slice(&e[0..8]);
                ext.copy_from_slice(&e[8..11]);
                if &name == b".       " || &name == b"..      " {
                    continue;
                }
                if Self::name_eq(&name, &ext, part) {
                    let hi = le16(e, 20) as u32;
                    let lo = le16(e, 26) as u32;
                    let first = (hi << 16) | lo;
                    let size = le32(e, 28);
                    // TRUE first cluster (0 = empty file), no .max() —
                    // write paths need the real value.
                    found =
                        Some((e[11] & 0x10 != 0, first, size, (o - 32) as u64));
                    break;
                }
            }
            match found {
                Some((is_dir, first, size, slot)) => {
                    if last {
                        return Some((dir, Some((is_dir, first, size)), Some(slot)));
                    }
                    if !is_dir || first < 2 {
                        return None; // path goes through a file / empty dir
                    }
                    dir = first;
                }
                None => {
                    // Final component (or an ancestor) doesn't exist.
                    return Some((dir, None, None));
                }
            }
        }
        None
    }

    /// Cluster of the dir that should contain the FINAL component of
    /// `path`, true when that dir is the root. Fails unless the parent
    /// really exists as a directory.
    fn parent_dir(&self, path: &str) -> Option<(u32, bool)> {
        let parts = Self::split(path);
        if parts.len() <= 1 {
            return Some((self.root_cluster, true));
        }
        if parts.len() > DEPTH_MAX {
            return None;
        }
        let mut p = String::new();
        for part in &parts[..parts.len() - 1] {
            p.push('/');
            p.push_str(part);
        }
        let (_, ent, _) = self.locate(&p)?;
        match ent {
            Some((true, first, _)) if first >= 2 => Some((first, false)),
            _ => None,
        }
    }

    /// Read a dir chain raw. The returned length IS the chain length as
    /// long as it is under DIR_MAX (dirs at/over the cap are unusable
    /// for writes). None = unreadable or capped.
    fn dir_raw(&self, dir: u32) -> Option<Vec<u8>> {
        let raw = if dir >= 2 {
            self.read_chain(dir, DIR_MAX)?
        } else {
            // Empty dir without clusters: treat as a fresh empty chain.
            return Some(Vec::new());
        };
        if raw.len() >= DIR_MAX {
            return None;
        }
        Some(raw)
    }

    /// Find a usable 32-byte slot for a NEW entry in `dir`, maintaining
    /// the FAT end-of-directory marker: remaining_free = internal/ext
    /// slots first (E5 without ceremony), else the 0x00 terminator slot;
    /// when everything is live or the terminator sat at the very end of
    /// the chain, a fresh zeroed cluster is appended (its slot 0 becomes
    /// the new marker). None = dir unreadable/full or disk error.
    fn dir_slot(&mut self, dir: u32) -> Option<DirSlot> {
        let raw = self.dir_raw(dir)?;
        let n = raw.len();
        let mut o = 0usize;
        while o + 32 <= n {
            let b0 = raw[o];
            if b0 == 0x00 {
                if o + 32 == n {
                    // We want the terminator itself as our entry, but
                    // there is no next slot to carry the new marker —
                    // extend first, then use this slot.
                    if self.dir_extend(dir).is_none() {
                        return None;
                    }
                    return Some(DirSlot {
                        off: o as u64,
                        was_term: true,
                        consumed_last: true,
                    });
                }
                return Some(DirSlot {
                    off: o as u64,
                    was_term: true,
                    consumed_last: false,
                });
            }
            if b0 == 0xE5 {
                return Some(DirSlot {
                    off: o as u64,
                    was_term: false,
                    consumed_last: false,
                });
            }
            o += 32;
        }
        // Every slot is live: append a cluster; its first slot (byte n)
        // is zeroed (new terminator) and becomes the entry slot.
        if self.dir_extend(dir).is_none() {
            return None;
        }
        Some(DirSlot {
            off: n as u64,
            was_term: false,
            consumed_last: false,
        })
    }

    /// Append one zeroed cluster to a dir chain, linking the previous
    /// last cluster to it. Returns the new chain length.
    fn dir_extend(&mut self, dir: u32) -> Option<u64> {
        let raw_len = self.dir_raw(dir)?.len() as u64;
        let chain = self.chain_of(dir, DIR_MAX / 32 + 2)?;
        if chain.is_empty() {
            return None; // can't link into a cluster-less dir
        }
        let last = chain[chain.len() - 1];
        let nc = self.alloc_one()?;
        if !self.fat_set_entry(last, nc) {
            self.free_chain(nc);
            return None;
        }
        if self.zero_cluster(nc).is_none() {
            self.free_chain(nc);
            return None;
        }
        Some(raw_len + self.cluster_bytes() as u64)
    }

    /// After using a 0x00-terminator slot that had successors, restore
    /// the marker: first byte of the slot at `off` becomes 0x00.
    fn write_term_at(&mut self, dir: u32, off: u64) -> bool {
        let chain = match self.chain_of(dir, DIR_MAX / 32 + 2) {
            Some(c) => c,
            None => return false,
        };
        let lba = match self.chain_pos_lba(&chain, off) {
            Some(l) => l,
            None => return false,
        };
        let mut s = [0u8; SECTOR];
        if !self.read_sector(lba, &mut s) {
            return false;
        }
        s[(off % SECTOR as u64) as usize] = 0x00;
        self.write_sector(lba, &s)
    }

    /// Write an entire 32-byte dir entry at `off` in `dir`'s chain.
    /// 512 % 32 == 0, so the slot never straddles a sector.
    fn write_entry_at(&mut self, dir: u32, off: u64, ent: &[u8; 32]) -> bool {
        let chain = match self.chain_of(dir, DIR_MAX / 32 + 2) {
            Some(c) => c,
            None => return false,
        };
        let lba = match self.chain_pos_lba(&chain, off) {
            Some(l) => l,
            None => return false,
        };
        let o = (off % SECTOR as u64) as usize;
        let mut s = [0u8; SECTOR];
        if !self.read_sector(lba, &mut s) {
            return false;
        }
        s[o..o + 32].copy_from_slice(ent);
        self.write_sector(lba, &s)
    }

    /// Patch first-cluster and/or size fields of an existing entry.
    fn patch_entry(
        &mut self,
        dir: u32,
        off: u64,
        first: Option<u32>,
        size: Option<u32>,
    ) -> bool {
        let chain = match self.chain_of(dir, DIR_MAX / 32 + 2) {
            Some(c) => c,
            None => return false,
        };
        let lba = match self.chain_pos_lba(&chain, off) {
            Some(l) => l,
            None => return false,
        };
        let o = (off % SECTOR as u64) as usize;
        if o + 32 > SECTOR {
            return false;
        }
        let mut s = [0u8; SECTOR];
        if !self.read_sector(lba, &mut s) {
            return false;
        }
        if let Some(f) = first {
            s[o + 20..o + 22]
                .copy_from_slice(&((f >> 16) as u16).to_le_bytes());
            s[o + 26..o + 28]
                .copy_from_slice(&((f & 0xFFFF) as u16).to_le_bytes());
        }
        if let Some(sz) = size {
            s[o + 28..o + 32].copy_from_slice(&sz.to_le_bytes());
        }
        self.write_sector(lba, &s)
    }

    /// Build a fresh 32-byte short-name entry (size 0; fixed stamps).
    fn dir_entry(name11: &[u8; 11], attr: u8, first: u32) -> [u8; 32] {
        let mut e = [0u8; 32];
        e[0..8].copy_from_slice(&name11[0..8]);
        e[8..11].copy_from_slice(&name11[8..11]);
        e[11] = attr;
        e[14..16].copy_from_slice(&STAMP_TIME.to_le_bytes());
        e[16..18].copy_from_slice(&STAMP_DATE.to_le_bytes());
        e[18..20].copy_from_slice(&STAMP_DATE.to_le_bytes());
        e[20..22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
        e[22..24].copy_from_slice(&STAMP_TIME.to_le_bytes());
        e[24..26].copy_from_slice(&STAMP_DATE.to_le_bytes());
        e[26..28].copy_from_slice(&((first & 0xFFFF) as u16).to_le_bytes());
        e // size @28 stays 0
    }

    /// True when `dir` (a dir cluster, >= 2) holds no visible entries.
    /// Dots/deleted/LFN/label slots don't count.
    fn dir_empty(&self, dir: u32) -> Option<bool> {
        if dir < 2 {
            return Some(true); // cluster-less dir: structurally empty
        }
        let raw = self.dir_raw(dir)?;
        let mut o = 0usize;
        while o + 32 <= raw.len() {
            let e = &raw[o..o + 32];
            o += 32;
            if e[0] == 0x00 {
                break;
            }
            if e[0] == 0xE5 || e[11] == 0x0F || e[11] & 0x08 != 0 {
                continue;
            }
            let name = &e[0..8];
            if name == b".       " || name == b"..      " {
                continue;
            }
            return Some(false);
        }
        Some(true)
    }

    // ---------------- read API (v0.x contract, unchanged) ----------------

    /// Read a whole file (cap 1 MiB).
    pub fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        let (_, ent, _) = self.locate(path)?;
        let (is_dir, first, size) = ent?;
        if is_dir {
            return None;
        }
        if size as usize > FILE_MAX {
            return None;
        }
        if size == 0 {
            return Some(Vec::new()); // empty file: no chain
        }
        let mut data = self.read_chain(first, size as usize)?;
        data.truncate(size as usize);
        Some(data)
    }

    /// List a directory: names (dirs suffixed '/').
    pub fn list_dir(&self, path: &str) -> Option<Vec<String>> {
        let (_, ent, _) = self.locate(path)?;
        let start = match ent {
            Some((true, first, _)) => first,
            Some((false, _, _)) => return None,
            None => self.root_cluster, // path was root itself
        };
        if start < 2 {
            return Some(Vec::new()); // empty directory
        }
        let raw = self.read_chain(start, DIR_MAX)?;
        let mut out = Vec::new();
        let mut o = 0usize;
        while o + 32 <= raw.len() {
            let e = &raw[o..o + 32];
            o += 32;
            if e[0] == 0x00 {
                break;
            }
            if e[0] == 0xE5 || e[11] == 0x0F || e[11] & 0x08 != 0 {
                continue;
            }
            let base = core::str::from_utf8(&e[0..8]).unwrap_or("").trim_end();
            let ext = core::str::from_utf8(&e[8..11]).unwrap_or("").trim_end();
            if base == "." || base == ".." || base.is_empty() {
                continue;
            }
            let mut name = String::from(base);
            if !ext.is_empty() {
                name.push('.');
                name.push_str(ext);
            }
            if e[11] & 0x10 != 0 {
                name.push('/');
            }
            out.push(name);
        }
        Some(out)
    }

    // ---------------- write API (M14, v1.1) ----------------

    /// Create an empty file. False if it exists / parent missing /
    /// name un-representable / disk full or erroring.
    pub fn create(&mut self, path: &str) -> bool {
        let name = match Self::split(path).last() {
            Some(n) => *n,
            None => return false,
        };
        let (pdir, _) = match self.parent_dir(path) {
            Some(x) => x,
            None => return false,
        };
        if let Some((_, Some(_), _)) = self.locate(path) {
            return false; // already there
        }
        let name11 = match Self::to_83(name) {
            Some(n) => n,
            None => return false,
        };
        let slot = match self.dir_slot(pdir) {
            Some(s) => s,
            None => return false,
        };
        let ent = Self::dir_entry(&name11, 0x20, 0); // archive, no cluster
        if !self.write_entry_at(pdir, slot.off, &ent) {
            return false;
        }
        if slot.was_term && !slot.consumed_last {
            // Restore the end-of-directory marker one slot later.
            if !self.write_term_at(pdir, slot.off + 32) {
                return false;
            }
        }
        true
    }

    /// Create a directory (one fresh cluster: '.', '..' + terminator).
    /// '..' points at the parent's cluster, or 0 when the parent is the
    /// root (root is not a cluster — classic FAT32 convention; our
    /// reader skips dot entries by name anyway).
    pub fn mkdir(&mut self, path: &str) -> bool {
        let name = match Self::split(path).last() {
            Some(n) => *n,
            None => return false,
        };
        let (pdir, proot) = match self.parent_dir(path) {
            Some(x) => x,
            None => return false,
        };
        if let Some((_, Some(_), _)) = self.locate(path) {
            return false;
        }
        let name11 = match Self::to_83(name) {
            Some(n) => n,
            None => return false,
        };
        let slot = match self.dir_slot(pdir) {
            Some(s) => s,
            None => return false,
        };
        let nc = match self.alloc_one() {
            Some(c) => c,
            None => return false,
        };
        // '.' -> self, '..' -> parent (0 = root parent). Literals are the
        // full 11 short-name bytes (8 base + 3 space-padded ext).
        let dot = Self::dir_entry(b".          ", 0x10, nc);
        let dotdot = if proot {
            Self::dir_entry(b"..         ", 0x10, 0)
        } else {
            Self::dir_entry(b"..         ", 0x10, pdir)
        };
        // First sector of the new cluster: the three entries.
        let lba0 = self.cluster_lba(nc);
        let mut s = [0u8; SECTOR];
        s[0..32].copy_from_slice(&dot);
        s[32..64].copy_from_slice(&dotdot);
        // slot 2 stays 0x00 = end of the new dir.
        let mut ok = self.write_sector(lba0, &s);
        if ok {
            let z = [0u8; SECTOR];
            for i in 1..self.sectors_per_cluster as u64 {
                if !self.write_sector(lba0 + i, &z) {
                    ok = false;
                    break;
                }
            }
        }
        if !self.write_entry_at(
            pdir,
            slot.off,
            &Self::dir_entry(&name11, 0x10, nc),
        ) {
            ok = false;
        }
        if ok && slot.was_term && !slot.consumed_last {
            ok = self.write_term_at(pdir, slot.off + 32);
        }
        if !ok {
            // Try to undo: drop the new dir cluster and mark the entry
            // deleted (best-effort; leakers are reparable by re-format).
            let _ = self.free_chain(nc);
            let _ = self.patch_entry(pdir, slot.off, Some(0), Some(0));
            let _ = self.write_e5(pdir, slot.off);
            return false;
        }
        true
    }

    /// Delete a file, or a directory when it is empty. Root can't go.
    pub fn delete(&mut self, path: &str) -> bool {
        let (dir, ent, slot) = match self.locate(path) {
            Some(x) => x,
            None => return false,
        };
        let (is_dir, first, _size) = match ent {
            Some(e) => e,
            None => return false, // not found (also covers "/")
        };
        if is_dir && self.dir_empty(first) != Some(true) {
            return false;
        }
        let slot = match slot {
            Some(s) => s,
            None => return false,
        };
        if first >= 2 {
            self.free_chain(first);
        }
        self.write_e5(dir, slot)
    }

    /// Mark an entry deleted: E5 in the slot's first byte (the rest of
    /// the entry stays stale, which is legal; scans stop at E5).
    fn write_e5(&mut self, dir: u32, off: u64) -> bool {
        let chain = match self.chain_of(dir, DIR_MAX / 32 + 2) {
            Some(c) => c,
            None => return false,
        };
        let lba = match self.chain_pos_lba(&chain, off) {
            Some(l) => l,
            None => return false,
        };
        let o = (off % SECTOR as u64) as usize;
        let mut s = [0u8; SECTOR];
        if !self.read_sector(lba, &mut s) {
            return false;
        }
        s[o] = 0xE5;
        self.write_sector(lba, &s)
    }

    /// pwrite `data` at byte `off` of the file at `path`. Writes past
    /// the current size extend the file (new clusters appended); the gap
    /// between old size and `off` is zero-filled (holes read as zeros).
    /// Returns the new size. None = missing/not-a-file/too big/error.
    pub fn write_at(&mut self, path: &str, off: u64, data: &[u8]) -> Option<u64> {
        let (dir, ent, slot) = self.locate(path)?;
        let (is_dir, first, size) = ent?;
        if is_dir {
            return None;
        }
        let slot = slot?;
        if data.is_empty() {
            // Nothing to write; the size stays (reads stop at size, so
            // a bare seek-past-end changes nothing visible).
            return Some(size as u64);
        }
        let off = off as usize;
        let end = off + data.len();
        if end > FILE_MAX {
            return None;
        }
        let cb = self.cluster_bytes();
        let needed = (end + cb - 1) / cb;
        // Current chain + extension to `needed` clusters.
        let mut chain = self.chain_of(first, needed.max(1))?;
        let mut head = first;
        let mut head_dirty = false;
        while chain.len() < needed {
            let nc = self.alloc_one()?;
            match chain.last() {
                Some(&prev) => {
                    if !self.fat_set_entry(prev, nc) {
                        self.free_chain(nc);
                        return None;
                    }
                }
                None => {
                    head = nc;
                    head_dirty = true; // was an empty file (first == 0)
                }
            }
            chain.push(nc);
        }
        if (off > size as usize)
            && !self.fill_zeros(&chain, size as u64, off as u64)
        {
            return None;
        }
        if !self.write_span(&chain, off as u64, data) {
            return None;
        }
        let f = if head_dirty { Some(head) } else { None };
        if !self.patch_entry(dir, slot, f, Some(end as u32)) {
            return None;
        }
        Some(end as u64)
    }

    /// Truncate or extend the file at `path` to `want` bytes. Extends
    /// are pre-zeroed; truncates free the excess clusters. Returns the
    /// resulting size (== want) or None.
    pub fn set_size(&mut self, path: &str, want: u64) -> Option<u64> {
        if want as usize > FILE_MAX {
            return None;
        }
        let (dir, ent, slot) = self.locate(path)?;
        let (is_dir, first, size) = ent?;
        if is_dir {
            return None;
        }
        let slot = slot?;
        let want = want as usize;
        let cb = self.cluster_bytes();
        let needed = (want + cb - 1) / cb;
        // Whole current chain (in case of truncation, we must free the
        // tail too; bound = the file cap in clusters + 2).
        let mut chain = self.chain_of(first, FILE_MAX / SECTOR + 2)?;
        let mut head = first;
        let mut head_dirty = false;
        if chain.len() > needed {
            // Truncate: re-point the kept tail at EOC, then free the rest.
            let tail: Vec<u32> = chain.split_off(needed);
            if let Some(&last) = chain.last() {
                if !self.fat_set_entry(last, EOC) {
                    return None;
                }
            }
            for &c in &tail {
                let _ = self.fat_set_entry(c, 0);
            }
        } else {
            while chain.len() < needed {
                let nc = self.alloc_one()?;
                match chain.last() {
                    Some(&prev) => {
                        if !self.fat_set_entry(prev, nc) {
                            self.free_chain(nc);
                            return None;
                        }
                    }
                    None => {
                        head = nc;
                        head_dirty = true;
                    }
                }
                chain.push(nc);
            }
            if want > size as usize
                && !self.fill_zeros(&chain, size as u64, want as u64)
            {
                return None;
            }
        }
        let f = if head_dirty || chain.is_empty() { Some(head) } else { None };
        if !self.patch_entry(dir, slot, f, Some(want as u32)) {
            return None;
        }
        Some(want as u64)
    }
}
