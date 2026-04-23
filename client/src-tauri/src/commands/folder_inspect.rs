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

use std::path::{Path, PathBuf};

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
    tokio::task::spawn_blocking(move || {
        match ps5upload_core::game_meta::inspect_folder(&p) {
            Ok(r) => {
                let needs_hint = r.meta_source == "none";
                let hint = if needs_hint { wrapped_game_hint(&p) } else { None };
                serde_json::json!({
                    "ok": true,
                    "result": r,
                    "wrapped_hint": hint,
                })
            }
            Err(e) => serde_json::json!({ "ok": false, "error": format!("{e:#}") }),
        }
    })
    .await
    .unwrap_or_else(|e| serde_json::json!({ "ok": false, "error": format!("join: {e}") }))
}

/// If `root` has exactly one subdirectory (ignoring hidden entries)
/// and that subdirectory parses as a game, return a lightweight hint
/// so the UI can suggest "did you mean `<child>`?". We deliberately
/// do NOT recurse further — wrapped-twice is almost certainly a
/// user-packing mistake and auto-descending hides what's uploaded.
fn wrapped_game_hint(root: &Path) -> Option<serde_json::Value> {
    let rd = std::fs::read_dir(root).ok()?;
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }
        if entry.file_type().ok()?.is_dir() {
            subdirs.push(entry.path());
            if subdirs.len() > 1 {
                return None;
            }
        }
    }
    let child = subdirs.into_iter().next()?;
    let meta = ps5upload_core::game_meta::inspect_folder(&child).ok()?;
    if meta.meta_source == "none" {
        return None;
    }
    Some(serde_json::json!({
        "path": child.to_string_lossy(),
        "title": meta.title,
        "title_id": meta.title_id,
    }))
}
