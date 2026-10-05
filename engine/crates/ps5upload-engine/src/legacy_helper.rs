//! MIGRATION SHIM: this file is deleted in the release after the AVA1 cutover.
//!
//! A console that still runs a helper released before the cutover answers only the old binary
//! protocol on :9113/:9114. The new app cannot talk to it, so before it sends the new helper it
//! has to recognise that helper, ask it to exit and wait for its ports to close. This is the only
//! engine code that speaks the old protocol; the constants are inlined on purpose (no `ftx2-proto`).
//!
//! The state and error tokens are stable (the client matches on them): see `protocol/ava1/CLIENT_CONTRACT.md`.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// State token: an older helper is running (it only speaks the old protocol).
pub const HELPER_OLD: &str = "helper_old";
/// State token: a new helper whose AVA1 server is not listening yet. Wait; do not replace.
pub const STARTING: &str = "starting";
/// State token: a new helper whose AVA1 server failed to start (replacing sends the same build).
pub const AVA1_FAILED: &str = "ava1_failed";
/// State token: nothing answers (the existing send-payload flow).
pub const NOT_RUNNING: &str = "not_running";
/// State token: the AVA1 port accepts connections.
pub const AVA1: &str = "ava1";
/// Error token: the older helper did not exit after its shutdown.
pub const LEGACY_HELPER_WEDGED: &str = "legacy_helper_wedged";

const MAGIC: u32 = 0x3258_5446; // "FTX2", little endian on the wire
const VERSION: u16 = 1;
const HELLO: u16 = 1;
const SHUTDOWN: u16 = 22;
const SHUTDOWN_ACK: u16 = 23;
const HEADER_LEN: usize = 28;

/// The ports of a console's helper. Tests point these at loopback listeners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ports {
    /// The old management port.
    pub mgmt: u16,
    /// The old transfer port (a build from before the port split only has this one).
    pub transfer: u16,
    /// The AVA1 port the new helper listens on.
    pub ava1: u16,
}

impl Default for Ports {
    fn default() -> Self {
        Ports {
            mgmt: 9114,
            transfer: 9113,
            ava1: 9120,
        }
    }
}

/// How long the old helper gets to close both ports (as the payload's own takeover), and the new one to open 9120.
pub const WAIT_CLOSE: Duration = Duration::from_secs(10);
pub const WAIT_AVA1: Duration = Duration::from_secs(20);

const IO_TIMEOUT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(100);

/// A body-less old-protocol frame.
pub fn frame(frame_type: u16) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    h[4..6].copy_from_slice(&VERSION.to_le_bytes());
    h[6..8].copy_from_slice(&frame_type.to_le_bytes());
    // flags, body_len and trace_id are zero
    h
}

fn targets(host: &str, port: u16) -> Vec<SocketAddr> {
    let addr = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    addr.to_socket_addrs()
        .map(|i| i.collect())
        .unwrap_or_default()
}

fn connect(host: &str, port: u16) -> Option<TcpStream> {
    for sa in targets(host, port) {
        if let Ok(s) = TcpStream::connect_timeout(&sa, IO_TIMEOUT) {
            let _ = s.set_read_timeout(Some(IO_TIMEOUT));
            let _ = s.set_write_timeout(Some(IO_TIMEOUT));
            return Some(s);
        }
    }
    None
}

/// Sends one body-less frame and reads the reply: its frame type and (up to 1 KiB of) body, or
/// `None` when the peer did not answer in the old protocol.
fn ask(host: &str, port: u16, frame_type: u16) -> Option<(u16, Vec<u8>)> {
    let mut s = connect(host, port)?;
    s.write_all(&frame(frame_type)).ok()?;
    let mut h = [0u8; HEADER_LEN];
    s.read_exact(&mut h).ok()?;
    if u32::from_le_bytes(h[0..4].try_into().ok()?) != MAGIC {
        return None;
    }
    let ty = u16::from_le_bytes(h[6..8].try_into().ok()?);
    let len = u64::from_le_bytes(h[12..20].try_into().ok()?).min(1024) as usize;
    let mut body = vec![0u8; len];
    if s.read_exact(&mut body).is_err() {
        body.clear(); // a header-only answer still proves the protocol
    }
    Some((ty, body))
}

#[derive(Debug, PartialEq, Eq)]
enum Build {
    /// Released before the cutover: the reply names no AVA1 port.
    Old,
    /// A new build; the reply's `"ava1"` field is `up`, `starting` or `failed`.
    New(String),
}

fn build_of(body: &[u8]) -> Build {
    let text = String::from_utf8_lossy(body);
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(v) if v.get("ava1_port").is_some() => Build::New(
            v.get("ava1")
                .and_then(|x| x.as_str())
                .unwrap_or("starting")
                .to_string(),
        ),
        _ => Build::Old,
    }
}

/// The helper's `Hello` answer, mgmt port first (or the transfer port for a build from before the split).
fn hello(host: &str, ports: Ports) -> Option<Build> {
    let (_, body) = ask(host, ports.mgmt, HELLO).or_else(|| ask(host, ports.transfer, HELLO))?;
    Some(build_of(&body))
}

/// True when the old protocol answers on the management port (or, for a helper from before the
/// port split, the transfer port): a plain connect plus one `Hello`. A new build answers too; use
/// [`state`] to tell them apart.
#[cfg(test)]
pub fn probe(host: &str, ports: Ports) -> bool {
    hello(host, ports).is_some()
}

/// True when something accepts a connection on `port`.
fn listening(host: &str, port: u16) -> bool {
    connect(host, port).is_some()
}

/// What runs on the console: [`AVA1`] when the AVA1 port accepts connections; else, when the old
/// protocol answers, [`HELPER_OLD`] for a build from before the cutover, [`STARTING`] for a new
/// build whose AVA1 server is still coming up and [`AVA1_FAILED`] for one whose AVA1 server did not
/// start; else [`NOT_RUNNING`]. Only [`HELPER_OLD`] is worth replacing.
pub fn state(host: &str, ports: Ports) -> &'static str {
    if listening(host, ports.ava1) {
        return AVA1;
    }
    match hello(host, ports) {
        None => NOT_RUNNING,
        Some(Build::Old) => HELPER_OLD,
        Some(Build::New(a)) if a == "failed" => AVA1_FAILED,
        Some(Build::New(_)) => STARTING,
    }
}

/// Folds the helper state into a failed status probe. The probe (`node.status` over AVA1) reports a
/// console with no AVA1 listener as `helper_not_ava1`, which cannot tell "nothing running" from "an
/// older helper that only speaks the old protocol". When the failure is that one, ask [`state`] and
/// lead the error with the precise token (`helper_old`, `helper_starting`, `ava1_failed`) so the
/// client can offer Update. Any other failure, or a console where nothing answers, is returned as is.
pub fn fold_status_error(error: String, host: &str, ports: Ports) -> String {
    if !error.contains("helper_not_ava1") {
        return error;
    }
    match state(host, ports) {
        HELPER_OLD => {
            format!("{HELPER_OLD}: the console runs an older helper; update it ({error})")
        }
        STARTING => format!("helper_starting: the helper is still starting ({error})"),
        AVA1_FAILED => {
            format!("{AVA1_FAILED}: the helper's transfer server did not start ({error})")
        }
        _ => error,
    }
}

/// Sends the old `Shutdown` frame. `true` when the helper acknowledged it.
pub fn shutdown(host: &str, ports: Ports) -> bool {
    ask(host, ports.mgmt, SHUTDOWN).map(|r| r.0) == Some(SHUTDOWN_ACK)
        || ask(host, ports.transfer, SHUTDOWN).map(|r| r.0) == Some(SHUTDOWN_ACK)
}

/// Waits until neither old port accepts a connection. `false` when one still does after `wait`.
pub fn wait_closed(host: &str, ports: Ports, wait: Duration) -> bool {
    let end = Instant::now() + wait;
    loop {
        if !listening(host, ports.mgmt) && !listening(host, ports.transfer) {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

/// Why a replacement did not complete.
#[derive(Debug, PartialEq, Eq)]
pub enum ReplaceError {
    /// The old helper is still listening after its shutdown request ([`LEGACY_HELPER_WEDGED`]).
    /// The new helper was not sent: it would lose the port bind.
    Wedged,
    /// The old helper exited but sending the new one failed (the caller's message).
    Send(String),
}

impl std::fmt::Display for ReplaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplaceError::Wedged => write!(
                f,
                "{LEGACY_HELPER_WEDGED}: the older helper did not exit after the shutdown request"
            ),
            ReplaceError::Send(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ReplaceError {}

/// What a completed replacement found.
#[derive(Debug, PartialEq, Eq)]
pub struct Replaced {
    /// The AVA1 port opened in time (`false`: the new helper may still be starting).
    pub ava1_up: bool,
}

/// Replaces an older helper: shutdown request, wait for both old ports to close, `send` the new
/// helper (the caller's stamped image, so its trust slot and launch token pair without a code),
/// then wait for the AVA1 port. `send` is not called while the old helper still holds its ports.
/// Blocking; one attempt only (the route enforces the 60 s between restarts).
pub fn replace(
    host: &str,
    ports: Ports,
    wait_close: Duration,
    wait_ava1: Duration,
    send: impl FnOnce() -> Result<(), String>,
) -> Result<Replaced, ReplaceError> {
    let _ = shutdown(host, ports);
    if !wait_closed(host, ports, wait_close) {
        return Err(ReplaceError::Wedged);
    }
    send().map_err(ReplaceError::Send)?;
    let end = Instant::now() + wait_ava1;
    loop {
        if listening(host, ports.ava1) {
            return Ok(Replaced { ava1_up: true });
        }
        if Instant::now() >= end {
            return Ok(Replaced { ava1_up: false });
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
#[path = "legacy_helper_tests.rs"]
mod tests;
