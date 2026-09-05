//! Best-effort "tell the currently-running payload to exit so a fresh
//! one can take its place" RPC.
//!
//! Why this exists: the PS5 ELF loader (port 9021) is fire-and-forget —
//! when the desktop pushes new payload bytes it spawns a fresh process,
//! but the OLD payload process is unaware and keeps running. The two
//! contend for the same management port (:9114). On most firmwares the
//! new payload's bind(:9114) fails, the new process exits, and the
//! user is left with the OLD payload still answering — but now with
//! wire-protocol expectations that may not match the desktop's current
//! build. Symptoms: install RPCs that bail with "read frame header"
//! because the old payload's frame handler doesn't understand a newer
//! frame, lingering :9113 transfers, the user's "I sent the payload
//! but nothing changed" report.
//!
//! The fix is desktop-side: BEFORE pushing fresh ELF bytes to :9021,
//! send a Shutdown frame to the existing :9114. The payload's
//! shutdown handler sets a flag the main loop honours; the old
//! process exits, its ports go free, the new payload's bind succeeds.
//!
//! Best-effort by design — every error path returns Ok(false) because
//! "no old payload running" is the common case (first session boot,
//! console reboot, etc) and we don't want to block the send.

use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;

use ftx2_proto::FrameType;

use crate::connection::{resolve_connect_targets, Connection};

/// The PS5 ELF loader's well-known port. Bytes written here are executed
/// as a fresh process once the sender half-closes the socket.
pub const PS5_LOADER_PORT: u16 = 9021;

/// The standalone DPI install daemon's port (`payload/dpi/`, and the same
/// port scene daemons like etaHEN/ezRemote listen on).
pub const DPI_DAEMON_PORT: u16 = 9040;

/// Refuse to stream anything larger than this to the loader. The PS5's
/// loader has no length prefix — it executes whatever it read at EOF — so
/// a wrong file picked up by a path/embed mistake should fail here rather
/// than be handed to the console.
const ELF_SEND_MAX_BYTES: u64 = 64 * 1024 * 1024;

const ELF_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const ELF_SEND_TIMEOUT: Duration = Duration::from_secs(60);

/// Send a Shutdown frame to the payload running at `mgmt_addr`
/// (typically `<ps5-ip>:9114`).
///
/// Returns Ok(true) iff a Shutdown_Ack was received — i.e. we hit a
/// real ps5upload payload and it acknowledged. Ok(false) on any
/// failure (nothing listening, wrong process answering, ACK timeout):
/// the caller proceeds as if there were no payload to displace, which
/// is the right behaviour for the "first-send-of-the-session" path.
///
/// IO timeout is tightened from Connection's default 30 s down to
/// 2 s for both the frame send and the ACK read — the payload's
/// shutdown handler is one mutex flip and a 2-byte response; if
/// either takes longer than 2 s we'd rather give up and let the
/// new payload's bind tell the user whatever is really wrong.
pub fn shutdown_running_payload(mgmt_addr: &str) -> std::io::Result<bool> {
    let mut c = match Connection::connect(mgmt_addr) {
        Ok(c) => c,
        Err(_) => return Ok(false),
    };
    // Tighten IO timeout for the small handshake. A failure here is
    // surprising (the TCP socket is freshly connected) so we surface
    // it as a real io::Error — but the caller treats anything ≠ true
    // the same way, so this still degrades gracefully.
    c.set_io_timeout(Duration::from_secs(2))?;
    if c.send_frame(FrameType::Shutdown, &[]).is_err() {
        return Ok(false);
    }
    match c.recv_frame() {
        Ok((hdr, _)) => {
            let ft = hdr.frame_type().unwrap_or(FrameType::Error);
            Ok(ft == FrameType::ShutdownAck)
        }
        Err(_) => Ok(false),
    }
}

/// Join a bare host/IP with a port. A bare IPv6 literal has to be
/// bracketed or `resolve_connect_targets` parses the last hextet as the
/// port — the same trap `strip_host_port` documents on the engine side.
pub fn join_host_port(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// True when `addr` accepts a TCP connection within `timeout`.
///
/// Used to answer "is the DPI daemon already listening on :9040?" without
/// speaking its protocol — a connect is the same liveness signal the
/// desktop client's `dpi_ensure` uses, and it must stay cheap because the
/// install cascade calls it before and after loading the daemon.
pub fn port_is_open(addr: &str, timeout: Duration) -> bool {
    let Ok(targets) = resolve_connect_targets(addr) else {
        return false;
    };
    targets
        .iter()
        .any(|sa| TcpStream::connect_timeout(sa, timeout).is_ok())
}

/// What is being loaded, which decides whether a payload already running
/// on the console has to be shut down first.
///
/// Only ps5upload binds :9114/:9113. Sending it while an older instance is
/// still alive means the new process loses the bind and the console keeps
/// answering with the old one — the "I sent the payload but nothing
/// changed" class of report. A companion daemon binds other ports and must
/// load ALONGSIDE the helper instead; evicting for one would tear the
/// helper down on every patch install.
///
/// This is a caller declaration rather than a sniff of the bytes on
/// purpose. The obvious heuristic — look for the "ps5upload" ASCII
/// signature — does not work on a bounded read: in the shipped payload
/// that string first appears about 1.4 MB in, past any window small enough
/// to be worth scanning. Every caller here already knows which image it
/// holds, so it says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderImage {
    /// The ps5upload payload itself.
    Ps5Upload,
    /// A daemon that coexists with it (the DPI installer on :9040, scene
    /// tools).
    Companion,
}

/// Stream raw ELF bytes to the PS5's loader and half-close so it executes
/// them. This is the engine-side twin of the desktop client's
/// `do_payload_send`: same ELF-magic gate, same old-payload eviction, same
/// bounded half-close.
///
/// It exists because the install cascade's DPI fallback — the only path
/// that lands a game *patch* — needs to load a daemon onto the console,
/// and a browser can neither open a TCP socket nor reach the desktop
/// client's embedded copy of that daemon (issue #295).
///
/// Whether to shut a running payload down before streaming these bytes.
///
/// Pulled out as a pure predicate because getting it wrong is silent in
/// both directions: too eager tears the helper down on every patch
/// install, too lax leaves the old process holding :9114 while the new one
/// exits. A non-loader port is a scene loader on its own port, which never
/// contends with our helper.
fn should_evict_running_payload(port: u16, image: LoaderImage) -> bool {
    port == PS5_LOADER_PORT && image == LoaderImage::Ps5Upload
}

/// Eviction is gated on `image` — see `LoaderImage` for why that is a
/// declaration and not a guess.
pub fn send_elf_to_loader(
    ip: &str,
    port: u16,
    bytes: &[u8],
    image: LoaderImage,
) -> Result<u64, String> {
    let size = bytes.len() as u64;
    if size > ELF_SEND_MAX_BYTES {
        return Err(format!(
            "payload is too large ({size} bytes > {ELF_SEND_MAX_BYTES} cap)"
        ));
    }
    if bytes.len() < 4 || &bytes[..4] != b"\x7FELF" {
        return Err(format!(
            "not an ELF image (first bytes {:02x?})",
            &bytes[..bytes.len().min(4)]
        ));
    }
    if should_evict_running_payload(port, image) {
        let mgmt_addr = join_host_port(ip, 9114);
        let _ = shutdown_running_payload(&mgmt_addr);
        // Grace period for FreeBSD to recycle :9114 after the old process
        // exits — the same 600 ms the desktop send waits.
        std::thread::sleep(Duration::from_millis(600));
    }

    let addr = join_host_port(ip, port);
    let targets = resolve_connect_targets(&addr).map_err(|e| format!("resolve {addr}: {e}"))?;
    let mut last_err = String::new();
    let mut stream = None;
    for sa in &targets {
        match TcpStream::connect_timeout(sa, ELF_CONNECT_TIMEOUT) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last_err = format!("connect {addr}: {e}"),
        }
    }
    let mut stream = stream.ok_or(last_err)?;
    stream
        .set_write_timeout(Some(ELF_SEND_TIMEOUT))
        .map_err(|e| format!("set write timeout: {e}"))?;
    stream
        .write_all(bytes)
        .map_err(|e| format!("write {addr}: {e}"))?;
    stream.flush().map_err(|e| format!("flush {addr}: {e}"))?;
    // The loader treats EOF on the write side as "go execute". A failure
    // here is not fatal on its own — the bytes are already in the kernel's
    // send buffer — but report it so a wedged console is visible.
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|e| format!("half-close {addr}: {e}"))?;
    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// We can't easily spin up a fake payload server in a unit test
    /// (Connection wants raw FTX2 framing and we'd be re-implementing
    /// the server side just to handshake). The interesting failure
    /// mode for callers is "nothing listening on that addr" — assert
    /// that surfaces as Ok(false), not Err.
    #[test]
    fn nothing_listening_returns_ok_false() {
        // 198.51.100.0/24 is RFC 5737 TEST-NET-2; nothing should answer.
        let res = shutdown_running_payload("198.51.100.1:9114");
        match res {
            Ok(false) => {}
            other => panic!("expected Ok(false), got {other:?}"),
        }
    }

    /// A non-ELF blob must never reach the loader. The loader has no
    /// length prefix and executes whatever it read at EOF, so an embed or
    /// path mistake that hands it a text file would run garbage on the
    /// console. Fail in the sender instead.
    #[test]
    fn refuses_bytes_that_are_not_an_elf() {
        let err = send_elf_to_loader(
            "127.0.0.1",
            PS5_LOADER_PORT,
            b"not an elf at all",
            LoaderImage::Companion,
        )
        .expect_err("non-ELF bytes must be rejected");
        assert!(err.contains("not an ELF"), "unexpected error: {err}");
    }

    /// The loader reads until EOF, so the send is only complete once the
    /// write side is half-closed. Assert both halves of that contract: the
    /// listener sees every byte, and its read returns 0 without us closing
    /// the whole socket.
    #[test]
    fn streams_the_image_and_half_closes() {
        use std::io::Read;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            let mut got = Vec::new();
            sock.read_to_end(&mut got).expect("read to EOF");
            got
        });

        // Declared a companion, so the send must not try to evict a
        // running helper first.
        let mut image = b"\x7FELF".to_vec();
        image.extend_from_slice(&[0xAAu8; 4096]);
        let sent =
            send_elf_to_loader("127.0.0.1", port, &image, LoaderImage::Companion).expect("send");

        assert_eq!(sent, image.len() as u64);
        assert_eq!(handle.join().expect("joined"), image);
    }

    /// The DPI daemon binds :9040 and must load ALONGSIDE a running
    /// ps5upload payload; only the helper itself contends for :9114.
    /// Getting this backwards tears the helper down on every patch
    /// install — or, the way it first shipped, silently never evicts,
    /// because the "ps5upload" signature this used to sniff for sits ~1.4
    /// MB into the image, past any bounded read.
    #[test]
    fn only_the_helper_triggers_eviction() {
        assert!(should_evict_running_payload(
            PS5_LOADER_PORT,
            LoaderImage::Ps5Upload
        ));
        assert!(!should_evict_running_payload(
            PS5_LOADER_PORT,
            LoaderImage::Companion
        ));
        // A scene loader on its own port isn't the ELF loader we know how
        // to reason about; leave whatever is running alone.
        assert!(!should_evict_running_payload(9020, LoaderImage::Ps5Upload));
    }

    #[test]
    fn port_is_open_reports_a_live_listener() {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        assert!(port_is_open(&addr, Duration::from_secs(2)));
        // TEST-NET-2 — nothing answers, and the probe must say so rather
        // than hang the install cascade.
        assert!(!port_is_open(
            "198.51.100.1:9040",
            Duration::from_millis(300)
        ));
    }
}
