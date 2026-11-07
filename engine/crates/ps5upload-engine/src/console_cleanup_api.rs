//! Console clean-up (spec 2026-10-22-console-cleanup-design.md). The helper scans the console and
//! removes what the user picked (`payload/src/storage_cleanup.c`); these routes forward the calls.
//!
//! * `POST /api/ps5/cleanup/scan`             `{addr}` -> `{scan_id}`
//! * `GET  /api/ps5/cleanup/scan/{id}?addr=`  -> `{state, phase, items[], reserve_bytes, ...}`
//! * `POST /api/ps5/cleanup/scan/{id}/apply`  `{addr, item_ids}` -> `{apply_id}`
//! * `GET  /api/ps5/cleanup/apply/{id}?addr=` -> `{state, done[], skipped[], failed[], freed_bytes}`
//! * `POST /api/ps5/cleanup/cancel`           `{addr, id}`: stops a scan, or an apply after its
//!   current item
//!
//! The destructive calls (apply, cancel) need an explicit `addr`. Apply is also refused (409
//! `busy`) while this engine installs to that console or runs a job against it: the helper
//! refuses on its own while the console has an install queued, but cannot see an install this
//! engine is still preparing.
//!
//! Errors are `{error, reason}`: `reason` is the helper's token (`busy`, `install_running`,
//! `unknown_scan`, `scan_expired`, `scan_not_done`, `unknown_apply`, `bad_request`), or
//! `unsupported` when the helper is older than this feature (501).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::console_cleanup as cc;
use serde::Deserialize;

use crate::{console_addr_or_default, required_console_addr, AppState};

#[derive(Deserialize)]
pub(crate) struct AddrBody {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct AddrQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct ApplyBody {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    item_ids: Vec<u32>,
}

#[derive(Deserialize)]
pub(crate) struct CancelBody {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    id: u32,
}

/// HTTP status for a helper refusal token.
fn status_for(reason: &str) -> StatusCode {
    match reason {
        "busy" | "install_running" | "scan_not_done" => StatusCode::CONFLICT,
        "unknown_scan" | "unknown_apply" => StatusCode::NOT_FOUND,
        "scan_expired" => StatusCode::GONE,
        "bad_request" => StatusCode::BAD_REQUEST,
        "unsupported" => StatusCode::NOT_IMPLEMENTED,
        _ => StatusCode::BAD_GATEWAY,
    }
}

/// The helper's token for an error: the cause of a refusal, `unsupported` for an older helper.
pub(crate) fn reason_of(e: &anyhow::Error) -> String {
    if ps5upload_core::console_files::is_unsupported(e) {
        return "unsupported".into();
    }
    if let Some(m) = e.downcast_ref::<ps5upload_core::mgmt::MgmtError>() {
        let cause = m.cause.trim();
        // The helper's refusal body, `{"ok":false,"err":"<token>"}`, travels as the cause.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(cause) {
            if let Some(t) = v.get("err").and_then(|t| t.as_str()) {
                return t.to_string();
            }
        }
        let head = cause.split_once(':').map_or(cause, |(h, _)| h).trim();
        if !head.is_empty() && head.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            return head.to_string();
        }
    }
    ps5upload_core::mgmt::error_token(e).unwrap_or_else(|| "failed".into())
}

fn error_response(e: anyhow::Error) -> Response {
    let reason = reason_of(&e);
    (
        status_for(&reason),
        Json(serde_json::json!({ "error": format!("{e:#}"), "reason": reason })),
    )
        .into_response()
}

async fn run<F>(f: F) -> Response
where
    F: FnOnce() -> anyhow::Result<serde_json::Value> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => error_response(e),
        Err(e) => error_response(anyhow::anyhow!("cleanup task failed: {e}")),
    }
}

pub(crate) async fn scan(State(state): State<AppState>, Json(req): Json<AddrBody>) -> Response {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    crate::log_at!(Info, "cleanup", "cleanup scan: addr={addr}");
    run(move || cc::scan_start(&addr)).await
}

pub(crate) async fn scan_status(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Query(q): Query<AddrQuery>,
) -> Response {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    run(move || cc::scan_status(&addr, id)).await
}

pub(crate) async fn apply(
    installs: crate::pkg_install::PkgInstallStateHandle,
    id: u32,
    req: ApplyBody,
) -> Response {
    // Destructive: acts only on the console the request names, never the default one.
    let Some(addr) = required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    if installs.jobs.active_for(&addr) || crate::helper_stop::is_busy(&addr) {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "an install or transfer to this PS5 is running; clean up when it is done",
                "reason": "busy",
            })),
        )
            .into_response();
    }
    if req.item_ids.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "no items", "reason": "bad_request" })),
        )
            .into_response();
    }
    crate::log_at!(
        Info,
        "cleanup",
        "cleanup apply: addr={addr} scan={id} items={}",
        req.item_ids.len()
    );
    let ids = req.item_ids;
    run(move || cc::apply_start(&addr, id, &ids)).await
}

pub(crate) async fn apply_status(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Query(q): Query<AddrQuery>,
) -> Response {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    run(move || cc::apply_status(&addr, id)).await
}

pub(crate) async fn cancel(Json(req): Json<CancelBody>) -> Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    let id = req.id;
    crate::log_at!(Info, "cleanup", "cleanup cancel: addr={addr} id={id}");
    run(move || cc::cancel(&addr, id)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_map_to_http_statuses() {
        assert_eq!(status_for("busy"), StatusCode::CONFLICT);
        assert_eq!(status_for("install_running"), StatusCode::CONFLICT);
        assert_eq!(status_for("unknown_scan"), StatusCode::NOT_FOUND);
        assert_eq!(status_for("scan_expired"), StatusCode::GONE);
        assert_eq!(status_for("unsupported"), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(status_for("anything_else"), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn a_helper_refusal_names_its_token() {
        let e: anyhow::Error = ps5upload_core::mgmt::MgmtError {
            label: "CLEANUP_APPLY".into(),
            status: 9,
            cause: "install_running".into(),
        }
        .into();
        assert_eq!(reason_of(&e), "install_running");
        let body: anyhow::Error = ps5upload_core::mgmt::MgmtError {
            label: "CLEANUP_APPLY".into(),
            status: 1,
            cause: r#"{"ok":false,"err":"bad_request"}"#.into(),
        }
        .into();
        assert_eq!(reason_of(&body), "bad_request");
        let old: anyhow::Error = ps5upload_core::mgmt::MgmtError {
            label: "CLEANUP_SCAN".into(),
            status: ps5upload_core::console_files::ERR_UNKNOWN_METHOD,
            cause: "unknown method".into(),
        }
        .into();
        assert_eq!(reason_of(&old), "unsupported");
    }
}
