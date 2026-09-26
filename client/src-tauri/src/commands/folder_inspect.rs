//! Tauri command: walk a local folder, parse `sce_sys/param.json`, and
//! return a preview payload for the Upload screen's pre-flight card.
//!
//! Wraps `ps5upload_core::game_meta::inspect_folder` so the engine HTTP
//! hop is skipped — folder inspection is a host-local operation that
//! only needs filesystem access.
//!
//! On top of the core inspect we surface a `wrapped_hint` when the
//! chosen folder is *not* itself a game but contains exactly one
//! subdirectory that is. This lets the Upload screen suggest the
//! wrapped child without silently descending into it — matches the
//! "root-only detection, but tell the user" UX decision.

use std::path::PathBuf;

/// Classify a host path so the renderer can route drag-drop into the
/// right picker branch (file vs folder). Keeps the fs-plugin scope out
/// of the renderer — we only need this for the drop handler and using
/// a dedicated command avoids widening `fs:default`.
#[tauri::command]
pub async fn path_kind(path: String) -> serde_json::Value {
    let p = PathBuf::from(&path);
    match tokio::fs::metadata(&p).await {
        Ok(md) if md.is_dir() => serde_json::json!({ "kind": "folder" }),
        Ok(md) if md.is_file() => serde_json::json!({ "kind": "file" }),
        Ok(_) => serde_json::json!({ "kind": "other" }),
        Err(_) => serde_json::json!({ "kind": "missing" }),
    }
}

#[tauri::command]
pub async fn inspect_folder(path: String) -> serde_json::Value {
    let p = PathBuf::from(&path);
    tokio::task::spawn_blocking(
        move || match ps5upload_core::game_meta::inspect_folder(&p) {
            Ok(r) => {
                let needs_hint = r.meta_source == "none";
                // Shared with the engine's /api/local/inspect-folder so
                // desktop and browser give the same answer.
                let hint = if needs_hint {
                    ps5upload_core::game_meta::wrapped_game_hint(&p)
                } else {
                    None
                };
                serde_json::json!({
                    "ok": true,
                    "result": r,
                    "wrapped_hint": hint,
                })
            }
            Err(e) => serde_json::json!({ "ok": false, "error": format!("{e:#}") }),
        },
    )
    .await
    .unwrap_or_else(|e| serde_json::json!({ "ok": false, "error": format!("join: {e}") }))
}
