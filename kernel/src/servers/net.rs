//! Net server (kernel task 6): owns the smoltcp interface (Phase B).
//!
//! Bring-up: DHCP discover on the virtio-net NIC (QEMU user NAT answers),
//! then one ICMP echo to the gateway as proof of a working stack, then a
//! DNS A-record query (google.com via slirp's 10.0.2.3 resolver) and one
//! ICMP echo to the resolved address — the real internet, end to end.
//! After that it polls forever (ARP upkeep etc.). B3 gives it an IRQ.
//! No IPC API yet — Phase C/D wires sockets to clients.
//!
//! Caveats, stated plainly: the DHCP xid/seed comes from rdtsc (not
//! crypto randomness), smoltcp parse paths run under panic=abort, and the
//! hand-rolled DNS parser is deliberately conservative (first A record,
//! answer section only) — hostile peers are a fuzz-target before v1.0
//! (see SECURITY.md).

use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken},
    socket::{dhcpv4, icmp, tcp, udp},
    storage::{PacketBuffer, PacketMetadata},
    time::Instant,
    wire::{
        EthernetAddress, HardwareAddress, IpAddress, IpCidr, Icmpv4Packet, Icmpv4Repr,
        IpEndpoint, IpListenEndpoint,
    },
};

use core::net::Ipv4Addr;

use greenlime_abi::ipc::Message;
use crate::arch::x86_64::idt;
use crate::drivers::virtio_net;
use crate::{kprintln, sched};

pub fn init() {}

/// Millis since boot (100 Hz PIT ticks).
fn now() -> Instant {
    Instant::from_millis(idt::ticks() as i64 * 10)
}

/// Boot-varying seed: KASLR seed (TSC-folded at boot in mm) mixed again
/// with the live TSC. NOT crypto randomness — DHCP xids and ISNs only.
fn seed() -> u64 {
    let mut s = crate::mm::kaslr_seed()
        ^ unsafe { core::arch::x86_64::_rdtsc() }.wrapping_mul(0xff51_ffd7_ed55_9ccd);
    // Final splitmix fold so the high bits vary early-boot too.
    s ^= s >> 33;
    s = s.wrapping_mul(0xff51_ffd7_ed55_9ccd);
    s ^ s >> 33
}

/// Unit device: packets are copied in/out of the driver per call, so no
/// borrows escape and the tokens own their bytes.
struct SmolDev;
struct GlRxToken(alloc::vec::Vec<u8>);
struct GlTxToken;

impl RxToken for GlRxToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl TxToken for GlTxToken {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut buf = alloc::vec![0u8; len];
        let r = f(&mut buf);
        if !virtio_net::send_pkt(&buf) {
            kprintln!("[net] tx drop (device timeout)");
        }
        r
    }
}

impl Device for SmolDev {
    type RxToken<'a> = GlRxToken;
    type TxToken<'a> = GlTxToken;

    fn receive(&mut self, _now: Instant) -> Option<(GlRxToken, GlTxToken)> {
        let mut buf = alloc::vec![0u8; 2048];
        match virtio_net::recv_pkt(&mut buf) {
            Some(n) => {
                buf.truncate(n);
                Some((GlRxToken(buf), GlTxToken))
            }
            None => None,
        }
    }

    fn transmit(&mut self, _now: Instant) -> Option<GlTxToken> {
        Some(GlTxToken)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1536;
        caps
    }
}

/// Poll until `deadline_ms` elapses. Returns the end timestamp basis.
/// Callers inline their own check between polls so borrows never overlap
/// (a `&mut sockets` closure + `&mut sockets` arg would double-borrow).
fn deadline_ms(ms: i64) -> i64 {
    now().total_millis() + ms
}

/// Kernel task entry: DHCP, one gateway ping, then broker forever.
pub unsafe extern "C" fn server_main() -> ! {
    let Some(mac) = virtio_net::mac_addr() else {
        // No NIC (headless/smp test boots): the port still exists, so the
        // broker duty MUST still be honored — fail CLOSED and FAST. A
        // client that blocks on a reply that never comes would deadlock
        // forever; an explicit refusal lets it degrade gracefully.
        kprintln!("[net] no NIC claimed, net services unavailable");
        let me = sched::current_id().unwrap_or(u32::MAX);
        loop {
            while let Some(req) = crate::ipc::try_recv(greenlime_abi::net::PORT_NET) {
                let mut rep = greenlime_abi::ipc::Message::new(greenlime_abi::net::NET_REPLY);
                rep.arg0 = u64::MAX;
                crate::ipc::send(req.sender as u32, rep, me as u32);
            }
            sched::yield_now();
        }
    };
    kprintln!("[net] up, soliciting DHCP...");

    let mut config = Config::new(HardwareAddress::Ethernet(
        EthernetAddress::from_bytes(&mac),
    ));
    config.random_seed = seed();
    let mut device = SmolDev;
    let mut iface = Interface::new(config, &mut device, now());
    let mut sockets = SocketSet::new(alloc::vec::Vec::new());
    let dhcp = sockets.add(dhcpv4::Socket::new());

    // 1. DHCP: wait for a lease (QEMU user NAT answers in milliseconds;
    //    60 s bound so a dead network fails loud, not silent).
    //    Also capture the resolver (option 6) for the DNS step; QEMU
    //    slirp's is always 10.0.2.3, but trust the lease when present.
    let mut dns_srv = Ipv4Addr::new(10, 0, 2, 3);
    let mut lease: Option<(IpCidr, IpAddress)> = None;
    let end = deadline_ms(60_000);
    loop {
        let _ = iface.poll(now(), &mut SmolDev, &mut sockets);
        if let Some(dhcpv4::Event::Configured(cfg)) =
            sockets.get_mut::<dhcpv4::Socket>(dhcp).poll()
        {
            let o = cfg.address.address().octets();
            let router = cfg
                .router
                .unwrap_or(Ipv4Addr::new(o[0], o[1], o[2], o[3]));
            if let Some(d) = cfg.dns_servers.iter().next() {
                dns_srv = *d;
            }
            lease = Some((IpCidr::Ipv4(cfg.address), IpAddress::Ipv4(router)));
            break;
        }
        if now().total_millis() >= end {
            break;
        }
        sched::yield_now();
    }
    if lease.is_none() {
        kprintln!("[net] dhcp: no lease, idling");
        loop {
            let _ = iface.poll(now(), &mut SmolDev, &mut sockets);
            sched::yield_now();
        }
    }
    let (addr, gw) = lease.unwrap();
    iface.update_ip_addrs(|a| {
        a.clear();
        let _ = a.push(addr);
    });
    let IpAddress::Ipv4(gw4) = gw;
    iface.routes_mut().add_default_ipv4_route(gw4).ok();
    kprintln!("[net] dhcp: {} gw {}", addr, gw);

    // 2. One ICMP echo to the gateway as proof of a working stack, then
    //    3. DNS google.com -> address, then 4. one ICMP echo to THAT
    //    address — real traffic off the virtual subnet, end to end.
    //    Shared ICMP socket (ident 0xC10C) so replies match by ident.
    let icmp = sockets.add(icmp::Socket::new(
        PacketBuffer::new(
            alloc::vec![PacketMetadata::EMPTY; 4],
            alloc::vec![0; 256],
        ),
        PacketBuffer::new(
            alloc::vec![PacketMetadata::EMPTY; 4],
            alloc::vec![0; 256],
        ),
    ));
    let bind_r = sockets
        .get_mut::<icmp::Socket>(icmp)
        .bind(icmp::Endpoint::Ident(0xC10C));
    let emit_r = icmp_emit(&mut sockets, icmp, gw, 0);
    kprintln!(
        "[net] ping: bind={} emit={}",
        bind_r.is_ok(),
        emit_r
    );
    let t0 = now().total_millis();
    let tx0 = virtio_net::tx_count();
    let rx0 = virtio_net::rx_count();
    match ping_poll(&mut iface, &mut sockets, icmp, t0, 10_000) {
        Some(dt) => kprintln!("[net] ping: reply from {} in {}ms", gw, dt),
        None => kprintln!(
            "[net] ping: no reply (tx+{} rx+{})",
            virtio_net::tx_count() - tx0,
            virtio_net::rx_count() - rx0
        ),
    }

    // 3. DNS: A-record query for google.com. Hand-rolled client (one RD-
    //    flagged query, first A recordparsed from the answer section,
    //    compression-pointer names skipped) — smaller than smoltcp's dns
    //    socket plumbing and every byte auditable. 5 s bound, loud either way.
    let mut resolved: Option<Ipv4Addr> = None;
    if let Some(q) = dns_build_query("google.com", 0x474C) {
        let qsock = sockets.add(udp::Socket::new(
            PacketBuffer::new(
                alloc::vec![udp::PacketMetadata::EMPTY; 4],
                alloc::vec![0; 512],
            ),
            PacketBuffer::new(
                alloc::vec![udp::PacketMetadata::EMPTY; 4],
                alloc::vec![0; 512],
            ),
        ));
        let qbind = sockets
            .get_mut::<udp::Socket>(qsock)
            .bind(IpListenEndpoint { addr: None, port: 49100 });
        let dst = IpEndpoint {
            addr: IpAddress::Ipv4(dns_srv),
            port: 53,
        };
        let qsend = match sockets.get_mut::<udp::Socket>(qsock).send(q.len(), dst) {
            Ok(buf) => {
                buf.copy_from_slice(&q);
                true
            }
            Err(_) => false,
        };
        kprintln!(
            "[net] dns: query server={} bind={} sent={}",
            dns_srv,
            qbind.is_ok(),
            qsend
        );
        let t0 = now().total_millis();
        let end = t0 + 15_000;
        let tx0 = virtio_net::tx_count();
        let rx0 = virtio_net::rx_count();
        loop {
            let _ = iface.poll(now(), &mut SmolDev, &mut sockets);
            if let Ok((payload, _)) = sockets.get_mut::<udp::Socket>(qsock).recv() {
                // Query-id echo + QR + rcode 0 = a real answer to OUR query.
                if qsend
                    && payload.len() >= 12
                    && payload[0] == 0x47
                    && payload[1] == 0x4C
                    && payload[2] & 0x80 != 0
                    && (payload[3] & 0x0F) == 0
                {
                    resolved = dns_parse_a(payload);
                }
            }
            if resolved.is_some() || now().total_millis() >= end {
                break;
            }
            sched::yield_now();
        }
        match resolved {
            Some(ip) => kprintln!(
                "[net] dns: google.com -> {} ({}ms)",
                ip,
                now().total_millis() - t0
            ),
            None => kprintln!(
                "[net] dns: no answer (tx+{} rx+{})",
                virtio_net::tx_count() - tx0,
                virtio_net::rx_count() - rx0
            ),
        }
        // CRITICAL: this socket shares (any, 49100) with brokered user
        // sockets forever after. smoltcp delivers an incoming datagram to
        // the FIRST matching socket and stops — the stale boot socket
        // would eat every reply addressed to a userland resolver client
        // (found live: demo blocked on a reply it never saw). Retire it.
        sockets.remove(qsock);
    } else {
        kprintln!("[net] dns: query build failed");
    }

    // 4. External ICMP: echo the REAL internet (slirp forwards ICMP echo
    //    via the host's ping sockets; if the host denies that we see
    //    tx+1 rx+0 — loud either way).
    if let Some(ip) = resolved {
        let emit_r = icmp_emit(&mut sockets, icmp, IpAddress::Ipv4(ip), 1);
        kprintln!("[net] ping: emit={}", emit_r);
        let t0 = now().total_millis();
        let tx0 = virtio_net::tx_count();
        let rx0 = virtio_net::rx_count();
        match ping_poll(&mut iface, &mut sockets, icmp, t0, 10_000) {
            Some(dt) => kprintln!("[net] ping: reply from {} in {}ms", ip, dt),
            None => kprintln!(
                "[net] ping: no reply (tx+{} rx+{})",
                virtio_net::tx_count() - tx0,
                virtio_net::rx_count() - rx0
            ),
        }
    } else {
        kprintln!("[net] ping: skipped (no resolver answer)");
    }

    // 5. Broker mode (Phase C): the net task is the only owner of the
    //    interface/SocketSet; userland opens sockets through port 18.
    //    Every lap: poll the iface (queuing + delivery), drain service
    //    requests, push arrived datagrams to owners, yield.
    //    The boot sockets (dhcp/icmp) stay in the set, harmless.
    kprintln!("[net] broker online (port {})", greenlime_abi::net::PORT_NET);
    let me = sched::current_id().unwrap_or(u32::MAX);
    // User handles = slot index in this table; None = free slot.
    let mut table: alloc::vec::Vec<Option<NetSock>> = {
        let mut t = alloc::vec::Vec::with_capacity(MAX_NET_SOCKETS);
        for _ in 0..MAX_NET_SOCKETS {
            t.push(None);
        }
        t
    };
    // TCP pull-model recv: one parked NET_RECV per slot; the drain loop
    // below satisfies it (data chunks or NET_EOF) once, next lap or later.
    let mut recv_pending: alloc::vec::Vec<Option<u32>> = {
        let mut t = alloc::vec::Vec::with_capacity(MAX_NET_SOCKETS);
        for _ in 0..MAX_NET_SOCKETS {
            t.push(None);
        }
        t
    };
    loop {
        let _ = iface.poll(now(), &mut SmolDev, &mut sockets);

        // Service requests (drain the whole queue every lap).
        while let Some(req) = crate::ipc::try_recv(greenlime_abi::net::PORT_NET) {
            net_broker_req(
                &mut iface,
                &mut sockets,
                &mut table,
                &mut recv_pending,
                req,
                me,
                dns_srv,
                icmp,
            );
        }

        // Per-slot maintenance: push UDP datagrams; satisfy parked TCP
        // recv requests (one chunk per lap; EOF when the stream ends).
        for (slot, entry) in table.iter().enumerate() {
            let Some(e) = entry else { continue };
            match e.kind {
                SockKind::Udp => {
                    let got = sockets.get_mut::<udp::Socket>(e.h).recv().ok();
                    let Some((payload, meta)) = got else { continue };
                    let src = match meta.endpoint.addr {
                        IpAddress::Ipv4(v4) => u32::from_be_bytes(v4.octets()),
                    };
                    let mut m = greenlime_abi::ipc::Message::new(greenlime_abi::net::NET_DATAGRAM);
                    m.arg0 = slot as u64;
                    m.arg1 = greenlime_abi::net::addr(src, meta.endpoint.port);
                    m.set_payload(payload);
                    if !crate::ipc::try_send(e.owner as u32, m, me as u32) {
                        // Owner mailbox full: drop (client retries; STAY
                        // non-blocking).
                        kprintln!("[net] datagram drop (mailbox full, slot {})", slot);
                    }
                }
                SockKind::Tcp => {
                    let Some(waiter) = recv_pending[slot] else { continue };
                    let mut buf = [0u8; 240];
                    let sock = sockets.get_mut::<tcp::Socket>(e.h);
                    let n = sock.recv_slice(&mut buf).unwrap_or(0);
                    if n > 0 {
                        // One chunk answers exactly one parked request
                        // (vfs-read semantics): the client pulls again for
                        // the next chunk. On the rare full mailbox, retry
                        // next lap (request stays parked).
                        let mut m = greenlime_abi::ipc::Message::new(greenlime_abi::net::NET_REPLY);
                        m.arg0 = n as u64;
                        m.set_payload(&buf[..n]);
                        if crate::ipc::try_send(waiter as u32, m, me as u32) {
                            recv_pending[slot] = None;
                        }
                    } else if !sock.may_recv() {
                        // Peer closed and the buffer drained: stream end.
                        let mut m = greenlime_abi::ipc::Message::new(greenlime_abi::net::NET_EOF);
                        m.arg0 = 0;
                        if crate::ipc::try_send(waiter as u32, m, me as u32) {
                            recv_pending[slot] = None;
                        }
                    }
                }
            }
        }

        sched::yield_now();
    }
}

/// Brokered socket protocol kind.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SockKind {
    Udp,
    Tcp,
}

/// A brokered socket: smoltcp handle + the single task allowed to touch it.
struct NetSock {
    h: SocketHandle,
    /// Task id of the owner (stamped at OPEN; SENDTO/SEND/CONNECT/CLOSE
    /// verify).
    owner: u32,
    /// UDP: bound local port. TCP: client port (assigned; nonzero).
    port: u16,
    /// Which protocol lives at `h` — the drain loop dispatches on it.
    kind: SockKind,
}

const MAX_NET_SOCKETS: usize = 8;
/// Client TCP sockets get ports from here (never collides: UDP brokers
/// check duplicates and TCP connects use this counter monotonically).
static NEXT_TCP_PORT: core::sync::atomic::AtomicU16 =
    core::sync::atomic::AtomicU16::new(49200);

/// One service request from userland. Fast ops reply inline; bounded ops
/// (connect/resolve/ping/send-backpressure) poll inside the handler — the
/// broker stalls but every client keeps its request/reply pairing, and no
/// handler can hang forever.
fn net_broker_req(
    iface: &mut Interface,
    sockets: &mut SocketSet,
    table: &mut [Option<NetSock>],
    recv_pending: &mut [Option<u32>],
    req: Message,
    me: u32,
    dns_srv: core::net::Ipv4Addr,
    icmp_h: SocketHandle,
) {
    use greenlime_abi::ipc::Message;
    use greenlime_abi::net;
    let to = req.sender as u32;
    // Every service op requires CAP_NET (kernel-spawn-time grant; the
    // stamped sender is the truth). Denied replies never queue work.
    if crate::sched::caps_of(req.sender) & crate::sched::caps::CAP_NET
        != crate::sched::caps::CAP_NET
    {
        kprintln!("[net] denied task {} (no CAP_NET)", req.sender);
        let mut rep = Message::new(net::NET_REPLY);
        rep.arg0 = u64::MAX;
        crate::ipc::send(to, rep, me as u32);
        return;
    }
    let mut rep = Message::new(net::NET_REPLY);
    match req.kind {
        net::NET_OPEN => {
            if req.arg0 == net::NET_KIND_UDP {
                let port = req.arg1 as u16 as u64;
                let sock = sockets.add(udp::Socket::new(
                    PacketBuffer::new(
                        alloc::vec![udp::PacketMetadata::EMPTY; 4],
                        alloc::vec![0; 256],
                    ),
                    PacketBuffer::new(
                        alloc::vec![udp::PacketMetadata::EMPTY; 4],
                        alloc::vec![0; 512],
                    ),
                ));
                let bind = sockets
                    .get_mut::<udp::Socket>(sock)
                    .bind(IpListenEndpoint { addr: None, port: port as u16 });
                if bind.is_err() || port == 0 {
                    kprintln!("[net] open: bad port {} (from task {})", port, req.sender);
                    sockets.remove(sock);
                    rep.arg0 = u64::MAX;
                } else if table.iter().any(|s| s.as_ref().map(|e| e.port) == Some(port as u16)) {
                    // Refuse duplicates: smoltcp delivers a datagram to the
                    // FIRST matching socket and stops — two same-port
                    // sockets would silently steal each other's mail.
                    kprintln!("[net] open: port {} in use", port);
                    sockets.remove(sock);
                    rep.arg0 = u64::MAX;
                } else if let Some(slot) = table.iter().position(|s| s.is_none()) {
                    table[slot] = Some(NetSock {
                        h: sock,
                        owner: req.sender,
                        port: port as u16,
                        kind: SockKind::Udp,
                    });
                    kprintln!(
                        "[net] open: slot {} udp port {} for task {}",
                        slot,
                        port,
                        req.sender
                    );
                    rep.arg0 = slot as u64;
                } else {
                    kprintln!("[net] open: socket table full");
                    sockets.remove(sock);
                    rep.arg0 = u64::MAX;
                }
            } else if req.arg0 == net::NET_KIND_TCP {
                // Client stream socket: buffers sized for HTTP-scale fetches;
                // submit binds nothing, NET_CONNECT sets the remote.
                let port = NEXT_TCP_PORT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                let sock = sockets.add(tcp::Socket::new(
                    tcp::SocketBuffer::new(alloc::vec![0; 4096]),
                    tcp::SocketBuffer::new(alloc::vec![0; 4096]),
                ));
                if let Some(slot) = table.iter().position(|s| s.is_none()) {
                    table[slot] = Some(NetSock {
                        h: sock,
                        owner: req.sender,
                        port,
                        kind: SockKind::Tcp,
                    });
                    kprintln!(
                        "[net] open: slot {} tcp port {} for task {}",
                        slot,
                        port,
                        req.sender
                    );
                    rep.arg0 = slot as u64;
                } else {
                    kprintln!("[net] open: socket table full");
                    sockets.remove(sock);
                    rep.arg0 = u64::MAX;
                }
            } else {
                kprintln!("[net] open: unknown kind {}", req.arg0);
                rep.arg0 = u64::MAX;
            }
        }
        net::NET_CONNECT => {
            let slot = req.arg0 as usize;
            let entry = table.get(slot).and_then(|s| s.as_ref());
            let Some(e) = entry.filter(|e| e.owner == req.sender && e.kind == SockKind::Tcp)
            else {
                rep.arg0 = u64::MAX;
                crate::ipc::send(to, rep, me as u32);
                return;
            };
            let remote = IpEndpoint {
                addr: IpAddress::Ipv4(core::net::Ipv4Addr::from(
                    greenlime_abi::net::addr_ip(req.arg1).to_be_bytes(),
                )),
                port: greenlime_abi::net::addr_port(req.arg1),
            };
            let ok = (|| {
                let s = sockets.get_mut::<tcp::Socket>(e.h);
                // Local addr auto-chosen from the remote when unspecified;
                // the port is our already-assigned ephemeral port.
                s.connect(iface.context(), remote, IpListenEndpoint { addr: None, port: e.port })
            })();
            let mut ok2 = ok.is_ok();
            if ok2 {
                let end = now().total_millis() + 8_000;
                loop {
                    let _ = iface.poll(now(), &mut SmolDev, sockets);
                    let st = sockets.get_mut::<tcp::Socket>(e.h).state();
                    if st == tcp::State::Established {
                        break;
                    }
                    if st == tcp::State::Closed {
                        // Reset / refused — drive the dead connection loud.
                        ok2 = false;
                        break;
                    }
                    if now().total_millis() >= end {
                        sockets.get_mut::<tcp::Socket>(e.h).abort();
                        ok2 = false;
                        break;
                    }
                    sched::yield_now();
                }
            }
            let remote_ip = remote.addr;
            kprintln!(
                "[net] connect: slot {} -> {}:{} ({})",
                slot,
                remote_ip,
                greenlime_abi::net::addr_port(req.arg1),
                if ok2 { "up" } else { "failed" }
            );
            rep.arg0 = if ok2 { 0 } else { u64::MAX };
        }
        net::NET_SEND => {
            let slot = req.arg0 as usize;
            let entry = table.get(slot).and_then(|s| s.as_ref());
            let Some(e) = entry.filter(|e| e.owner == req.sender && e.kind == SockKind::Tcp)
            else {
                rep.arg0 = u64::MAX;
                crate::ipc::send(to, rep, me as u32);
                return;
            };
            let data = req.payload_bytes();
            if data.is_empty() || !sockets.get_mut::<tcp::Socket>(e.h).may_send() {
                rep.arg0 = u64::MAX;
            } else {
                // Backpressure: if the tx buffer is full, poll it dry
                // (bounded) instead of failing the client — a stream send
                // of 240 B is small, but the model must not lie.
                let end = now().total_millis() + 4_000;
                loop {
                    match sockets.get_mut::<tcp::Socket>(e.h).send_slice(data) {
                        Ok(n) => {
                            rep.arg0 = n as u64;
                            break;
                        }
                        Err(_) if now().total_millis() >= end => {
                            rep.arg0 = u64::MAX;
                            break;
                        }
                        Err(_) => {
                            let _ = iface.poll(now(), &mut SmolDev, sockets);
                            sched::yield_now();
                        }
                    }
                }
            }
        }
        net::NET_RECV => {
            // Park the request; the drain loop answers (chunk or EOF). The
            // caller must be the owner and the connection must be TCP.
            let slot = req.arg0 as usize;
            let entry = table.get(slot).and_then(|s| s.as_ref());
            let owns = entry.map(|e| e.owner == req.sender && e.kind == SockKind::Tcp);
            if owns == Some(true) {
                if recv_pending[slot].is_some() {
                    rep.arg0 = u64::MAX; // one pending recv per slot
                } else {
                    recv_pending[slot] = Some(req.sender);
                    // No inline reply: the drain loop answers next lap(s).
                    return;
                }
            } else {
                rep.arg0 = u64::MAX;
            }
        }
        net::NET_PING => {
            // One ICMP echo through the shared icmp socket (owner of it:
            // this broker). Clients serialize through here; sequential use
            // is the contract. arg1 = dst packed.
            let dst = IpAddress::Ipv4(core::net::Ipv4Addr::from(
                greenlime_abi::net::addr_ip(req.arg1).to_be_bytes(),
            ));
            let seq = 0x7000u16.wrapping_add(now().total_millis() as u16);
            let emit_ok = icmp_emit(sockets, icmp_h, dst, seq);
            let t0 = now().total_millis();
            rep.arg0 = if emit_ok {
                match ping_poll(iface, sockets, icmp_h, t0, 5_000) {
                    Some(dt) => dt as u64,
                    None => {
                        kprintln!("[net] ping: no reply from {} (5s)", dst);
                        u64::MAX
                    }
                }
            } else {
                u64::MAX
            };
        }
        net::NET_CLOSE => {
            let slot = req.arg0 as usize;
            if slot < table.len()
                && table[slot].as_ref().map(|s| s.owner) == Some(req.sender)
            {
                if let Some(e) = table[slot].take() {
                    if e.kind == SockKind::Tcp {
                        // Abort, not close: no FIN-flush state machine this
                        // phase — a drained stream already saw its NET_EOF.
                        sockets.get_mut::<tcp::Socket>(e.h).abort();
                    }
                    sockets.remove(e.h);
                }
                if slot < recv_pending.len() {
                    recv_pending[slot] = None;
                }
                rep.arg0 = 0;
            } else {
                rep.arg0 = u64::MAX; // not yours / not open
            }
        }
        net::NET_SENDTO => {
            let slot = req.arg0 as usize;
            let entry = table.get(slot).and_then(|s| s.as_ref());
            // Owner check: a task may only write sockets it opened.
            let Some(e) = entry.filter(|e| e.owner == req.sender) else {
                rep.arg0 = u64::MAX;
                crate::ipc::send(to, rep, me as u32);
                return;
            };
            let dst = IpEndpoint {
                addr: IpAddress::Ipv4(core::net::Ipv4Addr::from(
                    greenlime_abi::net::addr_ip(req.arg1).to_be_bytes(),
                )),
                port: greenlime_abi::net::addr_port(req.arg1),
            };
            let data = req.payload_bytes();
            rep.arg0 = match sockets.get_mut::<udp::Socket>(e.h).send(data.len(), dst) {
                Ok(buf) => {
                    buf.copy_from_slice(data);
                    0
                }
                Err(_) => u64::MAX,
            };
        }
        net::NET_RESOLVE => {
            // Broker-held resolver: temporary socket, same wire format as
            // the boot-phase resolver, bounded wait, loud failure.
            let name = core::str::from_utf8(req.payload_bytes()).unwrap_or("");
            let mut result: Option<core::net::Ipv4Addr> = None;
            if let Some(q) = dns_build_query(name, 0x474C) {
                let h = sockets.add(udp::Socket::new(
                    PacketBuffer::new(
                        alloc::vec![udp::PacketMetadata::EMPTY; 4],
                        alloc::vec![0; 512],
                    ),
                    PacketBuffer::new(
                        alloc::vec![udp::PacketMetadata::EMPTY; 4],
                        alloc::vec![0; 512],
                    ),
                ));
                let qbind = sockets
                    .get_mut::<udp::Socket>(h)
                    .bind(IpListenEndpoint { addr: None, port: 49101 });
                if qbind.is_ok() {
                    let dst = IpEndpoint {
                        addr: IpAddress::Ipv4(dns_srv),
                        port: 53,
                    };
                    // Copy the query into the tx window; send() only
                    // RESERVES the space (found live: the wire saw 28
                    // perfect-length zero bytes — a silent DNS server, no
                    // pcap, no answer).
                    let qsend = match sockets.get_mut::<udp::Socket>(h).send(q.len(), dst) {
                        Ok(buf) => {
                            buf.copy_from_slice(&q);
                            q.len()
                        }
                        Err(_) => 0,
                    };
                    kprintln!("[net] resolve: {} query={} sent={}", name, q.len(), qsend);
                    let end = now().total_millis() + 8_000;
                    loop {
                        let _ = iface.poll(now(), &mut SmolDev, sockets);
                        if let Ok((payload, _)) =
                            sockets.get_mut::<udp::Socket>(h).recv()
                        {
                            // id 0x474C ("GL") echoed + QR + rcode 0.
                            let ok = payload.len() >= 12
                                && payload[0] == 0x47
                                && payload[1] == 0x4C
                                && payload[2] & 0x80 != 0
                                && (payload[3] & 0x0F) == 0;
                            kprintln!(
                                "[net] resolve: reply {}B code0={}",
                                payload.len(),
                                payload.len() >= 4 && (payload[3] & 0x0F) == 0
                            );
                            if ok {
                                if let Some(ip) = dns_parse_a(payload) {
                                    result = Some(ip);
                                }
                            }
                        }
                        if result.is_some() || now().total_millis() >= end {
                            break;
                        }
                        sched::yield_now();
                    }
                }
                sockets.remove(h);
            } else {
                kprintln!("[net] resolve: query build failed");
            }
            rep.arg0 = match result {
                Some(ip) => {

                    greenlime_abi::net::addr(
                        u32::from_be_bytes(ip.octets()),
                        0,
                    )
                }
                None => u64::MAX,
            };
        }
        _ => {
            // Unknown opcodes fail closed (never guess semantics).
            rep.arg0 = u64::MAX;
        }
    }
    crate::ipc::send(to, rep, me as u32);
}

/// Emit one echo request (ident 0xC10C) onto the shared ICMP socket.
/// The send borrows the socket transiently; the poll loop borrows it
/// per-iteration so none of them overlap.
fn icmp_emit(sockets: &mut SocketSet, h: SocketHandle, target: IpAddress, seq_no: u16) -> bool {
    let echo = Icmpv4Repr::EchoRequest {
        ident: 0xC10C,
        seq_no,
        data: b"greenlime",
    };
    let Some(buf) = sockets
        .get_mut::<icmp::Socket>(h)
        .send(echo.buffer_len(), target)
        .ok()
    else {
        return false;
    };
    echo.emit(
        &mut Icmpv4Packet::new_unchecked(buf),
        &smoltcp::phy::ChecksumCapabilities::default(),
    );
    true
}

/// Poll for an echo reply through the shared ICMP socket. Returns ms since
/// `t0` on the first reply, None on the `wait_ms` deadline (loud timeout).
fn ping_poll(
    iface: &mut Interface,
    sockets: &mut SocketSet,
    h: SocketHandle,
    t0: i64,
    wait_ms: i64,
) -> Option<i64> {
    let end = t0 + wait_ms;
    loop {
        let _ = iface.poll(now(), &mut SmolDev, sockets);
        if let Ok((_, _from)) = sockets.get_mut::<icmp::Socket>(h).recv() {
            return Some(now().total_millis() - t0);
        }
        if now().total_millis() >= end {
            return None;
        }
        sched::yield_now();
    }
}

/// A-record query for `name` with echo'd id `id` (13 B top + labels).
/// "google.com" = 0x474C-sized header + [3]goo[3]gle0 + type A + class IN.
fn dns_build_query(name: &str, id: u16) -> Option<alloc::vec::Vec<u8>> {
    let mut q = alloc::vec::Vec::new();
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00]); // flags: RD
    q.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]); // qd=1, an/ns/ar=0
    for label in name.split('.') {
        let n = label.len();
        if n == 0 || n > 63 {
            return None;
        }
        q.push(n as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&[0, 1]); // type A
    q.extend_from_slice(&[0, 1]); // class IN
    Some(q)
}

/// Parse a DNS reply for the FIRST A record (class IN) in the answer
/// section. Compression pointers terminate names in two bytes — we skip
/// names without chasing pointers (the type/class fields carry alignment,
/// so chasing is never needed for this v-shaped minimal client).
fn dns_parse_a(buf: &[u8]) -> Option<Ipv4Addr> {
    if buf.len() < 12 {
        return None;
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let mut i = 12;
    for _ in 0..qd {
        i = dns_skip_name(buf, i)?;
        i = i.checked_add(4)?; // question tail: type + class
        if i > buf.len() {
            return None;
        }
    }
    for _ in 0..an {
        i = dns_skip_name(buf, i)?;
        if i + 10 > buf.len() {
            return None;
        }
        let rtype = u16::from_be_bytes([buf[i], buf[i + 1]]);
        let rclass = u16::from_be_bytes([buf[i + 2], buf[i + 3]]);
        let rdlen = u16::from_be_bytes([buf[i + 8], buf[i + 9]]) as usize;
        let rdata = i + 10;
        if rtype == 1 && rclass == 1 && rdlen == 4 && rdata + 4 <= buf.len() {
            let o = &buf[rdata..rdata + 4];
            return Some(Ipv4Addr::new(o[0], o[1], o[2], o[3]));
        }
        i = rdata.checked_add(rdlen)?;
    }
    None
}

/// Advance past one DNS name: label run to the 0 terminator, or the two
/// bytes of a compression pointer (0xC0..). Rejects exotic label types.
fn dns_skip_name(buf: &[u8], mut i: usize) -> Option<usize> {
    loop {
        if i >= buf.len() {
            return None;
        }
        let len = buf[i];
        if len == 0 {
            return Some(i + 1);
        }
        if len & 0xC0 == 0xC0 {
            return i.checked_add(2).filter(|&j| j <= buf.len());
        }
        if len & 0xC0 != 0 {
            return None;
        }
        i += 1 + len as usize;
    }
}
