//! Transport to the PS5Upload installer daemon on TCP :9115 (JSON lines,
//! one request + one reply per connection). Replaces the ezremote-derived
//! DPI client. Reply parsing is pure and unit-tested; the socket calls are
//! exercised against an in-test fake daemon.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::payload_lifecycle::{
    join_host_port, port_is_open, send_elf_to_loader, LoaderImage, DPI_REASON_LOADER_SEND_FAILED,
    DPI_REASON_LOADER_UNREACHABLE, DPI_REASON_NO_BRINGUP, DPI_REASON_NO_IMAGE, INSTALLER_PORT,
    PS5_LOADER_PORT,
};

/// The name the installer is stored under when Payload Manager launches it.
const INSTALLER_ELF_NAME: &str = "ps5upload-installer.elf";

/// Must match the daemon's INST_VERSION in payload/installer/main.c.
pub const INSTALLER_VERSION: &str = "1.5.0";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const SHORT_TIMEOUT: Duration = Duration::from_secs(5); // hello / job / stop
const INSTALL_TIMEOUT: Duration = Duration::from_secs(900);
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
const ENSURE_WAIT_TOTAL: Duration = Duration::from_secs(45);
const ENSURE_POLL: Duration = Duration::from_millis(500);
/// How long a port that is open but answers no hello may stay that way (a
/// daemon still exiting) before it is reported as not coming up.
const MUTE_WAIT: Duration = Duration::from_secs(10);
/// How long a daemon told to `stop` may keep :9115 before the replacement is
/// sent anyway. 1.5.0+ lets go within a quarter second; older daemons only
/// notice the stop on their next accepted connection, which the release
/// probes below provide.
const STOP_RELEASE_WAIT: Duration = Duration::from_secs(10);
/// How long a refused loader port is retried before the loader counts as absent: past
/// elfldr's 3 s re-listen and the swap's kill-sleep-bind gap.
const LOADER_RETRY_WINDOW: Duration = Duration::from_secs(8);

#[derive(Debug, Clone)]
pub struct Hello {
    pub version: String,
    pub fw: String,
    pub state: String,
    pub init_rc: u32,
    pub escalated: bool,
}

#[derive(Debug)]
pub enum InstallReply {
    Accepted { job: String, via: String },
    Busy { job: String },
    NotReady { init_rc: u32 },
    BadRequest,
    BadPath,
    UnknownJob,
    Sony { code: u32, hint: Option<String> },
    Unknown(String),
}

#[derive(Debug, Clone)]
pub struct Job {
    pub phase: String,
    pub bytes_served: u64,
    pub total: u64,
    pub code: u32,
}

#[derive(Debug, Default)]
pub struct Ensure {
    pub listening: bool,
    pub sent: bool,
    pub state: Option<String>,
    pub reason: Option<&'static str>,
    pub error: Option<String>,
}

/// Connect to `addr`, send `request` + "\n", read one line back.
fn talk(addr: &str, read_timeout: Duration, request: &str) -> Result<String, String> {
    let sa = addr
        .parse::<std::net::SocketAddr>()
        .or_else(|_| {
            use std::net::ToSocketAddrs;
            addr.to_socket_addrs()
                .map_err(|e| e.to_string())?
                .next()
                .ok_or_else(|| format!("resolve {addr} failed"))
        })
        .map_err(|e: String| e)?;
    let mut s = TcpStream::connect_timeout(&sa, CONNECT_TIMEOUT)
        .map_err(|e| format!("connect {addr}: {e}"))?;
    s.set_read_timeout(Some(read_timeout)).ok();
    s.set_write_timeout(Some(SHORT_TIMEOUT)).ok();
    s.write_all(request.as_bytes())
        .and_then(|_| s.write_all(b"\n"))
        .map_err(|e| format!("write {addr}: {e}"))?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        match s.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.contains(&b'\n') {
                    break;
                }
            }
            Err(e) => return Err(format!("read {addr}: {e}")),
        }
    }
    let line = String::from_utf8_lossy(&buf);
    Ok(line.lines().next().unwrap_or("").to_string())
}

fn parse_hello(line: &str) -> Result<Hello, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("bad hello: {e}"))?;
    Ok(Hello {
        version: v["version"].as_str().unwrap_or("").to_string(),
        fw: v["fw"].as_str().unwrap_or("").to_string(),
        state: v["state"].as_str().unwrap_or("").to_string(),
        init_rc: v["init_rc"].as_u64().unwrap_or(0) as u32,
        escalated: v["escalated"].as_bool().unwrap_or(false),
    })
}

fn parse_install_reply(line: &str) -> InstallReply {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return InstallReply::Unknown(line.to_string()),
    };
    if v["ok"].as_bool() == Some(true) {
        return InstallReply::Accepted {
            job: v["job"].as_str().unwrap_or("").to_string(),
            via: v["via"].as_str().unwrap_or("").to_string(),
        };
    }
    if let Some(err) = v["error"].as_str() {
        return match err {
            "busy" => InstallReply::Busy {
                job: v["job"].as_str().unwrap_or("").to_string(),
            },
            "not_ready" => InstallReply::NotReady {
                init_rc: v["init_rc"].as_u64().unwrap_or(0) as u32,
            },
            "bad_path" => InstallReply::BadPath,
            "unknown_job" => InstallReply::UnknownJob,
            "bad_request" => InstallReply::BadRequest,
            other => InstallReply::Unknown(other.to_string()),
        };
    }
    if let Some(code) = v["code"].as_u64() {
        return InstallReply::Sony {
            code: code as u32,
            hint: v["hint"].as_str().map(|s| s.to_string()),
        };
    }
    InstallReply::Unknown(line.to_string())
}

fn parse_job(line: &str) -> Result<Job, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("bad job: {e}"))?;
    if v["ok"].as_bool() != Some(true) {
        return Err(line.to_string());
    }
    Ok(Job {
        phase: v["phase"].as_str().unwrap_or("").to_string(),
        bytes_served: v["bytes_served"].as_u64().unwrap_or(0),
        total: v["total"].as_u64().unwrap_or(0),
        code: v["code"].as_u64().unwrap_or(0) as u32,
    })
}

/// JSON-escape a string value for embedding in a request line.
fn esc(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

// ── addr-taking cores (unit-tested against the fake daemon) ────────────

fn hello_at(addr: &str) -> Result<Hello, String> {
    let line = talk(addr, SHORT_TIMEOUT, "{\"op\":\"hello\"}")?;
    parse_hello(&line)
}

fn install_at(addr: &str, request: &str) -> Result<InstallReply, String> {
    let line = talk(addr, INSTALL_TIMEOUT, request)?;
    Ok(parse_install_reply(&line))
}

fn job_at(addr: &str, id: &str) -> Result<Job, String> {
    let req = format!("{{\"op\":\"job\",\"job\":{}}}", esc(id));
    let line = talk(addr, SHORT_TIMEOUT, &req)?;
    parse_job(&line)
}

fn stop_at(addr: &str) -> Result<(), String> {
    let line = talk(addr, SHORT_TIMEOUT, "{\"op\":\"stop\"}")?;
    let v: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|e| format!("stop reply: {e}"))?;
    if v.get("ok").and_then(|b| b.as_bool()) == Some(true) {
        Ok(())
    } else {
        Err(format!("stop refused: {}", line.trim()))
    }
}

/// Wait for a stopped daemon to close `addr`. Each probe is a real connect:
/// a pre-1.5.0 daemon sits in accept() after replying to `stop` and leaves
/// only once one more connection arrives, so these probes are what let it go.
/// true once the port refuses, false if it stayed open for `window`.
fn wait_port_released(addr: &str, window: Duration, poll: Duration) -> bool {
    let deadline = Instant::now() + window;
    loop {
        if !port_is_open(addr, PROBE_TIMEOUT) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(poll);
    }
}

/// The daemon refused `stop` because an install is in flight or its file is
/// still being read (1.5.0+): {"ok":false,"error":"busy",...}.
fn stop_refused_busy(err: &str) -> bool {
    err.starts_with("stop refused:") && err.contains("\"busy\"")
}

/// running < current, comparing dotted numeric versions.
fn needs_upgrade(current: &str, running: &str) -> bool {
    fn triple(s: &str) -> (u64, u64, u64) {
        let mut it = s.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
        (
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
        )
    }
    triple(running) < triple(current)
}

/// Send `bytes` to the loader on `ip:port` as a companion image, retrying for up to `window`
/// while nothing is listening. elfldr leaves :9021 closed for a moment whenever it is replaced
/// (the new one kills the old by name, sleeps a second, then binds) and for 3 s after a failed
/// accept, and the app swaps in its patched elfldr right as an install starts: one refused
/// connect is not a missing loader. No bare probe first — a loader handed a connection and no
/// bytes waits on it. Any failure after connecting is returned at once, never resent.
pub fn send_to_loader_retrying(
    ip: &str,
    port: u16,
    bytes: &[u8],
    window: Duration,
    poll: Duration,
) -> Result<u64, String> {
    let deadline = Instant::now() + window;
    loop {
        match send_elf_to_loader(ip, port, bytes, LoaderImage::Companion) {
            Err(e) if e.starts_with("connect ") && Instant::now() < deadline => {
                std::thread::sleep(poll);
            }
            r => return r,
        }
    }
}

fn ensure_at(ip: &str, port: u16, elf: Option<&[u8]>, protect_running: bool) -> Ensure {
    ensure_with(ip, port, elf, protect_running, MUTE_WAIT)
}

fn ensure_with(
    ip: &str,
    port: u16,
    elf: Option<&[u8]>,
    protect_running: bool,
    mute_wait: Duration,
) -> Ensure {
    ensure_with_waits(
        ip,
        port,
        elf,
        protect_running,
        mute_wait,
        STOP_RELEASE_WAIT,
        ENSURE_WAIT_TOTAL,
    )
}

fn ensure_with_waits(
    ip: &str,
    port: u16,
    elf: Option<&[u8]>,
    protect_running: bool,
    mute_wait: Duration,
    stop_release_wait: Duration,
    bringup_wait: Duration,
) -> Ensure {
    let addr = join_host_port(ip, port);
    // The version being replaced, when this call stopped an older daemon: its
    // hello must not be mistaken for the new one coming up.
    let mut replacing: Option<Hello> = None;
    // 1) probe
    if port_is_open(&addr, PROBE_TIMEOUT) {
        if let Ok(h) = hello_at(&addr) {
            if needs_upgrade(INSTALLER_VERSION, &h.version) {
                if protect_running {
                    // never tear down a daemon that may be serving a loopback job
                    return Ensure {
                        listening: true,
                        sent: false,
                        state: Some(h.state),
                        reason: Some("upgrade_skipped_serving"),
                        error: None,
                    };
                }
                // upgrade: stop, wait for the old daemon to let go of the
                // port, then fall through to send a fresh copy. Sending while
                // the old one still held :9115 made the new one fail its bind
                // and exit, so the first install after an update failed with
                // "never came up".
                if let Err(e) = stop_at(&addr) {
                    if stop_refused_busy(&e) {
                        // mid-install: keep it; the next ensure upgrades
                        return Ensure {
                            listening: true,
                            sent: false,
                            state: Some(h.state),
                            reason: Some("upgrade_skipped_serving"),
                            error: None,
                        };
                    }
                }
                // Still held after the wait: send anyway. The new daemon
                // retries its bind for a few seconds, and the wait below
                // tells the old hello from the new one.
                let _ = wait_port_released(&addr, stop_release_wait, ENSURE_POLL);
                replacing = Some(h);
            } else {
                return Ensure {
                    listening: true,
                    sent: false,
                    state: Some(h.state),
                    reason: None,
                    error: None,
                };
            }
        } else {
            // Open but no hello: most likely a daemon still exiting. Wait for
            // it to answer (it was only busy) or to close (then send a fresh
            // copy below). Never call a mute port a running daemon.
            let deadline = Instant::now() + mute_wait;
            loop {
                if !port_is_open(&addr, PROBE_TIMEOUT) {
                    break;
                }
                if let Ok(h) = hello_at(&addr) {
                    return Ensure {
                        listening: true,
                        sent: false,
                        state: Some(h.state),
                        reason: None,
                        error: None,
                    };
                }
                if Instant::now() >= deadline {
                    return Ensure {
                        listening: false,
                        sent: false,
                        state: None,
                        reason: Some("daemon_mute"),
                        error: Some(
                            "the installer port is open but nothing on it answers; restart the console's payload loader"
                                .into(),
                        ),
                    };
                }
                std::thread::sleep(ENSURE_POLL);
            }
        }
    }
    // 2) send the bundled ELF as a companion image
    let Some(bytes) = elf else {
        return Ensure {
            listening: false,
            sent: false,
            state: None,
            reason: Some(DPI_REASON_NO_IMAGE),
            error: Some("this engine build carries no installer daemon".into()),
        };
    };
    // The console's ELF loader first; Payload Manager (:8084) when the loader
    // is gone or refuses the send. On setups with Payload Manager and the
    // WebKit Autoloader, :9021 is often dead while Payload Manager is up, and
    // every install used to stop here (#344, #345).
    let loader_result =
        send_to_loader_retrying(ip, PS5_LOADER_PORT, bytes, LOADER_RETRY_WINDOW, ENSURE_POLL);
    // A connect failure means nothing was listening; anything later means a loader took
    // the connection and the send itself failed.
    let loader_open = loader_result
        .as_ref()
        .err()
        .is_none_or(|e| !e.starts_with("connect "));
    let loader_result = loader_result.map_err(|e| {
        if loader_open {
            e
        } else {
            format!("nothing answered on the ELF loader port :9021 ({e})")
        }
    });
    let mut via_payload_manager = false;
    if let Err(loader_err) = loader_result {
        match crate::payload_manager::launch_elf(ip, INSTALLER_ELF_NAME, bytes) {
            Ok(()) => via_payload_manager = true,
            Err(pm_err) => {
                return Ensure {
                    listening: false,
                    sent: loader_open,
                    state: None,
                    reason: Some(if loader_open {
                        DPI_REASON_LOADER_SEND_FAILED
                    } else {
                        DPI_REASON_LOADER_UNREACHABLE
                    }),
                    error: Some(format!(
                        "{loader_err}, and {pm_err} to launch it instead. Load elfldr (or Payload Manager) on the PS5 and retry"
                    )),
                };
            }
        }
    }
    // 3) wait up to 45s for hello to answer (covers the boot wait)
    let deadline = Instant::now() + bringup_wait;
    let mut old_answering: Option<Hello> = None;
    let forget_stored_copy = || {
        if via_payload_manager {
            // The running daemon doesn't need it; don't leave it in the
            // user's Payload Manager list.
            crate::payload_manager::forget(ip, INSTALLER_ELF_NAME);
        }
    };
    while Instant::now() < deadline {
        if port_is_open(&addr, PROBE_TIMEOUT) {
            if let Ok(h) = hello_at(&addr) {
                if replacing.is_some() && needs_upgrade(INSTALLER_VERSION, &h.version) {
                    // the daemon being replaced, not yet gone
                    old_answering = Some(h);
                    std::thread::sleep(ENSURE_POLL);
                    continue;
                }
                forget_stored_copy();
                return Ensure {
                    listening: true,
                    sent: true,
                    state: Some(h.state),
                    reason: None,
                    error: None,
                };
            }
        }
        std::thread::sleep(ENSURE_POLL);
    }
    forget_stored_copy();
    if let Some(old) = old_answering {
        // The old daemon never left, so the new one could not bind. It still
        // installs; keep using it rather than failing the install.
        return Ensure {
            listening: true,
            sent: true,
            state: Some(old.state),
            reason: Some("upgrade_not_taken"),
            error: Some(format!(
                "installer {} kept :9115; {INSTALLER_VERSION} could not replace it",
                old.version
            )),
        };
    }
    Ensure {
        listening: false,
        sent: true,
        state: None,
        reason: Some(DPI_REASON_NO_BRINGUP),
        error: Some("installer delivered but :9115 never came up".into()),
    }
}

// ── public API (ip only; port :9115 implied) ──────────────────────────

pub fn hello(ip: &str) -> Result<Hello, String> {
    hello_at(&join_host_port(ip, INSTALLER_PORT))
}

pub fn install_url(ip: &str, url: &str, name_hint: &str) -> Result<InstallReply, String> {
    let req = format!(
        "{{\"op\":\"install\",\"url\":{},\"name_hint\":{}}}",
        esc(url),
        esc(name_hint)
    );
    install_at(&join_host_port(ip, INSTALLER_PORT), &req)
}

pub fn install_path(ip: &str, path: &str, name_hint: &str) -> Result<InstallReply, String> {
    let req = format!(
        "{{\"op\":\"install\",\"path\":{},\"name_hint\":{}}}",
        esc(path),
        esc(name_hint)
    );
    install_at(&join_host_port(ip, INSTALLER_PORT), &req)
}

/// Install the console's own file by its plain path, never over the daemon's loopback server
/// (`"route":"path"`, daemon 1.3.0+). The engine's guarded last resort: see
/// `install::path_fallback_allowed` for when it may be asked for.
pub fn install_path_plain(ip: &str, path: &str, name_hint: &str) -> Result<InstallReply, String> {
    install_at(
        &join_host_port(ip, INSTALLER_PORT),
        &plain_path_request(path, name_hint),
    )
}

fn plain_path_request(path: &str, name_hint: &str) -> String {
    format!(
        "{{\"op\":\"install\",\"path\":{},\"name_hint\":{},\"route\":\"path\"}}",
        esc(path),
        esc(name_hint)
    )
}

/// Ask the installer daemon to exit; the next `ensure` sends a fresh one.
/// Recycles a daemon whose Sony install state a failed network install has
/// wedged — measured on FW 5.10: after one stream the console could not
/// fetch, every later URL install failed instantly (0x80431064) until the
/// daemon process restarted.
pub fn stop(ip: &str) -> Result<(), String> {
    stop_at(&join_host_port(ip, INSTALLER_PORT))
}

pub fn job(ip: &str, id: &str) -> Result<Job, String> {
    job_at(&join_host_port(ip, INSTALLER_PORT), id)
}

pub fn ensure(ip: &str, elf: Option<&[u8]>, protect_running: bool) -> Ensure {
    ensure_at(ip, INSTALLER_PORT, elf, protect_running)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// A one-shot fake daemon: binds 127.0.0.1:0, accepts one connection,
    /// reads a line, sends `reply` + "\n". Returns (addr, join handle that
    /// yields the request line it received).
    fn fake_once(reply: &'static str) -> (String, thread::JoinHandle<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let h = thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            // Read the whole request line, as the daemon does: a request that arrives in two
            // segments, answered after the first, left bytes unread, and closing over unread
            // bytes resets the connection before the client reads the reply (a flake).
            let mut got = Vec::new();
            let mut buf = [0u8; 512];
            while !got.contains(&b'\n') {
                let n = s.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
            }
            let _ = s.write_all(reply.as_bytes());
            let _ = s.write_all(b"\n");
            String::from_utf8_lossy(&got).to_string()
        });
        (addr, h)
    }

    /// A multi-accept fake: replies `reply` to every connection and records
    /// each non-empty request line. `port_is_open` opens (and immediately
    /// drops) a probe connection before every hello/stop, so a one-shot fake
    /// cannot be used for the ensure() path — the probe would eat its single
    /// accept. Returns (ip, port, requests, stop-flag). Set the flag and the
    /// listener thread exits at its next poll.
    fn fake_multi(
        reply: &'static str,
    ) -> (
        String,
        u16,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let sa = l.local_addr().unwrap();
        let reqs = Arc::new(Mutex::new(Vec::<String>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let reqs2 = reqs.clone();
        let stop2 = stop.clone();
        thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match l.accept() {
                    Ok((mut s, _)) => {
                        // BSD/macOS: the accepted socket inherits the listener's
                        // O_NONBLOCK, so a read before the client writes returned
                        // WouldBlock and the request went unrecorded (a flake).
                        s.set_nonblocking(false).ok();
                        s.set_read_timeout(Some(Duration::from_millis(200))).ok();
                        // The whole request line (see fake_once).
                        let mut got = Vec::new();
                        let mut buf = [0u8; 512];
                        while !got.contains(&b'\n') {
                            match s.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => got.extend_from_slice(&buf[..n]),
                            }
                        }
                        if !got.is_empty() {
                            let line = String::from_utf8_lossy(&got).trim().to_string();
                            reqs2.lock().unwrap().push(line);
                        }
                        let _ = s.write_all(reply.as_bytes());
                        let _ = s.write_all(b"\n");
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        (sa.ip().to_string(), sa.port(), reqs, stop)
    }

    #[test]
    fn the_plain_path_request_names_its_route() {
        // The daemon serves a path over loopback unless the route says otherwise.
        let r = plain_path_request("/data/a \"b\".pkg", "PPSA1 (Base)");
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["op"], "install");
        assert_eq!(v["path"], "/data/a \"b\".pkg");
        assert_eq!(v["route"], "path");
        assert!(v.get("url").is_none());
    }

    #[test]
    fn a_port_that_answers_without_hello_is_not_a_running_daemon() {
        // A daemon mid-exit (or anything else on :9115) keeps the port open
        // but never says hello. Treating that as "listening" skipped sending
        // a fresh daemon and the install then hit a refused connect.
        let (ip, port, _reqs, stop) = fake_multi("not json");
        let e = ensure_with(&ip, port, None, false, Duration::from_millis(600));
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(!e.listening, "{e:?}");
        assert_eq!(e.reason, Some("daemon_mute"));
    }

    #[test]
    fn a_mute_daemon_that_exits_is_replaced() {
        // The port closes during the wait: fall through to sending a fresh
        // copy (here there is no image, which proves the send path ran).
        let (ip, port, _reqs, stop) = fake_multi("not json");
        let s2 = stop.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            s2.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let e = ensure_with(&ip, port, None, false, Duration::from_secs(5));
        assert_eq!(e.reason, Some(DPI_REASON_NO_IMAGE), "{e:?}");
    }

    #[test]
    fn parses_hello() {
        let h = parse_hello("{\"ok\":true,\"version\":\"1.0.0\",\"fw\":\"9\",\"state\":\"ready\",\"init_rc\":0,\"escalated\":true}").unwrap();
        assert_eq!(h.version, "1.0.0");
        assert_eq!(h.state, "ready");
        assert!(h.escalated);
    }

    #[test]
    fn parses_install_accepted() {
        match parse_install_reply("{\"ok\":true,\"job\":\"J1\",\"via\":\"loopback\"}") {
            InstallReply::Accepted { job, via } => {
                assert_eq!(job, "J1");
                assert_eq!(via, "loopback");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_busy_not_ready_badpath_sony() {
        assert!(matches!(
            parse_install_reply("{\"ok\":false,\"error\":\"busy\",\"job\":\"J2\"}"),
            InstallReply::Busy { .. }
        ));
        assert!(matches!(
            parse_install_reply("{\"ok\":false,\"error\":\"not_ready\",\"init_rc\":5}"),
            InstallReply::NotReady { init_rc: 5 }
        ));
        assert!(matches!(
            parse_install_reply("{\"ok\":false,\"error\":\"bad_path\"}"),
            InstallReply::BadPath
        ));
        match parse_install_reply(
            "{\"ok\":false,\"code\":2158559236,\"hint\":\"install the base game first\"}",
        ) {
            InstallReply::Sony { code, hint } => {
                assert_eq!(code, 2158559236);
                assert_eq!(hint.as_deref(), Some("install the base game first"));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_job() {
        let j = parse_job(
            "{\"ok\":true,\"phase\":\"serving\",\"bytes_served\":1024,\"total\":4096,\"code\":0}",
        )
        .unwrap();
        assert_eq!(j.phase, "serving");
        assert_eq!(j.bytes_served, 1024);
        assert_eq!(j.total, 4096);
    }

    #[test]
    fn hello_over_socket() {
        let (addr, h) = fake_once("{\"ok\":true,\"version\":\"1.0.0\",\"fw\":\"9\",\"state\":\"ready\",\"init_rc\":0,\"escalated\":true}");
        let res = hello_at(&addr).unwrap();
        assert_eq!(res.version, "1.0.0");
        assert_eq!(h.join().unwrap().trim(), "{\"op\":\"hello\"}");
    }

    #[test]
    fn install_url_sends_correct_request() {
        let (addr, h) = fake_once("{\"ok\":true,\"job\":\"J9\",\"via\":\"url\"}");
        let r = install_at(
            &addr,
            "{\"op\":\"install\",\"url\":\"http://h/a.pkg\",\"name_hint\":\"CUSA1 (Base)\"}",
        )
        .unwrap();
        assert!(matches!(r, InstallReply::Accepted { .. }));
        let got = h.join().unwrap();
        assert!(got.contains("\"url\":\"http://h/a.pkg\""));
        assert!(got.contains("\"name_hint\":\"CUSA1 (Base)\""));
    }

    #[test]
    fn needs_upgrade_compares_semver() {
        assert!(needs_upgrade("1.0.0", "0.9.9")); // running older -> upgrade
        assert!(!needs_upgrade("1.0.0", "1.0.0")); // same -> no
        assert!(!needs_upgrade("1.0.0", "1.1.0")); // running newer -> no
    }

    #[test]
    fn stop_sends_the_stop_op_and_reads_ok() {
        use std::sync::atomic::Ordering;
        let (ip, port, reqs, halt) = fake_multi("{\"ok\":true}");
        let r = stop_at(&format!("{ip}:{port}"));
        halt.store(true, Ordering::Relaxed);
        assert!(r.is_ok(), "{r:?}");
        assert!(reqs
            .lock()
            .unwrap()
            .iter()
            .any(|q| q == "{\"op\":\"stop\"}"));
    }

    #[test]
    fn ensure_already_listening_same_version_does_not_send() {
        use std::sync::atomic::Ordering;
        // The daemon answers with exactly the version this engine bundles.
        let hello: &'static str = Box::leak(
            format!(
                "{{\"ok\":true,\"version\":\"{INSTALLER_VERSION}\",\"fw\":\"9\",\"state\":\"ready\",\"init_rc\":0,\"escalated\":true}}"
            )
            .into_boxed_str(),
        );
        let (ip, port, reqs, stop) = fake_multi(hello);
        let e = ensure_at(&ip, port, None, false);
        stop.store(true, Ordering::Relaxed);
        assert!(e.listening);
        assert!(!e.sent);
        let got = reqs.lock().unwrap();
        // a hello was sent, and never a stop
        assert!(got.iter().any(|r| r == "{\"op\":\"hello\"}"));
        assert!(!got.iter().any(|r| r.contains("\"stop\"")));
    }

    #[test]
    fn ensure_protects_a_serving_daemon_from_stop() {
        use std::sync::atomic::Ordering;
        // Older version, but protect_running=true: must NOT send "stop".
        let (ip, port, reqs, stop) = fake_multi("{\"ok\":true,\"version\":\"0.9.0\",\"fw\":\"9\",\"state\":\"ready\",\"init_rc\":0,\"escalated\":true}");
        let e = ensure_at(&ip, port, Some(b"\x7FELF-ignored"), true);
        stop.store(true, Ordering::Relaxed);
        assert!(e.listening);
        assert!(!e.sent);
        assert_eq!(e.reason, Some("upgrade_skipped_serving"));
        let got = reqs.lock().unwrap();
        assert!(!got.iter().any(|r| r.contains("\"stop\"")));
    }

    const OLD_HELLO: &str = "{\"ok\":true,\"version\":\"1.4.0\",\"fw\":\"13\",\"state\":\"ready\",\"init_rc\":0,\"escalated\":true}";

    /// How a fake old daemon answers `stop`.
    #[derive(Clone, Copy, PartialEq)]
    enum StopStyle {
        /// 1.5.0+: replies ok and closes the port at once.
        Prompt,
        /// <= 1.4.0: replies ok, then only closes the port after accepting
        /// one more connection (it was blocked in accept()).
        AfterNextConnect,
        /// 1.5.0+ mid-install: refuses with busy and keeps running.
        Busy,
    }

    /// A fake old daemon on 127.0.0.1:0 that answers hello with OLD_HELLO and
    /// `stop` per `style`. Returns (ip, port, requests, kill-flag); the port
    /// closes when the fake exits.
    fn fake_old_daemon(
        style: StopStyle,
    ) -> (
        String,
        u16,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let sa = l.local_addr().unwrap();
        let reqs = Arc::new(Mutex::new(Vec::<String>::new()));
        let kill = Arc::new(AtomicBool::new(false));
        let (reqs2, kill2) = (reqs.clone(), kill.clone());
        thread::spawn(move || {
            let mut stopped = false;
            while !kill2.load(Ordering::Relaxed) {
                match l.accept() {
                    Ok((mut s, _)) => {
                        if stopped {
                            // the old daemon wakes, sees the flag, exits
                            return;
                        }
                        s.set_nonblocking(false).ok();
                        s.set_read_timeout(Some(Duration::from_millis(200))).ok();
                        let mut got = Vec::new();
                        let mut buf = [0u8; 512];
                        while !got.contains(&b'\n') {
                            match s.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => got.extend_from_slice(&buf[..n]),
                            }
                        }
                        let line = String::from_utf8_lossy(&got).trim().to_string();
                        if line.is_empty() {
                            continue;
                        }
                        reqs2.lock().unwrap().push(line.clone());
                        let reply = if line.contains("\"stop\"") {
                            if style == StopStyle::Busy {
                                "{\"ok\":false,\"error\":\"busy\",\"job\":\"1-1\"}"
                            } else {
                                "{\"ok\":true}"
                            }
                        } else {
                            OLD_HELLO
                        };
                        let _ = s.write_all(reply.as_bytes());
                        let _ = s.write_all(b"\n");
                        if line.contains("\"stop\"") {
                            match style {
                                StopStyle::Prompt => return,
                                StopStyle::AfterNextConnect => stopped = true,
                                StopStyle::Busy => {}
                            }
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        (sa.ip().to_string(), sa.port(), reqs, kill)
    }

    fn short_ensure(ip: &str, port: u16) -> Ensure {
        // No image: reaching the send step shows up as DPI_REASON_NO_IMAGE.
        ensure_with_waits(
            ip,
            port,
            None,
            false,
            Duration::from_millis(600),
            Duration::from_secs(5),
            Duration::from_secs(2),
        )
    }

    #[test]
    fn upgrade_waits_for_a_prompt_daemon_to_release_the_port() {
        let (ip, port, reqs, kill) = fake_old_daemon(StopStyle::Prompt);
        let t0 = Instant::now();
        let e = short_ensure(&ip, port);
        kill.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(e.reason, Some(DPI_REASON_NO_IMAGE), "{e:?}");
        assert!(!port_is_open(&format!("{ip}:{port}"), PROBE_TIMEOUT));
        assert!(t0.elapsed() < Duration::from_secs(4), "{:?}", t0.elapsed());
        assert!(reqs
            .lock()
            .unwrap()
            .iter()
            .any(|r| r == "{\"op\":\"stop\"}"));
    }

    #[test]
    fn upgrade_wakes_an_old_daemon_blocked_in_accept() {
        // A <=1.4.0 daemon only leaves after one more connection; the send
        // must not happen while it still holds the port (the first-install
        // "never came up" failure).
        let (ip, port, _reqs, kill) = fake_old_daemon(StopStyle::AfterNextConnect);
        let e = short_ensure(&ip, port);
        kill.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(e.reason, Some(DPI_REASON_NO_IMAGE), "{e:?}");
        assert!(!port_is_open(&format!("{ip}:{port}"), PROBE_TIMEOUT));
    }

    #[test]
    fn upgrade_keeps_a_daemon_that_refuses_stop_mid_install() {
        let (ip, port, reqs, kill) = fake_old_daemon(StopStyle::Busy);
        let e = short_ensure(&ip, port);
        kill.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(e.listening, "{e:?}");
        assert!(!e.sent);
        assert_eq!(e.reason, Some("upgrade_skipped_serving"));
        assert!(reqs
            .lock()
            .unwrap()
            .iter()
            .any(|r| r == "{\"op\":\"stop\"}"));
    }

    #[test]
    fn stop_refusal_is_recognised_only_as_busy() {
        assert!(stop_refused_busy(
            "stop refused: {\"ok\":false,\"error\":\"busy\",\"job\":\"\"}"
        ));
        assert!(!stop_refused_busy("stop reply: EOF while parsing"));
        assert!(!stop_refused_busy("connect 1.2.3.4:9115: refused"));
    }

    /// A free loopback port nothing listens on yet.
    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn tiny_elf() -> Vec<u8> {
        let mut image = b"\x7FELF".to_vec();
        image.extend_from_slice(&[0x5Au8; 256]);
        image
    }

    /// elfldr drops :9021 for a moment whenever it is replaced (the new one kills the old,
    /// sleeps, then binds) and for 3 s after a failed accept. An install that arrived in that
    /// gap used to fail at once with "nothing answered on the ELF loader port"; it must wait
    /// the gap out and deliver to the loader that comes back.
    #[test]
    fn a_loader_that_comes_back_within_the_window_gets_the_image() {
        let port = free_port();
        let h = thread::spawn(move || {
            thread::sleep(Duration::from_millis(1200));
            let l = TcpListener::bind(("127.0.0.1", port)).unwrap();
            let (mut s, _) = l.accept().unwrap();
            let mut got = Vec::new();
            s.read_to_end(&mut got).unwrap();
            got
        });
        let image = tiny_elf();
        let r = send_to_loader_retrying(
            "127.0.0.1",
            port,
            &image,
            Duration::from_secs(5),
            Duration::from_millis(100),
        );
        assert_eq!(r, Ok(image.len() as u64));
        assert_eq!(h.join().unwrap(), image);
    }

    #[test]
    fn a_loader_that_stays_down_fails_with_the_connect_error() {
        let port = free_port();
        let started = Instant::now();
        let r = send_to_loader_retrying(
            "127.0.0.1",
            port,
            &tiny_elf(),
            Duration::from_millis(600),
            Duration::from_millis(100),
        );
        let err = r.expect_err("nothing listens");
        assert!(err.starts_with("connect "), "{err}");
        assert!(started.elapsed() >= Duration::from_millis(600));
    }

    #[test]
    fn a_send_that_fails_after_connecting_is_not_retried() {
        // Not an ELF: refused before any connection, and never retried.
        let started = Instant::now();
        let r = send_to_loader_retrying(
            "127.0.0.1",
            free_port(),
            b"text",
            Duration::from_secs(5),
            Duration::from_millis(100),
        );
        assert!(r.is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}

#[cfg(test)]
mod live_tests {
    /// Hardware check: with an older daemon running on the console,
    /// `PS5UPLOAD_LIVE_INSTALLER_IP=<console> cargo test -p ps5upload-core
    /// installer_client::live -- --ignored --nocapture`. Runs the upgrade the
    /// engine runs before an install (protect_running=false) with the freshly
    /// built ELF and requires the new daemon up on the first try.
    #[test]
    #[ignore]
    fn live_upgrade_replaces_an_older_daemon_first_time() {
        let Ok(ip) = std::env::var("PS5UPLOAD_LIVE_INSTALLER_IP") else {
            return;
        };
        let before = super::hello(&ip).map(|h| h.version);
        let elf = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../payload/installer/ps5upload-installer.elf"
        ))
        .expect("installer ELF");
        let t0 = std::time::Instant::now();
        let e = super::ensure(&ip, Some(&elf), false);
        let took = t0.elapsed();
        let after = super::hello(&ip).map(|h| h.version);
        println!("before={before:?} ensure={e:?} took={took:?} after={after:?}");
        assert!(e.listening && e.sent, "{e:?}");
        assert_eq!(e.reason, None, "{e:?}");
        assert_eq!(after.as_deref(), Ok(super::INSTALLER_VERSION));
    }
}
