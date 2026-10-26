//! greenlime-wallpaper (task 8): the desktop wallpaper lives in USERSPACE.
//!
//! The build embeds `assets/wallpapers/wallpaper.{png,jpeg,jpg}` (see
//! build.rs; PNG wins if present — drop one there and rebuild); this task
//! decodes it with a pure-Rust decoder (zune-png or zune-jpeg), scales it
//! (nearest neighbor, integer math) to the dims winsrv reports, then
//! streams RGB pixels over IPC: WIN_WALL_SETUP (dims handshake) -> WIN_WALL_ROWS
//! (row-major pixel bands, one <= MAX_PAYLOAD chunk per message,
//! arg0 = first pixel index) -> WIN_WALL_DONE.
//!
//! The kernel never parses image data — winsrv only converts + blits.
//! One correctness clamp: on any pixel where ALL channels are >= 239 they
//! are capped at 238, so the white mouse cursor (240,240,240) stays the
//! only >=239 blob on screen — the input/tablet tests find the cursor by
//! exactly that predicate. Visually imperceptible on real wallpapers.
//!
//! Why the big bss heap: decode output (up to ~3 B * W * H) plus zune's
//! work buffers live here; the kernel-side heap is untouched.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use core::arch::asm;
use core::panic::PanicInfo;
use greenlime_abi::ipc::{MAX_PAYLOAD, Message, PORT_WINSRV, REPLY_OK};
use greenlime_abi::syscall::Syscall;
use greenlime_abi::win::{WIN_WALL_DONE, WIN_WALL_ROWS, WIN_WALL_SETUP};
use linked_list_allocator::LockedHeap;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_core::result::DecodingResult;
use zune_jpeg::JpegDecoder;
use zune_png::PngDecoder;

/// The wallpaper image (build.rs pins the path + its format).
static WALL: &[u8] = include_bytes!(env!("GREENLIME_WALLPAPER"));

/// Heap for the decoder: 16 MiB bss (zero-filled by the ELF loader).
const HEAP_SIZE: usize = 16 * 1024 * 1024;
static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];

#[global_allocator]
static ALLOC: LockedHeap = LockedHeap::empty();

/// A decoded source image: RGB triples ready for the scaler.
struct Decoded {
    w: usize,
    h: usize,
    rgb: Vec<u8>,
}

/// Extract RGB triples from zune's decode output. PNG output follows the
/// SOURCE format (RGB/RGBA/palette/luma) so whatever came back is
/// normalized here; 16-bit samples downshift to 8-bit. Memory-lean: one
/// exact-size output allocation, no intermediate sample copies (the 16 MiB
/// bss heap is tight against the decode buffer + zlib windows).
fn to_rgb3(res: DecodingResult, w: usize, h: usize, comp: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 3);
    match res {
        DecodingResult::U8(src) => {
            for px in src.chunks_exact(comp.max(1)) {
                let (r, g, b) = if comp < 3 {
                    (px[0], px[0], px[0])
                } else {
                    (px[0], px[1], px[2])
                };
                out.extend_from_slice(&[r, g, b]);
            }
        }
        DecodingResult::U16(src) => {
            for px in src.chunks_exact(comp.max(1)) {
                let (r, g, b) = if comp < 3 {
                    let v = (px[0] >> 8) as u8;
                    (v, v, v)
                } else {
                    ((px[0] >> 8) as u8, (px[1] >> 8) as u8, (px[2] >> 8) as u8)
                };
                out.extend_from_slice(&[r, g, b]);
            }
        }
        _ => {}
    }
    out
}

/// Decode the build-pinned asset with the decoder build.rs selected
/// (GREENLIME_WALL_FMT). Returns dims + RGB triples; None on any decode
/// failure (caller logs + exits). All unsafe/intrinsic paths in the
/// decoders are OFF — the scheduler swaps no FP state.
fn decode_wall() -> Option<Decoded> {
    match env!("GREENLIME_WALL_FMT") {
        "png" => {
            let mut dec = PngDecoder::new_with_options(
                ZCursor::new(WALL),
                DecoderOptions::default().set_use_unsafe(false),
            );
            dec.decode_headers().ok()?;
            let (w, h) = dec.dimensions()?;
            let comp = dec.colorspace().unwrap_or(ColorSpace::RGB).num_components();
            // Heap budget: decode buffer (output_buffer_size already
            // factors the bit depth) PLUS our re-packed RGB3 must fit the
            // 16 MiB bss heap together with zune's inflate windows.
            let src = dec.output_buffer_size().unwrap_or(usize::MAX);
            if src.saturating_add(w.checked_mul(h)?.saturating_mul(3)) > 13 * 1024 * 1024 {
                log("[wall] wallpaper too large for the decode heap");
                exit(2);
            }
            let res = dec.decode().ok()?;
            let rgb = to_rgb3(res, w, h, comp);
            Some(Decoded { w, h, rgb })
        }
        // JPEG (zune-jpeg): decode_headers first, then decode — same
        // order as before; output colorspace forced to RGB so grayscale
        // sources expand cleanly.
        _ => {
            let opts = DecoderOptions::default()
                .jpeg_set_out_colorspace(ColorSpace::RGB)
                .set_use_unsafe(false);
            let mut dec = JpegDecoder::new_with_options(ZCursor::new(WALL), opts);
            dec.decode_headers().ok()?;
            let info = dec.info()?;
            let (w, h) = (info.width as usize, info.height as usize);
            let rgb = dec.decode().ok()?;
            Some(Decoded { w, h, rgb })
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    exit(1)
}

/// Raw syscall: number in RAX, args in RDI/RSI/RDX (same ABI as demo).
#[inline(always)]
unsafe fn syscall(nr: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            out("rcx") _,
            out("r11") _,
            out("r10") _,
            options(nostack, preserves_flags),
        );
    }
    ret
}

fn log(msg: &str) {
    unsafe {
        syscall(
            Syscall::Log as u64,
            msg.as_ptr() as u64,
            msg.len() as u64,
            0,
        )
    };
}

fn exit(code: u64) -> ! {
    unsafe { syscall(Syscall::Exit as u64, code, 0, 0) };
    loop {
        unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}

fn getpid() -> u32 {
    unsafe { syscall(Syscall::GetPid as u64, 0, 0, 0) as u32 }
}

fn send(port: u32, msg: &Message) {
    unsafe {
        syscall(
            Syscall::IpcSend as u64,
            port as u64,
            msg as *const Message as u64,
            0,
        );
    };
}

fn recv(port: u32, msg: &mut Message) {
    unsafe {
        syscall(
            Syscall::IpcRecv as u64,
            port as u64,
            msg as *mut Message as u64,
            0,
        );
    };
}

/// Park forever: streaming is boot-time work; afterwards the task idles
/// like the paced fuzzer instead of spinning. (Future: a reload path.)
fn park() -> ! {
    loop {
        unsafe { syscall(Syscall::Park as u64, 0, 0, 0) };
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    unsafe {
        // Once, before any allocation. Static region spills over into bss.
        ALLOC.lock().init(
            core::ptr::addr_of!(HEAP).cast_mut().cast::<u8>(),
            HEAP_SIZE,
        );
    }
    let me = getpid();
    log("[wall] userspace wallpaper service starting");

    // 1. Decode (format follows build.rs's asset selection). Both decoders
    //    are pure Rust with unsafe/intrinsic paths off (no FP state swaps).
    let Some(d) = decode_wall() else {
        log("[wall] decode failed (bad wallpaper asset?)");
        exit(2);
    };
    let (sw, sh) = (d.w, d.h);
    if sw == 0 || sh == 0 {
        log("[wall] zero-size decoded image");
        exit(2);
    }
    let px = d.rgb;
    if px.len() < sw * sh * 3 {
        log("[wall] decoded buffer too small");
        exit(2);
    }
    log(alloc::format!(
        "[wall] decoded {}x{} ({})",
        sw,
        sh,
        env!("GREENLIME_WALL_FMT")
    )
    .as_str());

    // 2. Handshake: setup carries our source dims; winsrv replies the
    //    target dims packed arg0 = w | (h << 32).
    let mut m = Message::new(WIN_WALL_SETUP);
    m.arg0 = sw as u64;
    m.arg1 = sh as u64;
    send(PORT_WINSRV, &m);
    let mut rep = Message::new(0);
    recv(me, &mut rep);
    if rep.sender != 2 || rep.kind != REPLY_OK {
        // winsrv=2 (spawn order in servers::init); a foreign sender means
        // the IPC path lied — fail loud, never continue on lies.
        exit(3);
    }
    let (tw, th) = (
        (rep.arg0 & 0xFFFF_FFFF) as usize,
        (rep.arg0 >> 32) as usize,
    );
    if tw == 0 || th == 0 {
        log("[wall] winsrv rejected/zero target dims");
        exit(3);
    }

    // 3. Scale + clamp + stream. Each message = a run of consecutive
    //    row-major RGB triples starting at pixel index arg0.
    let total = tw * th;
    let chunk = MAX_PAYLOAD / 3; // pixels per message
    let mut sent = 0usize;
    let mut msgs = 0usize;
    let mut band = [0u8; MAX_PAYLOAD];
    while sent < total {
        let n = chunk.min(total - sent);
        for (i, o) in (0..n).zip((0..n).map(|i| i * 3)) {
            let dst = sent + i;
            let (dy, dx) = (dst / tw, dst % tw);
            let sx = dx * sw / tw; // nearest neighbor
            let sy = dy * sh / th;
            let sp = (sy * sw + sx) * 3;
            let (r, g, b) = (px[sp], px[sp + 1], px[sp + 2]);
            // Cursor-uniqueness clamp (all-white pixels only).
            band[o] = if r >= 239 && g >= 239 && b >= 239 {
                r.min(238)
            } else {
                r
            };
            band[o + 1] = if r >= 239 && g >= 239 && b >= 239 {
                g.min(238)
            } else {
                g
            };
            band[o + 2] = if r >= 239 && g >= 239 && b >= 239 {
                b.min(238)
            } else {
                b
            };
        }
        let mut m2 = Message::new(WIN_WALL_ROWS);
        m2.arg0 = sent as u64;
        m2.set_payload(&band[..n * 3]);
        send(PORT_WINSRV, &m2);
        sent += n;
        msgs += 1;
    }

    // 4. Done marker (winsrv prints the proof line + invalidates all).
    send(PORT_WINSRV, &Message::new(WIN_WALL_DONE));
    log(alloc::format!("[wall] wallpaper streamed ({} msgs)", msgs).as_str());
    park()
}
