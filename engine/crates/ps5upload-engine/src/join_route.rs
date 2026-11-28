//! Play Together routes: which way a guest's connection goes and how good it is
//! (apps/play/docs/ROUTES.md).
//!
//! **The tiers.** One reading (round trip, jitter, loss, headroom over the stream's bitrate, and
//! whether the guest fell behind lately) is Excellent, Good, Playable or Poor by fixed lines
//! ([`LIMITS`]); a reading inside every line of a tier is in it, the worst part decides. The same
//! lines as PS5Upload Play's (`apps/play/src/net/route.cpp`) and the guest page's
//! (`client/src/join/routeTier.ts`): a test here and there pins the same table.
//!
//! **The checks socket** (`GET /api/join/probe`, reached from the LAN and Tailscale like the guest
//! socket): a guest's app measures each way to this engine before it joins and, gently, the ways
//! it is not using while it plays, without joining on them. Text frames only from the guest:
//! * `{"p": n}` → `{"p": n, "us": <engine µs>}` at once (n: a whole number the guest picks);
//! * `{"burst": bytes}` → that many bytes (at most [`BURST_MAX`]) as binary messages of 16 KiB,
//!   then `{"burst_done": bytes}`: how fast the way carries, at connect only (a socket may ask
//!   [`BURSTS`] times within its first [`BURST_WINDOW`]; later asks get `{"burst_done": 0}`).
//!
//! Only while a Play Together is open here (else HTTP 409), [`PER_IP`] sockets at once from one
//! address, [`CONNECTS_PER_MIN`] a minute, [`MSGS_PER_S`] messages a second (past it the socket
//! is closed), 256 bytes a message, closed after [`IDLE`] without a message. Nothing about the
//! guest is kept: only counts for the diagnosis ([`counters_json`]).
//!
//! **The guest's report.** An app or page that chose its way says so in its `{"stats": …}`
//! (`route`, `tier`, `route_rtt_ms`, `route_jitter_ms`, `route_loss`, `route_switches`,
//! `backups`, `backup_tier`; join.rs `PageReport`), and the host's panel shows its words; for a
//! guest that does not, the engine's own reading ([`engine_tier`]).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::ConnectInfo;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// A tier, better is larger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Tier {
    Down,
    Poor,
    Playable,
    Good,
    Excellent,
}

impl Tier {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Tier::Down => "down",
            Tier::Poor => "poor",
            Tier::Playable => "playable",
            Tier::Good => "good",
            Tier::Excellent => "excellent",
        }
    }

    /// A tier word a guest said (anything else: None).
    pub(crate) fn from_word(w: &str) -> Option<Tier> {
        [
            Tier::Down,
            Tier::Poor,
            Tier::Playable,
            Tier::Good,
            Tier::Excellent,
        ]
        .into_iter()
        .find(|t| t.word() == w)
    }
}

/// The lines of one tier: round trip (ms), jitter (ms), loss (per thousand) at most, headroom
/// (percent of the stream's bitrate the way carries) at least.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub(crate) struct Limits {
    pub tier: &'static str,
    pub rtt_ms: u32,
    pub jitter_ms: u32,
    pub loss_permille: u32,
    pub headroom_pct: u32,
}

/// Excellent, Good, Playable (docs/ROUTES.md has where each number comes from).
pub(crate) const LIMITS: [Limits; 3] = [
    Limits {
        tier: "excellent",
        rtt_ms: 20,
        jitter_ms: 5,
        loss_permille: 1,
        headroom_pct: 150,
    },
    Limits {
        tier: "good",
        rtt_ms: 40,
        jitter_ms: 15,
        loss_permille: 5,
        headroom_pct: 125,
    },
    Limits {
        tier: "playable",
        rtt_ms: 80,
        jitter_ms: 30,
        loss_permille: 10,
        headroom_pct: 100,
    },
];

/// One reading; None: not known (never lowers the tier).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Reading {
    pub rtt_ms: Option<u32>,
    pub jitter_ms: Option<u32>,
    pub loss_permille: Option<u32>,
    pub headroom_pct: Option<u32>,
    /// Times the guest fell behind lately (the stream skipped it to the next keyframe).
    pub behind: u32,
}

fn within(r: &Reading, l: &Limits) -> bool {
    r.rtt_ms.is_none_or(|x| x <= l.rtt_ms)
        && r.jitter_ms.is_none_or(|x| x <= l.jitter_ms)
        && r.loss_permille.is_none_or(|x| x <= l.loss_permille)
        && r.headroom_pct.is_none_or(|x| x >= l.headroom_pct)
}

/// The tier of a reading of a connection that answers.
pub(crate) fn classify(r: &Reading) -> Tier {
    let mut t = if within(r, &LIMITS[0]) {
        Tier::Excellent
    } else if within(r, &LIMITS[1]) {
        Tier::Good
    } else if within(r, &LIMITS[2]) {
        Tier::Playable
    } else {
        Tier::Poor
    };
    if r.behind >= 3 {
        t = t.min(Tier::Poor);
    } else if r.behind >= 1 {
        t = t.min(Tier::Playable);
    }
    t
}

/// The engine's own reading of a guest that said none: its round trip, whether it falls behind
/// (`behind`: pictures were skipped in the last seconds) and how its page says the picture goes
/// ("smooth", "stutters", "poor"). None before a round trip was measured.
pub(crate) fn engine_tier(
    rtt_ms: Option<u32>,
    behind: bool,
    quality: Option<&str>,
) -> Option<Tier> {
    let rtt = rtt_ms.filter(|r| *r > 0)?;
    let behind = match (behind, quality) {
        (_, Some("poor")) => 3,
        (true, _) | (_, Some("stutters")) => 1,
        _ => 0,
    };
    Some(classify(&Reading {
        rtt_ms: Some(rtt),
        behind,
        ..Reading::default()
    }))
}

/// A way a guest said it uses ("lan", "tailscale", "internet").
pub(crate) fn route_word(w: &str) -> Option<&'static str> {
    ["lan", "tailscale", "internet"]
        .into_iter()
        .find(|x| *x == w)
}

// ── The checks socket ──────────────────────────────────────────────────────────────────────────

/// Sockets at once from one address (a guest checks two or three ways; Tailscale ones all come
/// from its console).
pub(crate) const PER_IP: usize = 4;
pub(crate) const CONNECTS_PER_MIN: usize = 30;
pub(crate) const MSGS_PER_S: u32 = 10;
pub(crate) const IDLE: Duration = Duration::from_secs(30);
/// A check socket lives at most this long (a long game's backups open new ones).
pub(crate) const LIFETIME: Duration = Duration::from_secs(6 * 3600);
pub(crate) const BURST_MAX: usize = 512 * 1024;
pub(crate) const BURSTS: u32 = 2;
pub(crate) const BURST_WINDOW: Duration = Duration::from_secs(30);
const MSG_MAX: usize = 256;
const PIECE: usize = 16 * 1024;

static CONNECTS: crate::join::ConnectLimit = crate::join::ConnectLimit::new();
static OPEN: Mutex<Option<HashMap<IpAddr, usize>>> = Mutex::new(None);

#[derive(Default)]
struct Counters {
    sockets: AtomicU64,
    refused: AtomicU64,
    checks: AtomicU64,
    bursts: AtomicU64,
    burst_bytes: AtomicU64,
    closed_rate: AtomicU64,
}
static COUNTERS: Counters = Counters {
    sockets: AtomicU64::new(0),
    refused: AtomicU64::new(0),
    checks: AtomicU64::new(0),
    bursts: AtomicU64::new(0),
    burst_bytes: AtomicU64::new(0),
    closed_rate: AtomicU64::new(0),
};

/// The checks socket's counts since the engine started (no addresses, nothing per guest).
pub(crate) fn counters_json() -> serde_json::Value {
    let c = &COUNTERS;
    let open: usize = OPEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map_or(0, |m| m.values().sum());
    serde_json::json!({
        "open": open,
        "sockets": c.sockets.load(Ordering::Relaxed),
        "refused": c.refused.load(Ordering::Relaxed),
        "checks": c.checks.load(Ordering::Relaxed),
        "bursts": c.bursts.load(Ordering::Relaxed),
        "burst_bytes": c.burst_bytes.load(Ordering::Relaxed),
        "closed_too_fast": c.closed_rate.load(Ordering::Relaxed),
    })
}

/// One socket from `ip` counted while it lives.
struct Slot(IpAddr);

impl Slot {
    fn take(ip: IpAddr) -> Option<Slot> {
        let key = crate::join::limit_key(ip);
        let mut g = OPEN.lock().unwrap_or_else(|e| e.into_inner());
        let n = g.get_or_insert_with(HashMap::new).entry(key).or_default();
        if *n >= PER_IP {
            return None;
        }
        *n += 1;
        Some(Slot(key))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut g = OPEN.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = g.as_mut() {
            if let Some(n) = m.get_mut(&self.0) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    m.remove(&self.0);
                }
            }
        }
    }
}

/// `GET /api/join/probe`: the checks socket (above).
pub async fn probe(
    upgrade: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
) -> Response {
    if let Some(r) = crate::public_limit::PROBE.gate(peer.ip()) {
        return r;
    }
    probe_with(upgrade, peer.ip(), crate::join::together_open()).await
}

pub(crate) async fn probe_with(upgrade: WebSocketUpgrade, ip: IpAddr, open: bool) -> Response {
    if !open {
        COUNTERS.refused.fetch_add(1, Ordering::Relaxed);
        return (StatusCode::CONFLICT, "Play Together is not open").into_response();
    }
    let limit = if ip.is_loopback() {
        CONNECTS_PER_MIN * 10
    } else {
        CONNECTS_PER_MIN
    };
    if !CONNECTS.allow(ip, limit, Instant::now()) {
        COUNTERS.refused.fetch_add(1, Ordering::Relaxed);
        return crate::public_limit::slow_down(20);
    }
    let Some(slot) = Slot::take(ip) else {
        COUNTERS.refused.fetch_add(1, Ordering::Relaxed);
        return (StatusCode::TOO_MANY_REQUESTS, "too many check sockets").into_response();
    };
    COUNTERS.sockets.fetch_add(1, Ordering::Relaxed);
    upgrade
        .max_message_size(MSG_MAX)
        .max_frame_size(MSG_MAX)
        .on_upgrade(move |s| async move {
            session(s).await;
            drop(slot);
        })
}

#[derive(serde::Deserialize)]
struct In {
    #[serde(default)]
    p: Option<u64>,
    #[serde(default)]
    burst: Option<u64>,
}

/// Messages a second: a bucket of [`MSGS_PER_S`] filled at that rate.
struct Rate {
    tokens: f64,
    at: Instant,
}

impl Rate {
    fn take(&mut self, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.at).as_secs_f64();
        self.at = now;
        self.tokens = (self.tokens + dt * f64::from(MSGS_PER_S)).min(f64::from(MSGS_PER_S));
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

async fn close(s: &mut WebSocket, code: u16, why: &'static str) {
    let _ = s
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: why.into(),
        })))
        .await;
}

async fn session(mut s: WebSocket) {
    let start = Instant::now();
    let mut rate = Rate {
        tokens: f64::from(MSGS_PER_S),
        at: start,
    };
    let mut bursts = 0u32;
    loop {
        let left = LIFETIME.saturating_sub(start.elapsed());
        if left.is_zero() {
            close(&mut s, 1000, "lifetime").await;
            return;
        }
        let m = match tokio::time::timeout(IDLE.min(left), s.recv()).await {
            Err(_) => {
                close(&mut s, 1000, "idle").await;
                return;
            }
            Ok(None) | Ok(Some(Err(_))) => return,
            Ok(Some(Ok(m))) => m,
        };
        let t = match m {
            Message::Text(t) => t,
            Message::Close(_) => return,
            // Pings are answered by the socket itself; anything binary is not ours.
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Binary(_) => {
                close(&mut s, 1003, "text only").await;
                return;
            }
        };
        if !rate.take(Instant::now()) {
            COUNTERS.closed_rate.fetch_add(1, Ordering::Relaxed);
            close(&mut s, 1008, "too fast").await;
            return;
        }
        let Ok(m) = serde_json::from_str::<In>(t.as_str()) else {
            continue;
        };
        if let Some(p) = m.p {
            COUNTERS.checks.fetch_add(1, Ordering::Relaxed);
            let a = serde_json::json!({"p": p, "us": crate::rp_stream_api::engine_us()});
            if s.send(Message::Text(a.to_string().into())).await.is_err() {
                return;
            }
        }
        if let Some(want) = m.burst {
            let ok = bursts < BURSTS && start.elapsed() < BURST_WINDOW;
            let n = if ok {
                (want as usize).min(BURST_MAX)
            } else {
                0
            };
            if ok {
                bursts += 1;
                COUNTERS.bursts.fetch_add(1, Ordering::Relaxed);
                COUNTERS.burst_bytes.fetch_add(n as u64, Ordering::Relaxed);
            }
            let piece = vec![0u8; PIECE];
            let mut sent = 0;
            while sent < n {
                let k = PIECE.min(n - sent);
                if s.send(Message::Binary(piece[..k].to_vec().into()))
                    .await
                    .is_err()
                {
                    return;
                }
                sent += k;
            }
            let done = serde_json::json!({ "burst_done": n });
            if s.send(Message::Text(done.to_string().into()))
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

/// `GET /api/join/routes?addr=`: what the host's panel and an agent read to tell how each guest
/// is connected: the tiers' lines, each guest's way and tier (with what its app said: round trip,
/// jitter, loss, changes of way, backups), and the checks socket's counts.
pub(crate) fn routes_json(state: &serde_json::Value) -> serde_json::Value {
    let guests: Vec<serde_json::Value> = state["guests"]
        .as_array()
        .map(|g| {
            g.iter()
                .map(|g| {
                    let page = &g["page"];
                    serde_json::json!({
                        "player": g["player"],
                        "name": g["name"],
                        "state": g["state"],
                        "route": g["route"],
                        "tier": g["tier"],
                        "tier_from": g["tier_from"],
                        "rtt_ms": g["rtt_ms"],
                        "route_rtt_ms": page["route_rtt_ms"],
                        "route_jitter_ms": page["route_jitter_ms"],
                        "route_loss_permille": page["route_loss"],
                        "route_switches": page["route_switches"],
                        "backups": page["backups"],
                        "backup_tier": page["backup_tier"],
                        "client_kind": g["client_kind"],
                        "quality": g["quality"],
                        "behind": g["behind"],
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    serde_json::json!({
        "open": state["open"],
        "tiers": LIMITS,
        "guests": guests,
        "probe": counters_json(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as TMsg;

    fn r(rtt: u32) -> Reading {
        Reading {
            rtt_ms: Some(rtt),
            ..Reading::default()
        }
    }

    /// The same table as apps/play/tests/test_route.cpp test_tiers and
    /// client/src/join/routeTier.test.ts: the three must not drift apart.
    #[test]
    fn tiers_match_the_app_and_the_page() {
        let t = |x: Reading| classify(&x).word();
        let full = |rtt, jit, loss, head, behind| Reading {
            rtt_ms: Some(rtt),
            jitter_ms: jit,
            loss_permille: loss,
            headroom_pct: head,
            behind,
        };
        assert_eq!(t(full(2, Some(1), Some(0), Some(400), 0)), "excellent");
        assert_eq!(t(full(20, Some(5), Some(1), Some(150), 0)), "excellent");
        assert_eq!(t(r(21)), "good");
        assert_eq!(t(full(40, Some(15), Some(5), Some(125), 0)), "good");
        assert_eq!(t(r(41)), "playable");
        assert_eq!(t(full(80, Some(30), Some(10), Some(100), 0)), "playable");
        assert_eq!(t(r(81)), "poor");
        assert_eq!(t(r(225)), "poor");
        assert_eq!(t(full(3, Some(20), None, None, 0)), "playable");
        assert_eq!(t(full(3, None, Some(20), None, 0)), "poor");
        assert_eq!(t(full(3, None, Some(3), None, 0)), "good");
        assert_eq!(t(full(3, None, None, Some(110), 0)), "playable");
        assert_eq!(t(full(3, None, None, Some(90), 0)), "poor");
        assert_eq!(t(full(3, Some(1), Some(0), None, 1)), "playable");
        assert_eq!(t(full(3, Some(1), Some(0), None, 3)), "poor");
        assert_eq!(t(r(10)), "excellent");
    }

    #[test]
    fn engine_reading_of_a_guest_that_said_nothing() {
        assert_eq!(engine_tier(None, false, None), None);
        assert_eq!(engine_tier(Some(0), false, None), None);
        assert_eq!(
            engine_tier(Some(4), false, Some("smooth")),
            Some(Tier::Excellent)
        );
        assert_eq!(engine_tier(Some(4), true, None), Some(Tier::Playable));
        assert_eq!(
            engine_tier(Some(4), false, Some("stutters")),
            Some(Tier::Playable)
        );
        assert_eq!(engine_tier(Some(4), false, Some("poor")), Some(Tier::Poor));
        assert_eq!(engine_tier(Some(60), false, None), Some(Tier::Playable));
        assert_eq!(Tier::from_word("good"), Some(Tier::Good));
        assert_eq!(Tier::from_word("great"), None);
        assert_eq!(route_word("tailscale"), Some("tailscale"));
        assert_eq!(route_word("moon"), None);
    }

    async fn serve(open: bool, last: u8) -> String {
        use axum::routing::get;
        let app =
            axum::Router::new().route(
                "/api/join/probe",
                get(
                    move |u: WebSocketUpgrade,
                          ConnectInfo(p): ConnectInfo<std::net::SocketAddr>| async move {
                        // From the loopback but counted as a LAN address of its own per test.
                        let _ = p;
                        probe_with(u, IpAddr::from([192, 0, 2, last]), open).await
                    },
                ),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                l,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        format!("ws://{addr}/api/join/probe")
    }

    async fn next_text(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> serde_json::Value {
        loop {
            let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let TMsg::Text(t) = m {
                return serde_json::from_str(t.as_str()).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn checks_bursts_and_limits() {
        let url = serve(true, 1).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        ws.send(TMsg::Text(r#"{"p":7}"#.into())).await.unwrap();
        let a = next_text(&mut ws).await;
        assert_eq!(a["p"], 7);
        assert!(a["us"].as_u64().is_some());
        // A burst: binary pieces adding up, then the total.
        ws.send(TMsg::Text(r#"{"burst":100000}"#.into()))
            .await
            .unwrap();
        let mut got = 0;
        loop {
            match ws.next().await.unwrap().unwrap() {
                TMsg::Binary(b) => {
                    assert!(b.len() <= PIECE);
                    got += b.len();
                }
                TMsg::Text(t) => {
                    let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                    assert_eq!(v["burst_done"], 100000);
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(got, 100000);
        // Capped, and only BURSTS of them.
        ws.send(TMsg::Text(r#"{"burst":99999999}"#.into()))
            .await
            .unwrap();
        let mut got = 0;
        loop {
            match ws.next().await.unwrap().unwrap() {
                TMsg::Binary(b) => got += b.len(),
                TMsg::Text(_) => break,
                _ => {}
            }
        }
        assert_eq!(got, BURST_MAX);
        ws.send(TMsg::Text(r#"{"burst":1000}"#.into()))
            .await
            .unwrap();
        assert_eq!(next_text(&mut ws).await["burst_done"], 0);
        // Too many messages a second: closed.
        for i in 0..30 {
            if ws
                .send(TMsg::Text(format!(r#"{{"p":{i}}}"#).into()))
                .await
                .is_err()
            {
                break;
            }
        }
        let mut closed = false;
        for _ in 0..40 {
            match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
                Ok(Some(Ok(TMsg::Close(Some(f))))) => {
                    assert_eq!(u16::from(f.code), 1008);
                    closed = true;
                    break;
                }
                Ok(Some(Ok(_))) => continue,
                _ => {
                    closed = true;
                    break;
                }
            }
        }
        assert!(closed);
        assert!(counters_json()["closed_too_fast"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn refused_while_closed_and_past_the_sockets_per_address() {
        let url = serve(false, 2).await;
        assert!(tokio_tungstenite::connect_async(&url).await.is_err());
        let url = serve(true, 3).await;
        let mut keep = Vec::new();
        for _ in 0..PER_IP {
            keep.push(tokio_tungstenite::connect_async(&url).await.unwrap().0);
        }
        assert!(tokio_tungstenite::connect_async(&url).await.is_err());
        keep.pop();
        // The slot comes back once the socket is gone.
        let mut ok = false;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if let Ok((ws, _)) = tokio_tungstenite::connect_async(&url).await {
                keep.push(ws);
                ok = true;
                break;
            }
        }
        assert!(ok);
    }

    #[test]
    fn the_routes_document() {
        let state = serde_json::json!({"open": true, "guests": [{
            "player": 2, "name": "Sam", "state": "playing", "route": "tailscale", "tier": "good",
            "tier_from": "guest", "rtt_ms": 30,
            "page": {"route_rtt_ms": 31, "route_jitter_ms": 4, "route_loss": 0, "route_switches": 1,
                     "backups": 1, "backup_tier": "playable"}
        }]});
        let r = routes_json(&state);
        assert_eq!(r["tiers"][1]["tier"], "good");
        assert_eq!(r["tiers"][2]["rtt_ms"], 80);
        let g = &r["guests"][0];
        assert_eq!(g["route"], "tailscale");
        assert_eq!(g["tier"], "good");
        assert_eq!(g["route_switches"], 1);
        assert_eq!(g["backup_tier"], "playable");
        assert!(r["probe"]["checks"].is_u64());
    }
}
