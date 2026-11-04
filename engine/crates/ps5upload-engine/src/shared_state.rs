//! Settings that belong to the engine's install rather than to one browser: in the self-hosted
//! web UI (Docker) every browser that opens the page sees the same consoles and settings. Each document has a
//! revision; a write names the revision it was made from and is refused (409, with the current
//! document) when another browser saved in between, so the browser merges and tries again instead
//! of overwriting. Kept in `state/web-shared.json` under the data folder (the Docker volume).
//!
//! The desktop and mobile apps keep their own copies and mirror them here, so `/api/consoles` and
//! `/api/settings` (consoles_api.rs, settings_api.rs: the typed views over these documents) answer
//! the same way on every platform.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use axum::extract::Path as UrlPath;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The documents the page may keep here. Anything else is refused, so the store can't become a
/// general dumping ground.
/// `consoles`: the console list (client state/rosterShare.ts). `prefs`: the settings every browser
/// shares, per key with the time it changed (client lib/sharedPrefs.ts).
pub const NAMES: &[&str] = &["consoles", "prefs"];

/// Largest document accepted (a roster of a few consoles is a few KiB).
pub const MAX_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Doc {
    /// 0 = never saved.
    pub rev: u64,
    pub value: Option<Value>,
}

pub type Docs = BTreeMap<String, Doc>;

/// A write made from `base_rev`: the new revision, or the current document when someone else
/// saved since.
pub fn put(docs: &mut Docs, name: &str, base_rev: u64, value: Value) -> Result<u64, Box<Doc>> {
    let doc = docs.entry(name.to_string()).or_default();
    if doc.rev != base_rev {
        return Err(Box::new(doc.clone()));
    }
    doc.rev += 1;
    doc.value = Some(value);
    Ok(doc.rev)
}

/// Where the documents are kept.
#[derive(Debug, PartialEq, Eq)]
enum Place {
    /// `<dir>/web-shared.json`
    File(PathBuf),
    /// Only in tests with no explicit directory.
    Memory,
    /// Nowhere that lasts, and why.
    Nowhere(String),
}

/// `PS5UPLOAD_STATE_DIR`, else `<data dir>/state`. Tests persist only with an explicit directory.
fn place_from(explicit: Option<String>, data_dir: Option<PathBuf>, testing: bool) -> Place {
    let dir = match explicit {
        Some(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ if testing => return Place::Memory,
        _ => match data_dir {
            Some(d) => d.join("state"),
            None => {
                return Place::Nowhere(
                    "no data directory to save in: set PS5UPLOAD_DATA_DIR on the engine".into(),
                )
            }
        },
    };
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Place::File(dir.join("web-shared.json")),
        Err(e) => Place::Nowhere(format!("cannot save in {}: {e}", dir.display())),
    }
}

fn place() -> Place {
    place_from(
        std::env::var("PS5UPLOAD_STATE_DIR").ok(),
        crate::remote::store::data_dir(),
        cfg!(test),
    )
}

fn path() -> Option<PathBuf> {
    match place() {
        Place::File(p) => Some(p),
        Place::Memory | Place::Nowhere(_) => None,
    }
}

/// The store, or an empty one when there is no file yet. A file that cannot be read or parsed is
/// reported and kept beside the store under a new name: the next save would overwrite it, and it
/// may be the only copy of someone's consoles and Wake keys. (The browsers treat an empty store as
/// one that lost its list and hand theirs back: client state/rosterShare.ts.)
fn load_from(path: &Path) -> Docs {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Docs::default(),
        Err(e) => {
            crate::log_warn!("shared settings: {} is unreadable: {e}", path.display());
            keep_bad_copy(path);
            return Docs::default();
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(docs) => docs,
        Err(e) => {
            crate::log_warn!("shared settings: {} is damaged: {e}", path.display());
            keep_bad_copy(path);
            Docs::default()
        }
    }
}

fn keep_bad_copy(path: &Path) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let copy = path.with_extension(format!("json.bad-{stamp}"));
    match std::fs::rename(path, &copy) {
        Ok(()) => crate::log_warn!("shared settings: kept the old file as {}", copy.display()),
        Err(e) => crate::log_warn!("shared settings: could not keep the old file: {e}"),
    }
}

fn save_to(path: &Path, docs: &Docs) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(docs).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)
        .and_then(|_| std::fs::rename(&tmp, path))
        .map_err(|e| crate::state_io::io_error("shared settings", path, e))
}

fn store() -> &'static Mutex<Option<Docs>> {
    static STORE: OnceLock<Mutex<Option<Docs>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(None))
}

/// Changes the store and saves it. Memory takes the change only once the file has it: a change a
/// full disk refused must not be served to the browsers as saved and then vanish at the restart.
fn with<R>(f: impl FnOnce(&mut Docs) -> R) -> (R, std::io::Result<()>) {
    let mut guard = store().lock().unwrap_or_else(|e| e.into_inner());
    let file = path();
    let docs = guard.get_or_insert_with(|| file.as_deref().map(load_from).unwrap_or_default());
    let mut next = docs.clone();
    let out = f(&mut next);
    let saved = match file {
        Some(file) => save_to(&file, &next),
        None => Ok(()),
    };
    if saved.is_ok() {
        *docs = next;
    }
    (out, saved)
}

/// The folder the documents are kept in; None when they are kept in memory only.
pub(crate) fn state_dir() -> Option<PathBuf> {
    path().and_then(|p| p.parent().map(Path::to_path_buf))
}

/// A document as it is now.
pub(crate) fn read(name: &str) -> Doc {
    let mut guard = store().lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(|| path().as_deref().map(load_from).unwrap_or_default())
        .get(name)
        .cloned()
        .unwrap_or_default()
}

/// Saves `value` over the revision `base_rev`: the new revision, or what went wrong.
pub(crate) fn write(name: &str, base_rev: u64, value: Value) -> Result<u64, String> {
    match with(|docs| put(docs, name, base_rev, value)) {
        (Ok(rev), Ok(())) => Ok(rev),
        (Ok(_), Err(e)) => Err(e.to_string()),
        (Err(cur), _) => Err(format!("{name} is at revision {}, not {base_rev}", cur.rev)),
    }
}

/// Rewrites a document without a new revision (the same content, stored differently).
pub(crate) fn set_quietly(name: &str, value: Value) -> std::io::Result<()> {
    with(|docs| {
        docs.entry(name.to_string()).or_default().value = Some(value);
    })
    .1
}

/// Changes a document in one step (a new revision): the document after it, or what went wrong.
pub(crate) fn update(name: &str, change: impl FnOnce(&mut Value)) -> Result<Doc, String> {
    let (doc, saved) = with(|docs| {
        let doc = docs.entry(name.to_string()).or_default();
        let mut value = doc.value.take().unwrap_or(Value::Null);
        change(&mut value);
        doc.value = Some(value);
        doc.rev += 1;
        doc.clone()
    });
    saved.map_err(|e| e.to_string())?;
    Ok(doc)
}

fn json_error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

fn unknown(name: &str) -> Option<Response> {
    (!NAMES.contains(&name)).then(|| {
        json_error(
            StatusCode::NOT_FOUND,
            format!("no shared setting named {name}"),
        )
    })
}

/// `GET /api/shared/{name}` — `{rev, value}`; `rev` 0 and `value` null when never saved. The console
/// list comes without its keys here (`GET /api/consoles/document` has them, for the app).
pub async fn get_doc(UrlPath(name): UrlPath<String>) -> Response {
    if let Some(r) = unknown(&name) {
        return r;
    }
    if name == crate::consoles_api::DOC {
        return Json(crate::consoles_api::shared_doc()).into_response();
    }
    // The first read of the file blocks: not on an async worker.
    let doc = tokio::task::spawn_blocking(move || read(&name))
        .await
        .unwrap_or_default();
    Json(doc).into_response()
}

/// `GET /api/shared/{name}/schema`: what the document holds, as JSON Schema.
pub async fn schema(UrlPath(name): UrlPath<String>) -> Response {
    if let Some(r) = unknown(&name) {
        return r;
    }
    Json(if name == crate::consoles_api::DOC {
        crate::consoles_api::document_schema()
    } else {
        crate::settings_api::document_schema()
    })
    .into_response()
}

/// Why a document's new value cannot be kept, as the answer to give.
pub(crate) fn refuse_value(value: &Value) -> Option<Response> {
    if !(value.is_object() || value.is_array()) {
        return Some(json_error(
            StatusCode::BAD_REQUEST,
            "value must be a JSON object or array",
        ));
    }
    let size = serde_json::to_vec(value)
        .map(|v| v.len())
        .unwrap_or(usize::MAX);
    (size > MAX_BYTES).then(|| {
        json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("{size} bytes is over {MAX_BYTES}"),
        )
    })
}

#[derive(Deserialize)]
pub struct PutBody {
    pub base_rev: u64,
    pub value: Value,
}

/// `PUT /api/shared/{name}` `{base_rev, value}` — 200 `{rev}`, or 409 `{rev, value}` (the current
/// document) when `base_rev` is not the current revision.
pub async fn put_doc(UrlPath(name): UrlPath<String>, Json(body): Json<PutBody>) -> Response {
    if let Some(r) = unknown(&name) {
        return r;
    }
    if name == crate::consoles_api::DOC {
        return crate::consoles_api::save_document(body, false).await;
    }
    if let Some(r) = refuse_value(&body.value) {
        return r;
    }
    let done = tokio::task::spawn_blocking(move || {
        // Refused rather than held in memory: a "saved" that a restart would lose is a lie.
        if let Place::Nowhere(why) = place() {
            return Err(why);
        }
        Ok(with(|docs| put(docs, &name, body.base_rev, body.value)))
    })
    .await;
    let done = match done {
        Ok(Ok(saved)) => Ok(saved),
        Ok(Err(why)) => {
            crate::log_warn!("shared settings not saved: {why}");
            return json_error(StatusCode::SERVICE_UNAVAILABLE, why);
        }
        Err(e) => Err(e),
    };
    match done {
        Ok((Ok(rev), Ok(()))) => Json(serde_json::json!({ "rev": rev })).into_response(),
        Ok((Ok(_), Err(e))) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Ok((Err(current), _)) => (StatusCode::CONFLICT, Json(*current)).into_response(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[cfg(test)]
mod place_tests {
    use super::*;

    #[test]
    fn no_data_directory_is_nowhere_and_says_so() {
        match place_from(None, None, false) {
            Place::Nowhere(why) => assert!(why.contains("PS5UPLOAD_DATA_DIR"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_directory_that_cannot_be_made_is_nowhere_too() {
        let file = std::env::temp_dir().join(format!("shared-state-{}", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        let got = place_from(Some(file.join("sub").display().to_string()), None, false);
        let _ = std::fs::remove_file(&file);
        assert!(matches!(got, Place::Nowhere(_)), "{got:?}");
    }

    #[test]
    fn tests_stay_in_memory_unless_given_a_directory() {
        assert_eq!(place_from(None, None, true), Place::Memory);
        let dir = std::env::temp_dir().join(format!("shared-state-dir-{}", std::process::id()));
        let got = place_from(Some(dir.display().to_string()), None, true);
        assert_eq!(got, Place::File(dir.join("web-shared.json")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_data_directory_is_used_when_nothing_is_set() {
        let dir = std::env::temp_dir().join(format!("shared-state-data-{}", std::process::id()));
        let got = place_from(None, Some(dir.clone()), false);
        assert_eq!(got, Place::File(dir.join("state").join("web-shared.json")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_write_from_an_old_revision_gets_the_current_document() {
        let mut docs = Docs::new();
        assert_eq!(put(&mut docs, "consoles", 0, json!([{"id": "a"}])), Ok(1));
        // A second browser that also started from nothing.
        let theirs = put(&mut docs, "consoles", 0, json!([{"id": "b"}])).unwrap_err();
        assert_eq!(
            *theirs,
            Doc {
                rev: 1,
                value: Some(json!([{"id": "a"}]))
            }
        );
        // After merging, it writes from revision 1.
        assert_eq!(
            put(&mut docs, "consoles", 1, json!([{"id": "a"}, {"id": "b"}])),
            Ok(2)
        );
        assert_eq!(docs["consoles"].rev, 2);
    }

    #[test]
    fn saved_and_loaded_through_the_file() {
        let dir = std::env::temp_dir().join(format!("ps5upload-shared-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("web-shared.json");
        let mut docs = Docs::new();
        put(&mut docs, "consoles", 0, json!({"profiles": []})).unwrap();
        save_to(&file, &docs).unwrap();
        assert_eq!(load_from(&file), docs);
        assert_eq!(load_from(&dir.join("missing.json")), Docs::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_file_loads_empty_and_is_kept_under_a_new_name() {
        let dir = std::env::temp_dir().join(format!("ps5upload-shared-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("web-shared.json");
        std::fs::write(&file, b"{ not json").unwrap();
        assert_eq!(load_from(&file), Docs::new());
        assert!(!file.exists());
        let kept: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            kept.iter().any(|n| n.starts_with("web-shared.json.bad-")),
            "{kept:?}"
        );
        assert_eq!(
            std::fs::read(dir.join(&kept[0])).unwrap(),
            b"{ not json".to_vec()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn only_known_names_and_json_containers_are_accepted() {
        let r = get_doc(UrlPath("theme".into())).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        let r = put_doc(
            UrlPath("consoles".into()),
            Json(PutBody {
                base_rev: 0,
                value: json!("text"),
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let r = put_doc(
            UrlPath("consoles".into()),
            Json(PutBody {
                base_rev: 0,
                value: json!(["x".repeat(MAX_BYTES)]),
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
