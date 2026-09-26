//! Resolve a user-picked save destination to a real, writable filesystem path.
//!
//! Desktop save dialogs return real filesystem paths, so `std::fs::File::create`
//! works on them directly. Android's save dialog (Storage Access Framework)
//! returns a `content://` URI instead — `std::fs` can't create one, so the
//! write fails with `No such file or directory (os error 2)`. That surfaced to
//! users as "Couldn't build the report" on the Bug Report page (and would hit
//! every other "save to a dialog-picked path" command the same way).
//!
//! We already hold all-files access on Android (`MANAGE_EXTERNAL_STORAGE` — see
//! `local_fs.rs`, which browses `/storage/emulated/0` directly). So rather than
//! plumb a SAF `ContentResolver.openOutputStream` through JNI (and deal with the
//! resulting non-seekable stream, which the `zip` writer can't use), we sidestep
//! SAF entirely: when the requested destination is a `content://` URI (or empty)
//! we redirect the write to the public Downloads directory under a real path the
//! user can find in any file manager, and hand that real path back to the caller
//! so the success UI reports where the file actually landed.

use std::path::PathBuf;

/// The public Downloads directory on Android. Always present and writable with
/// all-files access. A literal because Tauri's path API doesn't expose a
/// public-Downloads resolver on Android (only app-private dirs).
#[cfg(target_os = "android")]
const ANDROID_PUBLIC_DOWNLOADS: &str = "/storage/emulated/0/Download";

/// Resolve `requested` to a real path the caller can `File::create`.
///
/// On desktop this is `requested` unchanged. On Android, a `content://` URI (or
/// an empty string) is redirected into the public Downloads dir under
/// `fallback_name` — which must be a complete filename including extension
/// (e.g. `ps5upload-bugreport-2026-06-11_0930.zip`). Callers should surface the
/// returned path so the user knows where a redirected file went.
pub fn resolve_save_dest(requested: &str, fallback_name: &str) -> Result<PathBuf, String> {
    #[cfg(target_os = "android")]
    {
        // A `content://…` string is a Storage Access Framework URI, not a real
        // path; redirect it (along with an empty dest) into Downloads.
        if requested.is_empty() || requested.starts_with("content://") {
            let dir = PathBuf::from(ANDROID_PUBLIC_DOWNLOADS);
            std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
            return Ok(dir.join(sanitize_leaf(fallback_name)));
        }
        Ok(PathBuf::from(requested))
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = fallback_name;
        if requested.is_empty() {
            return Err("empty destination path".into());
        }
        Ok(PathBuf::from(requested))
    }
}

/// Reduce an arbitrary name to a safe single filename leaf (no path components,
/// no traversal) for joining under the Downloads dir.
#[cfg(target_os = "android")]
fn sanitize_leaf(name: &str) -> String {
    let leaf = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if leaf.is_empty() || leaf == "." || leaf == ".." {
        "ps5upload-export".to_string()
    } else {
        leaf.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "android"))]
    #[test]
    fn desktop_passes_real_paths_through() {
        assert_eq!(
            resolve_save_dest("/tmp/x.zip", "fallback.zip").unwrap(),
            PathBuf::from("/tmp/x.zip")
        );
        // An empty dest is the one thing we reject on desktop (no path to
        // redirect to, unlike Android's Downloads fallback).
        assert!(resolve_save_dest("", "fallback.zip").is_err());
    }

    #[cfg(target_os = "android")]
    #[test]
    fn android_redirects_content_uris_to_downloads() {
        let p = resolve_save_dest(
            "content://com.android.providers.downloads.documents/document/577",
            "ps5upload-bugreport.zip",
        )
        .unwrap();
        assert!(p.starts_with(ANDROID_PUBLIC_DOWNLOADS));
        assert_eq!(p.file_name().unwrap(), "ps5upload-bugreport.zip");
    }
}
