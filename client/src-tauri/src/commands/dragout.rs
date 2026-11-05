//! Staging folder for "drag a file out of Files onto the desktop".
//!
//! The OS drag (tauri-plugin-drag) can only carry files that exist on this
//! computer, so the renderer first downloads the PS5 file into a folder under
//! the app's local data and then starts the drag with that path. The folder is
//! per console-and-directory so the file keeps its own name, and old folders
//! are pruned on each use.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use tauri::{AppHandle, Manager};

/// Staged files older than this are removed whenever a new drag is prepared.
const KEEP_FOR: Duration = Duration::from_secs(24 * 60 * 60);

/// Create (or reuse) the staging folder for `key` and return its path. `key`
/// is any string that identifies the console and remote directory; it is
/// reduced to a filesystem-safe name. Off the main thread: pruning can delete a stale game
/// folder of many GB.
#[tauri::command]
pub async fn dragout_stage_dir(app: AppHandle, key: String) -> Result<String, String> {
    let root: PathBuf = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("no app data dir: {e}"))?
        .join("dragout");
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&root).map_err(|e| format!("create {}: {e}", root.display()))?;
        prune(&root);
        let safe: String = key
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let dir = root.join(format!("{safe}_{:08x}", fxhash(&key)));
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        Ok(dir.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("dragout_stage_dir: {e}"))?
}

/// Remove staging folders that have not been touched for `KEEP_FOR`.
fn prune(root: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let p = e.path();
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > KEEP_FOR);
        if old {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

/// A short, stable hash so two keys that sanitise alike still get their own folder.
fn fxhash(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}
