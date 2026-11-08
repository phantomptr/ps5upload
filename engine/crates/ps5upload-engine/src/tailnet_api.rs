//! "Put this PS5 on Tailscale" (`ps5upload_core::tailnet`, the helper's `tailnet` method).
//!
//! * `GET  /api/ps5/tailnet?addr=`  -> the console's node: `{ok, enabled, state, login_url, ip,
//!   name, home_derp, derp_connected, home_latency_ms, udp_port, endpoints[], key_expiry,
//!   warning, error, serving:{udp_flows, tcp_conns}, peers:[{name, ip, online, path, direct,
//!   derp_region, latency_ms, last_rx_ms}]}`; `state` is `off`, `connecting`, `needs-login`
//!   (open `login_url`), `wait-approval`, `running`, `paused`, `backoff` or `signed-out`
//! * `POST /api/ps5/tailnet` `{addr, op, control_url?, hostname?, authkey?}`: `op` is `enable`
//!   (join; the first time this answers `needs-login` with a `login_url` to open, unless an
//!   `authkey` was given), `disable` (leave, keep the keys) or `logout` (leave and forget them).
//!   Answers the same status. Needs an explicit `addr`.
//!
//! The engine's own node (Settings › Tailscale; `tailnet_engine.rs`):
//!
//! * `GET  /api/tailnet` -> `{ok, enabled, state, login_url, ip, name, control_url, hostname,
//!   home_derp, derp_connected, home_latency_ms, key_expiry, error, peers:[{name, ip, online,
//!   path, direct, derp_region, latency_ms}], consoles:[{host, ip, name, via, latency_ms}]}`
//! * `POST /api/tailnet` `{op: enable|disable|logout, control_url?, hostname?, authkey?}` ->
//!   the same
//! * `GET  /api/tailnet/via?addr=` -> `{via: lan|direct|relayed, latency_ms}`: how the engine
//!   reaches that console right now (the connection chip)
//!
//! The console then serves only Remote Play (TCP 9295, UDP 9296, 9302) and the helper (TCP 9120)
//! to the tailnet. Errors are `{error, reason}`; `unsupported` (501) when the helper is older.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::tailnet as tn;
use serde::Deserialize;

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
}

fn reply(r: anyhow::Result<Option<serde_json::Value>>) -> Response {
    match r {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => (
            StatusCode::NOT_IMPLEMENTED,
            Json(serde_json::json!({
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
            (code, Json(serde_json::json!({ "error": format!("{e:#}"), "reason": reason }))).into_response()
        }
    }
}

/// A console's node answered: remember its tailnet address for when it is away.
fn remember(addr: &str, r: &anyhow::Result<Option<serde_json::Value>>) {
    if let Ok(Some(v)) = r {
        if let Some(ip) = tn::tailnet_ip(v).and_then(|s| s.parse().ok()) {
            let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
            crate::tailnet_engine::engine().remember(addr, ip, name.trim_end_matches('.'));
        }
    }
}

pub(crate) async fn status(State(state): State<AppState>, Query(q): Query<AddrQuery>) -> Response {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || {
        let r = tn::console(&addr, &tn::Op::Status);
        remember(&addr, &r);
        r
    })
    .await;
    reply(r.unwrap_or_else(|e| Err(anyhow::anyhow!("tailnet task failed: {e}"))))
}

#[derive(Deserialize)]
pub(crate) struct EngineOpBody {
    op: String,
    /// op `remember`: a console's LAN address and its tailnet address (what "Put this PS5 on
    /// Tailscale" records by itself).
    #[serde(default)]
    host: String,
    #[serde(default)]
    ip: String,
    #[serde(default)]
    control_url: String,
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    authkey: String,
}

pub(crate) async fn engine_status() -> Response {
    match tokio::task::spawn_blocking(|| crate::tailnet_engine::engine().status_json()).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn engine_op(Json(req): Json<EngineOpBody>) -> Response {
    // The auth key is a secret: never logged.
    crate::log_at!(Info, "tailnet", "tailnet (this device) {}", req.op);
    let r = tokio::task::spawn_blocking(move || {
        let e = crate::tailnet_engine::engine();
        if req.op == "remember" {
            let ip: std::net::Ipv4Addr = req
                .ip
                .parse()
                .map_err(|_| anyhow::anyhow!("ip must be the console's tailnet IPv4 address"))?;
            if req.host.trim().is_empty() {
                anyhow::bail!("host (the console's LAN address) is required");
            }
            e.remember(&req.host, ip, "");
        }
        e.op(&req.op, &req.control_url, &req.hostname, &req.authkey)
    })
    .await;
    match r {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("{e:#}"), "reason": "bad_request" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct RouteQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    port: u16,
    #[serde(default)]
    udp: bool,
}

/// `GET /api/tailnet/route?addr=&port=&udp=` -> `{dial, via}`: where the engine dials that
/// console port right now (`dial` null = the console's own address). Opens the local mapping
/// when the console is away.
pub(crate) async fn route(State(state): State<AppState>, Query(q): Query<RouteQuery>) -> Response {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let (dial, v) = tokio::task::spawn_blocking(move || {
        let d = crate::tailnet_engine::engine().route(&addr, q.port, q.udp);
        (d, ps5upload_core::route::via(&addr))
    })
    .await
    .unwrap_or((None, ps5upload_core::route::Via::Lan));
    let (via, ms) = crate::tailnet_engine::via_json(&v);
    Json(serde_json::json!({ "dial": dial.map(|a| a.to_string()), "via": via, "latency_ms": ms }))
        .into_response()
}

pub(crate) async fn via(State(state): State<AppState>, Query(q): Query<AddrQuery>) -> Response {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let v = tokio::task::spawn_blocking(move || ps5upload_core::route::via(&addr))
        .await
        .unwrap_or(ps5upload_core::route::Via::Lan);
    let (via, ms) = crate::tailnet_engine::via_json(&v);
    Json(serde_json::json!({ "via": via, "latency_ms": ms })).into_response()
}

pub(crate) async fn op(Json(req): Json<OpBody>) -> Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    let op = match tn::Op::parse(&req.op, &req.control_url, &req.hostname, &req.authkey) {
        Ok(op) => op,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("{e:#}"), "reason": "bad_request" })),
            )
                .into_response()
        }
    };
    // The auth key is a secret: never logged.
    crate::log_at!(Info, "tailnet", "tailnet {}: addr={addr}", req.op);
    let r = tokio::task::spawn_blocking(move || {
        let r = tn::console(&addr, &op);
        remember(&addr, &r);
        r
    })
    .await;
    reply(r.unwrap_or_else(|e| Err(anyhow::anyhow!("tailnet task failed: {e}"))))
}
