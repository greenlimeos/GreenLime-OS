//! Physical Memory Manager: bitmap over Limine USABLE entries, 4 KiB frames.
//! Frames owned by the kernel image, bootloader, or MMIO are never marked
//! free — Limine reports those with non-USABLE types, so we only manage
//! USABLE ranges. Single lock, boot-CPU only (no SMP yet).

use limine::memory_map::EntryType;
use limine::response::MemoryMapResponse;
use spin::Mutex;

pub const FRAME_SIZE: u64 = 4096;
/// Bitmap covers first 4 GiB (1M frames = 128 KiB static). Frames above are
/// left reserved; QEMU gives us ~512 MiB, real PCs rarely need more for v0.2.
const MAX_FRAMES: usize = 1024 * 1024;
const BITMAP_BYTES: usize = MAX_FRAMES / 8;

struct Pmm {
    bitmap: [u8; BITMAP_BYTES], // 1 = used/reserved, 0 = free
    total: u64,
    free: u64,
    hint: usize, // scan resume point
}

static PMM: Mutex<Pmm> = Mutex::new(Pmm {
    bitmap: [0xFF; BITMAP_BYTES], // all reserved until init() proves usable
    total: 0,
    free: 0,
    hint: 0,
});

const fn align_up(x: u64, a: u64) -> u64 {
    (x + a - 1) & !(a - 1)
}

const fn align_down(x: u64, a: u64) -> u64 {
    x & !(a - 1)
}

/// Walk the Limine map, free every USABLE frame. Returns (free, total).
pub fn init(map: &MemoryMapResponse) -> (u64, u64) {
    let mut pmm = PMM.lock();
    let mut total = 0u64;
    for e in map.entries().iter() {
        if e.entry_type != EntryType::USABLE {
            continue;
        }
        let first = align_up(e.base, FRAME_SIZE) / FRAME_SIZE;
        let last = align_down(e.base.saturating_add(e.length), FRAME_SIZE) / FRAME_SIZE;
        for f in first..last {
            if (f as usize) >= MAX_FRAMES {
                break; // above 4 GiB: stays reserved
            }
            let i = f as usize;
            pmm.bitmap[i / 8] &= !(1 << (i % 8));
            total += 1;
        }
    }
    pmm.total = total;
    pmm.free = total;
    pmm.hint = 0;
    (total, total)
}

pub fn alloc_frame() -> Option<u64> {
    let mut pmm = PMM.lock();
    if pmm.free == 0 {
        return None;
    }
    for n in 0..MAX_FRAMES {
        let i = (pmm.hint + n) % MAX_FRAMES;
        if pmm.bitmap[i / 8] & (1 << (i % 8)) == 0 {
            pmm.bitmap[i / 8] |= 1 << (i % 8);
            pmm.free -= 1;
            pmm.hint = (i + 1) % MAX_FRAMES;
            return Some(i as u64 * FRAME_SIZE);
        }
    }
    None
}

pub fn free_frame(frame: u64) {
    debug_assert!(frame % FRAME_SIZE == 0, "free_frame: unaligned {:#x}", frame);
    let i = (frame / FRAME_SIZE) as usize;
    assert!(i < MAX_FRAMES, "free_frame: out of range {:#x}", frame);
    let mut pmm = PMM.lock();
    assert!(
        pmm.bitmap[i / 8] & (1 << (i % 8)) != 0,
        "free_frame: double free {:#x}",
        frame
    );
    pmm.bitmap[i / 8] &= !(1 << (i % 8));
    pmm.free += 1;
}

pub fn stats() -> (u64, u64) {
    let pmm = PMM.lock();
    (pmm.free, pmm.total)
}
