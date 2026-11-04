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

#[cfg(test)]
#[path = "user_config_tests.rs"]
mod tests;

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
    // Mobile (Android/iOS): `home_dir()` resolves to external storage
    // (e.g. /storage/emulated/0), where writing a dotfile is denied under
    // scoped storage (the "mkdir .ps5upload: Permission denied" we hit on
    // first launch). Use the app-private config dir instead — always
    // writable, no permission prompt, and not world-readable.
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let dir = app
            .path()
            .app_config_dir()
            .map_err(|e| format!("cannot resolve app config dir: {e}"))?;
        #[allow(clippy::needless_return)] // the desktop block below follows on other targets
        return Ok(dir.join("settings.json"));
    }
    // Desktop: the user-facing mirror lives at `~/.ps5upload/settings.json`.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let home = app
            .path()
            .home_dir()
            .map_err(|e| format!("cannot resolve home dir: {e}"))?;
        Ok(home.join(".ps5upload").join("settings.json"))
    }
}

fn ensure_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
    }
    Ok(())
}

/// Read the `engine_url` field from settings.json, if present and non-blank.
/// Sync (no `await`) so `engine::start` can seed its URL before deciding
/// whether to spawn the bundled sidecar.
///
/// Desktop-only: its sole caller is the desktop `engine::start` (engine.rs).
/// The mobile build links the engine in-process (engine_mobile.rs) and has no
/// configurable URL, so without this `cfg` the function is dead code on the
/// Android/iOS target and trips `-D warnings`.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub(crate) fn load_engine_url(app: &AppHandle) -> Option<String> {
    let path = user_config_path(app).ok()?;
    engine_url_in(&std::fs::read(&path).ok()?)
}

/// The `engine_url` of a settings file's bytes; a damaged file has none.
#[cfg(any(not(any(target_os = "android", target_os = "ios")), test))]
fn engine_url_in(bytes: &[u8]) -> Option<String> {
    let json: JsonValue = serde_json::from_slice(bytes).ok()?;
    let url = json.get("engine_url")?.as_str()?.trim();
    (!url.is_empty()).then(|| url.to_string())
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
    load_from(&user_config_path(&app)?)
}

fn load_from(path: &Path) -> Result<JsonValue, String> {
    match std::fs::read(path) {
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
    save_to(&user_config_path(&app)?, &config)
}

fn save_to(path: &Path, config: &JsonValue) -> Result<(), String> {
    ensure_parent(path)?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.tmp.{seq}"));
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| format!("serialize: {e}"))?;
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
    if let Err(e) = super::replace_file(&tmp, path) {
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

/// Resolve the config BASE dir (`~/.ps5upload` on desktop, app_config_dir on
/// mobile) — the parent that holds settings.json, crash-reports/, and any
/// legacy config files.
fn config_base_dir(app: &AppHandle) -> Result<PathBuf, String> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        app.path()
            .app_config_dir()
            .map_err(|e| format!("cannot resolve app config dir: {e}"))
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let home = app
            .path()
            .home_dir()
            .map_err(|e| format!("cannot resolve home dir: {e}"))?;
        Ok(home.join(".ps5upload"))
    }
}

/// Factory-reset: delete ALL local ps5upload data + metadata so the next
/// launch starts from defaults. Wipes:
///   - the config base dir (`~/.ps5upload`): settings.json, crash-reports/,
///     the engine's cached cover art under `cache/icons/`, and any legacy
///     `*.ini` / profile files — the whole dir is exclusively
///     ours, so it's removed wholesale and recreated empty.
///   - app_data stores (resume_txids.json, send_payload_history.json,
///     upload_queue.json, payload_playlists.json) — removed surgically so we
///     never touch the WebView's own storage that can also live under
///     app_data_dir.
///   - the downloaded-payload cache and the user's custom repos under
///     app_local_data_dir/payloads.
///
/// The engine sidecar keeps its connections and identity in memory and writes
/// them back under the config base dir, so it is stopped before the wipe and
/// started again afterwards (a remote engine is left alone). On a phone the engine
/// runs inside the app and cannot be stopped: its folder is wiped as well, and whatever
/// it writes back from memory goes on the next launch (`mark_engine_data_for_reset`), so
/// the renderer has the app close to finish. The renderer owns
/// clearing its localStorage + reloading the window; this command only touches
/// the filesystem. Returns a count of items removed.
#[tauri::command]
pub async fn app_data_reset(app: AppHandle) -> Result<usize, String> {
    // 0. Stop the sidecar so it cannot recreate what is removed below.
    crate::engine::stop().await;

    // 1-3. A failed step is returned only after the restart below: an early
    //      return left the app with no engine until it was relaunched.
    let wiped = wipe_app_data(&app);

    // 4. Bring the sidecar back so the reloaded window finds an engine. A
    //    failure here is logged, not returned: the data is gone, and the
    //    renderer's status polling reports an unreachable engine on its own.
    match crate::engine::start(&app).await {
        Ok(url) => {
            use tauri::Emitter;
            let _ = app.emit("ps5upload-engine-ready", &url);
        }
        Err(e) => {
            // Not eprintln!: it panics when the parent's stderr is gone.
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr(),
                "[tauri] engine restart after data reset failed: {e:#}"
            );
        }
    }

    wiped
}

/// Steps 1-3 of [`app_data_reset`]: the filesystem wipe, with the engine stopped.
fn wipe_app_data(app: &AppHandle) -> Result<usize, String> {
    let mut removed = 0usize;

    // 1. Config base dir — remove wholesale, then recreate empty so later
    //    writes don't fail on a missing parent.
    if let Ok(base) = config_base_dir(app) {
        if base.exists() {
            std::fs::remove_dir_all(&base).map_err(|e| format!("remove {base:?}: {e}"))?;
            removed += 1;
        }
        let _ = std::fs::create_dir_all(&base);
    }

    // Mobile: the in-process engine's folder (saved consoles, pairings, its identity).
    #[cfg(any(target_os = "android", target_os = "ios"))]
    if let Ok(data) = app.path().app_data_dir() {
        let engine_data = std::env::var_os("PS5UPLOAD_DATA_DIR")
            .filter(|v| !v.is_empty())
            .map_or_else(|| data.join("ps5upload"), PathBuf::from);
        mark_engine_data_for_reset(&data, &engine_data)
            .map_err(|e| format!("reset {engine_data:?}: {e}"))?;
        removed += 1;
    }

    // 2. app_data stores — surgical (NOT the whole app_data_dir).
    let mut targets: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(data) = app.path().app_data_dir() {
        for entry in [
            "resume_txids.json",
            "send_payload_history.json",
            "upload_queue.json",
            "payload_playlists.json",
            "payloads",
        ] {
            targets.push(data.join(entry));
        }
    }
    // 3. The payload cache and custom repos (app_local_data_dir/payloads).
    if let Ok(local) = app.path().app_local_data_dir() {
        targets.push(local.join("payloads"));
    }
    for p in targets {
        if !p.exists() {
            continue;
        }
        let r = if p.is_dir() {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        };
        r.map_err(|e| format!("remove {p:?}: {e}"))?;
        removed += 1;
    }

    Ok(removed)
}

/// Beside the app's data: the engine's folder is to be emptied before the next start.
#[cfg(any(target_os = "android", target_os = "ios", test))]
const ENGINE_RESET_MARKER: &str = "engine-data-reset-pending";

/// Reset of an engine that is running in this process: empty its folder now, so nothing of it is
/// left whatever happens next, and leave a marker first, because the running engine still holds
/// its data in memory and may write some of it back before the app ends.
#[cfg(any(target_os = "android", target_os = "ios", test))]
fn mark_engine_data_for_reset(data_dir: &Path, engine_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(data_dir.join(ENGINE_RESET_MARKER), b"")?;
    if engine_dir.exists() {
        std::fs::remove_dir_all(engine_dir)?;
    }
    // Empty but there: the running engine's next write must not fail on a missing parent.
    std::fs::create_dir_all(engine_dir)
}

/// Before the engine starts (`engine_mobile.rs`): finish a reset the last run marked.
#[cfg(any(target_os = "android", target_os = "ios", test))]
pub(crate) fn finish_engine_data_reset(data_dir: &Path, engine_dir: &Path) {
    let marker = data_dir.join(ENGINE_RESET_MARKER);
    if !marker.exists() {
        return;
    }
    // The marker goes only once the folder has: a failure is tried again on the next launch.
    if !engine_dir.exists() || std::fs::remove_dir_all(engine_dir).is_ok() {
        let _ = std::fs::remove_file(marker);
    }
}

/// Phone apps: end the process so the reset finishes on the next launch (see `app_data_reset`).
/// The engine shares the process, and leaving the app does not always end that. Does nothing on
/// desktop, whose reset restarts the engine by itself.
#[tauri::command]
pub async fn app_close_after_reset() {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    std::process::exit(0);
}

#[cfg(test)]
mod reset_tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ps5upload-reset-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_phone_reset_empties_the_engine_folder_and_what_it_writes_back_goes_next_launch() {
        // F7: saved consoles, pairings and the identity survived "Reset all data" on a phone.
        let data = scratch("phone");
        let engine = data.join("ps5upload");
        std::fs::create_dir_all(engine.join("cache")).unwrap();
        std::fs::write(engine.join("connections.json"), b"saved").unwrap();

        mark_engine_data_for_reset(&data, &engine).unwrap();
        assert!(engine.is_dir(), "there for the running engine");
        assert_eq!(std::fs::read_dir(&engine).unwrap().count(), 0, "emptied");

        // The running engine still has it all in memory.
        std::fs::write(engine.join("connections.json"), b"written back").unwrap();
        finish_engine_data_reset(&data, &engine);
        assert!(!engine.join("connections.json").exists());
        assert!(!data.join(ENGINE_RESET_MARKER).exists(), "done");

        // A normal launch after that touches nothing.
        std::fs::create_dir_all(&engine).unwrap();
        std::fs::write(engine.join("connections.json"), b"new").unwrap();
        finish_engine_data_reset(&data, &engine);
        assert!(engine.join("connections.json").exists());
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn a_reset_of_an_engine_that_never_wrote_anything_still_works() {
        let data = scratch("empty");
        let engine = data.join("ps5upload");
        mark_engine_data_for_reset(&data, &engine).unwrap();
        finish_engine_data_reset(&data, &engine);
        assert!(!data.join(ENGINE_RESET_MARKER).exists());
        let _ = std::fs::remove_dir_all(&data);
    }
}
