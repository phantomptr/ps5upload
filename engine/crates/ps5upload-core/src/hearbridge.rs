//! HearBridge (X-F1REBALL-X/HearBridge-PS5): Bluetooth headset audio for the PS5, with its
//! own small HTTP API. The app reads and drives it through the engine because the page
//! sends no CORS headers, so a browser page on another origin can't call it directly.
//!
//! Its API, read from its source (src/http.c):
//!   HTTP/1.1, `Connection: close`, on TCP 8090; when that port is taken it tries the next
//!   five (8091..8095).
//!   GET  /api/status    one JSON object: version, token, state, device, gain_pct, muted,
//!                       codec, latency{target_ms,estimate_ms}, battery{pct}, link{score,
//!                       rssi}, night{on}, lat_extra{auto}, game{id,name,profile}, events[]…
//!   GET  /api/saved     {"devices":[{addr,name,kind,current}]}  paired headsets
//!   GET  /api/devices   {"version":1,"devices":[{index,addr,name,kind,rssi}]}  last scan
//!   POST /api/volume?pct=0..500   /api/mute?on=0|1   /api/night?on=0|1
//!        /api/latency?auto=0|1 or ?ms=40..400       /api/reconnect   /api/stop
//!        /api/select?index=N or ?addr=XX:XX:..      /api/scan?user=1 (without user=1 a
//!        scan is ignored: an inquiry holds the radio the DualSense shares)
//!   Every POST needs header `X-HB-Token` with the token from /api/status (403 otherwise).
//!
//! Nothing is cached on disk. The port a console's HearBridge answered on is remembered in
//! memory so the next call tries it first; the token is read fresh for every change.
//!
//! Plain HTTP over a TcpStream, like `payload_manager`: the Android engine has no HTTP
//! client crate.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;

/// HearBridge's port, then the five it falls back to.
pub const HEARBRIDGE_PORTS: [u16; 6] = [8090, 8091, 8092, 8093, 8094, 8095];

const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// Finding HearBridge (every port, if need be) never takes longer than this.
const FIND_DEADLINE: Duration = Duration::from_secs(8);
/// /api/status is at most 32 KiB (its own buffer); saved/devices at most 8 KiB.
const MAX_RESPONSE: usize = 256 * 1024;

fn port_cache() -> &'static Mutex<HashMap<String, u16>> {
    static CACHE: OnceLock<Mutex<HashMap<String, u16>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A HearBridge that answered: its port, its status (without the token) and the token.
#[derive(Debug, Clone)]
pub struct Found {
    pub port: u16,
    pub status: Value,
    token: String,
}

/// Find HearBridge on `host` and read its status.
pub fn status(host: &str) -> Result<Found, String> {
    find_on(host, &HEARBRIDGE_PORTS)
}

fn find_on(host: &str, ports: &[u16]) -> Result<Found, String> {
    check_host(host)?;
    let deadline = Instant::now() + FIND_DEADLINE;
    let cached = port_cache().lock().ok().and_then(|c| c.get(host).copied());
    // The port it answered on last time first, then HearBridge's own order.
    let mut order: Vec<u16> = Vec::with_capacity(ports.len());
    if let Some(p) = cached.filter(|p| ports.contains(p)) {
        order.push(p);
    }
    order.extend(ports.iter().copied().filter(|p| Some(*p) != cached));
    let mut last = String::new();
    for port in order {
        if Instant::now() >= deadline {
            last = format!("gave up after {}s", FIND_DEADLINE.as_secs());
            break;
        }
        match status_at(host, port, deadline) {
            Ok(found) => {
                if let Ok(mut c) = port_cache().lock() {
                    c.insert(host.to_string(), port);
                }
                return Ok(found);
            }
            Err(e) => {
                last = e.msg;
                // Nothing answered at all (the console is off or unreachable): the other
                // ports are on the same host, so they would only time out the same way.
                if e.connect_timed_out {
                    break;
                }
            }
        }
    }
    if let Ok(mut c) = port_cache().lock() {
        c.remove(host);
    }
    let range = match (ports.first(), ports.last()) {
        (Some(a), Some(b)) if a != b => format!("{a}-{b}"),
        (Some(a), _) => a.to_string(),
        _ => String::new(),
    };
    Err(format!(
        "HearBridge didn't answer on {host} port {range} ({last})"
    ))
}

/// A console address the routes accept: no whitespace, control characters or URL syntax
/// that could change where the request goes or what it says.
pub fn check_host(host: &str) -> Result<(), String> {
    let bad = host.is_empty()
        || host.len() > 255
        || host.chars().any(|c| {
            c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\' | '?' | '#' | '@')
        });
    if bad {
        return Err(format!("not a console address: {host:?}"));
    }
    Ok(())
}

fn status_at(host: &str, port: u16, deadline: Instant) -> Result<Found, HttpErr> {
    let (code, body) = request(host, port, "GET", "/api/status", None, deadline)?;
    if code != 200 {
        return Err(format!(":{port} answered HTTP {code}").into());
    }
    parse_status(&body)
        .map(|(status, token)| Found {
            port,
            status,
            token,
        })
        .map_err(HttpErr::from)
}

/// The status object without its token, and the token. Anything that isn't a JSON object
/// with a `version` string is not HearBridge (another service on that port).
fn parse_status(body: &[u8]) -> Result<(Value, String), String> {
    let mut v: Value =
        serde_json::from_slice(body).map_err(|_| "not HearBridge (no JSON status)".to_string())?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| "not HearBridge (status is not an object)".to_string())?;
    if !obj.get("version").is_some_and(Value::is_string) {
        return Err("not HearBridge (no version in its status)".into());
    }
    let token = match obj.remove("token") {
        Some(Value::String(t)) => t,
        _ => String::new(),
    };
    Ok((v, token))
}

/// Which list to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceList {
    /// Paired headsets (`/api/saved`).
    Saved,
    /// The last scan's results (`/api/devices`).
    Scanned,
}

/// `{"devices":[…]}` from HearBridge; an empty list when it has none or sent something else.
pub fn devices(host: &str, which: DeviceList) -> Result<Value, String> {
    devices_on(host, which, &HEARBRIDGE_PORTS)
}

fn devices_on(host: &str, which: DeviceList, ports: &[u16]) -> Result<Value, String> {
    let found = find_on(host, ports)?;
    let path = match which {
        DeviceList::Saved => "/api/saved",
        DeviceList::Scanned => "/api/devices",
    };
    let (code, body) = request(host, found.port, "GET", path, None, io_deadline())?;
    if code != 200 {
        return Err(format!("HearBridge could not list devices (HTTP {code})"));
    }
    Ok(parse_devices(&body))
}

fn parse_devices(body: &[u8]) -> Value {
    let list = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("devices").filter(|d| d.is_array()).cloned())
        .unwrap_or_else(|| Value::Array(Vec::new()));
    serde_json::json!({ "devices": list })
}

/// An action's parameters, as the app sends them.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ActionParams {
    pub pct: Option<i64>,
    pub on: Option<bool>,
    pub index: Option<i64>,
    pub addr: Option<String>,
    pub auto: Option<bool>,
    pub ms: Option<i64>,
}

/// The changes the app makes. Pairing, EQ, backups and the rest stay on HearBridge's page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Volume(u16),
    Mute(bool),
    Reconnect,
    SelectIndex(u8),
    SelectAddr(String),
    Scan,
    Night(bool),
    LatencyAuto(bool),
    LatencyMs(u16),
    Stop,
}

fn is_mac(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 17
        && b.iter().enumerate().all(|(i, c)| {
            if i % 3 == 2 {
                *c == b':'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

impl Action {
    /// The action `name` with its parameters checked against the ranges HearBridge takes.
    /// Anything else is refused here, so only these requests ever reach the console.
    pub fn parse(name: &str, p: &ActionParams) -> Result<Action, String> {
        let need_on = || p.on.ok_or_else(|| format!("{name} needs on"));
        Ok(match name {
            "volume" => match p.pct {
                Some(v) if (0..=500).contains(&v) => Action::Volume(v as u16),
                _ => return Err("volume needs pct 0-500".into()),
            },
            "mute" => Action::Mute(need_on()?),
            "night" => Action::Night(need_on()?),
            "reconnect" => Action::Reconnect,
            "scan" => Action::Scan,
            "stop" => Action::Stop,
            "select" => match (&p.addr, p.index) {
                (Some(a), _) if is_mac(a) => Action::SelectAddr(a.to_ascii_uppercase()),
                (Some(_), _) => return Err("select needs a Bluetooth address".into()),
                (None, Some(i)) if (0..64).contains(&i) => Action::SelectIndex(i as u8),
                _ => return Err("select needs index 0-63 or addr".into()),
            },
            "latency" => match (p.auto, p.ms) {
                (Some(a), _) => Action::LatencyAuto(a),
                (None, Some(ms)) if (40..=400).contains(&ms) => Action::LatencyMs(ms as u16),
                _ => return Err("latency needs auto or ms 40-400".into()),
            },
            other => return Err(format!("unknown HearBridge action {other:?}")),
        })
    }

    fn path(&self) -> String {
        let b = |on: bool| if on { 1 } else { 0 };
        match self {
            Action::Volume(v) => format!("/api/volume?pct={v}"),
            Action::Mute(on) => format!("/api/mute?on={}", b(*on)),
            Action::Night(on) => format!("/api/night?on={}", b(*on)),
            Action::Reconnect => "/api/reconnect".into(),
            Action::Scan => "/api/scan?user=1".into(),
            Action::Stop => "/api/stop".into(),
            Action::SelectIndex(i) => format!("/api/select?index={i}"),
            Action::SelectAddr(a) => format!("/api/select?addr={a}"),
            Action::LatencyAuto(a) => format!("/api/latency?auto={}", b(*a)),
            Action::LatencyMs(ms) => format!("/api/latency?ms={ms}"),
        }
    }
}

/// Send `action` to HearBridge on `host`. Returns its answer: the new status (token removed)
/// for most actions, `{"ok":1}` for select/scan/reconnect.
pub fn act(host: &str, action: &Action) -> Result<Value, String> {
    act_on(host, action, &HEARBRIDGE_PORTS)
}

fn act_on(host: &str, action: &Action, ports: &[u16]) -> Result<Value, String> {
    let path = action.path();
    let mut found = find_on(host, ports)?;
    for attempt in 0..2 {
        if found.token.is_empty() {
            return Err("HearBridge gave no token; it may be too old for changes from here".into());
        }
        let (code, body) = request(
            host,
            found.port,
            "POST",
            &path,
            Some(&found.token),
            io_deadline(),
        )?;
        // 403: the token changed (HearBridge restarted between the read and the change).
        if code == 403 && attempt == 0 {
            found = match status_at(host, found.port, io_deadline()) {
                Ok(f) => f,
                Err(_) => find_on(host, ports)?,
            };
            continue;
        }
        if code != 200 {
            let msg = String::from_utf8_lossy(&body);
            return Err(format!(
                "HearBridge refused {path} (HTTP {code}: {})",
                msg.trim()
            ));
        }
        let mut v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if let Some(o) = v.as_object_mut() {
            o.remove("token");
        }
        return Ok(v);
    }
    Err("HearBridge refused the token".into())
}

/// A failed request, and whether it failed because nothing accepted the connection in time.
#[derive(Debug)]
struct HttpErr {
    msg: String,
    connect_timed_out: bool,
}

impl From<String> for HttpErr {
    fn from(msg: String) -> Self {
        HttpErr {
            msg,
            connect_timed_out: false,
        }
    }
}

impl From<HttpErr> for String {
    fn from(e: HttpErr) -> Self {
        e.msg
    }
}

/// The deadline for one request outside discovery.
fn io_deadline() -> Instant {
    Instant::now() + IO_TIMEOUT
}

fn request(
    host: &str,
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
    deadline: Instant,
) -> Result<(u16, Vec<u8>), HttpErr> {
    let left = || deadline.saturating_duration_since(Instant::now());
    let addr = crate::payload_lifecycle::join_host_port(host, port);
    let sock = std::net::ToSocketAddrs::to_socket_addrs(&addr)
        .map_err(|e| format!("resolve {addr}: {e}"))?
        .next()
        .ok_or_else(|| format!("resolve {addr}: no address"))?;
    let connect_for = CONNECT_TIMEOUT.min(left());
    if connect_for.is_zero() {
        return Err(format!("no time left to connect to {addr}").into());
    }
    let mut stream = TcpStream::connect_timeout(&sock, connect_for).map_err(|e| HttpErr {
        connect_timed_out: e.kind() == std::io::ErrorKind::TimedOut,
        msg: format!("connect {addr}: {e}"),
    })?;
    let io_for = IO_TIMEOUT.min(left()).max(Duration::from_millis(1));
    stream.set_read_timeout(Some(io_for)).ok();
    stream.set_write_timeout(Some(io_for)).ok();
    stream
        .write_all(request_head(method, path, &addr, token).as_bytes())
        .map_err(|e| format!("send to {addr}: {e}"))?;
    // Read to the end, but never past the deadline: a server that trickles bytes would
    // otherwise reset the per-read timeout forever.
    let mut resp = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        if Instant::now() >= deadline {
            return Err(format!("{addr} took too long to answer").into());
        }
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                resp.extend_from_slice(&buf[..n]);
                if resp.len() > MAX_RESPONSE {
                    return Err(format!("{addr} sent more than {MAX_RESPONSE} bytes").into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("read from {addr}: {e}").into()),
        }
    }
    parse_response(&resp).ok_or_else(|| format!("{addr} sent a response that is not HTTP").into())
}

/// The request line and headers. The User-Agent starts with "curl" on purpose: HearBridge
/// takes a status read from anything else as its page being open in a browser
/// (`page_seen_ms`), which may start a link wake meant for someone looking at that page
/// (http.c `/api/status`, main.c `lw_tick`). It does not avoid everything: every status read
/// still counts in `status_polls`, which lets HearBridge re-query the headset's volume while
/// reads keep coming, so the app polls slowly unless the Manage panel is open.
fn request_head(method: &str, path: &str, addr: &str, token: Option<&str>) -> String {
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nUser-Agent: curl-compatible ps5upload\r\n"
    );
    if let Some(t) = token {
        head.push_str(&format!("X-HB-Token: {t}\r\n"));
    }
    if method == "POST" {
        head.push_str("Content-Length: 0\r\n");
    }
    head.push_str("\r\n");
    head
}

fn parse_response(resp: &[u8]) -> Option<(u16, Vec<u8>)> {
    let split = resp.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&resp[..split]).ok()?;
    let status: u16 = head
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some((status, resp[split + 4..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::thread;
    use std::time::Instant;

    /// One request as the fake server saw it.
    #[derive(Debug, Clone)]
    struct Seen {
        line: String,
        token: Option<String>,
        ua: Option<String>,
    }

    /// A fake HearBridge on an ephemeral port: answers each request with `reply(line)`.
    fn fake(
        reply: impl Fn(&str, Option<&str>) -> (u16, String) + Send + Sync + 'static,
    ) -> (u16, Arc<Mutex<Vec<Seen>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let reply = Arc::new(reply);
        thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                let mut buf = [0u8; 4096];
                let mut got = Vec::new();
                while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => got.extend_from_slice(&buf[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&got).to_string();
                let line = text.lines().next().unwrap_or("").to_string();
                let header = |name: &str| {
                    text.lines().find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
                    })
                };
                let token = header("X-HB-Token");
                seen2.lock().unwrap().push(Seen {
                    line: line.clone(),
                    token: token.clone(),
                    ua: header("User-Agent"),
                });
                let (code, body) = reply(&line, token.as_deref());
                let _ = write!(
                    s,
                    "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (port, seen)
    }

    fn closed_port() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }

    const STATUS: &str = r#"{"version":"1.9.0","token":"abc123","connected":1,"state":"streaming","device":"WH-1000XM4","gain_pct":120,"muted":0,"codec":"SBC","latency":{"target_ms":120,"estimate_ms":180},"battery":{"status":"ok","level":3,"pct":80},"link":{"score":92,"rssi":-50},"events":["Connected"]}"#;

    #[test]
    fn reads_the_status_without_handing_out_the_token() {
        let (port, seen) = fake(|_, _| (200, STATUS.into()));
        let f = find_on("127.0.0.1", &[port]).unwrap();
        assert_eq!(f.port, port);
        assert_eq!(f.status["device"], "WH-1000XM4");
        assert!(f.status.get("token").is_none());
        assert_eq!(f.token, "abc123");
        let s = seen.lock().unwrap();
        assert_eq!(s[0].line, "GET /api/status HTTP/1.1");
        // Not counted as HearBridge's page being open (see request_head).
        assert!(s[0].ua.as_deref().unwrap().starts_with("curl"));
    }

    #[test]
    fn tolerates_an_older_status_with_few_fields() {
        let (status, token) = parse_status(br#"{"version":"0.3"}"#).unwrap();
        assert_eq!(status["version"], "0.3");
        assert_eq!(token, "");
        assert!(parse_status(b"<html>").is_err());
        assert!(parse_status(br#"{"ok":1}"#).is_err());
        assert!(parse_status(br#"[1]"#).is_err());
    }

    #[test]
    fn falls_back_to_the_next_port_like_hearbridge_does() {
        let dead = closed_port();
        // Something else on the second port: answers, but isn't HearBridge.
        let (other, _) = fake(|_, _| (200, "<html>hi</html>".into()));
        let (hb, _) = fake(|_, _| (200, STATUS.into()));
        let host = "127.0.0.1";
        let f = find_on(host, &[dead, other, hb]).unwrap();
        assert_eq!(f.port, hb);
        let err = find_on(host, &[dead]).unwrap_err();
        assert!(err.contains("HearBridge didn't answer"), "{err}");
    }

    #[test]
    fn sends_the_token_with_every_change() {
        let (port, seen) = fake(|line, token| {
            if line.starts_with("GET /api/status") {
                (200, STATUS.into())
            } else if token == Some("abc123") {
                (
                    200,
                    r#"{"version":"1.9.0","token":"abc123","gain_pct":200}"#.into(),
                )
            } else {
                (403, r#"{"error":"token"}"#.into())
            }
        });
        let v = act_on("127.0.0.1", &Action::Volume(200), &[port]).unwrap();
        assert_eq!(v["gain_pct"], 200);
        assert!(v.get("token").is_none());
        let s = seen.lock().unwrap();
        let post = s.iter().find(|r| r.line.starts_with("POST")).unwrap();
        assert_eq!(post.line, "POST /api/volume?pct=200 HTTP/1.1");
        assert_eq!(post.token.as_deref(), Some("abc123"));
    }

    #[test]
    fn rereads_the_token_once_when_hearbridge_restarted() {
        let calls = Arc::new(Mutex::new(0));
        let c2 = calls.clone();
        let (port, _) = fake(move |line, token| {
            if line.starts_with("GET /api/status") {
                let mut n = c2.lock().unwrap();
                *n += 1;
                // First read hands out a token that is stale by the time it is used.
                let t = if *n == 1 { "old" } else { "new" };
                (200, format!(r#"{{"version":"1.9.0","token":"{t}"}}"#))
            } else if token == Some("new") {
                (200, r#"{"ok":1}"#.into())
            } else {
                (403, r#"{"error":"token"}"#.into())
            }
        });
        let v = act_on("127.0.0.1", &Action::Reconnect, &[port]).unwrap();
        assert_eq!(v["ok"], 1);
    }

    #[test]
    fn a_missing_token_is_a_clear_error() {
        let (port, _) = fake(|_, _| (200, r#"{"version":"0.3"}"#.into()));
        let e = act_on("127.0.0.1", &Action::Stop, &[port]).unwrap_err();
        assert!(e.contains("no token"), "{e}");
    }

    #[test]
    fn lists_devices_and_survives_garbage() {
        let (port, _) = fake(|line, _| {
            if line.starts_with("GET /api/status") {
                (200, STATUS.into())
            } else if line.starts_with("GET /api/saved") {
                (
                    200,
                    r#"{"devices":[{"addr":"AA:BB:CC:DD:EE:FF","name":"Buds","kind":"audio","current":1}]}"#
                        .into(),
                )
            } else {
                (200, "not json".into())
            }
        });
        let saved = devices_on("127.0.0.1", DeviceList::Saved, &[port]).unwrap();
        assert_eq!(saved["devices"][0]["name"], "Buds");
        let scanned = devices_on("127.0.0.1", DeviceList::Scanned, &[port]).unwrap();
        assert_eq!(scanned, serde_json::json!({ "devices": [] }));
    }

    #[test]
    fn refuses_console_addresses_that_are_not_one() {
        for ok in ["192.0.2.99", "ps5.local", "[fe80::1]", "10.0.0.2:9120"] {
            assert!(check_host(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "1.2.3.4 x",
            "1.2.3.4\r\nX: y",
            "1.2.3.4/api",
            "a?b",
            "u@h",
            "a\\b",
        ] {
            assert!(check_host(bad).is_err(), "{bad:?}");
            assert!(find_on(bad, &[1]).is_err());
        }
    }

    #[test]
    fn an_unreachable_console_is_not_tried_on_every_port() {
        // A blackholed address: the first connect times out (or the network says
        // unreachable at once); either way the other five ports are not waited on.
        let t = Instant::now();
        let e = find_on("10.255.255.1", &HEARBRIDGE_PORTS).unwrap_err();
        assert!(
            t.elapsed() < Duration::from_secs(4),
            "{:?} {e}",
            t.elapsed()
        );
    }

    #[test]
    fn a_server_that_never_answers_is_given_up_on() {
        // Accepts and then says nothing: the read times out, discovery moves on.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let silent = l.local_addr().unwrap().port();
        thread::spawn(move || {
            let _held: Vec<_> = l.incoming().take(4).collect();
            thread::sleep(Duration::from_secs(30));
        });
        let (hb, _) = fake(|_, _| (200, STATUS.into()));
        let t = Instant::now();
        assert_eq!(find_on("127.0.0.1", &[silent, hb]).unwrap().port, hb);
        assert!(t.elapsed() <= FIND_DEADLINE + Duration::from_secs(1));
    }

    #[test]
    fn only_allowlisted_actions_with_sane_values_pass() {
        let p = |f: fn(&mut ActionParams)| {
            let mut a = ActionParams::default();
            f(&mut a);
            a
        };
        assert_eq!(
            Action::parse("volume", &p(|a| a.pct = Some(500)))
                .unwrap()
                .path(),
            "/api/volume?pct=500"
        );
        assert!(Action::parse("volume", &p(|a| a.pct = Some(501))).is_err());
        assert!(Action::parse("volume", &p(|_| {})).is_err());
        assert_eq!(
            Action::parse("mute", &p(|a| a.on = Some(true)))
                .unwrap()
                .path(),
            "/api/mute?on=1"
        );
        assert_eq!(
            Action::parse("scan", &p(|_| {})).unwrap().path(),
            "/api/scan?user=1"
        );
        assert_eq!(
            Action::parse("select", &p(|a| a.index = Some(3)))
                .unwrap()
                .path(),
            "/api/select?index=3"
        );
        assert_eq!(
            Action::parse("select", &p(|a| a.addr = Some("aa:bb:cc:dd:ee:ff".into())))
                .unwrap()
                .path(),
            "/api/select?addr=AA:BB:CC:DD:EE:FF"
        );
        assert!(Action::parse("select", &p(|a| a.addr = Some("x&y=1".into()))).is_err());
        assert!(Action::parse("select", &p(|a| a.index = Some(64))).is_err());
        assert_eq!(
            Action::parse("latency", &p(|a| a.auto = Some(false)))
                .unwrap()
                .path(),
            "/api/latency?auto=0"
        );
        assert_eq!(
            Action::parse("latency", &p(|a| a.ms = Some(40)))
                .unwrap()
                .path(),
            "/api/latency?ms=40"
        );
        assert!(Action::parse("latency", &p(|a| a.ms = Some(39))).is_err());
        for refused in ["forget", "eq", "restore", "backup", "hci", "../stop", ""] {
            assert!(Action::parse(refused, &p(|_| {})).is_err(), "{refused}");
        }
    }
}
