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

/// `PS5UPLOAD_STATE_DIR`, else `<data dir>/state`. Tests persist only with an explicit directory.
fn path() -> Option<PathBuf> {
    let dir = match std::env::var("PS5UPLOAD_STATE_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ if cfg!(test) => return None,
        _ => crate::remote::store::data_dir()?.join("state"),
    };
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("web-shared.json"))
}

fn load_from(path: &Path) -> Docs {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
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

fn with<R>(f: impl FnOnce(&mut Docs) -> R) -> (R, std::io::Result<()>) {
    let mut guard = store().lock().unwrap_or_else(|e| e.into_inner());
    let file = path();
    let docs = guard.get_or_insert_with(|| file.as_deref().map(load_from).unwrap_or_default());
    let out = f(docs);
    let saved = match file {
        Some(file) => save_to(&file, docs),
        None => Ok(()),
    };
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
    Json(read(&name)).into_response()
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
        with(|docs| put(docs, &name, body.base_rev, body.value))
    })
    .await;
    match done {
        Ok((Ok(rev), Ok(()))) => Json(serde_json::json!({ "rev": rev })).into_response(),
        Ok((Ok(_), Err(e))) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Ok((Err(current), _)) => (StatusCode::CONFLICT, Json(*current)).into_response(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
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
