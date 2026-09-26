//! HTTP surface for the backport library corpus.
//!
//! The corpus is Sony system libraries the user supplies from games they own —
//! we cannot ship them. Two ways in: import a pack they already have, or scan a
//! console and harvest the `fakelib/` of games that are already backported.
//! Both land in one persistent corpus that every later backport reuses.
//!
//! This lives in the engine rather than the desktop shell so the browser build
//! is not a second-class citizen — it has no filesystem of its own, and a
//! Tauri-only implementation would leave it unable to acquire libraries at all.
//! It also keeps ~58 MB of library bytes out of the renderer.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::{Multipart, Path as AxumPath, Query};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use ps5upload_core::fakelibs::{is_library_name, Corpus, IncomingLibrary, Origin};
use ps5upload_core::fs_ops::{fs_read, list_dir, ListDirOptions};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Where the corpus lives.
///
/// Beside the settings and logs the desktop app already writes to
/// `~/.ps5upload`, and NOT under `cache/` — a cache can be regenerated at will,
/// but rebuilding this needs the user's console awake or their original files
/// back. `PS5UPLOAD_FAKELIBS_DIR` exists so a container can mount a volume,
/// matching the `PS5UPLOAD_CACHE_DIR` escape hatch next door.
pub fn corpus_root() -> Option<PathBuf> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| {
        if let Ok(v) = std::env::var("PS5UPLOAD_FAKELIBS_DIR") {
            if !v.trim().is_empty() {
                return Some(PathBuf::from(v));
            }
        }
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()?;
        (!home.trim().is_empty()).then(|| PathBuf::from(home).join(".ps5upload").join("fakelibs"))
    })
    .clone()
}

fn err(code: StatusCode, msg: impl Into<String>) -> axum::response::Response {
    (code, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

fn no_home() -> axum::response::Response {
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "no home directory, so there is nowhere to keep the library corpus. \
         Set PS5UPLOAD_FAKELIBS_DIR to choose a location.",
    )
}

// ─── GET /api/fakelibs/corpus ────────────────────────────────────────────────

/// Manifest plus the counts Settings shows. An absent corpus is not an error:
/// most users will not have one yet, and the UI needs to offer the two ways to
/// build one rather than report a failure they cannot act on.
pub async fn get_corpus() -> impl IntoResponse {
    let Some(root) = corpus_root() else {
        return no_home();
    };
    let corpus = Corpus::open(&root);
    let m = corpus.manifest();
    let builds: usize = m.libraries.iter().map(|l| l.builds.len()).sum();
    let bytes: u64 = m
        .libraries
        .iter()
        .flat_map(|l| &l.builds)
        .map(|b| b.size)
        .sum();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "root": root.to_string_lossy(),
            "manifest": m,
            "summary": { "sets": m.sets.len(), "builds": builds, "bytes": bytes },
        })),
    )
        .into_response()
}

// ─── DELETE /api/fakelibs/set/:id ────────────────────────────────────────────

pub async fn delete_set(AxumPath(id): AxumPath<String>) -> impl IntoResponse {
    let Some(root) = corpus_root() else {
        return no_home();
    };
    let mut corpus = Corpus::open(&root);
    match corpus.delete_set(&id) {
        Ok(true) => (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response(),
        Ok(false) => err(StatusCode::NOT_FOUND, format!("no set {id}")),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
}

// ─── POST /api/fakelibs/import ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ImportQuery {
    /// What to call this set in the UI.
    pub label: String,
    /// Where the user got it, for the origin record. Free text.
    #[serde(default)]
    pub source: String,
}

/// One import becomes ONE set, kept whole.
///
/// Not a pool of loose files: a library set is only known to work as the unit
/// somebody actually shipped, and picking a build per name across imports
/// manufactures a combination nobody has run. Files that are not libraries are
/// dropped silently (an AppleDouble `._x.sprx` sidecar sits beside every real
/// file on a Mac); an import with no libraries at all is refused, because that
/// means the user picked the wrong folder and should be told.
pub async fn import(Query(q): Query<ImportQuery>, mut form: Multipart) -> impl IntoResponse {
    let Some(root) = corpus_root() else {
        return no_home();
    };
    let mut libraries = Vec::new();
    let mut ignored = Vec::new();
    loop {
        let field = match form.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return err(StatusCode::BAD_REQUEST, format!("malformed upload: {e}")),
        };
        let name = field.file_name().unwrap_or_default().to_string();
        let data = match field.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => return err(StatusCode::BAD_REQUEST, format!("reading {name}: {e}")),
        };
        // Take the basename: browsers send "folder/file.sprx" for directory
        // uploads, and a name with a separator must never reach the store.
        let base = name.rsplit(['/', '\\']).next().unwrap_or(&name).to_string();
        if is_library_name(&base) {
            libraries.push(IncomingLibrary { name: base, data });
        } else if !base.is_empty() {
            ignored.push(base);
        }
    }

    let label = if q.label.trim().is_empty() {
        "Imported set"
    } else {
        q.label.trim()
    };
    let origin = Origin::Import {
        source: q.source.clone(),
        at: now_iso(),
    };
    let mut corpus = Corpus::open(&root);
    match corpus.add_set(label, origin, libraries) {
        Ok(Some(id)) => (
            StatusCode::OK,
            Json(serde_json::json!({ "ok": true, "set_id": id, "ignored": ignored })),
        )
            .into_response(),
        // Already present. Not an error — it is what makes re-importing safe.
        Ok(None) => (
            StatusCode::OK,
            Json(
                serde_json::json!({ "ok": true, "set_id": null, "duplicate": true,
                                     "ignored": ignored }),
            ),
        )
            .into_response(),
        Err(e) => err(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")),
    }
}

// ─── POST /api/fakelibs/scan ─────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct ScanTitle {
    pub title_id: String,
    #[serde(default)]
    pub title_name: String,
    /// Disk-image (ShadowMount) title. Recorded with the sighting because it
    /// is free here — the client already has it — and it says how the source
    /// game is stored without a second lookup.
    #[serde(default)]
    pub image_backed: bool,
    pub source: String,
}

#[derive(Debug, Deserialize)]
pub struct ScanRequest {
    pub addr: String,
    #[serde(default)]
    pub console: String,
    /// Stable identity of the console (its host). `console` is a display name
    /// the user can change; keying the sighting dedupe on it made one machine
    /// count as two.
    #[serde(default)]
    pub console_key: String,
    /// Titles to walk. Supplied by the client, which already has them from
    /// /api/ps5/apps/installed — enumerating them again here would duplicate a
    /// large handler for no gain.
    pub titles: Vec<ScanTitle>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ScanState {
    pub done: bool,
    pub titles_total: usize,
    pub titles_done: usize,
    /// Title currently being read, for the progress line.
    pub current: String,
    /// Sets added, as `(title name, library count)`.
    pub added: Vec<(String, usize)>,
    /// Titles whose libraries the corpus already had.
    pub skipped: usize,
    /// Titles that HAVE a fakelib/ but whose eboot was never downgraded, so
    /// what is in that folder is not a backport and must not enter the corpus.
    pub not_backported: usize,
    /// Titles with no fakelib/ at all — not backported, nothing to harvest.
    pub without_libraries: usize,
    pub errors: Vec<String>,
    pub error: Option<String>,
}

type Registry = Mutex<std::collections::HashMap<Uuid, Arc<Mutex<ScanState>>>>;

fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Start a scan and return its id. Scanning pulls every library off the console
/// over the payload's file API and takes on the order of a minute, so it
/// reports progress rather than blocking a request for that long.
pub async fn start_scan(Json(req): Json<ScanRequest>) -> impl IntoResponse {
    if corpus_root().is_none() {
        return no_home();
    }
    let id = Uuid::new_v4();
    let state = Arc::new(Mutex::new(ScanState {
        titles_total: req.titles.len(),
        ..Default::default()
    }));
    registry().lock().unwrap().insert(id, Arc::clone(&state));
    tokio::task::spawn_blocking(move || run_scan(req, state));
    (
        StatusCode::OK,
        Json(serde_json::json!({ "scan_id": id.to_string() })),
    )
        .into_response()
}

pub async fn scan_status(AxumPath(id): AxumPath<String>) -> impl IntoResponse {
    let Ok(id) = id.parse::<Uuid>() else {
        return err(StatusCode::BAD_REQUEST, "not a scan id");
    };
    let state = registry().lock().unwrap().get(&id).cloned();
    match state {
        Some(s) => {
            let snapshot = s.lock().unwrap().clone();
            if snapshot.done {
                registry().lock().unwrap().remove(&id);
            }
            (StatusCode::OK, Json(snapshot)).into_response()
        }
        None => err(
            StatusCode::NOT_FOUND,
            "no such scan (it may have already finished)",
        ),
    }
}

/// The address the payload's filesystem RPCs answer on.
///
/// Callers hand us whatever they hold: a bare host (the Backport panel passes
/// `addr={host}`) or a transfer address (`:9113`). `list_dir`/`fs_read` only
/// answer on the mgmt port, so both have to be normalised. This lives in one
/// named place on purpose — while it was inlined, the scan path simply forgot
/// it, every listing failed, and a console full of backported games reported
/// "No backported games found" with zero errors.
fn fs_addr(addr: &str) -> String {
    crate::mgmt_addr_for(addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_addr_normalises_to_the_mgmt_port() {
        // The scan regression: a bare host and a transfer address must BOTH
        // land on the mgmt port. When they did not, `harvest` silently
        // reported every title as "not backported".
        assert_eq!(fs_addr("192.168.1.50"), "192.168.1.50:9114");
        assert_eq!(fs_addr("192.168.1.50:9113"), "192.168.1.50:9114");
        assert_eq!(fs_addr("192.168.1.50:9114"), "192.168.1.50:9114");
    }
}

fn run_scan(req: ScanRequest, state: Arc<Mutex<ScanState>>) {
    let Some(root) = corpus_root() else { return };
    let mut corpus = Corpus::open(&root);
    // The payload's filesystem RPCs answer on the MGMT port. Callers hand us a
    // bare host (the Backport panel passes `addr={host}`) or a transfer
    // address (:9113); every other handler normalises through
    // mgmt_addr_or_default, and this one did not. The result was that
    // `list_dir` failed for EVERY title, harvest() swallowed the error as
    // "no fakelib here", and a console full of backported games scanned as
    // "No backported games found" with zero errors reported. Measured on a
    // 9.60 console: bare/:9113 → 40 of 40 "without libraries"; :9114 → 34
    // sets found.
    let addr = fs_addr(&req.addr);
    for title in &req.titles {
        {
            let mut s = state.lock().unwrap();
            s.current = if title.title_name.is_empty() {
                title.title_id.clone()
            } else {
                title.title_name.clone()
            };
        }
        // A `fakelib/` folder is not proof of a backport. PPSA25411 was a
        // raw FW-11 rip carrying two leftover libraries; the scan recorded
        // them as a set, and the ranking then offered that set FIRST for the
        // very title it came from — a combination that could never work.
        // Read the eboot pair and skip anything still declaring its original
        // SDK. Unreadable is NOT "not backported": that would resurrect the
        // bug where an unreachable console scanned as an empty one, so an
        // unreadable eboot falls through to the old behaviour.
        if title_backport_state(&addr, &title.source) == Some(false) {
            let mut s = state.lock().unwrap();
            s.not_backported += 1;
            s.titles_done += 1;
            continue;
        }
        match harvest(&addr, title) {
            Ok(libs) if libs.is_empty() => state.lock().unwrap().without_libraries += 1,
            Ok(libs) => {
                let count = libs.len();
                let label = if title.title_name.is_empty() {
                    title.title_id.clone()
                } else {
                    title.title_name.clone()
                };
                let origin = Origin::Scan {
                    title_id: title.title_id.clone(),
                    title_name: title.title_name.clone(),
                    console: req.console.clone(),
                    console_key: req.console_key.clone(),
                    image_backed: title.image_backed,
                    at: now_iso(),
                };
                match corpus.add_set(&label, origin, libs) {
                    Ok(Some(_)) => state.lock().unwrap().added.push((label, count)),
                    Ok(None) => state.lock().unwrap().skipped += 1,
                    Err(e) => state.lock().unwrap().errors.push(format!("{label}: {e:#}")),
                }
            }
            Err(e) => state
                .lock()
                .unwrap()
                .errors
                .push(format!("{}: {e:#}", title.title_id)),
        }
        let mut s = state.lock().unwrap();
        s.titles_done += 1;
    }
    let mut s = state.lock().unwrap();
    s.current.clear();
    s.done = true;
}

/// Has this title's eboot been downgraded?
///
/// `Some(true)` backported, `Some(false)` still at its shipped SDK, `None`
/// when the eboot could not be read or parsed. The caller must treat `None` as
/// "carry on" rather than "skip": reporting an unreadable console as
/// un-backported is exactly the failure that once made a fully backported
/// machine scan as empty.
fn title_backport_state(addr: &str, source: &str) -> Option<bool> {
    let eboot = format!("{}/eboot.bin", source.trim_end_matches('/'));
    // Same small prefix the SDK-pair probe reads: the ELF walk needs only the
    // header and entry table, not a 50 MB file.
    let header = fs_read(addr, &eboot, 0, 256 * 1024).ok()?;
    let site = ps5upload_core::fakelibs::param_site_from_header(&header)?;
    let chunk = fs_read(addr, &eboot, site as u64, 0x18).ok()?;
    let pair = ps5upload_core::fakelibs::sdk_pair_at(&chunk)?;
    Some(ps5upload_core::fakelibs::looks_backported(pair))
}

/// Read one title's `fakelib/`. A title without one is not an error — it simply
/// has not been backported, which is true of most games on a console.
fn harvest(addr: &str, title: &ScanTitle) -> anyhow::Result<Vec<IncomingLibrary>> {
    let dir = format!("{}/fakelib", title.source.trim_end_matches('/'));
    let listing = match list_dir(addr, &dir, ListDirOptions::default()) {
        Ok(l) => l,
        Err(e) => {
            // A title with no `fakelib/` is the normal case — it simply has not
            // been backported. But ANY other failure (console unreachable,
            // wrong port, permission) used to return the same empty vec, so a
            // scan that could not talk to the console at all reported every
            // title as "not backported" and surfaced zero errors. That is how
            // a fully-backported console scanned as "No backported games
            // found". Tell the two apart by probing the title's own directory:
            // if that lists, `fakelib/` is genuinely absent; if it does not,
            // the console is the problem and the error must be reported.
            let parent = title.source.trim_end_matches('/');
            return match list_dir(addr, parent, ListDirOptions::default()) {
                Ok(_) => Ok(Vec::new()),
                Err(_) => Err(e.context(format!("listing {dir}"))),
            };
        }
    };
    let mut out = Vec::new();
    for entry in listing.entries {
        if entry.kind != "file" || !is_library_name(&entry.name) {
            continue;
        }
        let data = fs_read(addr, &format!("{dir}/{}", entry.name), 0, entry.size)?;
        out.push(IncomingLibrary {
            name: entry.name,
            data,
        });
    }
    Ok(out)
}

// ─── Backport packs ──────────────────────────────────────────────────────────
//
// A "pack" is how backports are actually distributed: `fakelib/` plus an
// eboot.bin that is ALREADY patched to the backport SDK pair, plus replacement
// `sce_module/` modules. Both endpoints take a HOST PATH rather than an upload
// because a pack eboot runs to hundreds of megabytes — pushing that through
// multipart into the engine, only to send it straight back out to the console,
// would double the transfer for no gain.

#[derive(Debug, Deserialize)]
pub struct PackQuery {
    pub path: String,
}

/// GET /api/backport/pack?path=... — what is in this folder?
///
/// Read-only: it never touches the corpus. The UI needs to show the user what
/// was recognised BEFORE anything is installed, because installing a pack
/// replaces the title's eboot.
pub async fn inspect_pack(Query(q): Query<PackQuery>) -> impl IntoResponse {
    let dir = PathBuf::from(q.path);
    match tokio::task::spawn_blocking(move || ps5upload_core::backport_pack::inspect(&dir)).await {
        Ok(Ok(c)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "is_pack": c.is_pack(),
                "title_id_hint": c.title_id_hint,
                "libraries": c.libraries,
                "eboot": c.eboot,
                "sce_modules": c.sce_modules,
                "game_prx": c.game_prx,
                "sce_sys": c.sce_sys,
                "other": c.other,
                "total_bytes": c.total_bytes(),
            })),
        )
            .into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")),
    }
}

#[derive(Debug, Deserialize)]
pub struct ImportPackReq {
    pub path: String,
    #[serde(default)]
    pub label: String,
}

/// POST /api/backport/pack/import — take ONLY `fakelib/` into the corpus.
///
/// Deliberately partial. The eboot and `sce_module/` are title-specific and
/// enormous; content-addressing them would bloat the store with bytes no other
/// game can ever reuse. They are installed straight from the folder instead.
pub async fn import_pack(Json(req): Json<ImportPackReq>) -> impl IntoResponse {
    let Some(root) = corpus_root() else {
        return no_home();
    };
    let dir = PathBuf::from(&req.path);
    let label_in = req.label.trim().to_string();
    let read = tokio::task::spawn_blocking(
        move || -> anyhow::Result<(Vec<IncomingLibrary>, String, Option<String>)> {
            let c = ps5upload_core::backport_pack::inspect(&dir)?;
            if !c.is_pack() {
                // Naming what WAS found turns "nothing happened" into a
                // pointer at the mistake: the user usually picked the parent
                // folder, or a pack whose libraries sit one level deeper.
                return Err(anyhow::anyhow!(
                    "no fakelib/ folder here, so this is not a backport pack ({} other file(s) seen)",
                    c.other.len() + c.sce_modules.len()
                ));
            }
            let mut libraries = Vec::new();
            for f in &c.libraries {
                let name = f
                    .rel_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&f.rel_path)
                    .to_string();
                let data = std::fs::read(dir.join(&f.rel_path))
                    .map_err(|e| anyhow::anyhow!("reading {}: {e}", f.rel_path))?;
                libraries.push(IncomingLibrary { name, data });
            }
            let folder = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("pack")
                .to_string();
            Ok((libraries, folder, c.title_id_hint))
        },
    )
    .await;

    let (libraries, folder, hint) = match read {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return err(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")),
    };

    // The title id is the name worth carrying: a downloaded folder is usually
    // called something like "[SITE]-FW 4xx PPSA19534 (v01.000.016)", which is
    // noise in a set list.
    let label = if !label_in.is_empty() {
        label_in
    } else {
        hint.clone().unwrap_or_else(|| folder.clone())
    };
    let origin = Origin::Import {
        source: folder,
        at: now_iso(),
    };
    let mut corpus = Corpus::open(&root);
    match corpus.add_set(&label, origin, libraries) {
        Ok(Some(id)) => (
            StatusCode::OK,
            Json(serde_json::json!({ "ok": true, "set_id": id, "title_id_hint": hint })),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "ok": true, "set_id": null, "duplicate": true, "title_id_hint": hint
            })),
        )
            .into_response(),
        Err(e) => err(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")),
    }
}

// ─── GET /api/ps5/title-sdk-pair ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SdkPairQuery {
    pub addr: String,
    /// The title's source directory; `eboot.bin` is read from inside it.
    pub path: String,
}

/// The SDK pair actually written in a title's eboot, and whether it is the one
/// a backport targets.
///
/// This exists because an un-backported title is INDISTINGUISHABLE from a wrong
/// library set: the launch returns ok, the game dies before producing a process,
/// and there is no `Call to unpatched function` line. Diagnosing that as a
/// library problem cost a full day. Checking the pair takes two small reads.
///
/// Note `param.json`'s `sdkVersion` is a different field and does NOT change
/// when the eboot is patched, so it cannot answer this.
pub async fn title_sdk_pair(Query(q): Query<SdkPairQuery>) -> impl IntoResponse {
    let eboot = format!("{}/eboot.bin", q.path.trim_end_matches('/'));
    // Same contract as the scan: fs_read answers on the mgmt port, and callers
    // pass a bare host or a :9113 transfer address. Without this the SDK-pair
    // probe silently failed and every title looked "not backported".
    let addr = fs_addr(&q.addr);
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<(u32, u32)>> {
        // The ELF walk needs only the header and entry table, so read a small
        // prefix rather than pulling a 50 MB eboot across the wire for 8 bytes.
        let header = fs_read(&addr, &eboot, 0, 256 * 1024)?;
        let Some(site) = ps5upload_core::fakelibs::param_site_from_header(&header) else {
            return Ok(None);
        };
        let chunk = fs_read(&addr, &eboot, site as u64, 0x18)?;
        Ok(ps5upload_core::fakelibs::sdk_pair_at(&chunk))
    })
    .await;

    match result {
        Ok(Ok(Some(pair))) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "ps4": pair.0,
                "ps5": pair.1,
                "backported": pair == ps5upload_core::fakelibs::BACKPORT_SDK_PAIR,
            })),
        )
            .into_response(),
        // Unreadable is not "not backported": saying so would send the user to
        // re-patch a title that may be fine.
        Ok(Ok(None)) => (
            StatusCode::OK,
            Json(serde_json::json!({ "ps4": null, "ps5": null, "backported": null })),
        )
            .into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_GATEWAY, format!("{e:#}")),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")),
    }
}

fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Good enough for an origin record; avoids pulling in a date crate.
    format!("{secs}")
}
