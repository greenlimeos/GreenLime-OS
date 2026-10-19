//! virtio-net over the LEGACY PCI transport (port I/O, polling), mirroring
//! virtio-blk's queue math. One outstanding TX at a time (a single staging
//! frame, polled to completion); RX buffers stay stocked like the tablet's
//! event buffers. No offloads negotiated (plain Etherne frames); the
//! 10-byte virtio-net header is zeroed on TX and stripped on RX.
//!
//! smoltcp talks to this through mac()/mtu()/send_pkt()/recv_pkt().

use spin::Mutex;
use x86_64::instructions::port::Port;

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch::x86_64::{idt, lapic};
use crate::drivers::{pci, virtio_blk};
use crate::kprintln;
use crate::mm::pmm;

// Legacy register offsets from BAR0 (same map as virtio-blk).
const R_DEVICE_FEAT: u16 = 0x00;
const R_DRIVER_FEAT: u16 = 0x04;
const R_QUEUE_ADDR: u16 = 0x08; // PFN of the queue (selected queue)
const R_QUEUE_SIZE: u16 = 0x0C; // max size (read-only in legacy)
const R_QUEUE_SEL: u16 = 0x0E;
const R_QUEUE_NOTIFY: u16 = 0x10;
const R_STATUS: u16 = 0x12;
const R_ISR: u16 = 0x13;
const R_CONFIG: u16 = 0x14; // net config: mac[6] at +0 (MSI-X disabled)
// Legacy MSI-X vector config (only valid once MSI-X is enabled; the plain
// config space shifts to 0x18 then — MAC must be read before enabling).
const R_MSI_CONFIG_VEC: u16 = 0x14; // config-change interrupt vector (table entry index)
const R_MSI_QUEUE_VEC: u16 = 0x16; // queue interrupt vector for R_QUEUE_SEL

const ST_ACK: u8 = 1;
const ST_DRIVER: u8 = 2;
const ST_DRIVER_OK: u8 = 4;
const ST_FEATURES_OK: u8 = 8;

/// VIRTIO_NET_F_MAC: MAC field valid (always offered by QEMU).
const F_MAC: u32 = 1 << 5;

const DESC_WRITE: u16 = 2;

/// virtio-net header (no offloads: all zeros on TX, skipped on RX).
const HDR_LEN: usize = 10;
/// RX buffers posted per queue (each one PMM frame, 4 KiB).
const RX_BUFS: usize = 32;
/// Biggest frame we move (1514 + header), fits one 4 KiB buffer.
const MAX_FRAME: usize = 1526;
/// Device-side spin budget (false, never panic — GLS-010 applies here too).
const TIMEOUT: u32 = 1 << 24;

/// 16-byte descriptor. Naturally aligned, assigned whole.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// One legacy queue: descriptor table + avail ring, used page-aligned after.
struct Queue {
    size: u16,
    phys: u64,
    used_off: usize,
    last_used: u16,
}

pub struct VirtioNet {
    io: u16,
    hhdm: u64,
    mac: [u8; 6],
    rx: Queue,
    tx: Queue,
    /// RX buffer frames by descriptor id (phys addresses).
    rx_bufs: alloc::vec::Vec<u64>,
    /// TX staging frame (header + packet, single outstanding TX).
    tx_frame: u64,
}

impl VirtioNet {
    /// Probe + init the first legacy virtio-net (0x1AF4:0x1000). None if absent.
    pub fn probe(devs: &[pci::PciDev]) -> Option<Self> {
        let d = devs
            .iter()
            .find(|d| d.vendor == 0x1AF4 && d.device == 0x1000)?;
        let io = pci::bar0_iobase(d)?;
        let hhdm = crate::boot::hhdm_offset()?;
        unsafe {
            pci::enable_io_busmaster(d);
            Port::<u8>::new(io + R_STATUS).write(0); // reset
            Port::<u8>::new(io + R_STATUS).write(ST_ACK);
            Port::<u8>::new(io + R_STATUS).write(ST_ACK | ST_DRIVER);
            let dev_feat = Port::<u32>::new(io + R_DEVICE_FEAT).read();
            if dev_feat & F_MAC == 0 {
                kprintln!("[drivers] virtio-net: no MAC, giving up");
                return None;
            }
            Port::<u32>::new(io + R_DRIVER_FEAT).write(F_MAC);
            Port::<u8>::new(io + R_STATUS).write(ST_ACK | ST_DRIVER | ST_FEATURES_OK);
            if Port::<u8>::new(io + R_STATUS).read() & ST_FEATURES_OK == 0 {
                kprintln!("[drivers] virtio-net: features rejected");
                return None;
            }
            let mut mac = [0u8; 6];
            for (i, b) in mac.iter_mut().enumerate() {
                *b = Port::<u8>::new(io + R_CONFIG + i as u16).read();
            }

            // Queue 0 = RX, queue 1 = TX (control queue ignored).
            let rx = Self::setup_queue(io, hhdm, 0)?;
            let tx = Self::setup_queue(io, hhdm, 1)?;

            // RX buffers: one PMM frame per descriptor id, posted now.
            let mut rx_bufs = alloc::vec::Vec::with_capacity(rx.size as usize);
            for _ in 0..rx.size {
                rx_bufs.push(pmm::alloc_frame()?);
            }

            // TX staging frame (header + packet, reused serially).
            let tx_frame = pmm::alloc_frame()?;

            let mut net = Self {
                io,
                hhdm,
                mac,
                rx,
                tx,
                rx_bufs,
                tx_frame,
            };
            // Stock min(size, RX_BUFS) receive buffers.
            let n = (net.rx.size as usize).min(RX_BUFS);
            for i in 0..n as u16 {
                net.post_rx(i);
            }
            let _ = Port::<u8>::new(io + R_ISR).read(); // ack pending

            Port::<u8>::new(io + R_STATUS)
                .write(ST_ACK | ST_DRIVER | ST_FEATURES_OK | ST_DRIVER_OK);
            // Queue self-test: one TX round-trip (notify + used-ring) and
            // one empty RX drain. A zeroed 60-byte frame hits no real
            // peer (slirp drops it); failure here fails the probe loudly
            // instead of wedging smoltcp later.
            if !net.send_pkt(&[0u8; 60]) {
                kprintln!("[drivers] virtio-net: TX self-test failed");
                return None;
            }
            let mut scratch = [0u8; 64];
            let _ = net.recv_pkt(&mut scratch);

            // B3: IRQ-driven RX. MSI-X first: enable + program table entry 0
            // to our vector, then wire config + both queue vectors to entry
            // 0. The vec_ctrl dword write is load-bearing (see pci.rs —
            // narrower stores are rejected by the table region and leave
            // the entry masked). INTx fallback (via the q35 PIRQ router)
            // if MSI-X can't arm.
            let irq = if lapic::base() != 0 {
                for i in 0..8 {
                    super::register_irq_handler(idt::DEVICE_IRQ_BASE + i, net_irq_handler);
                }
                let mut armed = None;
                if pci::msix_cap(d).is_some() && pci::msix_enable(d) {
                    pci::enable_mmio_busmaster(d);
                    let dest = (lapic::self_id() as u64) << 12;
                    let ok = pci::msix_program_entry(
                        d,
                        0,
                        idt::DEVICE_IRQ_BASE,
                        0xFEE0_0000 | dest, // LAPIC MSI window, this CPU
                        idt::DEVICE_IRQ_BASE as u32, // fixed delivery, our vector
                    )
                    .is_some();
                    if ok {
                        // Wire the device's interrupt sources to entry 0.
                        let vec = 0u16;
                        Port::<u16>::new(io + R_MSI_CONFIG_VEC).write(vec);
                        for q in [0u16, 1u16] {
                            Port::<u16>::new(io + R_QUEUE_SEL).write(q);
                            Port::<u16>::new(io + R_MSI_QUEUE_VEC).write(vec);
                        }
                        // Post-arming proof: ctl must show Enable|TableSize,
                        // all three wired sources must read back as vector 0.
                        let ctl = pci::msix_msg_ctrl(d).unwrap_or(0);
                        Port::<u16>::new(io + R_QUEUE_SEL).write(0);
                        let cfg = Port::<u16>::new(io + R_MSI_CONFIG_VEC).read();
                        let q0 = Port::<u16>::new(io + R_MSI_QUEUE_VEC).read();
                        Port::<u16>::new(io + R_QUEUE_SEL).write(1);
                        let q1 = Port::<u16>::new(io + R_MSI_QUEUE_VEC).read();
                        kprintln!(
                            "[drivers] virtio-net: msix armed ctl={:#06x} vecs {:#06x}/{:#06x}/{:#06x}",
                            ctl,
                            cfg,
                            q0,
                            q1,
                        );
                        armed = Some(idt::DEVICE_IRQ_BASE);
                    } else {
                        pci::msix_disable(d);
                        kprintln!("[drivers] virtio-net: msix unarmable, INTx fallback");
                    }
                }
                // INTx fallback: q35 ICH9 routes 00:02.0 pin A through
                // PIRQ G to IOAPIC GSI 22: pirq = (slot + pin-1) % 4 + 4,
                // gsi = pirq + 16.
                if armed.is_none() {
                    if let Some(pin) = pci::irq_pin(d) {
                        let intx = pin - 1; // 0..3 for INTx A..D
                        let gsi = (d.dev + intx) % 4 + 4 + 16;
                        if crate::arch::x86_64::ioapic::set_irq(gsi, idt::DEVICE_IRQ_BASE) {
                            armed = Some(idt::DEVICE_IRQ_BASE);
                        }
                    }
                }
                armed
            } else {
                None
            };

            kprintln!(
                "[drivers] virtio-net: {:02x}:{:02x} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} rxq={} txq={} irq={:?}",
                d.bus,
                d.dev,
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
                net.rx.size,
                net.tx.size,
                irq,
            );
            super::register("virtio-net", irq);
            Some(net)
        }
    }

    /// Size + map one legacy queue (desc + avail, used page-aligned after).
    fn setup_queue(io: u16, hhdm: u64, sel: u16) -> Option<Queue> {
        unsafe {
            Port::<u16>::new(io + R_QUEUE_SEL).write(sel);
            let size = Port::<u16>::new(io + R_QUEUE_SIZE).read().max(1);
            let desc_avail = size as usize * 16 + 6 + size as usize * 2;
            let used_at = (desc_avail + 4095) & !4095;
            let used_len = 6 + size as usize * 8;
            let pages = (used_at + used_len + 4095) / 4096;
            let base = virtio_blk::alloc_contiguous(pages)?;
            core::ptr::write_bytes((base + hhdm) as *mut u8, 0, pages * 4096);
            Port::<u32>::new(io + R_QUEUE_ADDR).write((base / 4096) as u32);
            Some(Queue {
                size,
                phys: base,
                used_off: used_at,
                last_used: 0,
            })
        }
    }

    fn desc(&self, q: &Queue, i: u16) -> *mut Desc {
        (q.phys + self.hhdm + i as u64 * 16) as *mut Desc
    }

    fn avail_idx(&self, q: &Queue) -> *mut u16 {
        (q.phys + self.hhdm + q.size as u64 * 16 + 2) as *mut u16
    }

    fn avail_slot(&self, q: &Queue, i: u16) -> *mut u16 {
        (q.phys + self.hhdm + q.size as u64 * 16 + 4 + (i % q.size) as u64 * 2)
            as *mut u16
    }

    fn used_idx(&self, q: &Queue) -> *const u16 {
        (q.phys + self.hhdm + q.used_off as u64 + 2) as *const u16
    }

    fn used_entry(&self, q: &Queue, i: u16) -> (u32, u32) {
        // (descriptor id, written length).
        let p = (q.phys + self.hhdm + q.used_off as u64 + 4 + (i % q.size) as u64 * 8)
            as *const u32;
        unsafe { (core::ptr::read_volatile(p), core::ptr::read_volatile(p.add(1))) }
    }

    fn bump_avail(&self, qsel: u16, q: &Queue, desc_id: u16) {
        unsafe {
            let idx = core::ptr::read_volatile(self.avail_idx(q));
            core::ptr::write_volatile(self.avail_slot(q, idx), desc_id);
            core::ptr::write_volatile(self.avail_idx(q), idx.wrapping_add(1));
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            Port::<u16>::new(self.io + R_QUEUE_NOTIFY).write(qsel);
        }
    }

    /// Post one RX buffer (descriptor id == buffer index).
    fn post_rx(&self, id: u16) {
        let buf = self.rx_bufs[id as usize];
        unsafe {
            self.desc(&self.rx, id).write(Desc {
                addr: buf,
                len: 4096,
                flags: DESC_WRITE,
                next: 0,
            });
        }
        self.bump_avail(0, &self.rx, id);
    }

    /// Non-blocking drain: copy one completed frame (sans header) into
    /// `out`, repost the buffer. Always checks the used ring, so RX works
    /// whether or not an IRQ path is armed (its role is to wake the drain
    /// promptly, not to gate it). The device ISR is acked here, in task
    /// context under the NET lock — never in the interrupt handler.
    pub fn recv_pkt(&mut self, out: &mut [u8]) -> Option<usize> {
        let irq = IRQ_PENDING.swap(false, Ordering::AcqRel);
        let used = unsafe { core::ptr::read_volatile(self.used_idx(&self.rx)) };
        if used == self.rx.last_used {
            if irq {
                // IRQ without a used entry: config event or spurious. Ack
                // so the device can re-assert / send the next MSI.
                let _ = unsafe { Port::<u8>::new(self.io + R_ISR).read() };
            }
            return None;
        }
        let (id32, len) = self.used_entry(&self.rx, self.rx.last_used);
        self.rx.last_used = self.rx.last_used.wrapping_add(1);
        let total = len as usize;
        if id32 as usize >= self.rx_bufs.len() || total <= HDR_LEN || total > 4096 {
            // Bogus device report: drop (+ repost if the id is at least
            // in range), never trust blindly.
            if (id32 as usize) < self.rx_bufs.len() {
                self.post_rx(id32 as u16);
            }
            let _ = unsafe { Port::<u8>::new(self.io + R_ISR).read() };
            return None;
        }
        let id = id32 as u16;
        let n = (total - HDR_LEN).min(out.len()).min(MAX_FRAME);
        unsafe {
            core::ptr::copy_nonoverlapping(
                (self.rx_bufs[id as usize] + self.hhdm + HDR_LEN as u64) as *const u8,
                out.as_mut_ptr(),
                n,
            );
        }
        self.post_rx(id);
        let _ = unsafe { Port::<u8>::new(self.io + R_ISR).read() };
        RX_OK.fetch_add(1, Ordering::SeqCst);
        Some(n)
    }

    /// Send one frame (polled to completion). False on timeout.
    pub fn send_pkt(&mut self, pkt: &[u8]) -> bool {
        if pkt.len() > MAX_FRAME - HDR_LEN {
            return false;
        }
        unsafe {
            let dst = (self.tx_frame + self.hhdm) as *mut u8;
            core::ptr::write_bytes(dst, 0, HDR_LEN);
            core::ptr::copy_nonoverlapping(pkt.as_ptr(), dst.add(HDR_LEN), pkt.len());
            self.desc(&self.tx, 0).write(Desc {
                addr: self.tx_frame,
                len: (HDR_LEN + pkt.len()) as u32,
                flags: 0,
                next: 0,
            });
        }
        self.bump_avail(1, &self.tx, 0);
        let mut spins = 0u32;
        loop {
            let used = unsafe { core::ptr::read_volatile(self.used_idx(&self.tx)) };
            if used != self.tx.last_used {
                break;
            }
            spins += 1;
            if spins > TIMEOUT {
                return false;
            }
            core::hint::spin_loop();
        }
        self.tx.last_used = self.tx.last_used.wrapping_add(1);
        let _ = unsafe { Port::<u8>::new(self.io + R_ISR).read() };
        TX_OK.fetch_add(1, Ordering::SeqCst);
        true
    }

    /// Our link-layer address (see mac_addr() below for the shared path).
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }
}

static NET: Mutex<Option<VirtioNet>> = Mutex::new(None);

/// Lifetime TX completions / RX arrivals (link stats for bring-up proof).
static TX_OK: AtomicU64 = AtomicU64::new(0);
static RX_OK: AtomicU64 = AtomicU64::new(0);

/// Set by net_irq_handler (interrupt context — must not lock NET, the net
/// task could hold it on this CPU). recv_pkt runs in task context under the
/// NET lock, consumes the flag, and acks the device ISR there.
static IRQ_PENDING: AtomicBool = AtomicBool::new(false);
/// Device IRQs seen so far (first few are printed as serial proof).
static IRQ_COUNT: AtomicU64 = AtomicU64::new(0);

pub fn tx_count() -> u64 {
    TX_OK.load(Ordering::SeqCst)
}

pub fn rx_count() -> u64 {
    RX_OK.load(Ordering::SeqCst)
}

/// Claim the first NIC (own PCI scan, like the tablet). Called once from
/// net bring-up (Phase B).
pub fn init() -> bool {
    let devs = pci::scan();
    match VirtioNet::probe(&devs) {
        Some(n) => {
            *NET.lock() = Some(n);
            true
        }
        None => {
            kprintln!("[drivers] virtio-net: none (serial only)");
            false
        }
    }
}

/// MAC of the claimed NIC (smoltcp seeds its interface with this).
pub fn mac_addr() -> Option<[u8; 6]> {
    NET.lock().as_ref().map(|n| n.mac())
}

/// Queue one frame for transmit (single outstanding, polled). False when
/// no NIC or the device timed out.
pub fn send_pkt(pkt: &[u8]) -> bool {
    NET.lock()
        .as_mut()
        .map(|n| n.send_pkt(pkt))
        .unwrap_or(false)
}

/// Drain one received frame into `out`. None when idle (or no NIC).
pub fn recv_pkt(out: &mut [u8]) -> Option<usize> {
    NET.lock().as_mut().and_then(|n| n.recv_pkt(out))
}

/// IRQ dispatch entry for virtio-net (registered from probe; runs inside the
/// IDT device-IRQ handler with IF clear). Must NOT take the NET lock: the net
/// task could be holding it on this very CPU, and this handler would wait on
/// it forever. Flag + count only; recv_pkt acks the device ISR in task
/// context.
fn net_irq_handler() {
    IRQ_PENDING.store(true, Ordering::Release);
    let n = IRQ_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
    if n <= 8 {
        kprintln!("[drivers] virtio-net: irq #{}", n);
    }
}
