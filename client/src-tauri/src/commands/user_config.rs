//! User-facing settings mirror at `~/.ps5upload/settings.json`.
//!
//! Rationale: Tauri's `app_data_dir()` is the conventional place to store
//! app preferences — on macOS that's `~/Library/Application Support/...`,
//! which is opaque to most users and hard to share/diff. The renderer
//! already keeps a fast localStorage cache for first-paint-critical
//! values (theme, language). This module adds a human-inspectable
//! mirror in the user's home directory so the settings can be viewed,
//! backed up, or hand-edited without digging into platform-specific
//! caches.
//!
//! Path: `$HOME/.ps5upload/settings.json` (or `%USERPROFILE%\.ps5upload\…`
//! on Windows). The directory is created on first write.
//!
//! The payload is opaque `serde_json::Value` — the schema is owned by
//! the renderer, which keeps the Rust side small and stable across
//! setting-shape changes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tauri::{AppHandle, Manager};

type JsonValue = serde_json::Value;

/// Monotonic counter for unique tmp-file suffixes. The renderer can fire
/// `user_config_save` concurrently — one from the debounced subscriber
/// flushing a user toggle, one from `persistNow` on first-launch
/// hydration — and a shared tmp path would race: both write, first
/// renames successfully, second finds tmp gone and fails with ENOENT.
/// This counter gives each in-flight save its own tmp name.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Resolve the home-directory settings path. Tauri's `home_dir()` is
/// platform-aware (`$HOME` on Unix, `%USERPROFILE%` on Windows) so we
/// don't have to build our own fallback chain.
fn user_config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let home = app
        .path()
        .home_dir()
        .map_err(|e| format!("cannot resolve home dir: {e}"))?;
    Ok(home.join(".ps5upload").join("settings.json"))
}

fn ensure_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
    }
    Ok(())
}

/// Return the absolute path the mirror writes to. Handy for the UI to
/// show in the Settings screen so users know where to look.
#[tauri::command]
pub async fn user_config_path_resolved(app: AppHandle) -> Result<String, String> {
    Ok(user_config_path(&app)?.to_string_lossy().into_owned())
}

/// Read the settings JSON from `~/.ps5upload/settings.json`. Returns
/// `null` (JSON) if the file doesn't exist — so the renderer can treat
/// "missing" and "empty" identically without an error-handling branch.
#[tauri::command]
pub async fn user_config_load(app: AppHandle) -> Result<JsonValue, String> {
    let path = user_config_path(&app)?;
    match std::fs::read(&path) {
        Ok(bytes) => {
            if bytes.is_empty() {
                return Ok(JsonValue::Null);
            }
            serde_json::from_slice(&bytes).map_err(|e| format!("parse {path:?}: {e}"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(JsonValue::Null),
        Err(e) => Err(format!("read {path:?}: {e}")),
    }
}

/// Write settings to `~/.ps5upload/settings.json`. Atomic write (tmp +
/// rename) so a crash mid-write doesn't corrupt the file. Directory is
/// created on demand. Each call uses a unique tmp name so concurrent
/// saves from the renderer don't race on a shared tmp path.
#[tauri::command]
pub async fn user_config_save(app: AppHandle, config: JsonValue) -> Result<(), String> {
    let path = user_config_path(&app)?;
    ensure_parent(&path)?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.tmp.{seq}"));
    let bytes = serde_json::to_vec_pretty(&config).map_err(|e| format!("serialize: {e}"))?;
    // Write + fsync before rename: on Linux ext4 the rename can land
    // on disk before the data write, leaving a zero-byte
    // settings.json after a crash. Matches the same pattern in
    // persistence.rs::write_json_atomic.
    //
    // Tmp cleanup on every failure path: previously we used `?`
    // propagation on write_all/sync_all, which left orphan
    // settings.json.tmp.N files behind on every failed save (ENOSPC,
    // permission, etc.). Over time these accumulate. Each failure
    // path now removes the tmp before returning the error. Matches
    // persistence.rs's explicit cleanup.
    {
        use std::io::Write;
        let mut f = match std::fs::File::create(&tmp) {
            Ok(f) => f,
            Err(e) => return Err(format!("create {tmp:?}: {e}")),
        };
        if let Err(e) = f.write_all(&bytes) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("write {tmp:?}: {e}"));
        }
        if let Err(e) = f.sync_all() {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("fsync {tmp:?}: {e}"));
        }
    }
    if let Err(e) = super::replace_file(&tmp, &path) {
        // Best-effort cleanup: if rename failed the tmp is still on
        // disk. Leaving it would accumulate `settings.json.tmp.N` files
        // over time across retries.
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("rename {tmp:?} -> {path:?}: {e}"));
    }
    // Sync the parent directory after the rename so the directory
    // entry update is persisted alongside the file contents. Without
    // this, on macOS APFS / Linux ext4-without-journal-data, a power
    // loss between the rename(2) and the next directory sync can lose
    // the rename (and revert to the previous settings.json). Sony's
    // BGFT install spool / payload's manifest spool use the same
    // pattern. fsync on a directory fd is a noop on Windows but the
    // tauri build target supports it on every Unix host the user runs.
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}
