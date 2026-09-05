//! Blocking TCP connection with FTX2 framing.
//!
//! `Connection` is the low-level I/O primitive used by both the lab tool and
//! the host engine. It owns a `TcpStream` and provides send/recv helpers that
//! deal entirely in `FrameHeader` + raw body bytes.
//!
//! For large-body frames (shard data), use `recv_header` + `recv_body_exact` /
//! `drain_body` rather than `recv_frame`, which buffers the full body.

use std::io::{self, IoSlice, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ftx2_proto::{FrameHeader, FrameType, FRAME_HEADER_LEN};

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BUFFERED_BODY: usize = 4 * 1024 * 1024; // 4 MiB — refuse to buffer more
const DRAIN_CHUNK: usize = 64 * 1024;

/// Number of times `connect` re-tries a connect that failed with a transient
/// local resource error before giving up. With the 100 ms→3.2 s backoff below
/// this rides out ~6 s of host-side socket-buffer / ephemeral-port pressure,
/// which is far longer than a WSAENOBUFS spike lasts in practice (TIME_WAIT
/// sockets drain in well under a second once the connect rate drops).
const MAX_CONNECT_RESOURCE_RETRIES: u32 = 6;

/// True when an `io::Error` is a *transient local* network-stack resource
/// exhaustion — the host kernel momentarily lacked socket-buffer space or a
/// free ephemeral port — as opposed to a problem with the peer.
///
/// The motivating case is Windows `WSAENOBUFS` (os error 10055) under
/// multi-stream upload churn: 4 concurrent transfer connections plus
/// resume-driven reconnects accumulate sockets in TIME_WAIT, exhausting the
/// nonpaged pool / dynamic-port range, and a fresh `connect()` (or a
/// mid-upload reconnect) fails. It is fully recoverable: the resource frees
/// within a few hundred ms, so the right response is a short backoff-and-retry,
/// never failing the whole transfer. Pre-fix this surfaced as
/// `ErrorKind::Other`, which `is_retryable_transfer_error` treats as fatal, so
/// one spike aborted the upload ("too much buffer error middle of upload").
///
/// Exposed `pub(crate)` so `transfer::is_retryable_transfer_error` can use the
/// same classification as a backstop to the in-`connect` retry below.
pub(crate) fn is_transient_local_resource_error(e: &io::Error) -> bool {
    match e.raw_os_error() {
        Some(code) => transient_resource_code(code),
        None => false,
    }
}

/// Windows variant: classify WSA error codes. `raw_os_error()` on Windows
/// sockets carries the `WSAGetLastError` value (e.g. the 10055 the user sees in
/// `(os error 10055)`), so we match the WSA code space directly.
#[cfg(windows)]
fn transient_resource_code(code: i32) -> bool {
    matches!(
        code,
        10055 /* WSAENOBUFS  — no buffer space available */
            | 10048 /* WSAEADDRINUSE — ephemeral port exhaustion under churn */
    )
}

/// POSIX variant: ENOBUFS / EADDRNOTAVAIL / EADDRINUSE map to the same
/// "host stack momentarily out of resources" condition (rare on Unix at our
/// connection rate, but the retry is harmless and keeps the two platforms'
/// behaviour aligned). Uses `libc` constants rather than hard-coded numbers
/// because ENOBUFS differs across Unixes (macOS/BSD 55, Linux 105).
#[cfg(unix)]
fn transient_resource_code(code: i32) -> bool {
    code == libc::ENOBUFS || code == libc::EADDRINUSE || code == libc::EADDRNOTAVAIL
}

#[cfg(not(any(windows, unix)))]
fn transient_resource_code(_code: i32) -> bool {
    false
}

/// Saturating buffer-size calculation for `drain_body`. Extracted as a
/// free function so it can be unit-tested without a TCP stream.
///
/// Returns `min(DRAIN_CHUNK, max(1, len_usize))` where `len_usize` is
/// `usize::try_from(len)` saturated to `DRAIN_CHUNK` on failure (only
/// possible on hypothetical >64-bit `u64` overflow targets). Caller
/// must short-circuit `len == 0` before calling — for a non-zero
/// `len` this never returns 0, which guarantees the drain loop
/// always makes forward progress.
fn drain_chunk_size(len: u64) -> usize {
    let len_usize = usize::try_from(len).unwrap_or(DRAIN_CHUNK);
    DRAIN_CHUNK.min(len_usize.max(1))
}

#[cfg(unix)]
fn tune_unix_socket_buffers(stream: &TcpStream) {
    // The 2026-04-17 best-practices audit suggested dropping SNDBUF to
    // 2 MiB on the grounds that the PS5's 512 KiB receive-buffer cap makes
    // a larger host send buffer pure bufferbloat. On the e1000-emulated
    // NIC in this lab that change produced a measured ~3-4% regression on
    // sustained large-file throughput (huge-file 88.65 -> 84.85 MiB/s),
    // with no compensating tail-latency gain that mattered. Keeping SNDBUF
    // at 8 MiB — empirically best for this class of NIC — and letting
    // `inflight_bytes` gate application-level queuing.
    use std::os::fd::AsRawFd;
    unsafe {
        let fd = stream.as_raw_fd();
        let bufsz: libc::c_int = 8 * 1024 * 1024;
        let rc_snd = libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &bufsz as *const _ as *const libc::c_void,
            std::mem::size_of_val(&bufsz) as libc::socklen_t,
        );
        if rc_snd != 0 {
            crate::core_log!(
                "connection: setsockopt(SO_SNDBUF={}) failed: {} — falling back to kernel default, throughput may regress",
                bufsz,
                std::io::Error::last_os_error(),
            );
        }
        let rc_rcv = libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &bufsz as *const _ as *const libc::c_void,
            std::mem::size_of_val(&bufsz) as libc::socklen_t,
        );
        if rc_rcv != 0 {
            crate::core_log!(
                "connection: setsockopt(SO_RCVBUF={}) failed: {} — falling back to kernel default",
                bufsz,
                std::io::Error::last_os_error(),
            );
        }
    }
}

pub struct Connection {
    stream: TcpStream,
}

/// Write every byte of `parts` to `stream` using `writev(2)`, handling
/// partial writes by advancing past already-written slices and retrying.
///
/// This replaces the old pattern of 2–3 sequential `write_all` calls per
/// frame. With `TCP_NODELAY` on, every `write(2)` risked landing in a
/// separate TCP segment (hdr in one packet, body in another); a single
/// `writev(2)` lets the kernel coalesce all parts into the same segment
/// when the total fits the current send window.
///
/// Stable-Rust note: `write_all_vectored` is nightly-only, so we implement
/// the advance-and-retry loop manually. Rust's `IoSlice` does not expose a
/// stable `advance` method either, so we slice into the underlying `&[u8]`
/// and rebuild the `IoSlice` array each iteration. Allocations are one
/// small `Vec<IoSlice>` per partial-write loop; typically zero since
/// `writev` rarely returns partial.
fn write_all_parts<W: Write>(stream: &mut W, parts: &[&[u8]]) -> io::Result<()> {
    // Copy parts into a mutable local slice so we can advance past written bytes.
    let mut cursor: [&[u8]; 4] = [&[], &[], &[], &[]];
    // A hard assert here would panic the whole engine process (and every
    // console's transfers) if a future caller passes >4 slices — surface
    // it as an io::Error instead, with a debug_assert to catch dev-time
    // misuse early.
    debug_assert!(
        parts.len() <= cursor.len(),
        "write_all_parts accepts ≤4 slices"
    );
    if parts.len() > cursor.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("write_all_parts accepts ≤4 slices, got {}", parts.len()),
        ));
    }
    for (i, p) in parts.iter().enumerate() {
        cursor[i] = p;
    }
    let mut head = 0;
    let len = parts.len();
    while head < len {
        // Skip any slices that are now empty (may happen after a partial write).
        while head < len && cursor[head].is_empty() {
            head += 1;
        }
        if head >= len {
            break;
        }
        // Build an IoSlice array referencing only the remaining unwritten slices.
        let mut iovs: [IoSlice<'_>; 4] = [
            IoSlice::new(&[]),
            IoSlice::new(&[]),
            IoSlice::new(&[]),
            IoSlice::new(&[]),
        ];
        let iov_count = len - head;
        for i in 0..iov_count {
            iovs[i] = IoSlice::new(cursor[head + i]);
        }
        let mut written = stream.write_vectored(&iovs[..iov_count])?;
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "write_vectored returned 0",
            ));
        }
        // Advance `cursor[head..]` past `written` bytes. Whole slices are
        // removed by incrementing `head`; a final partial slice is trimmed.
        while written > 0 && head < len {
            let first_len = cursor[head].len();
            if written >= first_len {
                written -= first_len;
                head += 1;
            } else {
                cursor[head] = &cursor[head][written..];
                written = 0;
            }
        }
    }
    Ok(())
}

/// Resolve a `host:port` string into the concrete socket addresses to try.
///
/// `SocketAddr`'s `FromStr` is a pure literal parser — it never consults the
/// resolver — so parsing straight into a `SocketAddr` (what this function
/// replaced) rejected every DNS name outright. Consoles reached by hostname
/// (`ps5.lan:9114`) failed at `Connection::connect` before any packet left the
/// host, which is issue #272.
///
/// Order matters: candidates are returned IPv4-first. The PS5's LAN listeners
/// are IPv4 in practice, and a name that also carries an unreachable AAAA
/// would otherwise burn a full `DEFAULT_CONNECT_TIMEOUT` on IPv6 before
/// falling back on every single frame.
///
/// The IP-literal fast path is kept ahead of `to_socket_addrs` so the common
/// case (the address the app stores after discovery) never touches the
/// resolver at all.
pub(crate) fn resolve_connect_targets(addr: &str) -> Result<Vec<SocketAddr>> {
    if let Ok(sa) = addr.parse::<SocketAddr>() {
        return Ok(vec![sa]);
    }
    let mut out: Vec<SocketAddr> = addr
        .to_socket_addrs()
        .with_context(|| format!("resolve addr: {addr}"))?
        .collect();
    if out.is_empty() {
        bail!("resolve addr: {addr} resolved to no addresses");
    }
    out.sort_by_key(|sa| !sa.is_ipv4());
    Ok(out)
}

impl Connection {
    /// Open a connection to the payload runtime control port.
    ///
    /// `addr` is `host:port`, where `host` is either an IP literal or a DNS
    /// name — see `resolve_connect_targets`.
    pub fn connect(addr: &str) -> Result<Self> {
        let targets = resolve_connect_targets(addr)?;
        // Retry transient *local* resource-exhaustion failures (Windows
        // WSAENOBUFS 10055 under multi-stream churn) with a short backoff.
        // These are not a dead peer — the host stack is momentarily out of
        // socket buffers / ephemeral ports and recovers within a few hundred
        // ms. A genuinely unreachable PS5 fails with ConnectionRefused/TimedOut
        // (not a transient-resource code), so it still fast-fails here.
        let mut attempt = 0u32;
        let stream = 'connected: loop {
            // Try every address the name resolved to before giving up. A
            // dual-stack host whose AAAA is dead must still reach the PS5 on
            // its A record; `resolve_connect_targets` puts IPv4 first so the
            // common case doesn't pay an IPv6 connect timeout to get there.
            let mut last_err: Option<io::Error> = None;
            for sa in &targets {
                match TcpStream::connect_timeout(sa, DEFAULT_CONNECT_TIMEOUT) {
                    Ok(s) => break 'connected s,
                    Err(e) => last_err = Some(e),
                }
            }
            // `resolve_connect_targets` guarantees a non-empty list, so the
            // loop above always ran at least once and set `last_err`.
            let e = match last_err {
                Some(e) => e,
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrNotAvailable,
                        format!("no addresses to connect to for {addr}"),
                    )
                    .into())
                }
            };
            if is_transient_local_resource_error(&e) && attempt < MAX_CONNECT_RESOURCE_RETRIES {
                // 100, 200, 400, 800, 1600, 3200 ms.
                let backoff = Duration::from_millis(100u64 << attempt.min(5));
                crate::core_log!(
                    "connection: connect to {} hit transient local resource error \
                     ({}); retry {}/{} after {} ms",
                    addr,
                    e,
                    attempt + 1,
                    MAX_CONNECT_RESOURCE_RETRIES,
                    backoff.as_millis(),
                );
                std::thread::sleep(backoff);
                attempt += 1;
                continue;
            }
            return Err(e).with_context(|| format!("connect to {addr}"));
        };
        stream.set_read_timeout(Some(DEFAULT_IO_TIMEOUT))?;
        stream.set_write_timeout(Some(DEFAULT_IO_TIMEOUT))?;
        stream.set_nodelay(true).context("set TCP_NODELAY")?;
        // Socket-buffer sizing for the PS5 <-> host LAN link. Unix only —
        // on Windows we rely on kernel autotuning (which is well-behaved
        // since Vista) because `libc::setsockopt` + `AsRawFd` aren't
        // available in that form and the Win32 setsockopt has a
        // different argument shape. Real-world throughput on Windows
        // through the default Winsock buffer heuristics is within a
        // few % of our tuned value on the e1000 reference NIC.
        #[cfg(unix)]
        tune_unix_socket_buffers(&stream);
        Ok(Self { stream })
    }

    pub fn set_io_timeout(&self, t: Duration) -> io::Result<()> {
        self.stream.set_read_timeout(Some(t))?;
        self.stream.set_write_timeout(Some(t))?;
        Ok(())
    }

    // ── Send ─────────────────────────────────────────────────────────────────

    /// Send a frame with the given body slice (may be empty).
    ///
    /// Uses `write_vectored` so the 28-byte frame header and the body are
    /// handed to the kernel in a single `writev(2)` call, instead of two
    /// sequential `write(2)` calls the old implementation paid. With
    /// `TCP_NODELAY` set, sequential writes risk landing in separate TCP
    /// segments — vectored writes let the kernel coalesce both parts into
    /// one packet when the body is small enough.
    pub fn send_frame(&mut self, ft: FrameType, body: &[u8]) -> Result<()> {
        self.send_frame_with_trace(ft, body, 0)
    }

    /// Like `send_frame` but stamps a caller-chosen trace_id into the
    /// header. Used by the FS_COPY / FS_MOVE job machinery: the
    /// engine generates a unique op_id at job-create time, sends the
    /// originating frame with that as trace_id, and reuses the same
    /// id when later calling FS_OP_STATUS / FS_OP_CANCEL — the
    /// payload's per-op table is keyed off this value.
    pub fn send_frame_with_trace(
        &mut self,
        ft: FrameType,
        body: &[u8],
        trace_id: u64,
    ) -> Result<()> {
        let hdr = FrameHeader::new(ft, 0, body.len() as u64, trace_id);
        let hdr_bytes = hdr.encode();
        if body.is_empty() {
            self.stream
                .write_all(&hdr_bytes)
                .context("write frame header")?;
        } else {
            write_all_parts(&mut self.stream, &[&hdr_bytes, body]).context("write frame")?;
        }
        Ok(())
    }

    /// Send a frame whose body is split across two slices (header prefix + payload).
    /// Useful for shard frames: `send_frame_split(StreamShard, &shard_hdr_bytes, shard_data)`.
    ///
    /// Three parts → one `writev(2)` instead of three `write(2)` calls.
    pub fn send_frame_split(&mut self, ft: FrameType, prefix: &[u8], rest: &[u8]) -> Result<()> {
        let body_len = (prefix.len() + rest.len()) as u64;
        let hdr = FrameHeader::new(ft, 0, body_len, 0);
        let hdr_bytes = hdr.encode();
        // Collect only non-empty slices so we don't pass zero-length IoSlices
        // (which are legal on Linux but cause extra system-call churn).
        let mut parts: [&[u8]; 3] = [&hdr_bytes, prefix, rest];
        let mut count = 0;
        let mut trimmed: [&[u8]; 3] = [&[], &[], &[]];
        for p in parts.iter_mut() {
            if !p.is_empty() {
                trimmed[count] = *p;
                count += 1;
            }
        }
        write_all_parts(&mut self.stream, &trimmed[..count]).context("write frame split")?;
        Ok(())
    }

    /// Like `send_frame_split`, but reports `rest` (the shard body) bytes into
    /// `progress` as they are accepted by the socket, in ~1 MiB increments.
    ///
    /// Why: a shard body is 64 MiB. On a slow link a whole shard lands as one
    /// progress jump every tens of seconds, so the upload's bytes-counter is
    /// flat in between and the rolling-rate readout decays toward zero —
    /// the "speed keeps dropping" sawtooth users report on Wi-Fi. Reporting the
    /// body in 1 MiB steps (the stream is an unbuffered TcpStream, so each
    /// `write_all` blocks until the kernel accepts those bytes) keeps the
    /// speed/ETA smooth and honest. Use ONLY for non-packed shards, where the
    /// body bytes equal the file-data bytes the progress denominator counts;
    /// packed (small-file) shards carry per-record framing and must keep their
    /// per-shard `planned_shard_data_len` accounting instead.
    pub fn send_frame_split_progress(
        &mut self,
        ft: FrameType,
        prefix: &[u8],
        rest: &[u8],
        progress: &std::sync::atomic::AtomicU64,
    ) -> Result<()> {
        let body_len = (prefix.len() + rest.len()) as u64;
        let hdr = FrameHeader::new(ft, 0, body_len, 0);
        let hdr_bytes = hdr.encode();
        // Frame header + shard prefix first (tiny) — one vectored write.
        let head: [&[u8]; 2] = [&hdr_bytes, prefix];
        let mut trimmed: [&[u8]; 2] = [&[], &[]];
        let mut count = 0;
        for s in head {
            if !s.is_empty() {
                trimmed[count] = s;
                count += 1;
            }
        }
        if count > 0 {
            write_all_parts(&mut self.stream, &trimmed[..count])
                .context("write frame split hdr")?;
        }
        // Body in progress-sized chunks; bump the counter as the kernel takes
        // each chunk. ~1 MiB keeps the readout smooth even at <2 MB/s without
        // adding meaningful syscall overhead on a 64 MiB shard.
        const PROGRESS_CHUNK: usize = 1 << 20;
        use std::sync::atomic::Ordering;
        for chunk in rest.chunks(PROGRESS_CHUNK) {
            self.stream
                .write_all(chunk)
                .context("write frame split body")?;
            progress.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }
        Ok(())
    }

    // ── Receive ───────────────────────────────────────────────────────────────

    /// Read just the frame header. Call `recv_body_exact` or `drain_body` next.
    pub fn recv_header(&mut self) -> Result<FrameHeader> {
        let mut buf = [0u8; FRAME_HEADER_LEN];
        self.stream
            .read_exact(&mut buf)
            .context("read frame header")?;
        FrameHeader::decode(&buf).context("decode frame header")
    }

    /// Read exactly `len` bytes into `buf`.
    pub fn recv_body_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        self.stream.read_exact(buf).context("read frame body")
    }

    /// Discard exactly `len` bytes from the stream.
    ///
    /// Sized buffer is `min(DRAIN_CHUNK, len)` so a small drain doesn't
    /// allocate 64 KiB unnecessarily. Pre-fix the buffer expression
    /// was `DRAIN_CHUNK.min(len as usize + 1)` — for a malformed
    /// `len = u64::MAX` (e.g. corrupted FrameHeader from a flaky link)
    /// the cast wrapped to `usize::MAX`, the `+ 1` overflowed to 0,
    /// the buffer was empty, every read advanced 0 bytes, and the
    /// loop ran forever. `drain_chunk_size` (extracted so it can be
    /// unit-tested in isolation) plus the `if len == 0` short-circuit
    /// close that hole.
    ///
    /// Overall deadline: each `read_exact` honors the configured per-
    /// syscall timeout (30 s default), but a peer that drips one byte
    /// every 29 s could keep us reading a 4 GiB body forever. Cap
    /// total wall-clock at 5 minutes regardless of byte count — that's
    /// well above any legitimate frame and an order of magnitude below
    /// "user-visible hang" for an interactive UI.
    pub fn drain_body(&mut self, mut len: u64) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        let chunk_size = drain_chunk_size(len);
        let mut chunk = vec![0u8; chunk_size];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        while len > 0 {
            if std::time::Instant::now() > deadline {
                bail!("drain_body deadline exceeded with {} bytes remaining", len);
            }
            let take = chunk.len().min(usize::try_from(len).unwrap_or(usize::MAX));
            self.stream
                .read_exact(&mut chunk[..take])
                .context("drain body")?;
            len -= take as u64;
        }
        Ok(())
    }

    /// Read a complete frame (header + body) into memory.
    /// Fails if `body_len > MAX_BUFFERED_BODY` — use `recv_header` + streaming
    /// for shard frames.
    pub fn recv_frame(&mut self) -> Result<(FrameHeader, Vec<u8>)> {
        let hdr = self.recv_header()?;
        if hdr.body_len > MAX_BUFFERED_BODY as u64 {
            bail!(
                "frame body too large to buffer ({} bytes); use recv_header + streaming",
                hdr.body_len
            );
        }
        let mut body = vec![0u8; hdr.body_len as usize];
        if hdr.body_len > 0 {
            self.recv_body_exact(&mut body)?;
        }
        Ok((hdr, body))
    }
}

#[cfg(test)]
mod drain_chunk_size_tests {
    //! Pin the buffer-sizing logic for `drain_body`. The combination
    //! of a `u64` parameter and a `usize` allocation is the kind of
    //! seam where a regression hides until a malformed wire frame
    //! arrives in production. Pre-2.2.61 the expression was
    //! `DRAIN_CHUNK.min(len as usize + 1)`, which produced a
    //! zero-sized buffer for `len = u64::MAX` and put the read loop
    //! into an infinite zero-byte spin.
    use super::*;

    #[test]
    fn small_lens_use_a_small_buffer() {
        // 1 byte to drain → 1 byte buffer. No point allocating
        // DRAIN_CHUNK for trivial drains.
        assert_eq!(drain_chunk_size(1), 1);
        assert_eq!(drain_chunk_size(7), 7);
    }

    #[test]
    fn at_drain_chunk_boundary_uses_full_chunk() {
        assert_eq!(drain_chunk_size(DRAIN_CHUNK as u64), DRAIN_CHUNK);
    }

    #[test]
    fn larger_than_drain_chunk_caps_at_drain_chunk() {
        // The whole point of chunked drain — we don't want one
        // 4 GiB allocation just because the body claims to be
        // 4 GiB. Cap at DRAIN_CHUNK regardless of `len`.
        assert_eq!(drain_chunk_size(DRAIN_CHUNK as u64 + 1), DRAIN_CHUNK);
        assert_eq!(drain_chunk_size(4 * 1024 * 1024 * 1024), DRAIN_CHUNK);
    }

    #[test]
    fn u64_max_does_not_produce_zero_buffer() {
        // The regression this guard exists to prevent: a malformed
        // FrameHeader.body_len = u64::MAX must still produce a
        // forward-progressing buffer. Pre-fix the buffer was 0
        // (overflow on `len as usize + 1`) and the read loop spun
        // forever, wedging the engine.
        let n = drain_chunk_size(u64::MAX);
        assert!(n > 0, "buffer must be > 0 for non-zero len");
        assert_eq!(n, DRAIN_CHUNK);
    }

    #[test]
    fn never_returns_zero_for_nonzero_len() {
        // Property: caller's contract is to short-circuit `len == 0`,
        // so for every other input drain_chunk_size must be ≥ 1.
        // This is what guarantees the drain loop terminates. A
        // sample of edge values walks the cliff cases.
        for &len in &[1u64, 2, DRAIN_CHUNK as u64 - 1, u64::MAX / 2, u64::MAX] {
            assert!(drain_chunk_size(len) > 0, "zero buffer for len={len}");
        }
    }
}

#[cfg(test)]
mod transient_resource_error_tests {
    //! Pin the WSAENOBUFS-class classification. The bug this guards against:
    //! a Windows host under multi-stream upload churn returns WSAENOBUFS
    //! (os error 10055) from `connect()`, which maps to `ErrorKind::Other`.
    //! Before the fix `is_retryable_transfer_error` treated `Other` as fatal,
    //! so one transient buffer spike aborted the whole upload. These tests
    //! lock in that the OS-code classifier recognises the transient codes and
    //! ignores unrelated ones.
    use super::*;

    #[test]
    #[cfg(windows)]
    fn wsaenobufs_is_transient() {
        let e = io::Error::from_raw_os_error(10055);
        assert!(is_transient_local_resource_error(&e));
    }

    #[test]
    #[cfg(windows)]
    fn wsaeaddrinuse_is_transient() {
        let e = io::Error::from_raw_os_error(10048);
        assert!(is_transient_local_resource_error(&e));
    }

    #[test]
    #[cfg(windows)]
    fn connection_refused_is_not_transient() {
        // WSAECONNREFUSED (10061) is a dead/closed peer — must fast-fail,
        // not retry, so a genuinely offline PS5 surfaces immediately.
        let e = io::Error::from_raw_os_error(10061);
        assert!(!is_transient_local_resource_error(&e));
    }

    #[test]
    #[cfg(unix)]
    fn enobufs_is_transient() {
        let e = io::Error::from_raw_os_error(libc::ENOBUFS);
        assert!(is_transient_local_resource_error(&e));
    }

    #[test]
    #[cfg(unix)]
    fn connection_refused_is_not_transient() {
        let e = io::Error::from_raw_os_error(libc::ECONNREFUSED);
        assert!(!is_transient_local_resource_error(&e));
    }

    #[test]
    fn non_os_error_is_not_transient() {
        // An error with no raw OS code (e.g. a synthesised ErrorKind) must
        // not be misclassified as a recoverable resource spike.
        let e = io::Error::other("synthetic");
        assert!(!is_transient_local_resource_error(&e));
    }
}

#[cfg(test)]
mod write_all_parts_tests {
    //! Pin the `write_all_parts` partial-write retry. The function
    //! is the single most subtle piece of `Connection`: under
    //! short-write pressure it has to advance past whole slices,
    //! trim a partial slice, and skip empty slices left in the
    //! middle of the cursor without ever shipping an empty IoSlice
    //! to the kernel as the only thing in the call. Every existing
    //! caller passes 1–3 slices today, so without these tests the
    //! advance logic is unexercised in CI.
    use super::*;
    use std::io::{self, IoSlice, Write};

    /// Writer that returns at most `chunk` bytes per `write_vectored`,
    /// to exercise the advance-and-retry loop. `out` accumulates
    /// everything actually written so callers can assert byte
    /// equality against the concatenated input.
    struct ShortWriter {
        out: Vec<u8>,
        chunk: usize,
    }

    impl Write for ShortWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            unreachable!("write_all_parts always uses write_vectored")
        }
        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            let mut total = 0;
            for s in bufs {
                if total >= self.chunk {
                    break;
                }
                let take = s.len().min(self.chunk - total);
                self.out.extend_from_slice(&s[..take]);
                total += take;
            }
            Ok(total)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writes_all_bytes_under_short_writes() {
        let a: &[u8] = b"AAAA";
        let b: &[u8] = b"BBBBBBBB";
        let c: &[u8] = b"CCCCCC";
        let mut w = ShortWriter {
            out: Vec::new(),
            chunk: 3,
        };
        write_all_parts(&mut w, &[a, b, c]).unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(a);
        expected.extend_from_slice(b);
        expected.extend_from_slice(c);
        assert_eq!(w.out, expected);
    }

    #[test]
    fn handles_writes_that_split_within_a_slice() {
        // chunk=3, slice "AB" (2 bytes): first writev consumes "AB"
        // and 1 byte of "CDEFGH"; subsequent writes carve up the
        // remainder. Tests the partial-slice trim path.
        let a: &[u8] = b"AB";
        let b: &[u8] = b"CDEFGH";
        let mut w = ShortWriter {
            out: Vec::new(),
            chunk: 3,
        };
        write_all_parts(&mut w, &[a, b]).unwrap();
        assert_eq!(w.out, b"ABCDEFGH");
    }

    #[test]
    fn returns_writezero_on_zero_progress() {
        // A writer that always returns 0 from write_vectored must
        // surface as ErrorKind::WriteZero rather than spinning.
        // This is the regression the explicit `if written == 0`
        // check exists for.
        struct StubbornWriter;
        impl Write for StubbornWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Ok(0)
            }
            fn write_vectored(&mut self, _: &[IoSlice<'_>]) -> io::Result<usize> {
                Ok(0)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut w = StubbornWriter;
        let err = write_all_parts(&mut w, &[b"hello"]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WriteZero);
    }

    #[test]
    fn empty_parts_list_is_a_noop() {
        let mut w = ShortWriter {
            out: Vec::new(),
            chunk: 1,
        };
        write_all_parts(&mut w, &[]).unwrap();
        assert!(w.out.is_empty());
    }

    #[test]
    fn skips_empty_slices_in_input() {
        // `send_frame_split` already pre-trims, so the production
        // path doesn't pass intermediate empties — but the function
        // documents that it tolerates them. Lock that contract in.
        let mut w = ShortWriter {
            out: Vec::new(),
            chunk: 1,
        };
        write_all_parts(&mut w, &[b"", b"X", b"", b"Y"]).unwrap();
        assert_eq!(w.out, b"XY");
    }

    #[test]
    fn fully_completes_with_writev_returning_one_byte() {
        // Pathological short writes (chunk=1) verify the loop
        // makes forward progress through every byte regardless
        // of vector layout. 28-byte FrameHeader + 100-byte body
        // approximates a real shard frame.
        let hdr = vec![0xaau8; 28];
        let body = vec![0x55u8; 100];
        let mut w = ShortWriter {
            out: Vec::new(),
            chunk: 1,
        };
        write_all_parts(&mut w, &[&hdr, &body]).unwrap();
        let mut expected = hdr.clone();
        expected.extend_from_slice(&body);
        assert_eq!(w.out, expected);
    }
}

#[cfg(test)]
mod addr_resolution_tests {
    //! Pin hostname support in `Connection::connect`.
    //!
    //! Regression guard for #272: `connect` used `addr.parse()`, which
    //! infers `SocketAddr` — and `SocketAddr`'s `FromStr` is a pure
    //! literal parser that never consults the resolver. Every FTX2 frame
    //! goes through here, so a console entered by DNS name (`ps5.lan:9114`)
    //! failed with "invalid socket address syntax" before a single packet
    //! left the host, while unrelated probes that use a resolving connect
    //! (the scene-tool port strip) lit up green — which is exactly the
    //! confusing split the reporter saw.
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr, TcpListener};

    #[test]
    fn literal_socket_addrs_resolve_to_themselves() {
        let out = resolve_connect_targets("192.168.1.131:9114").unwrap();
        assert_eq!(
            out,
            vec!["192.168.1.131:9114".parse::<SocketAddr>().unwrap()]
        );
    }

    #[test]
    fn bracketed_ipv6_literals_still_parse() {
        let out = resolve_connect_targets("[::1]:9114").unwrap();
        assert_eq!(out, vec!["[::1]:9114".parse::<SocketAddr>().unwrap()]);
    }

    #[test]
    fn hostnames_are_resolved() {
        // `localhost` is the one name every CI box resolves. It may map to
        // both ::1 and 127.0.0.1; we only assert that resolution happened
        // and produced something usable.
        let out = resolve_connect_targets("localhost:9114").unwrap();
        assert!(
            !out.is_empty(),
            "localhost must resolve to at least one addr"
        );
        assert!(out.iter().all(|sa| sa.port() == 9114));
    }

    #[test]
    fn ipv4_candidates_are_tried_first() {
        // A dual-stack name whose AAAA is unreachable would otherwise burn a
        // full connect timeout before falling back. The PS5's LAN listener is
        // IPv4 in practice, so IPv4 leads.
        let mixed = resolve_connect_targets("localhost:9114").unwrap();
        if mixed.iter().any(|sa| sa.is_ipv4()) && mixed.iter().any(|sa| sa.is_ipv6()) {
            assert!(mixed[0].is_ipv4(), "IPv4 must sort first: {mixed:?}");
        }
    }

    #[test]
    fn unresolvable_names_report_the_name() {
        let err = resolve_connect_targets("no-such-host.invalid:9114").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no-such-host.invalid:9114"),
            "error must name the address the user typed: {msg}"
        );
    }

    #[test]
    fn connect_accepts_a_hostname() {
        // The end-to-end shape of the bug: a real listener, reached by name.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let _ = listener.accept();
        });
        Connection::connect(&format!("localhost:{port}")).expect("connect by hostname must work");
        let _ = h.join();
    }
}
