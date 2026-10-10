//! Virtual Memory Manager: inspects Limine-built paging, translates addresses.
//! Limine already runs us paged (higher half + HHDM). Full map/unmap and
//! per-process tables land in v0.4 — that needs the IDT (v0.3) so a bad
//! mapping reports a page fault instead of triple-faulting QEMU.

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::structures::paging::mapper::Translate;
use x86_64::structures::paging::{OffsetPageTable, PageTable, PageTableFlags, PageTableIndex};
use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::{Cr3, Cr4},
};

use crate::{kprintln, mm::pmm};

static HHDM: AtomicU64 = AtomicU64::new(0);

pub fn init(hhdm: u64) {
    HHDM.store(hhdm, Ordering::SeqCst);
    let (l4_frame, _) = Cr3::read();
    // CR4.LA57 is bit 12; read raw so we don't depend on flag names.
    let level = if Cr4::read().bits() & (1 << 12) != 0 {
        5
    } else {
        4
    };
    kprintln!(
        "[mm] paging: cr3={:#x} level={} hhdm={:#x}",
        l4_frame.start_address().as_u64(),
        level,
        hhdm
    );
}

/// Translate a virtual address through the ACTIVE page tables. Read-only,
/// cannot fault (returns None for unmapped).
pub fn translate(virt: u64) -> Option<u64> {
    let hhdm = HHDM.load(Ordering::SeqCst);
    if hhdm == 0 {
        return None;
    }
    let (l4_frame, _) = Cr3::read();
    // SAFETY: Limine guarantees the HHDM maps all usable RAM, and CR3 points
    // to a valid level-4 table. We only read through this mapping.
    let l4_table = unsafe {
        &mut *((l4_frame.start_address().as_u64() + hhdm) as *mut PageTable)
    };
    let mapper = unsafe { OffsetPageTable::new(l4_table, VirtAddr::new(hhdm)) };
    mapper
        .translate_addr(VirtAddr::new(virt))
        .map(|a| a.as_u64())
}

/// Build a fresh user PML4: zeroed lower half, higher half cloned from the
/// kernel (kernel + HHDM mappings shared, still supervisor-only).
/// Returns the PHYSICAL address of the new top-level table.
pub fn new_user_table(hhdm: u64) -> u64 {
    let frame = pmm::alloc_frame().expect("[vmm] out of frames for user PML4");
    // SAFETY: fresh PMM frame, accessed once via HHDM on the boot CPU.
    // Higher-half entries (kernel + HHDM) are shared supervisor-only.
    unsafe {
        let dst = (frame + hhdm) as *mut PageTable;
        let (cur, _) = Cr3::read();
        let src = (cur.start_address().as_u64() + hhdm) as *const PageTable;
        core::ptr::write_bytes(dst, 0, 1); // zero all 512 entries
        let dst_table: &mut PageTable = &mut *dst;
        let src_table: &PageTable = &*src;
        for i in 256..512 {
            let idx = PageTableIndex::new(i);
            dst_table[idx] = src_table[idx].clone();
        }
    }
    frame
}

/// Map one 4 KiB MMIO page into the ACTIVE kernel page tables at its
/// physical address (identity). Returns the virtual address, or None if a
/// huge page is in the way (splitting isn't implemented — none expected).
/// Boot-time only, single CPU (callers run before interrupts/scheduler).
pub unsafe fn map_mmio_page(phys: u64) -> Option<u64> {
    debug_assert!(phys % 4096 == 0);
    let hhdm = HHDM.load(Ordering::SeqCst);
    if hhdm == 0 {
        return None;
    }
    let (l4_frame, _) = Cr3::read();
    let mut table_phys = l4_frame.start_address().as_u64();
    for shift in [39u64, 30, 21, 12] {
        let idx = PageTableIndex::new(((phys >> shift) & 0x1FF) as u16);
        // SAFETY: table_phys is a live mapped table (HHDM); boot CPU, IF off.
        let table = unsafe { &mut *((table_phys + hhdm) as *mut PageTable) };
        if shift == 12 {
            let virt = x86_64::VirtAddr::new(phys);
            if table[idx].is_unused() {
                table[idx].set_addr(
                    PhysAddr::new(phys),
                    PageTableFlags::PRESENT
                        | PageTableFlags::WRITABLE
                        | PageTableFlags::NO_EXECUTE
                        | PageTableFlags::NO_CACHE,
                );
                x86_64::instructions::tlb::flush(virt);
            } else if table[idx].addr().as_u64() != phys {
                return None; // live mapping elsewhere: don't clobber it
            }
            return Some(phys);
        }
        if table[idx].is_unused() {
            let frame = pmm::alloc_frame()?;
            // SAFETY: fresh PMM frame, accessed once via HHDM on boot CPU.
            unsafe { core::ptr::write_bytes((frame + hhdm) as *mut u8, 0, 4096) };
            table[idx].set_addr(
                PhysAddr::new(frame),
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
            );
        } else if table[idx]
            .flags()
            .contains(PageTableFlags::HUGE_PAGE)
        {
            return None; // 1G/2M leaf mid-walk: would need a split
        }
        table_phys = table[idx].addr().as_u64();
    }
    None
}

/// Map one 4 KiB page into a user table, creating intermediates as needed.
/// All tables and the leaf get USER_ACCESSIBLE; caller picks W/X via flags.
pub unsafe fn map_user_page(
    pml4_phys: u64,
    hhdm: u64,
    virt: u64,
    phys: u64,
    writable: bool,
    executable: bool,
) {
    debug_assert!(virt % 4096 == 0 && phys % 4096 == 0);
    let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if writable {
        flags |= PageTableFlags::WRITABLE;
    }
    if !executable {
        flags |= PageTableFlags::NO_EXECUTE;
    }

    let mut table_phys = pml4_phys;
    // Walk P4 -> P3 -> P2, creating tables; then set the P1 leaf.
    for shift in [39, 30, 21, 12] {
        let idx = PageTableIndex::new(((virt >> shift) & 0x1FF) as u16);
        // SAFETY: table_phys is a live mapped table (HHDM); single boot CPU.
        let table = unsafe { &mut *((table_phys + hhdm) as *mut PageTable) };
        if shift == 12 {
            table[idx].set_addr(PhysAddr::new(phys), flags);
            return;
        }
        if table[idx].is_unused() {
            let frame = pmm::alloc_frame().expect("[vmm] out of frames for tables");
            unsafe { core::ptr::write_bytes((frame + hhdm) as *mut u8, 0, 4096) };
            table[idx].set_addr(
                PhysAddr::new(frame),
                PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::USER_ACCESSIBLE,
            );
        }
        table_phys = table[idx].addr().as_u64();
    }
    unreachable!("map_user_page loop fell through");
}
