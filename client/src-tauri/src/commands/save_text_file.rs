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

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Cap for `read_text_file` so a user pointing the import dialog at a
/// huge/binary file can't balloon renderer memory. The settings/bug
/// bundles this reads back are tens of KiB; 16 MiB is generous headroom
/// while still bounding the worst case.
const READ_TEXT_FILE_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// A file being written for `dest`, which only changes once the whole file is there: picking an
/// existing file in the save dialog and running out of disk used to lose that file, truncated by
/// the write and then deleted by its clean-up. The new bytes go to a temporary file beside it and
/// `commit` renames that over it. Dropped uncommitted, the temporary file goes and `dest` is as
/// it was.
pub(super) struct ReplacingFile {
    /// Where the bytes go now.
    path: PathBuf,
    /// Where they end up. None: `path` is the destination itself, in a folder where a file
    /// cannot be created beside it (a save dialog can grant access to just the one file).
    dest: Option<PathBuf>,
    /// The destination did not exist before, so a failed write may delete what it left.
    remove_on_drop: bool,
}

/// Tells this process's temporary files apart.
static REPLACING_SEQ: AtomicU64 = AtomicU64::new(0);

impl ReplacingFile {
    pub(super) fn create(dest: &Path) -> std::io::Result<(Self, std::fs::File)> {
        // Through a link, not over it: the save dialog named what the link points at.
        let dest = std::fs::canonicalize(dest).unwrap_or_else(|_| dest.to_path_buf());
        let seq = REPLACING_SEQ.fetch_add(1, Ordering::Relaxed);
        let mut name = dest.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{}-{seq}.tmp", std::process::id()));
        let tmp = dest.with_file_name(name);
        match std::fs::File::create(&tmp) {
            Ok(file) => {
                // The file it replaces may be private: the new one keeps that.
                if let Ok(existing) = std::fs::metadata(&dest) {
                    let _ = std::fs::set_permissions(&tmp, existing.permissions());
                }
                Ok((
                    Self {
                        path: tmp,
                        dest: Some(dest),
                        remove_on_drop: true,
                    },
                    file,
                ))
            }
            Err(_) => {
                let existed = dest.exists();
                let file = std::fs::File::create(&dest)?;
                Ok((
                    Self {
                        path: dest,
                        dest: None,
                        remove_on_drop: !existed,
                    },
                    file,
                ))
            }
        }
    }

    /// All written: this is now the destination.
    pub(super) fn commit(mut self) -> std::io::Result<()> {
        if let Some(dest) = self.dest.take() {
            super::replace_file(&self.path, &dest)?;
        }
        self.remove_on_drop = false;
        Ok(())
    }
}

impl Drop for ReplacingFile {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Write `bytes` to `dest`, replacing what is there only when all of them made it.
fn write_replacing(dest: &Path, bytes: &[u8]) -> Result<(), String> {
    let describe = |e: std::io::Error| format!("write {}: {e}", dest.display());
    let (replacing, mut file) = ReplacingFile::create(dest).map_err(describe)?;
    file.write_all(bytes).map_err(describe)?;
    file.sync_all().map_err(describe)?;
    drop(file);
    replacing.commit().map_err(describe)
}

/// Write `contents` to `path` (an absolute path the user picked via the
/// save dialog). A failed write leaves no truncated artifact at their chosen path, and an
/// existing file there as it was (`ReplacingFile`).
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
    // On a slow/networked disk writing is enough to stutter the UI thread,
    // so route it through the blocking pool like the rest of the file-save commands.
    tokio::task::spawn_blocking(move || {
        // Android save dialogs return content:// URIs std::fs can't write to;
        // redirect those to a real path under Downloads (desktop pass-through),
        // using the renderer's chosen filename so distinct exports stay distinct.
        let fallback = dest_filename
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("ps5upload-export.txt");
        let dest_buf = super::save_dest::resolve_save_dest(&path, fallback)?;
        write_replacing(dest_buf.as_path(), contents.as_bytes())
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
        write_replacing(&dest, &bytes)?;
        Ok(dest.display().to_string())
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
mod replacing_tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("ps5upload-replacing-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_save_replaces_the_file_and_leaves_nothing_beside_it() {
        let dir = scratch("ok");
        let dest = dir.join("export.json");
        std::fs::write(&dest, b"old").unwrap();
        write_replacing(&dest, b"new").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert_eq!(names_in(&dir), ["export.json"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_that_fails_leaves_the_existing_file_as_it_was() {
        // F26: the old file was truncated by the failed write, then deleted by its clean-up.
        let dir = scratch("failed");
        let dest = dir.join("export.json");
        std::fs::write(&dest, b"precious").unwrap();
        let (replacing, mut file) = ReplacingFile::create(&dest).unwrap();
        file.write_all(b"half").unwrap();
        drop(file);
        // Never committed: what a full disk comes to.
        drop(replacing);
        assert_eq!(std::fs::read(&dest).unwrap(), b"precious");
        assert_eq!(names_in(&dir), ["export.json"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_new_file_leaves_nothing() {
        let dir = scratch("new");
        let dest = dir.join("export.json");
        let (replacing, file) = ReplacingFile::create(&dest).unwrap();
        drop(file);
        drop(replacing);
        assert_eq!(names_in(&dir), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_written_through_not_replaced() {
        let dir = scratch("link");
        let real = dir.join("real.json");
        let link = dir.join("link.json");
        std::fs::write(&real, b"old").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_replacing(&link, b"new").unwrap();
        assert_eq!(std::fs::read(&real).unwrap(), b"new");
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_private_file_stays_private_when_replaced() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode");
        let dest = dir.join("export.json");
        std::fs::write(&dest, b"old").unwrap();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_replacing(&dest, b"new").unwrap();
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_takes_no_new_file_still_saves_over_the_one_it_was_given() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("readonly");
        let dest = dir.join("export.json");
        std::fs::write(&dest, b"old").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root can write anywhere, so this only proves something for a normal user.
        let result = write_replacing(&dest, b"new");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        result.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert_eq!(names_in(&dir), ["export.json"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
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
