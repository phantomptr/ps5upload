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

/// A share typed with folders after it (`Media/Games/PS5`, a pasted `\\nas\Media\Games`): the
/// first name is the share and the rest goes in front of the start folder, the rule the engine's
/// own connections follow. The helper would ask the server for a share with that whole name.
fn split_typed_share(req: &mut serde_json::Value) {
    let Some(raw) = req
        .get("share")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
    else {
        return;
    };
    let start = req
        .get("start_path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let (share, start_path) = crate::remote::api::split_share(&raw, &start);
    if share.is_empty() {
        return;
    }
    req["share"] = share.into();
    req["start_path"] = start_path.trim_start_matches('/').into();
}

pub(crate) async fn save(Json(mut req): Json<serde_json::Value>) -> Response {
    let Some(addr) =
        required_console_addr(req.get("addr").and_then(|a| a.as_str()).map(str::to_string))
    else {
        return crate::missing_console_addr();
    };
    split_typed_share(&mut req);
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
    fn a_share_typed_with_folders_is_split_before_it_reaches_the_helper() {
        let split = |share: &str, start: &str| {
            let mut req = serde_json::json!({ "addr": "a", "share": share, "start_path": start });
            split_typed_share(&mut req);
            (
                req["share"].as_str().unwrap().to_string(),
                req["start_path"].as_str().unwrap().to_string(),
            )
        };
        let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(split("Media", "Games"), pair("Media", "Games"));
        assert_eq!(split("Media/Games/PS5", ""), pair("Media", "Games/PS5"));
        assert_eq!(
            split("Media/Games", "PS5/New"),
            pair("Media", "Games/PS5/New")
        );
        assert_eq!(split("\\\\nas\\Media\\Games", ""), pair("Media", "Games"));
        assert_eq!(
            split("smb://nas/Media/Games", "x"),
            pair("Media", "Games/x")
        );
        // A name in Chinese is one share, whole (#663).
        assert_eq!(split("共享/游戏", ""), pair("共享", "游戏"));
    }

    #[test]
    fn an_empty_share_stays_empty_so_the_server_text_can_supply_it() {
        let mut req = serde_json::json!({ "addr": "a", "server": "smb://nas/Media/Games" });
        split_typed_share(&mut req);
        assert!(req.get("share").is_none() && req.get("start_path").is_none());
        let mut req = serde_json::json!({ "addr": "a", "share": "  ", "start_path": "x" });
        split_typed_share(&mut req);
        assert_eq!(req["share"], "  ");
        assert_eq!(req["start_path"], "x");
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
