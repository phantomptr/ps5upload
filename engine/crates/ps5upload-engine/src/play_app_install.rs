//! PS5Upload Play on a PS5, from PS5Upload: install, update, remove and open the native app
//! (title `FAKE99261`) from the copy this engine carries. Nothing is downloaded: the release zip
//! (`PS5Upload-Play-FAKE99261-<version>.zip`, one `FAKE99261/` folder with `eboot.bin` and
//! `sce_sys/`) is embedded at build time (build.rs) when the build had one; an engine without it
//! says so (`bundled: null`, `bundled_missing: "not_included"`). `PS5UPLOAD_PLAY_ZIP` names a zip
//! to use instead (a local build of the app).
//!
//! **Installed exactly as the homebrew store app installs it**, so an update or removal by either
//! tool works on an install made by the other. What that app does, read from its source at
//! commit 0e1f031 (<https://github.com/darkxex/ProsperoStore/tree/0e1f031889643130af0a617b58b684e9cbfa67af>):
//! `src/install/transaction.cpp` (`resolve`, `apply`, `uninstall`, `managed`, `keep_previous`,
//! `refresh_registered`), `src/install/files.cpp` (`overlay_tree`), `src/install/archive.cpp`
//! (`open_to_all`, `extract_archive`), `src/system/locations.cpp` (`scan_policy`, `drive_root`,
//! `work_path_conflict`), `src/app/service.cpp` (the uninstall's `unregister_title`),
//! `src/app/store.cpp` (`kInternalLocation`, the update choice), `src/catalog/catalog.cpp`
//! (`format_receipt`, `parse_receipt`, `update_available`). How we match it, point by point (the
//! tests below pin the layout, the receipt and the scan rules):
//!
//! * **Where.** A new install goes to `/data/homebrew/FAKE99261` (its `kInternalLocation`, and the
//!   location it uses for an app whose catalog record says `"internalOnly": true`, as ours does).
//!   An installed app is updated and removed where it is: the receipt's `location` names it.
//! * **Work folders.** `<drive root>/prosperostore/{staging,backup,previous,trash}/FAKE99261`
//!   (`resolve()`; the drive root of `/data/homebrew` is `/data`), on the same drive as the app so
//!   every move is one `rename(2)`; refused while one of them is inside a folder ShadowMount+
//!   scans (`work_path_conflict()`, from `/data/shadowmount/config.ini` and `manual.lst`).
//! * **Its state.** `/data/prosperostore/journal.json` while a change is in progress: we refuse
//!   everything while it is there (the store must recover first). A receipt per app it manages,
//!   `/data/prosperostore/receipts/FAKE99261.json`, one line: `{"schema":1,"titleId",
//!   "location","contentVersion","releaseTag","sha256","installedAt"}`. The store manages (updates,
//!   removes) only an app whose receipt names the folder and the version in it, so we write the
//!   same receipt after every install and update (release tag `v<version>`, the zip's SHA-256: the
//!   same file as the release asset its catalog pins) and remove it with the app.
//! * **Version.** `sce_sys/param.json` `contentVersion` (NN.NNN.NNN), compared as text; an update
//!   needs a newer one.
//! * **Unpacking.** The app's folder from the zip, every folder and file mode 0777 (the console
//!   starts an app only when everyone may read and run its files), in staging first.
//! * **Install.** Staging, then one rename of the staged folder to the app's place: the app is
//!   either all there or not at all.
//! * **Update.** Its default ("Quick update", `overlay_tree()`): every staged folder made in the
//!   app's folder (0777), then each staged file renamed over its counterpart, `sce_sys/param.json`
//!   last; nothing set aside, files the new version doesn't bring stay. Its other choice ("Clean
//!   install", `mode: "clean"` here): the app's folder renamed to `backup/`, the staged one renamed
//!   into place, then the old one moved to `previous/FAKE99261` (replacing the one an earlier
//!   update left). Either way the console's copies of `sce_sys` are refreshed
//!   (`refresh_registered()`: each file into `/user/appmeta/FAKE99261/` and
//!   `/user/app/FAKE99261/sce_sys/` when they exist, `icon0.png` to `/user/app/FAKE99261/`).
//! * **Running.** It refuses to change a running app; we close it first (the user asked for the
//!   change; the app keeps its settings in `/data/ps5upload-play`, outside its folder).
//! * **Registering.** It never registers: ShadowMount+ scans `/data/homebrew` and registers the
//!   folder it finds there (a folder with `sce_sys/param.json`, stable for 10 s). We do the same
//!   when ShadowMount+ is running and the location is one it scans, and wait for it a while; only
//!   when it is not do we register through the helper (`/api/ps5/app/register`'s path). Which one
//!   did it is recorded in `/data/ps5upload/play/install.json` and in the answer.
//! * **Uninstall.** The app's folder renamed to `trash/FAKE99261` (out of the scanned place in one
//!   step) and deleted, the receipt removed, then the title taken off the home screen (Sony's
//!   uninstall). Its data under `/data` stays; here `delete_data` also deletes
//!   `/data/ps5upload-play` and the download data `/user/download/FAKE99261`.
//!
//! Also kept: a copy of the app's folder in `/data/ps5upload/play/FAKE99261/` (param.json written
//! last) with the zip's SHA-256 beside it (`FAKE99261.sha256`), which the console page (Minion)
//! installs from the same way without a computer.
//!
//! Routes:
//! * `GET /api/play/app/installed?addr=` -> `{title_id, bundled, bundled_missing?, installed,
//!   running, kept, store: {managed, busy}, action}`; `installed`: `{version, content_version,
//!   path, registered}`; `action`: `install`, `update`, `none`, `newer_installed`, `unavailable`.
//! * `POST /api/play/app/install {addr, mode?: "quick"|"clean", replace?, confirm_store?}`.
//!   Refusals (409): `store_busy`, `store_managed` (the store's install and the user did not
//!   confirm replacing it), `busy`, `app_running`, `not_newer` (without `replace`),
//!   `unexpected` (something else in the app's place), `backup_left`, `work_scanned`.
//! * `POST /api/play/app/remove {addr, delete_data?, confirm_store?}`.
//! * `POST /api/play/app/open {addr}`.
//!
//! Errors are `{error, reason}`.

use std::borrow::Cow;
use std::collections::HashSet;
use std::io::Read as _;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{console_addr_or_default, missing_console_addr, required_console_addr, AppState};

pub(crate) const TITLE_ID: &str = "FAKE99261";
/// Where a new install goes.
pub(crate) const DEFAULT_LOCATION: &str = "/data/homebrew";
/// The copy left for the console page to install from, and the zip's digest beside it.
pub(crate) const KEPT_DIR: &str = "/data/ps5upload/play/FAKE99261";
pub(crate) const KEPT_SHA: &str = "/data/ps5upload/play/FAKE99261.sha256";
/// Who registered the app, and what was installed when.
pub(crate) const INSTALL_RECORD: &str = "/data/ps5upload/play/install.json";
/// The app's own settings and logs (outside its folder, so an update keeps them).
pub(crate) const DATA_DIR: &str = "/data/ps5upload-play";
/// The app's download data (`/download0` inside the app).
pub(crate) const DOWNLOAD_DIR: &str = "/user/download/FAKE99261";
/// The store app's own state.
const STORE_ROOT: &str = "/data/prosperostore";
const STORE_RECEIPT: &str = "/data/prosperostore/receipts/FAKE99261.json";
const STORE_JOURNAL: &str = "/data/prosperostore/journal.json";
const SMP_CONFIG: &str = "/data/shadowmount/config.ini";
const SMP_MANUAL: &str = "/data/shadowmount/manual.lst";
/// The largest one file may be (the helper's single-write ceiling).
const FILE_MAX: u64 = 16 * 1024 * 1024;
const PARAM: &str = "sce_sys/param.json";
const EBOOT: &str = "eboot.bin";
/// How long ShadowMount+ gets to register a new install (10 s stable + a 15 s scan, twice over).
const SMP_REGISTER_WAIT: Duration = Duration::from_secs(90);

#[cfg(have_bundled_play)]
const EMBEDDED: Option<&[u8]> = Some(include_bytes!(env!("PS5UPLOAD_BUNDLED_PLAY_ZIP")) as &[u8]);
#[cfg(not(have_bundled_play))]
const EMBEDDED: Option<&[u8]> = None;

// ─── Versions ──────────────────────────────────────────────────────────────

/// The store's version test: exactly `NN.NNN.NNN`.
pub(crate) fn is_content_version(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() == 10
        && b[2] == b'.'
        && b[6] == b'.'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 2 || i == 6 || c.is_ascii_digit())
}

/// `13.000.000` -> (13, 0, 0).
pub(crate) fn version_key(content_version: &str) -> Option<(u32, u32, u32)> {
    let mut it = content_version.trim().split('.');
    let a = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    let c = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((a, b, c))
}

/// `13.000.000` as people read it: `13.0.0`. The text itself when it is not a version.
pub(crate) fn human_version(content_version: &str) -> String {
    match version_key(content_version) {
        Some((a, b, c)) => format!("{a}.{b}.{c}"),
        None => content_version.trim().to_string(),
    }
}

/// The store's update test (`catalog::update_available`): both versions well formed and the new
/// one greater, as text (which the fixed width makes numeric).
pub(crate) fn newer(installed: &str, available: &str) -> bool {
    is_content_version(installed) && is_content_version(available) && available > installed
}

/// What PS5Upload offers for the bundled and installed versions.
pub(crate) fn action_for(bundled: Option<&str>, installed: Option<&str>) -> &'static str {
    match (bundled.filter(|b| is_content_version(b)), installed) {
        (None, _) => "unavailable",
        (Some(_), None) => "install",
        (Some(b), Some(i)) if !is_content_version(i) || newer(i, b) => "update",
        (Some(b), Some(i)) if b == i => "none",
        _ => "newer_installed",
    }
}

// ─── The copy this engine carries ──────────────────────────────────────────

/// The app's files: (path inside `FAKE99261/`, bytes).
pub(crate) type AppFiles = Vec<(String, Vec<u8>)>;

/// The bundled app: its version, size and the zip's SHA-256.
#[derive(Debug, Clone)]
pub(crate) struct BundleInfo {
    pub content_version: String,
    pub bytes: u64,
    pub files: usize,
    pub sha256: String,
}

fn zip_bytes() -> Result<Option<Cow<'static, [u8]>>, String> {
    if let Some(p) = std::env::var_os("PS5UPLOAD_PLAY_ZIP").filter(|p| !p.is_empty()) {
        return std::fs::read(&p)
            .map(|b| Some(Cow::Owned(b)))
            .map_err(|e| format!("PS5UPLOAD_PLAY_ZIP ({}): {e}", p.to_string_lossy()));
    }
    Ok(EMBEDDED.map(Cow::Borrowed))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Reads and checks a release zip: one `FAKE99261/` top folder, safe names, `eboot.bin` and
/// `sce_sys/param.json` there, that param.json naming this title with a version, no file over the
/// helper's write ceiling. The zip reader checks each file's CRC-32. With `want_files`, the files'
/// bytes too (inside the folder, in no order).
pub(crate) fn read_zip(bytes: &[u8], want_files: bool) -> Result<(BundleInfo, AppFiles), String> {
    let mut z =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("not a zip: {e}"))?;
    let prefix = format!("{TITLE_ID}/");
    let mut files = Vec::new();
    let mut names = HashSet::new();
    let mut total = 0u64;
    let mut param: Option<Vec<u8>> = None;
    for i in 0..z.len() {
        let mut f = z.by_index(i).map_err(|e| format!("zip entry {i}: {e}"))?;
        let raw = f
            .name()
            .map(|n| n.to_string())
            .map_err(|e| format!("zip entry {i}: {e}"))?;
        let safe = f
            .enclosed_name()
            .ok_or_else(|| format!("unsafe name in the zip: {raw}"))?;
        let name = safe.to_string_lossy().replace('\\', "/");
        if name != TITLE_ID && !name.starts_with(&prefix) {
            return Err(format!("the zip holds {name} outside {TITLE_ID}/"));
        }
        if f.is_dir() {
            continue;
        }
        let rel = name[prefix.len()..].to_string();
        if rel.is_empty()
            || rel
                .split('/')
                .any(|c| c.is_empty() || c == "." || c == "..")
        {
            return Err(format!("unsafe name in the zip: {raw}"));
        }
        if f.size() > FILE_MAX {
            return Err(format!(
                "{rel} is {} bytes; one file may be 16 MiB at most",
                f.size()
            ));
        }
        total += f.size();
        let is_param = rel == PARAM;
        if want_files || is_param {
            let mut buf = Vec::with_capacity(f.size() as usize);
            f.read_to_end(&mut buf).map_err(|e| format!("{rel}: {e}"))?;
            if is_param {
                param = Some(buf.clone());
            }
            if want_files {
                files.push((rel.clone(), buf));
            }
        }
        names.insert(rel);
    }
    if !names.contains(EBOOT) {
        return Err(format!("the zip has no {TITLE_ID}/{EBOOT}"));
    }
    let param = param.ok_or_else(|| format!("the zip has no {TITLE_ID}/{PARAM}"))?;
    let (title, cv) = param_fields(&param).ok_or("its param.json is not readable")?;
    if title != TITLE_ID {
        return Err(format!("its param.json says {title}, not {TITLE_ID}"));
    }
    if !is_content_version(&cv) {
        return Err(format!("its param.json has no version ({cv})"));
    }
    Ok((
        BundleInfo {
            content_version: cv,
            bytes: total,
            files: names.len(),
            sha256: sha256_hex(bytes),
        },
        files,
    ))
}

/// `(titleId, contentVersion)` of a param.json.
fn param_fields(bytes: &[u8]) -> Option<(String, String)> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let title = v.get("titleId")?.as_str()?.to_string();
    let cv = v
        .get("contentVersion")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Some((title, cv))
}

/// The bundled app's version, or why there is none (`not_included`, or what is wrong with it).
/// The built-in copy is read once; a `PS5UPLOAD_PLAY_ZIP` file each time (it may be rebuilt).
pub(crate) fn bundle_info() -> Result<BundleInfo, String> {
    static EMBEDDED_INFO: OnceLock<Result<BundleInfo, String>> = OnceLock::new();
    let from_env = std::env::var_os("PS5UPLOAD_PLAY_ZIP").is_some_and(|p| !p.is_empty());
    let read = || -> Result<BundleInfo, String> {
        match zip_bytes()? {
            Some(b) => read_zip(&b, false).map(|(i, _)| i),
            None => Err("not_included".into()),
        }
    };
    if from_env {
        return read();
    }
    EMBEDDED_INFO.get_or_init(read).clone()
}

fn bundle_files() -> Result<(BundleInfo, AppFiles), String> {
    match zip_bytes()? {
        Some(b) => read_zip(&b, true),
        None => Err("not_included".into()),
    }
}

// ─── The store's layout (pure) ─────────────────────────────────────────────

/// The store's `clean_absolute_path`: absolute, no empty, `.` or `..` part, no backslash or
/// control character, shorter than 1024.
pub(crate) fn clean_absolute_path(p: &str) -> bool {
    if p.is_empty() || !p.starts_with('/') || p.len() >= 1024 {
        return false;
    }
    if p.bytes().any(|c| c < 32 || c == 127) {
        return false;
    }
    if p == "/" {
        return true;
    }
    p[1..]
        .split('/')
        .all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains('\\'))
}

/// `path` is `root` or inside it (the store's `within`).
fn within(root: &str, path: &str) -> bool {
    root == "/"
        || path == root
        || (path.starts_with(root) && path.as_bytes().get(root.len()) == Some(&b'/'))
}

/// The drive a location is on, as the store names it: `/data`, `/mnt/ext0`, `/mnt/ext1`,
/// `/mnt/usb0`..`/mnt/usb7`; None for anything else.
pub(crate) fn drive_root(path: &str) -> Option<String> {
    if !clean_absolute_path(path) {
        return None;
    }
    let mut drives = vec!["/data".to_string(), "/mnt/ext0".into(), "/mnt/ext1".into()];
    drives.extend((0..8).map(|i| format!("/mnt/usb{i}")));
    drives.into_iter().find(|d| within(d, path))
}

/// The store's folders for this app at a location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Layout {
    pub location: String,
    pub target: String,
    pub work: String,
    pub staging: String,
    pub backup: String,
    pub previous: String,
    pub trash: String,
}

pub(crate) fn layout(location: &str) -> Option<Layout> {
    let location = location.trim_end_matches('/');
    if location.is_empty() || !clean_absolute_path(location) {
        return None;
    }
    let work = format!("{}/prosperostore", drive_root(location)?);
    Some(Layout {
        location: location.to_string(),
        target: format!("{location}/{TITLE_ID}"),
        staging: format!("{work}/staging/{TITLE_ID}"),
        backup: format!("{work}/backup/{TITLE_ID}"),
        previous: format!("{work}/previous/{TITLE_ID}"),
        trash: format!("{work}/trash/{TITLE_ID}"),
        work,
    })
}

/// What ShadowMount+ scans (the store's `scan_policy`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScanPolicy {
    pub roots: Vec<String>,
    pub manual: Vec<String>,
    pub depth: usize,
}

fn tidy_path(v: &str) -> String {
    let mut v = v.trim();
    if v.len() >= 2 && (v.starts_with('"') || v.starts_with('\'')) && v.ends_with(&v[..1]) {
        v = v[1..v.len() - 1].trim();
    }
    let mut out = String::new();
    for c in v.chars() {
        if !(c == '/' && out.ends_with('/')) {
            out.push(c);
        }
    }
    out
}

fn add_path(list: &mut Vec<String>, p: &str) {
    let mut p = p;
    while p.len() > 1 && p.ends_with('/') {
        p = &p[..p.len() - 1];
    }
    if clean_absolute_path(p) && !list.iter().any(|x| x == p) {
        list.push(p.to_string());
    }
}

pub(crate) fn scan_policy(config: &str, manual: &str) -> ScanPolicy {
    let mut roots = Vec::new();
    let mut depth = 1;
    let mut recursive = false;
    for line in config.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', ';', '[']) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.split(['#', ';']).next().unwrap_or("").trim();
        if value.is_empty() {
            continue;
        }
        match key.as_str() {
            "scanpath" => add_path(&mut roots, &tidy_path(value)),
            "scan_depth" => {
                if let Ok(d) = value.parse::<usize>() {
                    if (1..=2).contains(&d) {
                        depth = d;
                    }
                }
            }
            "recursive_scan" => {
                recursive |= matches!(
                    value.to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on" | "ro"
                )
            }
            _ => {}
        }
    }
    if recursive {
        depth = 2;
    }
    if roots.is_empty() {
        roots = vec!["/data/homebrew".into(), "/data/etaHEN/games".into()];
        let mut drives = vec!["/mnt/ext0".to_string(), "/mnt/ext1".into()];
        drives.extend((0..8).map(|i| format!("/mnt/usb{i}")));
        for d in drives {
            roots.push(format!("{d}/homebrew"));
            roots.push(format!("{d}/etaHEN/games"));
            roots.push(d);
        }
    }
    add_path(&mut roots, "/mnt/shadowmnt/pfsc");
    add_path(&mut roots, "/mnt/shadowmnt");
    let mut man = Vec::new();
    for line in manual.lines() {
        let line = line.trim();
        if !line.is_empty() && !line.starts_with('#') {
            add_path(&mut man, &tidy_path(line));
        }
    }
    ScanPolicy {
        roots,
        manual: man,
        depth,
    }
}

/// Why a work folder would be seen by ShadowMount+ (the store's `work_path_conflict`), or None.
pub(crate) fn work_path_conflict(policy: &ScanPolicy, app_path: &str) -> Option<String> {
    if !clean_absolute_path(app_path) {
        return Some("the path isn't a plain one".into());
    }
    for root in &policy.roots {
        if within(app_path, root) {
            return Some(format!("scan path {root} is inside it"));
        }
        if within(root, app_path) {
            let rel = &app_path[if root == "/" { 0 } else { root.len() }..];
            let depth = rel.matches('/').count();
            let limit = policy.depth + usize::from(within("/mnt/shadowmnt/pfsc", root));
            if depth <= limit {
                return Some(format!("scan path {root}, scan depth {}", policy.depth));
            }
        }
    }
    for path in &policy.manual {
        let stray = path == "/" || path == "/mnt" || drive_root(path).as_deref() == Some(path);
        if (!stray && within(path, app_path)) || within(app_path, path) {
            return Some(format!("manual.lst entry {path}"));
        }
    }
    None
}

/// The store's receipt: `{"schema":1,...}` on one line, strings escaped as its `member()` does.
pub(crate) fn receipt_text(
    location: &str,
    content_version: &str,
    release_tag: &str,
    sha256: &str,
    installed_at: &str,
) -> String {
    fn member(out: &mut String, key: &str, value: &str) {
        out.push_str(if out.len() > 1 { ",\"" } else { "\"" });
        out.push_str(key);
        out.push_str("\":\"");
        for c in value.chars() {
            match c {
                '"' | '\\' => {
                    out.push('\\');
                    out.push(c);
                }
                c if (c as u32) < 32 || c as u32 == 127 => {
                    out.push_str(&format!("\\u{:04x}", c as u32));
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }
    let mut out = String::from("{\"schema\":1");
    member(&mut out, "titleId", TITLE_ID);
    member(&mut out, "location", location);
    member(&mut out, "contentVersion", content_version);
    let tag: String = release_tag.chars().take(128).collect();
    member(
        &mut out,
        "releaseTag",
        if tag.is_empty() { "unknown" } else { &tag },
    );
    member(&mut out, "sha256", sha256);
    member(&mut out, "installedAt", installed_at);
    out.push_str("}\n");
    out
}

/// A receipt's `(location, contentVersion)` when it is a well-formed one for this title.
pub(crate) fn receipt_fields(receipt: &[u8]) -> Option<(String, String)> {
    let v: Value = serde_json::from_slice(receipt).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let location = s("location")?;
    let cv = s("contentVersion")?;
    let sha = s("sha256")?;
    (s("titleId")? == TITLE_ID
        && clean_absolute_path(&location)
        && is_content_version(&cv)
        && sha.len() == 64
        && sha.bytes().all(|b| b.is_ascii_hexdigit()))
    .then_some((location, cv))
}

/// The store's ownership rule (`managed()`): a receipt for this location and the version there.
pub(crate) fn store_owns(receipt: &[u8], location: &str, installed_cv: Option<&str>) -> bool {
    receipt_fields(receipt)
        .is_some_and(|(loc, cv)| loc == location && Some(cv.as_str()) == installed_cv)
}

/// `YYYY-MM-DDTHH:MM:SSZ` (UTC), the store's `installedAt`.
pub(crate) fn utc_timestamp(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The order files are put in place: everything else (sorted, for a stable order), then
/// `sce_sys/param.json` last (the store's `overlay_tree`; a folder with a param.json reads as
/// ready to ShadowMount+).
pub(crate) fn place_order(rels: &[&str]) -> Vec<String> {
    let mut rest: Vec<String> = rels
        .iter()
        .filter(|r| **r != PARAM)
        .map(|r| r.to_string())
        .collect();
    rest.sort();
    if rels.contains(&PARAM) {
        rest.push(PARAM.into());
    }
    rest
}

/// The folders a set of files needs, parents first, the root first of all.
pub(crate) fn folders_for(root: &str, rels: &[&str]) -> Vec<String> {
    let mut out = vec![root.to_string()];
    for r in rels {
        let mut acc = root.to_string();
        let parts: Vec<&str> = r.split('/').collect();
        for p in &parts[..parts.len().saturating_sub(1)] {
            acc = format!("{acc}/{p}");
            if !out.contains(&acc) {
                out.push(acc.clone());
            }
        }
    }
    out
}

// ─── Console reads ─────────────────────────────────────────────────────────

/// A small console file, None when it is not there.
fn read_small(addr: &str, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
    match ps5upload_core::fs_ops::fs_read_with_timeout(
        addr,
        path,
        0,
        256 * 1024,
        Some(Duration::from_secs(15)),
        false,
    ) {
        Ok(b) => Ok(Some(b)),
        Err(e) if ps5upload_core::fs_ops::is_not_found(&format!("{e:#}")) => Ok(None),
        Err(e) => Err(e),
    }
}

fn folder_version(addr: &str, dir: &str) -> anyhow::Result<Option<String>> {
    Ok(read_small(addr, &format!("{dir}/{PARAM}"))?
        .and_then(|b| param_fields(&b))
        .filter(|(t, _)| t == TITLE_ID)
        .map(|(_, cv)| cv))
}

/// What is on the console: where the app is (or goes), its version, the store's state.
#[derive(Debug, Clone)]
struct Found {
    layout: Layout,
    installed: Option<String>,
    /// The store's receipt describes this install (its own rule).
    managed: bool,
    /// A store change is in progress (its journal is there).
    busy: bool,
}

fn find(addr: &str) -> anyhow::Result<Found> {
    let receipt = read_small(addr, STORE_RECEIPT)?;
    let busy = read_small(addr, STORE_JOURNAL)?.is_some();
    // Where the receipt says, when the app is there; else the default place.
    let mut choice = None;
    if let Some((loc, _)) = receipt.as_deref().and_then(receipt_fields) {
        if let Some(l) = layout(&loc) {
            if let Some(cv) = folder_version(addr, &l.target)? {
                choice = Some((l, Some(cv)));
            }
        }
    }
    let (layout, installed) = match choice {
        Some(c) => c,
        None => {
            let l = layout(DEFAULT_LOCATION).expect("the default location is valid");
            let cv = folder_version(addr, &l.target)?;
            (l, cv)
        }
    };
    let managed = receipt
        .as_deref()
        .is_some_and(|r| store_owns(r, &layout.location, installed.as_deref()));
    Ok(Found {
        layout,
        installed,
        managed,
        busy,
    })
}

/// The version an install record names (the console page writes `version` in the NN.NNN.NNN
/// form; `content_version` is the same).
fn record_version(r: &Value) -> Option<String> {
    r.get("content_version")
        .or_else(|| r.get("version"))
        .and_then(Value::as_str)
        .filter(|v| is_content_version(v))
        .map(str::to_string)
}

/// Our own record of the last install (who registered it), when it is there.
fn install_record(addr: &str) -> Option<Value> {
    read_small(addr, INSTALL_RECORD)
        .ok()
        .flatten()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

/// The app's process, when it runs: (app id, pid).
fn running(addr: &str) -> anyhow::Result<Option<(u32, i32)>> {
    let list = ps5upload_core::process_mgr::process_list(addr)?;
    let mut best: Option<(u32, i32)> = None;
    for p in list.processes {
        if p.kind == "app" && p.title_id == TITLE_ID && !p.is_self {
            // Prefer the process that carries Sony's app id (the one app-kill takes).
            if best.is_none_or(|(id, _)| id == 0 && p.app_id != 0) {
                best = Some((p.app_id, p.pid));
            }
        }
    }
    Ok(best)
}

/// Close the app (Sony's app-kill, then SIGKILL) and wait until it is gone. Ok(true) when it ran.
fn close(addr: &str) -> Result<bool, Refusal> {
    let Some((app_id, pid)) = running(addr).map_err(Refusal::console)? else {
        return Ok(false);
    };
    crate::log_at!(
        Info,
        "install",
        "closing PS5Upload Play on {addr} (app {app_id}, pid {pid})"
    );
    let mut killed = false;
    if app_id != 0 {
        killed = ps5upload_core::app_lifecycle::app_lifecycle(
            addr,
            ps5upload_core::app_lifecycle::AppAction::Kill,
            app_id,
        )
        .map(|a| a.ok)
        .unwrap_or(false);
    }
    if !killed && pid > 0 {
        let _ = ps5upload_core::process_mgr::process_kill(addr, pid);
    }
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(500));
        if running(addr).map_err(Refusal::console)?.is_none() {
            return Ok(true);
        }
    }
    Err(Refusal::conflict(
        "app_running",
        "PS5Upload Play is still open on the PS5 and would not close",
    ))
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub(crate) struct StoreState {
    pub managed: bool,
    pub busy: bool,
}

// ─── Refusals ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct Refusal {
    status: StatusCode,
    reason: &'static str,
    message: String,
}

impl Refusal {
    fn conflict(reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            reason,
            message: message.into(),
        }
    }
    fn console(e: anyhow::Error) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            reason: "console",
            message: format!("{e:#}"),
        }
    }
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": self.message, "reason": self.reason })),
        )
            .into_response()
    }
}

fn not_included(why: String) -> Refusal {
    if why == "not_included" {
        Refusal {
            status: StatusCode::NOT_FOUND,
            reason: "not_included",
            message: "PS5Upload Play is not included in this build of PS5Upload".into(),
        }
    } else {
        Refusal {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            reason: "bundle",
            message: format!("the PS5Upload Play copy in this build is not usable: {why}"),
        }
    }
}

fn store_busy() -> Refusal {
    Refusal::conflict(
        "store_busy",
        "another app has a change to an app on this PS5 in progress; open it to let it finish, then try again",
    )
}

/// One install or removal per console at a time.
#[derive(Debug)]
struct ConsoleJob(String);

fn jobs() -> &'static Mutex<HashSet<String>> {
    static JOBS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    JOBS.get_or_init(Default::default)
}

impl ConsoleJob {
    fn start(addr: &str) -> Result<Self, Refusal> {
        let mut j = jobs().lock().unwrap_or_else(|e| e.into_inner());
        if !j.insert(addr.to_string()) {
            return Err(Refusal::conflict(
                "busy",
                "PS5Upload Play is already being installed or removed on this PS5",
            ));
        }
        Ok(Self(addr.to_string()))
    }
}

impl Drop for ConsoleJob {
    fn drop(&mut self) {
        jobs()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

// ─── Status ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct PlayAppQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
}

fn registered(addr: &str) -> anyhow::Result<bool> {
    Ok(ps5upload_core::fs_ops::app_list_registered(addr)?
        .apps
        .iter()
        .any(|a| a.title_id == TITLE_ID))
}

fn status_blocking(addr: &str) -> anyhow::Result<Value> {
    let bundle = bundle_info();
    let found = find(addr)?;
    let is_registered = match &found.installed {
        Some(_) => registered(addr).ok(),
        None => None,
    };
    let running = running(addr).ok().map(|r| r.is_some());
    let kept = folder_version(addr, KEPT_DIR).unwrap_or(None);
    let bundled_cv = bundle.as_ref().ok().map(|b| b.content_version.clone());
    let action = action_for(bundled_cv.as_deref(), found.installed.as_deref());
    let record = install_record(addr);
    let mut out = json!({
        "title_id": TITLE_ID,
        "bundled": bundle.as_ref().ok().map(|b| json!({
            "version": human_version(&b.content_version),
            "content_version": b.content_version,
            "bytes": b.bytes,
            "files": b.files,
            "sha256": b.sha256,
        })),
        "installed": found.installed.as_ref().map(|cv| json!({
            "version": human_version(cv),
            "content_version": cv,
            "path": found.layout.target,
            "registered": is_registered,
            "registered_by": record.as_ref()
                .filter(|r| record_version(r).as_deref() == Some(cv.as_str()))
                .and_then(|r| r.get("registered_by").cloned()),
        })),
        "running": running,
        "kept": kept.as_ref().map(|cv| json!({
            "version": human_version(cv),
            "content_version": cv,
            "path": KEPT_DIR,
        })),
        "store": StoreState { managed: found.managed, busy: found.busy },
        "action": action,
    });
    if let Err(why) = &bundle {
        out["bundled_missing"] = json!(why);
    }
    Ok(out)
}

pub(crate) async fn installed(
    State(state): State<AppState>,
    Query(q): Query<PlayAppQuery>,
) -> Response {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    match tokio::task::spawn_blocking(move || status_blocking(&addr)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => Refusal::console(e).into_response(),
        Err(e) => Refusal::console(anyhow::anyhow!("{e}")).into_response(),
    }
}

// ─── Install / update ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct PlayAppInstallBody {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    /// `quick` (default: the new files over the installed ones) or `clean` (the folder replaced,
    /// the old one kept in the store's `previous` folder).
    #[serde(default)]
    mode: Option<String>,
    /// Put this version in place even when it is not newer than the installed one.
    #[serde(default)]
    replace: bool,
    /// The user agreed to update an install the other app made.
    #[serde(default)]
    confirm_store: bool,
}

fn write(addr: &str, path: &str, bytes: &[u8]) -> Result<(), Refusal> {
    ps5upload_core::diagnostics::fs_write_bytes_large(addr, path, bytes, false)
        .map(|_| ())
        .map_err(|e| Refusal::console(e.context(format!("writing {path}"))))
}

fn mkdirs(addr: &str, root: &str, rels: &[&str]) -> Result<(), Refusal> {
    for d in folders_for(root, rels) {
        ps5upload_core::fs_ops::fs_mkdir(addr, &d)
            .map_err(|e| Refusal::console(e.context(format!("making {d}"))))?;
    }
    Ok(())
}

fn chmod_777(addr: &str, path: &str, recursive: bool) -> Result<(), Refusal> {
    ps5upload_core::fs_ops::fs_chmod_with_timeout(
        addr,
        path,
        "0777",
        recursive,
        Some(Duration::from_secs(300)),
    )
    .map_err(|e| Refusal::console(e.context(format!("opening {path} to all (0777)"))))
}

fn delete_if_there(addr: &str, path: &str) -> anyhow::Result<bool> {
    match ps5upload_core::fs_ops::fs_exists(addr, path)? {
        false => Ok(false),
        true => {
            ps5upload_core::fs_ops::fs_delete_with_timeout(
                addr,
                path,
                Some(Duration::from_secs(300)),
            )?;
            Ok(true)
        }
    }
}

fn mv(addr: &str, from: &str, to: &str, replace: bool) -> Result<(), Refusal> {
    ps5upload_core::fs_ops::fs_move_with_timeout(addr, from, to, replace, None)
        .map_err(|e| Refusal::console(e.context(format!("moving {from} to {to}"))))
}

/// The app's folder written whole at `dir` (param.json last), every folder and file 0777.
fn write_tree(addr: &str, dir: &str, files: &[(String, Vec<u8>)]) -> Result<(), Refusal> {
    let rels: Vec<&str> = files.iter().map(|(r, _)| r.as_str()).collect();
    mkdirs(addr, dir, &rels)?;
    for rel in place_order(&rels) {
        let bytes = &files.iter().find(|(r, _)| *r == rel).expect("listed").1;
        write(addr, &format!("{dir}/{rel}"), bytes)?;
    }
    chmod_777(addr, dir, true)
}

/// The copy for the console page: written again unless it is already this version.
fn keep_copy(addr: &str, info: &BundleInfo, files: &[(String, Vec<u8>)]) -> Result<bool, Refusal> {
    if folder_version(addr, KEPT_DIR)
        .map_err(Refusal::console)?
        .as_deref()
        == Some(info.content_version.as_str())
    {
        return Ok(false);
    }
    delete_if_there(addr, KEPT_DIR)
        .map_err(|e| Refusal::console(e.context("clearing the old copy")))?;
    write_tree(addr, KEPT_DIR, files)?;
    write(addr, KEPT_SHA, info.sha256.as_bytes())?;
    Ok(true)
}

/// The store's `refresh_registered`: the console's copies of `sce_sys` brought up to date after
/// an update. Best effort.
fn refresh_registered(addr: &str, files: &[(String, Vec<u8>)]) -> usize {
    let app = format!("/user/app/{TITLE_ID}");
    let mut done = 0;
    for folder in [
        format!("/user/appmeta/{TITLE_ID}"),
        format!("{app}/sce_sys"),
    ] {
        if !ps5upload_core::fs_ops::fs_exists(addr, &folder).unwrap_or(false) {
            continue;
        }
        for (rel, bytes) in files {
            if let Some(name) = rel.strip_prefix("sce_sys/").filter(|n| !n.contains('/')) {
                if write(addr, &format!("{folder}/{name}"), bytes).is_ok() {
                    done += 1;
                }
            }
        }
    }
    if ps5upload_core::fs_ops::fs_exists(addr, &format!("{app}/icon0.png")).unwrap_or(false) {
        if let Some((_, b)) = files.iter().find(|(r, _)| r == "sce_sys/icon0.png") {
            if write(addr, &format!("{app}/icon0.png"), b).is_ok() {
                done += 1;
            }
        }
    }
    done
}

/// Whether ShadowMount+ will register a folder at `location`: it runs and scans that place.
fn smp_registers(addr: &str, location: &str) -> bool {
    let Ok(s) = ps5upload_core::smp::collect_status(addr) else {
        return false;
    };
    if !s.running {
        return false;
    }
    let manual = read_small(addr, SMP_MANUAL)
        .ok()
        .flatten()
        .unwrap_or_default();
    let policy = scan_policy(
        s.config_ini.as_deref().unwrap_or(""),
        &String::from_utf8_lossy(&manual),
    );
    policy.roots.iter().any(|r| r == location)
}

fn install_blocking(
    addr: &str,
    mode: &str,
    replace: bool,
    confirm_store: bool,
) -> Result<Value, Refusal> {
    let _job = ConsoleJob::start(addr)?;
    let (info, files) = bundle_files().map_err(not_included)?;
    let found = find(addr).map_err(Refusal::console)?;
    let l = found.layout.clone();
    if found.busy {
        return Err(store_busy());
    }
    let ours = install_record(addr)
        .and_then(|r| record_version(&r))
        .is_some_and(|cv| Some(cv) == found.installed);
    if found.managed && !ours && !confirm_store {
        return Err(Refusal::conflict(
            "store_managed",
            "PS5Upload Play on this PS5 was installed by another app; confirm to update it",
        ));
    }
    let update = found.installed.is_some();
    if !update && ps5upload_core::fs_ops::fs_exists(addr, &l.target).map_err(Refusal::console)? {
        return Err(Refusal::conflict(
            "unexpected",
            format!("{} holds something that is not PS5Upload Play", l.target),
        ));
    }
    if let Some(cv) = &found.installed {
        if !replace && cv != &info.content_version && !newer(cv, &info.content_version) {
            return Err(Refusal::conflict(
                "not_newer",
                format!(
                    "PS5Upload Play {} on the PS5 is newer than the copy here ({})",
                    human_version(cv),
                    human_version(&info.content_version)
                ),
            ));
        }
    }
    let clean = update && mode == "clean";
    // The store's work folders must be out of ShadowMount+'s sight.
    let config = read_small(addr, SMP_CONFIG)
        .map_err(Refusal::console)?
        .unwrap_or_default();
    let manual = read_small(addr, SMP_MANUAL)
        .map_err(Refusal::console)?
        .unwrap_or_default();
    let policy = scan_policy(
        &String::from_utf8_lossy(&config),
        &String::from_utf8_lossy(&manual),
    );
    for p in [&l.staging, &l.backup, &l.trash, &l.previous] {
        if let Some(why) = work_path_conflict(&policy, p) {
            return Err(Refusal::conflict(
                "work_scanned",
                format!("the install's work folder {p} would be scanned ({why})"),
            ));
        }
    }
    if clean && ps5upload_core::fs_ops::fs_exists(addr, &l.backup).map_err(Refusal::console)? {
        return Err(Refusal::conflict(
            "backup_left",
            format!(
                "an earlier update left {} behind; it needs a look first",
                l.backup
            ),
        ));
    }

    let started = Instant::now();
    crate::log_at!(
        Info,
        "install",
        "PS5Upload Play {} on {addr} at {} ({}; installed: {:?}, store-managed: {})",
        info.content_version,
        l.target,
        if !update {
            "install"
        } else if clean {
            "clean update"
        } else {
            "quick update"
        },
        found.installed,
        found.managed
    );
    let closed = close(addr)?;
    crate::folder_sizes::changed(addr, &[&l.target, &l.work, KEPT_DIR]);
    let kept = keep_copy(addr, &info, &files)?;

    // Staging: the app's folder whole, 0777.
    delete_if_there(addr, &l.staging)
        .map_err(|e| Refusal::console(e.context("cleaning the staging folder")))?;
    for d in [l.work.clone(), format!("{}/staging", l.work)] {
        ps5upload_core::fs_ops::fs_mkdir(addr, &d)
            .map_err(|e| Refusal::console(e.context(format!("making {d}"))))?;
    }
    write_tree(addr, &l.staging, &files)?;

    let mut previous_kept = None;
    if !update {
        ps5upload_core::fs_ops::fs_mkdir(addr, &l.location)
            .map_err(|e| Refusal::console(e.context(format!("making {}", l.location))))?;
        mv(addr, &l.staging, &l.target, false)?;
    } else if clean {
        ps5upload_core::fs_ops::fs_mkdir(addr, &format!("{}/backup", l.work))
            .map_err(|e| Refusal::console(e.context("making the backup folder")))?;
        mv(addr, &l.target, &l.backup, false)?;
        if let Err(e) = mv(addr, &l.staging, &l.target, false) {
            // Put the old one back, as the store does.
            let _ = mv(addr, &l.backup, &l.target, false);
            return Err(e);
        }
        ps5upload_core::fs_ops::fs_mkdir(addr, &format!("{}/previous", l.work))
            .map_err(|e| Refusal::console(e.context("making the previous folder")))?;
        delete_if_there(addr, &l.previous)
            .map_err(|e| Refusal::console(e.context("replacing the earlier previous folder")))?;
        mv(addr, &l.backup, &l.previous, false)?;
        previous_kept = Some(l.previous.clone());
    } else {
        // Quick update: folders made (0777), then each file over its counterpart, param.json last.
        let rels: Vec<&str> = files.iter().map(|(r, _)| r.as_str()).collect();
        for d in folders_for(&l.target, &rels) {
            ps5upload_core::fs_ops::fs_mkdir(addr, &d)
                .map_err(|e| Refusal::console(e.context(format!("making {d}"))))?;
            chmod_777(addr, &d, false)?;
        }
        for rel in place_order(&rels) {
            mv(
                addr,
                &format!("{}/{rel}", l.staging),
                &format!("{}/{rel}", l.target),
                true,
            )?;
        }
        delete_if_there(addr, &l.staging)
            .map_err(|e| Refusal::console(e.context("cleaning the staging folder")))?;
    }

    // The store's receipt, so it manages this install like one of its own.
    let at = utc_timestamp(now_unix());
    let receipt = receipt_text(
        &l.location,
        &info.content_version,
        &format!("v{}", human_version(&info.content_version)),
        &info.sha256,
        &at,
    );
    let receipt_error = ps5upload_core::fs_ops::fs_mkdir(addr, &format!("{STORE_ROOT}/receipts"))
        .and_then(|_| {
            ps5upload_core::diagnostics::fs_write_bytes(
                addr,
                STORE_RECEIPT,
                receipt.as_bytes(),
                false,
            )
        })
        .err()
        .map(|e| format!("{e:#}"));
    let refreshed = if update {
        refresh_registered(addr, &files)
    } else {
        0
    };

    // Registering: ShadowMount+'s, as for the store's installs; ours only without it.
    let already = registered(addr).unwrap_or(false);
    let smp = smp_registers(addr, &l.location);
    let mut register_error = None;
    let registered_by;
    let mut pending = false;
    if smp {
        registered_by = "shadowmount";
        if !already {
            let until = Instant::now() + SMP_REGISTER_WAIT;
            pending = true;
            while Instant::now() < until {
                std::thread::sleep(Duration::from_secs(3));
                if registered(addr).unwrap_or(false) {
                    pending = false;
                    break;
                }
            }
        }
    } else {
        registered_by = "ps5upload";
        // Again after a clean update too: a bind made by an earlier registration followed the
        // folder that was moved aside.
        if !already || clean {
            if let Err(e) = ps5upload_core::fs_ops::app_register(addr, &l.target, false) {
                register_error = Some(format!("{e:#}"));
            }
        }
    }
    let record = json!({
        "version": info.content_version,
        "content_version": info.content_version,
        "location": l.location,
        "registered_by": registered_by,
        "installed_at": at,
    });
    let _ = ps5upload_core::diagnostics::fs_write_bytes(
        addr,
        INSTALL_RECORD,
        record.to_string().as_bytes(),
        false,
    );
    let ms = started.elapsed().as_millis();
    match &register_error {
        None => crate::log_at!(
            Info,
            "install",
            "PS5Upload Play {} on {addr}: in place in {ms} ms; registered by {registered_by}{}",
            info.content_version,
            if pending { " (not listed yet)" } else { "" }
        ),
        Some(e) => crate::log_at!(
            Warn,
            "install",
            "PS5Upload Play {} on {addr}: in place in {ms} ms, but registering failed: {e}",
            info.content_version
        ),
    }
    if let Some(e) = &receipt_error {
        crate::log_at!(Warn, "install", "PS5Upload Play receipt on {addr}: {e}");
    }
    Ok(json!({
        "ok": register_error.is_none(),
        "operation": if !update { "install" } else { "update" },
        "mode": if !update { "install" } else if clean { "clean" } else { "quick" },
        "version": human_version(&info.content_version),
        "content_version": info.content_version,
        "previous": found.installed.as_deref().map(human_version),
        "path": l.target,
        "registered_by": registered_by,
        "registration_pending": pending,
        "register_error": register_error,
        "receipt_error": receipt_error,
        "previous_kept_at": previous_kept,
        "refreshed_files": refreshed,
        "closed_app": closed,
        "kept_copy_written": kept,
        "replaced_store_install": found.managed && !ours,
    }))
}

pub(crate) async fn install(Json(req): Json<PlayAppInstallBody>) -> Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let mode = req.mode.unwrap_or_else(|| "quick".into());
    if mode != "quick" && mode != "clean" {
        return Refusal {
            status: StatusCode::BAD_REQUEST,
            reason: "mode",
            message: "mode is quick or clean".into(),
        }
        .into_response();
    }
    let Some(busy) = crate::helper_stop::try_enter(&addr) else {
        return crate::helper_stop::stopping_refusal();
    };
    let (replace, confirm) = (req.replace, req.confirm_store);
    match tokio::task::spawn_blocking(move || {
        let _busy = busy;
        install_blocking(&addr, &mode, replace, confirm)
    })
    .await
    {
        Ok(Ok(v)) => {
            let status = if v["ok"] == json!(true) {
                StatusCode::OK
            } else {
                StatusCode::BAD_GATEWAY
            };
            (status, Json(v)).into_response()
        }
        Ok(Err(r)) => r.into_response(),
        Err(e) => Refusal::console(anyhow::anyhow!("{e}")).into_response(),
    }
}

// ─── Remove ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct PlayAppRemoveBody {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    /// Also delete the app's own data and its download data.
    #[serde(default)]
    delete_data: bool,
    #[serde(default)]
    confirm_store: bool,
}

fn remove_blocking(addr: &str, delete_data: bool, confirm_store: bool) -> Result<Value, Refusal> {
    let _job = ConsoleJob::start(addr)?;
    let found = find(addr).map_err(Refusal::console)?;
    let l = found.layout.clone();
    if found.busy {
        return Err(store_busy());
    }
    let ours = install_record(addr)
        .and_then(|r| record_version(&r))
        .is_some_and(|cv| Some(cv) == found.installed);
    if found.managed && !ours && !confirm_store {
        return Err(Refusal::conflict(
            "store_managed",
            "PS5Upload Play on this PS5 was installed by another app; confirm to remove it",
        ));
    }
    let was_registered = registered(addr).map_err(Refusal::console)?;
    let closed = close(addr)?;
    crate::log_at!(
        Info,
        "install",
        "remove PS5Upload Play from {addr} ({}: {:?}, registered: {was_registered}, delete data: {delete_data})",
        l.target,
        found.installed
    );
    crate::folder_sizes::changed(addr, &[&l.target, &l.work, KEPT_DIR, DATA_DIR]);
    // The store's uninstall: out of the scanned place in one rename, then deleted.
    let mut deleted: Vec<String> = Vec::new();
    let mut kept: Vec<Value> = Vec::new();
    if found.installed.is_some() {
        ps5upload_core::fs_ops::fs_mkdir(addr, &format!("{}/trash", l.work))
            .map_err(|e| Refusal::console(e.context("making the trash folder")))?;
        delete_if_there(addr, &l.trash)
            .map_err(|e| Refusal::console(e.context("emptying the trash folder")))?;
        mv(addr, &l.target, &l.trash, false)?;
        match delete_if_there(addr, &l.trash) {
            Ok(_) => deleted.push(l.target.clone()),
            Err(e) => kept.push(json!({"path": l.trash, "error": format!("{e:#}")})),
        }
    }
    // Its receipt, when it describes this install (the store's rule: never another location's).
    if let Ok(Some(r)) = read_small(addr, STORE_RECEIPT) {
        if receipt_fields(&r).is_some_and(|(loc, _)| loc == l.location) {
            match ps5upload_core::fs_ops::fs_delete(addr, STORE_RECEIPT) {
                Ok(()) => deleted.push(STORE_RECEIPT.into()),
                Err(e) => kept.push(json!({"path": STORE_RECEIPT, "error": format!("{e:#}")})),
            }
        }
    }
    // Then off the home screen (Sony's uninstall), as the store does once the folder is gone.
    let mut unregister = None;
    if was_registered {
        match ps5upload_core::fs_ops::app_unregister_scoped(addr, TITLE_ID, false) {
            Ok(o) => {
                unregister = Some(json!({
                    "sony_refused": o.sony_refused(),
                    "still_present": o.still_present,
                }))
            }
            Err(e) => kept.push(json!({"path": "registration", "error": format!("{e:#}")})),
        }
    }
    let mut extra = vec![KEPT_DIR, KEPT_SHA, INSTALL_RECORD];
    if delete_data {
        extra.extend([DATA_DIR, DOWNLOAD_DIR]);
    }
    for p in extra {
        match delete_if_there(addr, p) {
            Ok(true) => deleted.push(p.into()),
            Ok(false) => {}
            Err(e) => kept.push(json!({"path": p, "error": format!("{e:#}")})),
        }
    }
    if kept.is_empty() {
        crate::log_at!(
            Info,
            "install",
            "remove PS5Upload Play from {addr}: deleted {deleted:?}"
        );
    } else {
        crate::log_at!(
            Warn,
            "install",
            "remove PS5Upload Play from {addr}: deleted {deleted:?}, kept {kept:?}"
        );
    }
    Ok(json!({
        "ok": kept.is_empty(),
        "was_installed": found.installed.is_some(),
        "unregistered": was_registered,
        "unregister": unregister,
        "closed_app": closed,
        "deleted": deleted,
        "kept": kept,
    }))
}

pub(crate) async fn remove(Json(req): Json<PlayAppRemoveBody>) -> Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let Some(busy) = crate::helper_stop::try_enter(&addr) else {
        return crate::helper_stop::stopping_refusal();
    };
    let (delete_data, confirm) = (req.delete_data, req.confirm_store);
    crate::icon_cache::invalidate_console(&addr);
    match tokio::task::spawn_blocking(move || {
        let _busy = busy;
        remove_blocking(&addr, delete_data, confirm)
    })
    .await
    {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(r)) => r.into_response(),
        Err(e) => Refusal::console(anyhow::anyhow!("{e}")).into_response(),
    }
}

// ─── Open ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct PlayAppOpenBody {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    /// Go ahead even though friends are playing through Play Together (together_guard.rs).
    #[serde(default, deserialize_with = "crate::together_guard::de_confirm")]
    confirm_friends: bool,
}

/// Open PS5Upload Play on the console.
///
/// Opening it replaces the game running now: with friends playing through Play Together, 409
/// `friends_playing` unless confirmed (together_guard.rs).
pub(crate) async fn open(
    headers: axum::http::HeaderMap,
    axum::extract::Query(confirm): axum::extract::Query<crate::together_guard::ConfirmQuery>,
    Json(req): Json<PlayAppOpenBody>,
) -> Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    if let Some(r) =
        crate::together_guard::check(Some(&addr), &headers, req.confirm_friends || confirm.yes())
    {
        return r;
    }
    match tokio::task::spawn_blocking(move || ps5upload_core::fs_ops::app_launch(&addr, TITLE_ID))
        .await
    {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => Refusal {
            status: StatusCode::BAD_GATEWAY,
            reason: "launch",
            message: format!("{e:#}"),
        }
        .into_response(),
        Err(e) => Refusal::console(anyhow::anyhow!("{e}")).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in files {
            if name.ends_with('/') {
                w.add_directory(*name, opts).unwrap();
            } else {
                w.start_file(*name, opts).unwrap();
                w.write_all(body).unwrap();
            }
        }
        w.finish().unwrap().into_inner()
    }

    fn param(cv: &str) -> Vec<u8> {
        format!(r#"{{"titleId":"FAKE99261","contentVersion":"{cv}","applicationCategoryType":0}}"#)
            .into_bytes()
    }

    #[test]
    fn versions_read_both_ways_and_compare_as_the_store_does() {
        assert_eq!(version_key("13.000.000"), Some((13, 0, 0)));
        assert_eq!(human_version("13.000.000"), "13.0.0");
        assert_eq!(human_version("12.006.044"), "12.6.44");
        assert_eq!(human_version("odd"), "odd");
        assert!(is_content_version("13.000.000"));
        assert!(!is_content_version("13.0.0"));
        assert!(!is_content_version("13.000.00a"));
        assert!(newer("12.006.044", "13.000.000"));
        assert!(!newer("13.000.000", "13.000.000"));
        assert!(!newer("13.0.0", "13.000.001"));
    }

    #[test]
    fn a_release_zip_is_read_and_checked() {
        let p = param("13.000.000");
        let z = zip_of(&[
            ("FAKE99261/", b""),
            ("FAKE99261/eboot.bin", b"\x7fELF"),
            ("FAKE99261/sce_sys/param.json", &p),
            ("FAKE99261/sce_sys/icon0.png", b"png"),
        ]);
        let (info, files) = read_zip(&z, true).unwrap();
        assert_eq!(info.content_version, "13.000.000");
        assert_eq!(info.files, 3);
        assert_eq!(info.sha256, sha256_hex(&z));
        assert_eq!(info.sha256.len(), 64);
        let mut names: Vec<_> = files.iter().map(|(r, _)| r.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            ["eboot.bin", "sce_sys/icon0.png", "sce_sys/param.json"]
        );
        assert!(read_zip(&z, false).unwrap().1.is_empty());
    }

    #[test]
    fn a_bad_zip_is_refused() {
        let p = param("13.000.000");
        let no_eboot = zip_of(&[("FAKE99261/sce_sys/param.json", &p)]);
        assert!(read_zip(&no_eboot, false)
            .unwrap_err()
            .contains("eboot.bin"));
        let other_top = zip_of(&[
            ("FAKE99261/eboot.bin", b"x"),
            ("FAKE99261/sce_sys/param.json", &p),
            ("other/file", b"x"),
        ]);
        assert!(read_zip(&other_top, false).unwrap_err().contains("outside"));
        let other_title = zip_of(&[
            ("FAKE99261/eboot.bin", b"x"),
            (
                "FAKE99261/sce_sys/param.json",
                br#"{"titleId":"FAKE99260","contentVersion":"13.000.000"}"#,
            ),
        ]);
        assert!(read_zip(&other_title, false)
            .unwrap_err()
            .contains("FAKE99260"));
        let short_version = zip_of(&[
            ("FAKE99261/eboot.bin", b"x"),
            (
                "FAKE99261/sce_sys/param.json",
                br#"{"titleId":"FAKE99261","contentVersion":"13.0.0"}"#,
            ),
        ]);
        assert!(read_zip(&short_version, false)
            .unwrap_err()
            .contains("version"));
        assert!(read_zip(b"not a zip", false)
            .unwrap_err()
            .contains("not a zip"));
    }

    /// The store's folders (transaction.cpp `resolve()`), byte for byte.
    #[test]
    fn the_layout_is_the_stores() {
        let l = layout("/data/homebrew").unwrap();
        assert_eq!(l.target, "/data/homebrew/FAKE99261");
        assert_eq!(l.work, "/data/prosperostore");
        assert_eq!(l.staging, "/data/prosperostore/staging/FAKE99261");
        assert_eq!(l.backup, "/data/prosperostore/backup/FAKE99261");
        assert_eq!(l.previous, "/data/prosperostore/previous/FAKE99261");
        assert_eq!(l.trash, "/data/prosperostore/trash/FAKE99261");
        let usb = layout("/mnt/usb1/homebrew/").unwrap();
        assert_eq!(usb.target, "/mnt/usb1/homebrew/FAKE99261");
        assert_eq!(usb.work, "/mnt/usb1/prosperostore");
        assert_eq!(drive_root("/mnt/ext0"), Some("/mnt/ext0".into()));
        assert_eq!(drive_root("/mnt/usb8/x"), None);
        assert_eq!(drive_root("/data2/x"), None);
        assert!(layout("/opt/homebrew").is_none());
        assert!(layout("/data/../etc").is_none());
        assert!(layout("relative").is_none());
    }

    /// Its receipt (catalog.cpp `format_receipt`), byte for byte, and its ownership rule.
    #[test]
    fn the_receipt_is_the_stores() {
        let r = receipt_text(
            "/data/homebrew",
            "13.000.000",
            "v13.0.0",
            &"a".repeat(64),
            "2026-11-27T08:00:00Z",
        );
        assert_eq!(
            r,
            format!(
                "{{\"schema\":1,\"titleId\":\"FAKE99261\",\"location\":\"/data/homebrew\",\
                 \"contentVersion\":\"13.000.000\",\"releaseTag\":\"v13.0.0\",\"sha256\":\"{}\",\
                 \"installedAt\":\"2026-11-27T08:00:00Z\"}}\n",
                "a".repeat(64)
            )
        );
        assert_eq!(
            receipt_fields(r.as_bytes()),
            Some(("/data/homebrew".into(), "13.000.000".into()))
        );
        assert!(store_owns(
            r.as_bytes(),
            "/data/homebrew",
            Some("13.000.000")
        ));
        // Changed since by something else: the receipt no longer describes the folder.
        assert!(!store_owns(
            r.as_bytes(),
            "/data/homebrew",
            Some("13.001.000")
        ));
        assert!(!store_owns(
            r.as_bytes(),
            "/mnt/usb0/homebrew",
            Some("13.000.000")
        ));
        assert!(!store_owns(r.as_bytes(), "/data/homebrew", None));
        assert!(receipt_fields(br#"{"titleId":"FAKE99261","location":"/data/homebrew","contentVersion":"13.000.000","sha256":"xyz"}"#).is_none());
        assert!(receipt_fields(b"{").is_none());
        // An empty tag is "unknown", and text is escaped as the store escapes it.
        let odd = receipt_text("/data/a\"b", "13.000.000", "", &"0".repeat(64), "t");
        assert!(odd.contains(r#""location":"/data/a\"b""#), "{odd}");
        assert!(odd.contains(r#""releaseTag":"unknown""#), "{odd}");
    }

    #[test]
    fn installed_at_is_utc_in_the_stores_form() {
        assert_eq!(utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc_timestamp(1_795_737_600 + 3_661), "2026-11-27T01:01:01Z");
    }

    /// ShadowMount+'s scan places as the store reads them (locations.cpp `scan_policy`), and its
    /// refusal of a work folder ShadowMount+ would scan.
    #[test]
    fn work_folders_stay_out_of_shadowmounts_sight() {
        let d = scan_policy("", "");
        assert!(d.roots.contains(&"/data/homebrew".to_string()));
        assert!(d.roots.contains(&"/mnt/usb7".to_string()));
        assert!(d.roots.contains(&"/mnt/shadowmnt".to_string()));
        assert_eq!(d.depth, 1);
        let l = layout("/data/homebrew").unwrap();
        for p in [&l.staging, &l.backup, &l.previous, &l.trash] {
            assert_eq!(work_path_conflict(&d, p), None, "{p}");
        }
        // A scan of /data itself, two deep, would see /data/prosperostore/staging/FAKE99261? No:
        // that is three below. One of /data/prosperostore would.
        let wide = scan_policy("scanpath = \"/data\"\nrecursive_scan=yes\n", "");
        assert_eq!(wide.roots[0], "/data");
        assert_eq!(wide.depth, 2);
        assert_eq!(work_path_conflict(&wide, &l.staging), None);
        let close = scan_policy("scanpath=/data/prosperostore/staging\n", "");
        assert!(work_path_conflict(&close, &l.staging).is_some());
        let manual = scan_policy("", "/data/prosperostore/trash/FAKE99261\n# note\n");
        assert!(work_path_conflict(&manual, &l.trash).is_some());
        assert!(work_path_conflict(&manual, &l.staging).is_none());
        assert!(scan_policy("scan_depth=2\n", "").depth == 2);
        assert!(scan_policy("scan_depth=7\n", "").depth == 1);
    }

    #[test]
    fn param_json_goes_in_last() {
        let order = place_order(&[
            "sce_sys/param.json",
            "eboot.bin",
            "sce_sys/icon0.png",
            "a.prx",
        ]);
        assert_eq!(
            order,
            [
                "a.prx",
                "eboot.bin",
                "sce_sys/icon0.png",
                "sce_sys/param.json"
            ]
        );
        assert_eq!(
            folders_for(
                "/r",
                &["eboot.bin", "sce_sys/param.json", "sce_module/a/b.prx"]
            ),
            ["/r", "/r/sce_sys", "/r/sce_module", "/r/sce_module/a"]
        );
    }

    #[test]
    fn update_is_offered_only_for_a_newer_copy() {
        assert_eq!(action_for(None, None), "unavailable");
        assert_eq!(action_for(None, Some("13.000.000")), "unavailable");
        assert_eq!(action_for(Some("13.000.000"), None), "install");
        assert_eq!(action_for(Some("13.001.000"), Some("13.000.000")), "update");
        assert_eq!(action_for(Some("13.000.000"), Some("13.000.000")), "none");
        assert_eq!(
            action_for(Some("13.000.000"), Some("13.001.000")),
            "newer_installed"
        );
        assert_eq!(action_for(Some("13.000.000"), Some("weird")), "update");
    }

    #[tokio::test]
    async fn changes_need_a_console() {
        let r = install(Json(PlayAppInstallBody {
            addr: None,
            mode: None,
            replace: false,
            confirm_store: false,
        }))
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let r = remove(Json(PlayAppRemoveBody {
            addr: None,
            delete_data: false,
            confirm_store: false,
        }))
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let r = open(
            Default::default(),
            axum::extract::Query(Default::default()),
            Json(PlayAppOpenBody {
                addr: None,
                confirm_friends: false,
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn one_job_per_console() {
        let a = ConsoleJob::start("192.0.2.7").unwrap();
        assert_eq!(ConsoleJob::start("192.0.2.7").unwrap_err().reason, "busy");
        let _b = ConsoleJob::start("192.0.2.8").unwrap();
        drop(a);
        assert!(ConsoleJob::start("192.0.2.7").is_ok());
    }
}
