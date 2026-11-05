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

/// The longest the running helper is given to answer `node.shutdown` while a connection to the
/// loader waits. The call itself is bounded at 2 s once a session exists; finding out there is
/// no session can take a few seconds more.
pub const EVICT_BUDGET: Duration = Duration::from_secs(5);
/// Grace period for FreeBSD to recycle :9120 after the old helper exits.
pub const EVICT_GRACE: Duration = Duration::from_millis(600);

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
/// Both senders (`send_elf_to_loader` and `send_payload_image`) report a
/// failure to reach the loader as
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
/// them: the ELF-magic gate, old-payload eviction and half-close of
/// `send_payload_image`, without its stamping or Payload Manager fallback,
/// for the images the install cascade loads itself.
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
    let evict_host = should_evict_running_payload(port, image).then(|| ip.to_string());
    let sent = connect_evict_stream(ip, port, bytes, move || {
        if let Some(host) = evict_host {
            evict_running_helper(host);
        }
    });
    match sent {
        // Nothing answered on the loader, and no helper was stopped for it: launch through
        // the console's Payload Manager instead. A wedged elfldr leaves :9021 dead while
        // Payload Manager stays up (#344).
        Err(e) if falls_back_to_payload_manager(port, image, &e) => {
            match crate::payload_manager::launch_helper(ip, bytes) {
                Ok(()) => Ok(size),
                Err(pm) => Err(format!("{e} (and no fallback: {pm})")),
            }
        }
        r => r,
    }
}

/// Only the helper, only on the loader port, and only when nothing took the connection: a send
/// that broke after connecting may already be running on the console.
fn falls_back_to_payload_manager(port: u16, image: LoaderImage, err: &str) -> bool {
    port == PS5_LOADER_PORT && image == LoaderImage::Ps5Upload && err.starts_with("connect ")
}

/// Connect to the loader, run `evict`, then write the image on that same connection.
///
/// The order is the point. Stopping the running helper first and only then finding the loader
/// closed left a console with no helper and no way to send one. Connecting first means a closed
/// loader costs nothing, and holding the connection means no second, bare one is opened: a
/// loader handed a connection and no bytes must not be left waiting on it. elfldr serves one
/// connection at a time and waits for the first bytes, so a silent connection only blocks it
/// for as long as `evict` runs: [`EVICT_BUDGET`] plus [`EVICT_GRACE`], well under the 15 s read
/// deadline of our patched elfldr (the stock one has no deadline, and gets the bytes all the
/// same).
fn connect_evict_stream(
    ip: &str,
    port: u16,
    bytes: &[u8],
    evict: impl FnOnce(),
) -> Result<u64, String> {
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
    evict();
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
    Ok(bytes.len() as u64)
}

/// [`shutdown_running_payload`], given at most [`EVICT_BUDGET`] (the connection to the loader is
/// waiting), then [`EVICT_GRACE`] for the port to free. An answer that comes later changes
/// nothing: a new helper that finds an old one alive takes over by itself.
fn evict_running_helper(host: String) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(shutdown_running_payload(&host));
    });
    let _ = rx.recv_timeout(EVICT_BUDGET);
    std::thread::sleep(EVICT_GRACE);
}

/// A send that reached the console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentPayload {
    pub bytes: u64,
    /// :9021 did not answer, so the file was launched through Payload Manager on :8084.
    pub via_payload_manager: bool,
    /// One line for the user or a log, the same wording on every front end.
    pub status: String,
}

/// The most a payload file may be. The loader has no length prefix and runs whatever it reads at
/// EOF, so a wrong file picked up by mistake should stop here.
const PAYLOAD_FILE_MAX_BYTES: u64 = 128 * 1024 * 1024;
const PAYLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// A host name gets its own budget, separate from the connect one: a slow first lookup could
/// spend all of it, and the send then said "port not open" for a name that was only slow (#272).
const PAYLOAD_RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);
const PAYLOAD_SEND_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a refused :9021 is tried again before it counts as closed: elfldr leaves it closed for
/// about a second whenever it is replaced, and for 3 s after a failed accept. A desktop send has no
/// lock to wait on for that gap (the auto-loader, a playlist step).
const LOADER_REFUSED_RETRY: Duration = Duration::from_secs(8);
const LOADER_REFUSED_POLL: Duration = Duration::from_millis(500);
/// How much of a file to read for the signature: `is_ps5upload_payload` sees this window.
const PAYLOAD_PROBE_WINDOW: usize = 512 * 1024;

fn memmem_ascii(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack.len() >= needle.len()
        && haystack.windows(needle.len()).any(|w| w == needle)
}

/// True when the file about to be sent is a ps5upload helper, by file name or by the ASCII
/// signature in its section headers. Only those bind :9120, so only those contend with a running
/// helper and warrant evicting it. `head` is the leading part of the file.
pub fn is_ps5upload_payload(path: &str, head: &[u8]) -> bool {
    // Our installer daemon is a companion that runs alongside the helper, but it shares the name
    // and carries "ps5upload" in its strings: rule it out first, by its name or its log marker.
    let name = path.to_ascii_lowercase();
    let base = name.rsplit(['/', '\\']).next().unwrap_or(&name);
    if base.contains("installer") || memmem_ascii(head, b"PS5Upload installer") {
        return false;
    }
    base.contains("ps5upload")
        || memmem_ascii(head, b"ps5upload")
        || memmem_ascii(head, b"PS5UPLOAD")
}

/// Resolve on its own budget, so no lookup can hold a send for longer than that.
fn resolve_within(host: &str, addr: &str) -> Result<Vec<std::net::SocketAddr>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let name = addr.to_string();
    std::thread::Builder::new()
        .name("payload-resolve".into())
        .spawn(move || {
            let _ = tx.send(resolve_connect_targets(&name));
        })
        .map_err(|e| format!("cannot resolve \"{host}\": {e}"))?;
    match rx.recv_timeout(PAYLOAD_RESOLVE_TIMEOUT) {
        Ok(Ok(t)) => Ok(t),
        Ok(Err(e)) => Err(format!("cannot resolve \"{host}\": {e}")),
        Err(_) => Err(format!(
            "cannot resolve \"{host}\": name lookup timed out after {}s",
            PAYLOAD_RESOLVE_TIMEOUT.as_secs()
        )),
    }
}

/// Connect, run `before`, stream `image`, half-close. A failure to reach the loader starts with
/// `connect `, and then `before` has not run: stopping the running helper and only then finding the
/// loader closed left the console with neither. A refused connect is tried again for
/// `retry_refused`; a timeout already took its full budget.
fn stream_to_port(
    host: &str,
    port: u16,
    image: &[u8],
    retry_refused: Duration,
    before: impl FnOnce(),
) -> Result<u64, String> {
    let addr = join_host_port(host, port);
    let targets = resolve_within(host, &addr)?;
    let give_up = std::time::Instant::now() + retry_refused;
    let mut stream = None;
    while stream.is_none() {
        let mut last = format!("connect {addr}: no addresses to try");
        let mut was_refused = false;
        for sa in &targets {
            match TcpStream::connect_timeout(sa, PAYLOAD_CONNECT_TIMEOUT) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    was_refused = false;
                    last = format!("connect {addr}: timeout");
                }
                Err(e) => {
                    was_refused = e.kind() == std::io::ErrorKind::ConnectionRefused;
                    last = format!("connect {addr}: {e}");
                }
            }
        }
        if stream.is_none() {
            if !was_refused || std::time::Instant::now() >= give_up {
                return Err(last);
            }
            std::thread::sleep(LOADER_REFUSED_POLL);
        }
    }
    let mut stream = stream.ok_or_else(|| format!("connect {addr}: no connection"))?;
    before();
    let deadline = std::time::Instant::now() + PAYLOAD_SEND_TIMEOUT;
    for chunk in image.chunks(64 * 1024) {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err("send timed out".to_string());
        }
        stream
            .set_write_timeout(Some(left))
            .map_err(|e| format!("set write timeout: {e}"))?;
        stream.write_all(chunk).map_err(|e| match e.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                "send timed out".to_string()
            }
            _ => format!("write: {e}"),
        })?;
    }
    // The loader takes EOF as "go execute". The bytes are already in the kernel's buffer, so this
    // only fails on a socket that is really gone.
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|e| format!("shutdown: {e}"))?;
    Ok(image.len() as u64)
}

/// Launch an image through Payload Manager (:8084). The stored copy is removed once it has been
/// launched; the payload keeps running. Our own name, never the file's: Payload Manager files
/// uploads under a folder derived from the name and its cleanup clears that folder, so a user's
/// own "ps5upload" entry must not be the one we land in.
fn launch_through_payload_manager(host: &str, image: &[u8]) -> Result<u64, String> {
    let name = "ps5upload-helper.elf";
    crate::payload_manager::launch_elf(host, name, image)?;
    // Give it a moment to start before the stored copy goes.
    std::thread::sleep(Duration::from_secs(2));
    crate::payload_manager::forget(host, name);
    Ok(image.len() as u64)
}

/// Send an image to the console's loader, the one way every front end does it: refuse a non-ELF
/// on :9021, stamp a helper for this engine's trust, connect, shut down a running helper when this
/// is one, stream and half-close. When nothing answered on :9021 at all (not a send that broke mid-way,
/// which may already be running), launch it through Payload Manager instead: a wedged elfldr
/// leaves :9021 dead while Payload Manager stays up, and the helper could not be put back until
/// the user reloaded elfldr by hand (#344).
///
/// `label` is a file name or path, for the signature check and for messages. `stamp` writes this
/// engine's AVA1 trust into a helper image; a build without a slot comes back unchanged.
pub fn send_payload_image(
    host: &str,
    port: u16,
    label: &str,
    image: &[u8],
    stamp: &mut dyn FnMut(&mut Vec<u8>),
) -> Result<SentPayload, String> {
    let size = image.len() as u64;
    if size > PAYLOAD_FILE_MAX_BYTES {
        return Err(format!(
            "payload is too large ({size} bytes > {PAYLOAD_FILE_MAX_BYTES} cap)"
        ));
    }
    let mut is_helper = false;
    // Other ports serve .bin/.js/.lua/.jar flows, which don't begin with the ELF magic.
    if port == PS5_LOADER_PORT {
        if size < 4 {
            return Err(format!("not an ELF file: {label} (only {size} bytes)"));
        }
        if &image[..4] != b"\x7FELF" {
            return Err(format!(
                "not an ELF file: {label} (first 4 bytes {:02x?})",
                &image[..4]
            ));
        }
        is_helper = is_ps5upload_payload(label, &image[..image.len().min(PAYLOAD_PROBE_WINDOW)]);
    }
    let image_kind = if is_helper {
        LoaderImage::Ps5Upload
    } else {
        LoaderImage::Companion
    };
    let stamped;
    let to_send: &[u8] = if is_helper {
        let mut v = image.to_vec();
        stamp(&mut v);
        stamped = v;
        &stamped
    } else {
        image
    };
    let retry_refused = if port == PS5_LOADER_PORT {
        LOADER_REFUSED_RETRY
    } else {
        Duration::ZERO
    };
    let evict = should_evict_running_payload(port, image_kind);
    let sent = stream_to_port(host, port, to_send, retry_refused, || {
        if evict {
            evict_running_helper(host.to_string());
        }
    });
    match sent {
        Ok(n) => Ok(SentPayload {
            bytes: n,
            via_payload_manager: false,
            status: format!("sent {n} bytes to {host}:{port}"),
        }),
        Err(e) if port == PS5_LOADER_PORT && e.starts_with("connect ") => {
            let mut v = image.to_vec();
            stamp(&mut v);
            match launch_through_payload_manager(host, &v) {
                Ok(n) => Ok(SentPayload {
                    bytes: n,
                    via_payload_manager: true,
                    status: format!(
                        "{e}; launched {n} bytes through Payload Manager on {host}:8084 instead"
                    ),
                }),
                Err(pm) => Err(format!("{e} (and no fallback: {pm})")),
            }
        }
        Err(e) => Err(e),
    }
}

/// [`send_payload_image`] for a file on this machine.
pub fn send_payload_file(
    host: &str,
    path: &str,
    port: u16,
    stamp: &mut dyn FnMut(&mut Vec<u8>),
) -> Result<SentPayload, String> {
    let size = std::fs::metadata(path)
        .map_err(|e| format!("open {path}: {e}"))?
        .len();
    if size > PAYLOAD_FILE_MAX_BYTES {
        return Err(format!(
            "payload is too large ({size} bytes > {PAYLOAD_FILE_MAX_BYTES} cap)"
        ));
    }
    let image = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
    send_payload_image(host, port, path, &image, stamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_port_runs_nothing_and_a_refused_one_is_tried_again() {
        use std::io::Read;
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let mut ran = false;
        let e = stream_to_port("127.0.0.1", port, b"x", Duration::ZERO, || ran = true).unwrap_err();
        assert!(e.starts_with("connect "), "{e}");
        assert!(
            !ran,
            "the helper must not be stopped for a loader that is closed"
        );

        let opens = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(700));
            let l = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
            let (mut s, _) = l.accept().unwrap();
            let mut got = Vec::new();
            s.read_to_end(&mut got).unwrap();
            got
        });
        let mut ran = false;
        let sent = stream_to_port("127.0.0.1", port, b"abc", Duration::from_secs(5), || {
            ran = true
        });
        assert_eq!(sent, Ok(3));
        assert!(ran);
        assert_eq!(opens.join().unwrap(), b"abc");
    }

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

    /// A closed loader must cost the console nothing: the running helper is stopped only once
    /// the loader has taken the connection.
    #[test]
    fn a_closed_loader_never_stops_the_helper() {
        use std::net::TcpListener;
        // Bind and drop: the port is free and refuses.
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("free port")
            .port();
        let mut evicted = false;
        let err = connect_evict_stream("127.0.0.1", port, b"\x7FELF", || evicted = true)
            .expect_err("nothing listens");
        assert!(err.starts_with("connect "), "{err}");
        assert!(
            !evicted,
            "the helper was stopped for a loader that is not there"
        );
    }

    /// One connection, opened before the eviction, carries the whole image. A second connection
    /// or a write before the eviction would fail this.
    #[test]
    fn evicts_after_connecting_and_streams_on_the_same_connection() {
        use std::io::Read;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let image = b"\x7FELF-the-new-helper".to_vec();
        let mut seen_at_eviction = None;
        let sent = connect_evict_stream("127.0.0.1", port, &image, || {
            // The loader has the connection and no bytes yet.
            let (mut sock, _) = listener.accept().expect("connected before the eviction");
            sock.set_nonblocking(true).expect("nonblocking");
            let mut b = [0u8; 1];
            assert!(sock.read(&mut b).is_err(), "bytes came before the eviction");
            sock.set_nonblocking(false).expect("blocking");
            seen_at_eviction = Some(sock);
        })
        .expect("send");
        assert_eq!(sent, image.len() as u64);
        let mut got = Vec::new();
        seen_at_eviction
            .expect("eviction ran")
            .read_to_end(&mut got)
            .expect("read to EOF");
        assert_eq!(got, image);
    }

    /// The silent connection has to end well inside the patched elfldr's 15 s read deadline.
    #[test]
    fn the_silent_gap_stays_inside_the_loaders_deadline() {
        assert!(EVICT_BUDGET + EVICT_GRACE < Duration::from_secs(15));
    }

    #[test]
    fn only_a_refused_helper_send_goes_to_payload_manager() {
        let refused = "connect 1.2.3.4:9021: refused";
        assert!(falls_back_to_payload_manager(
            PS5_LOADER_PORT,
            LoaderImage::Ps5Upload,
            refused
        ));
        assert!(!falls_back_to_payload_manager(
            PS5_LOADER_PORT,
            LoaderImage::Companion,
            refused
        ));
        assert!(!falls_back_to_payload_manager(
            9020,
            LoaderImage::Ps5Upload,
            refused
        ));
        // Past the connect: it may already be running.
        assert!(!falls_back_to_payload_manager(
            PS5_LOADER_PORT,
            LoaderImage::Ps5Upload,
            "write 1.2.3.4:9021: broken pipe"
        ));
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

    fn no_stamp() -> impl FnMut(&mut Vec<u8>) {
        |_: &mut Vec<u8>| {}
    }

    fn temp_file(name: &str, content: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ps5upload-send-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    fn listen_once() -> (u16, std::thread::JoinHandle<Vec<u8>>) {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut got = Vec::new();
            sock.read_to_end(&mut got).unwrap();
            got
        });
        (port, h)
    }

    #[test]
    fn a_file_that_is_not_an_elf_stops_before_the_loader_port() {
        let p = temp_file(
            "fake.elf",
            b"NOT AN ELF, ENOUGH BYTES TO PASS THE SIZE CHECK",
        );
        let e = send_payload_file(
            "127.0.0.1",
            p.to_str().unwrap(),
            PS5_LOADER_PORT,
            &mut no_stamp(),
        )
        .unwrap_err();
        assert!(e.contains("not an ELF"), "{e}");
        let p = temp_file("tiny.elf", b"AB");
        let e = send_payload_file(
            "127.0.0.1",
            p.to_str().unwrap(),
            PS5_LOADER_PORT,
            &mut no_stamp(),
        )
        .unwrap_err();
        assert!(e.contains("not an ELF"), "{e}");
    }

    #[test]
    fn other_ports_take_other_file_types() {
        let (port, h) = listen_once();
        let body = b"#!/usr/bin/env lua\nprint('hi')\n";
        let p = temp_file("exploit.lua", body);
        let sent =
            send_payload_file("127.0.0.1", p.to_str().unwrap(), port, &mut no_stamp()).unwrap();
        assert_eq!(sent.bytes, body.len() as u64);
        assert!(!sent.via_payload_manager);
        assert_eq!(h.join().unwrap(), body);
    }

    #[test]
    fn a_companion_elf_is_sent_as_is_and_not_stamped() {
        let (port, h) = listen_once();
        let mut elf = b"\x7FELF".to_vec();
        elf.extend(vec![0xabu8; 200 * 1024]);
        let p = temp_file("ok.elf", &elf);
        let mut stamped = false;
        let sent = send_payload_file("127.0.0.1", p.to_str().unwrap(), port, &mut |_| {
            stamped = true;
        })
        .unwrap();
        assert_eq!(sent.bytes, elf.len() as u64);
        assert_eq!(
            sent.status,
            format!("sent {} bytes to 127.0.0.1:{port}", elf.len())
        );
        assert_eq!(h.join().unwrap(), elf);
        assert!(!stamped);
    }

    #[test]
    fn the_size_cap_applies_before_anything_is_read() {
        let p = temp_file("huge.elf", b"");
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_len(PAYLOAD_FILE_MAX_BYTES + 1)
            .unwrap();
        let e = send_payload_file(
            "127.0.0.1",
            p.to_str().unwrap(),
            PS5_LOADER_PORT,
            &mut no_stamp(),
        )
        .unwrap_err();
        assert!(e.contains("too large"), "{e}");
    }

    #[test]
    fn a_missing_file_is_named() {
        let e =
            send_payload_file("127.0.0.1", "/no/such/file.elf", 1, &mut no_stamp()).unwrap_err();
        assert!(e.contains("/no/such/file.elf"), "{e}");
    }

    #[test]
    fn a_dead_port_that_is_not_the_loader_gets_no_fallback() {
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let e = send_payload_image("127.0.0.1", closed, "x.elf", b"\x7FELFabc", &mut no_stamp())
            .unwrap_err();
        assert!(e.starts_with("connect "), "{e}");
        assert!(
            !e.contains("Payload Manager") && !e.contains("fallback"),
            "{e}"
        );
    }

    #[test]
    fn a_name_that_never_resolves_says_so() {
        let e = send_payload_image(
            "no-such-host.invalid",
            9020,
            "x.elf",
            b"\x7FELFabc",
            &mut no_stamp(),
        )
        .unwrap_err();
        assert!(e.starts_with("cannot resolve"), "{e}");
    }

    #[test]
    fn ps5upload_detection_gates_eviction() {
        assert!(is_ps5upload_payload(
            "/x/ps5upload.elf",
            b"\x7FELF\x00garbage"
        ));
        // A user-renamed copy is told by its signature, in either case.
        assert!(is_ps5upload_payload(
            "/x/helper.elf",
            b"\x7FELF .... ps5upload v2 ...."
        ));
        assert!(is_ps5upload_payload("/x/HELPER.ELF", b"\x7FELF PS5UPLOAD"));
        // The installer daemon and other scene ELFs leave a running helper alone.
        assert!(!is_ps5upload_payload(
            "/x/ps5upload-installer.elf",
            b"\x7FELF some other daemon"
        ));
        assert!(!is_ps5upload_payload(
            "/x/daemon.elf",
            b"\x7FELF ps5upload path, PS5Upload installer log marker"
        ));
        assert!(!is_ps5upload_payload("/x/exploit.elf", b"\x7FELF\x00\x00"));
    }
}
