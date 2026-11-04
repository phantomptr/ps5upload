//! Settings that belong to the engine's install rather than to one browser: in the self-hosted
//! web UI (Docker) every browser that opens the page sees the same consoles and settings. Each document has a
//! revision; a write names the revision it was made from and is refused (409, with the current
//! document) when another browser saved in between, so the browser merges and tries again instead
//! of overwriting. Kept in `state/web-shared.json` under the data folder (the Docker volume).
//!
//! The desktop and mobile apps keep these settings themselves; their engine is theirs alone.

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

/// `GET /api/shared/{name}` — `{rev, value}`; `rev` 0 and `value` null when never saved.
pub async fn get_doc(UrlPath(name): UrlPath<String>) -> Response {
    if let Some(r) = unknown(&name) {
        return r;
    }
    // The first read of the file blocks: not on an async worker.
    let doc = tokio::task::spawn_blocking(move || {
        let mut guard = store().lock().unwrap_or_else(|e| e.into_inner());
        guard
            .get_or_insert_with(|| path().as_deref().map(load_from).unwrap_or_default())
            .get(&name)
            .cloned()
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    Json(doc).into_response()
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
    if !(body.value.is_object() || body.value.is_array()) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "value must be a JSON object or array",
        );
    }
    let size = serde_json::to_vec(&body.value)
        .map(|v| v.len())
        .unwrap_or(usize::MAX);
    if size > MAX_BYTES {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("{size} bytes is over {MAX_BYTES}"),
        );
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
