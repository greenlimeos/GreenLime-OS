//! src/fetch.rs — the HTTPS fetch lane (M20): ONE TLS 1.3 + HTTP/1.1 GET
//! over the caller-supplied TCP link, with REAL certificate validation.
//!
//! Design laws, stated plainly:
//!   - transport comes from the caller: the shell owns the net-broker
//!     wire protocol, this crate owns crypto + TLS state machine + HTTP.
//!     The `TcpLink` trait is the entire interface between them.
//!   - rustls' std `write_tls/reader` API does not exist in this no_std
//!     build, so the connection runs through the `unbuffered` pump —
//!     the shape rustls itself reserves for exactly this environment.
//!     (The pump follows the crate's own `unbuffered-client` example
//!     arm-for-arm, which is the upstream-endorsed ownership story.)
//!   - the clock for certificate validity is `Syscall::Now` wired in as
//!     rustls' `TimeProvider` (CMOS boot epoch + uptime; the verifier
//!     receives its `now` through the client handshake).
//!   - validation is rustls' built-in webpki verifier over the vendored
//!     Mozilla root set. The noop-verifier pattern is not on the menu:
//!     a chain that does not verify is an error, not a shortcut.
//!   - redirects live ABOVE this lane: the shell owns the http lane too,
//!     follows `Location` hops itself, and gates scheme downgrades.
//!   - body bytes stream straight to the caller's closure; nothing is
//!     buffered here beyond the header block.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use rustls::client::UnbufferedClientConnection;
use rustls::unbuffered::{ConnectionState, EncodeError, UnbufferedStatus};use rustls::version::TLS13;
use rustls::{ClientConfig, RootCertStore};

use crate::sys;

/// What the TCP link can deliver on a receive.
pub enum Recv {
    /// n bytes appended into the caller's buffer.
    Data(usize),
    /// peer closed the TCP stream (broker NET_EOF / an empty tail).
    Fin,
    /// the broker refused or errored; the stream is unusable.
    Broken,
}

/// How the pump pushes/pulls raw TCP bytes. Implemented by the shell
/// against the net broker. `send` queues everything (the impl splits off
/// broker-message-size pieces and waits out queue backpressure).
pub trait TcpLink {
    fn send(&mut self, data: &[u8]) -> Result<(), ()>;
    fn recv(&mut self, buf: &mut [u8]) -> Recv;
}

/// Everything the shell needs to print the proof line and chase redirects.
pub struct HttpSummary {
    /// First status line, CR/LF trimmed ("HTTP/1.1 200 OK").
    pub status_line: Vec<u8>,
    /// The parsed three-digit status code; 0 when the line was garbage.
    pub status: u16,
    /// `Location` header value when one was present, unvalidated.
    pub location: Option<Vec<u8>>,
    /// Total header-block bytes (up to and including the blank line).
    pub header_bytes: usize,
    /// Total body bytes handed to the callback.
    pub body_bytes: usize,
    /// True when the TLS channel saw the server's close_notify. Servers
    /// that sleeve out with a bare TCP FIN get false — reported, not
    /// hidden.
    pub clean_tls_close: bool,
}

/// Everything that can go wrong, named. Setup covers config/root-store/
/// server-name construction; Tls carries the full rustls reason so a
/// refusal can quote the actual verifier failure.
pub enum FetchErr {
    Setup(String),
    Tcp,
    Tls(String),
    Http(&'static str),
}

/// The kernel clock, as rustls' TimeProvider. Certificate expiry checks
/// (and session-ticket freshness) both read here. The QEMU-UTC vs
/// real-BIOS-local-clock nuance is documented in docs/SECURITY.md — the
/// lane hides nothing; it also cannot fix a motherboard battery.
#[derive(Debug)]
struct KernelTimeProvider;

impl rustls::time_provider::TimeProvider for KernelTimeProvider {
    fn current_time(&self) -> Option<pki_types::UnixTime> {
        // Syscall::Now returns exactly the unit UnixTime wants: seconds.
        Some(pki_types::UnixTime::since_unix_epoch(core::time::Duration::from_secs(
            sys::now(),
        )))
    }
}

fn tls_config() -> Result<Arc<ClientConfig>, FetchErr> {
    // The clock first: the builder wants provider + time provider as an
    // explicit pair (config slots for both).
    let clock_provider: Arc<dyn rustls::time_provider::TimeProvider> =
        Arc::new(KernelTimeProvider);
    // The root store: the vendored Mozilla set straight in (the crate's
    // own documented construction).
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let mut cfg = ClientConfig::builder_with_details(
        crate::provider().into(),
        clock_provider,
    )
    .with_protocol_versions(&[&TLS13])
    .map_err(|e| FetchErr::Setup(e.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    // The clock injection point: all validity math flows from here.
    cfg.time_provider = Arc::new(KernelTimeProvider);
    Ok(Arc::new(cfg))
}

/// One https GET. The response body streams to `body_out`; the summary
/// travels back for the redirect chase and the proof line.
///
/// `path` starts with '/' (parsed by the shell); `server_name` is the
/// SNI + certificate identity (a DNS name — the shell refuses IP
/// literals on the https lane before this runs).
pub fn https_get(
    link: &mut dyn TcpLink,
    server_name: &[u8],
    path: &[u8],
    body_out: &mut dyn FnMut(&[u8]),
) -> Result<HttpSummary, FetchErr> {
    let cfg = tls_config()?;
    // ServerName law: TryFrom<&[u8]> only loans its lifetime, so the
    // owned path goes through String (lossy is fine; garbage UTF-8 is
    // an invalid DNS name, refused by the name validator downstream).
    let name: pki_types::ServerName<'static> = pki_types::ServerName::try_from(
        String::from_utf8_lossy(server_name).into_owned(),
    )
    .map_err(|e| FetchErr::Setup(e.to_string()))?;
    // ALPN says HTTP/1.1 up front — servers route on it.
    let alpn = vec![b"http/1.1".to_vec()];
    let mut conn = UnbufferedClientConnection::new_with_alpn(cfg, name, alpn)
        .map_err(|e| FetchErr::Setup(e.to_string()))?;

    // The example-sized wire shelves, grown for the max-TLS-record law:
    // a record is 16384 plaintext + up to 256 overhead, 16640 total, so
    // the read side holds one full record plus its spine with room.
    const INCOMING_CAP: usize = 20 * 1024;
    let mut incoming_used = 0usize;
    let mut incoming_tls = vec![0u8; INCOMING_CAP];
    let mut outgoing_tls = vec![0u8; 4096];
    let mut outgoing_used = 0usize;

    let mut sent_request = false;
    let mut received_response = false;
    let mut we_closed = false;
    let mut clean_close = false;
    let mut saw_fin = false;

    // HTTP response assembly: header block staged, body streamed.
    let mut hdr: Vec<u8> = Vec::with_capacity(1024);
    let mut status_line: Vec<u8> = Vec::new();
    let mut status = 0u16;
    let mut location: Option<Vec<u8>> = None;
    let mut headers_done = false;
    let mut header_bytes = 0usize;
    let mut body_bytes = 0usize;

    // The state machine walk. Mirrors rustls' unbuffered-client example:
    // every conn interaction goes through the state's own door.
    let mut iterations = 0usize;
    loop {
        iterations += 1;
        if iterations > 512 {
            return Err(FetchErr::Http("TLS state machine did not converge"));
        }

        let UnbufferedStatus { mut discard, state } =
            conn.process_tls_records(&mut incoming_tls[..incoming_used]);

        match state {
            Err(e) => return Err(FetchErr::Tls(e.to_string())),
            Ok(ConnectionState::ReadTraffic(mut r)) => {
                while let Some(res) = r.next_record() {
                    let rec = res.map_err(|e| FetchErr::Tls(e.to_string()))?;
                    discard += rec.discard;
                    let payload = rec.payload;
                    if headers_done {
                        body_out(payload);
                        body_bytes += payload.len();
                    } else {
                        hdr.extend_from_slice(payload);
                        if hdr.len() > 8 * 1024 {
                            return Err(FetchErr::Http("header block exceeds 8 KiB"));
                        }
                        if let Some(sp) = find_header_end(&hdr) {
                            status_line = parse_status_line(&hdr[..sp]);
                            status = parse_status_code(&status_line);
                            location = parse_location(&hdr[..sp]);
                            header_bytes = sp + 4;
                            headers_done = true;
                            // body bytes that rode along in this record
                            let rest = sp + 4;
                            if hdr.len() > rest {
                                body_out(&hdr[rest..]);
                                body_bytes += hdr.len() - rest;
                            }
                            hdr.clear();
                            hdr.shrink_to_fit();
                        }
                        received_response = true;
                    }
                }
            }
            Ok(ConnectionState::EncodeTlsData(mut e)) => {
                // One TLS chunk at a time; InsufficientSize grows the shelf.
                match e.encode(&mut outgoing_tls[outgoing_used..]) {
                    Ok(n) => outgoing_used += n,
                    Err(EncodeError::InsufficientSize(req)) => {
                        outgoing_tls.resize(outgoing_used + req.required_size, 0);
                        match e.encode(&mut outgoing_tls[outgoing_used..]) {
                            Ok(n) => outgoing_used += n,
                            Err(other) => return Err(FetchErr::Tls(other.to_string())),
                        }
                    }
                    Err(other) => return Err(FetchErr::Tls(other.to_string())),
                }
            }
            Ok(ConnectionState::TransmitTlsData(t)) => {
                if outgoing_used > 0 {
                    link.send(&outgoing_tls[..outgoing_used]).map_err(|_| FetchErr::Tcp)?;
                    outgoing_used = 0;
                }
                t.done();
            }
            Ok(ConnectionState::BlockedHandshake) => {
                pull_interleaved(
                    link,
                    &mut incoming_tls,
                    &mut incoming_used,
                    &mut saw_fin,
                    FetchErr::Tls("connection closed mid-handshake".into()),
                )?;
            }
            Ok(ConnectionState::WriteTraffic(mut w)) => {
                if !sent_request {
                    let request = build_request(server_name, path);
                    let written = w
                        .encrypt(&request, &mut outgoing_tls[outgoing_used..])
                        .map_err(|e| FetchErr::Tls(e.to_string()))?;
                    // InsufficientSize is a shelf-size complaint, not an
                    // error of the protocol; our request is < 4 KiB.
                    outgoing_used += written;
                    sent_request = true;
                    link.send(&outgoing_tls[..outgoing_used]).map_err(|_| FetchErr::Tcp)?;
                    outgoing_used = 0;
                } else if !received_response {
                    // TLS 1.3 flow: the request rode out with the
                    // handshake; the server's response streams next.
                    pull_interleaved(
                        link,
                        &mut incoming_tls,
                        &mut incoming_used,
                        &mut saw_fin,
                        FetchErr::Tls("connection closed before any response".into()),
                    )?;
                } else if !we_closed {
                    let n = w
                        .queue_close_notify(&mut outgoing_tls[outgoing_used..])
                        .map_err(|e| FetchErr::Tls(e.to_string()))?;
                    outgoing_used += n;
                    link.send(&outgoing_tls[..outgoing_used]).map_err(|_| FetchErr::Tcp)?;
                    outgoing_used = 0;
                    we_closed = true;
                } else {
                    // Waiting for the server's close_notify (or the tail of
                    // the body): pull. A bare FIN without close_notify is
                    // unclean - reported via clean_tls_close, not hidden.
                    pull_interleaved(link, &mut incoming_tls, &mut incoming_used, &mut saw_fin, FetchErr::Tcp)?;
                }
            }
            Ok(ConnectionState::PeerClosed) => {
                clean_close = true;
            }
            Ok(ConnectionState::Closed) => break,
            Ok(other) => {
                return Err(FetchErr::Tls(format!("unexpected TLS state: {other:?}")))
            }
        }

        if discard != 0 {
            incoming_tls.copy_within(discard..incoming_used, 0);
            incoming_used -= discard;
        }

        if saw_fin {
            break;
        }
    }

    Ok(HttpSummary {
        status_line,
        status,
        location,
        header_bytes,
        body_bytes,
        clean_tls_close: clean_close,
    })
}

/// One broker-chunk pull into the incoming shelf. The chunk is clamped to
/// the free room (never a wall); `Fin` maps to a stall-break via `saw_fin`
/// except where the caller hands a dedicated mid-stream error instead.
fn pull_interleaved(
    link: &mut dyn TcpLink,
    incoming_tls: &mut [u8],
    incoming_used: &mut usize,
    saw_fin: &mut bool,
    on_fin: FetchErr,
) -> Result<(), FetchErr> {
    let tail = *incoming_used;
    let room = incoming_tls.len() - tail;
    if room == 0 {
        return Err(FetchErr::Http("incoming TLS shelf full"));
    }
    let want = room.min(2048);
    match link.recv(&mut incoming_tls[tail..tail + want]) {
        Recv::Data(n) => {
            *incoming_used += n;
        }
        Recv::Fin => {
            if let FetchErr::Tcp = on_fin {
                // stream end while only drained body remains: honest end
                *saw_fin = true;
            } else {
                return Err(on_fin);
            }
        }
        Recv::Broken => return Err(FetchErr::Tcp),
    }
    Ok(())
}

fn build_request(server_name: &[u8], path: &[u8]) -> Vec<u8> {
    // HTTP/1.1 by the book: a Host header (exactly what the wire needs),
    // a UA that tells the truth about what this is, identity encoding
    // (no gzip decoder exists in this lane yet), and a close so the body
    // arrives in one connection life.
    let mut req = Vec::with_capacity(server_name.len() + path.len() + 112);
    req.extend_from_slice(b"GET ");
    req.extend_from_slice(path);
    req.extend_from_slice(b" HTTP/1.1\r\nHost: ");
    req.extend_from_slice(server_name);
    req.extend_from_slice(
        b"\r\nUser-Agent: GreenLime/1.0 (ring-3; TLS1.3; M20)\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n",
    );
    req
}

/// Index of the "\r\n\r\n" that ends an HTTP header block.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    (0..=buf.len() - 4).find(|&i| &buf[i..i + 4] == b"\r\n\r\n")
}

/// First status line, CR/LF trimmed. No fancy parsing: the shell prints
/// this byte-for-byte in the proof line.
fn parse_status_line(head: &[u8]) -> Vec<u8> {
    let mut line = head.split(|&b| b == b'\n').next().unwrap_or(head).to_vec();
    while matches!(line.last(), Some(b'\r') | Some(b'\n')) {
        line.pop();
    }
    if line.len() > 64 {
        line.truncate(64);
    }
    line
}

/// The 3-digit status code, taken from the first SP onwards. Zero when
/// the shape is not what HTTP promises (garbage goes out as 0, loudly).
fn parse_status_code(line: &[u8]) -> u16 {
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

/// Case-insensitive `Location` scan of the header block, per HTTP law.
fn parse_location(head: &[u8]) -> Option<Vec<u8>> {
    for line in head.split(|&b| b == b'\n') {
        let mut line = line;
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        if let Some(rest) = starts_ignore_case(line, b"Location:") {
            // a single leading space is law-expected; tolerate more slop
            return Some(rest.trim_ascii().to_vec());
        }
    }
    None
}

fn starts_ignore_case<'a>(hay: &'a [u8], needle: &[u8]) -> Option<&'a [u8]> {
    if hay.len() < needle.len() {
        return None;
    }
    let (n, y) = hay.split_at(needle.len());
    // Both sides lowered: bug history (live-found in a real run) — the
    // first version lowered only the header, so the uppercase 'L' of the
    // needle never matched and every Location was missed.
    for (&h, c) in n.iter().zip(needle.iter()) {
        if h.to_ascii_lowercase() != c.to_ascii_lowercase() {
            return None;
        }
    }
    Some(y)
}
