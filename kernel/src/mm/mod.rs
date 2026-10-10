//! Physical + virtual memory + kernel heap.
//! v0.2: PMM from Limine memmap, paging inspection, heap + self-test.

pub mod heap;
pub mod pmm;
pub mod vmm;

use crate::{boot, kprintln};

pub fn init() {
    let memmap = boot::memory_map().expect("[mm] Limine memory map missing");
    let hhdm = boot::hhdm_offset().expect("[mm] Limine HHDM missing");

    let (free, total) = pmm::init(memmap);
    kprintln!("[mm] pmm: {} free / {} total frames (4 KiB)", free, total);

    vmm::init(hhdm);
    // NOTE: heap::init runs earlier from kmain (fb console needs the heap).
    self_test(hhdm);
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
