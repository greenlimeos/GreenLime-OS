//! AHCI (SATA) block device driver — the bare-metal unlock (M19).
//!
//! PCI class 01/06 covers AHCI and Intel RST/RAID mode (both expose the
//! standard ABAR via BAR5), so real hardware — i5-4460-era PCH included —
//! is claimable. Registers are MMIO (map_mmio_page); the engine is POLLED
//! like virtio-blk (no IRQ path yet). Only 512-byte sectors, one
//! outstanding command, LBA48 via READ/WRITE DMA EXT (0x25/0x35).
//!
//! THE SAFETY LAW: mounting stays Fat32's call. `Fat32::open()` reads LBA
//! 0 and refuses anything without a valid BPB, so attaching a real
//! Windows/NTFS/empty-GPT disk boots to "vfs: ... (no disk)" and no write
//! can touch a filesystem this OS cannot parse. A genuinely-FAT32 disk
//! mounts and behaves exactly like the virtio test image.
//!
//! THE QEMU-NEUTRALITY LAW: q35 always ships an ICH9 SATA controller with
//! NO attached drives — virtio is this project's test disk. The probe is
//! read-first: it prints NOTHING and touches no engine state when every
//! implemented port is DET=0, and drops out to virtio otherwise, leaving
//! every existing QEMU run's trajectory untouched.
//!
//! Waits ride the 100 Hz tick grid (idt::ticks), not raw spin counts — a
//! spin cap big enough for real PHY settle would burn tens of TCG seconds
//! on the emulator (and did). Disk bring-up therefore costs ~ms here.
//!
//! Per-port memory: one PMM frame (DMA-safe < 4 GiB), laid out:
//!   [0..1024)     command list = 32 x 32-byte headers (header 0 only used)
//!   [1024..1280)  received FIS area (256 B, spec)
//!   [1280..1408)  command table = 64 B CFIS + 16 B ACMD + 48 B reserved
//!   [1408..1424)  one PRDT entry (512 B transfer)
//! Data itself travels through a per-op bounce frame like virtio.

use spin::Mutex;

use crate::arch::x86_64::idt;
use crate::drivers::Driver;
use crate::kprintln;
use crate::mm::pmm;

// ----- wait budgets (ms; resolved against the 100 Hz tick grid) -----
const WAIT_LINK_MS: u64 = 2000; // PHY settle / COMRESET response
const WAIT_SIG_MS: u64 = 2000; // D2H signature latch
const WAIT_RESET_MS: u64 = 1000; // engine onboarding (FR/CR)
const WAIT_IO_MS: u64 = 5000; // one 512 B DMA command

/// Now + ms, in 10 ms ticks.
fn deadline_ms(ms: u64) -> u64 {
    idt::ticks() + ms / 10
}
fn is_past(deadline: u64) -> bool {
    idt::ticks() >= deadline
}

// ----- HBA registers (ABAR-relative) -----
const R_GHC: u32 = 0x04;
const R_PI: u32 = 0x0C; // ports implemented bitmap
const GHC_AE: u32 = 1 << 31; // AHCI enable (must precede any port access)

const PORT_BASE: u32 = 0x100;
const PORT_STRIDE: u32 = 0x80;

// ----- Port registers (port-base-relative) -----
const P_CLB: u32 = 0x00; // command list base, 1024-aligned
const P_CLBU: u32 = 0x04;
const P_FB: u32 = 0x08; // FIS receive base, 256-aligned
const P_FBU: u32 = 0x0C;
const P_IS: u32 = 0x10; // interrupt status (write-1-to-clear)
const P_CMD: u32 = 0x18;
const P_TFD: u32 = 0x20;
const P_SIG: u32 = 0x24;
const P_SSTS: u32 = 0x28;
const P_SCTL: u32 = 0x2C;
const P_CI: u32 = 0x38; // command issued (write 1 to fire, HW clears)

const CMD_ST: u32 = 1 << 0; // command engine start
const CMD_FRE: u32 = 1 << 4; // FIS receive enable
const CMD_FR: u32 = 1 << 14; // FIS receive running
const CMD_CR: u32 = 1 << 15; // command list running

// ----- The one command we build -----
const CFIS_H2D: u8 = 0x27;
const FIS_C: u32 = 0x80; // byte1: command FIS flag
const CMD_READ_DMA_EXT: u8 = 0x25;
const CMD_WRITE_DMA_EXT: u8 = 0x35;
const CMD_IDENTIFY: u8 = 0xEC;
const SIG_ATA: u32 = 0x0000_0101; // plain SATA disk (ATAPI/others: no)
const SIG_TRANSIENT: u32 = 0xFFFF_FFFF; // link mid-retrain

const SECTOR: usize = 512;
/// Sectors per command: ONE (single-PRDT law).
const SECTORS_PER_CMD: u64 = 1;

fn sig_name(sig: u32) -> &'static str {
    match sig {
        0xEB14_0101 => "ATAPI (CD) - not supported",
        0x9669_0101 => "port multiplier - not supported",
        0xC33C_0101 => "enclosure management - not supported",
        _ => "unknown",
    }
}

/// Volatile MMIO primitives (x86 strongly orders; volatile keeps the
/// compiler from reordering across every port-register touch).
unsafe fn mmio_r32(addr: u64) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}
unsafe fn mmio_w32(addr: u64, v: u32) {
    unsafe { core::ptr::write_volatile(addr as *mut u32, v) }
}

/// Busy-wait until `mask` bits are clear, bounded by a tick deadline.
fn wait_clear(addr: u64, mask: u32, deadline: u64) -> bool {
    loop {
        if unsafe { mmio_r32(addr) } & mask == 0 {
            return true;
        }
        if is_past(deadline) {
            return false;
        }
        core::hint::spin_loop();
    }
}

/// Busy-wait until `mask` bits are set, bounded by a tick deadline.
fn wait_set(addr: u64, mask: u32, deadline: u64) -> bool {
    loop {
        if unsafe { mmio_r32(addr) } & mask == mask {
            return true;
        }
        if is_past(deadline) {
            return false;
        }
        core::hint::spin_loop();
    }
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

pub struct Ahci {
    /// Virtual ABAR (registers mapped single-page; HBA is a 4 KiB region).
    hba: u64,
    hhdm: u64,
    /// The chosen SATA port.
    port: u8,
    /// Phys of the port page (list + rx FIS + table, one frame).
    mem: u64,
    capacity_sectors: u64,
}

impl Ahci {
    /// Virtual port register base.
    fn port(&self) -> u64 {
        self.hba + (PORT_BASE + self.port as u32 * PORT_STRIDE) as u64
    }

    /// Probe. Returns the device only when a real SATA disk answers.
    /// Silent (no log lines, no hardware writes beyond GHC.AE) when the
    /// controller has no device on any implemented port.
    pub fn probe(devs: &[crate::drivers::pci::PciDev]) -> Option<Self> {
        let d = devs
            .iter()
            .find(|d| d.class == 0x01 && d.subclass == 0x06)?;
        let hhdm = crate::boot::hhdm_offset()?;

        // ---- BAR5 (the ABAR) ----
        let bar5_raw = unsafe { crate::drivers::pci::cfg_read_u32(d, 0x24) };
        if bar5_raw & 1 != 0 {
            return None; // I/O-space BAR: legacy IDE, no AHCI here
        }
        let mut phys = (bar5_raw & !0xF) as u64;
        if (bar5_raw & 0x6) == 0x4 {
            // 64-bit MMIO BAR: upper half in the next config dword.
            phys |= (unsafe { crate::drivers::pci::cfg_read_u32(d, 0x28) } as u64) << 32;
        }
        if phys == 0 {
            return None; // BAR never programmed — nothing to probe
        }
        unsafe { crate::drivers::pci::enable_mmio_busmaster(d) };
        let base = phys & !0xFFF;
        let off = (phys & 0xFFF) as u64;
        let hba = unsafe { crate::mm::vmm::map_mmio_page(base)? + off };

        unsafe {
            // AE first: the spec voids port-register behavior until it's set.
            mmio_w32(hba + R_GHC as u64, GHC_AE);

            // ---- Read-only census ----
            let pi = mmio_r32(hba + R_PI as u64);
            if pi == 0 {
                return None;
            }
            let mut det_of = [0u32; 32];
            for p in 0..32u8 {
                if pi & (1 << p) == 0 {
                    continue;
                }
                let port = hba + (PORT_BASE + p as u32 * PORT_STRIDE) as u64;
                let det = mmio_r32(port + P_SSTS as u64) & 0xF;
                det_of[p as usize] = det;
                if det == 0 {
                    continue; // truly empty port: not even a print
                }
                let sig = mmio_r32(port + P_SIG as u64);
                kprintln!(
                    "[drivers] ahci: port {} det={:#x} sig={:#010x} (census)",
                    p,
                    det,
                    sig
                );
            }

            // ---- COMRESET pass over every port showing a link. This is
            //      the takeover move, not just cold bring-up: firmware
            //      (SeaBIOS ABI runtime INT 13h on q35; PC setup firmware
            //      likewise) leaves the port owned, and the device must
            //      re-signal itself to us. (QEMU's AHCI model re-inits ITS
            //      per-port IDE housekeeping on exactly this SCTL edge -
            //      without it a stale firmware transfer counter silently
            //      ate our first 512-byte transfers.)
            let mut chosen: Option<u8> = None;
            for p in 0..32u8 {
                if pi & (1 << p) == 0 || det_of[p as usize] == 0 {
                    continue;
                }
                let port = hba + (PORT_BASE + p as u32 * PORT_STRIDE) as u64;
                // COMRESET: hold DET=1 to force the PHY to re-establish.
                mmio_w32(port + P_SCTL as u64, 1);
                let mut spins = 0u64;
                while spins < 200_000 {
                    core::hint::spin_loop();
                    spins += 1;
                }
                mmio_w32(port + P_SCTL as u64, 0);
                if !wait_set(port + P_SSTS as u64, 0x3, deadline_ms(WAIT_LINK_MS)) {
                    kprintln!("[drivers] ahci: port {}: no link after COMRESET", p);
                    continue;
                }
                // Signature latches in the D2H FIS at link-up:
                // transient 0xFFFF_FFFF until the device answers.
                let sig_d = deadline_ms(WAIT_SIG_MS);
                let sig = loop {
                    let s = mmio_r32(port + P_SIG as u64);
                    if s != SIG_TRANSIENT {
                        break s;
                    }
                    if is_past(sig_d) {
                        break s;
                    }
                    core::hint::spin_loop();
                };
                if sig == SIG_ATA {
                    kprintln!("[drivers] ahci: port {}: SATA device after COMRESET", p);
                    chosen = Some(p);
                    break;
                }
                kprintln!(
                    "[drivers] ahci: port {}: {}",
                    p,
                    sig_name(sig)
                );
            }
            let p = chosen?;
            let port = hba + (PORT_BASE + p as u32 * PORT_STRIDE) as u64;

            // ---- Port engine onboarding ----
            let mut a = Self {
                hba,
                hhdm,
                port: p,
                mem: 0,
                capacity_sectors: 0,
            };
            let port = port;

            // ---- Takeover: bring a firmware-started engine to a FULL stop.
            //      BIOS/UEFI-era drivers (SeaBIOS runtime INT 13h on q35;
            //      your PC's setup firmware likewise) leave FRE|ST running
            //      with THEIR list/FIS pages latched. Per spec PxCLB/PxFB
            //      may change only while the engine is stopped, and the
            //      HBA re-latches them on the FRE/ST 0->1 edges (QEMU
            //      re-maps on exactly those edges — our earlier RMW kept
            //      both bits set, made no edge, and QEMU kept executing
            //      SeaBIOS's stale header: start_dma + a 32 KiB parse of
            //      its table, no data to us). Stop both halves, wait the
            //      running-bits clear, THEN program, THEN re-arm.
            let c0 = mmio_r32(port + P_CMD as u64);
            kprintln!("[drivers] ahci: port {}: engine takeover (cmd {:#010x})", p, c0);
            if c0 & CMD_ST != 0 {
                mmio_w32(port + P_CMD as u64, c0 & !CMD_ST);
                if !wait_clear(port + P_CMD as u64, CMD_CR, deadline_ms(WAIT_RESET_MS)) {
                    kprintln!("[drivers] ahci: port {}: engine stop timed out", p);
                    return None;
                }
            }
            let c1 = mmio_r32(port + P_CMD as u64);
            if c1 & CMD_FRE != 0 {
                mmio_w32(port + P_CMD as u64, c1 & !CMD_FRE);
                if !wait_clear(port + P_CMD as u64, CMD_FR, deadline_ms(WAIT_RESET_MS)) {
                    kprintln!("[drivers] ahci: port {}: FIS stop timed out", p);
                    return None;
                }
            }

            // Port memory: ONE frame (list + rx FIS + one cmd table).
            let mem = pmm::alloc_frame()?; // phys < 4 GiB (PMM law)
            let memv = (mem + hhdm) as *mut u8;
            core::ptr::write_bytes(memv, 0, 4096);
            // Command list base, 1024-aligned: page offset 0.
            mmio_w32(port + P_CLB as u64, (mem & 0xFFFF_FFFF) as u32);
            mmio_w32(port + P_CLBU as u64, (mem >> 32) as u32);
            // FIS receive base, 256-aligned: page offset 1024.
            let fis_phys = mem + 1024;
            mmio_w32(port + P_FB as u64, (fis_phys & 0xFFFF_FFFF) as u32);
            mmio_w32(port + P_FBU as u64, (fis_phys >> 32) as u32);
            // Ack stale interrupt-status/link bits from firmware INT13 era.
            mmio_w32(port + P_IS as u64, 0xFFFF_FFFF);

            // Re-arm on TRUE edges now: FIS receive first (0->1), then the
            // command engine (0->1). CR=1 while the engine runs - the
            // "quiet" pre-issue check lives in issue() (TFD busy + CI),
            // not CR.
            mmio_w32(port + P_CMD as u64, mmio_r32(port + P_CMD as u64) | CMD_FRE);
            if !wait_set(port + P_CMD as u64, CMD_FR, deadline_ms(WAIT_RESET_MS)) {
                kprintln!("[drivers] ahci: port {}: FR never set", p);
                pmm::free_frame(mem);
                return None;
            }
            mmio_w32(port + P_CMD as u64, mmio_r32(port + P_CMD as u64) | CMD_ST);
            if !wait_set(port + P_CMD as u64, CMD_CR, deadline_ms(WAIT_RESET_MS)) {
                kprintln!("[drivers] ahci: port {}: engine never started", p);
                pmm::free_frame(mem);
                return None;
            }
            a.mem = mem;

            // ---- IDENTIFY: proves the engine end-to-end AND gives the
            //      capacity before we register as "the disk" ----
            let frame = pmm::alloc_frame()?;
            let ok = a.issue(CMD_IDENTIFY, 0, frame, WAIT_IO_MS);
            let mut cap: u64 = 0;
            if ok {
                let id: &[u8] =
                    core::slice::from_raw_parts((frame + hhdm) as *const u8, SECTOR);
                let lba48 = (le16(id, 166) >> 10) & 1 == 1; // word 83, bit 10
                cap = if lba48 {
                    (le32(id, 200) as u64) | ((le16(id, 204) as u64) << 32)
                } else {
                    le32(id, 120) as u64 // words 60/61
                };
                if cap == 0 {
                    // Concise failure line: capacity stayed zero means the
                    // command chain worked but the data did not. (The word
                    // 83/100 dumps and the table echo that pinned this during
                    // bring-up are gone; if a real disc ever hits this,
                    // -trace ahci* shows the device's side.)
                    kprintln!(
                        "[drivers] ahci: port {}: identify data incomplete (w83={:#04x} w100={:#010x})",
                        p,
                        le16(id, 166),
                        le32(id, 200)
                    );
                }
            }
            pmm::free_frame(frame); // bounce frames free on EVERY path
            if !ok {
                kprintln!("[drivers] ahci: port {}: identify command error", p);
                pmm::free_frame(mem);
                return None;
            }
            if cap == 0 {
                kprintln!("[drivers] ahci: port {}: identify ok but no capacity", p);
                pmm::free_frame(mem);
                return None;
            }
            a.capacity_sectors = cap;
            kprintln!(
                "[drivers] ahci: disk {} sectors ({} MiB) on port {}",
                cap,
                (cap * 512) / (1024 * 1024),
                p
            );
            Some(a)
        }
    }

    /// ONE command through header-0/PRDT-0, polled to completion.
    ///
    /// `cmd` is EXT-set; `lba` is the (u48) sector; `frame` the bounce
    /// page; `budget_ms` the tick-deadline cap. False on a stuck BSY, a
    /// timeout (engine stop→restart so the NEXT op starts quiet), or an
    /// ATA error flag (TFD.ERR).
    unsafe fn issue(&mut self, cmd: u8, lba: u64, frame: u64, budget_ms: u64) -> bool {
        let hhdm = self.hhdm;
        let port = self.port();
        unsafe {
            // Only issue from a quiet taskfile (BSY|DRQ = mid-command).
            if !wait_clear(port + P_TFD as u64, 0x88, deadline_ms(WAIT_RESET_MS)) {
                kprintln!("[drivers] ahci: taskfile stuck busy");
                return false;
            }

            // 1. Header 0: CFL=5 (H2D FIS), direction, 1 PRDT, and the
            //    COMMAND TABLE base (CTBA) — DW2/DW3 point at the table
            //    (CFIS+PRDT), while the DATA frame goes into the PRDT
            //    entry below. (First bring-up pointed CTBA at the bounce
            //    frame instead: the device parsed an all-zero CFIS,
            //    QEMU swallowed it as "unknown FIS type" and completed
            //    instantly — completion without data, exactly.)
            let list = (self.mem + hhdm) as *mut u32;
            let dir = if cmd == CMD_WRITE_DMA_EXT { 1 << 6 } else { 0 };
            list.write_volatile(5 | dir | (1 << 16)); // PRDTL = 1
            list.add(1).write_volatile(0); // task-file flags (none)
            list.add(2).write_volatile(((self.mem + 1280) & 0xFFFF_FFFF) as u32);
            list.add(3).write_volatile(((self.mem + 1280) >> 32) as u32);
            list.add(4).write_volatile(0);
            list.add(5).write_volatile(0);

            // 2. The register FIS (64 B at table offset 1280).
            let fis = (self.mem + hhdm + 1280) as *mut u32;
            fis.write_volatile(CFIS_H2D as u32 | FIS_C << 8 | (cmd as u32) << 16);
            fis.add(1).write_volatile(
                ((lba & 0xFF) as u32)
                    | (((lba >> 8) & 0xFF) as u32) << 8
                    | (((lba >> 16) & 0xFF) as u32) << 16
                    | 0x40 << 24, // device: LBA mode
            );
            fis.add(2).write_volatile(
                (((lba >> 24) & 0xFF) as u32)
                    | (((lba >> 32) & 0xFF) as u32) << 8 // features hi = 0
                    | (((lba >> 40) & 0xFF) as u32) << 24,
            );
            fis.add(3).write_volatile(SECTORS_PER_CMD as u32); // count lo=1
            fis.add(4).write_volatile(0); // ICC
            fis.add(5).write_volatile(0); // control

            // 3. One PRDT entry at table offset 1408: the whole 512 B.
            //    Entry layout (AHCI 4.2 / libata struct ahci_sg):
            //    dwords 0-1 = DBA, dword 2 = RESERVED (not the count!),
            //    dword 3 = DBC (byte count minus 1) | IIF(bit 31).
            //   We first wrote DBC at dword 2 and the HBA read the
            //    reserved dword instead - a 1-byte transfer of byte 0,
            //    then silence.
            //    (The DATA frame lives here, NOT in the header - see the
            //    CTBA note above.)
            let prdt = (self.mem + hhdm + 1408) as *mut u32;
            prdt.write_volatile((frame & 0xFFFF_FFFF) as u32);
            prdt.add(1).write_volatile((frame >> 32) as u32);
            prdt.add(2).write_volatile(0); // reserved
            prdt.add(3).write_volatile((SECTOR as u32) - 1); // DBC = 511

            // 4. Fire slot 0 (descriptor fence before the register write,
            //    matching the virtio avail-bump law).
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            mmio_w32(port + P_CI as u64, 1);

            // 5. Poll the issued bit clearing (no IRQ path yet).
            if !wait_clear(port + P_CI as u64, 1, deadline_ms(budget_ms)) {
                // Engine wedged: the spec stop/start dance so the NEXT op
                // begins from a genuinely idle port.
                let c = mmio_r32(port + P_CMD as u64);
                mmio_w32(port + P_CMD as u64, c & !CMD_ST);
                let _ = wait_clear(port + P_CMD as u64, CMD_CR, deadline_ms(WAIT_RESET_MS));
                mmio_w32(port + P_CMD as u64, c | CMD_ST);
                let _ = wait_set(port + P_CMD as u64, CMD_CR, deadline_ms(WAIT_RESET_MS));
                kprintln!("[drivers] ahci: command timeout on port {}", self.port);
                return false;
            }

            // 6. Device verdict = ATA status in the taskfile (ERR = bit 0).
            let st = mmio_r32(port + P_TFD as u64) & 0xFF;
            // Ack the interrupt-status bits this command raised.
            mmio_w32(port + P_IS as u64, mmio_r32(port + P_IS as u64));
            st & 0x01 == 0
        }
    }

    /// Read one 512-byte sector. The bounce frame is freed on EVERY path.
    pub fn read_block(&mut self, lba: u64, out: &mut [u8; SECTOR]) -> bool {
        let frame = match pmm::alloc_frame() {
            Some(f) => f,
            None => {
                kprintln!("[drivers] ahci: no bounce frame");
                return false;
            }
        };
        let ok = unsafe { self.issue(CMD_READ_DMA_EXT, lba, frame, WAIT_IO_MS) };
        if ok {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    (frame + self.hhdm) as *const u8,
                    out.as_mut_ptr(),
                    SECTOR,
                );
            }
        } else {
            kprintln!("[drivers] ahci: read {} failed", lba);
        }
        pmm::free_frame(frame);
        ok
    }

    /// Write one 512-byte sector. Same bounce law as read.
    pub fn write_block(&mut self, lba: u64, src: &[u8; SECTOR]) -> bool {
        let frame = match pmm::alloc_frame() {
            Some(f) => f,
            None => {
                kprintln!("[drivers] ahci: no bounce frame");
                return false;
            }
        };
        unsafe {
            core::ptr::copy_nonoverlapping(
                src.as_ptr(),
                (frame + self.hhdm) as *mut u8,
                SECTOR,
            );
        }
        let ok = unsafe { self.issue(CMD_WRITE_DMA_EXT, lba, frame, WAIT_IO_MS) };
        if !ok {
            kprintln!("[drivers] ahci: write {} failed", lba);
        }
        pmm::free_frame(frame);
        ok
    }
}

impl Driver for Ahci {
    fn name(&self) -> &'static str {
        "ahci"
    }
}

static DISK: Mutex<Option<Ahci>> = Mutex::new(None);

/// Claim the first real SATA disk. False (and silent) when none answers.
pub fn init(devs: &[crate::drivers::pci::PciDev]) -> bool {
    match Ahci::probe(devs) {
        Some(a) => {
            *DISK.lock() = Some(a);
            // Driver model: polled like virtio-blk until B3 wires IRQs.
            super::register("ahci", None);
            true
        }
        None => false,
    }
}

/// Read LBA into a 512-byte buffer. False if not behind this driver.
pub fn read_block(lba: u64, out: &mut [u8; SECTOR]) -> bool {
    match DISK.lock().as_mut() {
        Some(a) => {
            if lba >= a.capacity_sectors {
                return false;
            }
            a.read_block(lba, out)
        }
        None => false,
    }
}

/// Write a 512-byte buffer to LBA. False if not behind this driver.
pub fn write_block(lba: u64, src: &[u8; SECTOR]) -> bool {
    match DISK.lock().as_mut() {
        Some(a) => {
            if lba >= a.capacity_sectors {
                return false;
            }
            a.write_block(lba, src)
        }
        None => false,
    }
}
