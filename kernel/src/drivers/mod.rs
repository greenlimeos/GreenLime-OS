//! Drivers: hardware only. Trait + one file per device.
//! Serial/fb come up first (console); storage comes later (needs PMM+heap).

pub mod fb;
pub mod input;
pub mod pci;
pub mod serial;
pub mod storage;
pub mod virtio_blk;
pub mod virtio_input;

use crate::kprintln;

/// Early drivers: serial + framebuffer console. No allocation beyond heap,
// which is already up by the time this runs (see kmain order).
pub fn init() {
    serial::init();
}

/// After heap::init: bring up the TTF framebuffer console + boot splash.
/// The splash clears the screen and silences on-screen logging from here
/// on (serial keeps every line); the window server repaints at handoff.
pub fn init_framebuffer() {
    match fb::init() {
        Some((w, h)) => {
            kprintln!("[drivers] fb console: {}x{} TTF", w, h);
            fb::splash("GreenLime OS", "v0.9 \u{2014} booting");
        }
        None => kprintln!("[drivers] fb console: off (serial only)"),
    }
}

/// Driver model: probe for a device, init on match.
/// v0.7 wires virtio-blk concretely; the trait documents the shape every
/// future driver (AHCI, input, net) follows.
#[allow(dead_code)]
pub trait Driver {
    fn name(&self) -> &'static str;
    /// Called on the driver's IRQ line. Default: polled drivers ignore it.
    fn handle_irq(&mut self) {}
}
