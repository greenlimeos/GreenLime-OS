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
    req.set_payload(b"Demo - limeclient");
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
