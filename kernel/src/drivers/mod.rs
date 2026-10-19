//! Drivers: hardware only. Trait + one file per device.
//! Serial/fb come up first (console); storage comes later (needs PMM+heap).

pub mod fb;
pub mod input;
pub mod pci;
pub mod rtc;
pub mod serial;
pub mod storage;
pub mod virtio_blk;
pub mod virtio_input;
pub mod virtio_net;

use spin::Mutex;

use crate::kprintln;

/// Early drivers: serial + framebuffer console. No allocation beyond heap,
// which is already up by the time this runs (see kmain order).
pub fn init() {
    serial::init();
    // Wall clock at boot (CMOS, no IRQ): the desktop clock + greeting use it.
    rtc::init();
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
///
/// Lifecycle contract every driver follows:
/// 1. `probe` (PCI scan match for hardware; always-true for platform devices)
/// 2. `init` (program the device, then `super::register` with its IRQ line
///    — `None` while polled; B3 fills real lines when IRQ dispatch lands)
/// 3. `handle_irq` (called from the IRQ dispatcher; default ignores).
/// Serial/fb stay outside the model (pre-model console, no IRQ, no probe).
/// First `Driver` implementor + IRQ dispatch arrive with virtio-net (B3).
#[allow(dead_code)]
pub trait Driver {
    fn name(&self) -> &'static str;
    /// Called on the driver's IRQ line. Default: polled drivers ignore it.
    fn handle_irq(&mut self) {}
}

/// One registered driver: name + IRQ line (None = polled for now).
/// B3's IRQ dispatcher maps irq -> index into this table.
pub struct DriverReg {
    pub name: &'static str,
    pub irq: Option<u8>,
}

static REGISTRY: Mutex<alloc::vec::Vec<DriverReg>> = Mutex::new(alloc::vec::Vec::new());

/// Record a live driver. Called once per driver from its init path.
pub fn register(name: &'static str, irq: Option<u8>) {
    REGISTRY.lock().push(DriverReg { name, irq });
}

/// IRQ -> handler fn. Index = IRQ line (0..255). Set by drivers that want
/// IRQ-driven delivery; polled drivers leave None.
static IRQ_HANDLERS: spin::Mutex<[Option<fn()>; 256]> =
    spin::Mutex::new([None; 256]);

/// Register an IRQ handler for `irq`. Called from a driver's init path.
pub fn register_irq_handler(irq: u8, handler: fn()) {
    IRQ_HANDLERS.lock()[irq as usize] = Some(handler);
}

/// Dispatch an IRQ to the registered driver handler. Called from the IDT
/// IRQ handler (IF clear). No locks beyond the handler table read.
pub fn dispatch_irq(irq: u8) {
    if let Some(handler) = IRQ_HANDLERS.lock()[irq as usize] {
        handler();
    }
}

/// One-line registry summary (called from kmain after all inits).
pub fn report() {
    let r = REGISTRY.lock();
    let mut line = alloc::string::String::from("[drivers] registry:");
    for d in r.iter() {
        match d.irq {
            Some(n) => line.push_str(&alloc::format!(" {}(irq{})", d.name, n)),
            None => line.push_str(&alloc::format!(" {}(polled)", d.name)),
        }
    }
    kprintln!("{}", line);
}
