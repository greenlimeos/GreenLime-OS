//! virtio-input tablet over the MODERN virtio-pci transport (MMIO
//! capability regions, polled). QEMU's virtio-input is a post-1.0 device
//! with no legacy interface at all — the legacy IO path simply doesn't
//! exist for it (`disable-legacy`/`disable-modern` are no-ops) — so we
//! walk the PCI capability list for the common/notify/device-cfg regions.
//!
//! The device reports the HOST pointer (0..32767 normalized), which is
//! what lets the guest cursor track the real one exactly: the relative
//! PS/2 mouse has no shared coordinate system.
//!
//! One queue: eventq = virtqueue 0 (QEMU: evt added first, size 64).
//! Buffers stay stocked: QEMU's virtio_input_send drops an entire report
//! when it can't pop a buffer at SYN_REPORT time.

use spin::Mutex;

use crate::boot;
use crate::drivers::pci::{self, PciDev};
use crate::kprintln;
use crate::mm::{pmm, vmm};

// PCI capability: vendor-specific header carrying a virtio region.
const CAP_VNDR: u8 = 0x09;
const CAP_COMMON: u8 = 1;
const CAP_NOTIFY: u8 = 2;
const CAP_DEVICE: u8 = 4;

// Modern common-cfg register offsets (virtio 1.1 §4.1.4.4). Typed u64 so
// they add straight onto region base addresses.
const C_DRIVER_FEATURE_SEL: u64 = 0x08;
const C_DRIVER_FEATURE: u64 = 0x0C;
const C_NUM_QUEUES: u64 = 0x12;
const C_STATUS: u64 = 0x14;
const C_QUEUE_SELECT: u64 = 0x16;
const C_QUEUE_SIZE: u64 = 0x18;
const C_QUEUE_ENABLE: u64 = 0x1C;
const C_QUEUE_NOTIFY_OFF: u64 = 0x1E;
const C_QUEUE_DESC: u64 = 0x20;
const C_QUEUE_AVAIL: u64 = 0x28;
const C_QUEUE_USED: u64 = 0x30;

const ST_ACK: u8 = 1;
const ST_DRIVER: u8 = 2;
const ST_DRIVER_OK: u8 = 4;
const ST_FEATURES_OK: u8 = 8;
/// VIRTIO_F_VERSION_1 = feature bit 32 -> word 1, bit 0 (mandatory).
const F_VERSION_1_WORD1: u32 = 1;

const DESC_WRITE: u16 = 2;

/// virtio-input config select for axis ranges (virtio 1.1 / Linux).
const CFG_ABS_INFO: u8 = 0x12;
/// linux/input-event-codes.h.
const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;

/// Normalized position range the driver reports outward.
const NORM: i64 = 32767;

// --- MMIO access (addresses pre-verified by map_region) -------------------

unsafe fn mmio8(a: u64) -> u8 {
    unsafe { core::ptr::read_volatile(a as *const u8) }
}

unsafe fn mmio16(a: u64) -> u16 {
    unsafe { core::ptr::read_volatile(a as *const u16) }
}

unsafe fn mmio32(a: u64) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

unsafe fn w8(a: u64, v: u8) {
    unsafe { core::ptr::write_volatile(a as *mut u8, v) }
}

unsafe fn w16(a: u64, v: u16) {
    unsafe { core::ptr::write_volatile(a as *mut u16, v) }
}

unsafe fn w32(a: u64, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

/// 16-byte descriptor. Naturally aligned, assigned whole.
#[repr(C)]
#[derive(Clone, Copy)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

#[derive(Clone, Copy)]
struct AbsRange {
    min: i32,
    max: i32,
}

pub struct Tablet {
    notify: u64,      // virt: notify region base
    notify_mult: u32, // notify_off_multiplier
    notify_off: u16,  // our queue's notify offset
    queue_phys: u64, // queue area in RAM (accessed via HHDM)
    hhdm: u64,
    queue_size: u16,
    /// Byte offset of the used ring from queue_phys.
    used_offset: usize,
    last_used: u16,
    /// One PMM frame holds every 8-byte event buffer (buf_phys + i*8).
    buf_phys: u64,
    abs: [AbsRange; 2],
    /// Latest normalized position + button mask (bit0 = left).
    cur: [u16; 2],
    btn: u8,
    /// ABS/KEY arrived but no SYN_REPORT yet (QEMU batches per report).
    pending: bool,
}

/// Resolve a BAR-relative physical address to an accessible virtual one.
/// Limine maps the framebuffer for us but not general MMIO — fall back to
/// an explicit identity mapping of the page (boot-time, single CPU).
fn map_region(name: &str, phys: u64) -> Option<u64> {
    let hhdm = boot::hhdm_offset()?;
    let page = phys & !0xFFF;
    let off = phys & 0xFFF;
    if vmm::translate(page) == Some(page) {
        return Some(phys);
    }
    if vmm::translate(page + hhdm) == Some(page) {
        return Some(page + hhdm + off);
    }
    // SAFETY: boot-time, interrupts off, single CPU; verified unmapped.
    if let Some(v) = unsafe { vmm::map_mmio_page(page) } {
        return Some(v + off);
    }
    kprintln!(
        "[drivers] virtio-tablet: {} phys={:#x} unmapped (id={:?} hhdm={:?})",
        name,
        phys,
        vmm::translate(page),
        vmm::translate(page + hhdm)
    );
    None
}

/// Physical base of a BAR (32/64-bit memory BARs only). None if I/O/empty.
unsafe fn bar_phys(d: &PciDev, bar: u8) -> Option<u64> {
    let reg = 0x10u8 + bar * 4;
    let lo = pci::cfg_read_u32(d, reg);
    if lo == 0xFFFF_FFFF || lo & 1 != 0 {
        return None;
    }
    match (lo >> 1) & 3 {
        2 | 3 => {
            // 64-bit BAR (type 2 = non-prefetchable, 3 = prefetchable):
            // the next register holds the high half.
            let hi = pci::cfg_read_u32(d, reg + 4);
            Some(((hi as u64) << 32) | ((lo & 0xFFFFFFF0) as u64))
        }
        _ => Some((lo & 0xFFFFFFF0) as u64),
    }
}

impl Tablet {
    /// Probe + init the modern virtio-tablet (0x1AF4:0x1052). None when
    /// absent, capabilities missing, or a region can't be mapped.
    fn probe(devs: &[PciDev]) -> Option<Self> {
        let d = devs.iter().find(|d| d.vendor == 0x1AF4 && d.device == 0x1052)?;

        // Walk the PCI capability list for virtio region descriptors:
        // vendor cap -> {cfg_type, bar, offset within bar}; the notify cap
        // additionally carries notify_off_multiplier at cap+16.
        let mut caps = [(0u8, 0u8, 0u32); 5];
        let mut n = 0usize;
        let mut notify_mult: u32 = 0;
        let mut cap = unsafe { pci::cfg_read_u8(d, 0x34) };
        for _ in 0..32 {
            if cap == 0 || cap < 0x40 || cap > 0xF0 {
                break;
            }
            let id = unsafe { pci::cfg_read_u8(d, cap) };
            let next = unsafe { pci::cfg_read_u8(d, cap + 1) };
            // QEMU 10 layout: {vndr, next, cap_len, cfg_type, bar, id, pad[2],
            // offset@8, length@12}; notify cap adds mult@16.
            let ty = unsafe { pci::cfg_read_u8(d, cap + 3) };
            let bar = unsafe { pci::cfg_read_u8(d, cap + 4) };
            let off = unsafe { pci::cfg_read_u32(d, cap + 8) };
            if id == CAP_VNDR && n < 5 {
                if ty == CAP_NOTIFY {
                    notify_mult = unsafe { pci::cfg_read_u32(d, cap + 16) };
                }
                if matches!(ty, CAP_COMMON | CAP_NOTIFY | CAP_DEVICE) {
                    caps[n] = (ty, bar, off);
                    n += 1;
                }
            }
            cap = next;
        }
        let phys_of = |ty: u8| -> Option<u64> {
            let &(_, bar, off) = caps[..n].iter().find(|c| c.0 == ty)?;
            Some(unsafe { bar_phys(d, bar) }? + off as u64)
        };
        let (c_p, n_p, d_p) = (
            phys_of(CAP_COMMON),
            phys_of(CAP_NOTIFY),
            phys_of(CAP_DEVICE),
        );
        if c_p.is_none() || n_p.is_none() || d_p.is_none() {
            kprintln!(
                "[drivers] virtio-tablet: caps={} mult={} common={:?} notify={:?} device={:?}",
                n,
                notify_mult,
                c_p,
                n_p,
                d_p
            );
            return None;
        }

        let common = map_region("common", c_p?)?;
        let notify = map_region("notify", n_p?)?;
        let dev_cfg = map_region("device-cfg", d_p?)?;

        let hhdm = boot::hhdm_offset()?;
        unsafe {
            pci::enable_mmio_busmaster(d);
            // Reset + acknowledge (modern status sequence).
            w8(common + C_STATUS, 0);
            w8(common + C_STATUS, ST_ACK);
            w8(common + C_STATUS, ST_ACK | ST_DRIVER);
            // Features: accept nothing optional in word 0, VERSION_1 in
            // word 1 (mandatory for modern devices).
            w32(common + C_DRIVER_FEATURE_SEL, 0);
            w32(common + C_DRIVER_FEATURE, 0);
            w32(common + C_DRIVER_FEATURE_SEL, 1);
            w32(common + C_DRIVER_FEATURE, F_VERSION_1_WORD1);
            w8(common + C_STATUS, ST_ACK | ST_DRIVER | ST_FEATURES_OK);
            if mmio8(common + C_STATUS) & ST_FEATURES_OK == 0 {
                kprintln!("[drivers] virtio-tablet: features rejected");
                return None;
            }
            if mmio16(common + C_NUM_QUEUES) < 1 {
                kprintln!("[drivers] virtio-tablet: no queues");
                return None;
            }

            // Event queue = virtqueue 0: desc + avail, used aligned.
            w16(common + C_QUEUE_SELECT, 0);
            let max = mmio16(common + C_QUEUE_SIZE).max(2);
            let size = max.min(64); // power of two either way
            w16(common + C_QUEUE_SIZE, size);
            let desc_avail = size as usize * 16 + 6 + size as usize * 2;
            let used_at = (desc_avail + 4095) & !4095;
            let used_len = 6 + size as usize * 8;
            let pages = (used_at + used_len + 4095) / 4096;
            let base = super::virtio_blk::alloc_contiguous(pages)?;
            core::ptr::write_bytes((base + hhdm) as *mut u8, 0, pages * 4096);

            // Event buffers: one frame, zeroed (garbage must never parse
            // as plausible events).
            let buf = pmm::alloc_frame()?;
            core::ptr::write_bytes((buf + hhdm) as *mut u8, 0, 4096);
            // Stock the avail ring with ALL slots — the device drops whole
            // reports when it can't pop (virtio_input_send).
            let nbuf = (size as usize).min(4096 / 8);
            let ring = (base + hhdm + size as u64 * 16 + 4) as *mut u16;
            for i in 0..nbuf {
                let desc = (base + hhdm + i as u64 * 16) as *mut Desc;
                desc.write(Desc {
                    addr: buf + i as u64 * 8,
                    len: 8,
                    flags: DESC_WRITE,
                    next: 0,
                });
                *ring.add(i) = i as u16;
            }
            core::ptr::write_volatile(
                (base + hhdm + size as u64 * 16 + 2) as *mut u16,
                nbuf as u16,
            );
            w64(common + C_QUEUE_DESC, base);
            w64(common + C_QUEUE_AVAIL, base + size as u64 * 16);
            w64(common + C_QUEUE_USED, base + used_at as u64);
            w16(common + C_QUEUE_ENABLE, 1);
            let notify_off = mmio16(common + C_QUEUE_NOTIFY_OFF);

            w8(
                common + C_STATUS,
                ST_ACK | ST_DRIVER | ST_FEATURES_OK | ST_DRIVER_OK,
            );

            let abs = [
                Self::abs_range(dev_cfg, ABS_X).unwrap_or(AbsRange {
                    min: 0,
                    max: NORM as i32,
                }),
                Self::abs_range(dev_cfg, ABS_Y).unwrap_or(AbsRange {
                    min: 0,
                    max: NORM as i32,
                }),
            ];
            kprintln!(
                "[drivers] virtio-tablet: evt queue={} bufs={} x={}..{} y={}..{}",
                size,
                nbuf,
                abs[0].min,
                abs[0].max,
                abs[1].min,
                abs[1].max
            );
            Some(Self {
                notify,
                notify_mult,
                notify_off,
                queue_phys: base,
                hhdm,
                queue_size: size,
                used_offset: used_at,
                last_used: 0,
                buf_phys: buf,
                abs,
                cur: [(NORM / 2) as u16, (NORM / 2) as u16],
                btn: 0,
                pending: false,
            })
        }
    }

    /// Query an axis range from device config space (virtio_input_abs_info
    /// at config offset 8). None when the axis isn't registered.
    unsafe fn abs_range(dev_cfg: u64, axis: u16) -> Option<AbsRange> {
        unsafe {
            w8(dev_cfg, CFG_ABS_INFO);
            w8(dev_cfg + 1, axis as u8);
            let size = mmio8(dev_cfg + 2);
            if size < 16 {
                return None;
            }
            let min = mmio32(dev_cfg + 8) as i32;
            let max = mmio32(dev_cfg + 12) as i32;
            (max > min).then_some(AbsRange { min, max })
        }
    }

    fn avail_idx_ptr(&self) -> *mut u16 {
        (self.queue_phys + self.hhdm + self.queue_size as u64 * 16 + 2) as *mut u16
    }

    fn avail_ring_ptr(&self) -> *mut u16 {
        (self.queue_phys + self.hhdm + self.queue_size as u64 * 16 + 4) as *mut u16
    }

    fn used_idx_ptr(&self) -> *const u16 {
        (self.queue_phys + self.hhdm + self.used_offset as u64 + 2) as *const u16
    }

    /// Map a raw axis value into 0..32767.
    fn norm(&self, axis: usize, v: i32) -> u16 {
        let r = self.abs[axis];
        let span = (r.max - r.min) as i64;
        let n = ((v - r.min) as i64 * NORM / span.max(1)).clamp(0, NORM);
        n as u16
    }

    /// Fold one event into the report-in-progress. True on SYN_REPORT with
    /// something to report.
    fn handle(&mut self, etype: u16, ecode: u16, eval: i32) -> bool {
        match etype {
            EV_ABS => {
                match ecode {
                    ABS_X => {
                        self.cur[0] = self.norm(0, eval);
                        self.pending = true;
                    }
                    ABS_Y => {
                        self.cur[1] = self.norm(1, eval);
                        self.pending = true;
                    }
                    _ => {}
                }
                false
            }
            EV_KEY => {
                let bit = match ecode {
                    BTN_LEFT => Some(0u8),
                    BTN_RIGHT => Some(1),
                    BTN_MIDDLE => Some(2),
                    _ => None,
                };
                if let Some(b) = bit {
                    if eval != 0 {
                        self.btn |= 1 << b;
                    } else {
                        self.btn &= !(1 << b);
                    }
                    self.pending = true;
                }
                false
            }
            EV_SYN if ecode == SYN_REPORT => {
                let ready = self.pending;
                self.pending = false;
                ready
            }
            _ => false,
        }
    }
}

fn w64(a: u64, v: u64) {
    unsafe { core::ptr::write_volatile(a as *mut u64, v) }
}

static TABLET: Mutex<Option<Tablet>> = Mutex::new(None);

/// Probe + claim the tablet. Called after mm: needs PMM frames.
pub fn init() {
    let devs = pci::scan();
    match Tablet::probe(&devs) {
        Some(t) => *TABLET.lock() = Some(t),
        None => kprintln!("[drivers] virtio-tablet: none (PS/2 mouse only)"),
    }
}

/// True when a tablet is driving the pointer (winsrv then ignores PS/2
/// mouse bytes: QEMU may feed both devices).
pub fn available() -> bool {
    TABLET.lock().is_some()
}

/// True when the device delivered buffers we haven't drained yet. Cheap
/// RAM read — feeds the window server's idle fast-path (tablet events
/// never touch the PS/2 rings).
pub fn events_pending() -> bool {
    let guard = TABLET.lock();
    let Some(t) = guard.as_ref() else {
        return false;
    };
    unsafe { core::ptr::read_volatile(t.used_idx_ptr()) != t.last_used }
}

/// Drain queued events. Some((x, y, buttons)) when a complete report
/// (SYN_REPORT) landed since the last call. Normalized: x/y in 0..32767,
/// buttons bit0 = left, bit1 = right, bit2 = middle.
pub fn poll() -> Option<(u16, u16, u8)> {
    let mut guard = TABLET.lock();
    let t = guard.as_mut()?;
    let mut report = false;
    let mut consumed = false;
    unsafe {
        let used_idx = core::ptr::read_volatile(t.used_idx_ptr());
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        while t.last_used != used_idx {
            let slot = (t.last_used % t.queue_size) as usize;
            let elem = t.queue_phys + t.hhdm + t.used_offset as u64 + 4 + slot as u64 * 8;
            let id = core::ptr::read_volatile(elem as *const u32) as usize;
            let ev = t.buf_phys + t.hhdm + id as u64 * 8;
            let etype = core::ptr::read_volatile(ev as *const u16);
            let ecode = core::ptr::read_volatile((ev + 2) as *const u16);
            let eval = core::ptr::read_volatile((ev + 4) as *const u32) as i32;
            report |= t.handle(etype, ecode, eval);
            // Recycle immediately: a starved avail ring = dropped reports.
            let idx = core::ptr::read_volatile(t.avail_idx_ptr());
            core::ptr::write_volatile(
                t.avail_ring_ptr().add(idx as usize % t.queue_size as usize),
                id as u16,
            );
            core::ptr::write_volatile(t.avail_idx_ptr(), idx.wrapping_add(1));
            t.last_used = t.last_used.wrapping_add(1);
            consumed = true;
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        if consumed {
            // Spec: notify after avail updates (QEMU's handler is a no-op
            // for evt, but other devices may care).
            w16(
                t.notify + t.notify_off as u64 * t.notify_mult as u64,
                0, // queue index
            );
        }
    }
    if report {
        Some((t.cur[0], t.cur[1], t.btn))
    } else {
        None
    }
}
