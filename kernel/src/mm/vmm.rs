//! Virtual Memory Manager: inspects Limine-built paging, translates addresses.
//! Limine already runs us paged (higher half + HHDM). Full map/unmap and
//! per-process tables are here — they need the IDT so a bad
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

/// Walk the ACTIVE page tables to the leaf WITHOUT faulting and return its
/// flags. None = not mapped (any level unmapped). Read-only, never mutates.
/// Hardening gate: user pointers are validated HERE before any supervisor
/// access, so a garbage ring-3 pointer returns an error instead of paging
/// the kernel (GLS-003's old halt path, now impossible to reach).
pub fn leaf_flags(virt: u64) -> Option<PageTableFlags> {
    let hhdm = HHDM.load(Ordering::SeqCst);
    if hhdm == 0 {
        return None;
    }
    let (l4_frame, _) = Cr3::read();
    let mut table_phys = l4_frame.start_address().as_u64();
    // L4 -> L3 -> L2 only: intermediate levels may hold huge-page leaves,
    // but a 4 KiB leaf PTE must NEVER be descended into (its phys page is
    // data, not a table — dereferencing it faulted on MMIO-backed leaves
    // like the IOAPIC window in user CR3s).
    for shift in [39u64, 30, 21] {
        let idx = PageTableIndex::new(((virt >> shift) & 0x1FF) as u16);
        // SAFETY: table_phys is a live table reachable via HHDM (the walk
        // mirrors what the MMU itself would do; read-only).
        let table = unsafe { &*((table_phys + hhdm) as *const PageTable) };
        let entry = &table[idx];
        if entry.is_unused() {
            return None;
        }
        if entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            // 2 MiB / 1 GiB leaf: the walk stops early by protocol.
            return Some(entry.flags());
        }
        table_phys = entry.addr().as_u64();
    }
    // The level-1 entry itself is the leaf; read it, don't descend.
    // SAFETY: L1 table (level-1) at table_phys, HHDM-reachable.
    let table = unsafe { &*((table_phys + hhdm) as *const PageTable) };
    let entry = &table[PageTableIndex::new(((virt >> 12) & 0x1FF) as u16)];
    if entry.is_unused() {
        None
    } else {
        Some(entry.flags())
    }
}

/// User-range probe for the syscall gate: every page in [start, start+len)
/// must be mapped (present leaf) and — when `want_write` — user-accessible
/// and writable. Also rejects anything outside the user half. Call per
/// user pointer BEFORE the STAC window; a bad pointer is an error, never
/// a kernel-mode page fault.
pub fn user_range_ok(start: u64, len: u64, want_write: bool) -> bool {
    const USER_END: u64 = 0x0000_8000_0000_0000;
    if len == 0 || start == 0 {
        return false;
    }
    let end = if let Some(e) = start.checked_add(len) {
        e
    } else {
        return false;
    };
    if end > USER_END {
        return false;
    }
    let mut page = start & !0xFFF;
    while page < end {
        let Some(f) = leaf_flags(page) else {
            return false; // hole: unmapped page inside the range
        };
        if !f.contains(PageTableFlags::USER_ACCESSIBLE) {
            return false;
        }
        if want_write && !f.contains(PageTableFlags::WRITABLE) {
            return false;
        }
        page += 4096;
    }
    true
}

/// Boot-time W^X audit of the kernel's own mapping: text must be
/// executable and NOT writable; a data page must be writable and NX.
/// Limine maps per ELF segment permission; this verifies it stays true.
pub fn wx_audit(kernel_text: *const u8, kernel_data: *const u8) {
    let t = match leaf_flags(kernel_text as *const u8 as u64) {
        Some(f) => f,
        None => {
            kprintln!("[mm] wx: text page unmapped?!");
            return;
        }
    };
    let d = match leaf_flags(kernel_data as *const u8 as u64) {
        Some(f) => f,
        None => {
            kprintln!("[mm] wx: data page unmapped?!");
            return;
        }
    };
    let text_ok = t.contains(PageTableFlags::PRESENT)
        && !t.contains(PageTableFlags::WRITABLE)
        && !t.contains(PageTableFlags::NO_EXECUTE);
    let data_ok = d.contains(PageTableFlags::PRESENT)
        && d.contains(PageTableFlags::WRITABLE)
        && d.contains(PageTableFlags::NO_EXECUTE);
    kprintln!(
        "[mm] wx: kernel text R+X w={} | data RW+NX x={}",
        !t.contains(PageTableFlags::WRITABLE),
        d.contains(PageTableFlags::NO_EXECUTE)
    );
    if !text_ok || !data_ok {
        kprintln!("[mm] wx: VIOLATION — kernel image pages are not W^X)");
    }
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
        // Zero the whole table (a count of 1 here once shipped live and
        // left 511 garbage entries — PMM frames are NOT pre-zeroed).
        core::ptr::write_bytes(dst as *mut u8, 0, 4096);
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
    let hhdm = HHDM.load(Ordering::SeqCst);
    if hhdm == 0 {
        return None;
    }
    let (l4_frame, _) = Cr3::read();
    map_mmio_page_in(l4_frame.start_address().as_u64(), hhdm, phys)
}

/// Map one 4 KiB MMIO page into an ARBITRARY top-level table (identity).
/// Used for the kernel tables at boot and for every new user table
/// (shared kernel MMIO like the LAPIC must exist in all of them —
/// an AP timer tick under a user cr3 writes EOI, and a missing leaf
/// there is a supervisor fault that kills the CPU).
/// Existing entries with the same address get their flags REPAIRED
/// (a bootloader may have mapped the page read-only).
///
/// SAFETY: pml4_phys must be a live table reachable via HHDM; the caller
/// owns it (boot PML4, or a fresh user PML4 under construction).
pub unsafe fn map_mmio_page_in(pml4_phys: u64, hhdm: u64, phys: u64) -> Option<u64> {
    debug_assert!(phys % 4096 == 0);
    let mut table_phys = pml4_phys;
    for shift in [39u64, 30, 21, 12] {
        let idx = PageTableIndex::new(((phys >> shift) & 0x1FF) as u16);
        // SAFETY: table_phys is a live mapped table (HHDM); intermediates
        // are created here, and the caller guarantees exclusive access
        // (boot CPU, or a user table no CPU has loaded yet).
        let table = unsafe { &mut *((table_phys + hhdm) as *mut PageTable) };
        if shift == 12 {
            let virt = x86_64::VirtAddr::new(phys);
            let want = PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::NO_EXECUTE
                | PageTableFlags::NO_CACHE;
            if table[idx].is_unused() {
                table[idx].set_addr(PhysAddr::new(phys), want);
                x86_64::instructions::tlb::flush(virt);
            } else if table[idx].addr().as_u64() == phys {
                // Same page, possibly wrong flags (bootloader-mapped):
                // repair in place.
                table[idx].set_addr(PhysAddr::new(phys), want);
            } else {
                return None; // live mapping elsewhere: don't clobber it
            }
            return Some(phys);
        }
        if table[idx].is_unused() {
            let frame = pmm::alloc_frame()?;
            // SAFETY: fresh PMM frame, accessed once via HHDM.
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
