//! Settings backups (8.0.1): copies of an app's settings bundle (the same JSON as Settings › Export
//! bundle), kept by the engine in `<data dir>/settings-backups`. The app saves one a day and one
//! before a sync or a restore changes several settings at once; the newest KEEP stay. In Docker
//! the data dir is the container's volume, so the web UI's backups outlive the browser.
//!
//! * `GET  /api/settings-backup`          -> `{backups:[{id,at,reason,device,keys,bytes}]}` newest first
//! * `GET  /api/settings-backup/get?id=`  -> the bundle
//! * `POST /api/settings-backup` `{reason,device,bundle}` -> `{id}`
//!
//! A bundle holds the roster, and the roster holds what wakes each console and signs in to it
//! (the Remote Play keys). Those never reach the disk readable: they are taken out of the roster
//! and kept in the file encrypted with the saved connections' key (`connections.key`, this user
//! only). They come back only to a caller on this computer. Anyone else, such as another machine
//! reaching a Docker engine, gets the bundle without them (`meta.secrets_withheld`), and restoring
//! that keeps the keys the browser already has (`applyBundle`). A backup from before this is sealed
//! the first time the engine starts.
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use axum::extract::{ConnectInfo, Query};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::json_err;

const KEEP: usize = 14;
/// A bundle larger than this is refused (it would be something other than settings).
const MAX_BYTES: usize = 8 * 1024 * 1024;

/// The local storage key of the app's console list.
const ROSTER_KEY: &str = "ps5upload.roster.v1";
/// What a console's saved profile holds that opens the console: not settings.
const SECRET_FIELDS: [&str; 4] = [
    "wake_credential",
    "wake_regist_key",
    "wake_rp_key",
    // Each paired PS5 user's keys (consoles_api.rs).
    "rp_users",
];
/// Where a bundle on disk keeps [`SECRET_FIELDS`], encrypted: `{profile id: {field: value}}`.
const SEALED: &str = "sealed_secrets";

fn dir() -> Option<PathBuf> {
    Some(crate::remote::store::data_dir()?.join("settings-backups"))
}

fn secrets_key() -> Result<[u8; 32]> {
    let data = crate::remote::store::data_dir().ok_or_else(|| anyhow!("no data directory"))?;
    std::fs::create_dir_all(&data).with_context(|| format!("make {}", data.display()))?;
    crate::remote::store::load_or_create_key(&data.join("connections.key"))
}

/// Takes [`SECRET_FIELDS`] out of the bundle's roster: `{profile id: {field: value}}`, without the
/// empty ones. `None` when the roster holds none, or is not one this can read (left as it is:
/// a roster the app itself cannot read holds nothing it could use).
fn take_secrets(bundle: &mut Value) -> Option<serde_json::Map<String, Value>> {
    let text = bundle.get("local_storage")?.get(ROSTER_KEY)?.as_str()?;
    let mut roster: Value = serde_json::from_str(text).ok()?;
    let mut taken = serde_json::Map::new();
    for profile in roster.get_mut("profiles")?.as_array_mut()? {
        let Some(fields) = profile.as_object_mut() else {
            continue;
        };
        let id = fields
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut secrets = serde_json::Map::new();
        for field in SECRET_FIELDS {
            if let Some(v) = fields.remove(field) {
                let held = v.as_str().is_some_and(|s| !s.is_empty())
                    || v.as_array().is_some_and(|a| !a.is_empty());
                if held {
                    secrets.insert(field.to_string(), v);
                }
            }
        }
        if !secrets.is_empty() {
            taken.insert(id, Value::Object(secrets));
        }
    }
    bundle["local_storage"][ROSTER_KEY] = Value::String(roster.to_string());
    (!taken.is_empty()).then_some(taken)
}

/// Puts what [`take_secrets`] took back in the roster, by profile id.
fn put_secrets(bundle: &mut Value, secrets: &serde_json::Map<String, Value>) {
    let Some(text) = bundle
        .get("local_storage")
        .and_then(|l| l.get(ROSTER_KEY))
        .and_then(Value::as_str)
    else {
        return;
    };
    let Ok(mut roster) = serde_json::from_str::<Value>(text) else {
        return;
    };
    let Some(profiles) = roster.get_mut("profiles").and_then(Value::as_array_mut) else {
        return;
    };
    for profile in profiles {
        let held = profile
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| secrets.get(id))
            .and_then(Value::as_object);
        if let (Some(held), Some(fields)) = (held, profile.as_object_mut()) {
            for (k, v) in held {
                fields.insert(k.clone(), v.clone());
            }
        }
    }
    bundle["local_storage"][ROSTER_KEY] = Value::String(roster.to_string());
}

/// Moves the roster's secrets into [`SEALED`]. Without a key they are dropped, not written in the
/// clear: the backup is still worth having, and wake can be set up again.
fn seal_secrets(bundle: &mut Value, key: Option<&[u8; 32]>) -> Result<bool> {
    let Some(secrets) = take_secrets(bundle) else {
        return Ok(false);
    };
    if let Some(key) = key {
        let plain = serde_json::to_vec(&secrets)?;
        bundle[SEALED] = Value::String(crate::remote::store::seal(key, &plain)?);
    }
    Ok(true)
}

/// The bundle as a caller gets it. `key` is `Some` only for a caller allowed the secrets.
fn reveal(bundle: &mut Value, key: Option<&[u8; 32]>) {
    let sealed = bundle
        .as_object_mut()
        .and_then(|o| o.remove(SEALED))
        .and_then(|v| v.as_str().map(str::to_string));
    // A file sealed by nobody yet, in the clear.
    let mut withheld = take_secrets(bundle).is_some() || sealed.is_some();
    if let (Some(key), Some(sealed)) = (key, sealed) {
        let opened = crate::remote::store::open_sealed(key, &sealed).and_then(|plain| {
            serde_json::from_slice::<serde_json::Map<String, Value>>(&plain).ok()
        });
        if let Some(secrets) = opened {
            put_secrets(bundle, &secrets);
            withheld = false;
        }
    }
    if withheld {
        if let Some(meta) = bundle.get_mut("meta").and_then(Value::as_object_mut) {
            meta.insert("secrets_withheld".to_string(), Value::Bool(true));
        }
    }
}

/// Seals every backup in `folder` still holding secrets in the clear. Written by a build before
/// this one; they would otherwise stay readable until they rotate out.
fn seal_folder(folder: &Path, key: Option<&[u8; 32]>) {
    let Ok(rd) = std::fs::read_dir(folder) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !valid_id(&name) {
            continue;
        }
        let path = e.path();
        let Some(mut bundle) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        else {
            continue;
        };
        if !matches!(seal_secrets(&mut bundle, key), Ok(true)) {
            continue;
        }
        let tmp = folder.join(format!("{name}.tmp"));
        let written = serde_json::to_vec(&bundle)
            .map_err(anyhow::Error::from)
            .and_then(|body| Ok(std::fs::write(&tmp, body)?))
            .and_then(|()| Ok(std::fs::rename(&tmp, &path)?));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            crate::log_warn!("settings backup: could not seal {name}: {e:#}");
        }
    }
}

/// At the engine's start: see [`seal_folder`].
pub fn seal_old_backups() {
    if let Some(d) = dir().filter(|d| d.is_dir()) {
        seal_folder(&d, secrets_key().ok().as_ref());
    }
}

#[derive(Serialize)]
struct Entry {
    id: String,
    at: u64,
    reason: String,
    device: String,
    keys: usize,
    bytes: u64,
}

/// `<ms>-<reason>.json`; the reason is a short [a-z-] word.
fn valid_id(id: &str) -> bool {
    let Some(stem) = id.strip_suffix(".json") else {
        return false;
    };
    let mut parts = stem.splitn(2, '-');
    let at = parts.next().unwrap_or("");
    let reason = parts.next().unwrap_or("");
    !at.is_empty()
        && at.bytes().all(|b| b.is_ascii_digit())
        && reason.len() <= 24
        && reason.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
}

fn entries() -> Vec<(PathBuf, String)> {
    let Some(d) = dir() else { return Vec::new() };
    let mut v: Vec<(PathBuf, String)> = std::fs::read_dir(&d)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    valid_id(&n).then(|| (e.path(), n))
                })
                .collect()
        })
        .unwrap_or_default();
    // Newest first: the ids start with the time.
    v.sort_by_key(|(_, n)| {
        std::cmp::Reverse(
            n.split('-')
                .next()
                .and_then(|t| t.parse::<u64>().ok())
                .unwrap_or(0),
        )
    });
    v
}

fn list_now() -> Result<Value> {
    let out: Vec<Entry> = entries()
        .into_iter()
        .filter_map(|(p, id)| {
            let bytes = std::fs::metadata(&p).ok()?.len();
            let v: Value = serde_json::from_slice(&std::fs::read(&p).ok()?).ok()?;
            let meta = v.get("meta");
            let s = |k: &str| {
                meta.and_then(|m| m.get(k))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            Some(Entry {
                at: meta
                    .and_then(|m| m.get("generated_at_ms"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                reason: s("reason"),
                device: s("device"),
                keys: v
                    .get("local_storage")
                    .and_then(Value::as_object)
                    .map_or(0, |m| m.len()),
                bytes,
                id,
            })
        })
        .collect();
    Ok(json!({ "backups": out }))
}

#[derive(Deserialize)]
pub struct SaveReq {
    reason: String,
    bundle: Value,
}

fn save_now(mut req: SaveReq) -> Result<Value> {
    if !req
        .bundle
        .get("local_storage")
        .is_some_and(Value::is_object)
    {
        bail!("that is not a settings bundle");
    }
    let key = secrets_key();
    if let Err(e) = &key {
        crate::log_warn!("settings backup: saved without wake keys: {e:#}");
    }
    seal_secrets(&mut req.bundle, key.ok().as_ref())?;
    let reason: String = req
        .reason
        .chars()
        .filter(|c| c.is_ascii_lowercase() || *c == '-')
        .take(24)
        .collect();
    let body = serde_json::to_vec(&req.bundle)?;
    if body.len() > MAX_BYTES {
        bail!("the bundle is too large to be settings");
    }
    let d = dir().ok_or_else(|| anyhow!("no data directory"))?;
    std::fs::create_dir_all(&d).with_context(|| format!("make {}", d.display()))?;
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |t| t.as_millis() as u64);
    // Two in the same millisecond: the later one takes the next free id, never overwrites.
    let reason = if reason.is_empty() {
        "manual".to_string()
    } else {
        reason
    };
    let mut at = at;
    let mut id = format!("{at}-{reason}.json");
    while d.join(&id).exists() {
        at += 1;
        id = format!("{at}-{reason}.json");
    }
    let tmp = d.join(format!("{id}.tmp"));
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, d.join(&id))?;
    for (p, _) in entries().into_iter().skip(KEEP) {
        let _ = std::fs::remove_file(p);
    }
    Ok(json!({ "id": id }))
}

async fn answer(f: impl FnOnce() -> Result<Value> + Send + 'static) -> Response {
    match tokio::task::spawn_blocking(f)
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    }
}

pub async fn list() -> Response {
    answer(list_now).await
}

pub async fn save(Json(req): Json<SaveReq>) -> Response {
    answer(move || save_now(req)).await
}

#[derive(Deserialize)]
pub struct GetQuery {
    id: String,
}

pub async fn get(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<GetQuery>,
) -> Response {
    // This computer only, addressed as itself: the rule of the routes that act as the user.
    let local =
        crate::vinput_api::local_allowed(peer.ip()) && crate::vinput_api::host_is_local(&headers);
    answer(move || {
        if !valid_id(&q.id) {
            bail!("no such backup");
        }
        let d = dir().ok_or_else(|| anyhow!("no data directory"))?;
        let bytes = std::fs::read(d.join(&q.id)).map_err(|_| anyhow!("no such backup"))?;
        let mut bundle: Value = serde_json::from_slice(&bytes)?;
        let key = if local { secrets_key().ok() } else { None };
        reveal(&mut bundle, key.as_ref());
        Ok(bundle)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [9; 32];

    fn bundle() -> Value {
        let roster = json!({
            "active_id": "a",
            "profiles": [
                { "id": "a", "name": "Pro", "host": "10.0.0.2", "wake_credential": "111",
                  "wake_regist_key": "aabb", "wake_rp_key": "ccdd" },
                { "id": "b", "name": "Phat", "host": "10.0.0.3", "wake_credential": null },
            ],
        });
        json!({
            "meta": { "schema": 1 },
            "local_storage": { ROSTER_KEY: roster.to_string(), "ps5upload.theme": "dark" },
        })
    }

    fn profile(b: &Value, i: usize) -> Value {
        let roster = b["local_storage"][ROSTER_KEY].as_str().unwrap();
        serde_json::from_str::<Value>(roster).unwrap()["profiles"][i].clone()
    }

    #[test]
    fn a_saved_bundle_holds_no_key_in_the_clear() {
        let mut b = bundle();
        assert!(seal_secrets(&mut b, Some(&KEY)).unwrap());
        let on_disk = b.to_string();
        for secret in ["aabb", "ccdd", "wake_credential", "wake_rp_key", "\"111\""] {
            assert!(!on_disk.contains(secret), "{secret} in {on_disk}");
        }
        assert_eq!(profile(&b, 0)["name"], "Pro");
        assert_eq!(b["local_storage"]["ps5upload.theme"], "dark");
    }

    #[test]
    fn this_computer_gets_the_keys_back_and_a_restore_can_wake() {
        let mut b = bundle();
        seal_secrets(&mut b, Some(&KEY)).unwrap();
        reveal(&mut b, Some(&KEY));
        let p = profile(&b, 0);
        assert_eq!(p["wake_credential"], "111");
        assert_eq!(p["wake_regist_key"], "aabb");
        assert_eq!(p["wake_rp_key"], "ccdd");
        assert!(b.get(SEALED).is_none());
        assert!(b["meta"].get("secrets_withheld").is_none());
    }

    #[test]
    fn anyone_else_gets_the_bundle_without_them_and_is_told() {
        let mut b = bundle();
        seal_secrets(&mut b, Some(&KEY)).unwrap();
        reveal(&mut b, None);
        assert!(b.get(SEALED).is_none(), "not even encrypted");
        assert!(profile(&b, 0).get("wake_regist_key").is_none());
        assert_eq!(profile(&b, 0)["name"], "Pro");
        assert_eq!(b["meta"]["secrets_withheld"], true);
    }

    #[test]
    fn a_backup_from_before_sealing_is_never_served_in_the_clear() {
        let mut b = bundle();
        reveal(&mut b, None);
        assert!(!b.to_string().contains("aabb"));
        assert_eq!(b["meta"]["secrets_withheld"], true);
    }

    #[test]
    fn with_another_key_the_keys_are_gone_and_the_rest_still_restores() {
        let mut b = bundle();
        seal_secrets(&mut b, Some(&KEY)).unwrap();
        reveal(&mut b, Some(&[1; 32]));
        assert!(profile(&b, 0).get("wake_regist_key").is_none());
        assert_eq!(b["meta"]["secrets_withheld"], true);
    }

    #[test]
    fn with_no_key_the_secrets_are_dropped_not_written() {
        let mut b = bundle();
        assert!(seal_secrets(&mut b, None).unwrap());
        assert!(!b.to_string().contains("aabb"));
        assert!(b.get(SEALED).is_none());
    }

    #[test]
    fn a_bundle_with_no_keys_is_left_alone() {
        let mut b = json!({ "meta": {}, "local_storage": { "ps5upload.theme": "dark" } });
        assert!(!seal_secrets(&mut b, Some(&KEY)).unwrap());
        let same = b.clone();
        reveal(&mut b, Some(&KEY));
        assert_eq!(b, same);
    }

    #[test]
    fn old_backups_in_the_folder_are_sealed_in_place() {
        let d = crate::remote::store::test_dir();
        let old = d.join("1729800000000-daily.json");
        std::fs::write(&old, bundle().to_string()).unwrap();
        let other = d.join("notes.json");
        std::fs::write(&other, "wake_regist_key aabb").unwrap();
        seal_folder(&d, Some(&KEY));
        let text = std::fs::read_to_string(&old).unwrap();
        assert!(!text.contains("aabb"), "{text}");
        assert!(text.contains(SEALED));
        // And it still restores.
        let mut b: Value = serde_json::from_str(&text).unwrap();
        reveal(&mut b, Some(&KEY));
        assert_eq!(profile(&b, 0)["wake_regist_key"], "aabb");
        // Only backups are touched.
        assert_eq!(
            std::fs::read_to_string(&other).unwrap(),
            "wake_regist_key aabb"
        );
    }

    #[test]
    fn ids_cannot_leave_the_folder() {
        assert!(valid_id("1729800000000-daily.json"));
        assert!(valid_id("1729800000000-before-sync.json"));
        for bad in [
            "../x.json",
            "1729-../../etc.json",
            "1729-daily",
            "-daily.json",
            "abc-daily.json",
            "1729-Daily.json",
            "1729-a/b.json",
        ] {
            assert!(!valid_id(bad), "{bad}");
        }
    }
}
