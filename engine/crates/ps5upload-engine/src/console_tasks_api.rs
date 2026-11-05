//! Tasks started on the console page, for the apps and for API clients (#658). The list lives in
//! the helper (`ps5upload_core::console_tasks`); these routes forward.
//!
//! * `GET  /api/ps5/console-tasks?addr=` -> `{ok, supported, tasks[]}`: each task
//!   `{id, kind, state, name, files_done, files_total, bytes_done, bytes_total, age_s, cancellable}`.
//!   A helper older than this feature answers `supported: false` with no tasks, not an error.
//! * `POST /api/ps5/console-tasks/cancel` `{addr, id}` -> `{ok}`: needs an explicit `addr`.
//!
//! Errors are `{error, reason}`; `reason` is the helper's token (`bad_job`, `unknown_job`, ...) or
//! `unsupported` (501) for that older helper.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::console_tasks as ct;
use serde::Deserialize;

use crate::console_cleanup_api::reason_of;
use crate::{console_addr_or_default, missing_console_addr, required_console_addr, AppState};

#[derive(Deserialize)]
pub(crate) struct AddrQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct CancelBody {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    id: String,
}

fn status_for(reason: &str) -> StatusCode {
    match reason {
        "ended" => StatusCode::CONFLICT,
        "unknown_job" => StatusCode::NOT_FOUND,
        "unsupported" => StatusCode::NOT_IMPLEMENTED,
        "bad_job" => StatusCode::BAD_REQUEST,
        _ => StatusCode::BAD_GATEWAY,
    }
}

fn failure(e: &anyhow::Error) -> Response {
    let reason = reason_of(e);
    (
        status_for(&reason),
        Json(serde_json::json!({ "error": format!("{e:#}"), "reason": reason })),
    )
        .into_response()
}

fn task_failed(e: &tokio::task::JoinError) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": format!("console tasks failed: {e}"), "reason": "failed" })),
    )
        .into_response()
}

pub(crate) async fn list(State(state): State<AppState>, Query(q): Query<AddrQuery>) -> Response {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    match tokio::task::spawn_blocking(move || ct::list(&addr)).await {
        Ok(Ok(t)) => {
            Json(serde_json::json!({ "ok": true, "supported": t.supported, "tasks": t.tasks }))
                .into_response()
        }
        Ok(Err(e)) => failure(&e),
        Err(e) => task_failed(&e),
    }
}

pub(crate) async fn cancel(Json(req): Json<CancelBody>) -> Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let id = req.id;
    match tokio::task::spawn_blocking(move || ct::cancel(&addr, &id)).await {
        Ok(Ok(())) => Json(serde_json::json!({ "ok": true })).into_response(),
        Ok(Err(e)) => failure(&e),
        Err(e) => task_failed(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_maps_to_a_status() {
        assert_eq!(status_for("unknown_job"), StatusCode::NOT_FOUND);
        assert_eq!(status_for("unsupported"), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(status_for("bad_job"), StatusCode::BAD_REQUEST);
        assert_eq!(status_for("ended"), StatusCode::CONFLICT);
        assert_eq!(status_for("anything_else"), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn cancel_needs_a_console() {
        let r = cancel(Json(CancelBody {
            addr: None,
            id: "abc".into(),
        }))
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }
}
