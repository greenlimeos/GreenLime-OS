//! PCI configuration-space scanner (ports 0xCF8/0xCFC).
//! Enough to find virtio devices and name common classes. MSI-X and
//! bridges are future work (single root bus + devices is all QEMU gives us).

use alloc::vec::Vec;
use x86_64::instructions::port::Port;

use crate::kprintln;

const CONFIG_ADDR: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

#[derive(Debug, Clone, Copy)]
pub struct PciDev {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub bar0: u32,
}

fn cfg_addr(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    (1 << 31)
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC)
}

unsafe fn read_u32(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    unsafe {
        Port::<u32>::new(CONFIG_ADDR).write(cfg_addr(bus, dev, func, offset));
        Port::<u32>::new(CONFIG_DATA).read()
    }
}

pub unsafe fn write_u32(bus: u8, dev: u8, func: u8, offset: u8, val: u32) {
    unsafe {
        Port::<u32>::new(CONFIG_ADDR).write(cfg_addr(bus, dev, func, offset));
        Port::<u32>::new(CONFIG_DATA).write(val);
    }
}

fn class_name(class: u8, subclass: u8) -> &'static str {
    match (class, subclass) {
        (0x01, 0x01) => "IDE controller",
        (0x01, 0x06) => "SATA/AHCI controller",
        (0x01, _) => "mass storage",
        (0x02, _) => "network",
        (0x03, _) => "display",
        (0x06, 0x00) => "host bridge",
        (0x06, 0x01) => "ISA bridge",
        (0x06, 0x04) => "PCI bridge",
        (0x0C, 0x03) => "USB controller",
        (0xFF, 0xFF) => "unclassified",
        _ => "other",
    }
}

/// Scan buses/devices/functions, return everything present.
pub fn scan() -> Vec<PciDev> {
    let mut out = Vec::new();
    for bus in 0..=255u16 {
        for dev in 0..32u8 {
            // Function 0 decides presence + multifunction.
            let id = unsafe { read_u32(bus as u8, dev, 0, 0) };
            if id == 0xFFFF_FFFF || id == 0 {
                continue;
            }
            let header = unsafe { read_u32(bus as u8, dev, 0, 0x0C) };
            let multifunction = (header >> 16) & 0x80 != 0;
            let funcs = if multifunction { 8 } else { 1 };
            for func in 0..funcs {
                let id = unsafe { read_u32(bus as u8, dev, func, 0) };
                if id == 0xFFFF_FFFF || id == 0 {
                    continue;
                }
                let class_reg = unsafe { read_u32(bus as u8, dev, func, 0x08) };
                out.push(PciDev {
                    bus: bus as u8,
                    dev,
                    func,
                    vendor: (id & 0xFFFF) as u16,
                    device: (id >> 16) as u16,
                    class: (class_reg >> 24) as u8,
                    subclass: (class_reg >> 16) as u8,
                    prog_if: (class_reg >> 8) as u8,
                    bar0: unsafe { read_u32(bus as u8, dev, func, 0x10) },
                });
            }
        }
    }
    out
}

/// Print the device table (boot log proof of discovery).
pub fn print_table(devs: &[PciDev]) {
    kprintln!("[drivers] pci: {} devices", devs.len());
    for d in devs {
        kprintln!(
            "[drivers] pci {:02x}:{:02x}.{} {:04x}:{:04x} {} progif={:#04x} bar0={:#x}",
            d.bus,
            d.dev,
            d.func,
            d.vendor,
            d.device,
            class_name(d.class, d.subclass),
            d.prog_if,
            d.bar0
        );
        if d.class == 0x01 && d.subclass == 0x06 {
            kprintln!("[drivers] ahci: SATA controller present (full driver: later)");
        }
    }
}

/// Enable I/O space + bus mastering for a device.
pub unsafe fn enable_io_busmaster(d: &PciDev) {
    unsafe {
        let cmd = read_u32(d.bus, d.dev, d.func, 0x04);
        write_u32(d.bus, d.dev, d.func, 0x04, cmd | 0x05);
    }
}

/// Enable memory space + bus mastering (modern BARs).
pub unsafe fn enable_mmio_busmaster(d: &PciDev) {
    unsafe {
        let cmd = read_u32(d.bus, d.dev, d.func, 0x04);
        write_u32(d.bus, d.dev, d.func, 0x04, cmd | 0x06);
    }
}

/// Byte read from config space (capability lists are byte-structured).
pub unsafe fn cfg_read_u8(d: &PciDev, offset: u8) -> u8 {
    let v = unsafe { read_u32(d.bus, d.dev, d.func, offset & 0xFC) };
    (v >> ((offset & 3) * 8)) as u8
}

/// DWord read (capability structs live at dword-aligned addresses).
pub unsafe fn cfg_read_u32(d: &PciDev, offset: u8) -> u32 {
    unsafe { read_u32(d.bus, d.dev, d.func, offset & 0xFC) }
}

/// BAR0 as an I/O port base (None if memory-mapped — we only do PIO for now).
pub fn bar0_iobase(d: &PciDev) -> Option<u16> {
    if d.bar0 & 1 == 1 {
        Some((d.bar0 & 0xFFFC) as u16)
    } else {
        None
    }
}

// MSI-X capability (PCI 3.0 §6.12): ID 0x11.
// Capability struct: {cap_id(8), next(8), msg_ctrl(16), table_offset(32)}.
const PCI_CAP_ID_MSIX: u8 = 0x11;
const MSI_X_TABLE_OFFSET_MASK: u32 = 0xFFFFFFF8;
const MSI_X_TABLE_BIR_MASK: u32 = 0x7;
const MSI_X_ENTRY_BYTES: u32 = 16;

/// MSI-X table entry (16 bytes): msg addr (8), msg data (4), vector control (2), reserved (2).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MsiXEntry {
    pub addr: u64,
    pub data: u32,
    pub vec_ctrl: u16,
    _reserved: u16,
}

/// Walk the PCI capability list for an MSI-X capability. Returns the
/// capability offset or None. Called at boot (single CPU, IRQs off).
pub unsafe fn msix_cap(d: &PciDev) -> Option<u8> {
    let mut cap = unsafe { cfg_read_u8(d, 0x34) };
    for _ in 0..32 {
        if cap == 0 || cap < 0x40 || cap > 0xF0 {
            return None;
        }
        let id = unsafe { cfg_read_u8(d, cap) };
        let next = unsafe { cfg_read_u8(d, cap + 1) };
        if id == PCI_CAP_ID_MSIX {
            return Some(cap);
        }
        cap = next;
    }
    None
}

/// Read a u16 from PCI config space.
pub unsafe fn cfg_read_u16(d: &PciDev, offset: u8) -> u16 {
    let v = unsafe { read_u32(d.bus, d.dev, d.func, offset & 0xFC) };
    (v >> ((offset & 2) * 8)) as u16
}

/// Read a u32 from PCI config space at the given byte offset.
#[allow(dead_code)]
pub unsafe fn cfg_read_u32_at(d: &PciDev, offset: u8) -> u32 {
    unsafe { read_u32(d.bus, d.dev, d.func, offset) }
}

/// PCI Interrupt Line register (offset 0x3C): which IRQ line the device is
/// wired to (set by firmware/BIOS). Returns None if not programmable.
#[allow(dead_code)]
pub fn irq_line(d: &PciDev) -> Option<u8> {
    let v = unsafe { read_u32(d.bus, d.dev, d.func, 0x3C) };
    let line = (v & 0xFF) as u8;
    if line == 0xFF {
        None
    } else {
        Some(line)
    }
}

/// PCI Interrupt Pin register (offset 0x3D): which INTx pin (1=INTA, 2=INTB, …).
#[allow(dead_code)]
pub fn irq_pin(d: &PciDev) -> Option<u8> {
    let v = unsafe { read_u32(d.bus, d.dev, d.func, 0x3D) };
    let pin = ((v >> 8) & 0xFF) as u8;
    if pin == 0 {
        None
    } else {
        Some(pin)
    }
}

/// Program MSI-X table `entry` to deliver an MSI write to `msi_addr` with
/// `msi_data` (xAPIC: addr 0xFEE00000 | dest<<12, data = vector with fixed
/// delivery). Returns the physical page holding the table (kept mapped for
/// the device lifetime). MSI-X must already be enabled (msix_enable).
///
/// `vector` is used only for the boot breadcrumb. The entry write is
/// verified by read-back (QEMU backs the table with RAM); a mismatch means
/// the BAR/BIR parse was wrong, so we fail instead of arming nothing.
pub unsafe fn msix_program_entry(
    d: &PciDev,
    entry: u16,
    vector: u8,
    msi_addr: u64,
    msi_data: u32,
) -> Option<u64> {
    let cap = unsafe { msix_cap(d)? };
    // Message control: table size = entries - 1 (bits [10:0]).
    let msg_ctrl = unsafe { cfg_read_u16(d, cap + 2) };
    let table_size = (msg_ctrl as u32 & 0x7FF) + 1;
    if entry as u32 >= table_size {
        return None;
    }
    // Table Offset/BIR at cap + 4. Read as two u16s: cfg_read_u16 aligns
    // the underlying dword access itself, so this is correct even if the
    // capability isn't dword-aligned.
    let table_off = unsafe {
        (cfg_read_u16(d, cap + 4) as u32) | ((cfg_read_u16(d, cap + 6) as u32) << 16)
    };
    let bir = (table_off & MSI_X_TABLE_BIR_MASK) as u8;
    let offset: u64 = (table_off & MSI_X_TABLE_OFFSET_MASK) as u64;

    // BAR address for the table's BIR.
    let bar_addr = match bir {
        0 => Some(d.bar0 as u64),
        1 => unsafe { Some(read_u32(d.bus, d.dev, d.func, 0x14) as u64) },
        2 => {
            let lo = unsafe { read_u32(d.bus, d.dev, d.func, 0x18) };
            let hi = unsafe { read_u32(d.bus, d.dev, d.func, 0x1C) };
            Some(((hi as u64) << 32) | (lo as u64))
        }
        3 => unsafe { Some(read_u32(d.bus, d.dev, d.func, 0x20) as u64) },
        4 => unsafe { Some(read_u32(d.bus, d.dev, d.func, 0x24) as u64) },
        5 => unsafe { Some(read_u32(d.bus, d.dev, d.func, 0x28) as u64) },
        _ => None,
    }? & !0xFu64; // mask BAR type bits

    // The table sits at BAR + offset. Map the containing page identity
    // (probe runs on the boot CPU before the scheduler; repairs flags if
    // the page is already mapped) and build the entry pointer.
    let table_phys = bar_addr + offset;
    let page = table_phys & !0xFFFu64;
    let in_page = table_phys & 0xFFFu64;
    let table_virt = unsafe { crate::mm::vmm::map_mmio_page(page)? } + in_page;

    // Write the entry: addr, data, vector control (bit 0 = mask; 0 =
    // unmasked). The control write MUST be the 4-byte dword at +12
    // (vec_ctrl + reserved): the table's memory region rejects accesses
    // narrower than 4 bytes (valid.min_access_size = 4), so a 16-bit
    // store is silently dropped — QEMU keeps the reset-default mask bit
    // set and every later interrupt goes to the masked/pending path
    // instead of being delivered. This is exactly how the first MSI-X
    // bring-up looked silent (PBA bit 0 stuck at 1, no trace event).
    let entry_ptr =
        (table_virt + entry as u64 * MSI_X_ENTRY_BYTES as u64) as *mut MsiXEntry;
    unsafe {
        (*entry_ptr).addr = msi_addr;
        (*entry_ptr).data = msi_data;
        let ctrl_ptr =
            (table_virt + entry as u64 * MSI_X_ENTRY_BYTES as u64 + 12) as *mut u32;
        ctrl_ptr.write_volatile(0);
        // Verify the write landed (the table is RAM-backed on QEMU):
        // entry echoed AND the mask bit really clear.
        let rb = core::ptr::read_volatile(entry_ptr);
        let rb_ctrl = core::ptr::read_volatile(ctrl_ptr);
        if rb.addr != msi_addr || rb.data != msi_data || rb_ctrl & 1 != 0 {
            crate::kprintln!(
                "[pci] msix: entry {} read-back mismatch (addr={:#x} data={:#x} ctrl={:#x})",
                entry,
                rb.addr,
                rb.data,
                rb_ctrl
            );
            return None;
        }
    }
    crate::kprintln!(
        "[pci] msix: entry {} -> addr {:#x} data {:#x} (vector {:#04x}, cap {:#x}, bir {}, page {:#x})",
        entry,
        msi_addr,
        msi_data,
        vector,
        cap,
        bir,
        page
    );

    Some(page)
}

/// MSI-X Message Control (u16 at cap + 2): bit 15 = MSI-X Enable,
/// bit 14 = Function Mask (1 = mask all vectors), bits 10..0 = Table
/// Size - 1. Accessed via the u16 config helpers so the field is read or
/// written correctly regardless of the capability's dword alignment.
const MSI_X_CTL_ENABLE: u16 = 1 << 15;
const MSI_X_CTL_FUNC_MASK: u16 = 1 << 14;

/// Read the MSI-X Message Control register.
#[allow(dead_code)]
pub unsafe fn msix_msg_ctrl(d: &PciDev) -> Option<u16> {
    let cap = unsafe { msix_cap(d)? };
    Some(unsafe { cfg_read_u16(d, cap + 2) })
}

/// Enable MSI-X: clear Function Mask, set MSI-X Enable, confirm by reading
/// back. Until this bit is set the device never generates MSI writes — it
/// silently stays on legacy INTx, which looks like "MSI-X programmed but
/// no interrupts".
pub unsafe fn msix_enable(d: &PciDev) -> bool {
    let cap = match unsafe { msix_cap(d) } {
        Some(c) => c,
        None => return false,
    };
    let ctrl = unsafe { cfg_read_u16(d, cap + 2) };
    unsafe {
        cfg_write_u16(
            d,
            cap + 2,
            (ctrl & !MSI_X_CTL_FUNC_MASK) | MSI_X_CTL_ENABLE,
        )
    };
    unsafe { cfg_read_u16(d, cap + 2) & MSI_X_CTL_ENABLE != 0 }
}

/// Disable MSI-X (clears the enable bit; the device reverts to INTx).
#[allow(dead_code)]
pub unsafe fn msix_disable(d: &PciDev) -> bool {
    let cap = match unsafe { msix_cap(d) } {
        Some(c) => c,
        None => return false,
    };
    let ctrl = unsafe { cfg_read_u16(d, cap + 2) };
    unsafe { cfg_write_u16(d, cap + 2, ctrl & !MSI_X_CTL_ENABLE) };
    true
}

/// Read a u32 from PCI config space at the given byte offset.
#[allow(dead_code)]
pub unsafe fn cfg_write_u32(d: &PciDev, offset: u8, val: u32) {
    unsafe { write_u32(d.bus, d.dev, d.func, offset, val) }
}

/// Write a u16 to PCI config space at the given byte offset (read-modify-
/// write of the containing dword, so neighbors are preserved).
pub unsafe fn cfg_write_u16(d: &PciDev, offset: u8, val: u16) {
    let reg = offset & 0xFC;
    let shift = (offset & 2) * 8;
    unsafe {
        let v = read_u32(d.bus, d.dev, d.func, reg);
        let v = (v & !(0xFFFFu32 << shift)) | ((val as u32) << shift);
        write_u32(d.bus, d.dev, d.func, reg, v);
    }
}
