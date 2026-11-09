//! `/api/ps5/pldmgr/*` — the console's Payload Manager (itsPLK's `pldmgr.elf`, :8084) as
//! the app sees it: what it stores, launching and deleting entries, its boot autoload
//! list, and storing a payload under its real name. Every call is a plain HTTP request
//! to the console (`ps5upload_core::payload_manager`); nothing is cached here.

use axum::{
    extract::{Query, State},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use ps5upload_core::payload_manager as pm;

#[derive(Deserialize)]
pub(crate) struct HostParams {
    #[serde(alias = "console", alias = "addr")]
    host: String,
}

fn bad(e: String) -> Response {
    crate::json_err(axum::http::StatusCode::BAD_GATEWAY, e).into_response()
}

fn host(h: &str) -> String {
    ps5upload_core::events::console_host(h.trim())
}

/// GET /api/ps5/pldmgr?host= — `{ok, present, version, payloads, autoload}`; `present:false`
/// (with `error`) when nothing answers on :8084.
pub(crate) async fn overview(
    State(_s): State<crate::AppState>,
    Query(q): Query<HostParams>,
) -> Response {
    let h = host(&q.host);
    let r = tokio::task::spawn_blocking(move || {
        let version = match pm::version(&h) {
            Ok(v) => v,
            Err(e) => {
                return serde_json::json!({ "ok": true, "present": false, "error": e });
            }
        };
        let payloads = pm::list(&h);
        let autoload = pm::autoload_get(&h);
        serde_json::json!({
            "ok": true,
            "present": true,
            "version": version,
            "payloads": payloads.as_ref().ok(),
            "payloads_error": payloads.as_ref().err(),
            "autoload": autoload.as_ref().ok(),
            "autoload_error": autoload.as_ref().err(),
        })
    })
    .await;
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => bad(format!("{e}")),
    }
}

#[derive(Deserialize)]
pub(crate) struct LaunchReq {
    #[serde(alias = "console", alias = "addr")]
    host: String,
    path: String,
}

/// POST /api/ps5/pldmgr/launch `{host, path}` — launch a listed payload.
pub(crate) async fn launch(
    State(_s): State<crate::AppState>,
    Json(req): Json<LaunchReq>,
) -> Response {
    let h = host(&req.host);
    let path = req.path;
    crate::log_info!("pldmgr launch: host={h} path={path}");
    match tokio::task::spawn_blocking(move || pm::launch(&h, &path)).await {
        Ok(Ok(())) => Json(serde_json::json!({ "ok": true })).into_response(),
        Ok(Err(e)) => bad(e),
        Err(e) => bad(format!("{e}")),
    }
}

#[derive(Deserialize)]
pub(crate) struct DeleteReq {
    #[serde(alias = "console", alias = "addr")]
    host: String,
    filename: String,
}

/// POST /api/ps5/pldmgr/delete `{host, filename}` — remove a stored payload and its sidecar.
pub(crate) async fn delete(
    State(_s): State<crate::AppState>,
    Json(req): Json<DeleteReq>,
) -> Response {
    let h = host(&req.host);
    let name = req.filename;
    crate::log_info!("pldmgr delete: host={h} filename={name}");
    match tokio::task::spawn_blocking(move || pm::delete(&h, &name)).await {
        Ok(Ok(())) => Json(serde_json::json!({ "ok": true })).into_response(),
        Ok(Err(e)) => bad(e),
        Err(e) => bad(format!("{e}")),
    }
}

#[derive(Deserialize)]
pub(crate) struct AutoloadReq {
    #[serde(alias = "console", alias = "addr")]
    host: String,
    enabled: bool,
    list: Vec<String>,
}

/// POST /api/ps5/pldmgr/autoload `{host, enabled, list}` — write the boot autoload
/// switch and ordered list, then read it back: `{ok:true, autoload}` with what Payload
/// Manager now holds, or an error when it did not keep what was written.
pub(crate) async fn autoload(
    State(_s): State<crate::AppState>,
    Json(req): Json<AutoloadReq>,
) -> Response {
    let h = host(&req.host);
    crate::log_info!(
        "pldmgr autoload: host={h} enabled={} list=[{}]",
        req.enabled,
        req.list.join(",")
    );
    let r = tokio::task::spawn_blocking(move || {
        let before = pm::autoload_get(&h);
        match &before {
            Ok(a) => crate::log_info!(
                "pldmgr autoload: host={h} was enabled={} list=[{}]",
                a.enabled,
                a.list.join(",")
            ),
            Err(e) => {
                crate::log_warn!("pldmgr autoload: host={h} could not read the list first: {e}")
            }
        }
        let back = pm::autoload_set_checked(&h, req.enabled, &req.list);
        match &back {
            Ok(a) => crate::log_info!(
                "pldmgr autoload: host={h} read back enabled={} list=[{}]",
                a.enabled,
                a.list.join(",")
            ),
            Err(e) => crate::log_warn!("pldmgr autoload: host={h} {e}"),
        }
        back
    })
    .await;
    match r {
        Ok(Ok(a)) => Json(serde_json::json!({ "ok": true, "autoload": a })).into_response(),
        Ok(Err(e)) => bad(e),
        Err(e) => bad(format!("{e}")),
    }
}

#[derive(Deserialize)]
pub(crate) struct StoreReq {
    #[serde(alias = "console", alias = "addr")]
    host: String,
    /// The name Payload Manager files it under (its folder is derived from this).
    filename: String,
    /// A file on this computer (the catalogue's cached ELF).
    local_path: String,
    /// The caller saw `check`'s answer and accepts that storing into an existing folder
    /// removes that folder's other files.
    #[serde(default)]
    confirmed: bool,
}

/// POST /api/ps5/pldmgr/store `{host, filename, local_path, confirmed}` — store a local
/// file in Payload Manager under `filename`. Without `confirmed`, a folder that already
/// exists answers `{ok:false, needs_confirm:true, folder, file_exists}` and nothing is sent.
pub(crate) async fn store(
    State(_s): State<crate::AppState>,
    Json(req): Json<StoreReq>,
) -> Response {
    let h = host(&req.host);
    crate::log_info!(
        "pldmgr store: host={h} filename={} from={} confirmed={}",
        req.filename,
        req.local_path,
        req.confirmed
    );
    let r = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        let check = pm::check(&h, &req.filename)?;
        if check.folder_exists && !req.confirmed {
            return Ok(serde_json::json!({
                "ok": false,
                "needs_confirm": true,
                "folder": check.folder,
                "file_exists": check.file_exists,
            }));
        }
        let bytes =
            std::fs::read(&req.local_path).map_err(|e| format!("read {}: {e}", req.local_path))?;
        pm::store(&h, &req.filename, &bytes)?;
        Ok(serde_json::json!({ "ok": true, "folder": check.folder, "bytes": bytes.len() }))
    })
    .await;
    match r {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => bad(e),
        Err(e) => bad(format!("{e}")),
    }
}

#[derive(Deserialize)]
pub(crate) struct UploadParams {
    #[serde(alias = "console", alias = "addr")]
    host: String,
    filename: String,
    #[serde(default)]
    confirmed: bool,
}

/// Payload Manager stores payloads by file name; only an ELF or a raw `.bin` payload.
fn payload_file_ok(name: &str, bytes: &[u8]) -> Result<(), String> {
    let lower = name.to_ascii_lowercase();
    if !(lower.ends_with(".elf") || lower.ends_with(".bin")) {
        return Err(format!(
            "{name}: Payload Manager keeps .elf and .bin payloads"
        ));
    }
    if lower.ends_with(".elf") && !bytes.starts_with(b"\x7fELF") {
        return Err(format!("{name} is not an ELF file"));
    }
    if bytes.is_empty() {
        return Err(format!("{name} is empty"));
    }
    Ok(())
}

/// POST /api/ps5/pldmgr/upload?host=&filename=&confirmed= with the payload as the raw body:
/// [`store`] for a file the caller holds (a browser picking a local ELF has no path the engine
/// can read). Same confirm rule: storing into an existing folder answers `needs_confirm` first.
pub(crate) async fn upload(
    State(_s): State<crate::AppState>,
    Query(q): Query<UploadParams>,
    body: axum::body::Bytes,
) -> Response {
    let h = host(&q.host);
    crate::log_info!(
        "pldmgr upload: host={h} filename={} bytes={} confirmed={}",
        q.filename,
        body.len(),
        q.confirmed
    );
    if let Err(e) = payload_file_ok(&q.filename, &body) {
        return crate::json_err(axum::http::StatusCode::BAD_REQUEST, e).into_response();
    }
    let r = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        let check = pm::check(&h, &q.filename)?;
        if check.folder_exists && !q.confirmed {
            return Ok(serde_json::json!({
                "ok": false,
                "needs_confirm": true,
                "folder": check.folder,
                "file_exists": check.file_exists,
            }));
        }
        pm::store(&h, &q.filename, &body)?;
        Ok(serde_json::json!({ "ok": true, "folder": check.folder, "bytes": body.len() }))
    })
    .await;
    match r {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => bad(e),
        Err(e) => bad(format!("{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::payload_file_ok;

    #[test]
    fn only_elf_and_bin_payloads_are_stored() {
        assert!(payload_file_ok("a.elf", b"\x7fELF\x02").is_ok());
        assert!(payload_file_ok("A.BIN", b"\x01\x02").is_ok());
        assert!(payload_file_ok("a.elf", b"MZ").is_err());
        assert!(payload_file_ok("a.txt", b"\x7fELF").is_err());
        assert!(payload_file_ok("a.bin", b"").is_err());
    }
}
