//! greenlime-demo: the window server's second client (multi-window).
//! Opens its own window via WIN_CREATE, echoes every key it receives while
//! focused (console rules), and mirrors completed lines to serial as
//! `[demo] got: ...` — harnesses prove keyboard focus routing by typing
//! into this window and the terminal and asserting where the lines land.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;
use greenlime_abi::ipc::{Message, PORT_WINSRV, REPLY_OK};
use greenlime_abi::net::{self, PORT_NET};
use greenlime_abi::syscall::Syscall;
use greenlime_abi::win::*;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    exit(1)
}

/// Raw syscall: number in RAX, args in RDI/RSI/RDX. RCX/R11 clobbered by CPU,
/// R10 clobbered by the kernel trampoline; RDI/RSI/RDX are preserved.
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
    unsafe { syscall(Syscall::IpcSend as u64, port as u64, msg as *const Message as u64, 0) };
}

fn recv(port: u32, msg: &mut Message) {
    unsafe {
        syscall(
            Syscall::IpcRecv as u64,
            port as u64,
            msg as *mut Message as u64,
            0,
        )
    };
}

/// Send a request and wait for the winsrv reply (kernel stamps the sender).
fn rpc(me: u32, req: &Message) -> Message {
    send(PORT_WINSRV, req);
    let mut rep = Message::new(0);
    recv(me, &mut rep);
    // vfs=0, dev=1, winsrv=2 (spawn order in servers::init); a foreign
    // sender means the IPC path lied — fail loud, never continue on lies.
    if rep.sender != 2 {
        exit(3);
    }
    rep
}

/// Fire-and-forget window write (WIN_CLIENT_WRITE has no reply).
fn win_write(id: u32, text: &[u8]) {
    let mut m = Message::new(WIN_CLIENT_WRITE);
    m.arg0 = id as u64;
    m.set_payload(text);
    send(PORT_WINSRV, &m);
}

/// Serial line: `[demo] got: <body>`.
fn log_got(body: &[u8]) {
    const P: &[u8] = b"[demo] got: ";
    let mut buf = [0u8; 160];
    let n = P.len().min(buf.len());
    buf[..n].copy_from_slice(&P[..n]);
    let m = body.len().min(buf.len() - n);
    buf[n..n + m].copy_from_slice(&body[..m]);
    // ASCII by construction (filtered on input).
    let s = core::str::from_utf8(&buf[..n + m]).unwrap_or("[demo] got?");
    log(s);
}

/// Kernel iretq's here in ring 3 with a fresh stack.
#[no_mangle]
pub extern "C" fn _start() -> ! {    let me = getpid();
    log("[demo] starting (second window client)");

    // 0. Phase C proof: a REAL UDP socket from ring 3. The demo hand-builds
    //    a DNS query (the kernel's broker just moves bytes for us), sends
    //    it to slirp's resolver, eats the reply datagram, parses the A
    //    record and logs the address — same wire format as any OS client.
    //    Runs BEFORE window RPCs so no winsrv message interleaves with the
    //    net reply stream on our mailbox.
    net_demo(me);

    // 1. Open our window (server picks geometry, hands us keyboard focus).
    let mut req = Message::new(WIN_CREATE);
    req.set_payload(b"GreenLime Demo");
    let rep = rpc(me, &req);
    if rep.kind != REPLY_OK {
        log("[demo] no display, exiting");
        exit(0); // headless boot: nothing to show, but never wedge
    }
    let id = rep.arg0 as u32;
    log("[demo] window open");

    // 2. Banner.
    win_write(
        id,
        b"GreenLime demo client\nkeys typed while this window is\nfocused land here - click to focus\n> ",
    );

    // 2.5 M15: app graphics API demo — the window's content goes
    // graphics (an animated canvas streamed ring-3 -> winsrv), then
    // hands the window back to text mode. Console behavior below is
    // unchanged; keys typed during the animation buffer in the ring
    // and replay on the first read after the canvas releases.
    canvas_intro(me, id);

    // 3. Echo loop: raw keys in (focused only), mirror lines to serial.
    let mut line = [0u8; 64];
    let mut ln = 0usize;
    loop {
        let mut r = Message::new(WIN_CLIENT_READ);
        r.arg0 = id as u64;
        let rep = rpc(me, &r);
        if rep.kind != REPLY_OK {
            log("[demo] window closed, bye");
            exit(0); // window destroyed (X button / server EOF)
        }
        let data = rep.payload_bytes();
        // Echo verbatim into our window ('\n' breaks to a fresh line).
        win_write(id, data);
        for &b in data {
            if b == b'\n' {
                log_got(&line[..ln]);
                ln = 0;
            } else if b >= 0x20 && b < 0x7F && ln < line.len() {
                line[ln] = b;
                ln += 1;
            }
        }
    }
}

// ===== Phase C: ring-3 socket proof through the net broker (port 18) =====
// The demo hand-builds a DNS query (the broker just moves bytes), sends it
// to slirp's resolver, eats the pushed reply datagram, parses the first A
// record and logs the address: real UDP I/O from userspace, same wire
// format as every OS client. Before window RPCs so the net reply stream
// never interleaves with winsrv replies on our mailbox.

const NET_TASK: u32 = 6; // spawn order in servers::init; kernel-stamped
const DNS_PORT: u16 = 53;

/// Send a request to the broker and take back its reply. Kernel stamps the
/// sender; a foreign sender means the IPC path lied — exit loud (1).
fn net_req(me: u32, req: &Message) -> Message {
    unsafe {
        syscall(Syscall::IpcSend as u64, PORT_NET as u64, req as *const Message as u64, 0)
    };
    let mut rep = Message::new(0);
    unsafe { syscall(Syscall::IpcRecv as u64, me as u64, &mut rep as *mut Message as u64, 0) };
    if rep.sender != NET_TASK {
        exit(3);
    }
    rep
}

/// Open a UDP socket (returns handle, u32::MAX on refusal — logged, caller
/// may bail rather than wedge; net down must not kill the desktop demo).
fn net_open(me: u32, local_port: u16) -> u32 {
    let mut req = Message::new(net::NET_OPEN);
    req.arg0 = net::NET_KIND_UDP;
    req.arg1 = local_port as u64;
    let rep = net_req(me, &req);
    if rep.kind != net::NET_REPLY || rep.arg0 == u64::MAX {
        log("[demo] net: open refused");
        u32::MAX
    } else {
        rep.arg0 as u32
    }
}

/// Sendto the broker socket. Returns false when the broker rejected it.
fn net_sendto(me: u32, h: u32, dst: u64, data: &[u8]) -> bool {
    let mut req = Message::new(net::NET_SENDTO);
    req.arg0 = h as u64;
    req.arg1 = dst;
    if req.set_payload(data) < data.len() {
        return false; // >240 B: chunking is a Phase D (TCP) story
    }
    let rep = net_req(me, &req);
    rep.kind == net::NET_REPLY && rep.arg0 == 0
}

/// Wait for a datagram on handle `h`. Blocks on our own mailbox (the broker
/// pushes); passes over stray messages (none by construction at this
/// point of the demo lifecycle) and returns (src_ip, src_port, bytes).
fn net_recv(me: u32, h: u32) -> Option<(u32, u16, [u8; 240], usize)> {
    loop {
        let mut m = Message::new(0);
        unsafe { syscall(Syscall::IpcRecv as u64, me as u64, &mut m as *mut Message as u64, 0) };
        if m.sender != NET_TASK {
            exit(3);
        }
        if m.kind != net::NET_DATAGRAM || m.arg0 != h as u64 {
            continue; // not ours / stray — wait on
        }
        let n = m.len as usize;
        let mut buf = [0u8; 240];
        buf[..n].copy_from_slice(m.payload_bytes());
        return Some((net::addr_ip(m.arg1), net::addr_port(m.arg1), buf, n));
    }
}

/// DNS A-record parse. Returns the first A IN record in the answer
/// section; compression-pointer names are skipped (2 bytes, not chased).
fn dns_first_a(buf: &[u8]) -> Option<u32> {
    if buf.len() < 12 {
        return None;
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let mut i = 12;
    for _ in 0..qd {
        i = skip_name(buf, i)?;
        i = i.checked_add(4)?;
        if i > buf.len() {
            return None;
        }
    }
    for _ in 0..an {
        i = skip_name(buf, i)?;
        if i + 10 > buf.len() {
            return None;
        }
        let rtype = u16::from_be_bytes([buf[i], buf[i + 1]]);
        let rclass = u16::from_be_bytes([buf[i + 2], buf[i + 3]]);
        let rdlen = u16::from_be_bytes([buf[i + 8], buf[i + 9]]) as usize;
        let rdata = i + 10;
        if rtype == 1 && rclass == 1 && rdlen == 4 && rdata + 4 <= buf.len() {
            let o = &buf[rdata..rdata + 4];
            return Some(((o[0] as u32) << 24) | ((o[1] as u32) << 16) | ((o[2] as u32) << 8) | o[3] as u32);
        }
        i = rdata.checked_add(rdlen)?;
    }
    None
}

/// Skip one DNS name: labels to the 0 terminator, or a 2-byte pointer.
fn skip_name(buf: &[u8], mut i: usize) -> Option<usize> {
    loop {
        if i >= buf.len() {
            return None;
        }
        let len = buf[i];
        if len == 0 {
            return Some(i + 1);
        }
        if len & 0xC0 == 0xC0 {
            let end = i + 2;
            if end > buf.len() {
                return None;
            }
            return Some(end);
        }
        if len & 0xC0 != 0 {
            return None;
        }
        i += 1 + len as usize;
    }
}

/// Log `<prefix><ip>` with a dotted-quad formatter (no alloc in ring 3).
fn log_ip(prefix: &str, ip: u32) {
    let mut out = [0u8; 48];
    let mut n = 0usize;
    for &b in prefix.as_bytes() {
        if n < out.len() {
            out[n] = b;
            n += 1;
        }
    }
    let oct = [(ip >> 24) as u8, (ip >> 16) as u8, (ip >> 8) as u8, ip as u8];
    for (idx, &v) in oct.iter().enumerate() {
        if idx > 0 && n < out.len() {
            out[n] = b'.';
            n += 1;
        }
        if v >= 100 {
            put_dec(&mut out, &mut n, v / 100);
            put_dec(&mut out, &mut n, (v / 10) % 10);
        } else if v >= 10 {
            put_dec(&mut out, &mut n, v / 10);
        }
        put_dec(&mut out, &mut n, v % 10);
    }
    // ASCII by construction (digits + dots).
    let s = core::str::from_utf8(&out[..n]).unwrap_or("[demo] net ip?");
    log(s);
}

/// Append a single decimal digit (0..=9 only by construction).
fn put_dec(buf: &mut [u8], n: &mut usize, d: u8) {
    if *n < buf.len() {
        buf[*n] = b'0' + d;
        *n += 1;
    }
}

// ===== M15: animated client graphics through the app canvas API =========
// The demo switches its own window into graphics mode, streams 40 frames
// of pure pixel art through WIN_BLIT_SETUP/WIN_BLIT/WIN_BLIT_END (navy
// gradient field, an azure ridge wave, a lime ball in free flight), then
// releases the canvas and returns to console mode — both modes in one
// window, the whole M15 story in one task. Zero heap: the renderer is a
// pure function of (row, frame), one 1536 B row buffer + one 256 B dirty
// mask on the stack, one 2 KiB Message reused per sent row. Pacing is
// Park (10 ticks/frame): ~7 fps measured on the release build, ~1 s per
// frame on the debug build under TCG.
//
// Proof strings: `[demo] m15: canvas open WxH live`, one
// `[demo] m15: frame N ball X,Y` per frame (doubles as the headless
// test's motion ground truth), `[demo] m15: canvas released`. Canvas
// pixels are clamped so no pixel has all three channels >= 239 — the
// cursor stays the only all-channel >=239 blob on screen.

const CANVAS_W: u32 = 512;
const CANVAS_H_MAX: u32 = 256; // SETUP clamps down to the viewport, never up
const FRAMES: u32 = 40;
const PARKS_PER_FRAME: u32 = 10; // ~10 ticks ≈ 100 ms of pacing per frame
const BALL_R: i32 = 14;

/// Park until the next tick (ABI v5 pacing primitive, fuzzer's tool).
fn park() {
    unsafe { syscall(Syscall::Park as u64, 0, 0, 0) };
}

fn push_str(buf: &mut [u8], n: &mut usize, s: &[u8]) {
    for &b in s {
        if *n < buf.len() {
            buf[*n] = b;
            *n += 1;
        }
    }
}

fn push_u32(buf: &mut [u8], n: &mut usize, v: u32) {
    let mut tmp = [0u8; 10];
    let mut m = 0usize;
    if v == 0 {
        put_dec(&mut tmp, &mut m, 0);
    } else {
        let mut rest = v;
        while rest > 0 {
            put_dec(&mut tmp, &mut m, (rest % 10) as u8);
            rest /= 10;
        }
        // reversed: rewrite forward
        for i in (0..m).rev() {
            if *n < buf.len() {
                buf[*n] = tmp[i];
                *n += 1;
            }
        }
        return;
    }
    for i in (0..m).rev() {
        if *n < buf.len() {
            buf[*n] = tmp[i];
            *n += 1;
        }
    }
}

/// ASCII-by-construction log (digits + fixed prefixes only).
fn log_built(buf: &[u8], n: usize) {
    let s = core::str::from_utf8(&buf[..n]).unwrap_or("[demo] m15: ?");
    log(s);
}

/// Fire-and-forget one canvas row run: `arg1` = FIRST pixel index of the
/// run (row-major across the whole canvas), i.e. `row * cw`.
fn blit_row(id: u32, first_px: u32, bytes: &[u8]) {
    let mut m = Message::new(WIN_BLIT);
    m.arg0 = id as u64;
    m.arg1 = first_px as u64;
    m.set_payload(bytes);
    send(PORT_WINSRV, &m);
}

/// Mark the rows a ball disc touches (old or new position) as dirty.
fn mark_ball(dirty: &mut [bool; CANVAS_H_MAX as usize], by: i32, ch: u32) {
    let r = BALL_R;
    let y0 = (by - r).max(0) as u32;
    let y1 = (by + r).min(ch as i32 - 1) as u32;
    for y in y0..=y1 {
        if (y as usize) < dirty.len() {
            dirty[y as usize] = true;
        }
    }
}

/// The whole M15 story on the demo's own window, then back to console.
/// Park-paced (~10 fps), 40 frames, then a WIN_BLIT_END teardown whose
/// REPLY_OK is the sync point for the text-mode phase that follows.
fn canvas_intro(me: u32, id: u32) {
    // 1. SETUP: ask for 512x256; the server clamps to the viewport and
    //    answers with the ACTUAL dims (trust the reply, not the ask).
    let mut req = Message::new(WIN_BLIT_SETUP);
    req.arg0 = id as u64;
    req.arg1 = CANVAS_W as u64 | ((CANVAS_H_MAX as u64) << 32);
    let rep = rpc(me, &req);
    if rep.kind != REPLY_OK {
        log("[demo] m15: canvas refused, staying on text");
        return;
    }
    let cw = (rep.arg0 & 0xFFFF_FFFF) as u32;
    let ch = (rep.arg0 >> 32) as u32;
    {
        let mut buf = [0u8; 80];
        let mut n = 0usize;
        push_str(&mut buf, &mut n, b"[demo] m15: canvas open ");
        push_u32(&mut buf, &mut n, cw);
        push_str(&mut buf, &mut n, b"x");
        push_u32(&mut buf, &mut n, ch);
        push_str(&mut buf, &mut n, b" live");
        log_built(&buf, n);
    }
    // 2. Animate: ball in free flight (bounce off all four edges), the
    //    wave ridge scrolling underneath. Only dirty rows stream — the
    //    ball band (old + new) plus the always-moving wave band.
    let (mut bx, mut by) = (40i32, 40i32);
    let (mut vx, mut vy) = (7i32, 5i32);
    let mut dirty = [false; CANVAS_H_MAX as usize];
    let mut row = [0u8; 3 * CANVAS_W as usize];
    const WAVE_ROWS: u32 = 24;
    for t in 0..FRAMES {
        for _ in 0..PARKS_PER_FRAME {
            park();
        }
        mark_ball(&mut dirty, by, ch);
        bx += vx;
        if bx < BALL_R {
            bx = BALL_R;
            vx = -vx;
        }
        if bx > cw as i32 - 1 - BALL_R {
            bx = cw as i32 - 1 - BALL_R;
            vx = -vx;
        }
        by += vy;
        if by < BALL_R {
            by = BALL_R;
            vy = -vy;
        }
        if by > ch as i32 - 1 - BALL_R {
            by = ch as i32 - 1 - BALL_R;
            vy = -vy;
        }
        mark_ball(&mut dirty, by, ch);
        if ch >= 60 {
            let y1 = (ch as i32 - 28).max(0) as u32;
            for y in y1.saturating_sub(WAVE_ROWS)..=y1 {
                dirty[y as usize] = true;
            }
        }
        for y in 0..ch {
            if !dirty[y as usize] {
                continue;
            }
            for x in 0..cw {
                // 1. Gradient base (18,22,34) -> (26,31,48), Q8 lerp.
                let dyg = if ch > 1 { (y * 255) / (ch - 1) } else { 0 };
                let mut r = (18 * (255 - dyg) + 26 * dyg) / 255;
                let mut g = (22 * (255 - dyg) + 31 * dyg) / 255;
                let mut b = (34 * (255 - dyg) + 48 * dyg) / 255;
                // 2. Azure ridge wave (3 px thick, scrolling with t) —
                //    a real triangle ridge: rises 0..23, falls back down.
                let wave_base = ch as i32 - 28;
                let dyw = wave_base - y as i32;
                if dyw >= 0 && dyw <= 23 && dyw < 24 {
                    let ph = ((x * 3 + t * 11) % 48) as i32;
                    let hgt = if ph < 24 { ph } else { 47 - ph };
                    if dyw == 0 || (dyw >= 1 && hgt >= dyw && hgt - dyw < 3) {
                        r = 56;
                        g = 126;
                        b = 236;
                    }
                }
                // 3. Ball disc on top (whole disc lime).
                let dx = bx - x as i32;
                let dy2 = by - y as i32;
                if dx * dx + dy2 * dy2 <= BALL_R * BALL_R {
                    r = 170;
                    g = 232;
                    b = 64;
                }
                // Cursor uniqueness: never all three >= 239.
                if r >= 239 && g >= 239 && b >= 239 {
                    r = 238;
                }
                let o = (x * 3) as usize;
                row[o] = r as u8;
                row[o + 1] = g as u8;
                row[o + 2] = b as u8;
            }
            blit_row(id, y * cw, &row[..(cw * 3) as usize]);
            dirty[y as usize] = false;
        }
        // Heartbeat + motion ground truth for qmp_m15_test.py.
        {
            let mut buf = [0u8; 80];
            let mut n = 0usize;
            push_str(&mut buf, &mut n, b"[demo] m15: frame ");
            push_u32(&mut buf, &mut n, t);
            push_str(&mut buf, &mut n, b" ball ");
            push_u32(&mut buf, &mut n, bx as u32);
            push_str(&mut buf, &mut n, b",");
            push_u32(&mut buf, &mut n, by as u32);
            log_built(&buf, n);
        }
    }
    // 3. Teardown (REPLY_OK is the sync point): text mode returns.
    let mut req = Message::new(WIN_BLIT_END);
    req.arg0 = id as u64;
    let rep = rpc(me, &req);
    if rep.kind == REPLY_OK {
        log("[demo] m15: canvas released, text mode");
    } else {
        log("[demo] m15: release failed");
    }
}

/// Build a DNS A query for "google.com" with the demo task's own id byte
/// (so nested runs are tell-apart-able by the id) — 28 bytes, the same
/// shape any stub resolver emits.
fn dns_query(gid: u16) -> [u8; 28] {
    let mut q = [0u8; 28];
    q[0] = (gid >> 8) as u8;
    q[1] = gid as u8;
    q[2] = 0x01; // flags: RD ... bingo — length-true header this time!
    q[3] = 0x00;
    q[4] = 0; q[5] = 1; // qd=1
    q[6] = 0; q[7] = 0; // an=0
    q[8] = 0; q[9] = 0; // ns=0
    q[10] = 0; q[11] = 0; // ar=0
    // qname "google.com" (labels) + type A + class IN
    q[12] = 6;
    q[13..19].copy_from_slice(b"google");
    q[19] = 3;
    q[20..23].copy_from_slice(b"com");
    q[23] = 0;
    q[24] = 0; q[25] = 1; // type A
    q[26] = 0; q[27] = 1; // class IN
    q
}

/// Full ring-3 proof: open -> sendto resolver -> recv datagram -> parse A.
fn net_demo(me: u32) {
    log("[demo] net: ring-3 socket proof");
    let Some(h) = net_open_check(me, 49100) else { return };
    let q = dns_query(0x474C);
    let dst = net::addr(net::ipv4o(10, 0, 2, 3), DNS_PORT);
    if !net_sendto(me, h, dst, &q) {
        log("[demo] net: sendto refused");
        return;
    }
    let (src, port, buf, n) = match net_recv(me, h) {
        Some(x) => x,
        None => return,
    };
    // Reply sanity: our id echoed, QR set, rcode 0.
    if !(n >= 12 && buf[0] == 0x47 && buf[1] == 0x4C && buf[2] & 0x80 != 0 && (buf[3] & 0x0F) == 0) {
        log("[demo] net: bad dns reply");
        return;
    }
    match dns_first_a(&buf[..n]) {
        Some(ip) => {
            net_close_log(me, h);
            log_ip("[demo] net: google.com -> ", ip);
        }
        None => {
            log("[demo] net: resolver sent no A record");
        }
    }
    let _ = (src, port);
}

/// net_open with a rollback-friendly bail type.
fn net_open_check(me: u32, port: u16) -> Option<u32> {
    match net_open(me, port) {
        u32::MAX => None,
        h => Some(h),
    }
}

/// Close with a quiet log (only used at end of the proof).
fn net_close_log(me: u32, h: u32) {
    let mut req = Message::new(net::NET_CLOSE);
    req.arg0 = h as u64;
    let _ = net_req(me, &req);
}
