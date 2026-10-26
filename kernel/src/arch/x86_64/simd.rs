//! M13 SIMD support: SSE2 compositor kernels (hand-written asm) + the XMM
//! roundtrip probe.
//!
//! The kernel target is built WITHOUT `+sse` and WITH soft-float — the LLVM
//! backend rejects `#[target_feature(enable = "sse2")]` on this spec
//! (soft-float + sse is a hard mismatch, rust#117938), and enabling sse
//! globally would let auto-vectorization emit XMM code anywhere, including
//! IRQ handlers — which is exactly the corruption the per-task FXSAVE design
//! protects against. So all kernel SIMD lives in raw `global_asm!` blocks:
//! the assembler assembles it, LLVM never sees an ISA violation, and every
//! Rust path stays integer-only (IRQ handlers can never touch XMM).
//!
//! These functions are called exclusively from task context (winsrv's
//! compositor). SSE2 is part of the x86-64 baseline and every CPU enables
//! CR4.OSFXSR at boot; the context switch FXSAVEs/FXRSTORs XMM0–15, which
//! is what makes per-task XMM ownership hold across preemption.
//!
//! All kernels are BIT-EXACT with winsrv's scalar `blend_px` fixed-point
//! reference — compositor pixel contracts depend on that equality.

use core::arch::{asm, global_asm};

// ---------------------------------------------------------------------------
// XMM roundtrip probe: prove the context switch preserves XMM0–15.
// ---------------------------------------------------------------------------

// Sentinel/result buffers.
#[repr(C, align(16))]
struct ProbeBuf([u8; 256]);
static mut PROBE_IN: ProbeBuf = ProbeBuf([0; 256]);
static mut PROBE_OUT: ProbeBuf = ProbeBuf([0; 256]);

global_asm!(
    r#"
    .text
    .code64
    .global fx_probe_asm
    fx_probe_asm:
        # rdi = callback, rsi = sentinel [u8;256], rdx = out [u8;256]
        # Load 16 distinct 16-byte sentinels into XMM0..15, call the callback
        # (which yields to the scheduler and lets other tasks run), then spill
        # the registers. If FXSAVE/FXRSTOR is missing, the other tasks'
        # SSE codegen overwrites XMM and the comparison fails.
        push rbx
        push r12
        push r13                 # 3 pushes -> rsp 16-aligned before call
        mov rbx, rdi
        mov r12, rdx
        movdqu xmm0,  [rsi + 0x00]
        movdqu xmm1,  [rsi + 0x10]
        movdqu xmm2,  [rsi + 0x20]
        movdqu xmm3,  [rsi + 0x30]
        movdqu xmm4,  [rsi + 0x40]
        movdqu xmm5,  [rsi + 0x50]
        movdqu xmm6,  [rsi + 0x60]
        movdqu xmm7,  [rsi + 0x70]
        movdqu xmm8,  [rsi + 0x80]
        movdqu xmm9,  [rsi + 0x90]
        movdqu xmm10, [rsi + 0xA0]
        movdqu xmm11, [rsi + 0xB0]
        movdqu xmm12, [rsi + 0xC0]
        movdqu xmm13, [rsi + 0xD0]
        movdqu xmm14, [rsi + 0xE0]
        movdqu xmm15, [rsi + 0xF0]
        call rbx
        movdqu [r12 + 0x00], xmm0
        movdqu [r12 + 0x10], xmm1
        movdqu [r12 + 0x20], xmm2
        movdqu [r12 + 0x30], xmm3
        movdqu [r12 + 0x40], xmm4
        movdqu [r12 + 0x50], xmm5
        movdqu [r12 + 0x60], xmm6
        movdqu [r12 + 0x70], xmm7
        movdqu [r12 + 0x80], xmm8
        movdqu [r12 + 0x90], xmm9
        movdqu [r12 + 0xA0], xmm10
        movdqu [r12 + 0xB0], xmm11
        movdqu [r12 + 0xC0], xmm12
        movdqu [r12 + 0xD0], xmm13
        movdqu [r12 + 0xE0], xmm14
        movdqu [r12 + 0xF0], xmm15
        pop r13
        pop r12
        pop rbx
        ret
"#
);

extern "C" {
    fn fx_probe_asm(cb: extern "C" fn(), sentinel: *const u8, out: *mut u8);
}

/// Load XMM0–15 with sentinels, call `cb` (must yield to the scheduler so
/// other tasks run), then count bytes that changed. 0 = switch preserved all
/// 256 bytes. Task context only.
pub fn fx_roundtrip_probe(cb: extern "C" fn()) -> usize {
    // SAFETY: one-shot from winsrv's task; exclusive access to the two probe
    // buffers; `cb` just yields.
    unsafe {
        let inp = core::ptr::addr_of_mut!(PROBE_IN.0) as *mut u8;
        for i in 0..256 {
            *inp.add(i) = i as u8;
        }
        let out = core::ptr::addr_of_mut!(PROBE_OUT.0) as *mut u8;
        fx_probe_asm(cb, inp, out);
        let mut bad = 0usize;
        for i in 0..256 {
            if *out.add(i) != i as u8 {
                bad += 1;
            }
        }
        bad
    }
}

// ---------------------------------------------------------------------------
// SSE2 span kernels (assembly). Pixel format throughout: packed XRGB,
// 0x00RRGGBB (bytes B,G,R,X in memory). Blend math mirrors `blend_px`:
// out = (base*(256-a) + tint*a) >> 8 per channel, top byte forced to 0.
// ---------------------------------------------------------------------------

// copy_span_vec: rdi = dst, rsi = src, rdx = full-vectors-of-4 count.
global_asm!(
    r#"
    .text
    .code64
    .global copy_span_vec
    copy_span_vec:
        test rdx, rdx
        jz .Lcp_done
.Lcp_loop:
        movdqu xmm0, [rsi]
        movdqu [rdi], xmm0
        add rsi, 16
        add rdi, 16
        dec rdx
        jnz .Lcp_loop
.Lcp_done:
        ret
"#
);

// blend_const_vec: rdi = dst, rsi = src (wallpaper base), rdx = vector count,
// ecx = tint (0x00RRGGBB), r8d = alpha (0..=256).
global_asm!(
    r#"
    .text
    .code64
    .global blend_const_vec
    blend_const_vec:
        test rdx, rdx
        jz .Lbc_done
        # Constants (built once): xb = a, r9 = 256-a.
        push rbx
        push r12
        mov ebx, r8d               # a
        mov r9d, 256
        sub r9d, ebx               # ia = base weight
        # Tint*a per channel: tb = t.b*a, tg = t.g*a, tr = t.r*a (<= 65025).
        movzx r10d, cl             # tint B byte
        imul r10d, ebx
        mov eax, ecx
        shr eax, 8
        movzx r11d, al             # tint G byte
        imul r11d, ebx
        mov eax, ecx
        shr eax, 16
        movzx r12d, al             # tint R byte
        imul r12d, ebx
        # xmm5 tint vector: u16 lanes [tb, tg, tr, 0] per pixel.
        sub rsp, 16
        mov eax, r11d
        shl eax, 16
        or eax, r10d               # dword0 = tb | tg<<16
        mov [rsp], eax
        mov [rsp+8], eax           # dword2 = same
        mov eax, r12d
        mov [rsp+4], eax           # dword1 = tr | 0<<16
        mov [rsp+12], eax          # dword3 = same
        movdqu xmm5, [rsp]
        add rsp, 16
        # xmm4 ia broadcast to all 8 u16 lanes.
        movd xmm4, r9d
        punpcklwd xmm4, xmm4
        pshufd xmm4, xmm4, 0
        # xmm6 mask (kill the X byte after packuswb).
        mov eax, 0x00FFFFFF
        movd xmm6, eax
        pshufd xmm6, xmm6, 0
        pxor xmm7, xmm7
.Lbc_loop:
        movdqu xmm0, [rsi]
        movdqa xmm1, xmm0
        punpcklbw xmm0, xmm7       # px0,px1 as u16
        punpckhbw xmm1, xmm7       # px2,px3 as u16
        pmullw  xmm0, xmm4         # base*ia
        pmullw  xmm1, xmm4
        paddw   xmm0, xmm5         # + tint*a (sums <= 65280, no wrap)
        paddw   xmm1, xmm5
        psrlw   xmm0, 8            # >> 8
        psrlw   xmm1, 8
        packuswb xmm0, xmm1        # back to bytes, original order
        pand    xmm0, xmm6         # X byte := 0
        movdqu [rdi], xmm0
        add rsi, 16
        add rdi, 16
        dec rdx
        jnz .Lbc_loop
        pop r12
        pop rbx
.Lbc_done:
        ret
"#
);

// blend_over_vec: rdi = dst (in place base), rsi = over, rdx = vector count,
// ecx = alpha.
global_asm!(
    r#"
    .text
    .code64
    .global blend_over_vec
    blend_over_vec:
        test rdx, rdx
        jz .Lbo_done
        mov r9d, 256
        sub r9d, ecx               # ia
        # xmm4 ia broadcast, xmm5 a broadcast.
        movd xmm4, r9d
        punpcklwd xmm4, xmm4
        pshufd xmm4, xmm4, 0
        movd xmm5, ecx
        punpcklwd xmm5, xmm5
        pshufd xmm5, xmm5, 0
        # xmm6 mask.
        mov eax, 0x00FFFFFF
        movd xmm6, eax
        pshufd xmm6, xmm6, 0
        pxor xmm7, xmm7
.Lbo_loop:
        movdqu xmm0, [rdi]         # base
        movdqu xmm2, [rsi]         # over
        movdqa xmm1, xmm0
        movdqa xmm3, xmm2
        punpcklbw xmm0, xmm7       # base lo u16
        punpckhbw xmm1, xmm7       # base hi u16
        punpcklbw xmm2, xmm7       # over lo u16
        punpckhbw xmm3, xmm7       # over hi u16
        pmullw  xmm0, xmm4
        pmullw  xmm1, xmm4
        pmullw  xmm2, xmm5
        pmullw  xmm3, xmm5
        paddw   xmm0, xmm2         # base*ia + over*a
        paddw   xmm1, xmm3
        psrlw   xmm0, 8
        psrlw   xmm1, 8
        packuswb xmm0, xmm1
        pand    xmm0, xmm6
        movdqu [rdi], xmm0
        add rdi, 16
        add rsi, 16
        dec rdx
        jnz .Lbo_loop
.Lbo_done:
        ret
"#
);

extern "C" {
    fn copy_span_vec(dst: *mut u32, src: *const u32, vecs: usize);
    fn blend_const_vec(dst: *mut u32, src: *const u32, vecs: usize, tint: u32, a: u32);
    fn blend_over_vec(dst: *mut u32, over: *const u32, vecs: usize, a: u32);
}

/// Scalar blend mirror of winsrv's `blend_px` (channel-wise, Q8 alpha,
/// tint passed packed 0x00RRGGBB, top byte always zeroed in the result).
#[inline]
fn blend_scalar(w: u32, tint: u32, a: u32) -> u32 {
    let wr = (w >> 16) & 0xFF;
    let wg = (w >> 8) & 0xFF;
    let wb = w & 0xFF;
    let r = (wr * (256 - a) + ((tint >> 16) & 0xFF) * a) >> 8;
    let g = (wg * (256 - a) + ((tint >> 8) & 0xFF) * a) >> 8;
    let b = (wb * (256 - a) + (tint & 0xFF) * a) >> 8;
    (r << 16) | (g << 8) | b
}

/// Copy `n` pixels; SSE2 for 4-px runs, scalar tail.
pub fn copy_span(dst: *mut u32, src: *const u32, n: usize) {
    let vecs = n / 4;
    // SAFETY: caller guarantees [src, src+n) readable and [dst, dst+n)
    // writable (4-byte aligned u32 pixels); the asm touches exactly those
    // vecs*4 pixels and no other state.
    unsafe { copy_span_vec(dst, src, vecs) };
    let mut i = vecs * 4;
    while i < n {
        // SAFETY: same span guarantee.
        unsafe { *dst.add(i) = *src.add(i) };
        i += 1;
    }
}

/// out[i] = blend(src[i], tint, a) — acrylic (constant tint) kernel.
pub fn blend_const_span(src: *const u32, dst: *mut u32, n: usize, tint: u32, a: u32) {
    let vecs = n / 4;
    // SAFETY: distinct spans of `n+1` valid u32 pixels (src read, dst write).
    unsafe { blend_const_vec(dst, src, vecs, tint & 0x00FF_FFFF, a) };
    let mut i = vecs * 4;
    while i < n {
        // SAFETY: span guarantee.
        unsafe { *dst.add(i) = blend_scalar(*src.add(i), tint, a) };
        i += 1;
    }
}

/// out[i] = blend(dst[i], over[i], a) — scrim/shadow kernel (in place when
/// dst == base; each chunk reads its base before storing, exactly like the
/// scalar loop it replaces).
pub fn blend_over_span(dst: *mut u32, over: *const u32, n: usize, a: u32) {
    let vecs = n / 4;
    // SAFETY: valid spans; the in-place chunk (base = dst, overlay = over)
    // is read before written within each iteration.
    unsafe { blend_over_vec(dst, over, vecs, a) };
    let mut i = vecs * 4;
    while i < n {
        // SAFETY: span guarantee.
        unsafe {
            let ov = *over.add(i);
            *dst.add(i) = blend_scalar(*dst.add(i), ov & 0x00FF_FFFF, a);
        }
        i += 1;
    }
}

/// Read the TSC (benchmark timing only; not a correctness input).
#[inline]
pub fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    // SAFETY: RDTSC is unprivileged; reads are timing-only.
    unsafe {
        asm!(
            "rdtsc",
            out("eax") lo,
            out("edx") hi,
            options(nostack, nomem, preserves_flags)
        );
    }
    ((hi as u64) << 32) | lo as u64
}
