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
//! A restore is the engine's (`POST /api/save-backup/restore-file`), so it has one set of refusals,
//! the backup first and the putting back on failure.

pub use ps5upload_core::save_archive::SaveArchiveBackupFinalizeResp;
use ps5upload_core::save_archive::{
    backup_finalize, cleanup_temp_dir, make_temp_dir, zip_folder_noted,
};
use serde::Deserialize;
use std::path::{Component, Path};

/// `src_dir` as a scratch dir `save_archive_make_temp` made: under the system temp root, named
/// `ps5upload-save-…`, with no `.`/`..` in it (`Path::starts_with` compares components as
/// written, so `<tmp>/../../home/ps5upload-save-x` would otherwise count as under the temp root).
fn scratch_dir(src_dir: &str) -> Result<&Path, String> {
    let p = Path::new(src_dir);
    let tmp = std::env::temp_dir();
    let ours = p
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("ps5upload-save-"));
    let dots = p
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir));
    if !ours || dots || !p.starts_with(&tmp) || p == tmp.as_path() {
        return Err(format!("not one of our scratch dirs: {}", p.display()));
    }
    Ok(p)
}

/// `name` as one folder under a scratch dir. A title ID comes from the console's saves list, so
/// `..`, an absolute path or a name with a separator would point finalize (which deletes every
/// subfolder but `sce_sys`) at some other folder of this computer.
fn one_folder(name: &str) -> Result<&str, String> {
    let mut parts = Path::new(name).components();
    let single = matches!(
        (parts.next(), parts.next()),
        (Some(Component::Normal(_)), None)
    );
    if !single || name.contains(['/', '\\']) {
        return Err(format!("not a save folder name: {name:?}"));
    }
    Ok(name)
}

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
    if !path.is_empty() {
        scratch_dir(&path)?;
    }
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
    /// The zip's comment: which PS5 and user the backup came from, so a restore can refuse one
    /// that would not load here.
    #[serde(default)]
    pub note: Option<String>,
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
    tokio::task::spawn_blocking(move || {
        zip_replacing(
            Path::new(&req.src_dir),
            &req.inner_root,
            dest_buf.as_path(),
            req.note.as_deref().unwrap_or_default(),
        )
    })
    .await
    .map_err(|e| format!("zip task: {e}"))??;
    Ok(dest)
}

/// The zip written beside `dest` and renamed over it only once it is whole. A failed zip (a full
/// disk, or no `<inner_root>/` downloaded) used to truncate and then delete the file the user
/// picked to replace, often an older backup of the same save; now that file stays as it was, and
/// a failed new file leaves nothing behind.
fn zip_replacing(src_dir: &Path, inner_root: &str, dest: &Path, note: &str) -> Result<(), String> {
    if !src_dir.join(inner_root).is_dir() {
        return Err(format!(
            "zip: expected `{inner_root}/` under {}",
            src_dir.display()
        ));
    }
    let (replacing, file) = super::save_text_file::ReplacingFile::create(dest)
        .map_err(|e| format!("create {}: {e}", dest.display()))?;
    drop(file);
    zip_folder_noted(src_dir, inner_root, replacing.path(), note)
        .map_err(|e| format!("zip: {e:#}"))?;
    replacing
        .commit()
        .map_err(|e| format!("save {}: {e}", dest.display()))
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
    let title_dir = scratch_dir(&req.src_dir)?.join(one_folder(&req.title_id)?);
    tokio::task::spawn_blocking(move || backup_finalize(&title_dir))
        .await
        .map_err(|e| format!("backup finalize task: {e}"))?
        .map_err(|e| format!("backup finalize: {e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_takes_only_one_plain_folder_name() {
        assert_eq!(one_folder("CUSA12345"), Ok("CUSA12345"));
        for bad in [
            "",
            ".",
            "..",
            "/Users/me",
            "a/b",
            "a\\b",
            "C:\\Users\\me",
            "../x",
        ] {
            assert!(one_folder(bad).is_err(), "{bad:?} passed");
        }
    }

    #[test]
    fn only_our_scratch_dirs_are_worked_on() {
        let tmp = std::env::temp_dir();
        let ours = tmp.join("ps5upload-save-CUSA12345-1-2-3");
        assert!(scratch_dir(ours.to_str().unwrap()).is_ok());
        let up = tmp.join("..").join("..").join("ps5upload-save-x");
        assert!(scratch_dir(up.to_str().unwrap()).is_err());
        assert!(scratch_dir(tmp.to_str().unwrap()).is_err());
        assert!(scratch_dir(tmp.join("other").to_str().unwrap()).is_err());
        assert!(scratch_dir("/ps5upload-save-x").is_err());
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d =
            crate::test_tmp::temp_path(format!("ps5upload-zip-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn a_failed_zip_leaves_the_picked_file_as_it_was() {
        let dir = scratch("nofolder");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let dest = out.join("CUSA12345.zip");
        std::fs::write(&dest, b"older backup").unwrap();
        // Nothing was downloaded as <title_id>/: the old backup used to be deleted.
        assert!(zip_replacing(&src, "CUSA12345", &dest, "").is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"older backup");
        assert_eq!(names_in(&out), ["CUSA12345.zip"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_zip_that_fails_part_way_leaves_the_picked_file_as_it_was() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("partway");
        let save = dir.join("src").join("CUSA12345");
        std::fs::create_dir_all(&save).unwrap();
        std::fs::write(save.join("a.bin"), b"a").unwrap();
        let locked = save.join("b.bin");
        std::fs::write(&locked, b"b").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&locked).is_ok() {
            // Running as root: nothing can fail to open here.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let dest = out.join("CUSA12345.zip");
        std::fs::write(&dest, b"older backup").unwrap();
        assert!(zip_replacing(&dir.join("src"), "CUSA12345", &dest, "").is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"older backup");
        assert_eq!(names_in(&out), ["CUSA12345.zip"]);
        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_whole_zip_replaces_the_picked_file() {
        let dir = scratch("whole");
        let save = dir.join("src").join("CUSA12345");
        std::fs::create_dir_all(&save).unwrap();
        std::fs::write(save.join("a.bin"), b"a").unwrap();
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let dest = out.join("CUSA12345.zip");
        std::fs::write(&dest, b"older backup").unwrap();
        zip_replacing(&dir.join("src"), "CUSA12345", &dest, "note").unwrap();
        assert_eq!(
            ps5upload_core::save_archive::zip_comment(&dest).unwrap(),
            "note"
        );
        assert_eq!(names_in(&out), ["CUSA12345.zip"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ─── Automatic save backups ────────────────────────────────────────────────
