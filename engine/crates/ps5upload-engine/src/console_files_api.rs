//! The console file manager, the PC side: the console's pinned / recent folders and the
//! "Show on TV" / "Show on PC" handoffs (`ps5upload_core::console_files`).
//!
//! * `GET  /api/ps5/files/prefs?addr=`        -> `{supported, favorites, recent, last}`
//! * `POST /api/ps5/files/prefs/set`          `{addr, op: pin|unpin|visit, path}` -> the same
//! * `POST /api/ps5/files/handoff/put`        `{addr, to: console|pc, path}` -> `{supported, ok}`
//! * `POST /api/ps5/files/handoff/take`       `{addr, for: console|pc}` -> `{supported, path?, age_ms?}`
//!
//! An older helper does not know the methods: every route then answers 200
//! `{"supported":false}` and the PC app hides the features (feature detect, never an error).
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use ps5upload_core::console_files::{self as cf, FilesPrefs, Handoff, PrefsOp, Side};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{console_addr_or_default, json_err, required_console_addr, AddrQuery, AppState};

fn unsupported() -> Value {
    json!({ "supported": false })
}

fn prefs_answer(p: Option<FilesPrefs>) -> Value {
    match p {
        None => unsupported(),
        Some(p) => json!({
            "supported": true,
            "favorites": p.favorites,
            "recent": p.recent,
            "last": p.last,
        }),
    }
}

fn take_answer(h: Option<Option<Handoff>>) -> Value {
    match h {
        None => unsupported(),
        Some(None) => json!({ "supported": true }),
        Some(Some(h)) => json!({ "supported": true, "path": h.path, "age_ms": h.age_ms }),
    }
}

fn put_answer(r: Option<()>) -> Value {
    match r {
        None => unsupported(),
        Some(()) => json!({ "supported": true, "ok": true }),
    }
}

/// One call on a blocking thread, answered as JSON (a failure is 502 with the helper's words).
async fn answer(
    f: impl FnOnce() -> anyhow::Result<Value> + Send + 'static,
) -> axum::response::Response {
    let r = tokio::task::spawn_blocking(f)
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

pub(crate) fn prefs_get(addr: &str) -> anyhow::Result<Value> {
    Ok(prefs_answer(cf::prefs_get(addr)?))
}

pub(crate) fn prefs_set(addr: &str, op: PrefsOp, path: &str) -> anyhow::Result<Value> {
    Ok(prefs_answer(cf::prefs_set(addr, op, path)?))
}

pub(crate) fn handoff_put(addr: &str, to: Side, path: &str) -> anyhow::Result<Value> {
    Ok(put_answer(cf::handoff_put(addr, to, path)?))
}

pub(crate) fn handoff_take(addr: &str, side: Side) -> anyhow::Result<Value> {
    Ok(take_answer(cf::handoff_take(addr, side)?))
}

/// A path the helper would refuse anyway, refused here with a clear 400.
fn bad_path(path: &str) -> bool {
    !path.starts_with('/') || path.len() >= 1024 || path.contains('\0')
}

pub(crate) async fn prefs_get_handler(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    answer(move || prefs_get(&addr)).await
}

#[derive(Deserialize)]
pub(crate) struct PrefsSetReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    op: PrefsOp,
    path: String,
}

pub(crate) async fn prefs_set_handler(Json(req): Json<PrefsSetReq>) -> axum::response::Response {
    if bad_path(&req.path) {
        return json_err(StatusCode::BAD_REQUEST, "bad_path").into_response();
    }
    let Some(addr) = required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    answer(move || prefs_set(&addr, req.op, &req.path)).await
}

#[derive(Deserialize)]
pub(crate) struct HandoffPutReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    to: Side,
    path: String,
}

pub(crate) async fn handoff_put_handler(
    Json(req): Json<HandoffPutReq>,
) -> axum::response::Response {
    if bad_path(&req.path) {
        return json_err(StatusCode::BAD_REQUEST, "bad_path").into_response();
    }
    let Some(addr) = required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    answer(move || handoff_put(&addr, req.to, &req.path)).await
}

#[derive(Deserialize)]
pub(crate) struct HandoffTakeReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    #[serde(rename = "for")]
    side: Side,
}

pub(crate) async fn handoff_take_handler(Json(req): Json<HandoffTakeReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    answer(move || handoff_take(&addr, req.side)).await
}

/// File metadata for the desktop/browser properties dialog.
pub(crate) async fn stat_handler(
    State(state): State<AppState>,
    Json(req): Json<StatReq>,
) -> axum::response::Response {
    if bad_path(&req.path) {
        return json_err(StatusCode::BAD_REQUEST, "bad_path").into_response();
    }
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    answer(move || {
        Ok(serde_json::to_value(ps5upload_core::fs_ops::fs_stat(
            &addr, &req.path,
        )?)?)
    })
    .await
}

#[derive(Deserialize)]
pub(crate) struct StatReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    path: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps5upload_core::mgmt::{self, Method, MgmtError, MgmtTransport};
    use std::sync::Arc;
    use std::time::Duration;

    /// A helper that knows the files.* methods (`new`), or an older one that does not.
    struct Helper {
        new: bool,
    }

    impl MgmtTransport for Helper {
        fn call(
            &self,
            _addr: &str,
            method: Method,
            label: &str,
            body: &[u8],
            _timeout: Duration,
        ) -> anyhow::Result<Option<Vec<u8>>> {
            if !self.new {
                return Err(MgmtError {
                    label: label.into(),
                    status: cf::ERR_UNKNOWN_METHOD,
                    cause: "unknown_method".into(),
                }
                .into());
            }
            let body = String::from_utf8_lossy(body);
            Ok(Some(match method.id {
                148 | 149 => {
                    br#"{"v":1,"favorites":["/data/homebrew"],"recent":[],"last":null}"#.to_vec()
                }
                150 => br#"{"ok":true}"#.to_vec(),
                151 if body.contains("\"pc\"") => {
                    br#"{"path":"/mnt/usb0/games","age_ms":900}"#.to_vec()
                }
                151 => b"{}".to_vec(),
                _ => return Ok(None),
            }))
        }
    }

    #[test]
    fn a_new_helper_answers_supported_with_its_data() {
        let _g = mgmt::scoped_transport(Arc::new(Helper { new: true }));
        let p = prefs_get("10.0.0.2").unwrap();
        assert_eq!(p["supported"], true);
        assert_eq!(p["favorites"], json!(["/data/homebrew"]));
        assert_eq!(p["last"], Value::Null);
        let p = prefs_set("10.0.0.2", PrefsOp::Pin, "/data/homebrew").unwrap();
        assert_eq!(p["favorites"], json!(["/data/homebrew"]));
        assert_eq!(
            handoff_put("10.0.0.2", Side::Console, "/data").unwrap(),
            json!({ "supported": true, "ok": true })
        );
        assert_eq!(
            handoff_take("10.0.0.2", Side::Pc).unwrap(),
            json!({ "supported": true, "path": "/mnt/usb0/games", "age_ms": 900 })
        );
        assert_eq!(
            handoff_take("10.0.0.2", Side::Console).unwrap(),
            json!({ "supported": true })
        );
    }

    #[test]
    fn an_older_helper_is_reported_unsupported_on_every_route() {
        let _g = mgmt::scoped_transport(Arc::new(Helper { new: false }));
        let no = json!({ "supported": false });
        assert_eq!(prefs_get("h").unwrap(), no);
        assert_eq!(prefs_set("h", PrefsOp::Visit, "/data").unwrap(), no);
        assert_eq!(handoff_put("h", Side::Console, "/data").unwrap(), no);
        assert_eq!(handoff_take("h", Side::Pc).unwrap(), no);
    }

    #[test]
    fn requests_parse_with_their_wire_names() {
        let r: HandoffTakeReq = serde_json::from_str(r#"{"addr":"h","for":"pc"}"#).unwrap();
        assert_eq!(r.side, Side::Pc);
        let r: HandoffPutReq =
            serde_json::from_str(r#"{"to":"console","path":"/data/x"}"#).unwrap();
        assert_eq!(r.to, Side::Console);
        let r: PrefsSetReq = serde_json::from_str(r#"{"op":"unpin","path":"/data"}"#).unwrap();
        assert_eq!(r.op, PrefsOp::Unpin);
        assert!(serde_json::from_str::<HandoffTakeReq>(r#"{"for":"tv"}"#).is_err());
        assert!(serde_json::from_str::<PrefsSetReq>(r#"{"op":"drop","path":"/"}"#).is_err());
        assert!(bad_path("data") && bad_path("") && !bad_path("/data/x"));
    }
}

#[derive(Deserialize)]
pub(crate) struct MinionTileReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    tile: Option<bool>,
    enabled: Option<bool>,
    lan: Option<bool>,
    #[serde(default)]
    remove: bool,
}

pub(crate) async fn minion_tile_handler(
    Json(req): Json<MinionTileReq>,
) -> axum::response::Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    let change = cf::MinionChange {
        tile: req.tile,
        enabled: req.enabled,
        lan: req.lan,
        remove: req.remove,
    };
    answer(move || cf::minion_tile(&addr, change)).await
}

#[derive(Deserialize)]
pub(crate) struct CheatImportReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    path: String,
}

pub(crate) async fn cheat_import_handler(
    Json(req): Json<CheatImportReq>,
) -> axum::response::Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    answer(move || {
        Ok(serde_json::to_value(
            ps5upload_core::cheats::cheats_import_local(&addr, std::path::Path::new(&req.path))?,
        )?)
    })
    .await
}
