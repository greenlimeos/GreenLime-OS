//! Storage bring-up (runs after mm: needs PMM frames + heap).
//! PCI scan -> ONE disk claim: AHCI (real SATA) preferred, virtio-blk
//! (the QEMU test disk) as fallback. The `read_block`/`write_block` API
//! is the only thing FAT32 ever sees, so the backend switch is invisible
//! above this file.

use crate::drivers::{ahci, pci, virtio_blk};
use crate::kprintln;

/// Driver registry entry point. Probes everything, inits what we support.
pub fn init() {
    let devs = pci::scan();
    pci::print_table(&devs);
    if ahci::init(&devs) {
        kprintln!("[drivers] storage: ahci ready");
    } else if virtio_blk::init(&devs) {
        kprintln!("[drivers] storage: virtio-blk ready");
    }
}

/// Read LBA into a 512-byte buffer. False if no disk / bad lba.
pub fn read_block(lba: u64, out: &mut [u8; 512]) -> bool {
    if ahci::read_block(lba, out) {
        return true;
    }
    virtio_blk::read_block(lba, out)
}

/// Write a 512-byte buffer to LBA (M14: FAT32 needs a real write path).
pub fn write_block(lba: u64, src: &[u8; 512]) -> bool {
    if ahci::write_block(lba, src) {
        return true;
    }
    virtio_blk::write_block(lba, src)
}
