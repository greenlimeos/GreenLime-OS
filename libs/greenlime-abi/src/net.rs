//! Net IPC protocol (ABI v4). The net task brokers smoltcp for userland:
//! it is the ONLY task allowed to poll the interface/SocketSet, so sockets
//! are brokered through this port-18 protocol like windows through 19.
//!
//! Wire rules (all Message-based via IpcSend/IpcRecv):
//! - Requests go to `PORT_NET`; payload = bytes (op-specific).
//! - Replies are pushed to the requester's OWN mailbox (port == pid) with
//!   kind `NET_REPLY`; `arg0` = result (handle / 0 ok / u64::MAX on error),
//!   never forged — the kernel stamps `sender` and clients must check it.
//! - Incoming datagrams are PUSHED to the socket owner's mailbox as kind
//!   `NET_DATAGRAM` (arg0 = handle, arg1 = packed src ip:port). Queue-full
//!   drops the datagram (client retries; no blocking in the broker loop).
//! - `MAX_PAYLOAD` (2048 B since ABI v6) is the message cap; UDP datagrams
//!   ride one message each, TCP is a stream of pulled payload-size chunks.
//!
//! THREADING MODEL: every handle is single-owner (stamped at OPEN; SENDTO,
//! SEND, CONNECT and CLOSE verify it). One task, one handle, no shared
//! socket state.

/// Broker port (task 6 owns it; vfs 16, dev 17, winsrv 19).
pub const PORT_NET: u32 = 18;

/// Open a socket. arg0 = kind (NET_KIND_*), arg1 = local port.
/// UDP: port must be nonzero. TCP: 0 = broker assigns a client port.
/// Replies `NET_REPLY` arg0 = handle, or u64::MAX (bad kind / port / full).
pub const NET_OPEN: u32 = 0x0100;
/// Close handle arg0 (owner only). Replies arg0 = 0 / u64::MAX.
pub const NET_CLOSE: u32 = 0x0101;
/// Sendto handle arg0, dst packed in arg1, bytes in payload. Replies
/// arg0 = bytes accepted (0 = queued, broker drains) or u64::MAX.
pub const NET_SENDTO: u32 = 0x0102;
/// Resolve (A record): payload = hostname. Replies arg0 = IPv4 packed,
/// u64::MAX = no answer (loud; no rcode detail yet).
pub const NET_RESOLVE: u32 = 0x0103;
/// Reply kind (to the requester's mailbox). arg0 = result.
pub const NET_REPLY: u32 = 0x0104;
/// Unsolicited datagram push (to the owner's mailbox).
/// arg0 = handle, arg1 = packed src (ip<<16 | port), payload = bytes.
pub const NET_DATAGRAM: u32 = 0x0105;

// ---- Phase D: TCP over the same broker ----
/// Connect handle arg0 to dst packed in arg1 ([ip][port]). TCP sockets
/// open with local port 0 = broker assigns an ephemeral port. The broker
/// polls to ESTABLISHED (bounded); replies arg0 = 0 / u64::MAX.
pub const NET_CONNECT: u32 = 0x0106;
/// Stream send: handle arg0, payload bytes (<=240 B per message; a client
/// streams by looping). The broker drains a full tx buffer via polls
/// (bounded) rather than failing on backpressure. arg0 = bytes accepted.
pub const NET_SEND: u32 = 0x0107;
/// Stream recv: handle arg0. Replies NET_REPLY with payload = up to
/// MAX_PAYLOAD (2048 B since v6) and arg0 = byte count (one code path with
/// the drain loop). Held by the broker until data arrives; when the peer closed and the buffer drained,
/// replies kind NET_EOF (arg0 = 0) — stream end, not an error. One pending
/// recv per handle; a second before completion is rejected.
pub const NET_RECV: u32 = 0x0108;
/// TCP stream end for a pending NET_RECV. arg0 = 0.
pub const NET_EOF: u32 = 0x0109;
/// ICMP echo: arg1 = dst packed (ip<<16|0; port unused). The owner of the
/// only ICMP socket is the broker; clients serialize through here. Replies
/// arg0 = RTT ms, u64::MAX = no reply in 5 s.
pub const NET_PING: u32 = 0x010A;

/// Socket kinds for NET_OPEN.
pub const NET_KIND_UDP: u64 = 0;
/// TCP (Phase D): client socket — NET_CONNECT, then NET_SEND/NET_RECV
/// (pulled 240 B chunks, NET_EOF on stream end), NET_CLOSE.
pub const NET_KIND_TCP: u64 = 1;

/// Pack IPv4 (byte-packed big-endian) + port into an arg64:
/// [ip:32][port:16].
pub const fn addr(ip: u32, port: u16) -> u64 {
    ((ip as u64) << 16) | (port as u64)
}
/// Unpack an arg64 produced by `addr`.
pub const fn addr_ip(a: u64) -> u32 {
    (a >> 16) as u32
}
/// Unpack an arg64 produced by `addr`.
pub const fn addr_port(a: u64) -> u16 {
    a as u16
}

/// IPv4 address as a packed u32 from dotted octets.
pub const fn ipv4o(a: u8, b: u8, c: u8, d: u8) -> u32 {
    ((a as u32) << 24) | ((b as u32) << 16) | ((c as u32) << 8) | d as u32
}
