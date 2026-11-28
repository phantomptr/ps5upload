//! Play again (apps/play/docs/EASY_JOIN.md): a guest's app the host let in can keep a **friend
//! pass** for this engine, and later ask to play again with one press, and the host can send it an
//! invite on its PS5.
//!
//! * The guest's hello says `"remember": true`; once the host lets them in, `"joined"` carries
//!   `"friend": "<pass>"` (32 random bytes, base64url). The engine keeps only its SHA-256, with
//!   the console it was for and the name the guest gave, in `play_together_friends.json` (mode
//!   0600): at most [`MAX_FRIENDS`], each forgotten after [`UNUSED_FOR`] without use.
//! * The hello may also name the guest app's waiting room on the hub, `"inbox": {"room", "hub"}`
//!   (a bound room the app hosts while it is open): kept with the pass, and only when `hub` is the
//!   hub this engine uses (it never dials a host a guest named).
//! * `{"hello": {"again": "<pass>"}}` asks the host again (join.rs `Entry::Again`): Play Together
//!   open on that console → a normal ask; closed while the console is being played → join.rs opens
//!   it, asks, and closes it again when the host says no. One ask per pass every [`AGAIN_GAP`].
//!   A pass never seats anyone by itself: the host lets them in every time.
//!
//! **Host routes** (this computer, or a peer `PS5UPLOAD_ALLOW_IP` admits): `GET
//! /api/join/friends` (`{friends: [{id, name, console, made_unix, last_unix, can_invite}]}`, never
//! a pass), `POST /api/join/friends/forget {id}`, `POST /api/join/friends/send {id, addr?}` (sends
//! the current invite of `addr`, else of the console the friend played on, to the friend's app
//! through the hub: `{"ok": true}`, or 409 `not_open` when their app isn't open now, 409
//! `no_inbox`, 409 `not_playing`, 502 `hub`).
//!
//! **The hand-off** on the hub (protocol v1): the engine joins the friend's room as a guest and
//! signals `{"invite": {"link", "links", "name"}}` to its host (the app), which answers
//! `{"invite_ok": true}` or `{"invite_ok": false, "why"}`.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message as TMsg;

use crate::pad_link::same_key;
use crate::vinput_api::host_key;

const FILE: &str = "play_together_friends.json";
/// Friends kept at most (the one used longest ago makes room).
pub(crate) const MAX_FRIENDS: usize = 64;
/// A pass not used for this long is forgotten.
pub(crate) const UNUSED_FOR: Duration = Duration::from_secs(180 * 24 * 3600);
/// One "again" ask per pass this often at most.
pub(crate) const AGAIN_GAP: Duration = Duration::from_secs(if cfg!(test) { 1 } else { 20 });
/// How long the friend's app has to take a sent invite.
const SEND_WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Friend {
    id: String,
    pass_sha256: String,
    /// The console (`host_key`) the guest played on: the only one the pass asks for.
    console: String,
    name: String,
    made_unix: u64,
    last_unix: u64,
    #[serde(default)]
    inbox_room: Option<String>,
    #[serde(default)]
    inbox_hub: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct FriendsFile {
    version: u32,
    friends: Vec<Friend>,
}

struct Store {
    path: Option<PathBuf>,
    friends: Vec<Friend>,
    /// When each pass last asked (by id).
    asked: std::collections::HashMap<String, Instant>,
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);

/// Where the friends are kept (the tests keep them in memory: never the user's data folder).
fn store_path() -> Option<PathBuf> {
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    {
        ps5upload_ava1::pool::data_dir().map(|d| d.join(FILE))
    }
}

fn with_store<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    let mut g = STORE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| Store::open(store_path())))
}

fn sha256_hex(s: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Store {
    fn open(path: Option<PathBuf>) -> Store {
        let friends = match path.as_deref().map(std::fs::read) {
            Some(Ok(b)) => match serde_json::from_slice::<FriendsFile>(&b) {
                Ok(f) => f.friends,
                Err(e) => {
                    if let Some(p) = path.as_deref() {
                        crate::log_at!(
                            Warn,
                            "together",
                            "join: the friends' file did not read ({e}); set aside"
                        );
                        let _ = std::fs::rename(p, p.with_extension("bad"));
                    }
                    Vec::new()
                }
            },
            _ => Vec::new(),
        };
        Store {
            path,
            friends,
            asked: Default::default(),
        }
    }

    fn save(&self) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        let body = match serde_json::to_vec_pretty(&FriendsFile {
            version: 1,
            friends: self.friends.clone(),
        }) {
            Ok(b) => b,
            Err(_) => return,
        };
        let tmp = path.with_extension("json.tmp");
        let r = crate::play_app::write_private(&tmp, &body).and_then(|_| {
            std::fs::rename(&tmp, path)?;
            Ok(())
        });
        if let Err(e) = r {
            crate::log_at!(Warn, "together", "join: friends not saved: {e:#}");
        }
    }

    fn prune(&mut self, now: u64) {
        let before = self.friends.len();
        self.friends
            .retain(|f| now.saturating_sub(f.last_unix) < UNUSED_FOR.as_secs());
        if self.friends.len() != before {
            self.save();
        }
    }

    /// The friend holding `pass`: every record compared, each in constant time.
    fn find(&self, pass: &str) -> Option<usize> {
        if pass.is_empty() || pass.len() > 128 {
            return None;
        }
        let h = sha256_hex(pass);
        let mut found = None;
        for (i, f) in self.friends.iter().enumerate() {
            if same_key(&f.pass_sha256, &h) {
                found = Some(i);
            }
        }
        found
    }
}

/// The waiting room a guest's hello names, when it is one this engine may use: a bound room id on
/// the hub this engine uses (`room` 22 base64url characters, `hub` that hub's host).
pub(crate) fn inbox_from_hello(v: Option<&Value>) -> Option<(String, String)> {
    let ours = crate::join_net::hub_host(&crate::join::settings().hub_url);
    inbox_on(v, ours.as_deref())
}

/// [`inbox_from_hello`] for the hub host `ours`.
fn inbox_on(v: Option<&Value>, ours: Option<&str>) -> Option<(String, String)> {
    let v = v?;
    let room = v.get("room")?.as_str()?;
    let hub = v.get("hub")?.as_str()?;
    let room_ok = room.len() == 22
        && room
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    (room_ok && ours == Some(hub)).then(|| (room.to_string(), hub.to_string()))
}

/// A new pass for a guest the host just let in on `console`: the pass (for "joined"), or None
/// (no randomness). A guest app that gives the same waiting room again replaces its old pass.
/// Friend passes this engine keeps (Play again over the internet needs the room's lobby on the
/// hub while any exist: join_net/lobby.rs).
pub(crate) fn count() -> usize {
    let t = unix_now();
    with_store(|s| {
        s.prune(t);
        s.friends.len()
    })
}

pub(crate) fn issue(console: &str, name: &str, inbox: Option<(String, String)>) -> Option<String> {
    let pass = crate::join_net::random_b64(32)?;
    let mut idb = [0u8; 4];
    getrandom::fill(&mut idb).ok()?;
    let id: String = idb.iter().map(|b| format!("{b:02x}")).collect();
    with_store(|s| s.issue(&id, &pass, console, name, inbox, unix_now()));
    Some(pass)
}

/// Who a pass is: its id, console and the name it was given.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Known {
    pub id: String,
    pub console: String,
    pub name: String,
}

/// Why an "again" ask was not let through.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AgainErr {
    /// Not a pass this engine knows (or forgotten).
    Unknown,
    /// Asked again too soon.
    SlowDown,
}

/// A guest asks to play again with `pass`: who they are, once per [`AGAIN_GAP`]. Marks it used,
/// and keeps the waiting room the hello named.
pub(crate) fn again(
    pass: &str,
    inbox: Option<(String, String)>,
    now: Instant,
) -> Result<Known, AgainErr> {
    with_store(|s| s.again(pass, inbox, now, unix_now()))
}

impl Store {
    fn issue(
        &mut self,
        id: &str,
        pass: &str,
        console: &str,
        name: &str,
        inbox: Option<(String, String)>,
        now: u64,
    ) {
        let console = host_key(console);
        self.prune(now);
        if let Some((room, _)) = &inbox {
            self.friends
                .retain(|f| !(f.console == console && f.inbox_room.as_ref() == Some(room)));
        }
        while self.friends.len() >= MAX_FRIENDS {
            let Some(i) = self
                .friends
                .iter()
                .enumerate()
                .min_by_key(|(_, f)| f.last_unix)
                .map(|(i, _)| i)
            else {
                break;
            };
            self.friends.remove(i);
        }
        let (inbox_room, inbox_hub) = match inbox {
            Some((r, h)) => (Some(r), Some(h)),
            None => (None, None),
        };
        self.friends.push(Friend {
            id: id.to_string(),
            pass_sha256: sha256_hex(pass),
            console,
            name: name.to_string(),
            made_unix: now,
            last_unix: now,
            inbox_room,
            inbox_hub,
        });
        self.save();
    }

    fn again(
        &mut self,
        pass: &str,
        inbox: Option<(String, String)>,
        now: Instant,
        t: u64,
    ) -> Result<Known, AgainErr> {
        self.prune(t);
        let i = self.find(pass).ok_or(AgainErr::Unknown)?;
        let id = self.friends[i].id.clone();
        if self
            .asked
            .get(&id)
            .is_some_and(|at| now.saturating_duration_since(*at) < AGAIN_GAP)
        {
            return Err(AgainErr::SlowDown);
        }
        self.asked
            .retain(|_, at| now.saturating_duration_since(*at) < AGAIN_GAP);
        self.asked.insert(id, now);
        let f = &mut self.friends[i];
        f.last_unix = t;
        if let Some((r, h)) = inbox {
            f.inbox_room = Some(r);
            f.inbox_hub = Some(h);
        }
        let k = Known {
            id: f.id.clone(),
            console: f.console.clone(),
            name: f.name.clone(),
        };
        self.save();
        Ok(k)
    }
}

fn list_json() -> Value {
    let t = unix_now();
    with_store(|s| {
        s.prune(t);
        let mut v: Vec<&Friend> = s.friends.iter().collect();
        v.sort_by_key(|f| std::cmp::Reverse(f.last_unix));
        json!({"friends": v.iter().map(|f| json!({
            "id": f.id,
            "name": f.name,
            "console": f.console,
            "made_unix": f.made_unix,
            "last_unix": f.last_unix,
            "can_invite": f.inbox_room.is_some(),
        })).collect::<Vec<_>>()})
    })
}

fn forget(id: &str) -> bool {
    with_store(|s| {
        let before = s.friends.len();
        s.friends.retain(|f| f.id != id);
        let gone = s.friends.len() != before;
        if gone {
            s.save();
        }
        gone
    })
}

/// A waiting room on the hub: (room id, hub host).
type Inbox = (String, String);

/// The friend's console, name and waiting room, by id.
fn of(id: &str) -> Option<(String, String, Option<Inbox>)> {
    with_store(|s| {
        s.friends.iter().find(|f| f.id == id).map(|f| {
            (
                f.console.clone(),
                f.name.clone(),
                f.inbox_room.clone().zip(f.inbox_hub.clone()),
            )
        })
    })
}

/// Why a hand-off did not go.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SendErr {
    /// The app's room isn't on the hub (the app is closed).
    NotOpen,
    /// The hub could not be reached, or said something else.
    Hub(String),
    /// The app answered no (`why`).
    Refused(String),
}

/// Joins the app's waiting `room` on the hub at `url` and hands it `invite` (the hand-off's
/// `data`); waits for its answer.
pub(crate) async fn hand_off(url: &str, room: &str, invite: Value) -> Result<(), SendErr> {
    let mut hub = crate::join_net::hub::connect(url)
        .await
        .map_err(SendErr::Hub)?;
    let text = |v: Value| TMsg::Text(v.to_string().into());
    let r = async {
        hub.send(text(json!({"v": 1, "t": "join", "room": room})))
            .await
            .map_err(|e| SendErr::Hub(e.to_string()))?;
        let mut joined = false;
        loop {
            let m = match hub.next().await {
                Some(Ok(TMsg::Text(t))) => {
                    serde_json::from_str::<Value>(t.as_str()).unwrap_or_default()
                }
                Some(Ok(_)) => continue,
                _ => return Err(SendErr::Hub("closed".into())),
            };
            match (m["t"].as_str(), joined) {
                (Some("joined"), false) => {
                    joined = true;
                    hub.send(text(
                        json!({"v": 1, "t": "signal", "to": "host", "data": {"invite": invite.clone()}}),
                    ))
                    .await
                    .map_err(|e| SendErr::Hub(e.to_string()))?;
                }
                (Some("error"), _) if m["why"] == "no_room" => return Err(SendErr::NotOpen),
                (Some("error"), _) | (Some("closed"), _) => {
                    return Err(SendErr::Hub(m["why"].as_str().unwrap_or("closed").into()))
                }
                (Some("signal"), true) if m["data"].get("invite_ok").is_some() => {
                    return if m["data"]["invite_ok"] == true {
                        Ok(())
                    } else {
                        Err(SendErr::Refused(
                            m["data"]["why"]
                                .as_str()
                                .unwrap_or("no")
                                .chars()
                                .take(24)
                                .collect(),
                        ))
                    };
                }
                _ => {}
            }
        }
    };
    let out = match tokio::time::timeout(SEND_WAIT, r).await {
        Ok(r) => r,
        Err(_) => Err(SendErr::Hub("timeout".into())),
    };
    let _ = hub.send(text(json!({"v": 1, "t": "leave"}))).await;
    let _ = hub.close(None).await;
    out
}

// ── Routes ───────────────────────────────────────────────────────────────────────────────────────

fn bad(code: StatusCode, err: &str) -> Response {
    (code, Json(json!({"error": err}))).into_response()
}

/// `GET /api/join/friends`.
pub async fn list(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = crate::join::host_only(&peer, &headers) {
        return r;
    }
    Json(list_json()).into_response()
}

#[derive(Deserialize)]
pub struct IdBody {
    id: String,
}

/// `POST /api/join/friends/forget {id}`: the pass stops working.
pub async fn forget_route(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<IdBody>,
) -> Response {
    if let Some(r) = crate::join::host_only(&peer, &headers) {
        return r;
    }
    let ok = forget(&b.id);
    if ok {
        crate::log_at!(Info, "together", "join: forgot friend {}", b.id);
    }
    Json(json!({"ok": ok})).into_response()
}

#[derive(Deserialize)]
pub struct SendBody {
    id: String,
    /// The console whose invite goes (default: the one the friend played on).
    #[serde(default, alias = "console")]
    addr: Option<String>,
}

/// `POST /api/join/friends/send {id, addr?}`: the current invite to the friend's PS5.
pub async fn send(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<SendBody>,
) -> Response {
    if let Some(r) = crate::join::host_only(&peer, &headers) {
        return r;
    }
    let Some((console, _name, inbox)) = of(&b.id) else {
        return bad(StatusCode::NOT_FOUND, "unknown");
    };
    let Some((room, hub)) = inbox else {
        return bad(StatusCode::CONFLICT, "no_inbox");
    };
    let addr = b.addr.unwrap_or(console);
    let Some(invite) = crate::join::invite_for_friend(&addr).await else {
        return bad(StatusCode::CONFLICT, "not_playing");
    };
    let url = crate::share::client::hub_url(&hub);
    match hand_off(&url, &room, invite).await {
        Ok(()) => {
            crate::log_at!(Info, "together", "join: invite sent to friend {}", b.id);
            Json(json!({"ok": true})).into_response()
        }
        Err(SendErr::NotOpen) => bad(StatusCode::CONFLICT, "not_open"),
        Err(SendErr::Refused(why)) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "refused", "why": why})),
        )
            .into_response(),
        Err(SendErr::Hub(e)) => {
            crate::log_at!(
                Debug,
                "together",
                "join: invite to friend {}: hub {e}",
                b.id
            );
            bad(StatusCode::BAD_GATEWAY, "hub")
        }
    }
}

#[cfg(test)]
#[path = "join_friends_tests.rs"]
mod tests;
