//! Paging helpers + CPU memory protections.
//! SMEP (no kernel fetch of user pages) and SMAP (no kernel reads of user
//! pages without STAC) are enabled here when the CPU has them. User-pointer
//! reads must bracket with stac/clac — see syscall::sys_log.

use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::registers::control::Cr4;

use crate::kprintln;

const CR4_SMEP: u64 = 1 << 20;
const CR4_SMAP: u64 = 1 << 21;

static SMAP_ON: AtomicBool = AtomicBool::new(false);

/// True once SMAP is engaged: user reads need a STAC window.
pub fn smap_on() -> bool {
    SMAP_ON.load(Ordering::SeqCst)
}

pub fn init() {
    // CPUID.(7:0).EBX: bit 7 = SMEP, bit 20 = SMAP.
    let ebx = core::arch::x86_64::__cpuid_count(7, 0).ebx;
    let mut want = 0u64;
    if ebx & (1 << 7) != 0 {
        want |= CR4_SMEP;
    }
    if ebx & (1 << 20) != 0 {
        want |= CR4_SMAP;
    }
    if want != 0 {
        // SAFETY: only sets restriction bits; the kernel never executes user
        // pages, and all user data reads use stac/clac windows.
        unsafe {
            let cr4 = Cr4::read().bits();
            Cr4::write_raw(cr4 | want);
        }
    }
    SMAP_ON.store(want & CR4_SMAP != 0, Ordering::SeqCst);
    kprintln!(
        "[arch] protections: smep={} smap={}",
        want & CR4_SMEP != 0,
        want & CR4_SMAP != 0
    );
}
