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

unsafe fn write_u32(bus: u8, dev: u8, func: u8, offset: u8, val: u32) {
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
