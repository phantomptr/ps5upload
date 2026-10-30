//! Read/write text at a user-chosen absolute path.
//!
//! Why these exist as custom commands instead of `@tauri-apps/plugin-fs`
//! `writeTextFile` / `readTextFile`: the fs plugin enforces *path*
//! scopes, not just command permissions. Our renderer features
//! (bug-report bundle, settings export/import, search/stats exports) let
//! the user pick the file through the native save/open dialog, which
//! returns an arbitrary absolute path (e.g.
//! `~/Downloads/ps5upload-bug-report.json`) that's outside any default
//! fs scope — so the plugin rejected the write with "fs.write_text_file
//! not allowed" (and the read side fails identically: opening a file via
//! the dialog plugin does NOT grant the fs plugin read access to it).
//!
//! Widening the fs scope to `$HOME/**` would have fixed it, but that
//! grants the *web renderer* broad disk rights. Routing through backend
//! commands keeps the actual IO in trusted Rust and matches how
//! `save_archive.rs::save_archive_zip` already writes user-picked `.zip`
//! paths (unrestricted `File::create`). The path always originates from
//! the OS dialog the user just interacted with, so it carries explicit
//! user intent — we trust it the same way the zip-save path does.

use std::path::Path;

/// Cap for `read_text_file` so a user pointing the import dialog at a
/// huge/binary file can't balloon renderer memory. The settings/bug
/// bundles this reads back are tens of KiB; 16 MiB is generous headroom
/// while still bounding the worst case.
const READ_TEXT_FILE_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Write `contents` to `path` (an absolute path the user picked via the
/// save dialog). On a mid-write failure the partial file is removed so
/// the user never finds a truncated artifact at their chosen path —
/// same belt-and-suspenders cleanup `save_archive_zip` does on ENOSPC.
#[tauri::command]
pub async fn save_text_file(
    path: String,
    contents: String,
    // The filename the renderer offered in the save dialog (e.g.
    // `ps5upload-settings-<ms>.json`). Needed on Android, where `path` is a
    // content:// SAF URI with no usable name: without it every export
    // (settings/search/stats/logs) collides on one fixed Downloads filename
    // and overwrites the others. Optional; desktop ignores it.
    dest_filename: Option<String>,
) -> Result<(), String> {
    // std::fs::write is one open+write+close; on a slow/networked disk
    // that's enough to stutter the UI thread, so route it through the
    // blocking pool like the rest of the file-save commands.
    tokio::task::spawn_blocking(move || {
        // Android save dialogs return content:// URIs std::fs can't write to;
        // redirect those to a real path under Downloads (desktop pass-through),
        // using the renderer's chosen filename so distinct exports stay distinct.
        let fallback = dest_filename
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("ps5upload-export.txt");
        let dest_buf = super::save_dest::resolve_save_dest(&path, fallback)?;
        let dest = dest_buf.as_path();
        match std::fs::write(dest, contents.as_bytes()) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Best-effort: drop a half-written file so a failed
                // export doesn't masquerade as a valid one. If the
                // unlink itself fails, the original write error is
                // still what we surface.
                let _ = std::fs::remove_file(dest);
                Err(format!("write {}: {e}", dest.display()))
            }
        }
    })
    .await
    .map_err(|e| format!("save_text_file join: {e}"))?
}

/// Read UTF-8 text from `path` (an absolute path the user picked via the
/// open dialog). Rejects files larger than `READ_TEXT_FILE_MAX_BYTES`
/// before reading them so a mis-picked multi-GB file can't OOM the
/// renderer. Backs the settings-bundle *import*, the read mirror of
/// `save_text_file`.
#[tauri::command]
pub async fn read_text_file(path: String) -> Result<String, String> {
    if path.is_empty() {
        return Err("read_text_file: empty source path".into());
    }
    tokio::task::spawn_blocking(move || {
        let src = Path::new(&path);
        let meta = std::fs::metadata(src).map_err(|e| format!("stat {}: {e}", src.display()))?;
        if meta.len() > READ_TEXT_FILE_MAX_BYTES {
            return Err(format!(
                "file too large to import ({} bytes; limit {} bytes)",
                meta.len(),
                READ_TEXT_FILE_MAX_BYTES
            ));
        }
        std::fs::read_to_string(src).map_err(|e| format!("read {}: {e}", src.display()))
    })
    .await
    .map_err(|e| format!("read_text_file join: {e}"))?
}

/// Write bytes (a Remote Play screenshot or clip) to an absolute path the
/// renderer chose in a downloads folder (`pickSaveDest.ts::downloadsDest`).
/// The bytes come as the raw IPC body, not as JSON or base64: a clip can be
/// hundreds of MB. The path and the file name ride in the `x-path` and
/// `x-name` headers, percent-encoded (a header is ASCII; a path may not be).
/// Returns where the file landed (on Android a `content://` request is
/// redirected into Downloads, as for `save_text_file`).
#[tauri::command]
pub async fn save_binary_file(request: tauri::ipc::Request<'_>) -> Result<String, String> {
    let header = |k: &str| -> String {
        request
            .headers()
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(percent_decode)
            .unwrap_or_default()
    };
    let path = header("x-path");
    let name = header("x-name");
    let bytes = body_bytes(request.body())?;
    tokio::task::spawn_blocking(move || {
        let fallback = if name.trim().is_empty() {
            "ps5upload-capture.bin"
        } else {
            name.as_str()
        };
        let dest = super::save_dest::resolve_save_dest(&path, fallback)?;
        match std::fs::write(&dest, &bytes) {
            Ok(()) => Ok(dest.display().to_string()),
            Err(e) => {
                let _ = std::fs::remove_file(&dest);
                Err(format!("write {}: {e}", dest.display()))
            }
        }
    })
    .await
    .map_err(|e| format!("save_binary_file join: {e}"))?
}

/// The bytes of an IPC body: raw where the webview sends raw bodies, a JSON
/// array of numbers where it falls back to JSON messages.
pub(super) fn body_bytes(body: &tauri::ipc::InvokeBody) -> Result<Vec<u8>, String> {
    match body {
        tauri::ipc::InvokeBody::Raw(v) => Ok(v.clone()),
        tauri::ipc::InvokeBody::Json(serde_json::Value::Array(a)) => a
            .iter()
            .map(|n| {
                n.as_u64()
                    .filter(|&b| b <= 255)
                    .map(|b| b as u8)
                    .ok_or_else(|| "save_binary_file: body is not bytes".to_string())
            })
            .collect(),
        _ => Err("save_binary_file: no bytes in the request".into()),
    }
}

/// `%XX` escapes back to bytes, read as UTF-8 (what `encodeURIComponent` wrote).
pub(super) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod binary_tests {
    use super::*;

    #[test]
    fn percent_decoding_undoes_encode_uri_component() {
        assert_eq!(
            percent_decode("C%3A%5CUsers%5C%E5%BC%A0%5CDownloads%2FPS5%20a.png"),
            "C:\\Users\\张\\Downloads/PS5 a.png"
        );
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn body_bytes_takes_raw_or_a_json_byte_array() {
        let raw = tauri::ipc::InvokeBody::Raw(vec![1, 2, 255]);
        assert_eq!(body_bytes(&raw).unwrap(), vec![1, 2, 255]);
        let json = tauri::ipc::InvokeBody::Json(serde_json::json!([0, 137, 80]));
        assert_eq!(body_bytes(&json).unwrap(), vec![0, 137, 80]);
        let bad = tauri::ipc::InvokeBody::Json(serde_json::json!([256]));
        assert!(body_bytes(&bad).is_err());
    }
}
