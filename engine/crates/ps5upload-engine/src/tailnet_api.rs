//! "Put this PS5 on Tailscale" (`ps5upload_core::tailnet`, the helper's `tailnet` method).
//!
//! * `GET  /api/ps5/tailnet?addr=`  -> the console's node: `{ok, enabled, state, login_url, ip,
//!   name, hostname, signed_in, allow_mode, allow, home_derp, derp_connected, home_latency_ms,
//!   udp_port, endpoints[], key_expiry, warning, error, serving:{udp_flows, tcp_conns},
//!   peers:[{name, ip, online, path, direct, derp_region, latency_ms, last_rx_ms}],
//!   blocked_recent:[{ip, name, port, proto, count, first, last}]}`; `state` is
//!   `off`, `connecting`, `needs-login` (open `login_url`), `wait-approval`, `running`, `paused`,
//!   `backoff` or `signed-out`. `allow_mode` is `listed` (only the devices in `allow`, a
//!   comma-separated list of tailnet IPs, may reach the PS5) or `all`. `blocked_recent`: the devices
//!   the list turned away lately (at most 8, newest first, unix-second times; empty with `all`):
//!   the app offers Allow (`allow_add`) or Ignore (`blocked_ignore`). Absent from an older helper.
//! * `POST /api/ps5/tailnet` `{addr, op, control_url?, hostname?, authkey?, allow_mode?, allow?,
//!   ip?}`: `op` is `enable` (join; the first time this answers `needs-login` with a `login_url`
//!   to open, unless an `authkey` was given; with no `hostname` and none saved, the console's own
//!   name + "-ps5u"), `disable` (pause: leave, keep the keys), `logout` (leave and forget them),
//!   `rename` (`hostname`, any time), `renew` (sign in again keeping the node, its address and
//!   name: a `login_url` follows), `allow` (`allow_mode`, `allow`), `allow_add` (`ip`) or
//!   `blocked_ignore` (`ip`: stop listing it in `blocked_recent` until the helper restarts).
//!   Answers the same status. Needs an explicit `addr`.
//!
//! The engine's own node (Settings › Tailscale; `tailnet_engine.rs`):
//!
//! * `GET  /api/tailnet` -> `{ok, enabled, state, login_url, ip, name, control_url, hostname,
//!   default_hostname, renamed_to, signed_in, home_derp, derp_connected, home_latency_ms,
//!   key_expiry, error, warning, backend, system:{detected, ip, iface, choice}, peers:[{name, ip,
//!   online, path, direct, derp_region, latency_ms}], consoles:[{host, ip, name, via,
//!   latency_ms, tested_at}]}`; `tested_at` (unix ms, 0 never): when "Try Tailscale now" last
//!   reached that console through Tailscale
//! * `POST /api/tailnet` `{op, control_url?, hostname?, authkey?, choice?, host?, ip?, secs?}`:
//!   `op` is `enable`, `disable` (pause), `logout`, `rename`, `renew`, `system_choice`
//!   (`choice`: `system` to use the system's Tailscale app, `own` to sign PS5Upload in itself),
//!   `remember` (`host`, `ip`, `name?`) or `test_path` (`host`, `secs`: send that console through
//!   Tailscale for a while from home, to see it work; 0 ends it) -> the same status
//! * `GET  /api/tailnet/via?addr=` -> `{via: lan|direct|relayed|system, latency_ms}`
//! * `GET  /api/tailnet/route?addr=[&port=&udp=]` -> why the engine reaches that console the way
//!   it does: `{decision, via, backend, home:{answers, age_ms}, tailnet_ip, tailnet_online,
//!   testing, last_switch_ms, ...}`; with a `port`, also `dial` (null = its own address)
//! * `GET  /api/tailnet/check?addr=` -> the Connection check by path: `{home:[{port, open,
//!   error}], tailnet:{backend, ip, online, checks:[...]}|null, used, device:{...}}`
//! * `GET  /api/tailnet/consoles` and `POST /api/tailnet/consoles/add`: the consoles on this device's
//!   Tailscale network, met at home or not (`tailnet_discover.rs`)
//!
//! The console then serves only Remote Play (TCP 9295, UDP 9296, 9302) and the helper (TCP 9120)
//! to the tailnet. Errors are `{error, reason}`; `unsupported` (501) when the helper is older.

use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::tailnet as tn;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tailnet_engine::Backend;
use crate::{console_addr_or_default, required_console_addr, AppState};

#[derive(Deserialize)]
pub(crate) struct AddrQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct OpBody {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    op: String,
    #[serde(default)]
    control_url: String,
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    authkey: String,
    #[serde(default)]
    allow_mode: String,
    #[serde(default)]
    allow: String,
    #[serde(default)]
    ip: String,
}

fn reply(r: anyhow::Result<Option<Value>>) -> Response {
    match r {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({
                "error": "this PS5's helper is older than Tailscale support; send the payload again",
                "reason": "unsupported",
            })),
        )
            .into_response(),
        Err(e) => {
            let reason = crate::console_cleanup_api::reason_of(&e);
            let code = match reason.as_str() {
                "bad_request" | "unknown_op" => StatusCode::BAD_REQUEST,
                "unsupported" => StatusCode::NOT_IMPLEMENTED,
                _ => StatusCode::BAD_GATEWAY,
            };
            (code, Json(json!({ "error": format!("{e:#}"), "reason": reason }))).into_response()
        }
    }
}

/// Whether a console's allow-list should get this device: it lists devices, and this device's
/// tailnet address is not on it yet.
pub(crate) fn needs_self_added(status: &Value, me: &str) -> bool {
    if me.is_empty() || status.get("allow_mode").and_then(Value::as_str) != Some("listed") {
        return false;
    }
    let list = status.get("allow").and_then(Value::as_str).unwrap_or("");
    !list.split(',').any(|x| x.trim() == me)
}

/// A console's node answered: remember its tailnet address for when it is away, and, when it
/// was reached at home, put this device on its allow-list (a device that talks to the PS5 at
/// home through PS5Upload is one the user added).
fn remember(addr: &str, r: &mut anyhow::Result<Option<Value>>) {
    let Ok(Some(v)) = r else { return };
    if let Some(ip) = tn::tailnet_ip(v).and_then(|s| s.parse().ok()) {
        let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
        crate::tailnet_engine::engine().remember(addr, ip, name.trim_end_matches('.'));
    }
    if ps5upload_core::route::via(addr).is_tailnet() {
        return;
    }
    // Reached at home: an entry added away at its tailnet address is this console (it says so
    // itself), so it becomes the home one, never a second entry.
    if let Some(ip) = tn::tailnet_ip(v) {
        if crate::consoles_api::link_home(addr, &ip) {
            crate::log_at!(
                Info,
                "tailnet",
                "tailnet {addr}: linked to its entry at {ip}"
            );
        }
    }
    let Some(me) = crate::tailnet_engine::engine().self_ip() else {
        return;
    };
    if needs_self_added(v, &me.to_string()) {
        let op = tn::Op::AllowAdd { ip: me.to_string() };
        if let Ok(Some(after)) = tn::console(addr, &op) {
            crate::log_at!(
                Info,
                "tailnet",
                "tailnet {addr}: this device added to its list"
            );
            *v = after;
        }
    }
}

// ── A pause the console could not take ──────────────────────────────────────────────────────────
//
// The console keeps a pause itself (its settings.json: through a helper send, a reboot, an app
// restart). One sent while it can't be reached (off, no helper, away) never got there: the engine
// keeps it (`tailnet-app/pending-pause.json`) and delivers it the next time the console answers.
// Switching it on (or Sign out) drops it.

const PENDING_FILE: &str = "pending-pause.json";

static PENDING: std::sync::Mutex<Option<std::collections::BTreeSet<String>>> =
    std::sync::Mutex::new(None);

fn pending_path() -> Option<std::path::PathBuf> {
    if cfg!(test) {
        return None; // the tests never write the user's data folder
    }
    crate::remote::store::data_dir().map(|d| d.join("tailnet-app").join(PENDING_FILE))
}

fn with_pending<R>(f: impl FnOnce(&mut std::collections::BTreeSet<String>) -> R) -> R {
    let mut g = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let set = g.get_or_insert_with(|| {
        pending_path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    });
    let before = set.clone();
    let r = f(set);
    if *set != before {
        if let Some(p) = pending_path() {
            let _ = p.parent().map(std::fs::create_dir_all);
            let tmp = p.with_extension("json.tmp");
            let ok = serde_json::to_vec(&*set)
                .ok()
                .is_some_and(|b| std::fs::write(&tmp, b).is_ok())
                && std::fs::rename(&tmp, &p).is_ok();
            if !ok {
                crate::log_at!(
                    Warn,
                    "tailnet",
                    "tailnet: the paused consoles list was not saved"
                );
            }
        }
    }
    r
}

pub(crate) fn pause_pending(addr: &str) -> bool {
    with_pending(|s| s.contains(addr))
}

fn set_pause_pending(addr: &str, on: bool) {
    with_pending(|s| {
        if on {
            s.insert(addr.to_string());
        } else {
            s.remove(addr);
        }
    });
}

/// After an op on `addr`: a pause it could not take is kept; switching on or signing out drops
/// any kept pause. Answers what the app should show for a kept pause (paused, until it lands).
fn after_op(
    addr: &str,
    op: &tn::Op,
    r: anyhow::Result<Option<Value>>,
) -> anyhow::Result<Option<Value>> {
    match op {
        tn::Op::Disable => {
            if let Err(e) = &r {
                set_pause_pending(addr, true);
                crate::log_at!(
                    Info,
                    "tailnet",
                    "tailnet {addr}: pause kept until the PS5 answers ({e:#})"
                );
                return Ok(Some(json!({
                    "ok": true,
                    "enabled": false,
                    "state": "off",
                    // Only a set-up node is paused: the panel shows Paused, not Set up.
                    "signed_in": true,
                    "pause_pending": true,
                })));
            }
            set_pause_pending(addr, false);
        }
        tn::Op::Enable { .. } | tn::Op::Logout => set_pause_pending(addr, false),
        _ => {}
    }
    r
}

/// Before reading a console's status: a kept pause goes first.
fn deliver_pause(addr: &str, call: &dyn Fn(&tn::Op) -> anyhow::Result<Option<Value>>) {
    if !pause_pending(addr) {
        return;
    }
    if let Ok(Some(_)) = call(&tn::Op::Disable) {
        set_pause_pending(addr, false);
        crate::log_at!(
            Info,
            "tailnet",
            "tailnet {addr}: the pause made while it was away is in place"
        );
    }
}

pub(crate) async fn status(State(state): State<AppState>, Query(q): Query<AddrQuery>) -> Response {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || {
        deliver_pause(&addr, &|op| tn::console(&addr, op));
        let mut r = tn::console(&addr, &tn::Op::Status);
        remember(&addr, &mut r);
        r
    })
    .await;
    reply(r.unwrap_or_else(|e| Err(anyhow::anyhow!("tailnet task failed: {e}"))))
}

#[derive(Deserialize)]
pub(crate) struct EngineOpBody {
    op: String,
    /// op `remember`: a console's LAN address and its tailnet address (what "Put this PS5 on
    /// Tailscale" records by itself). op `test_path`: the console.
    #[serde(default)]
    host: String,
    #[serde(default)]
    ip: String,
    /// op `remember`: the console's name on the tailnet (optional).
    #[serde(default)]
    name: String,
    #[serde(default)]
    control_url: String,
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    authkey: String,
    /// op `system_choice`: "system" or "own".
    #[serde(default)]
    choice: String,
    /// op `test_path`: how long, 0 to end it.
    #[serde(default)]
    secs: u64,
}

pub(crate) async fn engine_status() -> Response {
    match tokio::task::spawn_blocking(|| crate::tailnet_engine::engine().status_json()).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn engine_op(Json(req): Json<EngineOpBody>) -> Response {
    // The auth key is a secret: never logged.
    crate::log_at!(Info, "tailnet", "tailnet (this device) {}", req.op);
    let r = tokio::task::spawn_blocking(move || {
        let e = crate::tailnet_engine::engine();
        match req.op.as_str() {
            "remember" => {
                let ip: std::net::Ipv4Addr = req.ip.parse().map_err(|_| {
                    anyhow::anyhow!("ip must be the console's tailnet IPv4 address")
                })?;
                if req.host.trim().is_empty() {
                    anyhow::bail!("host (the console's LAN address) is required");
                }
                e.remember(&req.host, ip, req.name.trim());
            }
            "test_path" => {
                if req.host.trim().is_empty() {
                    anyhow::bail!("host (the console's address) is required");
                }
                e.test_path(&req.host, req.secs)?;
            }
            _ => {}
        }
        e.op(
            &req.op,
            &req.control_url,
            &req.hostname,
            &req.authkey,
            &req.choice,
        )
    })
    .await;
    match r {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("{e:#}"), "reason": "bad_request" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct RouteQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    udp: bool,
}

/// `GET /api/tailnet/route?addr=[&port=&udp=]`: why the engine reaches that console the way it
/// does now (all in memory: the last home probe and its age, the tailnet address, the way this
/// device has, the last switch). With a `port`, also `dial`: where it dials that port (null =
/// the console's own address; opens the local mapping when the console is away).
pub(crate) async fn route(State(state): State<AppState>, Query(q): Query<RouteQuery>) -> Response {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let v = tokio::task::spawn_blocking(move || {
        let e = crate::tailnet_engine::engine();
        let mut v = e.explain(&addr);
        if let Some(port) = q.port {
            let d = e.route(&addr, port, q.udp);
            v["dial"] = json!(d.map(|a| a.to_string()));
        }
        v
    })
    .await
    .unwrap_or_else(|e| json!({ "error": e.to_string() }));
    Json(v).into_response()
}

pub(crate) async fn via(State(state): State<AppState>, Query(q): Query<AddrQuery>) -> Response {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let v = tokio::task::spawn_blocking(move || ps5upload_core::route::via(&addr))
        .await
        .unwrap_or(ps5upload_core::route::Via::Lan);
    let (via, ms) = crate::tailnet_engine::via_json(&v);
    Json(json!({ "via": via, "latency_ms": ms })).into_response()
}

/// The ports the Connection check looks at on the home network: the loader, the helper and
/// Remote Play.
const HOME_PORTS: &[u16] = &[9021, 9120, 9295];
/// Over the tailnet: what the console serves there (9021 never is).
const TAILNET_PORTS: &[u16] = &[9120, 9295];
const CHECK_TIMEOUT: Duration = Duration::from_millis(1500);

fn probe_all(host: &str, ports: &[u16]) -> Vec<Value> {
    let handles: Vec<_> = ports
        .iter()
        .map(|&port| {
            let host = host.to_string();
            std::thread::spawn(move || {
                let r = ps5upload_core::payload_lifecycle::probe_port(&host, port, CHECK_TIMEOUT);
                json!({ "port": port, "open": r.is_ok(), "error": r.err() })
            })
        })
        .collect();
    handles.into_iter().filter_map(|h| h.join().ok()).collect()
}

/// `GET /api/tailnet/check?addr=`: the Connection check by path. The home network first (the
/// loader, the helper, Remote Play); when nothing answers there and the console has a Tailscale
/// address, the same over Tailscale. Says which path each check used, and this device's own
/// Tailscale state. Blocking work: a few connects, at most a couple of seconds.
pub(crate) async fn check(State(state): State<AppState>, Query(q): Query<AddrQuery>) -> Response {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let v = tokio::task::spawn_blocking(move || check_blocking(&addr))
        .await
        .unwrap_or_else(|e| json!({ "error": e.to_string() }));
    Json(v).into_response()
}

fn check_blocking(addr: &str) -> Value {
    let e = crate::tailnet_engine::engine();
    let host = ps5upload_core::events::console_host(addr);
    let home = probe_all(&host, HOME_PORTS);
    let home_ok = home.iter().any(|c| c["open"] == true);
    let me = e.status_json();
    let backend = e.backend();
    let device = json!({
        "backend": me["backend"],
        "enabled": me["enabled"],
        "signed_in": me["signed_in"],
        "state": me["state"],
        "system_detected": me["system"]["detected"],
    });
    let ip = e.known_ip(&host);
    let tailnet = match (home_ok, ip) {
        (false, Some(ip)) if backend != Backend::None => {
            let explain = e.explain(&host);
            let checks: Vec<Value> = match backend {
                // The system app: the console's tailnet address itself.
                Backend::System => probe_all(&ip.to_string(), TAILNET_PORTS),
                // The built-in node: a local mapping always accepts, so its peer list says
                // whether the console is there.
                _ => {
                    let online = explain["tailnet_online"] == true;
                    TAILNET_PORTS
                        .iter()
                        .map(|p| json!({ "port": p, "open": online, "error": if online { Value::Null } else { json!("not online on Tailscale") } }))
                        .collect()
                }
            };
            Some(json!({
                "backend": backend_name(backend),
                "ip": ip.to_string(),
                "online": explain["tailnet_online"],
                "checks": checks,
            }))
        }
        _ => None,
    };
    let used = if home_ok {
        "lan"
    } else if tailnet.as_ref().is_some_and(|t| {
        t["checks"]
            .as_array()
            .is_some_and(|c| c.iter().any(|x| x["open"] == true))
    }) {
        backend_name(backend)
    } else {
        "none"
    };
    json!({
        "home": home,
        "tailnet": tailnet,
        "tailnet_ip": ip.map(|i| i.to_string()),
        "used": used,
        "device": device,
    })
}

fn backend_name(b: Backend) -> &'static str {
    match b {
        Backend::Builtin => "builtin",
        Backend::System => "system",
        Backend::None => "none",
    }
}

/// The name a console asks for when none is set: its own name (DDP host-name) + "-ps5u".
fn default_console_hostname(addr: &str) -> String {
    let ddp = ps5upload_core::ddp::probe(addr, Duration::from_secs(2))
        .map(|s| s.host_name)
        .unwrap_or_default();
    crate::tailnet_system::default_console_name(&ddp)
}

pub(crate) async fn op(Json(req): Json<OpBody>) -> Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    let fields = tn::OpFields {
        control_url: &req.control_url,
        hostname: &req.hostname,
        authkey: &req.authkey,
        allow_mode: &req.allow_mode,
        allow: &req.allow,
        ip: &req.ip,
    };
    let op = match tn::Op::parse_fields(&req.op, &fields) {
        Ok(op) => op,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("{e:#}"), "reason": "bad_request" })),
            )
                .into_response()
        }
    };
    // The auth key is a secret: never logged.
    crate::log_at!(Info, "tailnet", "tailnet {}: addr={addr}", req.op);
    let r = tokio::task::spawn_blocking(move || {
        let mut op = op;
        // First enable with no name: the console's own name + "-ps5u", unless one is saved.
        if let tn::Op::Enable { hostname, .. } = &mut op {
            if hostname.is_empty() {
                let saved = tn::console(&addr, &tn::Op::Status)
                    .ok()
                    .flatten()
                    .and_then(|v| {
                        v.get("hostname")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or_default();
                if saved.is_empty() {
                    *hostname = default_console_hostname(&addr);
                }
            }
        }
        let r = tn::console(&addr, &op);
        let mut r = after_op(&addr, &op, r);
        remember(&addr, &mut r);
        r
    })
    .await;
    reply(r.unwrap_or_else(|e| Err(anyhow::anyhow!("tailnet task failed: {e}"))))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pause sent while the PS5 can't be reached is kept and shown as paused, lands the next time
    /// it answers, and is dropped by switching it on.
    #[test]
    fn a_pause_the_console_missed_lands_when_it_answers() {
        let addr = "192.0.2.77";
        let down = || Err(anyhow::anyhow!("connect: timed out"));
        let shown = after_op(addr, &tn::Op::Disable, down()).unwrap().unwrap();
        assert_eq!(shown["enabled"], false);
        assert_eq!(shown["pause_pending"], true);
        assert!(pause_pending(addr));
        // Still away: kept.
        deliver_pause(addr, &|_| down());
        assert!(pause_pending(addr));
        // Back: the pause is sent, once.
        let sent = std::sync::Mutex::new(Vec::new());
        deliver_pause(addr, &|op| {
            sent.lock()
                .unwrap()
                .push(format!("{:?}", tn::request_body(op)));
            Ok(Some(json!({"ok": true, "enabled": false})))
        });
        assert!(!pause_pending(addr));
        assert_eq!(sent.lock().unwrap().len(), 1);
        assert!(sent.lock().unwrap()[0].contains("disable"));
        deliver_pause(addr, &|_| panic!("nothing left to send"));
        // Switched on while away: the kept pause is dropped.
        let _ = after_op(addr, &tn::Op::Disable, down());
        let on = tn::Op::parse_fields("enable", &tn::OpFields::default()).unwrap();
        let _ = after_op(addr, &on, down());
        assert!(!pause_pending(addr));
    }

    #[test]
    fn this_device_is_added_only_to_a_listing_console_that_lacks_it() {
        let listed = json!({ "allow_mode": "listed", "allow": "100.64.0.5,100.64.0.6" });
        assert!(!needs_self_added(&listed, "100.64.0.5"));
        assert!(needs_self_added(&listed, "100.64.0.7"));
        assert!(needs_self_added(
            &json!({ "allow_mode": "listed", "allow": "" }),
            "100.64.0.7"
        ));
        assert!(!needs_self_added(
            &json!({ "allow_mode": "all", "allow": "" }),
            "100.64.0.7"
        ));
        // An older helper says nothing of a list: nothing to add.
        assert!(!needs_self_added(
            &json!({ "ip": "100.64.0.9" }),
            "100.64.0.7"
        ));
        assert!(!needs_self_added(&listed, ""));
    }
}
