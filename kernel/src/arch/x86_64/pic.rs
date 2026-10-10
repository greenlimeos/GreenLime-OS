//! 8259 PIC + PIT timer programming. Only timer (IRQ0) and keyboard (IRQ1)
//! are unmasked; everything else stays masked until its driver arrives.

use pic8259::ChainedPics;
use spin::Mutex;
use x86_64::instructions::port::Port;

use crate::kprintln;

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = 40;

pub static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

/// Init PIC, mask all but timer+keyboard, program PIT channel 0 at ~100 Hz.
pub fn init() {
    unsafe {
        PICS.lock().initialize();
        // Timer + keyboard + cascade on master; mouse (IRQ12) on slave.
        PICS.lock().write_masks(0xF8, 0xEF);
    }
    // PIT: channel 0, lo+hi access, mode 3 (square wave), binary.
    const DIVISOR: u16 = 11932; // 1193182 Hz / 100 Hz
    unsafe {
        Port::<u8>::new(0x43).write(0x36);
        Port::<u8>::new(0x40).write((DIVISOR & 0xFF) as u8);
        Port::<u8>::new(0x40).write((DIVISOR >> 8) as u8);
    }
    kprintln!("[arch] pic masked (timer+kbd live), pit @100Hz");
}

pub fn eoi(vector: u8) {
    // SAFETY: vector is a valid IRQ we unmasked; lock held, no reentry
    // (handlers run with IF clear, main masks IRQs around console).
    unsafe { PICS.lock().notify_end_of_interrupt(vector) };
}
