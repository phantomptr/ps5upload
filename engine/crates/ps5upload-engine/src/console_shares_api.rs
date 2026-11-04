//! The console's saved network shares (#498): what the console page (PS5Upload Minion) browses in
//! Files and scans for its game Library. They live on the console (`payload/src/smb_shares.c`);
//! these routes forward the PC's add / edit / remove to the helper.
//!
//! * `GET  /api/ps5/console-shares?addr=`   -> `{ok, shares[], max}`
//! * `POST /api/ps5/console-shares/save`    `{addr, id?, name, server, share, user, password?,
//!   domain, start_path}` -> `{ok, share, dialect}`: the console signs in to the share first
//! * `POST /api/ps5/console-shares/remove`  `{addr, id}` -> `{ok}`
//!
//! Save and remove need an explicit `addr`. Errors are `{error, reason}`: `reason` is the helper's
//! token (`smb_auth`, `smb_unreachable`, `smb_no_share`, `not_found`, `shares_full`, `bad_*`,
//! `unknown_share`, `busy`, `smb_daemon_unavailable`, `smb_loader_unavailable`), or `unsupported`
//! (501) for a helper older than this feature. The password is never logged.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::console_shares as cs;
use serde::Deserialize;

use crate::console_cleanup_api::reason_of;
use crate::{console_addr_or_default, required_console_addr, AppState};

#[derive(Deserialize)]
pub(crate) struct AddrQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct RemoveBody {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    id: String,
}

/// HTTP status for a helper refusal token.
fn status_for(reason: &str) -> StatusCode {
    match reason {
        "smb_auth" | "access_denied" => StatusCode::FORBIDDEN,
        "unknown_share" | "smb_no_share" | "not_found" | "not_dir" => StatusCode::NOT_FOUND,
        "shares_full" => StatusCode::INSUFFICIENT_STORAGE,
        "busy" => StatusCode::CONFLICT,
        "unsupported" => StatusCode::NOT_IMPLEMENTED,
        r if r.starts_with("bad_") => StatusCode::BAD_REQUEST,
        _ => StatusCode::BAD_GATEWAY,
    }
}

async fn run<F>(f: F) -> Response
where
    F: FnOnce() -> anyhow::Result<serde_json::Value> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => {
            let reason = reason_of(&e);
            (
                status_for(&reason),
                Json(serde_json::json!({ "error": format!("{e:#}"), "reason": reason })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("console shares task failed: {e}"), "reason": "failed" })),
        )
            .into_response(),
    }
}

pub(crate) async fn list(State(state): State<AppState>, Query(q): Query<AddrQuery>) -> Response {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    run(move || cs::list(&addr)).await
}

pub(crate) async fn save(Json(req): Json<serde_json::Value>) -> Response {
    let Some(addr) =
        required_console_addr(req.get("addr").and_then(|a| a.as_str()).map(str::to_string))
    else {
        return crate::missing_console_addr();
    };
    crate::log_info!(
        "console share save: addr={addr} id={}",
        req.get("id").and_then(|v| v.as_str()).unwrap_or("(new)")
    );
    run(move || cs::save(&addr, &req)).await
}

pub(crate) async fn remove(Json(req): Json<RemoveBody>) -> Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    crate::log_info!("console share remove: addr={addr} id={}", req.id);
    let id = req.id;
    run(move || cs::remove(&addr, &id)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_map_to_http_statuses() {
        assert_eq!(status_for("smb_auth"), StatusCode::FORBIDDEN);
        assert_eq!(status_for("unknown_share"), StatusCode::NOT_FOUND);
        assert_eq!(status_for("bad_host"), StatusCode::BAD_REQUEST);
        assert_eq!(status_for("shares_full"), StatusCode::INSUFFICIENT_STORAGE);
        assert_eq!(status_for("unsupported"), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(status_for("smb_unreachable"), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn a_helper_refusal_names_its_token() {
        let e: anyhow::Error = ps5upload_core::mgmt::MgmtError {
            label: "SMB_SHARES_SAVE".into(),
            status: 1,
            cause: "smb_auth".into(),
        }
        .into();
        assert_eq!(reason_of(&e), "smb_auth");
    }
}
