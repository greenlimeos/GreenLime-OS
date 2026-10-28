//! kernel/src/crypto.rs — the M20 ring-3 entropy lane: a ChaCha8 DRBG the
//! syscall gate hands out through `Syscall::Rand`.
//!
//! Honest scope (this OS states limits, it does not hide them): this is
//! NOT a certified CSPRNG. The seed is the KASLR seed laced with fresh
//! TSC reads — the same entropy class the net stack already trusts for
//! its jitter randomness (`[mm] kaslr: seed ... pinned (stack jitter +
//! net rng)`). The concrete deliverable is "TLS 1.3 handshakes get fresh
//! ephemeral keys per connection", not "protect a nuclear football";
//! anything in this kernel that crosses that bar (server-side long-lived
//! secrets) must earn a real entropy-pool milestone first.
//!
//! Shape: standard ChaCha state, 32-byte key + 64-bit block counter, two
//! rounds-pairs = 8 rounds total. Every refill asks the counter where the
//! blocks it consumed left off, under a mutex: two racing syscallers must
//! never see the same counter block — that is the one corruption TLS
//! would actually notice (reused key = same keystream = plaintext).
//! A racy but DIFFERENT mix of blocks would be merely cosmetic.

use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

/// ChaCha stream constants: "expand 32-byte k".
const SIGMA: [u32; 4] = [0x6170_7865, 0x3320_646E, 0x7962_2D32, 0x6B20_6574];

static READY: AtomicBool = AtomicBool::new(false);
/// Serializes every refill. Boot thread sets it up before any task runs;
/// the syscall path is the only writer after that.
static GATE: Mutex<Drbg> = Mutex::new(Drbg::empty());

struct Drbg {
    key: [u8; 32],
    counter: u64,
}

impl Drbg {
    const fn empty() -> Self {
        Drbg {
            key: [0u8; 32],
            counter: 0,
        }
    }
}

/// One ChaCha quarter-round (the whole algorithm is 8 of these pairs).
fn quarter_round(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] ^= s[a];
    s[d] = s[d].rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[d] ^= s[c];
    s[d] = s[d].rotate_left(12);
    s[b] = s[b].wrapping_add(s[c]);
    s[c] ^= s[b];
    s[c] = s[c].rotate_left(8);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] ^= s[a];
    s[d] = s[d].rotate_left(7);
}

/// Standard ChaCha-x block: sigma + key words + 64-bit counter split over
/// words 12/13 (original Bernstein layout, no nonce word pair — the
/// counter IS the nonce lane; it never repeats under the mutex).
fn chacha_block(key: &[u8; 32], counter: u64, out: &mut [u32; 16]) {
    let mut s = [0u32; 16];
    s[..4].copy_from_slice(&SIGMA);
    for (i, w) in s[4..12].iter_mut().enumerate() {
        let o = i * 4;
        *w = u32::from_le_bytes([key[o], key[o + 1], key[o + 2], key[o + 3]]);
    }
    s[12] = counter as u32;
    s[13] = (counter >> 32) as u32;
    let start = s;
    for _ in 0..4 {
        // 8 rounds = 4 iterations of (one column group + one diagonal
        // group); a "normal ChaCha20 round" is itself four parallel QRs.
        quarter_round(&mut s, 0, 4, 8, 12);
        quarter_round(&mut s, 1, 5, 9, 13);
        quarter_round(&mut s, 2, 6, 10, 14);
        quarter_round(&mut s, 3, 7, 11, 15);
        quarter_round(&mut s, 0, 5, 10, 15);
        quarter_round(&mut s, 1, 6, 11, 12);
        quarter_round(&mut s, 2, 7, 8, 13);
        quarter_round(&mut s, 3, 4, 9, 14);
    }
    for i in 0..16 {
        out[i] = start[i].wrapping_add(s[i]);
    }
}

/// Boot-time init: fold the KASLR seed with two fresh TSC reads so two
/// boots sharing a heart still diverge (the TSC keeps ticking between
/// LFs). Runs before any task exists, on the single boot CPU.
pub fn init() {
    let t1 = rdtsc();
    let s = crate::mm::kaslr_seed();
    let t2 = rdtsc();
    // Spread bits; NOT load-bearing crypto — the load-bearing insight is
    // simply that the seed survives (kaslr pinned) and the TSC moves.
    let a = (s ^ t1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let b = (s.rotate_left(23) ^ t2).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let mut key = [0u8; 32];
    key[..8].copy_from_slice(&a.to_le_bytes());
    key[8..16].copy_from_slice(&b.to_le_bytes());
    key[16..24].copy_from_slice(&a.rotate_right(19).to_le_bytes());
    key[24..32].copy_from_slice(&b.rotate_right(29).to_le_bytes());
    {
        let mut g = GATE.lock();
        g.key = key;
        g.counter = 0;
    }
    READY.store(true, Ordering::SeqCst);
}

/// Fill `buf` from the DRBG. Blocks under the refill mutex; on any call
/// before init (impossible through the syscall gate, but belt-double-law
/// for internal users) the buffer is zero-filled and FALSE comes back.
pub fn rand_fill(buf: &mut [u8]) -> bool {
    if !READY.load(Ordering::SeqCst) {
        unsafe { core::ptr::write_bytes(buf.as_mut_ptr(), 0, buf.len()) };
        return false;
    }
    let mut g = GATE.lock();
    let mut block = [0u32; 16];
    let mut off = 0usize;
    while off < buf.len() {
        let counter = g.counter;
        chacha_block(&g.key, counter, &mut block);
        g.counter = counter.wrapping_add(1);
        let raw = by_words(&block);
        let n = core::cmp::min(64, buf.len() - off);
        buf[off..off + n].copy_from_slice(&raw[..n]);
        off += n;
    }
    drop(g);
    // Deadbolt: zero the stack copy of the last block. The tail bytes of a
    // partially-consumed block are live keystream material — not leaking
    // what the kernel can cheaply avoid leaking is house law. (The DRBG
    // state itself is safe by construction: the counter already moved on.)
    unsafe { core::ptr::write_bytes(block.as_mut_ptr(), 0, block.len()) };
    true
}

fn by_words(w: &[u32; 16]) -> [u8; 64] {
    let mut out = [0u8; 64];
    for (i, word) in w.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

#[inline]
fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdtsc",
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack, preserves_flags)
        );
    }
    (hi as u64) << 32 | lo as u64
}
