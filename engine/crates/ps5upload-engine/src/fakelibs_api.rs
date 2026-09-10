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
    pub source: String,
}

#[derive(Debug, Deserialize)]
pub struct ScanRequest {
    pub addr: String,
    #[serde(default)]
    pub console: String,
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

fn run_scan(req: ScanRequest, state: Arc<Mutex<ScanState>>) {
    let Some(root) = corpus_root() else { return };
    let mut corpus = Corpus::open(&root);
    for title in &req.titles {
        {
            let mut s = state.lock().unwrap();
            s.current = if title.title_name.is_empty() {
                title.title_id.clone()
            } else {
                title.title_name.clone()
            };
        }
        match harvest(&req.addr, title) {
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
                    console: req.console.clone(),
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

/// Read one title's `fakelib/`. A title without one is not an error — it simply
/// has not been backported, which is true of most games on a console.
fn harvest(addr: &str, title: &ScanTitle) -> anyhow::Result<Vec<IncomingLibrary>> {
    let dir = format!("{}/fakelib", title.source.trim_end_matches('/'));
    let listing = match list_dir(addr, &dir, ListDirOptions::default()) {
        Ok(l) => l,
        Err(_) => return Ok(Vec::new()),
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
    let addr = q.addr.clone();
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
