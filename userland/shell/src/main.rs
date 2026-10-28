//! greenlime-shell: interactive builtin shell over IPC.
//! With the GUI terminal (`term0`) it reads canonical lines straight from
//! the keyboard (dev OP_READ -> winsrv WIN_READ) and mirrors all output to
//! `serial0` so test harnesses can assert on what actually ran. Headless
//! (serial console only, no input path) it falls back to the scripted demo.
//! All output goes through the DEV server; all files through the VFS server.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;
use greenlime_abi::ipc::{Message, MAX_PAYLOAD, PORT_DEV, PORT_VFS, REPLY_OK};
use greenlime_abi::net::{self as netab, PORT_NET};
use greenlime_abi::syscall::Syscall;
use greenlime_abi::vfs::*;
use greenlime_tls::fetch as tlsf;
use linked_list_allocator::LockedHeap;

/// The M20 heap: rustls + webpki chain parsing allocates (client config,
/// certificate chains, the response header stage). Zero-filled bss,
/// initialized at _start before the first allocation - same pattern as
/// the wallpaper service.
const HEAP_SIZE: usize = 4 * 1024 * 1024;
static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];

#[global_allocator]
static ALLOC: LockedHeap = LockedHeap::empty();

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

fn rpc(port: u32, me: u32, req: &Message, expect_from: u32) -> Message {
    send(port, req);
    let mut rep = Message::new(0);
    recv(me, &mut rep);
    // The kernel stamps sender; a foreign message means the IPC path lied
    // (wrong mailbox/misroute). Fail loud, never continue on lies.
    if rep.sender != expect_from {
        exit(3);
    }
    rep
}

fn vfs_open(me: u32, path: &[u8]) -> Option<u64> {
    let mut req = Message::new(OP_OPEN);
    req.set_payload(path);
    let rep = rpc(PORT_VFS, me, &req, 0);
    if rep.kind == REPLY_OK {
        Some(rep.arg0)
    } else {
        None
    }
}

fn vfs_read(me: u32, handle: u64, offset: u64, out: &mut Message) -> usize {
    let mut req = Message::new(OP_READ);
    req.arg0 = handle;
    req.arg1 = offset;
    send(PORT_VFS, &req);
    recv(me, out);
    if out.sender != 0 {
        exit(3);
    }
    if out.kind != REPLY_OK {
        return 0;
    }
    out.len as usize
}

fn dev_open(me: u32) -> u64 {
    // Prefer the GUI terminal; fall back to the serial console.
    for name in [&b"term0"[..], &b"serial0"[..]] {
        let mut req = Message::new(OP_OPEN);
        req.set_payload(name);
        let rep = rpc(PORT_DEV, me, &req, 1);
        if rep.kind == REPLY_OK {
            return rep.arg0;
        }
    }
    exit(2);
}

fn assert_reply_ok(rep: &Message, what: &[u8]) {
    if rep.kind != REPLY_OK {
        // Best-effort: we have no stderr, just exit loudly non-zero.
        let _ = what;
        exit(2);
    }
}

fn dev_write(me: u32, fd: u64, mut data: &[u8]) {
    while !data.is_empty() {
        let n = data.len().min(240);
        let mut req = Message::new(OP_WRITE);
        req.arg0 = fd;
        req.set_payload(&data[..n]);
        let rep = rpc(PORT_DEV, me, &req, 1);
        assert_reply_ok(&rep, b"dev write");
        data = &data[n..];
    }
}

/// Blocking console read (term0: waits for a typed line). None = EOF.
fn dev_read(me: u32, fd: u64, buf: &mut [u8]) -> Option<usize> {
    let mut req = Message::new(OP_READ);
    req.arg0 = fd;
    send(PORT_DEV, &req);
    let mut rep = Message::new(0);
    recv(me, &mut rep);
    if rep.sender != 1 {
        exit(3); // forged/foreign reply: never continue on lies
    }
    if rep.kind != REPLY_OK {
        return None;
    }
    let n = (rep.len as usize).min(buf.len());
    buf[..n].copy_from_slice(&rep.payload[..n]);
    Some(n)
}

/// Interactive output: terminal + serial mirror (harness ground truth).
fn out(me: u32, fd: u64, ser: Option<u64>, data: &[u8]) {
    dev_write(me, fd, data);
    if let Some(s) = ser {
        dev_write(me, s, data);
    }
}

/// Blocking canonical line read. One OS read can return several typed-ahead
/// lines, so unread bytes carry over in `acc` (`*bn` valid, `*pos` pending).
/// Returns the line length without the trailing '\n'. None = stdin gone.
fn read_line(
    me: u32,
    fd: u64,
    acc: &mut [u8; 512],
    bn: &mut usize,
    pos: &mut usize,
    line: &mut [u8],
) -> Option<usize> {
    loop {
        // 1) An unprocessed '\n' ends the line.
        if let Some(p) = acc[*pos..*bn].iter().position(|&b| b == b'\n') {
            let n = p.min(line.len());
            line[..n].copy_from_slice(&acc[*pos..*pos + n]);
            *pos += p + 1;
            return Some(n);
        }
        // 2) Compact consumed bytes, then block for more input.
        if *pos > 0 {
            let rest = *bn - *pos;
            acc.copy_within(*pos..*bn, 0);
            *bn = rest;
            *pos = 0;
        }
        let mut chunk = [0u8; 240];
        let n = dev_read(me, fd, &mut chunk)?;
        if n == 0 {
            return None;
        }
        let add = n.min(acc.len() - *bn);
        acc[*bn..*bn + add].copy_from_slice(&chunk[..add]);
        *bn += add;
        if *bn >= acc.len() {
            // 512 bytes without a newline: give up rather than wedge.
            let n = (*bn - *pos).min(line.len());
            line[..n].copy_from_slice(&acc[*pos..*pos + n]);
            *pos = *bn;
            return Some(n);
        }
    }
}

// ===== Phase D net tools (ABI v4 over the port-18 broker) =====

/// Task id of the net broker (spawn order = always 6; kernel-stamped).
const NET_TASK: u32 = 5; // M18: demo left boot; net took its slot (was 6)

/// Send a request to the broker and take its reply. Kernel stamps the
/// sender; a foreign reply means the IPC path lied — die loud.
fn net_req(me: u32, req: &Message) -> Message {
    send(PORT_NET, req);
    let mut rep = Message::new(0);
    recv(me, &mut rep);
    if rep.sender != NET_TASK {
        exit(3);
    }
    rep
}

/// Resolve a hostname. None = broker said no (loud, no retry).
fn net_resolve(me: u32, name: &[u8]) -> Option<u32> {
    let mut req = Message::new(netab::NET_RESOLVE);
    if req.set_payload(name) < name.len() {
        return None; // 240 B cap; a hostname never gets near it
    }
    let rep = net_req(me, &req);
    (rep.kind == netab::NET_REPLY && rep.arg0 != u64::MAX)
        .then(|| netab::addr_ip(rep.arg0))
}

/// One ICMP echo round-trip. Returns the RTT in ms.
fn net_ping(me: u32, ip: u32) -> Option<u64> {
    let mut req = Message::new(netab::NET_PING);
    req.arg1 = netab::addr(ip, 0);
    let rep = net_req(me, &req);
    (rep.kind == netab::NET_REPLY && rep.arg0 != u64::MAX).then_some(rep.arg0)
}

/// ping <host>: DNS resolve via the broker, then an ICMP echo owned by
/// the broker's ICMP socket. Prints the same two lines a real ping does.
fn ping(me: u32, fd: u64, ser: Option<u64>, host: &[u8]) {
    out(me, fd, ser, b"ping: resolving ");
    out(me, fd, ser, host);
    out(me, fd, ser, b"\n");
    let Some(ip) = net_resolve(me, host) else {
        out(me, fd, ser, b"ping: resolve failed\n");
        return;
    };
    let mut b = [0u8; 64];
    let mut n = 0usize;
    w_cmd(&mut b, &mut n, b"ping: ");
    w_ip(&mut b, &mut n, host);
    w_cmd(&mut b, &mut n, b" -> ");
    w_ipq(&mut b, &mut n, ip);
    w_cmd(&mut b, &mut n, b"\n");
    out(me, fd, ser, &b[..n]);

    let Some(rtt) = net_ping(me, ip) else {
        out(me, fd, ser, b"ping: no reply in 5s\n");
        return;
    };
    n = 0;
    w_cmd(&mut b, &mut n, b"ping: reply from ");
    w_ipq(&mut b, &mut n, ip);
    w_cmd(&mut b, &mut n, b" in ");
    w_dec(&mut b, &mut n, rtt);
    w_cmd(&mut b, &mut n, b"ms\n");
    out(me, fd, ser, &b[..n]);
}

/// fetch <url> — scheme-aware gateway (M20). `fetch host` and
/// `fetch http://..` stay on the plain HTTP lane; `fetch https://..` is
/// the M20 lane: TLS 1.3 with REAL webpki chain validation before a
/// single body byte moves. Redirects are chased above the lanes (both
/// belong to the shell, which knows the scheme it is on): 5 hops max,
/// never downward from https to http.
fn fetch(me: u32, fd: u64, ser: Option<u64>, arg: &[u8]) {
    fetch_hop(me, fd, ser, arg, 5);
}

/// One scheme-parsed hop. Recursion depth is taken as `hops` on purpose:
/// Location chains walk here, one shell call per hop.
fn fetch_hop(me: u32, fd: u64, ser: Option<u64>, arg: &[u8], hops: u8) {
    if hops == 0 {
        out(me, fd, ser, b"fetch: too many redirects\n");
        return;
    }
    // Scheme parse. Without one, it is plain http-80 (the historical lane).
    let (tls, rest) = if arg.len() >= 8 && arg[..8].eq_ignore_ascii_case(b"https://") {
        (true, &arg[8..])
    } else if arg.len() >= 7 && arg[..7].eq_ignore_ascii_case(b"http://") {
        (false, &arg[7..])
    } else {
        (false, arg)
    };
    // host[:port][/path] split; the path keeps its leading slash.
    let (host_raw, path) = match rest.iter().position(|&b| b == b'/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, b"/".as_slice()),
    };
    let (host, port) = match split_host_port(host_raw, if tls { 443 } else { 80 }) {
        (h, Some(p)) => (h, p),
        _ => {
            out(me, fd, ser, b"fetch: bad port\n");
            return;
        }
    };
    if tls {
        fetch_https(me, fd, ser, host, port, path, hops);
    } else {
        fetch_http(me, fd, ser, host, port, path, hops);
    }
}

/// host[:port] split. Port must be all digits (1..=5 of them); anything
/// else is not a port and gets refused before DNS ever sees it.
fn split_host_port(host: &[u8], dflt: u16) -> (&[u8], Option<u16>) {
    match host.iter().position(|&b| b == b':') {
        None => (host, Some(dflt)),
        Some(i) => {
            let p = &host[i + 1..];
            if p.is_empty() || p.len() > 5 || !p.iter().all(|&c| (b'0'..=b'9').contains(&c)) {
                return (host, None);
            }
            let mut v = 0u16;
            for &d in p {
                v = v * 10 + (d - b'0') as u16;
            }
            (&host[..i], Some(v))
        }
    }
}

/// The M20 lane: one TLS 1.3 GET with REAL certificate validation, then
/// the redirect chase. The body streams with the 512-B console budget
/// like the plain lane; every byte is counted either way.
fn fetch_https(
    me: u32,
    fd: u64,
    ser: Option<u64>,
    host: &[u8],
    port: u16,
    path: &[u8],
    hops: u8,
) {
    out(me, fd, ser, b"fetch: resolving ");
    out(me, fd, ser, host);
    out(me, fd, ser, b"\n");
    // SNI law: the certificate identity is a DNS name. An IP literal has
    // no name to verify, so the lane refuses it honestly.
    let ip_lit = !host.is_empty()
        && host.iter().all(|&c| (b'0'..=b'9').contains(&c) || c == b'.');
    if ip_lit {
        out(me, fd, ser, b"fetch: https: no IP literals (SNI needs a DNS name)\n");
        return;
    }
    let Some(ip) = net_resolve(me, host) else {
        out(me, fd, ser, b"fetch: resolve failed\n");
        return;
    };
    let Some(h) = net_open_connect(me, fd, ser, ip, port) else {
        return;
    };
    let mut link = FetchLink { me, h };
    let mut shown = 0usize;
    let mut sink = |chunk: &[u8]| body_sink(me, fd, ser, chunk, &mut shown);
    let r = tlsf::https_get(&mut link, host, path, &mut sink);
    match r {
        Ok(sum) => {
            // Proof line: tag, status line (byte-for-byte), total wire
            // bytes, and the TLS-close kind — rustls close_notify vs a
            // bare TCP FIN. Nothing about the channel is hidden.
            let mut b = [0u8; 192];
            let mut n = 0usize;
            w_cmd(&mut b, &mut n, b"fetch: TLS1.3 ");
            w_cmd(&mut b, &mut n, &sum.status_line);
            w_cmd(&mut b, &mut n, b" ");
            w_dec(&mut b, &mut n, (sum.header_bytes + sum.body_bytes) as u64);
            w_cmd(&mut b, &mut n, b"B ");
            w_cmd(
                &mut b,
                &mut n,
                if sum.clean_tls_close { b"close_notify" } else { b"tcp-fin" },
            );
            w_cmd(&mut b, &mut n, b"\n");
            out(me, fd, ser, &b[..n]);
            close_h(me, h);
            let redir = if sum.status >= 300 && sum.status < 400 {
                sum.location.as_deref()
            } else {
                None
            };
            maybe_chase(me, fd, ser, redir, host, port, true, hops);
        }
        Err(e) => {
            print_fetch_err(me, fd, ser, &e);
            close_h(me, h);
        }
    }
}

/// Open a client TCP socket via the broker and connect it to ip:port.
/// Prints and closes on failure; hands back the handle on success.
fn net_open_connect(me: u32, fd: u64, ser: Option<u64>, ip: u32, port: u16) -> Option<u64> {
    let mut req = Message::new(netab::NET_OPEN);
    req.arg0 = netab::NET_KIND_TCP;
    req.arg1 = 0; // client socket: broker assigns an ephemeral port
    let rep = net_req(me, &req);
    if rep.kind != netab::NET_REPLY || rep.arg0 == u64::MAX {
        out(me, fd, ser, b"fetch: open refused\n");
        return None;
    }
    let h = rep.arg0;
    let mut req = Message::new(netab::NET_CONNECT);
    req.arg0 = h;
    req.arg1 = netab::addr(ip, port);
    let rep = net_req(me, &req);
    if rep.kind != netab::NET_REPLY || rep.arg0 != 0 {
        let mut b = [0u8; 72];
        let mut n = 0usize;
        w_cmd(&mut b, &mut n, b"fetch: connect refused (");
        w_ipq(&mut b, &mut n, ip);
        w_cmd(&mut b, &mut n, b":");
        w_dec(&mut b, &mut n, port as u64);
        w_cmd(&mut b, &mut n, b")\n");
        out(me, fd, ser, &b[..n]);
        close_h(me, h);
        return None;
    }
    Some(h)
}

/// Console body budget shared by both lanes: first 512 bytes straight
/// in, everything else silently counted.
fn body_sink(me: u32, fd: u64, ser: Option<u64>, chunk: &[u8], shown: &mut usize) {
    if *shown < 512 {
        let take = chunk.len().min(512 - *shown);
        out(me, fd, ser, &chunk[..take]);
        *shown += take;
    }
}

/// Redirect chase law: 3xx + Location hops at the NEW scheme when hops
/// remain. https -> http is refused (never downgrade); http -> https is
/// the accepted upgrade (that is the acceptance target). Full URLs
/// replay scheme+host+path as-is; a site-relative Location (/new or
/// //host/new) replays under the current host with the chosen scheme;
/// anything else is refused loudly.
fn maybe_chase(
    me: u32,
    fd: u64,
    ser: Option<u64>,
    redir: Option<&[u8]>,
    host: &[u8],
    port: u16,
    came_tls: bool,
    hops: u8,
) {
    let Some(loc) = redir else { return };
    if hops == 0 {
        out(me, fd, ser, b"fetch: redirect chain too long\n");
        return;
    }
    if loc.is_empty() {
        out(me, fd, ser, b"fetch: redirect: empty Location\n");
        return;
    }
    let dflt_port = if came_tls { 443 } else { 80 };
    // Site-relative or protocol-relative: rebuild scheme://host[:port]...
    let rebuild = |loc2: &[u8]| {
        let mut arg = [0u8; 256];
        let mut n = 0usize;
        w_cmd(
            &mut arg,
            &mut n,
            if came_tls { b"https://" } else { b"http://" },
        );
        w_cmd(&mut arg, &mut n, host);
        if port != dflt_port || loc2[..2].eq_ignore_ascii_case(b"//") {
            w_cmd(&mut arg, &mut n, b":");
            w_dec(&mut arg, &mut n, port as u64);
        }
        // protocol-relative passes the leading //; site-relative passes
        // the /path straight (double slashes would break the router,
        // so trim one span).
        let trimmed = if loc2.starts_with(b"//") { &loc2[1..] } else { loc2 };
        w_cmd(&mut arg, &mut n, if trimmed.is_empty() { b"/" } else { trimmed });
        fetch_hop(me, fd, ser, &arg[..n], hops - 1);
    };
    if loc.len() >= 8 && loc[..8].eq_ignore_ascii_case(b"https://") {
        fetch_hop(me, fd, ser, loc, hops - 1);
    } else if loc.len() >= 7 && loc[..7].eq_ignore_ascii_case(b"http://") {
        if came_tls {
            out(me, fd, ser, b"fetch: https -> http refused (no downgrade)\n");
            return;
        }
        fetch_hop(me, fd, ser, loc, hops - 1);
    } else if loc.first() == Some(&b'/') {
        rebuild(loc);
    } else {
        out(me, fd, ser, b"fetch: redirect: unsupported Location form\n");
    }
}

/// The refusal voice of the TLS lane, quoted raw: whoever reads the
/// console sees exactly why the chain did not verify.
fn print_fetch_err(me: u32, fd: u64, ser: Option<u64>, e: &tlsf::FetchErr) {
    let mut b = [0u8; 256];
    let mut n = 0usize;
    w_cmd(&mut b, &mut n, b"fetch: https refused: ");
    let m: &[u8] = match e {
        tlsf::FetchErr::Setup(s) => s.as_bytes(),
        tlsf::FetchErr::Tcp => b"tcp: broker refused the socket",
        tlsf::FetchErr::Tls(s) => s.as_bytes(),
        tlsf::FetchErr::Http(s) => s.as_bytes(),
    };
    w_cmd(&mut b, &mut n, m);
    w_cmd(&mut b, &mut n, b"\n");
    out(me, fd, ser, &b[..n]);
}

/// The M20 transport adapter: greenlime-tls sees a `TcpLink`; the shell
/// turns every call into the broker's wire protocol. Send splits into
/// 2048-B broker messages (reply says how many bytes entered the
/// socket); recv holds ONE parked NET_RECV per slot — the broker's hard
/// law — where NET_EOF means the peer closed and the buffer drained.
struct FetchLink {
    me: u32,
    h: u64,
}

impl tlsf::TcpLink for FetchLink {
    fn send(&mut self, data: &[u8]) -> Result<(), ()> {
        let mut off = 0usize;
        while off < data.len() {
            let take = (data.len() - off).min(MAX_PAYLOAD);
            let mut req = Message::new(netab::NET_SEND);
            req.arg0 = self.h;
            req.set_payload(&data[off..off + take]);
            let rep = net_req(self.me, &req);
            if rep.kind != netab::NET_REPLY || rep.arg0 == u64::MAX || rep.arg0 == 0 {
                return Err(());
            }
            off += rep.arg0 as usize;
        }
        Ok(())
    }

    fn recv(&mut self, buf: &mut [u8]) -> tlsf::Recv {
        let mut req = Message::new(netab::NET_RECV);
        req.arg0 = self.h;
        let rep = net_req(self.me, &req);
        if rep.kind == netab::NET_EOF {
            return tlsf::Recv::Fin;
        }
        if rep.kind != netab::NET_REPLY {
            return tlsf::Recv::Broken;
        }
        let chunk = rep.payload_bytes();
        let n = chunk.len().min(buf.len());
        buf[..n].copy_from_slice(&chunk[..n]);
        tlsf::Recv::Data(n)
    }
}

/// fetch <host> HTTP lane: the pre-M20 plain lane, now with the scheme-
/// aware redirect chase. User-Agent: GreenLime/1.0 — this is the string
/// an OS gets (or doesn't get) counted by.
fn fetch_http(
    me: u32,
    fd: u64,
    ser: Option<u64>,
    host: &[u8],
    port: u16,
    path: &[u8],
    hops: u8,
) {
    out(me, fd, ser, b"fetch: resolving ");
    out(me, fd, ser, host);
    out(me, fd, ser, b"\n");
    let Some(ip) = net_resolve(me, host) else {
        out(me, fd, ser, b"fetch: resolve failed\n");
        return;
    };
    let Some(h) = net_open_connect(me, fd, ser, ip, port) else {
        return;
    };

    // 2. One 240-B-capped request.
    let mut req = [0u8; 240];
    let mut rn = 0usize;
    w_cmd(&mut req, &mut rn, b"GET ");
    w_cmd(&mut req, &mut rn, path);
    w_cmd(&mut req, &mut rn, b" HTTP/1.1\r\nHost: ");
    if rn + host.len() >= 200 {
        out(me, fd, ser, b"fetch: host name too long\n");
        close_h(me, h);
        return;
    }
    w_cmd(&mut req, &mut rn, host);
    w_cmd(
        &mut req,
        &mut rn,
        b"\r\nUser-Agent: GreenLime/1.0 (ring-3 fetch)\r\nConnection: close\r\n\r\n",
    );
    let mut reqm = Message::new(netab::NET_SEND);
    reqm.arg0 = h;
    reqm.set_payload(&req[..rn]);
    let rep = net_req(me, &reqm);
    if rep.kind != netab::NET_REPLY || rep.arg0 != rn as u64 {
        out(me, fd, ser, b"fetch: send refused\n");
        close_h(me, h);
        return;
    }

    // 3. Pull the stream: print the status line, then the first 512 bytes
    //    of the body (terminal-friendly); count everything. The staged
    //    header block cap is 1 KiB: real 301 redirect blocks (Google's
    //    is ~600 B) must fit whole, or the redirect chase becomes
    //    impossible (the overflow flip is an honest no-chase).
    let mut hdr = [0u8; 1024];
    let mut hl = 0usize;
    let mut body = false;
    let mut body_shown = 0usize;
    let mut total = 0usize;
    let mut redir_ok = false;
    let mut code = 0u16;
    let mut loc = [0u8; 240];
    let mut loc_len = 0usize;
    loop {
        let mut req = Message::new(netab::NET_RECV);
        req.arg0 = h;
        send(PORT_NET, &req);
        let mut rep = Message::new(0);
        recv(me, &mut rep);
        if rep.sender != NET_TASK {
            exit(3);
        }
        if rep.kind == netab::NET_EOF {
            break; // stream end (peer FIN + drained)
        }
        if rep.kind != netab::NET_REPLY {
            out(me, fd, ser, b"fetch: stream broke\n");
            break;
        }
        let chunk = rep.payload_bytes();
        total += chunk.len();
        let mut ci = 0usize; // scan position inside this chunk
        while ci < chunk.len() {
            if body {
                // Dump body bytes (HTML) to the console within the budget;
                // after 512 shown, keep counting silently.
                if body_shown < 512 {
                    let take = (chunk.len() - ci).min(512 - body_shown);
                    out(me, fd, ser, &chunk[ci..ci + take]);
                    body_shown += take;
                    ci += take;
                } else {
                    ci = chunk.len();
                }
            } else {
                if hl < hdr.len() {
                    hdr[hl] = chunk[ci];
                    hl += 1;
                }
                if (hl >= 4
                    && hdr[hl - 4] == b'\r'
                    && hdr[hl - 3] == b'\n'
                    && hdr[hl - 2] == b'\r'
                    && hdr[hl - 1] == b'\n')
                    || hl >= hdr.len()
                {
                    // Header complete only if the separator really ended
                    // it (an overflow flip means we never saw the blank
                    // line: no chase is possible)
                    redir_ok = hl >= 4
                        && hdr[hl - 4] == b'\r'
                        && hdr[hl - 3] == b'\n'
                        && hdr[hl - 2] == b'\r'
                        && hdr[hl - 1] == b'\n';
                    body = true;
                    // Status line = bytes up to the first '\n' ('\r'
                    // trimmed) → "fetch: HTTP/1.1 200 OK".
                    let mut sl = 0usize;
                    for i in 0..hl {
                        if hdr[i] == b'\n' {
                            break;
                        }
                        sl = i + 1;
                    }
                    let mut sco = sl.min(hdr.len());
                    if sco > 0 && hdr[sco - 1] == b'\r' {
                        sco -= 1;
                    }
                    let mut ob = [0u8; 400];
                    let mut on = 0usize;
                    w_cmd(&mut ob, &mut on, b"fetch: ");
                    w_cmd(&mut ob, &mut on, &hdr[..sco]);
                    w_cmd(&mut ob, &mut on, b"\n");
                    out(me, fd, ser, &ob[..on]);
                    // Redirect material: status code + Location value,
                    // both straight from the staged bytes.
                    code = parse_code(&hdr[..sco]);
                    if let Some((ls, le)) = find_location(&hdr[..hl.min(hdr.len())]) {
                        // clamp: a 240-B Location is already absurd
                        loc_len = (le - ls).min(loc.len());
                        loc[..loc_len].copy_from_slice(&hdr[ls..ls + loc_len]);
                    }
                }
                ci += 1;
            }
        }
    }
    let mut b = [0u8; 32];
    let mut n = 0usize;
    w_cmd(&mut b, &mut n, b"fetch: stream end, ");
    w_dec(&mut b, &mut n, total as u64);
    w_cmd(&mut b, &mut n, b" bytes total\n");
    out(me, fd, ser, &b[..n]);
    close_h(me, h);
    let redir = if redir_ok && code >= 300 && code < 400 && loc_len > 0 {
        Some(&loc[..loc_len])
    } else {
        None
    };
    maybe_chase(me, fd, ser, redir, host, port, false, hops);
}

/// Three digits after the first space of a status line; 0 on garbage.
fn parse_code(line: &[u8]) -> u16 {
    let Some(sp) = line.iter().position(|&b| b == b' ') else {
        return 0;
    };
    let rest = &line[sp + 1..];
    if rest.len() < 3 {
        return 0;
    }
    let mut v = 0u16;
    for &d in &rest[..3] {
        if !(b'0'..=b'9').contains(&d) {
            return 0;
        }
        v = v * 10 + (d - b'0') as u16;
    }
    v
}

/// Case-insensitive `Location:` scan over a staged header block; the
/// value runs to that line's CR (the caller's out of the head block).
fn find_location(head: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0usize;
    while i + 9 <= head.len() {
        if head[i + 1..i + 9].eq_ignore_ascii_case(b"ocation:") && head[i] | 0x20 == b'l' {
            let mut j = i + 9;
            while j < head.len() && head[j] == b' ' {
                j += 1;
            }
            let end = head[j..]
                .iter()
                .position(|&c| c == b'\r' || c == b'\n')
                .map(|d| j + d)
                .unwrap_or(head.len());
            return Some((j, end));
        }
        match head[i..].iter().position(|&c| c == b'\n') {
            Some(off) => i += off + 1,
            None => break,
        }
    }
    None
}

/// Net CLOSE (best-effort: the broker frees the handle either way).
fn close_h(me: u32, h: u64) {
    let mut req = Message::new(netab::NET_CLOSE);
    req.arg0 = h;
    let _ = net_req(me, &req);
}

/// Append fixed bytes to a stack log buffer.
fn w_cmd(buf: &mut [u8], n: &mut usize, s: &[u8]) {
    let take = s.len().min(buf.len() - *n);
    buf[*n..*n + take].copy_from_slice(&s[..take]);
    *n += take;
}

/// Append a decimal number.
fn w_dec(buf: &mut [u8], n: &mut usize, v: u64) {
    if v == 0 {
        w_cmd(buf, n, b"0");
        return;
    }
    let mut rev = [0u8; 20];
    let mut r = 0usize;
    let mut x = v;
    while x > 0 {
        rev[r] = b'0' + (x % 10) as u8;
        r += 1;
        x /= 10;
    }
    let mut i = r;
    while i > 0 {
        i -= 1;
        w_cmd(buf, n, &rev[i..i + 1]);
    }
}

/// Append a dotted-quad IPv4.
fn w_ipq(buf: &mut [u8], n: &mut usize, ip: u32) {
    let oct = [(ip >> 24) as u8, (ip >> 16) as u8, (ip >> 8) as u8, ip as u8];
    for (i, &v) in oct.iter().enumerate() {
        if i > 0 {
            w_cmd(buf, n, b".");
        }
        if v >= 100 {
            w_dec(buf, n, (v / 100) as u64);
            w_dec(buf, n, ((v / 10) % 10) as u64);
            w_dec(buf, n, (v % 10) as u64);
        } else if v >= 10 {
            w_dec(buf, n, (v / 10) as u64);
            w_dec(buf, n, (v % 10) as u64);
        } else {
            w_dec(buf, n, v as u64);
        }
    }
}

/// Append raw hostname bytes.
fn w_ip(buf: &mut [u8], n: &mut usize, host: &[u8]) {
    w_cmd(buf, n, host);
}

/// ls <path>: list a directory (no handles needed).
fn ls(me: u32, fd: u64, ser: Option<u64>, path: &[u8]) {
    let mut req = Message::new(OP_READDIR);
    req.arg1 = 0;
    req.set_payload(path);
    let mut off = 0u64;
    loop {
        req.arg1 = off;
        send(PORT_VFS, &req);
        let mut rep = Message::new(0);
        recv(me, &mut rep);
        if rep.sender != 0 {
            exit(3); // forged/foreign message: never continue on lies
        }
        if rep.kind != REPLY_OK {
            out(me, fd, ser, b"ls: not a directory\n");
            return;
        }
        let n = rep.len as usize;
        if n == 0 {
            break;
        }
        let mut chunk = [0u8; 240];
        chunk[..n].copy_from_slice(&rep.payload[..n]);
        out(me, fd, ser, &chunk[..n]);
        off += n as u64;
        if n < 200 {
            break;
        }
    }
}

/// cat <path>: stream a VFS file to the console. Missing file noted.
fn cat(me: u32, fd: u64, ser: Option<u64>, path: &[u8]) {
    let handle = match vfs_open(me, path) {
        Some(h) => h,
        None => {
            // One atomic line (serial proof lines must stay contiguous).
            let mut nb = [0u8; 128];
            let mut nn = 0usize;
            w_cmd(&mut nb, &mut nn, b"cat: not found: ");
            w_cmd(&mut nb, &mut nn, path);
            w_cmd(&mut nb, &mut nn, b"\n");
            out(me, fd, ser, &nb[..nn]);
            return;
        }
    };
    let mut off = 0u64;
    loop {
        let mut rep = Message::new(0);
        let n = vfs_read(me, handle, off, &mut rep);
        if n == 0 {
            break;
        }
        // Copy out before reusing rep (aliasing: read from rep first).
        let mut chunk = [0u8; 240];
        chunk[..n].copy_from_slice(&rep.payload[..n]);
        out(me, fd, ser, &chunk[..n]);
        off += n as u64;
        if n < 200 {
            break; // short read = EOF
        }
    }
    let mut req = Message::new(OP_CLOSE);
    req.arg0 = handle;
    let rep = rpc(PORT_VFS, me, &req, 0);
    assert_reply_ok(&rep, b"vfs close");
}

/// Best-effort disk op by path (OP_CREATE / OP_DELETE / OP_MKDIR).
/// True on REPLY_OK; False on caps denial, missing parent, or disk err.
fn vfs_path_op(me: u32, kind: u32, path: &[u8]) -> bool {
    let mut req = Message::new(kind);
    req.set_payload(path);
    let rep = rpc(PORT_VFS, me, &req, 0);
    rep.kind == REPLY_OK
}

/// `echo text > /disk/file` (M14): replace a file's contents. Overwrite
/// is delete-then-create (always truncates even when the old file was
/// longer), then pwrite text + newline at offset 0 in chunks. Serial
/// prints a deterministic "wrote N bytes -> path" proof line.
fn write_file(me: u32, fd: u64, ser: Option<u64>, text: &[u8], target: &[u8]) {
    if target.is_empty() {
        out(me, fd, ser, b"shell: missing target file\n");
        return;
    }
    // Delete any existing file first ("not found" is fine — fresh file).
    let _ = vfs_path_op(me, OP_DELETE, target);
    if !vfs_path_op(me, OP_CREATE, target) {
        out(me, fd, ser, b"echo: cannot create ");
        out(me, fd, ser, target);
        out(me, fd, ser, b"\n");
        return;
    }
    let handle = match vfs_open(me, target) {
        Some(h) => h,
        None => {
            out(me, fd, ser, b"echo: cannot open ");
            out(me, fd, ser, target);
            out(me, fd, ser, b"\n");
            return;
        }
    };
    let mut off = 0u64;
    // The echo newline rides in the LAST chunk so the kernel proofs a
    // single "[vfs] wrote N bytes" line.
    let mut buf = [0u8; 961];
    if text.is_empty() {
        buf[0] = b'\n';
        req_write(me, handle, 0, &buf[..1], &mut off, fd, ser);
    } else {
        let nc = (text.len() + 959) / 960;
        for (i, chunk) in text.chunks(960).enumerate() {
            if i + 1 == nc {
                buf[..chunk.len()].copy_from_slice(chunk);
                buf[chunk.len()] = b'\n';
                req_write(me, handle, off, &buf[..chunk.len() + 1], &mut off, fd, ser);
            } else {
                req_write(me, handle, off, chunk, &mut off, fd, ser);
            }
        }
    }
    let mut req = Message::new(OP_CLOSE);
    req.arg0 = handle;
    let rep = rpc(PORT_VFS, me, &req, 0);
    assert_reply_ok(&rep, b"vfs close");
    // One atomic console line: "wrote N bytes -> <target>".
    let mut nb = [0u8; 128];
    let mut nn = 0usize;
    w_cmd(&mut nb, &mut nn, b"wrote ");
    w_dec(&mut nb, &mut nn, off);
    w_cmd(&mut nb, &mut nn, b" bytes -> ");
    w_cmd(&mut nb, &mut nn, target);
    w_cmd(&mut nb, &mut nn, b"\n");
    out(me, fd, ser, &nb[..nn]);
}

/// One pwrite round trip; aborts the cell on error.
fn req_write(me: u32, handle: u64, off: u64, data: &[u8], done: &mut u64, fd: u64, ser: Option<u64>) {
    let mut req = Message::new(OP_WRITE);
    req.arg0 = handle;
    req.arg1 = off;
    req.set_payload(data);
    let rep = rpc(PORT_VFS, me, &req, 0);
    if rep.kind != REPLY_OK {
        out(me, fd, ser, b"echo: write failed\n");
        exit(1);
    }
    *done += rep.arg0;
}

/// Byte index of a ` > ` separator (redirection), or None.
fn find_redir(s: &[u8]) -> Option<usize> {
    if s.len() < 3 {
        return None;
    }
    for i in 0..=s.len() - 3 {
        if &s[i..i + 3] == b" > " {
            return Some(i);
        }
    }
    None
}

fn run_line(me: u32, fd: u64, ser: Option<u64>, line: &[u8]) {
    if line == b"exit" {
        out(me, fd, ser, b"bye\n");
        exit(0);
    } else if line == b"help" {
        out(
            me,
            fd,
            ser,
            b"commands: help echo cat ps kill ls exit ping fetch rm mkdir exec; echo x > /disk/f\n",
        );
    } else if line.starts_with(b"echo ") {
        let rest = &line[5..];
        // M14: `echo text > /disk/file` replaces a disk file's contents.
        if let Some(i) = find_redir(rest) {
            write_file(me, fd, ser, trim(&rest[..i]), trim(&rest[i + 2..]));
        } else {
            out(me, fd, ser, rest);
            out(me, fd, ser, b"\n");
        }
    } else if line.starts_with(b"cat ") {
        cat(me, fd, ser, trim(&line[4..]));
    } else if line.starts_with(b"ls ") {
        ls(me, fd, ser, trim(&line[3..]));
    } else if line == b"ls" {
        ls(me, fd, ser, b"/disk");
    } else if line == b"ps" {
        cat(me, fd, ser, b"/ps");
    } else if line.starts_with(b"kill ") {
        match parse_u32(trim(&line[5..])) {
            Some(pid) => {
                let r = unsafe {
                    syscall(Syscall::Kill as u64, pid as u64, 0, 0)
                };
                if r == 0 {
                    out(me, fd, ser, b"killed\n");
                } else {
                    out(me, fd, ser, b"kill: no such task\n");
                }
            }
            None => out(me, fd, ser, b"kill: bad pid\n"),
        }
    } else if line.starts_with(b"rm ") {
        // M14: delete a file or an empty dir (admin-capped server-side).
        let t = trim(&line[3..]);
        let mut nb = [0u8; 96];
        let mut nn = 0usize;
        if t.is_empty() {
            out(me, fd, ser, b"rm: missing path\n");
        } else if vfs_path_op(me, OP_DELETE, t) {
            w_cmd(&mut nb, &mut nn, b"rm: ok ");
            w_cmd(&mut nb, &mut nn, t);
            w_cmd(&mut nb, &mut nn, b"\n");
            out(me, fd, ser, &nb[..nn]);
        } else {
            w_cmd(&mut nb, &mut nn, b"rm: err ");
            w_cmd(&mut nb, &mut nn, t);
            w_cmd(&mut nb, &mut nn, b"\n");
            out(me, fd, ser, &nb[..nn]);
        }
    } else if line.starts_with(b"mkdir ") {
        // M14: create a directory (admin-capped server-side).
        let t = trim(&line[6..]);
        let mut nb = [0u8; 96];
        let mut nn = 0usize;
        if t.is_empty() {
            out(me, fd, ser, b"mkdir: missing path\n");
        } else if vfs_path_op(me, OP_MKDIR, t) {
            w_cmd(&mut nb, &mut nn, b"mkdir: ok ");
            w_cmd(&mut nb, &mut nn, t);
            w_cmd(&mut nb, &mut nn, b"\n");
            out(me, fd, ser, &nb[..nn]);
        } else {
            w_cmd(&mut nb, &mut nn, b"mkdir: err ");
            w_cmd(&mut nb, &mut nn, t);
            w_cmd(&mut nb, &mut nn, b"\n");
            out(me, fd, ser, &nb[..nn]);
        }
    } else if line.starts_with(b"exec ") {
        // M17: run a .gle app from the disk. The admin gate lives in the
        // vfs server; the kernel validates the container, spawns a fresh
        // ring-3 task and replies its id (shown as the task's next proof
        // line on serial — [sched]/[winsrv]/[hello]).
        let t = trim(&line[5..]);
        let mut nb = [0u8; 128];
        let mut nn = 0usize;
        if t.is_empty() {
            out(me, fd, ser, b"exec: missing path\n");
        } else if vfs_path_op(me, OP_EXEC, t) {
            w_cmd(&mut nb, &mut nn, b"exec: started ");
            w_cmd(&mut nb, &mut nn, t);
            w_cmd(&mut nb, &mut nn, b"\n");
            out(me, fd, ser, &nb[..nn]);
        } else {
            w_cmd(&mut nb, &mut nn, b"exec: refused ");
            w_cmd(&mut nb, &mut nn, t);
            w_cmd(&mut nb, &mut nn, b"\n");
            out(me, fd, ser, &nb[..nn]);
        }
    } else if line.starts_with(b"ping ") {
        ping(me, fd, ser, trim(&line[5..]));
    } else if line.starts_with(b"fetch ") {
        fetch(me, fd, ser, trim(&line[6..]));
    } else if line.is_empty() {
    } else {
        out(me, fd, ser, b"unknown command\n");
    }
}

fn trim(mut b: &[u8]) -> &[u8] {
    while let Some((&c, rest)) = b.split_first() {
        if c == b' ' || c == b'\t' {
            b = rest;
        } else {
            break;
        }
    }
    while let Some((&c, _)) = b.split_last() {
        if c == b' ' || c == b'\t' {
            b = &b[..b.len() - 1];
        } else {
            break;
        }
    }
    b
}

fn parse_u32(b: &[u8]) -> Option<u32> {
    if b.is_empty() {
        return None;
    }
    let mut v = 0u32;
    for &c in b {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((c - b'0') as u32)?;
    }
    Some(v)
}

/// Kernel iretq's here in ring 3 with a fresh stack.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    unsafe {
        // Once, before any allocation. Static region spills into bss.
        ALLOC.lock().init(
            core::ptr::addr_of!(HEAP).cast_mut().cast::<u8>(),
            HEAP_SIZE,
        );
    }
    let me = getpid();
    let fd = dev_open(me);
    // dev encodes the console in the handle's top bit (term0 = GUI).
    if fd & (1u64 << 63) != 0 {
        interactive(me, fd);
    }
    scripted(me, fd)
}

/// GUI mode: type commands, get output (also mirrored to serial0).
fn interactive(me: u32, fd: u64) -> ! {
    // Best-effort serial mirror: lets test harnesses assert on real output.
    let mut req = Message::new(OP_OPEN);
    req.set_payload(b"serial0");
    let rep = rpc(PORT_DEV, me, &req, 1);
    let ser = (rep.kind == REPLY_OK).then_some(rep.arg0);

    out(me, fd, ser, b"GreenLime shell (interactive) - type help\n");
    if let Some(s) = ser {
        dev_write(me, s, b"[shell] interactive ready\n");
    }
    let mut acc = [0u8; 512];
    let mut bn = 0usize;
    let mut pos = 0usize;
    let mut line = [0u8; 256];
    loop {
        out(me, fd, ser, b"$ ");
        let Some(n) = read_line(me, fd, &mut acc, &mut bn, &mut pos, &mut line) else {
            if let Some(s) = ser {
                dev_write(me, s, b"[shell] stdin gone\n");
            }
            exit(0);
        };
        run_line(me, fd, ser, trim(&line[..n]));
    }
}

/// Headless fallback: scripted demo (no input path on serial0).
fn scripted(me: u32, fd: u64) -> ! {
    dev_write(me, fd, b"GreenLime shell (scripted)\n");
    for line in [
        &b"help"[..],
        &b"echo hi from GreenLime shell"[..],
        &b"cat /motd.txt"[..],
        &b"cat /hello.txt"[..],
        &b"cat /nope.txt"[..],
        &b"ps"[..],
        &b"ls /disk"[..],
        &b"cat /disk/hello.txt"[..],
        &b"ls /disk/docs"[..],
        &b"cat /disk/docs/note.txt"[..],
        &b"kill 999"[..],
    ] {
        dev_write(me, fd, b"$ ");
        dev_write(me, fd, line);
        dev_write(me, fd, b"\n");
        run_line(me, fd, None, line);
    }
    dev_write(me, fd, b"shell done, exiting\n");
    exit(0);
}
