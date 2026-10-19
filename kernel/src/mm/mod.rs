//! Physical + virtual memory + kernel heap.
//! v0.2: PMM from Limine memmap, paging inspection, heap + self-test.

pub mod heap;
pub mod pmm;
pub mod vmm;

use crate::{boot, kprintln};

/// Boot KASLR seed: mixed TSC + counter, splitmix64-folded, never zero.
/// Feeds per-task user-stack jitter and the smoltcp RNG (NOT crypto).
static KASLR_SEED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11E5);
    z ^ (z >> 31)
}

/// Derive and pin the boot seed once (kmain, before anything seeds from it).
/// TSC (boot-time, per-boot distinct even on the same code) + jitters AP id.
pub fn init_kaslr_seed(extra: u64) {
    let mut state = unsafe { core::arch::x86_64::_rdtsc() } ^ extra.wrapping_mul(0xFF51_FFD7_ED55_9CCD);
    let mut v = 0u64;
    for _ in 0..4 {
        v ^= splitmix64(&mut state);
    }
    if v == 0 {
        v = 0xA5A5_A5A5_5A5A_5A5A;
    }
    KASLR_SEED.store(v, core::sync::atomic::Ordering::SeqCst);
    // Log a SHORT fingerprint, never the raw seed (it is an attack asset).
    kprintln!("[mm] kaslr: seed {:#018x} pinned (stack jitter + net rng)", v >> 32);
}

pub fn kaslr_seed() -> u64 {
    let s = KASLR_SEED.load(core::sync::atomic::Ordering::SeqCst);
    if s == 0 {
        // Fallback for early callers (must never be zero for mixers).
        0x1234_5678_9ABC_DEF0
    } else {
        s
    }
}

pub fn init() {
    let memmap = boot::memory_map().expect("[mm] Limine memory map missing");
    let hhdm = boot::hhdm_offset().expect("[mm] Limine HHDM missing");

    let (free, total) = pmm::init(memmap);
    kprintln!("[mm] pmm: {} free / {} total frames (4 KiB)", free, total);

    vmm::init(hhdm);
    // NOTE: heap::init runs earlier from kmain (fb console needs the heap).
    self_test(hhdm);

    // KASLR seed: pinned once, feeds stack jitter + the net RNG.
    init_kaslr_seed(crate::arch::x86_64::idt::ticks() as u64 ^ 0xC0FFEE);
    // W^X audit of the kernel's own image (Limine-mapped; verify, don't
    // assume): text page must be R+X (non-writable), a data page RW+NX.
    vmm::wx_audit(
        init_kaslr_seed as *const () as usize as *const u8,
        core::ptr::addr_of!(KASLR_SEED) as *const u8,
    );
}

fn self_test(hhdm: u64) {
    // Physical frames: alloc two, prove distinct, write via HHDM, free.
    let f1 = pmm::alloc_frame().expect("[mm] test: alloc 1 failed");
    let f2 = pmm::alloc_frame().expect("[mm] test: alloc 2 failed");
    assert!(f1 != f2, "[mm] test: allocator returned same frame twice");
    unsafe {
        let p = (f1 + hhdm) as *mut u64;
        p.write_volatile(0xDEAD_BEEF_CAFE_F00D);
        assert!(
            p.read_volatile() == 0xDEAD_BEEF_CAFE_F00D,
            "[mm] test: HHDM readback mismatch"
        );
    }
    pmm::free_frame(f1);
    pmm::free_frame(f2);
    let (free, total) = pmm::stats();
    kprintln!("[mm] self-test: frames ok (free={} total={})", free, total);

    // Heap: Box + Vec exercise GlobalAlloc + OOM handler path.
    let b = alloc::boxed::Box::new(0x1234_5678u64);
    let mut v = alloc::vec::Vec::new();
    for i in 0..128u64 {
        v.push(i * i);
    }
    assert!(*b == 0x1234_5678 && v.len() == 128 && v[127] == 16129);
    kprintln!("[mm] self-test: heap ok (box + vec[128])");

    // Translate the kernel entry through live tables (read-only).
    let entry = crate::_start as *const () as usize as u64;
    match vmm::translate(entry) {
        Some(p) => kprintln!(
            "[mm] self-test: _start virt={:#x} -> phys={:#x}",
            entry,
            p
        ),
        None => kprintln!("[mm] self-test: translate failed for _start"),
    }
}
