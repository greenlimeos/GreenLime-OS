//! Storage bring-up (runs after mm: needs PMM frames + heap).
//! PCI scan -> virtio-blk claim. Single disk.

use crate::drivers::{pci, virtio_blk};
use crate::kprintln;

/// Driver registry entry point. Probes everything, inits what we support.
pub fn init() {
    let devs = pci::scan();
    pci::print_table(&devs);
    if virtio_blk::init(&devs) {
        kprintln!("[drivers] storage: virtio-blk ready");
    }
}

/// Read LBA into a 512-byte buffer. False if no disk / bad lba.
pub fn read_block(lba: u64, out: &mut [u8; 512]) -> bool {
    virtio_blk::read_block(lba, out)
}

/// Write a 512-byte buffer to LBA (M14: FAT32 needs a real write path).
pub fn write_block(lba: u64, src: &[u8; 512]) -> bool {
    virtio_blk::write_block(lba, src)
}
