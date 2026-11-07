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
//! left alone (`unchanged`). A name that carries the old version moves with the new one, where
//! the slot's `meta.json` names it (as fthdgn-gh/Alembic does for its own payload).
//!
//! "Load the helper at boot" (#715) adds two more: `keep_previous` keeps the copy being replaced
//! beside it as `<name>.prev` (not an `.elf`, so no loader starts it and no later replace touches
//! it), `POST /api/ps5/helper/restore-previous` puts that copy back when the new one did not come
//! up, and `POST /api/ps5/helper/store-in-pldmgr` stores this app's helper in Payload Manager.

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
    /// Keep the copy being replaced as `<path>.prev` first.
    #[serde(default)]
    keep_previous: bool,
}

/// Where the copy a replace kept lives: beside it, under a name no loader starts.
pub(crate) fn previous_path(path: &str) -> String {
    format!("{path}.prev")
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
    /// A small text file, when it can be read.
    fn read_text(&self, path: &str) -> Option<String>;
    /// Moves a file within its folder. Never replaces one.
    fn rename(&self, from: &str, to: &str) -> Result<(), String>;
    fn exists(&self, path: &str) -> bool;
    /// The whole file.
    fn read_all(&self, path: &str) -> Result<Vec<u8>, String>;
}

fn same_bytes(console: &dyn Console, path: &str, bytes: &[u8]) -> bool {
    let want = blake3::hash(bytes).to_hex().to_string();
    console
        .blake3(path)
        .is_some_and(|h| h.eq_ignore_ascii_case(&want))
}

fn replace(
    console: &dyn Console,
    path: &str,
    bytes: &[u8],
    keep_previous: bool,
) -> Result<Outcome, String> {
    if !console.is_file(path)? {
        return Ok(Outcome::NotFound);
    }
    if same_bytes(console, path, bytes) {
        return Ok(Outcome::Unchanged);
    }
    if keep_previous {
        // The copy that last came up is the one to fall back to: kept before anything is
        // written, and the replace stops when it cannot be kept.
        let old = console.read_all(path)?;
        if !old.starts_with(b"\x7fELF") {
            return Err(format!("{path} is not an ELF file; not keeping it"));
        }
        console.write(&previous_path(path), &old)?;
    }
    console.write(path, bytes)?;
    // A kept copy is found beside the file by its name (restore_previous): renaming the file to
    // the new version would leave it behind under the old name.
    if !keep_previous {
        follow_version(console, path, env!("CARGO_PKG_VERSION"));
    }
    Ok(Outcome::Updated(bytes.len()))
}

/// A helper's file name with its version changed to `version`: `ps5upload-11.2.0.elf`, also
/// `ps5upload_v11.2.0.elf`. `None` when the name holds no version, or this one already.
fn renamed_for_version(name: &str, version: &str) -> Option<String> {
    let stem = name.strip_suffix(".elf")?;
    let rest = stem.strip_prefix("ps5upload")?;
    let lead = rest.len() - rest.trim_start_matches(['-', '_', 'v', '.']).len();
    let (prefix, old) = rest.split_at(lead);
    let parts: Vec<&str> = old.split('.').collect();
    let numbered = parts.len() >= 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    (numbered && old != version).then(|| format!("ps5upload{prefix}{version}.elf"))
}

/// Payload Manager lists a slot's file by the name in its `meta.json`, so a name carrying the old
/// version kept saying so after the bytes were replaced. It moves with the version where that file
/// names it, and the file's text follows. Best effort: the new bytes are already in place, and a
/// name that stays is only out of date.
fn follow_version(console: &dyn Console, path: &str, version: &str) {
    let Some((dir, name)) = path.rsplit_once('/') else {
        return;
    };
    let Some(to) = renamed_for_version(name, version) else {
        return;
    };
    let meta_path = format!("{dir}/meta.json");
    let Some(meta) = console.read_text(&meta_path) else {
        return;
    };
    let target = format!("{dir}/{to}");
    if !meta.contains(name) || console.exists(&target) {
        return;
    }
    if console.rename(path, &target).is_err() {
        return;
    }
    // Back as it was if the slot can't be told: a slot naming a missing file wouldn't start.
    if console
        .write(&meta_path, meta.replace(name, &to).as_bytes())
        .is_err()
    {
        let _ = console.rename(&target, path);
    }
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
    fn read_text(&self, path: &str) -> Option<String> {
        let bytes = ps5upload_core::fs_ops::fs_read(self.0, path, 0, 1 << 20).ok()?;
        String::from_utf8(bytes).ok()
    }
    fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        ps5upload_core::fs_ops::fs_move(self.0, from, to).map_err(|e| format!("{e:#}"))
    }
    fn exists(&self, path: &str) -> bool {
        ps5upload_core::fs_ops::fs_exists(self.0, path).unwrap_or(true)
    }
    fn read_all(&self, path: &str) -> Result<Vec<u8>, String> {
        let size = ps5upload_core::fs_ops::fs_stat(self.0, path)
            .map_err(|e| format!("{e:#}"))?
            .size;
        if size > MAX_HELPER_BYTES {
            return Err(format!("{path} is {size} bytes, too large for the helper"));
        }
        ps5upload_core::fs_ops::fs_read_whole(self.0, path, size).map_err(|e| format!("{e:#}"))
    }
}

/// No helper build comes near this; a larger file is not one.
const MAX_HELPER_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
enum Restore {
    Restored(usize),
    Unchanged,
    NotFound,
}

/// Put back the copy a `keep_previous` replace kept. The kept copy stays, so it is still there
/// to fall back to.
fn restore_previous(console: &dyn Console, path: &str) -> Result<Restore, String> {
    let prev = previous_path(path);
    if !console.is_file(&prev)? {
        return Ok(Restore::NotFound);
    }
    let bytes = console.read_all(&prev)?;
    if !bytes.starts_with(b"\x7fELF") {
        return Err(format!("{prev} is not an ELF file"));
    }
    if console.is_file(path)? && same_bytes(console, path, &bytes) {
        return Ok(Restore::Unchanged);
    }
    console.write(path, &bytes)?;
    Ok(Restore::Restored(bytes.len()))
}

#[derive(Deserialize)]
pub(crate) struct RestoreReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    path: String,
}

/// `{ok:true, status:"restored"|"unchanged"}`; `{ok:false, error:"not_found"}` when no copy was
/// kept; 400 for a path the replace route would not write either.
pub(crate) async fn restore_previous_handler(
    State(_state): State<crate::AppState>,
    Json(req): Json<RestoreReq>,
) -> axum::response::Response {
    let path = req.path.trim().to_string();
    if let Err(why) = path_allowed(&path) {
        return crate::json_err(StatusCode::BAD_REQUEST, why).into_response();
    }
    let Some(addr) = crate::required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    crate::log_info!("helper copy: restore the previous {path} on {addr}");
    let r = tokio::task::spawn_blocking(move || restore_previous(&Live(&addr), &path)).await;
    match r {
        Ok(Ok(Restore::Restored(n))) => {
            Json(serde_json::json!({ "ok": true, "status": "restored", "bytes": n }))
                .into_response()
        }
        Ok(Ok(Restore::Unchanged)) => {
            Json(serde_json::json!({ "ok": true, "status": "unchanged", "bytes": 0 }))
                .into_response()
        }
        Ok(Ok(Restore::NotFound)) => {
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

/// The name the helper is stored under in Payload Manager.
pub(crate) const PLDMGR_HELPER_NAME: &str = "ps5upload.elf";

#[derive(Deserialize)]
pub(crate) struct StoreReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    #[serde(default)]
    local_path: Option<String>,
    /// The caller accepts that storing into an existing `ps5upload` folder removes its other
    /// files (Payload Manager's own rule).
    #[serde(default)]
    confirmed: bool,
}

/// `POST /api/ps5/helper/store-in-pldmgr {addr, local_path?, confirmed}`: this app's helper,
/// stamped like every stored copy, into Payload Manager as `ps5upload.elf`. Answers like
/// `/api/ps5/pldmgr/store`: `{ok:false, needs_confirm:true, folder, file_exists}` first when
/// the folder is there and `confirmed` is not set.
pub(crate) async fn store_in_pldmgr_handler(
    State(_state): State<crate::AppState>,
    Json(req): Json<StoreReq>,
) -> axum::response::Response {
    let Some(addr) = crate::required_console_addr(req.addr) else {
        return crate::missing_console_addr();
    };
    let host = ps5upload_core::events::console_host(addr.trim());
    crate::log_info!(
        "helper copy: store in Payload Manager on {host} confirmed={}",
        req.confirmed
    );
    let r = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        use ps5upload_core::payload_manager as pm;
        let check = pm::check(&host, PLDMGR_HELPER_NAME)?;
        if check.folder_exists && !req.confirmed {
            return Ok(serde_json::json!({
                "ok": false,
                "needs_confirm": true,
                "folder": check.folder,
                "file_exists": check.file_exists,
            }));
        }
        let image = helper_image(req.local_path.as_deref())?;
        let bytes = crate::ava1_api::stamped_stored_helper(&image);
        pm::store(&host, PLDMGR_HELPER_NAME, &bytes)?;
        Ok(serde_json::json!({ "ok": true, "folder": check.folder, "bytes": bytes.len() }))
    })
    .await;
    match r {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => {
            crate::log_warn!("helper copy: {e}");
            crate::json_err(StatusCode::BAD_GATEWAY, e).into_response()
        }
        Err(e) => {
            crate::json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response()
        }
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
        replace(&Live(&addr), &path, &bytes, req.keep_previous)
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

    #[derive(Default)]
    struct Slot {
        meta: Option<String>,
        taken: bool,
        moves: RefCell<Vec<(String, String)>>,
        written: RefCell<Vec<(String, String)>>,
        fail_write: bool,
    }

    impl Console for Slot {
        fn is_file(&self, _: &str) -> Result<bool, String> {
            Ok(true)
        }
        fn blake3(&self, _: &str) -> Option<String> {
            None
        }
        fn write(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
            if self.fail_write && path.ends_with("meta.json") {
                return Err("no".into());
            }
            self.written.borrow_mut().push((
                path.to_string(),
                String::from_utf8_lossy(bytes).into_owned(),
            ));
            Ok(())
        }
        fn read_text(&self, _: &str) -> Option<String> {
            self.meta.clone()
        }
        fn rename(&self, from: &str, to: &str) -> Result<(), String> {
            self.moves
                .borrow_mut()
                .push((from.to_string(), to.to_string()));
            Ok(())
        }
        fn exists(&self, _: &str) -> bool {
            self.taken
        }
        fn read_all(&self, _: &str) -> Result<Vec<u8>, String> {
            Err("unused".into())
        }
    }

    #[test]
    fn a_name_with_a_version_gets_the_new_one() {
        let r = |n: &str| renamed_for_version(n, "11.4.0");
        assert_eq!(
            r("ps5upload-11.2.0.elf").as_deref(),
            Some("ps5upload-11.4.0.elf")
        );
        assert_eq!(
            r("ps5upload_v9.1.elf").as_deref(),
            Some("ps5upload_v11.4.0.elf")
        );
        assert_eq!(r("ps5upload.elf"), None, "no version in it");
        assert_eq!(r("ps5upload-11.4.0.elf"), None, "already this one");
        assert_eq!(r("ps5upload-helper.elf"), None);
        assert_eq!(r("other-1.0.elf"), None);
    }

    #[test]
    fn a_slot_follows_only_where_its_meta_names_the_file() {
        let path = "/data/pldmgr/slot/ps5upload-1.0.0.elf";
        let slot = Slot {
            meta: Some(r#"{"file":"ps5upload-1.0.0.elf","x":1}"#.into()),
            ..Default::default()
        };
        follow_version(&slot, path, "2.0.0");
        assert_eq!(
            slot.moves.borrow()[0].1,
            "/data/pldmgr/slot/ps5upload-2.0.0.elf"
        );
        assert_eq!(
            slot.written.borrow()[0].1,
            r#"{"file":"ps5upload-2.0.0.elf","x":1}"#
        );

        let unnamed = Slot {
            meta: Some("{}".into()),
            ..Default::default()
        };
        follow_version(&unnamed, path, "2.0.0");
        assert!(unnamed.moves.borrow().is_empty());

        let no_meta = Slot::default();
        follow_version(&no_meta, path, "2.0.0");
        assert!(no_meta.moves.borrow().is_empty());

        let taken = Slot {
            meta: Some("ps5upload-1.0.0.elf".into()),
            taken: true,
            ..Default::default()
        };
        follow_version(&taken, path, "2.0.0");
        assert!(taken.moves.borrow().is_empty(), "never replaces a file");

        let failing = Slot {
            meta: Some("ps5upload-1.0.0.elf".into()),
            fail_write: true,
            ..Default::default()
        };
        follow_version(&failing, path, "2.0.0");
        let moves = failing.moves.borrow();
        assert_eq!(moves.len(), 2);
        assert_eq!(moves[1].1, path, "put back when the meta can't be written");
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
        fn read_text(&self, _: &str) -> Option<String> {
            None
        }
        fn rename(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn exists(&self, _: &str) -> bool {
            false
        }
        fn read_all(&self, _: &str) -> Result<Vec<u8>, String> {
            self.file.clone().ok_or_else(|| "absent".into())
        }
    }

    #[test]
    fn writes_an_older_copy_skips_a_current_one_and_never_creates() {
        let new = b"\x7fELFnew".to_vec();
        let old = Fake {
            file: Some(b"\x7fELFold".to_vec()),
            writes: RefCell::new(0),
        };
        assert_eq!(
            replace(&old, "/p", &new, false),
            Ok(Outcome::Updated(new.len()))
        );
        assert_eq!(*old.writes.borrow(), 1);

        let same = Fake {
            file: Some(new.clone()),
            writes: RefCell::new(0),
        };
        assert_eq!(replace(&same, "/p", &new, false), Ok(Outcome::Unchanged));
        assert_eq!(*same.writes.borrow(), 0);

        let none = Fake {
            file: None,
            writes: RefCell::new(0),
        };
        assert_eq!(replace(&none, "/p", &new, false), Ok(Outcome::NotFound));
        assert_eq!(*none.writes.borrow(), 0);
    }

    /// Files by path, for the keep / restore round trip.
    #[derive(Default)]
    struct Disk(RefCell<std::collections::HashMap<String, Vec<u8>>>);

    impl Disk {
        fn with(files: &[(&str, &[u8])]) -> Self {
            let d = Disk::default();
            for (p, b) in files {
                d.0.borrow_mut().insert(p.to_string(), b.to_vec());
            }
            d
        }
        fn get(&self, p: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(p).cloned()
        }
    }

    impl Console for Disk {
        fn is_file(&self, p: &str) -> Result<bool, String> {
            Ok(self.0.borrow().contains_key(p))
        }
        fn blake3(&self, p: &str) -> Option<String> {
            self.get(p).map(|b| blake3::hash(&b).to_hex().to_string())
        }
        fn write(&self, p: &str, b: &[u8]) -> Result<(), String> {
            self.0.borrow_mut().insert(p.to_string(), b.to_vec());
            Ok(())
        }
        fn read_text(&self, _: &str) -> Option<String> {
            None
        }
        fn rename(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn exists(&self, p: &str) -> bool {
            self.0.borrow().contains_key(p)
        }
        fn read_all(&self, p: &str) -> Result<Vec<u8>, String> {
            self.get(p).ok_or_else(|| "absent".into())
        }
    }

    #[test]
    fn keeps_the_replaced_copy_and_puts_it_back() {
        let p = "/data/pldmgr/payloads/local/ps5upload/ps5upload.elf";
        let disk = Disk::with(&[(p, b"\x7fELFold")]);
        assert_eq!(
            replace(&disk, p, b"\x7fELFnew", true),
            Ok(Outcome::Updated(7))
        );
        assert_eq!(disk.get(p).as_deref(), Some(&b"\x7fELFnew"[..]));
        assert_eq!(
            disk.get(&previous_path(p)).as_deref(),
            Some(&b"\x7fELFold"[..])
        );
        // The same bytes again: nothing written, the kept copy is still the older one.
        assert_eq!(
            replace(&disk, p, b"\x7fELFnew", true),
            Ok(Outcome::Unchanged)
        );
        assert_eq!(
            disk.get(&previous_path(p)).as_deref(),
            Some(&b"\x7fELFold"[..])
        );
        assert_eq!(restore_previous(&disk, p), Ok(Restore::Restored(7)));
        assert_eq!(disk.get(p).as_deref(), Some(&b"\x7fELFold"[..]));
        assert!(disk.get(&previous_path(p)).is_some(), "the kept copy stays");
        assert_eq!(restore_previous(&disk, p), Ok(Restore::Unchanged));
    }

    #[test]
    fn nothing_kept_nothing_restored_and_a_non_elf_is_never_kept() {
        let p = "/data/ps5_autoloader/ps5upload.elf";
        let disk = Disk::with(&[(p, b"\x7fELFold")]);
        assert_eq!(restore_previous(&disk, p), Ok(Restore::NotFound));
        assert_eq!(disk.get(p).as_deref(), Some(&b"\x7fELFold"[..]));

        let junk = Disk::with(&[(p, b"not an elf")]);
        assert!(replace(&junk, p, b"\x7fELFnew", true).is_err());
        assert_eq!(
            junk.get(p).as_deref(),
            Some(&b"not an elf"[..]),
            "nothing written when the old copy cannot be kept"
        );
        // Without keep_previous a replace never writes a .prev.
        let plain = Disk::with(&[(p, b"\x7fELFold")]);
        replace(&plain, p, b"\x7fELFnew", false).unwrap();
        assert!(plain.get(&previous_path(p)).is_none());
    }

    #[test]
    fn the_kept_copy_is_not_a_helper_name() {
        let kept = previous_path("/data/ps5_autoloader/ps5upload.elf");
        let name = kept.rsplit('/').next().unwrap();
        assert!(!is_helper_file_name(name), "{name}");
    }
}
