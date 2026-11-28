//! Play Rooms: Recent players (design 7.5), for the host only.
//!
//! The last [`MAX`] people who played in this computer's rooms, for [`KEEP`]: name, device tag,
//! when, how long, how they came. Kept in `play_room_history.json` (0600, in the data folder),
//! never sent to guests, never in a bug report (`diag` has the count only).
//!
//! * `GET /api/join/history` → `{players: [{id, name, tag, last_unix, minutes, came}]}`, newest
//!   first (`last_unix`: when they last joined; `minutes`: how long they played then).
//! * `POST /api/join/history/forget {id}` or `{all: true}` → `{ok, left}`.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

const FILE: &str = "play_room_history.json";
/// People remembered at most.
pub(crate) const MAX: usize = 200;
/// How long someone is remembered after they last played.
pub(crate) const KEEP_S: u64 = 90 * 24 * 3600;

/// One person who played.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Played {
    /// This entry's id (for Forget): random, never their device's.
    pub id: String,
    pub name: String,
    /// Their device tag ("4F2A"), when their app or page said one.
    #[serde(default)]
    pub tag: Option<String>,
    /// When they last joined (unix seconds) and how long they played then.
    pub last_unix: u64,
    pub played_s: u64,
    /// How they came: invite, again, nearby, friends, rooms, group.
    pub came: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    players: Vec<Played>,
}

static LIST: Mutex<Option<Vec<Played>>> = Mutex::new(None);

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn with_list<R>(f: impl FnOnce(&mut Vec<Played>) -> R) -> R {
    let mut g = LIST.lock().unwrap_or_else(|e| e.into_inner());
    let v = g.get_or_insert_with(|| {
        crate::join_room::data_path(FILE)
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Stored>(&b).ok())
            .map(|s| s.players)
            .unwrap_or_default()
    });
    f(v)
}

fn save(v: &[Played]) {
    let Some(path) = crate::join_room::data_path(FILE) else {
        return; // the tests: memory only
    };
    let Ok(body) = serde_json::to_vec(&Stored {
        players: v.to_vec(),
    }) else {
        return;
    };
    if let Err(e) = crate::join_room::write_private(&path, &body) {
        crate::log_at!(
            Warn,
            "together",
            "join: recent players could not be saved: {e}"
        );
    }
}

/// Old entries out, newest first, at most [`MAX`].
fn tidy(v: &mut Vec<Played>, now: u64) {
    v.retain(|p| now.saturating_sub(p.last_unix + p.played_s) < KEEP_S);
    v.sort_by_key(|p| std::cmp::Reverse(p.last_unix));
    v.truncate(MAX);
}

/// Someone left after playing: remembered (the same name and tag again replaces their entry).
pub(crate) fn played(name: &str, tag: Option<&str>, came: &str, last_unix: u64, played_s: u64) {
    let now = unix_now();
    let Some(id) = crate::pad_link::random_key().map(|k| k.chars().take(12).collect::<String>())
    else {
        return;
    };
    let snapshot = with_list(|v| {
        v.retain(|p| !(p.name == name && p.tag.as_deref() == tag));
        v.push(Played {
            id,
            name: name.to_string(),
            tag: tag.map(str::to_string),
            last_unix,
            played_s,
            came: came.to_string(),
        });
        tidy(v, now);
        v.clone()
    });
    save(&snapshot);
}

/// The list as the host's views show it.
pub(crate) fn list() -> Vec<Played> {
    let now = unix_now();
    with_list(|v| {
        tidy(v, now);
        v.clone()
    })
}

/// Forgets one entry (`Some(id)`) or everyone (None): how many are left, or None when that id
/// isn't there.
pub(crate) fn forget(id: Option<&str>) -> Option<usize> {
    let (left, snapshot) = with_list(|v| {
        match id {
            Some(id) => {
                let before = v.len();
                v.retain(|p| p.id != id);
                if v.len() == before {
                    return None;
                }
            }
            None => v.clear(),
        }
        Some((v.len(), v.clone()))
    })?;
    save(&snapshot);
    Some(left)
}

/// What a bug report may carry: the count only (names are the host's friends).
pub(crate) fn diag() -> serde_json::Value {
    serde_json::json!({"recent_players": with_list(|v| v.len())})
}

#[cfg(test)]
pub(crate) fn test_clear() {
    *LIST.lock().unwrap_or_else(|e| e.into_inner()) = Some(Vec::new());
}

/// `GET /api/join/history` (host only).
pub async fn get(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = crate::join::host_only(&peer, &headers) {
        return r;
    }
    let players: Vec<_> = list()
        .into_iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "name": p.name,
                "tag": p.tag,
                "last_unix": p.last_unix,
                "minutes": p.played_s.div_ceil(60),
                "came": p.came,
            })
        })
        .collect();
    Json(serde_json::json!({ "players": players })).into_response()
}

#[derive(Deserialize)]
pub struct ForgetBody {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    all: bool,
}

/// `POST /api/join/history/forget {id}` or `{all: true}` (host only).
pub async fn forget_route(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<ForgetBody>,
) -> Response {
    if let Some(r) = crate::join::host_only(&peer, &headers) {
        return r;
    }
    let id = match (b.id.as_deref(), b.all) {
        (Some(id), false) if !id.is_empty() => Some(id),
        (None, true) => None,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "id_or_all"})),
            )
                .into_response()
        }
    };
    match forget(id) {
        Some(left) => Json(serde_json::json!({"ok": true, "left": left})).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "not_found"})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_players_are_newest_first_capped_and_expire() {
        let now = 10_000_000;
        let mut v: Vec<Played> = (0..MAX as u64 + 5)
            .map(|i| Played {
                id: format!("{i}"),
                name: format!("P{i}"),
                tag: None,
                last_unix: now - 1000 + i,
                played_s: 60,
                came: "invite".into(),
            })
            .collect();
        v.push(Played {
            id: "old".into(),
            name: "Old".into(),
            tag: Some("4F2A".into()),
            last_unix: now - KEEP_S - 100,
            played_s: 10,
            came: "rooms".into(),
        });
        tidy(&mut v, now);
        assert_eq!(v.len(), MAX);
        assert_eq!(v[0].name, format!("P{}", MAX + 4), "newest first");
        assert!(!v.iter().any(|p| p.id == "old"), "90 days on, forgotten");
    }
}
