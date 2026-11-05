//! `POST /api/ps5/helper/replace-copy` — overwrite a copy of the ps5upload helper that the
//! console keeps for boot (an autoloader folder, Payload Manager's storage) with the helper
//! this app carries (#540, #541).
//!
//! "Send helper" only replaces the running helper in memory; an autoloader launches its own
//! stored copy at every boot, so the old version came back after each restart. The client
//! finds those copies (`client/src/lib/helperStoredCopies.ts`) and asks for each one here.
//!
//! The route touches one existing file and only one whose name is the helper's
//! (`ps5upload*.elf`, never the installer daemon): it refuses to create a file, to write
//! outside the folders a boot loader reads, or to write anything that is not an ELF. The
//! bytes are stamped with this engine's AVA1 key (no launch token: a stored copy starts at
//! every boot, a token is good for one launch), so the copy that starts at boot trusts
//! this engine like the one just sent. A copy that already holds exactly those bytes is
//! left alone (`unchanged`).

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct ReplaceReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    /// The stored copy on the console.
    path: String,
    /// The helper ELF on this computer (the desktop app's bundled one); otherwise the
    /// engine's own bundled image.
    #[serde(default)]
    local_path: Option<String>,
}

/// A file name the ps5upload helper is stored under: `ps5upload.elf`, `PS5Upload-8.5.1.elf`,
/// `ps5upload_v9.elf`… but never the installer daemon (`ps5upload-installer.elf`).
pub(crate) fn is_helper_file_name(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    l.starts_with("ps5upload") && l.ends_with(".elf") && !l.contains("installer")
}

/// The folders a boot loader reads payloads from: internal storage and USB / extended drives.
fn path_allowed(path: &str) -> Result<(), &'static str> {
    if !path.starts_with('/') || path.split('/').any(|s| s == "..") || path.contains("//") {
        return Err("bad_path");
    }
    let name = path.rsplit('/').next().unwrap_or("");
    if !is_helper_file_name(name) {
        return Err("not_the_helper");
    }
    let under = |root: &str| path.starts_with(root);
    if under("/data/") || under("/mnt/usb") || under("/mnt/ext") {
        Ok(())
    } else {
        Err("path_not_allowed")
    }
}

/// The helper image to store: `local_path` when given, else the engine's bundled one.
fn helper_image(local_path: Option<&str>) -> Result<Vec<u8>, String> {
    let bytes = match local_path.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => std::fs::read(p).map_err(|e| format!("read {p}: {e}"))?,
        None => crate::bundled_payload::image_bytes(crate::bundled_payload::Image::Payload)?
            .into_owned(),
    };
    if !bytes.starts_with(b"\x7fELF") {
        return Err("the helper image is not an ELF file".into());
    }
    Ok(bytes)
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Updated(usize),
    Unchanged,
    NotFound,
}

/// The console side, so the decision is testable without one.
trait Console {
    /// `Ok(true)` when `path` is a regular file, `Ok(false)` when it is absent or not a file.
    fn is_file(&self, path: &str) -> Result<bool, String>;
    fn blake3(&self, path: &str) -> Option<String>;
    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), String>;
}

fn replace(console: &dyn Console, path: &str, bytes: &[u8]) -> Result<Outcome, String> {
    if !console.is_file(path)? {
        return Ok(Outcome::NotFound);
    }
    let want = blake3::hash(bytes).to_hex().to_string();
    if console
        .blake3(path)
        .is_some_and(|h| h.eq_ignore_ascii_case(&want))
    {
        return Ok(Outcome::Unchanged);
    }
    console.write(path, bytes)?;
    Ok(Outcome::Updated(bytes.len()))
}

struct Live<'a>(&'a str);

impl Console for Live<'_> {
    fn is_file(&self, path: &str) -> Result<bool, String> {
        match ps5upload_core::fs_ops::fs_stat(self.0, path) {
            Ok(s) => Ok(s.kind == "file"),
            Err(e) => {
                let m = format!("{e:#}");
                if ps5upload_core::fs_ops::is_not_found(&m) {
                    Ok(false)
                } else {
                    Err(m)
                }
            }
        }
    }
    fn blake3(&self, path: &str) -> Option<String> {
        ps5upload_core::fs_ops::fs_hash_with_timeout(
            self.0,
            path,
            Some(std::time::Duration::from_secs(20)),
        )
        .ok()
        .map(|h| h.hash)
    }
    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        // Written whole or not at all: the write commits once, beside the old file.
        ps5upload_core::diagnostics::fs_write_bytes_large(self.0, path, bytes, false)
            .map(|_| ())
            .map_err(|e| format!("{e:#}"))
    }
}

/// `{ok:true, status:"updated"|"unchanged", bytes}`; `{ok:false, error:"not_found"}` when no
/// file is there (nothing is created); 400 for a path this route never writes.
pub(crate) async fn replace_copy_handler(
    State(_state): State<crate::AppState>,
    Json(req): Json<ReplaceReq>,
) -> axum::response::Response {
    let path = req.path.trim().to_string();
    if let Err(why) = path_allowed(&path) {
        return crate::json_err(StatusCode::BAD_REQUEST, why).into_response();
    }
    // A write names its console; no default.
    let Some(addr) = crate::required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    crate::log_info!("helper copy: replace {path} on {addr}");
    let r = tokio::task::spawn_blocking(move || -> Result<Outcome, String> {
        let image = helper_image(req.local_path.as_deref())?;
        let bytes = crate::ava1_api::stamped_stored_helper(&image);
        replace(&Live(&addr), &path, &bytes)
    })
    .await;
    match r {
        Ok(Ok(Outcome::Updated(n))) => {
            Json(serde_json::json!({ "ok": true, "status": "updated", "bytes": n })).into_response()
        }
        Ok(Ok(Outcome::Unchanged)) => {
            Json(serde_json::json!({ "ok": true, "status": "unchanged", "bytes": 0 }))
                .into_response()
        }
        Ok(Ok(Outcome::NotFound)) => {
            Json(serde_json::json!({ "ok": false, "error": "not_found" })).into_response()
        }
        Ok(Err(e)) => {
            crate::log_warn!("helper copy: {e}");
            crate::json_err(StatusCode::BAD_GATEWAY, e).into_response()
        }
        Err(e) => {
            crate::json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn helper_names() {
        for ok in [
            "ps5upload.elf",
            "PS5Upload.ELF",
            "ps5upload-8.5.1.elf",
            "ps5upload_v9.elf",
        ] {
            assert!(is_helper_file_name(ok), "{ok}");
        }
        for no in [
            "ps5upload-installer.elf",
            "kstuff.elf",
            "elfldr-ps5upload.elf",
            "ps5upload.bin",
            "ps5upload.elf.txt",
            "my-ps5upload.elf",
        ] {
            assert!(!is_helper_file_name(no), "{no}");
        }
    }

    #[test]
    fn only_helper_copies_in_boot_folders() {
        assert!(path_allowed("/data/ps5_autoloader/ps5upload.elf").is_ok());
        assert!(path_allowed("/mnt/usb0/ps5_autoloader/ps5upload.elf").is_ok());
        assert!(path_allowed("/data/pldmgr/payloads/local/ps5upload/ps5upload.elf").is_ok());
        assert_eq!(
            path_allowed("/data/ps5_autoloader/kstuff.elf"),
            Err("not_the_helper")
        );
        assert_eq!(
            path_allowed("/system/ps5upload.elf"),
            Err("path_not_allowed")
        );
        assert_eq!(path_allowed("data/ps5upload.elf"), Err("bad_path"));
        assert_eq!(
            path_allowed("/data/../system/ps5upload.elf"),
            Err("bad_path")
        );
        assert_eq!(
            path_allowed("/mnt/usb0/x/../../../ps5upload.elf"),
            Err("bad_path")
        );
    }

    struct Fake {
        file: Option<Vec<u8>>,
        writes: RefCell<usize>,
    }

    impl Console for Fake {
        fn is_file(&self, _: &str) -> Result<bool, String> {
            Ok(self.file.is_some())
        }
        fn blake3(&self, _: &str) -> Option<String> {
            self.file
                .as_ref()
                .map(|b| blake3::hash(b).to_hex().to_uppercase())
        }
        fn write(&self, _: &str, _: &[u8]) -> Result<(), String> {
            *self.writes.borrow_mut() += 1;
            Ok(())
        }
    }

    #[test]
    fn writes_an_older_copy_skips_a_current_one_and_never_creates() {
        let new = b"\x7fELFnew".to_vec();
        let old = Fake {
            file: Some(b"\x7fELFold".to_vec()),
            writes: RefCell::new(0),
        };
        assert_eq!(replace(&old, "/p", &new), Ok(Outcome::Updated(new.len())));
        assert_eq!(*old.writes.borrow(), 1);

        let same = Fake {
            file: Some(new.clone()),
            writes: RefCell::new(0),
        };
        assert_eq!(replace(&same, "/p", &new), Ok(Outcome::Unchanged));
        assert_eq!(*same.writes.borrow(), 0);

        let none = Fake {
            file: None,
            writes: RefCell::new(0),
        };
        assert_eq!(replace(&none, "/p", &new), Ok(Outcome::NotFound));
        assert_eq!(*none.writes.borrow(), 0);
    }
}
