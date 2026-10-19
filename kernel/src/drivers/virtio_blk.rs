//! VirtIO-blk over legacy PCI transport (port I/O, polling).
//! Legacy is simple and QEMU still provides it (transitional devices).
//! One queue, one outstanding request, no interrupts yet (poll used.idx).
//! Buffers are single PMM frames (below 4 GiB, DMA-safe addressing).

use spin::Mutex;
use x86_64::instructions::port::Port;

use crate::drivers::pci;
use crate::drivers::Driver;
use crate::kprintln;
use crate::mm::pmm;

// Legacy register offsets from BAR0.
const R_DEVICE_FEAT: u16 = 0x00;
const R_DRIVER_FEAT: u16 = 0x04;
const R_QUEUE_ADDR: u16 = 0x08; // PFN of the queue (selected queue)
const R_QUEUE_SIZE: u16 = 0x0C; // max size (read-only in legacy)
const R_QUEUE_SEL: u16 = 0x0E;
const R_QUEUE_NOTIFY: u16 = 0x10;
const R_STATUS: u16 = 0x12;
const R_ISR: u16 = 0x13;
const R_CONFIG: u16 = 0x14; // blk config: capacity u64 at +0

const ST_ACK: u8 = 1;
const ST_DRIVER: u8 = 2;
const ST_DRIVER_OK: u8 = 4;
const ST_FEATURES_OK: u8 = 8;

const BLK_T_IN: u32 = 0; // read
const BLK_S_OK: u8 = 0;

const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;

const SECTOR: usize = 512;
const TIMEOUT: u32 = 1 << 24;

/// 16-byte descriptor. Naturally aligned (8+4+2+2), assigned whole.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

pub struct VirtioBlk {
    io: u16,
    queue_size: u16,
    /// Phys base of the queue area (desc table + avail + used).
    queue_phys: u64,
    /// Byte offset of the used ring from queue_phys.
    used_offset: usize,
    hhdm: u64,
    last_used: u16,
    capacity_sectors: u64,
    blk_size: u32,
}

impl VirtioBlk {
    /// Probe + init the first legacy virtio-blk (0x1AF4:0x1001). None if absent.
    pub fn probe(devs: &[pci::PciDev]) -> Option<Self> {
        let d = devs
            .iter()
            .find(|d| d.vendor == 0x1AF4 && d.device == 0x1001)?;
        let io = pci::bar0_iobase(d)?;
        let hhdm = crate::boot::hhdm_offset()?;
        unsafe {
            pci::enable_io_busmaster(d);
            Port::<u8>::new(io + R_STATUS).write(0); // reset
            Port::<u8>::new(io + R_STATUS).write(ST_ACK);
            Port::<u8>::new(io + R_STATUS).write(ST_ACK | ST_DRIVER);
            let _dev_feat = Port::<u32>::new(io + R_DEVICE_FEAT).read();
            Port::<u32>::new(io + R_DRIVER_FEAT).write(0); // plain reads
            Port::<u8>::new(io + R_STATUS).write(ST_ACK | ST_DRIVER | ST_FEATURES_OK);
            if Port::<u8>::new(io + R_STATUS).read() & ST_FEATURES_OK == 0 {
                kprintln!("[drivers] virtio-blk: features rejected");
                return None;
            }

            // Queue 0 sizing: desc table + avail, used page-aligned after.
            Port::<u16>::new(io + R_QUEUE_SEL).write(0);
            let max = Port::<u16>::new(io + R_QUEUE_SIZE).read().max(1);
            let desc_avail = max as usize * 16 + 6 + max as usize * 2;
            let used_at = (desc_avail + 4095) & !4095;
            let used_len = 6 + max as usize * 8;
            let pages = (used_at + used_len + 4095) / 4096;
            let base = alloc_contiguous(pages)?;
            core::ptr::write_bytes((base + hhdm) as *mut u8, 0, pages * 4096);
            Port::<u32>::new(io + R_QUEUE_ADDR).write((base / 4096) as u32);

            // Block config: capacity (sectors) + block size.
            let cap_lo = Port::<u32>::new(io + R_CONFIG).read() as u64;
            let cap_hi = Port::<u32>::new(io + R_CONFIG + 4).read() as u64;
            let blk_size = Port::<u32>::new(io + R_CONFIG + 20).read();

            Port::<u8>::new(io + R_STATUS)
                .write(ST_ACK | ST_DRIVER | ST_FEATURES_OK | ST_DRIVER_OK);
            let _ = Port::<u8>::new(io + R_ISR).read(); // ack pending

            kprintln!(
                "[drivers] virtio-blk: {:02x}:{:02x} queue={} cap={} sectors blk={}",
                d.bus,
                d.dev,
                max,
                cap_lo | (cap_hi << 32),
                if blk_size == 0 { 512 } else { blk_size }
            );
            Some(Self {
                io,
                queue_size: max,
                queue_phys: base,
                used_offset: used_at,
                hhdm,
                last_used: 0,
                capacity_sectors: cap_lo | (cap_hi << 32),
                blk_size: if blk_size == 0 { 512 } else { blk_size },
            })
        }
    }

    /// Virtual pointer to descriptor i (CPU writes; device reads via phys).
    fn desc(&self, i: usize) -> *mut Desc {
        (self.queue_phys + self.hhdm + i as u64 * 16) as *mut Desc
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

    /// Read one 512-byte sector (polls used ring). False on timeout or
    /// device error (GLS-010: these used to panic, so a bad disk image
    /// halted the machine; a corrupt disk is now just a failed read).
    /// The bounce frame is freed on every path.
    pub fn read_block(&mut self, lba: u64, out: &mut [u8; SECTOR]) -> bool {
        assert!(self.blk_size == 512, "[drivers] virtio-blk: blk_size != 512");
        // One bounce frame: [header 16][data 512][status 1].
        let frame = pmm::alloc_frame().expect("[drivers] virtio-blk: no bounce frame");
        let hhdm = self.hhdm;
        let hdr = frame;
        let data = frame + 16;
        let status = frame + 16 + 512;
        unsafe {
            let w32 = |p: u64, v: u32| core::ptr::write_volatile((p + hhdm) as *mut u32, v);
            let w64 = |p: u64, v: u64| core::ptr::write_volatile((p + hhdm) as *mut u64, v);
            let w8 = |p: u64, v: u8| core::ptr::write_volatile((p + hhdm) as *mut u8, v);
            w32(hdr, BLK_T_IN);
            w32(hdr + 4, 0);
            w64(hdr + 8, lba);
            w8(status, 0xFF);

            // SAFETY: descriptors live in our zeroed queue pages; written
            // before the avail bump, consumed by the device after notify.
            self.desc(0).write(Desc { addr: hdr, len: 16, flags: DESC_NEXT, next: 1 });
            self.desc(1).write(Desc {
                addr: data,
                len: 512,
                flags: DESC_NEXT | DESC_WRITE,
                next: 2,
            });
            self.desc(2).write(Desc { addr: status, len: 1, flags: DESC_WRITE, next: 0 });

            let idx = core::ptr::read_volatile(self.avail_idx_ptr());
            core::ptr::write_volatile(
                self.avail_ring_ptr().add((idx % self.queue_size) as usize),
                0,
            );
            core::ptr::write_volatile(self.avail_idx_ptr(), idx.wrapping_add(1));
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

            Port::<u16>::new(self.io + R_QUEUE_NOTIFY).write(0);
            let _ = Port::<u8>::new(self.io + R_ISR).read();

            let mut spins = 0u32;
            loop {
                if core::ptr::read_volatile(self.used_idx_ptr()) != self.last_used {
                    break;
                }
                spins += 1;
                if spins > TIMEOUT {
                    pmm::free_frame(frame);
                    return false;
                }
                core::hint::spin_loop();
            }
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            self.last_used = self.last_used.wrapping_add(1);
            let st = core::ptr::read_volatile((status + hhdm) as *const u8);
            if st != BLK_S_OK {
                pmm::free_frame(frame);
                return false;
            }
            core::ptr::copy_nonoverlapping(
                (data + hhdm) as *const u8,
                out.as_mut_ptr(),
                SECTOR,
            );
        }
        pmm::free_frame(frame);
        true
    }
}

/// Allocate `n` physically contiguous frames (retry loop; early-boot RAM is
/// mostly contiguous, so this terminates fast in practice).
pub(crate) fn alloc_contiguous(n: usize) -> Option<u64> {
    for _ in 0..64 {
        let mut v = alloc::vec::Vec::with_capacity(n);
        let mut ok = true;
        for _ in 0..n {
            match pmm::alloc_frame() {
                Some(f) => v.push(f),
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && (0..n).all(|i| v[i] == v[0] + i as u64 * 4096) {
            return Some(v[0]);
        }
        for f in v {
            pmm::free_frame(f);
        }
    }
    None
}

impl Driver for VirtioBlk {
    fn name(&self) -> &'static str {
        "virtio-blk"
    }
    // Polling for now: no IRQ path yet (handle_irq default no-op).
}

static BLK: Mutex<Option<VirtioBlk>> = Mutex::new(None);

/// Claim the first disk. Called once from storage init.
pub fn init(devs: &[pci::PciDev]) -> bool {
    match VirtioBlk::probe(devs) {
        Some(b) => {
            *BLK.lock() = Some(b);
            // Driver model: legacy virtio-blk, polled until B3 wires IRQs.
            super::register("virtio-blk", None);
            true
        }
        None => {
            kprintln!("[drivers] virtio-blk: no disk (boot with disk.img)");
            false
        }
    }
}

/// Read LBA into a 512-byte buffer. False if no disk / bad lba.
pub fn read_block(lba: u64, out: &mut [u8; SECTOR]) -> bool {
    match BLK.lock().as_mut() {
        Some(b) => {
            if lba >= b.capacity_sectors {
                return false;
            }
            b.read_block(lba, out)
        }
        None => false,
    }
}
