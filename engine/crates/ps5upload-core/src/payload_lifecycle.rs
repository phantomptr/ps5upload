//! Best-effort "tell the currently-running payload to exit so a fresh
//! one can take its place" RPC.
//!
//! Why this exists: the PS5 ELF loader (port 9021) is fire-and-forget —
//! when the desktop pushes new payload bytes it spawns a fresh process,
//! but the OLD payload process is unaware and keeps running. The two
//! contend for the same listening port (:9120). On most firmwares the
//! new payload's bind fails, the new process exits, and the user is
//! left with the OLD payload still answering — but now with
//! expectations that may not match the desktop's current build. Symptom:
//! the user's "I sent the payload but nothing changed" report.
//!
//! The fix is desktop-side: BEFORE pushing fresh ELF bytes to :9021,
//! ask the running helper to exit with `node.shutdown` over the paired
//! AVA1 session. The payload's shutdown handler sets a flag the main loop
//! honours; the old process exits, its port goes free, the new payload's
//! bind succeeds. A new payload that starts while an old one is alive
//! takes over by itself (the payload's takeover, flag file).
//!
//! Best-effort by design — every error path returns Ok(false) because
//! "no old payload running" is the common case (first session boot,
//! console reboot, etc) and we don't want to block the send.

use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;

use crate::mgmt::{self, m};
use crate::net::resolve_connect_targets;

/// The PS5 ELF loader's well-known port. Bytes written here are executed
/// as a fresh process once the sender half-closes the socket.
pub const PS5_LOADER_PORT: u16 = 9021;

/// Why a `dpi-ensure` failed, in a form a UI can branch on. Prose is a bad
/// contract: 5.17.6 collapsed three unrelated causes into one message that
/// told a user to rebuild their engine when the real problem was that their
/// console's ELF loader had stopped answering on :9021.
pub const DPI_REASON_NO_IMAGE: &str = "no_image";
pub const DPI_REASON_LOADER_UNREACHABLE: &str = "loader_unreachable";
pub const DPI_REASON_LOADER_SEND_FAILED: &str = "loader_send_failed";
pub const DPI_REASON_NO_BRINGUP: &str = "no_bringup";

/// Classify a loader-send failure.
///
/// Both senders (`send_elf_to_loader` here, and the desktop's
/// `do_payload_send`) report a failure to reach the loader as
/// `connect <addr>: <cause>`; every other failure they can return happens
/// after the socket is up. Keying off that prefix keeps the distinction the
/// user needs — "your loader isn't listening" vs "the send broke" — without a
/// pre-flight probe, which we deliberately avoid: connecting to an ELF loader
/// and closing without sending bytes can make it execute an empty image.
pub fn dpi_send_failure_reason(err: &str) -> &'static str {
    if err.starts_with("connect ") {
        DPI_REASON_LOADER_UNREACHABLE
    } else {
        DPI_REASON_LOADER_SEND_FAILED
    }
}

/// The PS5Upload installer daemon's port. It replaced the old standalone DPI
/// daemon (which listened on :9040); the daemon loads as a companion image on
/// the :9021 loader and binds this port.
pub const INSTALLER_PORT: u16 = 9115;

/// Refuse to stream anything larger than this to the loader. The PS5's
/// loader has no length prefix — it executes whatever it read at EOF — so
/// a wrong file picked up by a path/embed mistake should fail here rather
/// than be handed to the console.
const ELF_SEND_MAX_BYTES: u64 = 64 * 1024 * 1024;

const ELF_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const ELF_SEND_TIMEOUT: Duration = Duration::from_secs(60);

/// Ask the helper running at `mgmt_addr` (the console's host; a `:port` suffix is ignored, the
/// AVA1 session uses its own port) to exit: `node.shutdown` over the paired session.
///
/// Returns Ok(true) iff the helper acknowledged. Ok(false) on any failure (nothing listening, not
/// paired, ACK timeout): the caller proceeds as if there were no payload to displace, which is the
/// right behaviour for the "first-send-of-the-session" path. The 2 s deadline is the old one: the
/// handler is a flag flip and a tiny reply; if it takes longer we would rather give up and let
/// the new payload's own takeover (or the bind error) say what is really wrong.
pub fn shutdown_running_payload(mgmt_addr: &str) -> std::io::Result<bool> {
    // `node.shutdown` through the management seam: over AVA1. Any failure, a refusal for not being paired included, is "nothing to displace".
    Ok(mgmt::call_with(
        mgmt_addr,
        m::NODE_SHUTDOWN,
        "SHUTDOWN",
        &[],
        Some(Duration::from_secs(2)),
    )
    .is_ok())
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

/// Probe `host:port`, keeping "the name never resolved" distinct from
/// "nothing answered".
///
/// The desktop client's `port_check` learned to separate these in #272:
/// collapsing them made a DNS typo read as "your PS5 isn't jailbroken".
/// The browser has no sockets of its own, so a self-hosted web UI has to
/// borrow the engine's — and the engine is the better prober anyway, since
/// it sits on the console's LAN while the browser may not.
pub fn probe_port(host: &str, port: u16, timeout: Duration) -> Result<(), String> {
    let addr = join_host_port(host, port);
    let targets = resolve_connect_targets(&addr).map_err(|e| format!("resolve {host}: {e}"))?;
    if targets.is_empty() {
        return Err(format!("resolve {host}: no addresses"));
    }
    let mut last = String::from("no addresses to try");
    for sa in &targets {
        match TcpStream::connect_timeout(sa, timeout) {
            Ok(_) => return Ok(()),
            Err(e) => last = format!("connect {sa}: {e}"),
        }
    }
    Err(last)
}

/// What is being loaded, which decides whether a payload already running
/// on the console has to be shut down first.
///
/// Only ps5upload binds :9120. Sending it while an older instance is
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
/// client's embedded copy of that daemon (the web UI half of #152).
///
/// Whether to shut a running payload down before streaming these bytes.
///
/// Pulled out as a pure predicate because getting it wrong is silent in
/// both directions: too eager tears the helper down on every patch
/// install, too lax leaves the old process holding its port while the new one
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
        let _ = shutdown_running_payload(ip);
        // Grace period for FreeBSD to recycle the port after the old process
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

    /// The interesting failure mode for callers is "nothing listening on
    /// that addr" — assert that surfaces as Ok(false), not Err. The
    /// acknowledged path is covered with a scripted transport below.
    #[test]
    fn nothing_listening_returns_ok_false() {
        // 198.51.100.0/24 is RFC 5737 TEST-NET-2; nothing should answer.
        let res = shutdown_running_payload("198.51.100.1");
        match res {
            Ok(false) => {}
            other => panic!("expected Ok(false), got {other:?}"),
        }
    }

    struct Scripted {
        ok: bool,
        seen: std::sync::Mutex<Vec<(u16, Vec<u8>)>>,
    }
    impl crate::mgmt::MgmtTransport for Scripted {
        fn call(
            &self,
            _addr: &str,
            method: crate::mgmt::Method,
            label: &str,
            body: &[u8],
            _timeout: Duration,
        ) -> anyhow::Result<Option<Vec<u8>>> {
            self.seen.lock().unwrap().push((method.id, body.to_vec()));
            if self.ok {
                Ok(Some(b"{}".to_vec()))
            } else {
                Err(crate::mgmt::MgmtError {
                    label: label.to_string(),
                    status: 3,
                    cause: "unpaired".into(),
                }
                .into())
            }
        }
    }

    /// The shutdown goes out as `node.shutdown` (method 5, empty body) through the management
    /// seam, so it rides the paired AVA1 session; a refusal is Ok(false).
    #[test]
    fn shutdown_is_node_shutdown_over_the_seam() {
        let t = std::sync::Arc::new(Scripted {
            ok: true,
            seen: Default::default(),
        });
        let _g = crate::mgmt::scoped_transport(t.clone());
        assert!(shutdown_running_payload("10.0.0.2").unwrap());
        assert_eq!(t.seen.lock().unwrap().as_slice(), &[(5u16, Vec::new())]);

        let t = std::sync::Arc::new(Scripted {
            ok: false,
            seen: Default::default(),
        });
        let _g = crate::mgmt::scoped_transport(t);
        assert!(!shutdown_running_payload("10.0.0.2").unwrap());
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
    /// ps5upload payload; only the helper itself contends for :9120.
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

    #[test]
    fn probe_port_separates_a_bad_name_from_a_dead_host() {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        assert!(probe_port("127.0.0.1", port, Duration::from_secs(2)).is_ok());

        // A name that cannot resolve and a host that never answers are
        // different problems, and the Connection screen says so (#272).
        // Collapsing them is what made a typo read as "not jailbroken".
        let unresolvable = probe_port("no-such-host.invalid", 9020, Duration::from_millis(300))
            .expect_err("should not resolve");
        assert!(
            unresolvable.starts_with("resolve "),
            "want a resolve error, got: {unresolvable}"
        );

        let unreachable = probe_port("198.51.100.1", 9020, Duration::from_millis(300))
            .expect_err("nothing listens on TEST-NET-2");
        assert!(
            unreachable.starts_with("connect "),
            "want a connect error, got: {unreachable}"
        );
    }
}
