//! The console list as the API sees it, on every platform, and the Remote Play keys that go with
//! each console.
//!
//! The list is the `consoles` document of shared_state.rs: the self-hosted web UI has kept it there
//! for a while, and the desktop and mobile apps now mirror their own list into it too
//! (client state/rosterShare.ts). So a script or agent sees the consoles the app shows, and a
//! change made here shows up in the app.
//!
//! * `GET /api/consoles`: `{rev, consoles: [{id, name, host, notes, has_rp_credentials,
//!   has_wake_credential, rp_users: [{name, chosen}]}], paired_hosts_not_listed}`. Never a key,
//!   and of the paired PS5 users only their names.
//! * `PUT /api/consoles` `{base_rev?, consoles: [{id?, name?, host, notes?, regist_key?, rp_key?,
//!   wake_credential?}]}`: the whole list, in order. A console without `id` keeps the id of the
//!   entry with its address, or gets a new one. Keys are write-only: leaving them out keeps what is
//!   stored, an empty string clears them. With `base_rev`, a stale list answers 409 with the
//!   current one.
//! * `PATCH /api/consoles/settings` `{console, settings: {name: value | null}}`: one console's own
//!   settings (Settings › This PS5): wake_for_uploads, keep_awake_mode (off | transfers | always),
//!   rest_after_upload, auto_bring_up, auto_redeploy_on_wake, install_path_fallback. null goes
//!   back to the app's default. `GET /api/consoles` lists each console's `settings`.
//! * `GET|PUT /api/consoles/document`: the same document in the app's own form, Wake keys included:
//!   the app and the web UI need them to start a stream from the page. Not for scripts.
//! * `/api/shared/consoles` keeps working for a page that was already open during an update, without
//!   the keys.
//!
//! The keys (`wake_credential`, `wake_regist_key`, `wake_rp_key`, `rp_users`) are never written to
//! the document on disk. They live in `console-keys.json` beside it, each console's sealed with a per-install key
//! in `console-keys.key` (as remote/store.rs does for saved connections), by console address. A
//! document saved by an older version is moved over the first time it is read.
//!
//! `POST /api/ps5/power/pair` fills them too, so `POST /api/ps5/rp/start {host}` works after pairing
//! with nothing else to send. A console taken off the list loses its keys.
//!
//! **One pairing per PS5 user.** A registration is made for one PSN account, and a session signs in
//! the user it was made for. So a console keeps a list, `rp_users`: `[{account_id, name,
//! wake_credential, wake_regist_key, wake_rp_key}]`, one per user, by account id. Pairing another
//! user adds to it; pairing a user again replaces that user's. The profile's own `wake_*` fields
//! stay what they always were, the keys everything uses (Start, Wake, Wake & sign in), and they are
//! always one of the list's: choosing a user copies that user's keys there. A console paired before
//! the list existed has only those fields: they read as one user, with no account id or name
//! (`users_of`), and become the list's first entry the next time a user is paired. In the store the
//! list is kept as JSON text beside the other fields, so an older engine carries it along unread.

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
/// The paired PS5 users ([`RpUser`]): an array in the app's document, JSON text in the store.
const RP_USERS: &str = "rp_users";
/// A profile's fields that are secrets: never on disk in the document, never in a view.
const KEY_FIELDS: [&str; 4] = [WAKE_CREDENTIAL, REGIST_KEY, RP_KEY, RP_USERS];

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
                Some(list) if f == RP_USERS => set_users(&mut creds, &parse_users(&list)),
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
            let v = if f == RP_USERS {
                serde_json::from_str(v).unwrap_or(Value::Null)
            } else {
                Value::String(v.clone())
            };
            obj.insert(f.clone(), v);
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
        // This console's own settings (Settings › This PS5); a missing one uses the app's default.
        "settings": p.get("settings").cloned().filter(Value::is_object).unwrap_or_else(|| json!({})),
        "has_rp_credentials": has(REGIST_KEY) && has(RP_KEY),
        "has_wake_credential": creds.and_then(wake_of).is_some(),
        "rp_users": creds.map(users_view).unwrap_or_else(|| json!([])),
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

/// `GET /api/consoles`. Each console says whether it has keys (`has_rp_credentials`,
/// `has_wake_credential`) and lists the PS5 users it is paired as, by name only: `rp_users:
/// [{name, chosen}]`, `chosen` being the one Remote Play starts as when `POST /api/ps5/rp/start`
/// names no `user`. A user paired without a known name is "user N" (its place, from 1).
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
                // Cleared keys unpair the console: every user's, not only the chosen one's.
                p.insert(RP_USERS.into(), Value::Null);
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

/// The settings one console keeps for itself (the app's `ConsoleSettings`, Settings › This PS5),
/// and what each may be. A setting left out of a console uses the app's default.
const CONSOLE_SETTINGS: &[(&str, &[&str])] = &[
    ("wake_for_uploads", &[]),
    ("keep_awake_mode", &["off", "transfers", "always"]),
    ("rest_after_upload", &[]),
    ("auto_bring_up", &[]),
    ("auto_redeploy_on_wake", &[]),
    ("install_path_fallback", &[]),
];

/// `PATCH /api/consoles/settings` `{console, settings: {name: value | null}}`.
#[derive(Deserialize)]
pub struct SettingsPatch {
    #[serde(alias = "addr", alias = "host")]
    console: String,
    settings: Map<String, Value>,
}

/// Checks `changes` against [`CONSOLE_SETTINGS`] (all or none) and applies them to `current`:
/// a value sets it, null goes back to the app's default.
fn apply_console_settings(
    current: &mut Map<String, Value>,
    changes: &Map<String, Value>,
) -> Result<(), String> {
    for (k, v) in changes {
        let Some((_, allowed)) = CONSOLE_SETTINGS.iter().find(|(name, _)| name == k) else {
            let names: Vec<&str> = CONSOLE_SETTINGS.iter().map(|(n, _)| *n).collect();
            return Err(format!(
                "{k} is not a console setting (they are: {})",
                names.join(", ")
            ));
        };
        let ok = match v {
            Value::Null => true,
            Value::Bool(_) => allowed.is_empty(),
            Value::String(t) => allowed.contains(&t.as_str()),
            _ => false,
        };
        if !ok {
            return Err(if allowed.is_empty() {
                format!("{k} is true, false or null")
            } else {
                format!("{k} is one of {} or null", allowed.join(", "))
            });
        }
    }
    for (k, v) in changes {
        if v.is_null() {
            current.remove(k);
        } else {
            current.insert(k.clone(), v.clone());
        }
    }
    Ok(())
}

/// `PATCH /api/consoles/settings`: one console's own settings, by its id, name or address. The
/// app picks the change up through the console list, as it does a renamed console.
pub async fn patch_settings(Json(req): Json<SettingsPatch>) -> Response {
    let done = tokio::task::spawn_blocking(move || -> Result<Value, Box<Response>> {
        migrate();
        let _g = ROSTER.lock().unwrap_or_else(|e| e.into_inner());
        let doc = shared_state::read(DOC);
        let mut list = profiles_of(doc.value.as_ref());
        let want = req.console.trim();
        let at = list.iter().position(|p| {
            str_of(p, "id") == want
                || str_of(p, "name").eq_ignore_ascii_case(want)
                || (!key_of(want).is_empty() && key_of(str_of(p, "host")) == key_of(want))
        });
        let Some(at) = at else {
            return Err(Box::new(json_error(
                StatusCode::NOT_FOUND,
                "no_console",
                format!("no console {want} on the list"),
            )));
        };
        let mut current = list[at]
            .get("settings")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        apply_console_settings(&mut current, &req.settings)
            .map_err(|m| Box::new(json_error(StatusCode::BAD_REQUEST, "bad_setting", m)))?;
        if let Some(p) = list[at].as_object_mut() {
            p.insert("settings".into(), Value::Object(current));
        }
        let mut value = doc
            .value
            .clone()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        value["profiles"] = Value::Array(list);
        shared_state::write(DOC, doc.rev, value).map_err(|m| {
            Box::new(json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "save_failed",
                m,
            ))
        })?;
        Ok(view(&shared_state::read(DOC), &lock_keys()))
    })
    .await;
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

/// The stored Remote Play keys of a console, as hex: (regist key, rp key). The chosen user's.
#[cfg(test)]
fn rp_keys(host: &str) -> Option<(String, String)> {
    rp_keys_for(host, None).ok()
}

/// Why [`rp_keys_for`] found no keys.
pub(crate) enum NoKeys {
    /// The console is not paired here.
    NotPaired,
    /// It is, but no paired user is the one asked for: the names of those that are.
    NoSuchUser(Vec<String>),
}

/// The keys of one of a console's paired users (`user`: an account id, a name, "user N" or N, as
/// [`find_user`] reads it), or the chosen user's when `user` is None or blank.
pub(crate) fn rp_keys_for(host: &str, user: Option<&str>) -> Result<(String, String), NoKeys> {
    migrate();
    let keys = lock_keys();
    let c = keys.get(host).ok_or(NoKeys::NotPaired)?;
    let users = users_of(c);
    let Some(wanted) = user.map(str::trim).filter(|u| !u.is_empty()) else {
        return match (c.get(REGIST_KEY), c.get(RP_KEY)) {
            (Some(r), Some(k)) => Ok((r.clone(), k.clone())),
            // Users, but none chosen (a list saved by hand): the first.
            _ => users
                .first()
                .map(|u| (u.wake_regist_key.clone(), u.wake_rp_key.clone()))
                .ok_or(NoKeys::NotPaired),
        };
    };
    if users.is_empty() {
        return Err(NoKeys::NotPaired);
    }
    match find_user(&users, wanted) {
        Some(i) => Ok((
            users[i].wake_regist_key.clone(),
            users[i].wake_rp_key.clone(),
        )),
        None => Err(NoKeys::NoSuchUser(
            users
                .iter()
                .enumerate()
                .map(|(i, u)| display_name(i, u))
                .collect(),
        )),
    }
}

/// The name of the paired user whose regist key this is (hex), as the view shows it.
pub(crate) fn user_name_of(host: &str, regist_key_hex: &str) -> Option<String> {
    let keys = lock_keys();
    let users = users_of(keys.get(host)?);
    users
        .iter()
        .position(|u| {
            u.wake_regist_key
                .eq_ignore_ascii_case(regist_key_hex.trim())
        })
        .map(|i| display_name(i, &users[i]))
}

// ── Paired PS5 users ────────────────────────────────────────────────────────────────────────────

/// One PS5 user this app is paired as. No Debug: it holds keys.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub(crate) struct RpUser {
    /// The user's PSN account id, as paired (base64 of its 8 bytes). Empty for a pairing kept
    /// before users were (its account was not noted) or keys typed in by hand.
    #[serde(default)]
    account_id: String,
    /// The user's name on the console, for showing. Can be empty.
    #[serde(default)]
    name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    wake_credential: String,
    #[serde(default)]
    wake_regist_key: String,
    #[serde(default)]
    wake_rp_key: String,
}

/// The users in a document's `rp_users` (or the store's text of it): only those with usable keys,
/// each pairing once.
fn parse_users(v: &Value) -> Vec<RpUser> {
    let parsed: Option<Vec<RpUser>> = match v {
        Value::String(s) => serde_json::from_str(s).ok(),
        Value::Array(_) => serde_json::from_value(v.clone()).ok(),
        _ => None,
    };
    let mut out: Vec<RpUser> = Vec::new();
    for mut u in parsed.unwrap_or_default() {
        u.wake_regist_key = u.wake_regist_key.trim().to_ascii_lowercase();
        u.wake_rp_key = u.wake_rp_key.trim().to_ascii_lowercase();
        u.account_id = u.account_id.trim().to_string();
        u.name = u.name.trim().to_string();
        u.wake_credential = u.wake_credential.trim().to_string();
        let usable =
            ps5upload_core::rp_session::SessionCreds::from_hex(&u.wake_regist_key, &u.wake_rp_key)
                .is_ok();
        if usable && !out.iter().any(|o| o.wake_regist_key == u.wake_regist_key) {
            out.push(u);
        }
    }
    out
}

/// A console's paired users: its list, and the chosen keys as a user of their own when the list
/// does not hold them (a pairing from before the list, or keys typed in by hand).
fn users_of(c: &Creds) -> Vec<RpUser> {
    let mut users = c
        .get(RP_USERS)
        .map(|s| parse_users(&Value::String(s.clone())))
        .unwrap_or_default();
    if let (Some(r), Some(k)) = (c.get(REGIST_KEY), c.get(RP_KEY)) {
        let listed = users
            .iter()
            .any(|u| u.wake_regist_key.eq_ignore_ascii_case(r.trim()));
        let usable = ps5upload_core::rp_session::SessionCreds::from_hex(r, k).is_ok();
        if !listed && usable {
            users.insert(
                0,
                RpUser {
                    wake_credential: c.get(WAKE_CREDENTIAL).cloned().unwrap_or_default(),
                    wake_regist_key: r.trim().to_ascii_lowercase(),
                    wake_rp_key: k.trim().to_ascii_lowercase(),
                    ..Default::default()
                },
            );
        }
    }
    users
}

fn set_users(c: &mut Creds, users: &[RpUser]) {
    match serde_json::to_string(users) {
        Ok(text) if !users.is_empty() => {
            c.insert(RP_USERS.to_string(), text);
        }
        _ => {
            c.remove(RP_USERS);
        }
    }
}

/// How a user is named where only names go (the view, MCP): the name, else "user N" (from 1).
fn display_name(i: usize, u: &RpUser) -> String {
    if u.name.trim().is_empty() {
        format!("user {}", i + 1)
    } else {
        u.name.clone()
    }
}

/// The user `wanted` names: an account id, a name (any case), "user N", or N.
fn find_user(users: &[RpUser], wanted: &str) -> Option<usize> {
    let w = wanted.trim();
    users
        .iter()
        .position(|u| !u.account_id.is_empty() && u.account_id == w)
        .or_else(|| {
            users
                .iter()
                .enumerate()
                .position(|(i, u)| display_name(i, u).eq_ignore_ascii_case(w))
        })
        .or_else(|| {
            let n: usize = w.parse().ok()?;
            (1..=users.len()).contains(&n).then(|| n - 1)
        })
}

/// The paired users as anyone may see them: names, and which one Start uses.
fn users_view(c: &Creds) -> Value {
    let chosen = c.get(REGIST_KEY).map(|r| r.trim().to_ascii_lowercase());
    let users = users_of(c);
    Value::Array(
        users
            .iter()
            .enumerate()
            .map(|(i, u)| {
                json!({
                    "name": display_name(i, u),
                    "chosen": chosen.as_deref() == Some(u.wake_regist_key.as_str()),
                })
            })
            .collect(),
    )
}

/// A new pairing: added to the console's users (it replaces the same account's, or the same
/// keys'), and chosen, as a new pairing always was.
fn add_pairing(c: &mut Creds, mut new: RpUser) {
    let mut users = users_of(c);
    let same = users.iter().position(|u| {
        (!new.account_id.is_empty() && u.account_id == new.account_id)
            || u.wake_regist_key == new.wake_regist_key
    });
    match same {
        Some(i) => {
            if new.name.is_empty() {
                new.name = users[i].name.clone();
            }
            users[i] = new.clone();
        }
        None => users.push(new.clone()),
    }
    set_users(c, &users);
    choose(c, &new);
}

/// `u`'s keys become the ones everything uses.
fn choose(c: &mut Creds, u: &RpUser) {
    c.insert(REGIST_KEY.to_string(), u.wake_regist_key.clone());
    c.insert(RP_KEY.to_string(), u.wake_rp_key.clone());
    if u.wake_credential.is_empty() {
        c.remove(WAKE_CREDENTIAL);
    } else {
        c.insert(WAKE_CREDENTIAL.to_string(), u.wake_credential.clone());
    }
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
    if ps5upload_core::rp_session::SessionCreds::from_hex(&p.regist_key, &p.rp_key).is_ok() {
        add_pairing(
            &mut creds,
            RpUser {
                account_id: p.account_id_b64.trim().to_string(),
                name: p.user_name.trim().to_string(),
                wake_credential: p.credential.trim().to_string(),
                wake_regist_key: p.regist_key.trim().to_ascii_lowercase(),
                wake_rp_key: p.rp_key.trim().to_ascii_lowercase(),
            },
        );
    } else if !p.credential.trim().is_empty() {
        // A wake credential with no session keys: kept as it always was.
        creds.insert(WAKE_CREDENTIAL.to_string(), p.credential.clone());
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

// ---- a console found on the tailnet (tailnet_discover.rs) ----

/// What adding a console found on the tailnet does to the list: the same console at home and
/// away is one entry, at its home address when one is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TailnetPlan {
    /// Already listed (at this index), nothing to change.
    Exists(usize),
    /// Listed at its tailnet address, and its home address is now known: move it there (same id,
    /// so its keys and settings follow).
    Relink(usize, String),
    /// Listed twice, at home (`keep`) and at its tailnet address (`drop`): one entry, at home.
    Merge { keep: usize, drop: usize },
    /// Not listed: a new entry.
    Add,
}

/// The plan for a console with tailnet address `tailnet_ip`, home address `lan_host` when known,
/// and `hosts`: every other address this engine met the same console at (its AVA1 key's pins).
pub(crate) fn tailnet_plan(
    list: &[Value],
    tailnet_ip: &str,
    lan_host: Option<&str>,
    hosts: &[String],
) -> TailnetPlan {
    let at = |h: &str| {
        let k = key_of(h);
        (!k.is_empty())
            .then(|| list.iter().position(|p| key_of(str_of(p, "host")) == k))
            .flatten()
    };
    let at_lan = lan_host.and_then(at);
    let at_ts = at(tailnet_ip);
    match (at_lan, at_ts) {
        (Some(keep), Some(drop)) if keep != drop => TailnetPlan::Merge { keep, drop },
        (Some(i), _) => TailnetPlan::Exists(i),
        (None, Some(i)) => match lan_host {
            Some(h) => TailnetPlan::Relink(i, h.to_string()),
            None => TailnetPlan::Exists(i),
        },
        (None, None) => hosts
            .iter()
            .find_map(|h| at(h))
            .map(TailnetPlan::Exists)
            .unwrap_or(TailnetPlan::Add),
    }
}

/// `plan` applied to `list`; the entry it ends on (its index). `name` names a new entry.
fn apply_tailnet_plan(
    list: &mut Vec<Value>,
    plan: &TailnetPlan,
    tailnet_ip: &str,
    lan_host: Option<&str>,
    name: &str,
) -> usize {
    match plan {
        TailnetPlan::Exists(i) => *i,
        TailnetPlan::Relink(i, host) => {
            if let Some(p) = list[*i].as_object_mut() {
                // A name that was only the address follows it.
                if p.get("name").and_then(Value::as_str) == Some(tailnet_ip) {
                    p.insert("name".into(), host.clone().into());
                }
                p.insert("host".into(), host.clone().into());
            }
            *i
        }
        TailnetPlan::Merge { keep, drop } => {
            let gone = list[*drop].clone();
            if let (Some(k), Some(g)) = (list[*keep].as_object_mut(), gone.as_object()) {
                for (f, v) in g {
                    let empty = k
                        .get(f)
                        .is_none_or(|x| x.is_null() || x.as_str() == Some(""));
                    if f != "id" && f != "host" && f != "name" && empty && !v.is_null() {
                        k.insert(f.clone(), v.clone());
                    }
                }
            }
            list.remove(*drop);
            if *drop < *keep {
                keep - 1
            } else {
                *keep
            }
        }
        TailnetPlan::Add => {
            let host = lan_host.unwrap_or(tailnet_ip);
            let name = name.trim();
            list.push(json!({
                "id": new_id(),
                "name": if name.is_empty() { host } else { name },
                "host": host,
                "last_seen_at": null,
                "last_seen_kernel": null,
                "last_seen_payload": null,
            }));
            list.len() - 1
        }
    }
}

fn tailnet_action(plan: &TailnetPlan) -> &'static str {
    match plan {
        TailnetPlan::Exists(_) => "exists",
        TailnetPlan::Relink(..) => "linked",
        TailnetPlan::Merge { .. } => "merged",
        TailnetPlan::Add => "added",
    }
}

/// Writes one change to the list as `PUT /api/consoles` does (one writer, keys moved along).
fn change_list<R>(change: impl FnOnce(&mut Vec<Value>) -> Option<R>) -> Result<Option<R>, String> {
    migrate();
    let _g = ROSTER.lock().unwrap_or_else(|e| e.into_inner());
    let doc = shared_state::read(DOC);
    let old = profiles_of(doc.value.as_ref());
    let mut list = old.clone();
    let Some(r) = change(&mut list) else {
        return Ok(None);
    };
    let mut value = doc
        .value
        .clone()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let mut keys = lock_keys();
    if take_keys(&mut keys, &old, &mut list) {
        keys.save().map_err(|e| format!("{e:#}"))?;
    }
    value["profiles"] = Value::Array(list);
    shared_state::write(DOC, doc.rev, value)?;
    Ok(Some(r))
}

/// The console the list has for these addresses (see [`tailnet_plan`]): `{id, name, host}`.
pub(crate) fn tailnet_listed(tailnet_ip: &str, lan_host: Option<&str>, hosts: &[String]) -> Value {
    let list = profiles_of(shared_state::read(DOC).value.as_ref());
    let i = match tailnet_plan(&list, tailnet_ip, lan_host, hosts) {
        TailnetPlan::Exists(i) | TailnetPlan::Relink(i, _) => i,
        TailnetPlan::Merge { keep, .. } => keep,
        TailnetPlan::Add => return Value::Null,
    };
    let p = &list[i];
    json!({ "id": str_of(p, "id"), "name": str_of(p, "name"), "host": str_of(p, "host") })
}

/// Adds a console found on the tailnet, or finds it: `(action, {id, name, host})`, action being
/// `added`, `linked` (moved to its home address), `merged` (two entries became one) or `exists`.
pub(crate) fn tailnet_upsert(
    name: &str,
    tailnet_ip: &str,
    lan_host: Option<&str>,
    hosts: &[String],
) -> Result<(&'static str, Value), String> {
    let r = change_list(|list| {
        let plan = tailnet_plan(list, tailnet_ip, lan_host, hosts);
        let i = apply_tailnet_plan(list, &plan, tailnet_ip, lan_host, name);
        let p = &list[i];
        Some((
            tailnet_action(&plan),
            json!({ "id": str_of(p, "id"), "name": str_of(p, "name"), "host": str_of(p, "host") }),
        ))
    })?;
    r.ok_or_else(|| "nothing changed".to_string())
}

/// The console at `lan_host` reported tailnet address `tailnet_ip` (reached at home): an entry
/// added away at that tailnet address becomes the home one (moved, or merged into the entry
/// already at `lan_host`). Whether the list changed.
pub(crate) fn link_home(lan_host: &str, tailnet_ip: &str) -> bool {
    if key_of(lan_host) == key_of(tailnet_ip) {
        return false;
    }
    change_list(|list| {
        let plan = tailnet_plan(list, tailnet_ip, Some(lan_host), &[]);
        match plan {
            TailnetPlan::Relink(..) | TailnetPlan::Merge { .. } => {
                apply_tailnet_plan(list, &plan, tailnet_ip, Some(lan_host), "");
                Some(())
            }
            _ => None,
        }
    })
    .ok()
    .flatten()
    .is_some()
}

#[cfg(test)]
mod tailnet_merge_tests {
    use super::*;

    fn p(id: &str, host: &str) -> Value {
        json!({"id": id, "name": id, "host": host})
    }

    #[test]
    fn the_same_console_at_home_and_away_is_one_entry() {
        let ts = "100.64.0.9";
        // Never met: added at home when its home address is known, else at its tailnet address.
        assert_eq!(
            tailnet_plan(&[], ts, Some("192.168.1.20"), &[]),
            TailnetPlan::Add
        );
        let mut list = vec![p("a", "10.0.0.2")];
        let i = apply_tailnet_plan(&mut list, &TailnetPlan::Add, ts, None, "den-ps5u");
        assert_eq!(
            (str_of(&list[i], "host"), str_of(&list[i], "name")),
            (ts, "den-ps5u")
        );
        let i = apply_tailnet_plan(&mut list, &TailnetPlan::Add, ts, Some("192.168.1.20"), "");
        assert_eq!(str_of(&list[i], "host"), "192.168.1.20");

        // Listed at home already (the Mac's case): nothing new, whichever address is asked.
        let home = vec![p("a", "192.168.1.20")];
        assert_eq!(
            tailnet_plan(&home, ts, Some("192.168.1.20"), &[]),
            TailnetPlan::Exists(0)
        );
        // Home address unknown to the probe, but the same key was met there: still that entry.
        assert_eq!(
            tailnet_plan(&home, ts, None, &["192.168.1.20".into()]),
            TailnetPlan::Exists(0)
        );
        // With a port, or another spelling: the same entry.
        let ported = vec![p("a", "192.168.1.20:9021")];
        assert_eq!(
            tailnet_plan(&ported, ts, Some("192.168.1.20"), &[]),
            TailnetPlan::Exists(0)
        );

        // Added away at its tailnet address (the phone's case), then seen at home: moved.
        let mut away = vec![
            p("x", "10.0.0.5"),
            json!({"id": "b", "name": ts, "host": ts}),
        ];
        let plan = tailnet_plan(&away, ts, Some("192.168.1.20"), &[]);
        assert_eq!(plan, TailnetPlan::Relink(1, "192.168.1.20".into()));
        let i = apply_tailnet_plan(&mut away, &plan, ts, Some("192.168.1.20"), "");
        assert_eq!(away.len(), 2);
        assert_eq!(
            str_of(&away[i], "id"),
            "b",
            "same entry: its keys and settings follow"
        );
        assert_eq!(str_of(&away[i], "host"), "192.168.1.20");
        assert_eq!(
            str_of(&away[i], "name"),
            "192.168.1.20",
            "an address-only name follows"
        );
        // Asked again: exists, no duplicate.
        assert_eq!(
            tailnet_plan(&away, ts, Some("192.168.1.20"), &[]),
            TailnetPlan::Exists(1)
        );
        assert_eq!(tailnet_plan(&away, ts, None, &[]), TailnetPlan::Add);
        // (Away, the probe knows no home address: the entry now at home is found by the
        // addresses its key was met at.)
        assert_eq!(
            tailnet_plan(&away, ts, None, &["192.168.1.20".into()]),
            TailnetPlan::Exists(1)
        );

        // Listed at both (added away, then added again at home): merged into the home entry.
        let mut both = vec![
            json!({"id": "t", "name": "Den", "host": ts, "notes": "fan loud"}),
            p("h", "192.168.1.20"),
        ];
        let plan = tailnet_plan(&both, ts, Some("192.168.1.20"), &[]);
        assert_eq!(plan, TailnetPlan::Merge { keep: 1, drop: 0 });
        let i = apply_tailnet_plan(&mut both, &plan, ts, Some("192.168.1.20"), "");
        assert_eq!(both.len(), 1);
        assert_eq!(i, 0);
        assert_eq!(str_of(&both[0], "id"), "h");
        assert_eq!(
            str_of(&both[0], "notes"),
            "fan loud",
            "what only the dropped one had is kept"
        );
        assert_eq!(tailnet_action(&plan), "merged");
    }

    #[test]
    fn a_tailnet_entry_without_a_home_address_stays_as_it_is() {
        let ts = "100.64.0.9";
        let list = vec![p("t", ts)];
        assert_eq!(tailnet_plan(&list, ts, None, &[]), TailnetPlan::Exists(0));
        assert_eq!(tailnet_action(&TailnetPlan::Exists(0)), "exists");
        // Another console's tailnet address is not this one.
        assert_eq!(
            tailnet_plan(&list, "100.64.0.10", None, &[]),
            TailnetPlan::Add
        );
    }
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
    fn console_settings_are_checked_all_or_none_and_null_restores_the_default() {
        let mut cur = Map::new();
        cur.insert("rest_after_upload".into(), json!(true));
        let ok: Map<String, Value> = serde_json::from_value(json!({
            "wake_for_uploads": false,
            "keep_awake_mode": "always",
            "rest_after_upload": null,
        }))
        .unwrap();
        apply_console_settings(&mut cur, &ok).unwrap();
        assert_eq!(
            Value::Object(cur.clone()),
            json!({"wake_for_uploads": false, "keep_awake_mode": "always"})
        );
        // A wrong one changes nothing, and is named.
        for bad in [
            json!({"wake_for_uploads": true, "keep_awake_mode": "sometimes"}),
            json!({"auto_bring_up": "yes"}),
            json!({"theme": "dark"}),
            json!({"install_path_fallback": 1}),
        ] {
            let bad: Map<String, Value> = serde_json::from_value(bad).unwrap();
            let before = cur.clone();
            assert!(apply_console_settings(&mut cur, &bad).is_err());
            assert_eq!(cur, before);
        }
        let err = apply_console_settings(
            &mut cur,
            &serde_json::from_value(json!({"theme": "x"})).unwrap(),
        )
        .unwrap_err();
        assert!(
            err.contains("theme") && err.contains("keep_awake_mode"),
            "{err}"
        );
    }

    #[test]
    fn the_view_lists_each_console_settings_and_never_a_key() {
        let keys = KeyStore::in_memory();
        let v = console_view(
            &json!({"id": "a", "name": "A", "host": "192.0.2.9", "settings": {"rest_after_upload": true}}),
            &keys,
        );
        assert_eq!(v["settings"], json!({"rest_after_upload": true}));
        let v = console_view(&p("b", "192.0.2.10"), &keys);
        assert_eq!(v["settings"], json!({}));
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

        // A second user paired on the same console: both kept, the new one used by default.
        let second = ps5upload_core::rp_regist::PairResult {
            credential: "43".into(),
            regist_key: R2.into(),
            rp_key: K2.into(),
            account_id_b64: "QUFBQUFBQUI=".into(),
            user_name: "Bob".into(),
            ..Default::default()
        };
        remember_pairing("10.1.0.3", &second);
        assert_eq!(rp_keys("10.1.0.3"), Some((R2.into(), K2.into())));
        assert_eq!(wake_credential("10.1.0.3").as_deref(), Some("43"));
        assert!(matches!(
            rp_keys_for("10.1.0.3", Some("user 1")),
            Ok((r, _)) if r == R1
        ));
        assert!(matches!(
            rp_keys_for("10.1.0.3", Some("bob")),
            Ok((r, _)) if r == R2
        ));
        assert!(matches!(
            rp_keys_for("10.1.0.3", Some("Carol")),
            Err(NoKeys::NoSuchUser(names)) if names == ["user 1", "Bob"]
        ));
        assert!(matches!(
            rp_keys_for("10.1.0.77", Some("Bob")),
            Err(NoKeys::NotPaired)
        ));
        assert_eq!(user_name_of("10.1.0.3", R2).as_deref(), Some("Bob"));
        let (_, v) = body(list().await).await;
        assert_eq!(
            v["consoles"][1]["rp_users"],
            json!([{"name": "user 1", "chosen": false}, {"name": "Bob", "chosen": true}])
        );
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

    /// Fake keys for a second and third user (not real Remote Play keys).
    const R2: &str = "35653666376138620000000000000000";
    const K2: &str = "00112233445566778899aabbccddeeff";
    const R3: &str = "39616262636364640000000000000000";
    const K3: &str = "0f0e0d0c0b0a09080706050403020100";

    fn user(account: &str, name: &str, r: &str, k: &str) -> RpUser {
        RpUser {
            account_id: account.into(),
            name: name.into(),
            wake_credential: String::new(),
            wake_regist_key: r.into(),
            wake_rp_key: k.into(),
        }
    }

    fn legacy() -> Creds {
        Creds::from([
            (REGIST_KEY.to_string(), R1.to_string()),
            (RP_KEY.to_string(), K1.to_string()),
            (WAKE_CREDENTIAL.to_string(), "12345".to_string()),
        ])
    }

    #[test]
    fn a_pairing_from_before_users_reads_as_one_user_and_stays_the_default() {
        let c = legacy();
        let users = users_of(&c);
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].wake_regist_key, R1);
        assert_eq!(users[0].wake_credential, "12345");
        assert_eq!(users_view(&c), json!([{"name": "user 1", "chosen": true}]));
        // Nothing to move: the store is not rewritten for it.
        assert!(!c.contains_key(RP_USERS));
    }

    #[test]
    fn pairing_another_user_adds_one_and_again_replaces_it() {
        let mut c = legacy();
        add_pairing(&mut c, user("QUFBQUFBQUI=", "Alice", R2, K2));
        let users = users_of(&c);
        assert_eq!(users.len(), 2, "the old pairing is kept, first");
        assert_eq!(users[0].wake_regist_key, R1);
        assert_eq!(users[1].name, "Alice");
        // The new pairing is the one used, as a pairing always was.
        assert_eq!(c[REGIST_KEY], R2);
        assert!(
            !c.contains_key(WAKE_CREDENTIAL),
            "the old user's credential went with it"
        );
        assert_eq!(
            users_view(&c),
            json!([{"name": "user 1", "chosen": false}, {"name": "Alice", "chosen": true}])
        );
        // The same account again: replaced, the name kept when none came.
        add_pairing(&mut c, user("QUFBQUFBQUI=", "", R3, K3));
        let users = users_of(&c);
        assert_eq!(users.len(), 2);
        assert_eq!(users[1].wake_regist_key, R3);
        assert_eq!(users[1].name, "Alice");
    }

    #[test]
    fn a_user_is_found_by_account_name_or_number() {
        let mut c = legacy();
        add_pairing(&mut c, user("QUFBQUFBQUI=", "Alice", R2, K2));
        let users = users_of(&c);
        assert_eq!(find_user(&users, "QUFBQUFBQUI="), Some(1));
        assert_eq!(find_user(&users, "alice"), Some(1));
        assert_eq!(find_user(&users, "User 1"), Some(0));
        assert_eq!(find_user(&users, "1"), Some(0));
        assert_eq!(find_user(&users, "3"), None);
        assert_eq!(find_user(&users, "Bob"), None);
    }

    #[test]
    fn users_go_through_the_document_and_never_into_the_view() {
        let mut keys = KeyStore::open(None);
        let doc_users = json!([
            {"account_id": "QUFBQUFBQUE=", "name": "Alice", "wake_regist_key": R1, "wake_rp_key": K1},
            {"account_id": "QUFBQUFBQUI=", "name": "Bob", "wake_regist_key": R2, "wake_rp_key": K2.to_uppercase()},
            {"account_id": "bad", "name": "Broken", "wake_regist_key": "zz", "wake_rp_key": K2},
        ]);
        let mut list = vec![json!({
            "id": "a", "host": "10.0.0.2",
            "wake_regist_key": R2, "wake_rp_key": K2, "rp_users": doc_users,
        })];
        assert!(take_keys(&mut keys, &[], &mut list));
        assert!(!has_key_field(&list[0]));
        let c = keys.get("10.0.0.2").unwrap().clone();
        let users = users_of(&c);
        assert_eq!(users.len(), 2, "the unusable one is dropped");
        assert_eq!(users[1].wake_rp_key, K2, "keys kept in one case");

        let doc = Doc {
            rev: 1,
            value: Some(json!({"profiles": list})),
        };
        let mut full = doc.clone();
        with_keys(&keys, &mut full);
        let back = &full.value.as_ref().unwrap()["profiles"][0][RP_USERS];
        assert_eq!(back[1]["name"], "Bob");
        assert_eq!(back[0][REGIST_KEY], R1);

        let v = view(&doc, &keys);
        let text = v.to_string();
        for secret in [R1, K1, R2, K2, "QUFBQUFBQUE="] {
            assert!(!text.contains(secret), "{text}");
        }
        assert_eq!(
            v["consoles"][0]["rp_users"],
            json!([{"name": "Alice", "chosen": false}, {"name": "Bob", "chosen": true}])
        );

        // Clearing the list keeps the chosen keys, as one user.
        let mut cleared = vec![json!({"id": "a", "host": "10.0.0.2", "rp_users": null})];
        take_keys(&mut keys, &list, &mut cleared);
        let c = keys.get("10.0.0.2").unwrap();
        assert!(!c.contains_key(RP_USERS));
        assert_eq!(users_of(c).len(), 1);
    }

    #[test]
    fn clearing_the_keys_through_the_list_unpairs_every_user() {
        let mut c = legacy();
        add_pairing(&mut c, user("QUFBQUFBQUI=", "Alice", R2, K2));
        let mut cleared = input("10.0.0.2");
        cleared.regist_key = Some(String::new());
        cleared.rp_key = Some(String::new());
        let out = build_profiles(&[], vec![cleared]).unwrap();
        assert_eq!(out[0][RP_USERS], Value::Null);
        let mut keys = KeyStore::open(None);
        keys.set("10.0.0.2", c);
        let mut list = out;
        take_keys(&mut keys, &[p("a", "10.0.0.2")], &mut list);
        assert!(keys.get("10.0.0.2").is_none());
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
