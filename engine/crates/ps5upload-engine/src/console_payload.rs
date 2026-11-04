//! Load a payload that already sits on the console (File system → Load payload): read it back
//! through the helper, then send it to the console's ELF loader on :9021 as a companion image,
//! under the same per-console loader lock and refused-connect retry the install's daemon send
//! uses. The console page does the same on the console itself (payload/src/webui_pkg.c).
use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

use crate::pkg_install::PkgInstallStateHandle;

/// The helper marker every ps5upload helper image carries (its AVA1 trust slot): sending one
/// from here would replace the running helper behind the app's back (Connection does that).
const HELPER_MARKER: &[u8] = b"AVA1TRUST";
/// The loader's ceiling (payload_lifecycle's ELF_SEND_MAX_BYTES).
const PAYLOAD_MAX: u64 = 64 * 1024 * 1024;
const READ_CHUNK: u64 = 2 * 1024 * 1024;
const LOADER_RETRY: Duration = Duration::from_secs(8);
const LOADER_POLL: Duration = Duration::from_millis(250);

#[derive(Deserialize)]
pub struct LoadRequest {
    #[serde(alias = "console", alias = "addr")]
    host: String,
    path: String,
}

/// A file the loader is offered: `.elf` or `.bin` (any case), like the Payloads screen's :9021 rule.
pub fn is_payload_name(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.ends_with(".elf") || p.ends_with(".bin")
}

/// Whether `bytes` is a ps5upload helper image.
pub fn is_helper(bytes: &[u8]) -> bool {
    bytes
        .windows(HELPER_MARKER.len())
        .any(|w| w == HELPER_MARKER)
}

/// Why `bytes` must not go to the loader, or None.
pub fn refusal(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() < 64 || &bytes[..4] != b"\x7FELF" {
        return Some("payload_not_elf");
    }
    if is_helper(bytes) {
        return Some("payload_is_helper");
    }
    None
}

/// The file, read through the helper in 2 MiB pieces; `payload_too_large` past the cap.
fn read_console_file(mgmt: &str, path: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let piece = ps5upload_core::fs_ops::fs_read(mgmt, path, out.len() as u64, READ_CHUNK)
            .map_err(|e| format!("read {path}: {e}"))?;
        let n = piece.len() as u64;
        out.extend_from_slice(&piece);
        if out.len() as u64 > PAYLOAD_MAX {
            return Err("payload_too_large".into());
        }
        if n < READ_CHUNK {
            return Ok(out);
        }
    }
}

fn err(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

/// POST /api/ps5/payload/load-console {host, path} → {bytes} | {error}.
pub async fn load_handler(
    State(state): State<PkgInstallStateHandle>,
    Json(req): Json<LoadRequest>,
) -> Response {
    let host = crate::install::console_id(req.host.trim());
    let path = req.path.trim().to_string();
    if host.is_empty() || !path.starts_with('/') || !is_payload_name(&path) {
        return err(StatusCode::BAD_REQUEST, "payload_not_payload");
    }
    // A payload sent into an install can take the loader (and the daemon's bring-up) from under it.
    if state.jobs.active_for(&host) {
        return err(StatusCode::CONFLICT, "pkg_install_busy");
    }
    let mgmt = crate::pkg_install::normalize_mgmt_addr(&host);
    let r = tokio::task::spawn_blocking(move || -> Result<u64, String> {
        let bytes = read_console_file(&mgmt, &path)?;
        if let Some(why) = refusal(&bytes) {
            return Err(why.to_string());
        }
        // Wait out an elfldr swap (or an install's daemon send) rather than send into its gap.
        let lock = crate::elfldr_guard::loader_lock(&host);
        let _loader = lock.lock().unwrap_or_else(|e| e.into_inner());
        ps5upload_core::installer_client::send_to_loader_retrying(
            &host,
            ps5upload_core::payload_lifecycle::PS5_LOADER_PORT,
            &bytes,
            LOADER_RETRY,
            LOADER_POLL,
        )
        .map_err(|e| {
            if e.starts_with("connect ") {
                "loader_unreachable".to_string()
            } else {
                format!("loader_send_failed: {e}")
            }
        })
    })
    .await;
    match r {
        Ok(Ok(bytes)) => {
            crate::log_info!("payload load-console: sent {bytes} bytes");
            (StatusCode::OK, Json(json!({ "bytes": bytes }))).into_response()
        }
        Ok(Err(e)) => {
            let status = match e.as_str() {
                "payload_not_elf" => StatusCode::BAD_REQUEST,
                "payload_too_large" => StatusCode::PAYLOAD_TOO_LARGE,
                "payload_is_helper" => StatusCode::CONFLICT,
                "loader_unreachable" => StatusCode::BAD_GATEWAY,
                _ => StatusCode::BAD_GATEWAY,
            };
            crate::log_warn!("payload load-console: {e}");
            (status, Json(json!({ "error": e }))).into_response()
        }
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("task failed: {e}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_names() {
        assert!(is_payload_name("/data/kstuff.ELF"));
        assert!(is_payload_name("/mnt/usb0/x.bin"));
        assert!(!is_payload_name("/data/x.elf.txt"));
        assert!(!is_payload_name("/data/x.pkg"));
    }

    #[test]
    fn refuses_non_elf_and_helpers() {
        let mut elf = b"\x7FELF".to_vec();
        elf.resize(128, 0);
        assert_eq!(refusal(&elf), None);
        assert_eq!(refusal(b"not an elf at all"), Some("payload_not_elf"));
        assert_eq!(refusal(b"\x7FELF"), Some("payload_not_elf"));
        let mut helper = elf.clone();
        helper.extend_from_slice(b"....AVA1TRUST....");
        assert_eq!(refusal(&helper), Some("payload_is_helper"));
    }
}
