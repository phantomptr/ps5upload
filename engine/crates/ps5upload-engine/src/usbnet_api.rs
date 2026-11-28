//! A USB Ethernet adapter on the PS5, EXPERIMENTAL (`ps5upload_core::usbnet`, the helper's
//! `usbnet` method; the routing is `usbnet_engine.rs`).
//!
//! * `GET  /api/ps5/usbnet?addr=` -> the helper's adapter: `{ok, experimental, enabled, state,
//!   error, adapter:{usb_id, chip, max_mbps}|null, link_mbps, ip, mask, gateway, source, static_ip,
//!   port, minion, rx_bytes, tx_bytes, rx_frames, tx_frames, errors, link_changes, uptime_s,
//!   restarts, seen:["vvvv:pppp"]}` plus the engine's own view, `route:{reachable, preferred,
//!   checked_ms_ago}`: `reachable` is this computer's last TCP probe of `ip:port` (it answered),
//!   `preferred` whether transfers to this console go through the adapter now, `checked_ms_ago`
//!   how old that probe is (null before the first). `state` is `off` (the setting), `none` (no
//!   supported adapter plugged in), `paused` (rest mode), `starting`, `no_link`, `no_address`,
//!   `ready` (it serves at `ip`), `failed` or `unsupported`; `source` is `dhcp`, `link-local`,
//!   `static` or empty. `seen` lists the console's other USB devices (vendor:product), so an
//!   adapter this feature does not know can be named in a report.
//! * `POST /api/ps5/usbnet` `{addr, op, ip?, mask?, gateway?}`: `op` is `status`, `enable`,
//!   `disable` (the setting, kept on the console) or `address` (`ip` a static IPv4 address with
//!   optional `mask` and `gateway`; `ip` "" goes back to DHCP). Answers the same as the GET.
//!   Needs an explicit `addr`.
//!
//! The console's built-in network is never touched. Errors are `{error, reason}`; `unsupported`
//! (501) when the helper is older than the feature.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::usbnet as un;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{console_addr_or_default, required_console_addr, AppState};

#[derive(Deserialize)]
pub(crate) struct UsbnetQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct UsbnetBody {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    #[serde(default)]
    op: String,
    #[serde(default)]
    ip: String,
    #[serde(default)]
    mask: String,
    #[serde(default)]
    gateway: String,
}

/// The helper's answer with the engine's view added (`route`), or the error.
fn reply(addr: &str, r: anyhow::Result<Option<Value>>) -> Response {
    match r {
        Ok(Some(mut v)) => {
            crate::usbnet_engine::note_answer(addr, &v);
            if let Some(o) = v.as_object_mut() {
                o.insert("route".into(), crate::usbnet_engine::route_json(addr));
            }
            Json(v).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({
                "error": "this PS5's helper is older than USB network adapter support; send the payload again",
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

/// One call to the helper and its answer, off the async threads (the call, and the router's
/// home check that `route` reads, may wait on the network).
async fn call(addr: String, op: un::Op) -> Response {
    tokio::task::spawn_blocking(move || reply(&addr, un::console(&addr, &op)))
        .await
        .unwrap_or_else(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("usbnet task failed: {e}") })),
            )
                .into_response()
        })
}

pub(crate) async fn status(
    State(state): State<AppState>,
    Query(q): Query<UsbnetQuery>,
) -> Response {
    call(
        console_addr_or_default(q.addr, &state.default_ps5_addr),
        un::Op::Status,
    )
    .await
}

pub(crate) async fn op(Json(req): Json<UsbnetBody>) -> Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    let op = match un::Op::parse(req.op.trim(), &req.ip, &req.mask, &req.gateway) {
        Ok(op) => op,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("{e:#}"), "reason": "bad_request" })),
            )
                .into_response()
        }
    };
    if op.mutates() {
        crate::log_at!(Info, "usbnet", "usbnet {addr}: {}", req.op.trim());
    }
    call(addr, op).await
}
