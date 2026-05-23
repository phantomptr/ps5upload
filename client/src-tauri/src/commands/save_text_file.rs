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
pub async fn save_text_file(path: String, contents: String) -> Result<(), String> {
    if path.is_empty() {
        return Err("save_text_file: empty destination path".into());
    }
    // std::fs::write is one open+write+close; on a slow/networked disk
    // that's enough to stutter the UI thread, so route it through the
    // blocking pool like the rest of the file-save commands.
    tokio::task::spawn_blocking(move || {
        let dest = Path::new(&path);
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
