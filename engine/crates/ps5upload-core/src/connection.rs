//! Blocking TCP connection with FTX2 framing.
//!
//! `Connection` is the low-level I/O primitive used by both the lab tool and
//! the host engine. It owns a `TcpStream` and provides send/recv helpers that
//! deal entirely in `FrameHeader` + raw body bytes.
//!
//! For large-body frames (shard data), use `recv_header` + `recv_body_exact` /
//! `drain_body` rather than `recv_frame`, which buffers the full body.

use std::io::{self, IoSlice, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ftx2_proto::{FrameHeader, FrameType, FRAME_HEADER_LEN};

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BUFFERED_BODY: usize = 4 * 1024 * 1024; // 4 MiB — refuse to buffer more
const DRAIN_CHUNK: usize = 64 * 1024;

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
    assert!(
        parts.len() <= cursor.len(),
        "write_all_parts accepts ≤4 slices"
    );
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

impl Connection {
    /// Open a connection to the payload runtime control port.
    pub fn connect(addr: &str) -> Result<Self> {
        let sock_addr = addr
            .parse()
            .with_context(|| format!("parse addr: {addr}"))?;
        let stream = TcpStream::connect_timeout(&sock_addr, DEFAULT_CONNECT_TIMEOUT)
            .with_context(|| format!("connect to {addr}"))?;
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
        let hdr = FrameHeader::new(ft, 0, body.len() as u64, 0);
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
    pub fn drain_body(&mut self, mut len: u64) -> Result<()> {
        let mut chunk = vec![0u8; DRAIN_CHUNK.min(len as usize + 1)];
        while len > 0 {
            let take = (len as usize).min(chunk.len());
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
