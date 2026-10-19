//! PS/2 input: keyboard scancodes + mouse packets into lock-free rings.
//! IRQ handlers push (no locks: single producer, IF=0); the window server
//! pops. No input is ever lost to locking, at worst to a full ring.

use core::sync::atomic::{AtomicU8, Ordering};
use x86_64::instructions::port::Port;

use crate::arch::x86_64::pic;
use crate::kprintln;

const RING: usize = 256;

struct ByteRing {
    buf: [u8; RING],
    head: AtomicU8, // write index (producer)
    tail: AtomicU8, // read index (consumer)
}

impl ByteRing {
    const fn new() -> Self {
        Self {
            buf: [0; RING],
            head: AtomicU8::new(0),
            tail: AtomicU8::new(0),
        }
    }

    /// Called from IRQ handlers (IF clear, single producer). Drops on full.
    fn push(&self, b: u8) {
        let h = self.head.load(Ordering::Relaxed);
        if h.wrapping_add(1) == self.tail.load(Ordering::Acquire) {
            return; // full: drop oldest policy would need tail move; drop newest
        }
        // SAFETY: slot exclusively ours (single producer, consumer only reads
        // committed slots: head published after the write).
        unsafe {
            *(self.buf.as_ptr().add(h as usize) as *mut u8) = b;
        }
        self.head.store(h.wrapping_add(1), Ordering::Release);
    }

    /// Called from the server task. None when empty.
    fn pop(&self) -> Option<u8> {
        let t = self.tail.load(Ordering::Relaxed);
        if t == self.head.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: slot committed by producer before head publish.
        let b = unsafe { *self.buf.as_ptr().add(t as usize) };
        self.tail.store(t.wrapping_add(1), Ordering::Release);
        Some(b)
    }
}

static KEYS: ByteRing = ByteRing::new();
static MOUSE: ByteRing = ByteRing::new();

/// IRQ1 path calls this (after the serial print in the handler).
pub fn push_key(sc: u8) {
    KEYS.push(sc);
}

/// IRQ12 path calls this with the raw AUX byte.
pub fn push_mouse(b: u8) {
    MOUSE.push(b);
}

pub fn pop_key() -> Option<u8> {
    KEYS.pop()
}

pub fn pop_mouse() -> Option<u8> {
    MOUSE.pop()
}

/// True if either ring holds bytes (peek for the idle fast-path).
pub fn has_pending() -> bool {
    KEYS.head.load(Ordering::Acquire) != KEYS.tail.load(Ordering::Relaxed)
        || MOUSE.head.load(Ordering::Acquire) != MOUSE.tail.load(Ordering::Relaxed)
}

fn wait_ibf() -> bool {
    for _ in 0..100_000 {
        if unsafe { Port::<u8>::new(0x64).read() } & 0x02 == 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

fn wait_obf() -> bool {
    for _ in 0..100_000 {
        if unsafe { Port::<u8>::new(0x64).read() } & 0x01 != 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

fn mouse_cmd(cmd: u8) -> bool {
    if !wait_ibf() {
        return false;
    }
    unsafe { Port::<u8>::new(0x64).write(0xD4) };
    if !wait_ibf() {
        return false;
    }
    unsafe { Port::<u8>::new(0x60).write(cmd) };
    if !wait_obf() {
        return false;
    }
    unsafe { Port::<u8>::new(0x60).read() == 0xFA }
}

/// Enable the AUX port + mouse streaming. Keyboard keeps working regardless.
pub fn init() {
    // Unmask cascade (IRQ2) + timer + keyboard on master, mouse on slave.
    // Forgetting the cascade bit silently eats all slave IRQs (lesson).
    unsafe {
        pic::PICS.lock().write_masks(0xF8, 0xEF);
    }
    // Enable AUX device.
    if !wait_ibf() {
        kprintln!("[drivers] input: controller stuck, mouse off");
        return;
    }
    unsafe { Port::<u8>::new(0x64).write(0xA8) };
    // Controller config: keep keyboard IRQ, enable mouse IRQ.
    if !wait_ibf() {
        return;
    }
    unsafe { Port::<u8>::new(0x64).write(0x20) };
    if !wait_obf() {
        return;
    }
    let mut cfg = unsafe { Port::<u8>::new(0x60).read() };
    cfg |= 0x03; // kbd IRQ + mouse IRQ on
    cfg &= !0x20; // mouse clock on (clear disable bit)
    if !wait_ibf() {
        return;
    }
    unsafe { Port::<u8>::new(0x64).write(0x60) };
    if !wait_ibf() {
        return;
    }
    unsafe { Port::<u8>::new(0x60).write(cfg) };
    // Mouse: defaults, then enable streaming.
    let ok = mouse_cmd(0xF6) && mouse_cmd(0xF4);
    // Drain any boot bytes so packets start aligned.
    if wait_obf() {
        let _ = unsafe { Port::<u8>::new(0x60).read() };
    }
    kprintln!(
        "[drivers] input: keyboard ring live, mouse {}",
        if ok { "streaming" } else { "off" }
    );
    // Driver model: PS/2 keyboard (IRQ1) + AUX mouse (IRQ12).
    super::register("ps2-kbd", Some(1));
    super::register("ps2-mouse", Some(12));
}
