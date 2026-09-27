//! Transport to the PS5Upload installer daemon on TCP :9115 (JSON lines,
//! one request + one reply per connection). Replaces the ezremote-derived
//! DPI client. Reply parsing is pure and unit-tested; the socket calls are
//! exercised against an in-test fake daemon.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::payload_lifecycle::{
    join_host_port, port_is_open, send_elf_to_loader, LoaderImage, INSTALLER_PORT, PS5_LOADER_PORT,
};

/// Must match the daemon's INST_VERSION in payload/installer/main.c.
pub const INSTALLER_VERSION: &str = "1.0.0";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const SHORT_TIMEOUT: Duration = Duration::from_secs(5); // hello / job / stop
const INSTALL_TIMEOUT: Duration = Duration::from_secs(900);
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
const ENSURE_WAIT_TOTAL: Duration = Duration::from_secs(45);
const ENSURE_POLL: Duration = Duration::from_millis(500);

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

fn ensure_at(ip: &str, port: u16, elf: Option<&[u8]>, protect_running: bool) -> Ensure {
    let addr = join_host_port(ip, port);
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
                // upgrade: stop, then fall through to send a fresh copy
                let _ = talk(&addr, SHORT_TIMEOUT, "{\"op\":\"stop\"}");
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
            return Ensure {
                listening: true,
                sent: false,
                state: None,
                reason: None,
                error: None,
            };
        }
    }
    // 2) send the bundled ELF as a companion image
    let Some(bytes) = elf else {
        return Ensure {
            listening: false,
            sent: false,
            state: None,
            reason: Some("no_image"),
            error: Some("this engine build carries no installer daemon".into()),
        };
    };
    let loader = join_host_port(ip, PS5_LOADER_PORT);
    if !port_is_open(&loader, PROBE_TIMEOUT) {
        return Ensure {
            listening: false,
            sent: false,
            state: None,
            reason: Some("loader_unreachable"),
            error: Some("nothing answered on the ELF loader port :9021".into()),
        };
    }
    if let Err(e) = send_elf_to_loader(ip, PS5_LOADER_PORT, bytes, LoaderImage::Companion) {
        return Ensure {
            listening: false,
            sent: true,
            state: None,
            reason: Some("loader_send_failed"),
            error: Some(e),
        };
    }
    // 3) wait up to 45s for hello to answer (covers the boot wait)
    let deadline = Instant::now() + ENSURE_WAIT_TOTAL;
    while Instant::now() < deadline {
        if port_is_open(&addr, PROBE_TIMEOUT) {
            if let Ok(h) = hello_at(&addr) {
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
    Ensure {
        listening: false,
        sent: true,
        state: None,
        reason: Some("no_bringup"),
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
            let mut buf = [0u8; 512];
            let n = s.read(&mut buf).unwrap();
            let _ = s.write_all(reply.as_bytes());
            let _ = s.write_all(b"\n");
            String::from_utf8_lossy(&buf[..n]).to_string()
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
                        s.set_read_timeout(Some(Duration::from_millis(200))).ok();
                        let mut buf = [0u8; 512];
                        let n = s.read(&mut buf).unwrap_or(0);
                        if n > 0 {
                            let line = String::from_utf8_lossy(&buf[..n]).trim().to_string();
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
        let (ip, port, reqs, stop) = fake_multi("{\"ok\":true,\"version\":\"1.0.0\",\"fw\":\"9\",\"state\":\"ready\",\"init_rc\":0,\"escalated\":true}");
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
}
