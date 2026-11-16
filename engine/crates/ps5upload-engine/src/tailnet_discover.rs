//! "Find PS5s on your Tailscale network": the consoles a device can reach on the tailnet without
//! ever having met them at home.
//!
//! * The candidates are this device's tailnet peers (the engine's own node's netmap, and the
//!   system Tailscale app's list where its CLI can be read: desktop only) that are online and
//!   named like a PS5Upload console (`…-ps5u`, or `…-ps5u-N` when Tailscale made the name
//!   unique). `all=1` takes every online peer instead (a console renamed by hand). The name is
//!   no proof: the app on a phone or computer names its own node `…-ps5u` too. A peer whose
//!   Tailscale reports a phone's or computer's OS is left out, and each entry says what the
//!   probe made of it (`kind`, [`peer_kind`]): only `ps5` is a console.
//! * Each is confirmed by connecting to its tailnet address on TCP 9120 through the way this
//!   device has (the node's local mapping, or the address itself through the system app) and
//!   reading the first two AVA1 handshake messages: the console's key and nothing more. The
//!   handshake is dropped before this device says who it is, so the console admits, pairs and
//!   lists nothing for it (`ps5upload_ava1::Pool::peek_console`).
//! * The key ties the console to the addresses this engine met it at (the AVA1 pins) and to the
//!   tailnet engine's known consoles: its home address when one is known, and the console list's
//!   entry for it (`listed`), so the same console at home and away is one entry.
//! * Nothing runs in the background. A list is kept for a minute; `refresh=1` looks again. The app
//!   asks while the Connection screen is open.
//!
//! On a phone with only the system Tailscale app there is no peer list to read (`can_list` false):
//! `addr=` probes one address the user typed (a 100.x address or a MagicDNS name).

use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tailnet_engine::{engine, PeerSeen};
use crate::tailnet_system::{is_tailscale_ip, SUFFIX};

/// How long a list is answered from memory.
const CACHE_TTL: Duration = Duration::from_secs(60);
/// One console's probe, connect and two handshake messages.
const PROBE_LIMIT: Duration = Duration::from_millis(2500);
/// At most this many peers are probed per look.
const MAX_PROBES: usize = 16;
const HELPER_PORT: u16 = 9120;

/// Is this a PS5Upload console's tailnet name: `<base>-ps5u`, or `<base>-ps5u-<n>` (Tailscale's
/// own de-duplication)? Only the first DNS label counts.
pub fn is_console_name(name: &str) -> bool {
    let label = name
        .trim_end_matches('.')
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if label.ends_with(SUFFIX) && label.len() > SUFFIX.len() {
        return true;
    }
    match label.rsplit_once('-') {
        Some((head, n)) => {
            !n.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
                && head.ends_with(SUFFIX)
                && head.len() > SUFFIX.len()
        }
        None => false,
    }
}

/// The OS a PS5's helper node reports to Tailscale (payload/src/tailnet_node.c).
const PS5_OS: &str = "ps5";

/// Does the OS a peer's Tailscale reports say it is something other than a PS5 (a phone, a
/// computer)? False when no OS is known.
pub fn os_is_not_ps5(os: &str) -> bool {
    let os = os.trim();
    !os.is_empty() && !os.eq_ignore_ascii_case(PS5_OS)
}

/// What a tailnet peer is, from the strongest signal there is:
///
/// * `"ps5"`: its PS5Upload helper answered the AVA1 handshake on 9120 as the PS5 (only the
///   helper serves AVA1 there; the app on a phone or computer does not), or its Tailscale says
///   its OS is the PS5's.
/// * `"device"`: its Tailscale says it is another OS (a phone, a computer), or something other
///   than the helper answered.
/// * `"unknown"`: no answer and no OS. Every PS5Upload device names its node `…-ps5u`, the app on
///   a phone or computer as well as the PS5, so a name like a console's is no sign of one.
pub fn peer_kind(os: &str, p: &Probe) -> &'static str {
    if p.key.is_some() {
        let says_ps5 = p
            .name
            .as_deref()
            .is_none_or(|n| n.eq_ignore_ascii_case("PS5"));
        return if says_ps5 && !os_is_not_ps5(os) {
            "ps5"
        } else {
            "device"
        };
    }
    if os_is_not_ps5(os) {
        "device"
    } else if os.trim().eq_ignore_ascii_case(PS5_OS) {
        "ps5"
    } else {
        "unknown"
    }
}

/// The peers worth probing: online, named like a console (or any, with `all`), not this device,
/// not one whose Tailscale says it is a phone or a computer, one per address, at most
/// [`MAX_PROBES`].
pub fn candidates(peers: &[PeerSeen], all: bool, me: Option<Ipv4Addr>) -> Vec<PeerSeen> {
    let mut out: Vec<PeerSeen> = Vec::new();
    for p in peers {
        if !p.online || Some(p.ip) == me || out.iter().any(|o| o.ip == p.ip) || os_is_not_ps5(&p.os)
        {
            continue;
        }
        if all || is_console_name(&p.name) {
            out.push(p.clone());
        }
    }
    // Console names first when everything is asked for.
    out.sort_by_key(|p| !is_console_name(&p.name));
    out.truncate(MAX_PROBES);
    out
}

/// The name to show for a console: its tailnet name without the app's suffix.
pub fn display_name(tailnet_name: &str) -> String {
    let label = tailnet_name
        .trim_end_matches('.')
        .split('.')
        .next()
        .unwrap_or("");
    let base = match label.rsplit_once('-') {
        Some((head, n)) if n.bytes().all(|b| b.is_ascii_digit()) && head.ends_with(SUFFIX) => head,
        _ => label,
    };
    base.strip_suffix(SUFFIX).unwrap_or(base).to_string()
}

/// A typed address: a tailnet IPv4, a peer's name (its first label, with or without the suffix,
/// or the whole MagicDNS name), or a name the system resolves to a tailnet address (MagicDNS
/// through the system app).
pub fn resolve(addr: &str, peers: &[PeerSeen]) -> Option<Ipv4Addr> {
    let a = addr.trim().trim_end_matches('.');
    if a.is_empty() {
        return None;
    }
    if let Ok(ip) = a.parse::<Ipv4Addr>() {
        return is_tailscale_ip(&IpAddr::V4(ip)).then_some(ip);
    }
    let first = a.split('.').next().unwrap_or("").to_ascii_lowercase();
    let by_name = peers.iter().find(|p| {
        let n = p.name.to_ascii_lowercase();
        n == first || display_name(&n) == first
    });
    if let Some(p) = by_name {
        return Some(p.ip);
    }
    (a, HELPER_PORT)
        .to_socket_addrs()
        .ok()?
        .find_map(|s| match s {
            SocketAddr::V4(v4) if is_tailscale_ip(&IpAddr::V4(*v4.ip())) => Some(*v4.ip()),
            _ => None,
        })
}

/// What a probe found at one tailnet address (the engine-side half of a list entry).
#[derive(Clone, Debug, Default)]
pub struct Probe {
    pub reachable: bool,
    pub via: Option<&'static str>,
    pub key: Option<[u8; 32]>,
    /// The name the AVA1 server gave (the PS5's helper says "PS5").
    pub name: Option<String>,
    pub error: Option<String>,
}

/// Connects to `ip`'s helper through this device's tailnet way and reads its key.
pub async fn probe(ip: Ipv4Addr) -> Probe {
    let dial = tokio::task::spawn_blocking(move || engine().tailnet_dial(ip, HELPER_PORT))
        .await
        .ok()
        .flatten();
    let Some((at, via)) = dial else {
        return Probe {
            error: Some("this device is not on Tailscale".into()),
            ..Default::default()
        };
    };
    probe_at(&at.to_string(), via).await
}

/// [`probe`] at an address to dial as it is (tests use a fake helper on loopback).
pub async fn probe_at(dial: &str, via: &'static str) -> Probe {
    match ps5upload_ava1::pool().peek_console(dial, PROBE_LIMIT).await {
        Ok(p) => Probe {
            reachable: true,
            via: Some(via),
            key: Some(p.key),
            name: Some(p.name),
            error: None,
        },
        Err(e) => Probe {
            reachable: false,
            via: Some(via),
            key: None,
            name: None,
            error: Some(probe_error(&e)),
        },
    }
}

fn probe_error(e: &ava1::Ava1Error) -> String {
    match e {
        ava1::Ava1Error::Timeout => {
            "no answer on port 9120: the helper is not running, or this PS5 \
            lets only listed devices in over Tailscale"
                .into()
        }
        other => format!("{other}"),
    }
}

/// One list entry: the probe and what the engine knows of the console.
pub fn entry(peer: &PeerSeen, p: &Probe) -> Value {
    let e = engine();
    let ip = peer.ip;
    let ip_s = ip.to_string();
    // Every address this console is known at: remembered with this tailnet address, and the
    // AVA1 pins of its key.
    let mut hosts: Vec<String> = e.known_hosts_for(ip);
    if let Some(k) = &p.key {
        for h in ps5upload_ava1::pool().hosts_pinned_to(k) {
            if !hosts.contains(&h) {
                hosts.push(h);
            }
        }
    }
    let lan_host = home_of(&hosts, &ip_s);
    let paired = p.key.is_some_and(|k| ps5upload_ava1::pool().knows_key(&k));
    let name = if peer.name.is_empty() {
        e.known_name_for(ip)
    } else {
        peer.name.clone()
    };
    let listed = crate::consoles_api::tailnet_listed(&ip_s, lan_host.as_deref(), &hosts);
    json!({
        "name": display_name(&name),
        "tailnet_name": name,
        "tailnet_ip": ip_s,
        "lan_host": lan_host,
        "id": p.key.map(|k| ava1::hex::encode(&k)),
        "reachable": p.reachable,
        "via": p.via,
        "online": peer.online,
        "paired": paired,
        "hosts": hosts,
        "listed": listed,
        "error": p.error,
        "kind": peer_kind(&peer.os, p),
        "os": peer.os,
    })
}

/// The home address among the addresses a console is known at: one that is not a tailnet one
/// (nor its own tailnet address).
pub fn home_of(hosts: &[String], tailnet_ip: &str) -> Option<String> {
    hosts
        .iter()
        .find(|h| {
            let bare = ps5upload_core::events::console_host(h);
            bare != tailnet_ip && !bare.parse::<IpAddr>().is_ok_and(|ip| is_tailscale_ip(&ip))
        })
        .cloned()
}

struct Cached {
    at: Instant,
    backend: &'static str,
    can_list: bool,
    consoles: Vec<Value>,
}

static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

fn lock_cache() -> std::sync::MutexGuard<'static, Option<Cached>> {
    CACHE.lock().unwrap_or_else(|p| p.into_inner())
}

fn backend_name() -> &'static str {
    match engine().backend() {
        crate::tailnet_engine::Backend::Builtin => "builtin",
        crate::tailnet_engine::Backend::System => "system",
        crate::tailnet_engine::Backend::None => "none",
    }
}

/// Probes these peers at once.
async fn probe_all(peers: Vec<PeerSeen>) -> Vec<Value> {
    let mut set = tokio::task::JoinSet::new();
    for (i, peer) in peers.into_iter().enumerate() {
        set.spawn(async move {
            let p = probe(peer.ip).await;
            (i, peer, p)
        });
    }
    let mut found = Vec::new();
    while let Some(r) = set.join_next().await {
        if let Ok(x) = r {
            found.push(x);
        }
    }
    found.sort_by_key(|(i, _, _)| *i);
    let rows = tokio::task::spawn_blocking(move || {
        found
            .iter()
            .map(|(_, peer, p)| entry(peer, p))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    // Consoles that answered first.
    let mut rows = rows;
    rows.sort_by_key(|r| r["reachable"] != true);
    rows
}

#[derive(Deserialize)]
pub(crate) struct ConsolesQuery {
    /// One address to probe: a tailnet IPv4 or a MagicDNS name.
    #[serde(default, alias = "host", alias = "console")]
    addr: Option<String>,
    /// Look again instead of answering from the last minute's list.
    #[serde(default)]
    refresh: Option<String>,
    /// Probe every online peer, not only the ones named like a console.
    #[serde(default)]
    all: Option<String>,
}

fn truthy(v: &Option<String>) -> bool {
    matches!(v.as_deref(), Some("1" | "true" | "yes"))
}

/// One typed address, probed: the same entry a list has, or why not.
async fn probe_one(addr: &str) -> Result<Value, (StatusCode, Value)> {
    let addr = addr.to_string();
    let found = tokio::task::spawn_blocking(move || {
        let (peers, _) = engine().peers_seen();
        resolve(&addr, &peers).map(|ip| {
            peers.into_iter().find(|p| p.ip == ip).unwrap_or(PeerSeen {
                name: String::new(),
                ip,
                online: true,
                source: "typed",
                os: String::new(),
            })
        })
    })
    .await
    .ok()
    .flatten();
    let Some(peer) = found else {
        return Err((
            StatusCode::BAD_REQUEST,
            json!({
                "error": "not a Tailscale address: type the PS5's 100.x address or its name in Tailscale",
                "reason": "not_tailnet",
            }),
        ));
    };
    let p = probe(peer.ip).await;
    let row = tokio::task::spawn_blocking(move || entry(&peer, &p))
        .await
        .unwrap_or(Value::Null);
    // Kept with the list, so a console found by address shows with the others.
    if let Some(c) = lock_cache().as_mut() {
        c.consoles.retain(|r| r["tailnet_ip"] != row["tailnet_ip"]);
        c.consoles.push(row.clone());
    }
    Ok(row)
}

/// `GET /api/tailnet/consoles[?refresh=1&all=1&addr=]` — the PS5Upload consoles on this device's
/// Tailscale network: `{ok, backend, can_list, cached, age_ms, consoles: [{name, tailnet_name,
/// tailnet_ip, lan_host, id, reachable, via, online, paired, hosts, listed, error, kind, os}]}`.
///
/// * `kind` ([`peer_kind`]): `ps5` when its helper answered as the PS5 (or its Tailscale says
///   its OS is the PS5's), `device` for a phone or computer, `unknown` otherwise. Only `ps5` is
///   offered as a console: every PS5Upload device's node is named `…-ps5u`. `os`: what its
///   Tailscale reports, when this device's list carries it.
///
/// * `backend`: how this device is on Tailscale (`builtin`, `system`, `none`). `can_list`: whether
///   a peer list could be read (false on a phone with only the system Tailscale app: type the
///   address instead, `addr=`).
/// * Each console was asked on TCP 9120 through Tailscale: `reachable` when its helper answered,
///   `id` its AVA1 key (hex, the same at every address), `via` `builtin` or `system`, `error`
///   why not. Nothing is paired or pinned by asking, and no code shows on the PS5.
/// * `lan_host`: its home address when this engine knows one; `hosts` every address it was met
///   at; `listed`: the console list's entry for it (`{id, name, host}`) or null; `paired`: this
///   engine is trusted by it already.
/// * `addr=` probes that one address now (a 100.x address or a MagicDNS name) and answers
///   `{ok, console: {...}}`; 400 `not_tailnet` when it is neither.
///
/// Answered from the last minute's look unless `refresh=1`; nothing is looked for in the
/// background.
pub(crate) async fn consoles(Query(q): Query<ConsolesQuery>) -> Response {
    if let Some(addr) = q.addr.as_deref().filter(|a| !a.trim().is_empty()) {
        return match probe_one(addr).await {
            Ok(row) => Json(json!({ "ok": true, "console": row })).into_response(),
            Err((code, body)) => (code, Json(body)).into_response(),
        };
    }
    if !truthy(&q.refresh) && !truthy(&q.all) {
        if let Some(c) = lock_cache().as_ref() {
            if c.at.elapsed() < CACHE_TTL {
                return Json(json!({
                    "ok": true,
                    "backend": c.backend,
                    "can_list": c.can_list,
                    "cached": true,
                    "age_ms": c.at.elapsed().as_millis() as u64,
                    "consoles": c.consoles,
                }))
                .into_response();
            }
        }
    }
    let all = truthy(&q.all);
    let looked = tokio::task::spawn_blocking(move || {
        let e = engine();
        let (peers, can_list) = e.peers_seen();
        (
            candidates(&peers, all, e.self_ip()),
            can_list,
            backend_name(),
        )
    })
    .await;
    let Ok((peers, can_list, backend)) = looked else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "the look failed" })),
        )
            .into_response();
    };
    let rows = probe_all(peers).await;
    *lock_cache() = Some(Cached {
        at: Instant::now(),
        backend,
        can_list,
        consoles: rows.clone(),
    });
    crate::log_at!(
        Info,
        "tailnet",
        "tailnet: looked for consoles: {} found, {} answer",
        rows.len(),
        rows.iter().filter(|r| r["reachable"] == true).count()
    );
    Json(json!({
        "ok": true,
        "backend": backend,
        "can_list": can_list,
        "cached": false,
        "age_ms": 0,
        "consoles": rows,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct AddBody {
    /// The console's tailnet address or MagicDNS name (as found, or typed).
    #[serde(alias = "tailnet_ip", alias = "host")]
    addr: String,
    /// A name for a new entry; its tailnet name otherwise.
    #[serde(default)]
    name: String,
}

/// `POST /api/tailnet/consoles/add` `{addr, name?}` — puts a console found on Tailscale (or typed:
/// a 100.x address or a MagicDNS name) on the console list, probing it first. Answers `{ok,
/// action, console: {id, name, host}, found: {...the list entry...}}`; `action` is `added`,
/// `linked` (its entry at the tailnet address moved to its home address), `merged` (two entries
/// for it became one) or `exists`.
///
/// One entry per console: at its home address when one is known (so the home network is used
/// when back home, Tailscale when away), else at its tailnet address, which becomes the home
/// one when the console is next reached at home. Either way its tailnet address is remembered
/// for the router. A console that does not answer is still added (its list may not let this
/// device in yet: the PS5 then shows "tried to reach this PS5" with Allow). A peer known to be a
/// phone or a computer ([`peer_kind`] `device`) is refused: 400 `not_a_console`.
pub(crate) async fn add(Json(req): Json<AddBody>) -> Response {
    let row = match probe_one(&req.addr).await {
        Ok(row) => row,
        Err((code, body)) => return (code, Json(body)).into_response(),
    };
    // A phone or a computer on the tailnet (the app runs there too) is never put on the console
    // list.
    if row["kind"] == "device" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "this is not a PS5: it is a phone or computer on your Tailscale network",
                "reason": "not_a_console",
                "found": row,
            })),
        )
            .into_response();
    }
    let name = req.name.trim().to_string();
    let r = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let ip_s = row["tailnet_ip"].as_str().unwrap_or("").to_string();
        let ip: Ipv4Addr = ip_s.parse().map_err(|_| "no tailnet address".to_string())?;
        let lan = row["lan_host"].as_str().map(str::to_string);
        let hosts: Vec<String> = row["hosts"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|h| h.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let show = if name.is_empty() {
            row["name"].as_str().unwrap_or("").to_string()
        } else {
            name
        };
        let (action, console) =
            crate::consoles_api::tailnet_upsert(&show, &ip_s, lan.as_deref(), &hosts)?;
        let host = console["host"].as_str().unwrap_or(&ip_s).to_string();
        engine().remember(&host, ip, row["tailnet_name"].as_str().unwrap_or(""));
        if let Some(c) = lock_cache().as_mut() {
            for r in c
                .consoles
                .iter_mut()
                .filter(|r| r["tailnet_ip"] == ip_s.as_str())
            {
                r["listed"] = console.clone();
            }
        }
        crate::log_at!(
            Info,
            "tailnet",
            "tailnet: console {ip_s} {action} as {host}"
        );
        let mut found = row;
        found["listed"] = console.clone();
        Ok(json!({ "ok": true, "action": action, "console": console, "found": found }))
    })
    .await;
    match r {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e, "reason": "save_failed" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(name: &str, ip: [u8; 4], online: bool) -> PeerSeen {
        PeerSeen {
            name: name.into(),
            ip: Ipv4Addr::from(ip),
            online,
            source: "builtin",
            os: String::new(),
        }
    }

    fn with_os(mut p: PeerSeen, os: &str) -> PeerSeen {
        p.os = os.into();
        p.source = "system";
        p
    }

    fn answered(name: &str) -> Probe {
        Probe {
            reachable: true,
            via: Some("builtin"),
            key: Some([7; 32]),
            name: Some(name.into()),
            error: None,
        }
    }

    fn silent() -> Probe {
        Probe {
            reachable: false,
            via: Some("builtin"),
            key: None,
            name: None,
            error: Some("no answer on port 9120".into()),
        }
    }

    #[test]
    fn a_phone_or_a_computer_named_like_a_console_is_a_device_not_a_ps5() {
        // The PS5: its helper answers AVA1 on 9120 as "PS5", or its Tailscale says "ps5".
        assert_eq!(peer_kind("", &answered("PS5")), "ps5");
        assert_eq!(peer_kind("ps5", &answered("PS5")), "ps5");
        assert_eq!(peer_kind("ps5", &silent()), "ps5");
        // A phone running PS5Upload: its node is "pixel-9-ps5u", nothing answers on 9120. With
        // the system app's list its OS says so; with the built-in node nothing is known.
        assert!(is_console_name("pixel-9-ps5u"));
        assert_eq!(peer_kind("android", &silent()), "device");
        assert_eq!(peer_kind("iOS", &silent()), "device");
        assert_eq!(peer_kind("", &silent()), "unknown");
        // A computer: the same.
        assert_eq!(peer_kind("macOS", &silent()), "device");
        assert_eq!(peer_kind("windows", &silent()), "device");
        // Something other than the helper answering AVA1, or an OS that is not the PS5's.
        assert_eq!(peer_kind("", &answered("Pixel 9")), "device");
        assert_eq!(peer_kind("linux", &answered("PS5")), "device");
        assert!(!os_is_not_ps5(""));
        assert!(!os_is_not_ps5("PS5"));
        assert!(os_is_not_ps5("android"));
    }

    #[test]
    fn a_peer_whose_tailscale_says_phone_or_computer_is_never_probed() {
        let peers = vec![
            with_os(peer("den-ps5u", [100, 64, 0, 10], true), "ps5"),
            with_os(peer("pixel-9-ps5u", [100, 64, 0, 11], true), "android"),
            with_os(peer("yunpengs-mbp-ps5u", [100, 64, 0, 12], true), "macOS"),
            // The built-in node has no OS for its peers: probed, and the probe decides.
            peer("galaxy-s24-ps5u", [100, 64, 0, 13], true),
        ];
        let ips: Vec<String> = candidates(&peers, false, None)
            .iter()
            .map(|p| p.ip.to_string())
            .collect();
        assert_eq!(ips, vec!["100.64.0.10", "100.64.0.13"]);
        assert_eq!(candidates(&peers, true, None).len(), 2);
        // Its entry says what it is.
        let row = entry(&peers[3], &silent());
        assert_eq!(row["kind"], "unknown");
        let row = entry(&peers[0], &silent());
        assert_eq!(row["kind"], "ps5");
        assert_eq!(row["os"], "ps5");
    }

    #[test]
    fn console_names_are_the_apps_own() {
        assert!(is_console_name("yunpengs-ps5pro-ps5u"));
        assert!(is_console_name("yunpengs-ps5fat-ps5u.tail1234.ts.net."));
        assert!(is_console_name("den-ps5u-1"));
        assert!(is_console_name("DEN-PS5U"));
        assert!(!is_console_name("-ps5u"));
        assert!(!is_console_name("ps5u"));
        assert!(!is_console_name("den-ps5u-x"));
        assert!(!is_console_name("living-room-ps5"));
        assert!(!is_console_name("pixel-9"));
        assert!(!is_console_name(""));
        assert_eq!(display_name("yunpengs-ps5pro-ps5u"), "yunpengs-ps5pro");
        assert_eq!(display_name("den-ps5u-2.tail1.ts.net."), "den");
        assert_eq!(display_name("pixel-9"), "pixel-9");
    }

    #[test]
    fn candidates_are_online_console_peers_once_and_never_this_device() {
        let me = Ipv4Addr::new(100, 64, 0, 2);
        let peers = vec![
            peer("yunpengs-ps5pro-ps5u", [100, 64, 0, 10], true),
            peer("yunpengs-ps5fat-ps5u", [100, 64, 0, 11], true),
            peer("old-ps5u", [100, 64, 0, 12], false),
            peer("pixel-9", [100, 64, 0, 13], true),
            // This device (its own name carries the suffix too).
            peer("yunpengs-mac-ps5u", [100, 64, 0, 2], true),
            // The same peer from the system app as well.
            peer("yunpengs-ps5pro-ps5u", [100, 64, 0, 10], true),
        ];
        let c = candidates(&peers, false, Some(me));
        let ips: Vec<String> = c.iter().map(|p| p.ip.to_string()).collect();
        assert_eq!(ips, vec!["100.64.0.10", "100.64.0.11"]);
        // all: every online peer, console names first.
        let c = candidates(&peers, true, Some(me));
        let ips: Vec<String> = c.iter().map(|p| p.ip.to_string()).collect();
        assert_eq!(ips, vec!["100.64.0.10", "100.64.0.11", "100.64.0.13"]);
        // A big tailnet: capped.
        let many: Vec<PeerSeen> = (0..40)
            .map(|i| peer(&format!("p{i}-ps5u"), [100, 64, 1, i as u8], true))
            .collect();
        assert_eq!(candidates(&many, false, None).len(), MAX_PROBES);
    }

    #[test]
    fn typed_addresses_resolve_to_tailnet_ones() {
        let peers = vec![peer("den-ps5u", [100, 64, 0, 10], true)];
        assert_eq!(
            resolve("100.64.0.9", &peers),
            Some(Ipv4Addr::new(100, 64, 0, 9))
        );
        assert_eq!(
            resolve(" 100.101.2.3 ", &peers),
            Some(Ipv4Addr::new(100, 101, 2, 3))
        );
        // Not a tailnet address.
        assert_eq!(resolve("192.168.1.20", &peers), None);
        assert_eq!(resolve("100.128.0.1", &peers), None);
        // By name: the label, without the suffix, or the whole MagicDNS name.
        assert_eq!(
            resolve("den-ps5u", &peers),
            Some(Ipv4Addr::new(100, 64, 0, 10))
        );
        assert_eq!(resolve("DEN", &peers), Some(Ipv4Addr::new(100, 64, 0, 10)));
        assert_eq!(
            resolve("den-ps5u.tail1234.ts.net.", &peers),
            Some(Ipv4Addr::new(100, 64, 0, 10))
        );
        assert_eq!(resolve("", &peers), None);
        // "localhost" resolves, but not to a tailnet address.
        assert_eq!(resolve("localhost", &peers), None);
    }

    #[test]
    fn the_home_address_is_the_one_that_is_not_a_tailnet_one() {
        let ts = "100.64.0.9";
        assert_eq!(
            home_of(&["100.64.0.9".into(), "192.168.1.20".into()], ts).as_deref(),
            Some("192.168.1.20")
        );
        assert_eq!(home_of(&["100.64.0.9".into()], ts), None);
        assert_eq!(
            home_of(&["100.64.0.9:9021".into(), "100.70.1.1".into()], ts),
            None
        );
        assert_eq!(home_of(&[], ts), None);
    }

    /// A fake helper on loopback: the AVA1 server side, which records whether it ever got far
    /// enough to admit this device.
    async fn fake_helper() -> (
        String,
        [u8; 32],
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let id = Arc::new(ava1::keys::Identity::generate().unwrap());
        let key = id.public();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let admitted = Arc::new(AtomicBool::new(false));
        let a2 = admitted.clone();
        tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let (rh, wh) = s.into_split();
            let mut r = ava1::conn::FrameReader::new(rh);
            let mut w = ava1::conn::FrameWriter::new(wh);
            let hs1 = r.recv().await.unwrap();
            let _ = ava1::handshake::server(
                &mut r,
                &mut w,
                hs1,
                &id,
                "PS5",
                |_| {
                    a2.store(true, Ordering::SeqCst);
                    ava1::handshake::Admission::Known
                },
                ava1::gen::CAP_DATA_PLANE,
            )
            .await;
        });
        (addr, key, admitted)
    }

    #[tokio::test]
    async fn the_probe_reads_the_helpers_key_through_the_tailnet_way() {
        let (addr, key, admitted) = fake_helper().await;
        let p = probe_at(&addr, "builtin").await;
        assert!(p.reachable, "{:?}", p.error);
        assert_eq!(p.key, Some(key));
        assert_eq!(p.via, Some("builtin"));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !admitted.load(std::sync::atomic::Ordering::SeqCst),
            "asking never admits (no pairing, no device listed, no code on screen)"
        );
        let row = entry(&peer("den-ps5u", [100, 64, 0, 77], true), &p);
        assert_eq!(row["name"], "den");
        assert_eq!(row["tailnet_ip"], "100.64.0.77");
        assert_eq!(row["reachable"], true);
        assert_eq!(row["id"], ava1::hex::encode(&key));
        assert_eq!(row["via"], "builtin");
        assert_eq!(row["kind"], "ps5", "the helper answered as the PS5");

        // Nothing there: not reachable, with a reason.
        let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = dead.local_addr().unwrap().to_string();
        drop(dead);
        let p = probe_at(&dead_addr, "system").await;
        assert!(!p.reachable && p.key.is_none() && p.error.is_some());
        let row = entry(&peer("x-ps5u", [100, 64, 0, 78], true), &p);
        assert_eq!(row["reachable"], false);
        assert!(row["id"].is_null());
        assert_eq!(row["kind"], "unknown");
    }
}
