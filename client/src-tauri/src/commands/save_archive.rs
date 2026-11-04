//! Save-data backup/restore as .zip archives: the Tauri commands the Saves screen drives. The
//! file work itself (scratch dirs, zipping, format-aware finalize/prepare, strict unzip, the
//! automatic-backup slots) is in `ps5upload_core::save_archive`, shared with the engine's
//! scheduled save backups.
//!
//! Backup flow (driven from the renderer):
//!   1. `save_archive_make_temp` — create a scratch dir under the system temp root.
//!   2. existing `transfer_download` — pulls the PS5 save folder into the scratch dir as
//!      `<scratch>/<title_id>/<files>` (the engine's download walker prefixes by basename).
//!   3. `save_archive_zip` — zip `<scratch>/<title_id>/` into the user-picked `.zip` path.
//!   4. `save_archive_cleanup_temp` — remove the scratch dir.
//!
//! Restore flow:
//!   1. `save_archive_make_temp` — scratch dir.
//!   2. `save_archive_unzip` — strict-validate the zip's layout, then extract into the scratch dir.
//!   3. existing `transfer_dir` from `<scratch>/<title_id>/` → PS5 path.
//!   4. `save_archive_cleanup_temp`.

use ps5upload_core::save_archive::{
    backup_finalize, cleanup_temp_dir, make_temp_dir, restore_prepare, unzip_strict, zip_folder,
};
pub use ps5upload_core::save_archive::{
    SaveArchiveBackupFinalizeResp, SaveArchiveRestorePrepareResp, SaveArchiveUnzipResp,
};
use serde::Deserialize;
use std::path::Path;

#[tauri::command]
pub async fn save_archive_make_temp(prefix: String) -> Result<String, String> {
    // spawn_blocking keeps FS work off the main thread, like the rest of this file.
    tokio::task::spawn_blocking(move || {
        make_temp_dir(&prefix).map(|d| d.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("save_archive_make_temp join: {e}"))?
}

#[tauri::command]
pub async fn save_archive_cleanup_temp(path: String) -> Result<(), String> {
    // remove_dir_all walks every entry under the scratch dir: off the UI thread.
    tokio::task::spawn_blocking(move || cleanup_temp_dir(&path))
        .await
        .map_err(|e| format!("save_archive_cleanup_temp join: {e}"))?
}

// ── zip a downloaded save folder ────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SaveArchiveZipReq {
    /// Scratch dir that contains `<inner_root>/<files>` (the structure
    /// produced by `transfer_download` against a PS5 save folder).
    pub src_dir: String,
    /// Title ID — must match the folder name under `src_dir`.
    pub inner_root: String,
    /// Absolute path of the `.zip` file to write.
    pub dest_zip: String,
    /// Filename the renderer offered in the save dialog (e.g. `<title_id>.zip`).
    /// On Android `dest_zip` is a content:// SAF URI with no usable name; this
    /// keeps each save backup from colliding on one fixed Downloads filename.
    #[serde(default)]
    pub dest_filename: Option<String>,
}

#[tauri::command]
/// Returns the path the zip was written to: the picked one on desktop, the Downloads file an
/// Android `content://` (or empty) destination was redirected to.
pub async fn save_archive_zip(req: SaveArchiveZipReq) -> Result<String, String> {
    // Android save dialogs return content:// URIs std::fs can't create; redirect
    // those to a real path under Downloads (desktop paths pass through), using
    // the renderer's filename so distinct backups stay distinct.
    let fallback = req
        .dest_filename
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("ps5upload-save-archive.zip");
    let dest_buf = super::save_dest::resolve_save_dest(&req.dest_zip, fallback)?;
    let dest = dest_buf.to_string_lossy().into_owned();
    let result = tokio::task::spawn_blocking(move || {
        zip_folder(Path::new(&req.src_dir), &req.inner_root, dest_buf.as_path())
    })
    .await
    .map_err(|e| format!("zip task: {e}"))?;
    if let Err(e) = result {
        // On failure (typically ENOSPC mid-write), remove the
        // half-written .zip so the user doesn't see a corrupt file
        // at their picked path. Best-effort — if the unlink itself
        // fails, the original error is still what surfaces.
        let _ = std::fs::remove_file(&dest);
        return Err(format!("zip: {e:#}"));
    }
    Ok(dest)
}

#[derive(Debug, Deserialize)]
pub struct SaveArchiveBackupFinalizeReq {
    /// Scratch dir that contains `<title_id>/<files>` from a fresh
    /// `transfer_download`. We mutate the contents in place.
    pub src_dir: String,
    /// Title ID (folder name under `src_dir`).
    pub title_id: String,
}

#[tauri::command]
pub async fn save_archive_backup_finalize(
    req: SaveArchiveBackupFinalizeReq,
) -> Result<SaveArchiveBackupFinalizeResp, String> {
    tokio::task::spawn_blocking(move || {
        backup_finalize(&Path::new(&req.src_dir).join(&req.title_id))
    })
    .await
    .map_err(|e| format!("backup finalize task: {e}"))?
    .map_err(|e| format!("backup finalize: {e:#}"))
}

#[derive(Debug, Deserialize)]
pub struct SaveArchiveRestorePrepareReq {
    pub src_dir: String,
    pub title_id: String,
}

#[tauri::command]
pub async fn save_archive_restore_prepare(
    req: SaveArchiveRestorePrepareReq,
) -> Result<SaveArchiveRestorePrepareResp, String> {
    tokio::task::spawn_blocking(move || {
        restore_prepare(&Path::new(&req.src_dir).join(&req.title_id))
    })
    .await
    .map_err(|e| format!("restore prepare task: {e}"))?
    .map_err(|e| format!("restore prepare: {e:#}"))
}

// ── strict unzip for restore ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SaveArchiveUnzipReq {
    /// Path to the user-picked `.zip`.
    pub zip_path: String,
    /// Empty scratch dir to extract into.
    pub dest_dir: String,
    /// Title ID we expect as the zip's single top-level folder.
    pub expected_inner: String,
}

#[tauri::command]
pub async fn save_archive_unzip(req: SaveArchiveUnzipReq) -> Result<SaveArchiveUnzipResp, String> {
    tokio::task::spawn_blocking(move || {
        unzip_strict(
            Path::new(&req.zip_path),
            Path::new(&req.dest_dir),
            &req.expected_inner,
        )
    })
    .await
    .map_err(|e| format!("unzip task: {e}"))?
    .map_err(|e| format!("unzip: {e:#}"))
}

// ─── Automatic save backups ────────────────────────────────────────────────
