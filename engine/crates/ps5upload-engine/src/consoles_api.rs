//! The console list as the API sees it, on every platform, and the Remote Play keys that go with
//! each console.
//!
//! The list is the `consoles` document of shared_state.rs: the self-hosted web UI has kept it there
//! for a while, and the desktop and mobile apps now mirror their own list into it too
//! (client state/rosterShare.ts). So a script or agent sees the consoles the app shows, and a
//! change made here shows up in the app.
//!
//! * `GET /api/consoles`: `{rev, consoles: [{id, name, host, notes, has_rp_credentials,
//!   has_wake_credential}], paired_hosts_not_listed}`. Never a key.
//! * `PUT /api/consoles` `{base_rev?, consoles: [{id?, name?, host, notes?, regist_key?, rp_key?,
//!   wake_credential?}]}`: the whole list, in order. A console without `id` keeps the id of the
//!   entry with its address, or gets a new one. Keys are write-only: leaving them out keeps what is
//!   stored, an empty string clears them. With `base_rev`, a stale list answers 409 with the
//!   current one.
//! * `GET|PUT /api/consoles/document`: the same document in the app's own form, Wake keys included:
//!   the app and the web UI need them to start a stream from the page. Not for scripts.
//! * `/api/shared/consoles` keeps working for a page that was already open during an update, without
//!   the keys.
//!
//! The keys (`wake_credential`, `wake_regist_key`, `wake_rp_key`) are never written to the document
//! on disk. They live in `console-keys.json` beside it, each console's sealed with a per-install key
//! in `console-keys.key` (as remote/store.rs does for saved connections), by console address. A
//! document saved by an older version is moved over the first time it is read.
//!
//! `POST /api/ps5/power/pair` fills them too, so `POST /api/ps5/rp/start {host}` works after pairing
//! with nothing else to send. A console taken off the list loses its keys.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::remote::store::{load_or_create_key, open_sealed, seal};
use crate::shared_state::{self, Doc};

/// The document's name in shared_state.rs.
pub(crate) const DOC: &str = "consoles";

const WAKE_CREDENTIAL: &str = "wake_credential";
const REGIST_KEY: &str = "wake_regist_key";
const RP_KEY: &str = "wake_rp_key";
/// A profile's fields that are secrets: never on disk in the document, never in a view.
const KEY_FIELDS: [&str; 3] = [WAKE_CREDENTIAL, REGIST_KEY, RP_KEY];

/// One console's keys, by field name (`KEY_FIELDS`). No Debug on anything that holds them.
type Creds = BTreeMap<String, String>;

/// On disk: each console's keys as `base64(nonce || ciphertext)`.
#[derive(Serialize, Deserialize, Default)]
struct KeyFile {
    version: u32,
    consoles: BTreeMap<String, String>,
}

/// The console's address without a port, the way the keys are filed.
fn key_of(host: &str) -> String {
    crate::vinput_api::host_key(host.trim()).to_ascii_lowercase()
}

pub(crate) struct KeyStore {
    /// None: kept in memory only (tests, or a data folder that can't be written).
    dir: Option<PathBuf>,
    key: [u8; 32],
    map: BTreeMap<String, Creds>,
}

impl KeyStore {
    fn in_memory() -> Self {
        let mut key = [0u8; 32];
        let _ = getrandom::fill(&mut key);
        KeyStore {
            dir: None,
            key,
            map: BTreeMap::new(),
        }
    }

    pub(crate) fn open(dir: Option<&Path>) -> Self {
        let Some(dir) = dir else {
            return Self::in_memory();
        };
        let key = match load_or_create_key(&dir.join("console-keys.key")) {
            Ok(k) => k,
            Err(e) => {
                crate::log_warn!("consoles: Remote Play keys stay in memory only: {e:#}");
                return Self::in_memory();
            }
        };
        let mut map = BTreeMap::new();
        if let Ok(bytes) = std::fs::read(dir.join("console-keys.json")) {
            if let Ok(file) = serde_json::from_slice::<KeyFile>(&bytes) {
                for (host, blob) in file.consoles {
                    // Keys that no longer decrypt (the key file was replaced) are gone: the user
                    // pairs again.
                    if let Some(c) = open_sealed(&key, &blob)
                        .and_then(|p| serde_json::from_slice::<Creds>(&p).ok())
                    {
                        map.insert(host, c);
                    }
                }
            }
        }
        KeyStore {
            dir: Some(dir.to_path_buf()),
            key,
            map,
        }
    }

    fn save(&self) -> anyhow::Result<()> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        let mut file = KeyFile {
            version: 1,
            ..Default::default()
        };
        for (host, creds) in &self.map {
            file.consoles
                .insert(host.clone(), seal(&self.key, &serde_json::to_vec(creds)?)?);
        }
        let path = dir.join("console-keys.json");
        let tmp = dir.join("console-keys.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&file)?)
            .and_then(|_| std::fs::rename(&tmp, &path))
            .map_err(|e| anyhow::anyhow!(crate::state_io::describe("console keys", &path, &e)))
    }

    fn get(&self, host: &str) -> Option<&Creds> {
        self.map.get(&key_of(host))
    }

    fn set(&mut self, host: &str, creds: Creds) {
        let k = key_of(host);
        if creds.is_empty() {
            self.map.remove(&k);
        } else {
            self.map.insert(k, creds);
        }
    }
}

fn keys() -> &'static Mutex<KeyStore> {
    static KEYS: OnceLock<Mutex<KeyStore>> = OnceLock::new();
    KEYS.get_or_init(|| Mutex::new(KeyStore::open(shared_state::state_dir().as_deref())))
}

fn lock_keys() -> std::sync::MutexGuard<'static, KeyStore> {
    keys().lock().unwrap_or_else(|e| e.into_inner())
}

/// One write to the list at a time: its check, its keys and its document go together.
static ROSTER: Mutex<()> = Mutex::new(());

fn profiles_of(value: Option<&Value>) -> Vec<Value> {
    value
        .and_then(|v| v.get("profiles"))
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default()
}

fn str_of<'a>(p: &'a Value, field: &str) -> &'a str {
    p.get(field).and_then(|v| v.as_str()).unwrap_or("")
}

/// A list about to be saved: the keys in it move to the key store, and it comes out without them.
/// A key field left out keeps what is stored (a profile that changed address takes its keys along);
/// null or an empty string clears it. A console no longer on the list loses its keys. Returns
/// whether the key store changed.
fn take_keys(keys: &mut KeyStore, old: &[Value], new: &mut [Value]) -> bool {
    let before = keys.map.clone();
    // By console: an older list can hold one address twice, and each copy adds to the other.
    let mut next: BTreeMap<String, Creds> = BTreeMap::new();
    for p in new.iter_mut() {
        let Some(obj) = p.as_object_mut() else {
            continue;
        };
        let host = obj
            .get("host")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // What this console has now: under its address, or the address it had until this save.
        let moved_from = old
            .iter()
            .find(|o| !id.is_empty() && str_of(o, "id") == id)
            .map(|o| str_of(o, "host").to_string());
        let mut creds = next
            .get(&key_of(&host))
            .or_else(|| keys.get(&host))
            .cloned()
            .or_else(|| moved_from.and_then(|h| keys.get(&h).cloned()))
            .unwrap_or_default();
        for f in KEY_FIELDS {
            match obj.remove(f) {
                None => {}
                Some(Value::String(s)) if !s.trim().is_empty() => {
                    creds.insert(f.to_string(), s);
                }
                // null, "", or anything that is not a key.
                Some(_) => {
                    creds.remove(f);
                }
            }
        }
        if !key_of(&host).is_empty() {
            next.insert(key_of(&host), creds);
        }
    }
    for o in old {
        let h = str_of(o, "host");
        if !h.is_empty() {
            keys.map.remove(&key_of(h));
        }
    }
    for (host, creds) in next {
        keys.set(&host, creds);
    }
    keys.map != before
}

/// The list as the app reads it: its keys put back.
fn with_keys(keys: &KeyStore, doc: &mut Doc) {
    let Some(list) = doc
        .value
        .as_mut()
        .and_then(|v| v.get_mut("profiles"))
        .and_then(|p| p.as_array_mut())
    else {
        return;
    };
    for p in list {
        let host = str_of(p, "host").to_string();
        let (Some(obj), Some(creds)) = (p.as_object_mut(), keys.get(&host)) else {
            continue;
        };
        for (f, v) in creds {
            obj.insert(f.clone(), Value::String(v.clone()));
        }
    }
}

/// A document from before the keys moved: moved now, without a new revision (nothing about the
/// list changed for anyone reading it). Once per run.
fn migrate() {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.load(Ordering::SeqCst) {
        return;
    }
    // A second caller waits here for the first, then finds it done. A failure tries again next time.
    let _g = ROSTER.lock().unwrap_or_else(|e| e.into_inner());
    if DONE.load(Ordering::SeqCst) {
        return;
    }
    let doc = shared_state::read(DOC);
    let mut list = profiles_of(doc.value.as_ref());
    if !list.iter().any(has_key_field) {
        DONE.store(true, Ordering::SeqCst);
        return;
    }
    let mut keys = lock_keys();
    // Old and new the same list: nobody is off it, so nothing is dropped.
    let old = list.clone();
    take_keys(&mut keys, &old, &mut list);
    if let Err(e) = keys.save() {
        crate::log_warn!("consoles: could not move the saved keys: {e:#}");
        return;
    }
    drop(keys);
    let mut value = doc.value.unwrap_or_else(|| json!({}));
    value["profiles"] = Value::Array(list);
    if let Err(e) = shared_state::set_quietly(DOC, value) {
        crate::log_warn!("consoles: could not rewrite the console list: {e}");
    } else {
        DONE.store(true, Ordering::SeqCst);
        crate::log_info!("consoles: moved the saved Remote Play keys to the encrypted store");
    }
}

fn has_key_field(p: &Value) -> bool {
    KEY_FIELDS.iter().any(|f| p.get(*f).is_some())
}

fn json_error(status: StatusCode, code: &str, msg: impl Into<String>) -> Response {
    (status, Json(json!({"error": msg.into(), "code": code}))).into_response()
}

// ── The app's document ──────────────────────────────────────────────────────────────────────────

/// `/api/shared/consoles` read: no keys.
pub(crate) fn shared_doc() -> Doc {
    migrate();
    without_key_fields(shared_state::read(DOC))
}

/// The document with no key in it, whether or not the move to the encrypted store has worked.
fn without_key_fields(mut doc: Doc) -> Doc {
    let list = doc
        .value
        .as_mut()
        .and_then(|v| v.get_mut("profiles"))
        .and_then(Value::as_array_mut);
    for profile in list.into_iter().flatten() {
        if let Some(fields) = profile.as_object_mut() {
            for f in KEY_FIELDS {
                fields.remove(f);
            }
        }
    }
    doc
}

/// `GET /api/consoles/document`: the document with its keys, for the app.
pub async fn get_document() -> Response {
    let doc = tokio::task::spawn_blocking(|| {
        migrate();
        let mut doc = shared_state::read(DOC);
        with_keys(&lock_keys(), &mut doc);
        doc
    })
    .await
    .unwrap_or_default();
    Json(doc).into_response()
}

/// `PUT /api/consoles/document` `{base_rev, value}`: 200 `{rev}`, or 409 with the current document.
pub async fn put_document(Json(body): Json<shared_state::PutBody>) -> Response {
    save_document(body, true).await
}

/// A whole-document save, from the app (`reply_keys`: its conflict answer carries the keys) or a
/// page still on `/api/shared/consoles`.
pub(crate) async fn save_document(body: shared_state::PutBody, reply_keys: bool) -> Response {
    if let Some(r) = shared_state::refuse_value(&body.value) {
        return r;
    }
    let done = tokio::task::spawn_blocking(move || {
        migrate();
        let _g = ROSTER.lock().unwrap_or_else(|e| e.into_inner());
        let mut doc = shared_state::read(DOC);
        if doc.rev != body.base_rev {
            if reply_keys {
                with_keys(&lock_keys(), &mut doc);
            }
            return Err(Err(doc));
        }
        let old = profiles_of(doc.value.as_ref());
        let mut value = body.value;
        if let Some(list) = value.get_mut("profiles").and_then(|p| p.as_array_mut()) {
            let mut keys = lock_keys();
            if take_keys(&mut keys, &old, list) {
                keys.save().map_err(|e| Ok(format!("{e:#}")))?;
            }
        }
        shared_state::write(DOC, doc.rev, value).map_err(Ok)
    })
    .await;
    match done {
        Ok(Ok(rev)) => Json(json!({ "rev": rev })).into_response(),
        Ok(Err(Err(current))) => (StatusCode::CONFLICT, Json(current)).into_response(),
        Ok(Err(Ok(msg))) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "save_failed", msg),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "save_failed",
            e.to_string(),
        ),
    }
}

// ── The typed view ──────────────────────────────────────────────────────────────────────────────

/// The number a wake carries: as stored, or worked out from the regist key.
fn wake_of(c: &Creds) -> Option<String> {
    c.get(WAKE_CREDENTIAL).cloned().or_else(|| {
        let n = ps5upload_core::rp_regist::credential_from_regist_key(c.get(REGIST_KEY)?).ok()?;
        Some(n.to_string())
    })
}

fn console_view(p: &Value, keys: &KeyStore) -> Value {
    let creds = keys.get(str_of(p, "host"));
    let has = |f: &str| creds.is_some_and(|c| c.contains_key(f));
    json!({
        "id": str_of(p, "id"),
        "name": str_of(p, "name"),
        "host": str_of(p, "host"),
        "notes": p.get("notes").cloned().unwrap_or(Value::Null),
        "has_rp_credentials": has(REGIST_KEY) && has(RP_KEY),
        "has_wake_credential": creds.and_then(wake_of).is_some(),
    })
}

fn view(doc: &Doc, keys: &KeyStore) -> Value {
    let list = profiles_of(doc.value.as_ref());
    let listed: Vec<String> = list.iter().map(|p| key_of(str_of(p, "host"))).collect();
    let others: Vec<&String> = keys.map.keys().filter(|h| !listed.contains(h)).collect();
    json!({
        "rev": doc.rev,
        "consoles": list.iter().map(|p| console_view(p, keys)).collect::<Vec<_>>(),
        "paired_hosts_not_listed": others,
    })
}

/// `GET /api/consoles`.
pub async fn list() -> Response {
    let v = tokio::task::spawn_blocking(|| {
        migrate();
        view(&shared_state::read(DOC), &lock_keys())
    })
    .await
    .unwrap_or(Value::Null);
    Json(v).into_response()
}

/// One console in `PUT /api/consoles`. No Debug: it can carry keys.
#[derive(Deserialize)]
pub struct ConsoleIn {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, alias = "addr", alias = "console")]
    host: String,
    /// Left out keeps the notes; an empty string clears them.
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    regist_key: Option<String>,
    #[serde(default)]
    rp_key: Option<String>,
    #[serde(default)]
    wake_credential: Option<String>,
}

#[derive(Deserialize)]
pub struct PutConsoles {
    #[serde(default)]
    base_rev: Option<u64>,
    consoles: Vec<ConsoleIn>,
}

/// A new console's id: `crypto.randomUUID()`'s shape, which the app uses.
fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// What `PUT /api/consoles` asks for, as profiles to save, or what is wrong with it.
fn build_profiles(old: &[Value], input: Vec<ConsoleIn>) -> Result<Vec<Value>, String> {
    let mut out: Vec<Value> = Vec::new();
    let mut seen_hosts: Vec<String> = Vec::new();
    let mut seen_ids: Vec<String> = Vec::new();
    for c in input {
        let host = c.host.trim().to_string();
        if key_of(&host).is_empty() {
            return Err("every console needs a host".into());
        }
        let hk = key_of(&host);
        if seen_hosts.contains(&hk) {
            return Err(format!("{host} is on the list twice"));
        }
        seen_hosts.push(hk.clone());
        let id = c.id.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let earlier = old.iter().find(|o| match id {
            Some(id) => str_of(o, "id") == id,
            None => key_of(str_of(o, "host")) == hk,
        });
        let id = id
            .map(str::to_string)
            .or_else(|| earlier.map(|o| str_of(o, "id").to_string()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(new_id);
        if seen_ids.contains(&id) {
            return Err(format!("the id {id} is on the list twice"));
        }
        seen_ids.push(id.clone());
        // What the app keeps beyond these fields stays as it is.
        let mut p: Map<String, Value> = earlier
            .and_then(|o| o.as_object().cloned())
            .unwrap_or_default();
        let name = c
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                p.get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| host.clone());
        p.insert("id".into(), id.into());
        p.insert("name".into(), name.into());
        p.insert("host".into(), host.into());
        match c.notes.as_deref().map(str::trim) {
            Some("") => {
                p.remove("notes");
            }
            Some(n) => {
                p.insert("notes".into(), n.into());
            }
            None => {}
        }
        match (c.regist_key.as_deref(), c.rp_key.as_deref()) {
            (None, None) => {}
            (Some(r), Some(k)) if r.trim().is_empty() && k.trim().is_empty() => {
                p.insert(REGIST_KEY.into(), Value::Null);
                p.insert(RP_KEY.into(), Value::Null);
            }
            (Some(r), Some(k)) => {
                ps5upload_core::rp_session::SessionCreds::from_hex(r, k)
                    .map_err(|e| format!("{e:#}"))?;
                p.insert(REGIST_KEY.into(), r.trim().to_ascii_lowercase().into());
                p.insert(RP_KEY.into(), k.trim().to_ascii_lowercase().into());
            }
            _ => return Err("regist_key and rp_key go together".into()),
        }
        if let Some(w) = c.wake_credential.as_deref() {
            p.insert(WAKE_CREDENTIAL.into(), w.trim().into());
        }
        out.push(Value::Object(p));
    }
    Ok(out)
}

/// `PUT /api/consoles`.
pub async fn put(Json(req): Json<PutConsoles>) -> Response {
    let done = tokio::task::spawn_blocking(move || save_list(req)).await;
    match done {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(r)) => *r,
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "save_failed",
            e.to_string(),
        ),
    }
}

fn save_list(req: PutConsoles) -> Result<Value, Box<Response>> {
    let failed = |m: String| {
        Box::new(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "save_failed",
            m,
        ))
    };
    migrate();
    let _g = ROSTER.lock().unwrap_or_else(|e| e.into_inner());
    let doc = shared_state::read(DOC);
    if req.base_rev.is_some_and(|r| r != doc.rev) {
        let mut v = view(&doc, &lock_keys());
        v["error"] = "the console list changed since you read it".into();
        v["code"] = "stale_rev".into();
        return Err(Box::new((StatusCode::CONFLICT, Json(v)).into_response()));
    }
    let old = profiles_of(doc.value.as_ref());
    let mut list = build_profiles(&old, req.consoles)
        .map_err(|m| Box::new(json_error(StatusCode::BAD_REQUEST, "bad_console", m)))?;
    let mut value = doc
        .value
        .clone()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    // Before anything changes: a list too large to keep must not leave its keys behind.
    value["profiles"] = Value::Array(list.clone());
    if let Some(r) = shared_state::refuse_value(&value) {
        return Err(Box::new(r));
    }
    let mut keys = lock_keys();
    if take_keys(&mut keys, &old, &mut list) {
        keys.save().map_err(|e| failed(format!("{e:#}")))?;
    }
    value["profiles"] = Value::Array(list);
    shared_state::write(DOC, doc.rev, value).map_err(failed)?;
    Ok(view(&shared_state::read(DOC), &keys))
}

// ── For the rest of the engine ──────────────────────────────────────────────────────────────────

/// What a route was given to name a console: a console's id or name on the list means its address;
/// anything else (an address, usually) is passed on as it is.
pub(crate) fn resolve(reference: &str) -> String {
    let r = reference.trim();
    if r.is_empty() || r.contains(':') || r.parse::<std::net::IpAddr>().is_ok() {
        return reference.to_string();
    }
    let doc = shared_state::read(DOC);
    let list = profiles_of(doc.value.as_ref());
    let found = list
        .iter()
        .find(|p| str_of(p, "id") == r)
        .or_else(|| {
            list.iter()
                .find(|p| str_of(p, "name").eq_ignore_ascii_case(r))
        })
        .map(|p| str_of(p, "host"))
        .filter(|h| !h.trim().is_empty());
    match found {
        Some(h) => h.to_string(),
        None => reference.to_string(),
    }
}

/// The stored Remote Play keys of a console, as hex: (regist key, rp key).
pub(crate) fn rp_keys(host: &str) -> Option<(String, String)> {
    migrate();
    let keys = lock_keys();
    let c = keys.get(host)?;
    Some((c.get(REGIST_KEY)?.clone(), c.get(RP_KEY)?.clone()))
}

/// The stored wake credential of a console: its own, or worked out from its regist key.
pub(crate) fn wake_credential(host: &str) -> Option<String> {
    migrate();
    wake_of(lock_keys().get(host)?)
}

/// A pairing succeeded: its keys are this console's now. A console on the list gets a new revision,
/// so the app picks them up on its next look.
pub(crate) fn remember_pairing(host: &str, p: &ps5upload_core::rp_regist::PairResult) {
    migrate();
    let _g = ROSTER.lock().unwrap_or_else(|e| e.into_inner());
    let mut keys = lock_keys();
    let mut creds = keys.get(host).cloned().unwrap_or_default();
    for (f, v) in [
        (WAKE_CREDENTIAL, &p.credential),
        (REGIST_KEY, &p.regist_key),
        (RP_KEY, &p.rp_key),
    ] {
        if !v.trim().is_empty() {
            creds.insert(f.to_string(), v.clone());
        }
    }
    keys.set(host, creds);
    if let Err(e) = keys.save() {
        crate::log_warn!("consoles: could not save the pairing's keys: {e:#}");
    }
    drop(keys);
    let doc = shared_state::read(DOC);
    let listed = profiles_of(doc.value.as_ref())
        .iter()
        .any(|o| key_of(str_of(o, "host")) == key_of(host));
    if let (true, Some(value)) = (listed, doc.value) {
        if let Err(e) = shared_state::write(DOC, doc.rev, value) {
            crate::log_warn!("consoles: could not note the pairing: {e}");
        }
    }
}

/// JSON Schema of the `consoles` document (`GET /api/shared/consoles/schema`).
pub(crate) fn document_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "PS5Upload console list",
        "type": "object",
        "properties": {
            "profiles": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["id", "name", "host"],
                    "properties": {
                        "id": {"type": "string"},
                        "name": {"type": "string"},
                        "host": {"type": "string", "description": "The console's IP address or name."},
                        "notes": {"type": ["string", "null"]},
                    },
                    "additionalProperties": true,
                },
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As the console reports one: the text "1a2b3c4d", NUL-padded to 16 bytes.
    const R1: &str = "31613262336334640000000000000000";
    const K1: &str = "ffeeddccbbaa99887766554433221100";

    fn p(id: &str, host: &str) -> Value {
        json!({"id": id, "name": id, "host": host})
    }

    #[test]
    fn the_keyless_route_strips_keys_even_when_the_move_failed() {
        let doc = Doc {
            rev: 3,
            value: Some(json!({"profiles": [{
                "id": "a", "host": "10.0.0.2",
                "wake_regist_key": R1, "wake_rp_key": K1, "wake_credential": "12345",
            }]})),
        };
        let out = without_key_fields(doc);
        assert_eq!(out.rev, 3);
        let text = out.value.unwrap().to_string();
        assert!(text.contains("10.0.0.2"));
        for secret in [R1, K1, "12345"] {
            assert!(!text.contains(secret), "{text}");
        }
    }

    #[test]
    fn keys_leave_the_list_and_come_back_for_the_app() {
        let mut keys = KeyStore::open(None);
        let mut new = vec![json!({
            "id": "a", "name": "A", "host": "10.0.0.2:9021",
            "wake_regist_key": R1, "wake_rp_key": K1, "wake_credential": "12345",
        })];
        assert!(take_keys(&mut keys, &[], &mut new));
        assert!(!has_key_field(&new[0]), "nothing secret stays in the list");
        assert_eq!(keys.get("10.0.0.2").unwrap()[RP_KEY], K1);

        let doc = Doc {
            rev: 1,
            value: Some(json!({"profiles": new})),
        };
        let mut full = doc.clone();
        with_keys(&keys, &mut full);
        let back = &full.value.as_ref().unwrap()["profiles"][0];
        assert_eq!(back[REGIST_KEY], R1);
        assert_eq!(back[WAKE_CREDENTIAL], "12345");
        // The view says only that they are there.
        let v = view(&doc, &keys);
        let s = v.to_string();
        assert!(!s.contains(R1) && !s.contains(K1) && !s.contains("12345"));
        assert_eq!(v["consoles"][0]["has_rp_credentials"], true);
        assert_eq!(v["consoles"][0]["has_wake_credential"], true);
    }

    #[test]
    fn left_out_keeps_null_clears_and_off_the_list_forgets() {
        let mut keys = KeyStore::open(None);
        let mut first =
            vec![json!({"id": "a", "host": "10.0.0.2", "wake_rp_key": K1, "wake_regist_key": R1})];
        take_keys(&mut keys, &[], &mut first);
        // An older page saves the list without the fields: nothing is lost.
        let mut same = vec![p("a", "10.0.0.2")];
        assert!(!take_keys(&mut keys, &first, &mut same));
        assert!(keys.get("10.0.0.2").is_some());
        // The user cleared them.
        let mut cleared = vec![
            json!({"id": "a", "host": "10.0.0.2", "wake_rp_key": null, "wake_regist_key": ""}),
        ];
        assert!(take_keys(&mut keys, &same, &mut cleared));
        assert!(keys.get("10.0.0.2").is_none());
        // A console taken off the list loses its keys; one that was never on it keeps them.
        let mut again = vec![json!({"id": "a", "host": "10.0.0.2", "wake_rp_key": K1})];
        take_keys(&mut keys, &cleared, &mut again);
        keys.set(
            "10.0.0.9",
            Creds::from([(RP_KEY.to_string(), K1.to_string())]),
        );
        take_keys(&mut keys, &again, &mut []);
        assert!(keys.get("10.0.0.2").is_none());
        assert!(keys.get("10.0.0.9").is_some());
    }

    #[test]
    fn one_address_twice_on_a_list_keeps_the_keys_of_either() {
        let mut keys = KeyStore::open(None);
        let mut list = vec![
            json!({"id": "a", "host": "10.0.0.2", "wake_rp_key": K1, "wake_regist_key": R1}),
            json!({"id": "b", "host": "10.0.0.2:9021"}),
        ];
        take_keys(&mut keys, &[], &mut list);
        assert_eq!(keys.get("10.0.0.2").unwrap()[RP_KEY], K1);
    }

    #[test]
    fn a_console_that_changes_address_takes_its_keys_along() {
        let mut keys = KeyStore::open(None);
        let mut first =
            vec![json!({"id": "a", "host": "10.0.0.2", "wake_rp_key": K1, "wake_regist_key": R1})];
        take_keys(&mut keys, &[], &mut first);
        let mut moved = vec![p("a", "10.0.0.3")];
        take_keys(&mut keys, &first, &mut moved);
        assert!(keys.get("10.0.0.2").is_none());
        assert_eq!(keys.get("10.0.0.3").unwrap()[REGIST_KEY], R1);
    }

    #[test]
    fn keys_are_sealed_on_disk_and_read_back() {
        let dir = crate::remote::store::test_dir();
        let mut keys = KeyStore::open(Some(&dir));
        let mut list = vec![json!({"id": "a", "host": "10.0.0.2", "wake_rp_key": K1})];
        take_keys(&mut keys, &[], &mut list);
        keys.save().unwrap();
        let raw = std::fs::read_to_string(dir.join("console-keys.json")).unwrap();
        assert!(
            !raw.contains(K1),
            "the key must not be readable in the file"
        );
        let again = KeyStore::open(Some(&dir));
        assert_eq!(again.get("10.0.0.2").unwrap()[RP_KEY], K1);
        // A replaced key file: the keys are gone, the store still opens.
        std::fs::remove_file(dir.join("console-keys.key")).unwrap();
        assert!(KeyStore::open(Some(&dir)).get("10.0.0.2").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn input(host: &str) -> ConsoleIn {
        ConsoleIn {
            id: None,
            name: None,
            host: host.into(),
            notes: None,
            regist_key: None,
            rp_key: None,
            wake_credential: None,
        }
    }

    #[test]
    fn a_list_keeps_ids_names_and_what_the_app_stores() {
        let old = vec![
            json!({"id": "a", "name": "Living room", "host": "10.0.0.2", "notes": "n", "future": 1}),
        ];
        let out = build_profiles(&old, vec![input("10.0.0.2"), input("10.0.0.3")]).unwrap();
        assert_eq!(out[0]["id"], "a");
        assert_eq!(out[0]["name"], "Living room");
        assert_eq!(out[0]["notes"], "n");
        assert_eq!(out[0]["future"], 1);
        assert_eq!(out[1]["name"], "10.0.0.3");
        assert!(!str_of(&out[1], "id").is_empty());
        assert_ne!(out[1]["id"], "a");
    }

    #[test]
    fn a_bad_list_is_refused() {
        assert!(build_profiles(&[], vec![input(" ")]).is_err());
        assert!(build_profiles(&[], vec![input("10.0.0.2"), input("10.0.0.2:9021")]).is_err());
        let mut half = input("10.0.0.2");
        half.regist_key = Some(R1.into());
        assert!(build_profiles(&[], vec![half]).is_err());
        let mut bad = input("10.0.0.2");
        bad.regist_key = Some("zz".into());
        bad.rp_key = Some(K1.into());
        assert!(build_profiles(&[], vec![bad]).is_err());
    }

    async fn body(r: Response) -> (StatusCode, Value) {
        let st = r.status();
        let b = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        (st, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    /// The whole way through the handlers. The only test that saves a list (the store is global).
    #[tokio::test]
    async fn the_list_through_its_routes_never_shows_a_key() {
        let req: PutConsoles = serde_json::from_value(json!({
            "consoles": [
                {"host": "10.1.0.2", "name": "Living room", "regist_key": R1, "rp_key": K1},
                {"addr": "10.1.0.3"},
            ],
        }))
        .unwrap();
        let (st, v) = body(put(Json(req)).await).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        let rev = v["rev"].as_u64().unwrap();
        assert_eq!(v["consoles"][0]["name"], "Living room");
        assert_eq!(v["consoles"][0]["has_rp_credentials"], true);
        assert_eq!(v["consoles"][1]["has_rp_credentials"], false);
        assert_eq!(v["consoles"][1]["name"], "10.1.0.3");
        let id = v["consoles"][0]["id"].as_str().unwrap().to_string();

        // Reads: only the app's own document carries the keys, and nothing else ever does.
        let (_, listed) = body(list().await).await;
        assert!(!listed.to_string().contains(K1), "{listed}");
        let (_, shared) =
            body(shared_state::get_doc(axum::extract::Path("consoles".into())).await).await;
        assert!(!shared.to_string().contains(K1), "{shared}");
        assert_eq!(shared["value"]["profiles"][0]["name"], "Living room");
        let (_, full) = body(get_document().await).await;
        assert_eq!(full["value"]["profiles"][0][RP_KEY], K1);
        assert_eq!(full["rev"], rev);

        // Other routes can name it, and a start finds its keys.
        assert_eq!(resolve(&id), "10.1.0.2");
        assert_eq!(resolve("LIVING ROOM"), "10.1.0.2");
        assert_eq!(rp_keys("10.1.0.2:9021"), Some((R1.into(), K1.into())));
        // Worked out from the regist key.
        assert_eq!(
            wake_credential("10.1.0.2"),
            Some(0x1a2b3c4d_u64.to_string())
        );

        // A stale list changes nothing and gets the current one, still without keys.
        let stale: PutConsoles =
            serde_json::from_value(json!({"base_rev": rev - 1, "consoles": []})).unwrap();
        let (st, v) = body(put(Json(stale)).await).await;
        assert_eq!(st, StatusCode::CONFLICT);
        assert_eq!(v["code"], "stale_rev");
        assert_eq!(v["consoles"].as_array().unwrap().len(), 2);
        assert!(!v.to_string().contains(K1));

        // The app saves what it read: the keys stay out of the document at rest.
        let saved = shared_state::PutBody {
            base_rev: rev,
            value: full["value"].clone(),
        };
        let (st, v) = body(put_document(Json(saved)).await).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert!(!has_key_field(
            &shared_state::read(DOC).value.unwrap()["profiles"][0]
        ));
        assert_eq!(rp_keys("10.1.0.2"), Some((R1.into(), K1.into())));

        // A conflict tells the app its current list with the keys; an older page's without.
        let old = shared_state::PutBody {
            base_rev: 0,
            value: json!({"profiles": []}),
        };
        let (st, v) = body(save_document(old, true).await).await;
        assert_eq!(st, StatusCode::CONFLICT);
        assert_eq!(v["value"]["profiles"][0][RP_KEY], K1);
        let old = shared_state::PutBody {
            base_rev: 0,
            value: json!({"profiles": []}),
        };
        let (_, v) = body(save_document(old, false).await).await;
        assert!(!v.to_string().contains(K1));

        // Pairing a console on the list gets it a new revision, so the app looks.
        let before = shared_state::read(DOC).rev;
        let paired = ps5upload_core::rp_regist::PairResult {
            credential: "42".into(),
            regist_key: R1.into(),
            rp_key: K1.into(),
            ..Default::default()
        };
        remember_pairing("10.1.0.3", &paired);
        assert_eq!(shared_state::read(DOC).rev, before + 1);
        assert_eq!(wake_credential("10.1.0.3").as_deref(), Some("42"));
        // Pairing one that is not: the keys are kept, and the list says so.
        remember_pairing("10.1.0.9", &paired);
        assert_eq!(shared_state::read(DOC).rev, before + 1);
        let (_, v) = body(list().await).await;
        assert_eq!(v["paired_hosts_not_listed"], json!(["10.1.0.9"]));
    }

    #[test]
    fn an_older_document_is_cleaned_of_its_keys_without_losing_them() {
        let mut keys = KeyStore::open(None);
        let old =
            vec![json!({"id": "a", "host": "10.0.0.2", "wake_rp_key": K1, "wake_regist_key": R1})];
        let mut list = old.clone();
        assert!(list.iter().any(has_key_field));
        take_keys(&mut keys, &old, &mut list);
        assert!(!list.iter().any(has_key_field));
        assert_eq!(keys.get("10.0.0.2").unwrap()[REGIST_KEY], R1);
    }

    #[test]
    fn an_address_is_never_taken_for_a_name() {
        assert_eq!(resolve("10.0.0.2"), "10.0.0.2");
        assert_eq!(resolve("10.0.0.2:9021"), "10.0.0.2:9021");
        assert_eq!(resolve("[::1]"), "[::1]");
        assert_eq!(resolve(""), "");
        assert_eq!(resolve("no such console"), "no such console");
    }
}
