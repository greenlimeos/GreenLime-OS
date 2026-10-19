//! Limine boot protocol glue. ONLY file that knows Limine.
//! Everything else uses `boot_info()` data, never Limine types.

use limine::request::{
    FramebufferRequest, HhdmRequest, MemoryMapRequest, MpRequest, StackSizeRequest,
};
use limine::response::{HhdmResponse, MemoryMapResponse, MpResponse};
use limine::BaseRevision;

use crate::kprintln;

// Require Limine protocol revision >= 2 (v0.5 crate supports up to latest).
#[used]
#[link_section = ".requests"]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[link_section = ".requests"]
static STACK_REQUEST: StackSizeRequest = StackSizeRequest::new().with_size(0x100000);

#[used]
#[link_section = ".requests"]
static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();

#[used]
#[link_section = ".requests"]
static MEMORY_MAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();

#[used]
#[link_section = ".requests"]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[used]
#[link_section = ".requests"]
static MP_REQUEST: MpRequest = MpRequest::new();

/// Validate bootloader answered with a supported revision.
pub fn init() {
    assert!(
        BASE_REVISION.is_supported(),
        "Limine base revision not supported"
    );
}

/// Borrow the Limine memory map for the PMM. Valid for kernel lifetime.
pub fn memory_map() -> Option<&'static MemoryMapResponse> {
    MEMORY_MAP_REQUEST.get_response()
}

/// HHDM slide for phys<->virt conversion (used by PMM self-test + VMM).
pub fn hhdm_offset() -> Option<u64> {
    HHDM_REQUEST.get_response().map(HhdmResponse::offset)
}

/// MP (SMP) response: CPU list for AP bring-up. Valid for kernel lifetime
/// (bootloader memory is never handed to the PMM).
pub fn smp_response() -> Option<&'static MpResponse> {
    MP_REQUEST.get_response()
}

/// Raw framebuffer geometry for the fb console. The address Limine reports
/// is directly writable (it maps the mode itself).
pub struct FramebufferInfo {
    pub address: u64,
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
    pub bpp: u16,
}

pub fn framebuffer() -> Option<FramebufferInfo> {
    let fb = FRAMEBUFFER_REQUEST
        .get_response()?
        .framebuffers()
        .next()?;
    Some(FramebufferInfo {
        address: fb.addr() as u64,
        width: fb.width(),
        height: fb.height(),
        pitch: fb.pitch(),
        bpp: fb.bpp(),
    })
}

/// Print framebuffer + memory map summary. Called after drivers::init.
pub fn boot_info() {
    if let Some(resp) = FRAMEBUFFER_REQUEST.get_response() {
        let count = resp.framebuffers().count();
        kprintln!("[boot] framebuffers: {}", count);
        if let Some(fb) = resp.framebuffers().next() {
            kprintln!(
                "[boot] fb0: {}x{} pitch={} bpp={}",
                fb.width(),
                fb.height(),
                fb.pitch(),
                fb.bpp()
            );
        }
    } else {
        kprintln!("[boot] no framebuffer (serial only)");
    }

    if let Some(resp) = MEMORY_MAP_REQUEST.get_response() {
        kprintln!("[boot] memmap entries: {}", resp.entries().len());
        let usable: u64 = resp
            .entries()
            .iter()
            .filter(|e| e.entry_type == limine::memory_map::EntryType::USABLE)
            .map(|e| e.length)
            .sum();
        kprintln!("[boot] usable RAM: {} MiB", usable / 1024 / 1024);
        // v0.2: hand `resp` to mm::pmm::init_from_limine().
    } else {
        kprintln!("[boot] no memory map!");
    }

    if let Some(resp) = HHDM_REQUEST.get_response() {
        kprintln!("[boot] hhdm offset: {:#x}", resp.offset());
    }
}
