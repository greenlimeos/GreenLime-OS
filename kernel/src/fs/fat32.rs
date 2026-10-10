//! FAT32 reader (read-only, 512-byte sectors, no LFN).
//! Enough to load files and list directories off a test disk.

use alloc::string::String;
use alloc::vec::Vec;

const SECTOR: usize = 512;
const EOC_MIN: u32 = 0x0FFF_FFF8;

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

pub struct Fat32 {
    sectors_per_cluster: u8,
    fat_start: u32, // lba
    data_start: u32, // lba of cluster 2
    root_cluster: u32,
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
            return None; // v0.7 supports 512-byte sectors only
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
            data_start: reserved + fats * fatsz,
            root_cluster: root,
        })
    }

    fn read_sector(&self, lba: u64, buf: &mut [u8; SECTOR]) -> bool {
        crate::drivers::storage::read_block(lba, buf)
    }

    fn cluster_lba(&self, c: u32) -> u64 {
        self.data_start as u64 + (c - 2) as u64 * self.sectors_per_cluster as u64
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

    /// Read a whole cluster chain starting at `start` (up to `max` bytes).
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

    fn split(path: &str) -> Vec<&str> {
        path.split('/').filter(|s| !s.is_empty()).collect()
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

    /// Walk to a path: Ok((dir_cluster_of_parent_or_self, entry?)).
    /// Returns the dir cluster + optional (is_dir, first_cluster, size).
    fn walk(&self, path: &str) -> Option<(u32, Option<(bool, u32, u32)>)> {
        let mut dir = self.root_cluster;
        let parts = Self::split(path);
        if parts.is_empty() {
            return Some((dir, None)); // root itself
        }
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            let mut found: Option<(bool, u32, u32)> = None;
            // Scan dir entries across the chain (cap: whole dir or 64KB).
            let raw = self.read_chain(dir, 64 * 1024)?;
            let mut o = 0usize;
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
                    found = Some((e[11] & 0x10 != 0, first.max(2), size));
                    break;
                }
            }
            let (is_dir, first, _size) = found?;
            if !last && !is_dir {
                return None; // path goes through a file
            }
            if last {
                return Some((dir, Some((is_dir, first, _size))));
            }
            if first < 2 {
                return None; // descending into an empty dir: nothing there
            }
            dir = first;
        }
        None
    }

    /// Read a whole file (cap 1 MiB).
    pub fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        let (_, ent) = self.walk(path)?;
        let (is_dir, first, size) = ent?;
        if is_dir {
            return None;
        }
        if size as usize > 1024 * 1024 {
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
        let (_, ent) = self.walk(path)?;
        let start = match ent {
            Some((true, first, _)) => first,
            Some((false, _, _)) => return None,
            None => self.root_cluster, // path was root itself
        };
        if start < 2 {
            return Some(Vec::new()); // empty directory
        }
        let raw = self.read_chain(start, 64 * 1024)?;
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
}
