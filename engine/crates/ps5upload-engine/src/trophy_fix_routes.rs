//! Trophy files that still name an account id the user no longer has (#853).
//!
//! Offline activation changes a user's account id in the registry; their TRPTITLE.DAT files keep
//! the old one and the console stops adding trophies to them. The helper's `trophy.fix`
//! (payload/src/trophy_fix.c) puts the new id in, keeping a backup of each file on the console.
//!
//! - `POST /api/profile/trophies/fix?dry_run=false` {addr, uid, slot | new, old?}: the fix. Without
//!   `dry_run=false` it is a dry run that changes nothing. `new` defaults to the slot's account id;
//!   with no `old` the account field is set whatever it holds (Repair).
//! - `POST /api/profile/trophies/check` the same, always a dry run (the MCP tool's route).
//! - `POST /api/profile/trophies/backups` {addr, uid}: the backups kept on the console.
//! - `POST /api/profile/trophies/restore` {addr, uid, backup}: puts one back.
//!
//! A change is refused while a game runs (it may have its trophy files open); a process list that
//! cannot be read counts as a game running.

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use ps5upload_core::profile::{self as prof, parse_account_hex};

use crate::{json_err, missing_console_addr, required_console_addr};

#[derive(Deserialize)]
pub(crate) struct TrophyFixReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    #[serde(default)]
    uid: u32,
    /// The account slot that goes with the user: `new` defaults to its id.
    #[serde(default)]
    slot: Option<i32>,
    /// "0x" + hex. Absent: whatever the account field holds is replaced.
    #[serde(default)]
    old: Option<String>,
    #[serde(default)]
    new: Option<String>,
    #[serde(default)]
    backup: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct DryRunQuery {
    #[serde(default)]
    dry_run: Option<String>,
}

/// `?dry_run` as the route reads it: absent, empty, "true", "1" or "yes" is a dry run; only
/// "false", "0" or "no" writes. Anything else is refused.
pub(crate) fn dry_run_flag(q: Option<&str>) -> Result<bool, String> {
    match q.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") | Some("true") | Some("1") | Some("yes") => Ok(true),
        Some("false") | Some("0") | Some("no") => Ok(false),
        Some(other) => Err(format!("dry_run must be true or false, not {other:?}")),
    }
}

/// The old id a request names: None when it names none ("" or "any" too); Err when it is not hex.
pub(crate) fn old_id_of(old: Option<&str>) -> Result<Option<u64>, String> {
    match old.map(str::trim) {
        None | Some("") | Some("any") => Ok(None),
        Some(s) => parse_account_hex(s)
            .map(Some)
            .ok_or_else(|| format!("not an account id: {s:?}")),
    }
}

/// The trophy fix an activation asks for, as (uid, old, new): when a user goes with the slot and
/// the id changed. An old id of 0 (a user that never had one) counts: their trophy files hold 0,
/// and the helper only ever changes the account field for it.
pub(crate) fn activation_fix(
    uid: Option<u32>,
    old_id: Option<&str>,
    new_id: &str,
) -> Option<(u32, u64, u64)> {
    let uid = uid.filter(|u| *u != 0)?;
    let old = parse_account_hex(old_id?)?;
    let new = parse_account_hex(new_id).filter(|n| *n != 0)?;
    (old != new).then_some((uid, old, new))
}

/// The title of a game running on `addr`, if one is. Err when the process list cannot be read.
fn game_running(addr: &str) -> Result<Option<String>, String> {
    Ok(crate::save_backup_server::running_titles(addr)?
        .into_iter()
        .next())
}

fn refuse_running(addr: &str) -> Option<Response> {
    match game_running(addr) {
        Ok(None) => None,
        Ok(Some(title)) => Some(
            (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": format!("game_running: close {title} on the PS5 first"),
                    "error_reason": "game_running",
                    "title_id": title,
                })),
            )
                .into_response(),
        ),
        Err(e) => Some(
            json_err(
                StatusCode::BAD_GATEWAY,
                format!("could not check for a running game: {e}"),
            )
            .into_response(),
        ),
    }
}

/// The account id a slot has now (`new` for a Repair).
fn slot_id(addr: &str, slot: i32) -> Result<u64, String> {
    let info = prof::profile_info(addr).map_err(|e| format!("{e:#}"))?;
    let s = info
        .slots
        .iter()
        .find(|s| s.slot == slot)
        .ok_or_else(|| format!("no account slot {slot}"))?;
    parse_account_hex(&s.id)
        .filter(|id| *id != 0)
        .ok_or_else(|| "this account has no account id: activate it first".to_string())
}

fn blocking_err(e: tokio::task::JoinError) -> Response {
    json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response()
}

async fn fix(req: TrophyFixReq, dry_run: bool) -> Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr().into_response();
    };
    if req.uid == 0 {
        return json_err(StatusCode::BAD_REQUEST, "uid is required").into_response();
    }
    let old = match old_id_of(req.old.as_deref()) {
        Ok(o) => o,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let given_new = match req.new.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => match parse_account_hex(s).filter(|n| *n != 0) {
            Some(n) => Some(n),
            None => {
                return json_err(StatusCode::BAD_REQUEST, format!("not an account id: {s:?}"))
                    .into_response()
            }
        },
    };
    if given_new.is_none() && req.slot.is_none() {
        return json_err(StatusCode::BAD_REQUEST, "slot or new is required").into_response();
    }
    let (uid, slot) = (req.uid, req.slot);
    crate::log_info!("trophy_fix: addr={addr} uid={uid} dry_run={dry_run}");
    let r = tokio::task::spawn_blocking(move || -> Result<Value, Refusal> {
        let new = match given_new {
            Some(n) => n,
            None => {
                slot_id(&addr, slot.unwrap_or(0)).map_err(|e| refuse(StatusCode::BAD_REQUEST, e))?
            }
        };
        if !dry_run {
            if let Some(refusal) = refuse_running(&addr) {
                return Err(Box::new(refusal));
            }
        }
        prof::trophy_fix(&addr, uid, old, new, dry_run)
            .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
            .map_err(|e| refuse(StatusCode::BAD_GATEWAY, format!("{e:#}")))
    })
    .await;
    match r {
        Ok(Ok(v)) => (StatusCode::OK, Json(v)).into_response(),
        Ok(Err(resp)) => *resp,
        Err(e) => blocking_err(e),
    }
}

/// An answer that is not a success, boxed (a `Response` is large for a `Result`'s error).
type Refusal = Box<Response>;

fn refuse(code: StatusCode, msg: impl Into<String>) -> Refusal {
    Box::new(json_err(code, msg).into_response())
}

/// POST /api/profile/trophies/fix — a dry run unless `?dry_run=false`.
pub(crate) async fn trophy_fix_handler(
    Query(q): Query<DryRunQuery>,
    Json(req): Json<TrophyFixReq>,
) -> Response {
    match dry_run_flag(q.dry_run.as_deref()) {
        Ok(dry_run) => fix(req, dry_run).await,
        Err(e) => json_err(StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// POST /api/profile/trophies/check — what the fix would change; never writes.
pub(crate) async fn trophy_check_handler(Json(req): Json<TrophyFixReq>) -> Response {
    fix(req, true).await
}

/// POST /api/profile/trophies/backups — the trophy backups kept on the console, newest first.
pub(crate) async fn trophy_backups_handler(Json(req): Json<TrophyFixReq>) -> Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr().into_response();
    };
    let uid = req.uid;
    if uid == 0 {
        return json_err(StatusCode::BAD_REQUEST, "uid is required").into_response();
    }
    let r = tokio::task::spawn_blocking(move || prof::trophy_backups(&addr, uid)).await;
    match r {
        Ok(Ok(backups)) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "backups": backups })),
        )
            .into_response(),
        Ok(Err(e)) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
        Err(e) => blocking_err(e),
    }
}

/// POST /api/profile/trophies/restore — puts a backup's files back (no game running).
pub(crate) async fn trophy_restore_handler(Json(req): Json<TrophyFixReq>) -> Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr().into_response();
    };
    let uid = req.uid;
    let Some(name) = req.backup.filter(|b| !b.trim().is_empty()) else {
        return json_err(StatusCode::BAD_REQUEST, "backup is required").into_response();
    };
    if uid == 0 {
        return json_err(StatusCode::BAD_REQUEST, "uid is required").into_response();
    }
    crate::log_info!("trophy_restore: addr={addr} uid={uid} backup={name}");
    let r = tokio::task::spawn_blocking(move || -> Result<Value, Refusal> {
        if let Some(refusal) = refuse_running(&addr) {
            return Err(Box::new(refusal));
        }
        prof::trophy_restore(&addr, uid, &name)
            .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
            .map_err(|e| refuse(StatusCode::BAD_GATEWAY, format!("{e:#}")))
    })
    .await;
    match r {
        Ok(Ok(v)) => (StatusCode::OK, Json(v)).into_response(),
        Ok(Err(resp)) => *resp,
        Err(e) => blocking_err(e),
    }
}

/// After an activation: the fix it asks for ([`activation_fix`]), run with a backup, as the
/// `trophies` field of the activation's answer. Never fails the activation: a game running or a
/// failed fix is reported, and the Repair action can do it later.
pub(crate) fn after_activation(
    addr: &str,
    uid: Option<u32>,
    old_id: Option<&str>,
    new_id: &str,
) -> Value {
    let Some((uid, old, new)) = activation_fix(uid, old_id, new_id) else {
        return Value::Null;
    };
    match game_running(addr) {
        Ok(Some(title)) => return json!({ "skipped": "game_running", "title_id": title }),
        Err(e) => return json!({ "skipped": "process_list", "error": e }),
        Ok(None) => {}
    }
    match prof::trophy_fix(addr, uid, Some(old), new, false) {
        Ok(r) => json!({
            "changed": r.changed,
            "failed": r.failed,
            "left": r.left,
            "backup": r.backup,
        }),
        Err(e) => json!({ "error": format!("{e:#}") }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_is_the_default_and_only_false_writes() {
        assert_eq!(dry_run_flag(None), Ok(true));
        assert_eq!(dry_run_flag(Some("")), Ok(true));
        assert_eq!(dry_run_flag(Some("true")), Ok(true));
        assert_eq!(dry_run_flag(Some("1")), Ok(true));
        assert_eq!(dry_run_flag(Some("FALSE")), Ok(false));
        assert_eq!(dry_run_flag(Some("0")), Ok(false));
        assert_eq!(dry_run_flag(Some("no")), Ok(false));
        assert!(dry_run_flag(Some("maybe")).is_err());
    }

    #[test]
    fn old_id_absent_means_any() {
        assert_eq!(old_id_of(None), Ok(None));
        assert_eq!(old_id_of(Some(" ")), Ok(None));
        assert_eq!(old_id_of(Some("any")), Ok(None));
        assert_eq!(old_id_of(Some("0x0")), Ok(Some(0)));
        assert_eq!(old_id_of(Some("0x1122")), Ok(Some(0x1122)));
        assert!(old_id_of(Some("zz")).is_err());
    }

    #[test]
    fn activation_fixes_only_a_changed_id_of_a_known_user() {
        let new = "0x99aabbccddeeff01";
        assert_eq!(
            activation_fix(Some(7), Some("0x0000000000000000"), new),
            Some((7, 0, 0x99aabbccddeeff01))
        );
        assert_eq!(
            activation_fix(Some(7), Some("0x1122334455667788"), new),
            Some((7, 0x1122334455667788, 0x99aabbccddeeff01))
        );
        assert_eq!(activation_fix(Some(7), Some(new), new), None);
        assert_eq!(activation_fix(None, Some("0x0"), new), None);
        assert_eq!(activation_fix(Some(0), Some("0x0"), new), None);
        assert_eq!(activation_fix(Some(7), None, new), None); // an older helper does not say
        assert_eq!(activation_fix(Some(7), Some("0x0"), "0x0"), None);
    }

    #[test]
    fn request_reads_the_console_aliases() {
        let r: TrophyFixReq =
            serde_json::from_str(r#"{"console":"10.0.0.2","uid":5,"slot":2}"#).unwrap();
        assert_eq!(r.addr.as_deref(), Some("10.0.0.2"));
        assert_eq!((r.uid, r.slot), (5, Some(2)));
        assert!(r.old.is_none() && r.new.is_none());
    }
}
