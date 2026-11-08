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

pub(crate) async fn status(State(state): State<AppState>, Query(q): Query<AddrQuery>) -> Response {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || tn::console(&addr, &tn::Op::Status)).await;
    reply(r.unwrap_or_else(|e| Err(anyhow::anyhow!("tailnet task failed: {e}"))))
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
    let r = tokio::task::spawn_blocking(move || tn::console(&addr, &op)).await;
    reply(r.unwrap_or_else(|e| Err(anyhow::anyhow!("tailnet task failed: {e}"))))
}
