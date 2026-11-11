//! Share: send an app's things to a friend (Games › an app › Share, Collection › an item ›
//! Share). The sender picks what to share and gets a link (and its QR code); the friend opens it,
//! sees what is offered and accepts some of it; the sender is asked, "<name> wants: …", and
//! allows or declines; only then do the files go, straight from one computer to the other.
//!
//! **What can be shared**: the app itself, homebrew only ([`retail`]: the engine reads how the
//! app was built from its own PARAM and refuses a commercial game, its updates and add-ons in any
//! format, whatever the page sent); saves of any
//! game (the files inside, as Share a save makes them: save_copy.rs); screenshots and clips;
//! cheats and which are on; controls; stream settings; a note.
//!
//! **The connection**: the link names a room on the signalling hub, the sender's DTLS
//! fingerprint, the hub, and the sender's page on the home network as a hint. No secret: the room
//! only lets someone ask. The hub introduces the two sides (join_net/hub.rs, a room of
//! [`Kind::Share`]); the data goes over a WebRTC data channel labelled `"share"`, DTLS-encrypted,
//! ICE choosing the home network, then Tailscale, then the internet. With the hub out of reach a
//! receiver on the same network uses the sender's `/api/share/ws` (the LAN hint) instead.
//! [`wire`] has the messages; [`client`] the receiving side's connection.
//!
//! **What the friend can read**: only the files on the lists of the items they accepted, by
//! item id and path, each checked again when served ([`manifest`]): no `..`, no absolute path, no
//! symbolic link anywhere on the way.
//!
//! **The link's life** ([`machine`]): it expires (24 h by default, 1 h to 7 days) and is one-use
//! by default (the first Allow uses it up; an allowed friend can still reconnect with the token
//! the Allow gave and carry on where the files stopped). Cancel stops everything at once.
//!
//! **Routes** (this computer only, but `/share` and `/api/share/ws`):
//! * `POST /api/share` make a share; `POST /api/share/check` the same checks without making it;
//!   `GET /api/share` shares, receives and history; `GET /api/share/{id}`;
//!   `POST /api/share/{id}/decide {request, allow}`; `DELETE /api/share/{id}` cancels.
//! * `POST /api/share/receive {link, name}` opens a link; `GET /api/share/receive/{id}`;
//!   `POST /api/share/receive/{id}/accept {items, dest?}`; `DELETE /api/share/receive/{id}`;
//!   `POST /api/share/receive/{id}/use-save {addr, folder, user_id?, item}`;
//!   `GET /api/share/receive/{id}/data/{item}` the small data items (controls, a note…).
//!
//! **Diagnostics**: what is shown is in memory and read when asked; one log line per share
//! event (category `together`), nothing periodic, nothing on the hub. History is a small file.

pub(crate) mod client;
pub(crate) mod machine;
pub(crate) mod manifest;
pub(crate) mod retail;
pub(crate) mod wire;

#[cfg(test)]
mod tests;

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, Path as AxPath};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};

use crate::join_net::Kind;
use machine::{Machine, ReqState, ShareState};
use manifest::{FileEntry, Item, ItemKind, Loc};
use wire::{RxCmd, RxEnd, RxShared};

/// How long a link works unless the sender chooses.
pub(crate) const DEFAULT_TTL: Duration = Duration::from_secs(24 * 3600);
const MIN_TTL: Duration = Duration::from_secs(60);
const MAX_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// Shares kept at once (ended ones go first).
const MAX_SHARES: usize = 32;
/// Receives kept at once.
const MAX_RECEIVES: usize = 16;
/// History entries kept.
const HISTORY_MAX: usize = 100;
/// Reconnects a receive makes after its connection dropped mid-way.
const RECONNECTS: u32 = 3;
const NOTE_MAX: usize = 2000;

fn bad(code: StatusCode, err: impl Into<String>) -> Response {
    (code, Json(json!({"error": err.into()}))).into_response()
}

fn now_ms() -> u64 {
    wire::now_ms()
}

fn event(v: Value) {
    crate::event_bus::bus().publish_json("share", &v);
}

// ── The sending side's shares ───────────────────────────────────────────────────────────────────

pub(crate) struct Entry {
    pub id: String,
    pub from: String,
    pub title: String,
    pub title_id: Option<String>,
    pub created_ms: u64,
    pub items: Arc<Vec<Item>>,
    pub m: Machine,
    /// The room on the hub (None when invites work only on the home network, or no hub).
    room: Option<crate::join_net::hub::Room>,
    /// The room's name when there is no hub room.
    lan_room: String,
    /// The engine's page on the home network (`http://192.168.1.10:19113/share`).
    lan: Option<String>,
    /// Files made for the share (a save's zip), gone when it ends.
    staging: Option<PathBuf>,
    changed: watch::Sender<u64>,
    /// When each request started sending, for the activity and the history.
    started: Vec<(u64, u64)>,
    /// Requests whose ending went into the history.
    recorded: Vec<u64>,
}

impl Entry {
    fn room_id(&self) -> String {
        self.room
            .as_ref()
            .map(|r| r.id())
            .unwrap_or_else(|| self.lan_room.clone())
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n += 1);
    }

    /// The link, and the home network's.
    fn links(&self) -> (Option<String>, Option<String>, &'static str) {
        let room = self.room_id();
        let lan_link = self.lan.as_ref().map(|l| format!("{l}#room={room}"));
        let net = self.room.as_ref().and_then(|r| {
            if r.offline() {
                return None;
            }
            let fp = crate::join_net::fingerprint()?;
            let hub = crate::join_net::hub_host(r.url())?;
            let mut s = format!(
                "{}#room={room}&fp={fp}&hub={hub}",
                crate::join_net::SHARE_PAGE
            );
            if let Some(l) = &self.lan {
                s.push_str("&lan=");
                s.push_str(&crate::join_net::pct(l));
            }
            Some(s)
        });
        let works = if net.is_some() { "anywhere" } else { "wifi" };
        (net.or(lan_link.clone()), lan_link, works)
    }

    fn json(&mut self, now: Instant) -> Value {
        let state = self.m.tick(now);
        let (url, lan_url, works) = self.links();
        let names = |wants: &[String]| -> Vec<String> {
            wants
                .iter()
                .filter_map(|w| {
                    self.items
                        .iter()
                        .find(|i| &i.id == w)
                        .map(|i| i.name.clone())
                })
                .collect()
        };
        let requests: Vec<Value> = self
            .m
            .requests
            .iter()
            .map(|r| {
                json!({
                    "request": r.rid,
                    "name": r.name,
                    "wants": r.wants,
                    "wants_names": names(&r.wants),
                    "state": r.state.word(),
                    "bytes_done": r.bytes_done,
                    "bytes_total": r.bytes_total,
                    "why": r.why,
                    "asked_s_ago": now.saturating_duration_since(r.asked_at).as_secs(),
                })
            })
            .collect();
        json!({
            "id": self.id,
            "state": state.word(),
            "from": self.from,
            "title": self.title,
            "title_id": self.title_id,
            "one_use": self.m.one_use,
            "created_ms": self.created_ms,
            "expires_in_s": self.m.expires_at.saturating_duration_since(now).as_secs(),
            "url": url,
            "lan_url": lan_url,
            // "anywhere": the internet link (it works on the same Wi-Fi too); "wifi": the home
            // network's link only (invites work only on the Wi-Fi, or the hub can't be reached).
            "works": works,
            "room": self.room_id(),
            "hub_state": self.room.as_ref().map(|r| r.json("", None, None)["state"].clone()),
            "items": self.items.iter().map(|i| i.listing()).collect::<Vec<_>>(),
            "requests": requests,
        })
    }

    fn offer(&mut self, now: Instant) -> Value {
        json!({
            "from": self.from,
            "title": self.title,
            "title_id": self.title_id,
            "items": self.items.iter().map(|i| i.listing()).collect::<Vec<_>>(),
            "expires_in_s": self.m.expires_at.saturating_duration_since(now).as_secs(),
            "one_use": self.m.one_use,
        })
    }
}

static SHARES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

fn with_shares<R>(f: impl FnOnce(&mut Vec<Entry>) -> R) -> R {
    f(&mut SHARES.lock().unwrap_or_else(|e| e.into_inner()))
}

fn with_share<R>(id: &str, f: impl FnOnce(&mut Entry) -> R) -> Option<R> {
    with_shares(|v| v.iter_mut().find(|e| e.id == id).map(f))
}

/// Records the endings of `id`'s requests (history, activity, a log line) once each, and lets
/// the share's files and hub room go when nothing more can happen.
fn settle(id: &str) {
    /// An ending to record: the activity id (empty: none), the friend, the items' names, the
    /// state, bytes sent, how long it took, why.
    type Ending = (
        String,
        String,
        Vec<String>,
        ReqState,
        u64,
        u64,
        Option<String>,
    );
    let now = Instant::now();
    let mut ended: Vec<Ending> = Vec::new();
    let mut gone_staging = None;
    with_share(id, |e| {
        let items = e.items.clone();
        for r in &e.m.requests {
            if !r.state.ended() || e.recorded.contains(&r.rid) {
                continue;
            }
            e.recorded.push(r.rid);
            let names = r
                .wants
                .iter()
                .filter_map(|w| items.iter().find(|i| &i.id == w).map(|i| i.name.clone()))
                .collect();
            let started = e
                .started
                .iter()
                .find(|(rid, _)| *rid == r.rid)
                .map(|(_, t)| *t);
            if let Some(t) = started {
                ended.push((
                    format!("share-{}-{}", e.id, r.rid),
                    r.name.clone(),
                    names,
                    r.state,
                    r.bytes_done,
                    now_ms().saturating_sub(t),
                    r.why.clone(),
                ));
            } else if r.state == ReqState::Declined {
                ended.push((
                    String::new(),
                    r.name.clone(),
                    names,
                    r.state,
                    0,
                    0,
                    r.why.clone(),
                ));
            }
        }
        if e.m.finished(now) {
            e.room = None;
            gone_staging = e.staging.take();
        }
    });
    for (job, name, names, state, bytes, elapsed, why) in ended {
        let ok = state == ReqState::Done;
        if !job.is_empty() {
            crate::activity::share_ended(
                &job,
                "share",
                &format!("Shared with {name}"),
                ok,
                state == ReqState::Cancelled,
                (!ok).then(|| why.clone().unwrap_or_else(|| state.word().into())),
                bytes,
                elapsed,
            );
        }
        history_add(json!({
            "at_ms": now_ms(),
            "dir": "sent",
            "name": name,
            "items": names,
            "state": state.word(),
        }));
    }
    if let Some(dir) = gone_staging {
        remove_staging(&dir);
    }
}

/// Removes a share's own folder of made files (only below the engine's `shares` folder).
fn remove_staging(dir: &Path) {
    let Some(base) = shares_dir() else { return };
    if dir.starts_with(&base) && dir != base {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// How old a folder in `shares` is before the start sweep takes it: no share lives past
/// [`MAX_TTL`], so one a day older belongs to no engine.
const LEFT_SHARE_AGE: Duration = Duration::from_secs(MAX_TTL.as_secs() + 24 * 3600);

/// Removes what an earlier run left in the engine's `shares` folder: a share is kept in memory
/// only, so after a restart nothing knows the files made for it (a save's zip), and nothing
/// would remove them. By age, not "from before this start": another engine may be running on
/// the same data folder (the app's and one started by hand), and its shares are live.
pub fn sweep_left_shares() {
    let Some(cutoff) = std::time::SystemTime::now().checked_sub(LEFT_SHARE_AGE) else {
        return;
    };
    if let Some(base) = shares_dir() {
        let n = crate::sweep_older_than(&base, cutoff);
        if n > 0 {
            crate::log_at!(
                Info,
                "together",
                "share: removed {n} folder(s) an earlier run left"
            );
        }
    }
}

fn shares_dir() -> Option<PathBuf> {
    if cfg!(test) {
        return Some(std::env::temp_dir().join(format!("ps5u-shares-{}", std::process::id())));
    }
    crate::remote::store::data_dir().map(|d| d.join("shares"))
}

/// The registry the sending sessions use.
pub(crate) struct Registry;

impl wire::Shares for Registry {
    fn find(&self, room: &str) -> Option<String> {
        with_shares(|v| v.iter().find(|e| e.room_id() == room).map(|e| e.id.clone()))
    }
    fn hello(&self, id: &str) -> Result<Value, &'static str> {
        let now = Instant::now();
        with_share(id, |e| {
            e.m.hello(now)?;
            Ok(e.offer(now))
        })
        .unwrap_or(Err("no_share"))
    }
    fn ask(&self, id: &str, name: &str, wants: &[String]) -> Result<u64, &'static str> {
        let r = with_share(id, |e| {
            let rid = e.m.ask(Instant::now(), name, wants)?;
            e.bump();
            let names: Vec<String> = wants
                .iter()
                .filter_map(|w| e.items.iter().find(|i| &i.id == w).map(|i| i.name.clone()))
                .collect();
            Ok((rid, names))
        })
        .unwrap_or(Err("no_share"));
        if let Ok((rid, names)) = &r {
            crate::log_at!(
                Info,
                "together",
                "share {id}: a friend asks for {} item(s)",
                names.len()
            );
            event(
                json!({"share": id, "event": "asked", "request": rid, "name": name, "wants": names}),
            );
        }
        r.map(|x| x.0)
    }
    fn resume(&self, id: &str, token: &str) -> Result<u64, &'static str> {
        let r = with_share(id, |e| e.m.resume(Instant::now(), token)).unwrap_or(Err("no_share"));
        if r.is_ok() {
            crate::log_at!(Info, "together", "share {id}: a friend carries on");
        }
        r
    }
    fn state(&self, id: &str, rid: u64) -> Option<(ReqState, Option<String>)> {
        with_share(id, |e| {
            e.m.tick(Instant::now());
            e.m.request(rid).map(|r| (r.state, r.token.clone()))
        })
        .flatten()
    }
    async fn changed(&self, id: &str) {
        let rx = with_share(id, |e| e.changed.subscribe());
        match rx {
            Some(mut rx) => {
                let _ = tokio::time::timeout(Duration::from_secs(1), rx.changed()).await;
            }
            None => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
    fn items(&self, id: &str, rid: u64) -> Option<(Arc<Vec<Item>>, Vec<String>)> {
        with_share(id, |e| {
            e.m.request(rid).map(|r| (e.items.clone(), r.wants.clone()))
        })
        .flatten()
    }
    fn sending(&self, id: &str, rid: u64, total: u64) -> Result<(), &'static str> {
        let r = with_share(id, |e| {
            e.m.sending(rid, total)?;
            if !e.started.iter().any(|(r, _)| *r == rid) {
                e.started.push((rid, now_ms()));
            }
            e.bump();
            Ok(())
        })
        .unwrap_or(Err("no_share"));
        if r.is_ok() {
            crate::log_at!(Info, "together", "share {id}: sending to a friend");
            event(json!({"share": id, "event": "sending", "request": rid}));
        }
        r
    }
    fn progress(&self, id: &str, rid: u64, done: u64) {
        with_share(id, |e| e.m.progress(rid, done));
    }
    fn done(&self, id: &str, rid: u64) {
        let ok = with_share(id, |e| {
            let r = e.m.done(rid);
            e.bump();
            r.is_ok()
        });
        if ok == Some(true) {
            crate::log_at!(Info, "together", "share {id}: the friend has everything");
            event(json!({"share": id, "event": "done", "request": rid}));
        }
        settle(id);
    }
    fn gone(&self, id: &str, rid: u64, cancelled: bool, why: &str) {
        with_share(id, |e| {
            e.m.gone(rid, cancelled, Some(why));
            e.bump();
        });
        crate::log_at!(
            Info,
            "together",
            "share {id}: the friend's connection ended ({why})"
        );
        event(json!({"share": id, "event": "gone", "request": rid, "why": why}));
        settle(id);
    }
    fn path(&self, id: &str, _rid: u64, path: Option<&'static str>) {
        if let Some(p) = path {
            crate::log_at!(Info, "together", "share {id}: connected over {p}");
        }
    }
}

/// A receiver's data channel (rtc.rs, a room of [`Kind::Share`]).
pub(crate) async fn rtc_session(socket: crate::join_net::RtcSocket, _ip: IpAddr, room: String) {
    wire::sender_session(socket, Arc::new(Registry), Some(room)).await;
}

// ── Making a share ──────────────────────────────────────────────────────────────────────────────

/// One thing to share, as the app's page names it.
#[derive(Debug, Clone, Deserialize)]
pub struct ShareItemSpec {
    pub kind: ItemKind,
    /// The console it is on (an app folder, a save, screenshots, a cheat file).
    #[serde(default, alias = "addr", alias = "host")]
    pub console: Option<String>,
    /// A path: on the console when `console` is given, else on this computer.
    #[serde(default)]
    pub path: Option<String>,
    /// Several (screenshots and clips).
    #[serde(default)]
    pub paths: Vec<String>,
    /// A save folder on the console.
    #[serde(default)]
    pub folder: Option<String>,
    /// What the friend sees (the engine names it when left out).
    #[serde(default)]
    pub name: Option<String>,
    /// Data items: controls, stream settings, which cheats are on.
    #[serde(default)]
    pub data: Option<Value>,
    /// A note's text.
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ShareCreateBody {
    /// The sender's name as the friend sees it.
    #[serde(default)]
    pub from: Option<String>,
    /// The game or app the share is about.
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub title_id: Option<String>,
    /// Seconds the link works (default 24 h; 60 s to 7 days).
    #[serde(default)]
    pub expires_in_s: Option<u64>,
    /// One friend only (default true).
    #[serde(default)]
    pub one_use: Option<bool>,
    pub items: Vec<ShareItemSpec>,
}

/// Why an item can't be shared: the message, and the Title ID a retail refusal names.
#[derive(Debug, Clone)]
pub(crate) struct ItemRefused {
    pub message: String,
    pub retail: Option<String>,
}

impl From<String> for ItemRefused {
    fn from(message: String) -> Self {
        ItemRefused {
            message,
            retail: None,
        }
    }
}

impl From<retail::Refusal> for ItemRefused {
    fn from(r: retail::Refusal) -> Self {
        ItemRefused {
            message: r.message(),
            retail: r.title_id().map(str::to_string).or(Some(String::new())),
        }
    }
}

/// A plain Title ID (letters and digits, 4 to 16): one that may name a folder.
fn plain_title_id(t: &str) -> Option<&str> {
    let t = t.trim();
    Some(t).filter(|t| (4..=16).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// The Title ID whose `/user/appmeta/<Title ID>/` copy stands for an app folder whose own
/// `sce_sys` can't be read: only an installed app's own folder (`/user/app/<ID>`, the same under
/// `/mnt/<drive>/`, or `/system_ex/app/<ID>`), where the folder's name is the console's. Any other
/// folder (one the user named after a homebrew, say) borrows no other app's PARAM: it has none,
/// and is refused.
fn appmeta_title_id(folder: &str) -> Option<&str> {
    let folder = folder.trim_end_matches('/');
    let (parent, id) = folder.rsplit_once('/')?;
    let installed = parent == "/user/app"
        || parent == "/system_ex/app"
        || parent
            .strip_prefix("/mnt/")
            .and_then(|rest| rest.strip_suffix("/user/app"))
            .is_some_and(|drive| !drive.is_empty() && !drive.contains('/'));
    installed.then_some(id).and_then(plain_title_id)
}

/// How an app on a console was built: its `sce_sys/param.json`, else its `param.sfo`, else, for
/// an installed app's own folder, the copy the console keeps in `/user/appmeta/<Title ID>/`
/// (always readable; a retail game's own `sce_sys` is encrypted). Nothing the page sends (a Title
/// ID) picks which PARAM is read.
fn console_build(addr: &str, folder: &str) -> retail::Build {
    let folder = folder.trim_end_matches('/');
    let read = |p: &str| ps5upload_core::fs_ops::fs_read(addr, p, 0, 1 << 20).ok();
    let from = |dir: &str| {
        let b = read(&format!("{dir}/param.json"))
            .map(|b| retail::Build::from_param_json_bytes(&b))
            .unwrap_or_default();
        if b.read {
            return b;
        }
        read(&format!("{dir}/param.sfo"))
            .map(|b| retail::Build::from_sfo_bytes(&b))
            .unwrap_or_default()
    };
    let b = from(&format!("{folder}/sce_sys"));
    if b.read {
        return b;
    }
    match appmeta_title_id(folder) {
        Some(id) => from(&format!("/user/appmeta/{}", id.to_ascii_uppercase())),
        None => b,
    }
}

/// How an app on this computer was built (any format the Collection reads): the PARAM inside
/// the folder, package or image.
fn local_build(path: &Path, format: &str) -> retail::Build {
    if format == "mount.ffpfs" {
        use ps5upload_fpkg::source::SourceTree;
        let Ok(mut tree) = ps5_dump_forge_pfs::PfsSource::open(path) else {
            return retail::Build::default();
        };
        return tree
            .read("sce_sys/param.json")
            .map(|b| retail::Build::from_param_json_bytes(&b))
            .unwrap_or_default();
    }
    match crate::inspect::inspect_local(path) {
        Ok(g) => retail::Build::from_inspection(&g),
        Err(_) => retail::Build::default(),
    }
}

/// The rule's verdict on a build, as Share reports it.
pub(crate) fn check_app(build: &retail::Build) -> Result<(), ItemRefused> {
    retail::check(build).map_err(ItemRefused::from)
}

fn inline(path: &str, bytes: Vec<u8>) -> Result<FileEntry, ItemRefused> {
    if bytes.len() > manifest::MAX_INLINE {
        return Err("too much data in one item".to_string().into());
    }
    Ok(FileEntry {
        path: path.to_string(),
        size: bytes.len() as u64,
        loc: Loc::Inline(Arc::new(bytes)),
    })
}

/// Builds one item from what the page sent: reads the app's identity itself and refuses a
/// retail one; lists the files. Blocking (it may read the console).
pub(crate) fn build_item(
    n: usize,
    spec: &ShareItemSpec,
    title: &str,
    title_id: Option<&str>,
    staging: &Path,
) -> Result<Item, ItemRefused> {
    let id = format!("i{n}");
    let console = spec
        .console
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    let label = |what: &str| {
        spec.name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                if title.is_empty() {
                    what.to_string()
                } else {
                    format!("{title} {what}")
                }
            })
    };
    let mut item = Item {
        id,
        kind: spec.kind,
        name: String::new(),
        title_id: title_id.map(str::to_string),
        format: None,
        root: None,
        files: Vec::new(),
        links_skipped: 0,
        build: None,
    };
    match spec.kind {
        ItemKind::App => {
            // The app's own PARAM decides, never the Title ID the page sent.
            let path = spec
                .path
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .ok_or_else(|| ItemRefused::from("name the app's folder or file".to_string()))?;
            if let Some(addr) = console {
                let build = console_build(addr, path);
                check_app(&build)?;
                let (files, links) = manifest::walk_console(addr, path)?;
                item.title_id = Some(build.title_id.clone()).filter(|t| !t.is_empty());
                item.build = Some(build);
                item.format = Some("folder".into());
                item.files = files;
                item.links_skipped = links;
            } else {
                let p = PathBuf::from(path);
                let format = manifest::local_format(&p).ok_or_else(|| {
                    ItemRefused::from(
                        "Share an app as a folder, a package or a mount image; an archive can't \
                         be checked."
                            .to_string(),
                    )
                })?;
                let build = local_build(&p, format);
                check_app(&build)?;
                item.title_id = Some(build.title_id.clone()).filter(|t| !t.is_empty());
                item.build = Some(build);
                item.format = Some(format.into());
                if format == "folder" {
                    let (files, links) = manifest::walk_local(&p)?;
                    item.root = Some(p);
                    item.files = files;
                    item.links_skipped = links;
                } else {
                    item.files = vec![manifest::local_file(&p)?];
                    item.root = p.parent().map(Path::to_path_buf);
                }
            }
            item.name = spec
                .name
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| {
                    if title.is_empty() {
                        item.title_id.clone().unwrap_or_else(|| "App".into())
                    } else {
                        title.to_string()
                    }
                });
        }
        ItemKind::Save => {
            item.name = label("save");
            if let Some(addr) = console {
                let folder = spec
                    .folder
                    .as_deref()
                    .or(spec.path.as_deref())
                    .ok_or_else(|| ItemRefused::from("name the save".to_string()))?;
                let shared =
                    crate::save_copy::share_file(&crate::save_copy::Live::new(addr), folder)?;
                std::fs::create_dir_all(staging)
                    .map_err(|e| format!("{}: {e}", staging.display()))?;
                let dest = staging.join(wire::safe_name(&shared.name));
                std::fs::copy(&shared.zip, &dest).map_err(|e| format!("copy: {e}"))?;
                item.files = vec![manifest::local_file(&dest)?];
                item.root = Some(staging.to_path_buf());
            } else {
                // A shared-save file already on this computer.
                let p = PathBuf::from(
                    spec.path
                        .as_deref()
                        .ok_or_else(|| ItemRefused::from("name the save file".to_string()))?,
                );
                item.files = vec![manifest::local_file(&p)?];
                item.root = p.parent().map(Path::to_path_buf);
            }
        }
        ItemKind::Media => {
            item.name = label("screenshots and clips");
            let mut paths: Vec<&str> = spec.paths.iter().map(String::as_str).collect();
            paths.extend(spec.path.as_deref());
            if paths.is_empty() {
                return Err("choose the screenshots or clips".to_string().into());
            }
            if let Some(addr) = console {
                for p in paths.iter().take(500) {
                    let mut f = manifest::console_file(addr, p)?;
                    f.path = unique(&item.files, &f.path);
                    item.files.push(f);
                }
            } else {
                // Local files: each from its own folder, so each is checked on its own root.
                if paths.len() != 1 {
                    return Err("share local screenshots one folder at a time"
                        .to_string()
                        .into());
                }
                let p = PathBuf::from(paths[0]);
                if p.is_dir() {
                    let (files, links) = manifest::walk_local(&p)?;
                    item.files = files;
                    item.links_skipped = links;
                    item.root = Some(p);
                } else {
                    item.files = vec![manifest::local_file(&p)?];
                    item.root = p.parent().map(Path::to_path_buf);
                }
            }
        }
        ItemKind::Cheats => {
            item.name = label("cheats");
            if let (Some(addr), Some(p)) = (console, spec.path.as_deref()) {
                item.files.push(manifest::console_file(addr, p)?);
            }
            if let Some(d) = &spec.data {
                item.files
                    .push(inline("cheats-on.json", d.to_string().into_bytes())?);
            }
            if item.files.is_empty() {
                return Err("no cheat file to share".to_string().into());
            }
        }
        ItemKind::Controls | ItemKind::Stream => {
            item.name = label(if spec.kind == ItemKind::Controls {
                "controls"
            } else {
                "stream settings"
            });
            let d = spec
                .data
                .as_ref()
                .ok_or_else(|| ItemRefused::from("nothing to share".to_string()))?;
            let file = if spec.kind == ItemKind::Controls {
                "controls.json"
            } else {
                "stream.json"
            };
            item.files.push(inline(file, d.to_string().into_bytes())?);
        }
        ItemKind::Note => {
            item.name = spec.name.clone().unwrap_or_else(|| "Note".into());
            let t: String = spec
                .text
                .as_deref()
                .unwrap_or("")
                .trim()
                .chars()
                .take(NOTE_MAX)
                .collect();
            if t.is_empty() {
                return Err("the note is empty".to_string().into());
            }
            item.files.push(inline("note.txt", t.into_bytes())?);
        }
    }
    item.name = item.name.chars().take(120).collect();
    Ok(item)
}

/// `name`, or `name (2)`… when another file has it.
fn unique(files: &[FileEntry], name: &str) -> String {
    let mut n = name.to_string();
    let mut k = 2;
    while files.iter().any(|f| f.path == n) {
        n = format!("{k}-{name}");
        k += 1;
    }
    n
}

fn refused_json(i: usize, r: &ItemRefused) -> Value {
    json!({
        "item": i,
        "error": r.message,
        "code": if r.retail.is_some() { "retail" } else { "item" },
        "title_id": r.retail.clone().filter(|s| !s.is_empty()),
    })
}

/// Makes the share's items, or the first refusal.
fn build_all(b: &ShareCreateBody, staging: &Path) -> Result<Vec<Item>, (usize, ItemRefused)> {
    let title = b.title.as_deref().unwrap_or("").trim().to_string();
    let title_id = b
        .title_id
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let mut out = Vec::new();
    for (i, spec) in b.items.iter().enumerate().take(20) {
        out.push(build_item(i + 1, spec, &title, title_id, staging).map_err(|r| (i, r))?);
    }
    Ok(out)
}

/// The home network's page of this engine, for the link's hint.
fn lan_page(headers: &HeaderMap) -> Option<String> {
    let port = crate::pad_link::lan_port(headers);
    let ip = crate::pad_link::reach_ips(None).0.into_iter().next()?;
    Some(format!("{}/share", crate::pad_link::ip_base(ip, port)))
}

/// Puts a made share in the registry and opens its room. The share's view.
pub(crate) fn register(
    b: &ShareCreateBody,
    items: Vec<Item>,
    staging: Option<PathBuf>,
    lan: Option<String>,
) -> Result<Value, &'static str> {
    let id = crate::join_net::random_b64(9).ok_or("no_random")?;
    let lan_room = crate::join_net::random_b64(16).ok_or("no_random")?;
    let ttl = b
        .expires_in_s
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TTL)
        .clamp(MIN_TTL, MAX_TTL);
    let now = Instant::now();
    let item_ids = items.iter().map(|i| i.id.clone()).collect();
    let settings = crate::join::settings();
    let room = if settings.internet_invites {
        let sid = id.clone();
        crate::join_net::hub::Room::start(
            Kind::Share,
            Box::new(move || {
                with_share(&sid, |e| {
                    matches!(
                        e.m.tick(Instant::now()),
                        ShareState::Open | ShareState::Used
                    )
                })
                // Not in the registry yet (the room starts first): wanted. Once the share is
                // gone, its room went with it.
                .unwrap_or(true)
            }),
            settings.hub_url.clone(),
        )
    } else {
        None
    };
    let from: String = b
        .from
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    let mut e = Entry {
        id: id.clone(),
        from: if from.is_empty() {
            "A friend".into()
        } else {
            from
        },
        title: b
            .title
            .clone()
            .unwrap_or_default()
            .chars()
            .take(120)
            .collect(),
        title_id: b.title_id.clone(),
        created_ms: now_ms(),
        items: Arc::new(items),
        m: Machine::new(now, ttl, b.one_use.unwrap_or(true), item_ids),
        room,
        lan_room,
        lan,
        staging,
        changed: watch::channel(0).0,
        started: Vec::new(),
        recorded: Vec::new(),
    };
    let view = e.json(now);
    let dropped = with_shares(|v| {
        let mut dropped = Vec::new();
        while v.len() >= MAX_SHARES {
            let i = v.iter_mut().position(|x| x.m.finished(now)).unwrap_or(0);
            let mut x = v.remove(i);
            x.m.cancel();
            dropped.extend(x.staging.take());
        }
        v.push(e);
        dropped
    });
    for d in dropped {
        remove_staging(&d);
    }
    // At its time, the link stops (one task, not a timer that ticks).
    let sid = id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(ttl).await;
        with_share(&sid, |e| {
            e.m.tick(Instant::now());
            e.bump();
        });
        settle(&sid);
    });
    crate::log_at!(
        Info,
        "together",
        "share {id}: made ({} item(s), {} h, one use: {})",
        view["items"].as_array().map_or(0, |a| a.len()),
        ttl.as_secs() / 3600,
        b.one_use.unwrap_or(true)
    );
    event(json!({"share": id, "event": "made"}));
    Ok(view)
}

/// `POST /api/share`: makes a share and its link. 403 with `code: "retail"` when an app is a
/// commercial game.
pub async fn create(headers: HeaderMap, Json(b): Json<ShareCreateBody>) -> Response {
    if b.items.is_empty() {
        return bad(StatusCode::BAD_REQUEST, "choose something to share");
    }
    let Some(base) = shares_dir() else {
        return bad(StatusCode::INTERNAL_SERVER_ERROR, "no data folder");
    };
    let Some(sub) = crate::join_net::random_b64(9) else {
        return bad(StatusCode::INTERNAL_SERVER_ERROR, "no_random");
    };
    let staging = base.join(sub);
    let lan = lan_page(&headers);
    let st = staging.clone();
    let built = tokio::task::spawn_blocking(move || {
        let r = build_all(&b, &st);
        (b, r)
    })
    .await;
    let (b, built) = match built {
        Ok(x) => x,
        Err(e) => return bad(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let items = match built {
        Ok(items) => items,
        Err((i, r)) => {
            remove_staging(&staging);
            if r.retail.is_some() {
                crate::log_at!(Info, "together", "share: refused a retail app");
            }
            let code = if r.retail.is_some() {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::BAD_REQUEST
            };
            return (code, Json(refused_json(i, &r))).into_response();
        }
    };
    let staging = staging.exists().then_some(staging);
    match register(&b, items, staging, lan) {
        Ok(v) => Json(v).into_response(),
        Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// `POST /api/share/check {title_id?, items}`: each item's verdict, nothing made. An app's
/// identity is read as Share would (`ok: false, code: "retail"` for a commercial game).
pub async fn check(Json(b): Json<ShareCreateBody>) -> Response {
    let r = tokio::task::spawn_blocking(move || {
        // The page's title_id is not read here: an app's own PARAM decides (console_build).
        b.items
            .iter()
            .enumerate()
            .take(20)
            .map(|(i, spec)| {
                if spec.kind != ItemKind::App {
                    return json!({"item": i, "ok": true});
                }
                let verdict = match spec
                    .path
                    .as_deref()
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                {
                    Some(p) => match spec.console.as_deref() {
                        Some(addr) => check_app(&console_build(addr, p)),
                        None => {
                            let pb = PathBuf::from(p);
                            match manifest::local_format(&pb) {
                                Some(f) => check_app(&local_build(&pb, f)),
                                None => Err(ItemRefused::from(
                                    "Share an app as a folder, a package or a mount image."
                                        .to_string(),
                                )),
                            }
                        }
                    },
                    // Nothing to read: nothing shows it is homebrew.
                    None => check_app(&retail::Build::default()),
                };
                match verdict {
                    Ok(()) => json!({"item": i, "ok": true}),
                    Err(r) => {
                        let mut v = refused_json(i, &r);
                        v["ok"] = json!(false);
                        v
                    }
                }
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    Json(json!({ "items": r })).into_response()
}

#[derive(Deserialize)]
pub struct ShareDecideBody {
    pub request: u64,
    pub allow: bool,
}

/// `POST /api/share/{id}/decide {request, allow}`: the sender's Allow or Decline.
pub async fn decide(AxPath(id): AxPath<String>, Json(b): Json<ShareDecideBody>) -> Response {
    let token = crate::join_net::random_b64(24).unwrap_or_default();
    if token.is_empty() {
        return bad(StatusCode::INTERNAL_SERVER_ERROR, "no_random");
    }
    let r = with_share(&id, |e| {
        let r = e.m.decide(Instant::now(), b.request, b.allow, token);
        e.bump();
        r.map(|s| (s, e.json(Instant::now())))
    });
    match r {
        None => bad(StatusCode::NOT_FOUND, "no such share"),
        Some(Err(why)) => bad(StatusCode::CONFLICT, why),
        Some(Ok((s, view))) => {
            crate::log_at!(
                Info,
                "together",
                "share {id}: {}",
                if b.allow { "allowed" } else { "declined" }
            );
            event(json!({"share": id, "event": s.word(), "request": b.request}));
            settle(&id);
            Json(view).into_response()
        }
    }
}

/// `DELETE /api/share/{id}`: cancels the share; its link stops and every transfer ends.
pub async fn cancel(AxPath(id): AxPath<String>) -> Response {
    let r = with_share(&id, |e| {
        e.m.cancel();
        e.bump();
        e.json(Instant::now())
    });
    match r {
        None => bad(StatusCode::NOT_FOUND, "no such share"),
        Some(view) => {
            crate::log_at!(Info, "together", "share {id}: cancelled");
            event(json!({"share": id, "event": "cancelled"}));
            settle(&id);
            Json(view).into_response()
        }
    }
}

/// `GET /api/share/{id}`.
pub async fn get_one(AxPath(id): AxPath<String>) -> Response {
    match with_share(&id, |e| e.json(Instant::now())) {
        Some(v) => Json(v).into_response(),
        None => bad(StatusCode::NOT_FOUND, "no such share"),
    }
}

/// `GET /api/share`: the shares, the receives, and the history.
pub async fn list() -> Response {
    let now = Instant::now();
    let shares: Vec<Value> = with_shares(|v| v.iter_mut().map(|e| e.json(now)).collect());
    let receives: Vec<Value> = with_rx(|v| v.iter().map(Rx::json).collect());
    Json(json!({
        "shares": shares,
        "receives": receives,
        "history": history_read(),
    }))
    .into_response()
}

/// `GET /api/share/ws`: a receiver on the home network (reached from the LAN; the session checks
/// the room itself, and the sender still allows).
pub async fn ws(
    upgrade: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
) -> Response {
    if !crate::join::connect_allowed(peer.ip(), Instant::now()) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "too many tries, wait a minute",
        )
            .into_response();
    }
    upgrade
        .max_message_size(wire::TEXT_MAX)
        .max_frame_size(wire::TEXT_MAX)
        .on_upgrade(move |s| wire::sender_session(s, Arc::new(Registry), None))
}

/// `GET /share`: the page a browser on the home network opens a link with (downloads only).
pub async fn page() -> impl IntoResponse {
    Html(include_str!("share_page.html"))
}

// ── History ─────────────────────────────────────────────────────────────────────────────────────

fn history_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    crate::remote::store::data_dir().map(|d| d.join("share-history.json"))
}

static HISTORY_MEM: Mutex<Vec<Value>> = Mutex::new(Vec::new());

fn history_read() -> Vec<Value> {
    match history_path() {
        Some(p) => std::fs::read(&p)
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<Value>>(&b).ok())
            .unwrap_or_default(),
        None => HISTORY_MEM
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone(),
    }
}

fn history_add(v: Value) {
    static LOCK: Mutex<()> = Mutex::new(());
    let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut h = history_read();
    h.insert(0, v);
    h.truncate(HISTORY_MAX);
    match history_path() {
        Some(p) => {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, serde_json::to_vec(&h).unwrap_or_default()).is_ok() {
                let _ = std::fs::rename(&tmp, &p);
            }
        }
        None => *HISTORY_MEM.lock().unwrap_or_else(|e| e.into_inner()) = h,
    }
}

// ── The receiving side ──────────────────────────────────────────────────────────────────────────

struct Rx {
    id: String,
    sh: Arc<Mutex<RxShared>>,
    cmd: mpsc::UnboundedSender<RxCmd>,
    started_ms: u64,
    recorded: bool,
}

impl Rx {
    fn json(&self) -> Value {
        let s = self.sh.lock().unwrap_or_else(|e| e.into_inner());
        json!({
            "id": self.id,
            "state": s.state,
            "from": s.from,
            "title": s.title,
            "title_id": s.title_id,
            "items": s.items,
            "accepted": s.accepted,
            "expires_in_s": s.expires_in_s,
            "bytes_done": s.bytes_done,
            "bytes_total": s.bytes_total,
            "error": s.error,
            "path": s.path,
            "dest": s.dest,
            "received": s.received.iter().map(|(id, p)| {
                let item = s.items.iter().find(|i| &i.id == id);
                // An app that is one file (a package, an image): that file, for an install.
                let file = (s.state == "done"
                    && item.is_some_and(|i| i.kind == "app" && i.format.as_deref() != Some("folder")))
                .then(|| {
                    std::fs::read_dir(p).ok().and_then(|rd| {
                        let files: Vec<PathBuf> = rd
                            .filter_map(Result::ok)
                            .map(|e| e.path())
                            .filter(|f| std::fs::symlink_metadata(f).is_ok_and(|m| m.is_file()))
                            .collect();
                        (files.len() == 1).then(|| files[0].clone())
                    })
                })
                .flatten();
                json!({
                    "item": id,
                    "kind": item.map(|i| i.kind.clone()),
                    "format": item.and_then(|i| i.format.clone()),
                    "title_id": item.and_then(|i| i.title_id.clone()),
                    "path": p,
                    "file": file,
                })
            }).collect::<Vec<_>>(),
            "started_ms": self.started_ms,
        })
    }
}

static RECEIVES: Mutex<Vec<Rx>> = Mutex::new(Vec::new());

fn with_rx<R>(f: impl FnOnce(&mut Vec<Rx>) -> R) -> R {
    f(&mut RECEIVES.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Where received things go unless the person chose: the Collection's first folder (so an app
/// shows there), else the engine's own `received` folder.
fn default_dest() -> Option<PathBuf> {
    let base = crate::collection::store::dir()
        .map(|d| crate::collection::store::load_settings_at(&d))
        .and_then(|s| s.roots.into_iter().next())
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .map(|p| p.join("Received"))
        .or_else(|| crate::remote::store::data_dir().map(|d| d.join("received")))?;
    Some(base)
}

/// Opens a link: connects (the hub and a direct channel, or the home network's socket) and
/// follows the transfer to its end, connecting again with the token when it drops mid-way.
async fn run_receive(
    link: client::ShareLink,
    name: String,
    sh: Arc<Mutex<RxShared>>,
    mut cmds: mpsc::UnboundedReceiver<RxCmd>,
    rx_id: String,
) {
    let mut tries = 0u32;
    loop {
        let end = match connect_any(&link).await {
            Ok(Conn::Rtc(l)) => wire::receive(l, &link.room, &name, sh.clone(), &mut cmds).await,
            Ok(Conn::Lan(l)) => wire::receive(*l, &link.room, &name, sh.clone(), &mut cmds).await,
            Err(e) => {
                let resumable = sh.lock().unwrap_or_else(|e| e.into_inner()).token.is_some();
                if resumable && tries < RECONNECTS {
                    RxEnd::Lost
                } else {
                    let mut s = sh.lock().unwrap_or_else(|e| e.into_inner());
                    s.state = "failed";
                    s.error = Some(e);
                    RxEnd::Over
                }
            }
        };
        match end {
            RxEnd::Lost if tries < RECONNECTS => {
                tries += 1;
                {
                    let mut s = sh.lock().unwrap_or_else(|e| e.into_inner());
                    if s.token.is_none() {
                        s.state = "failed";
                        s.error = Some("The connection to the sender was lost.".into());
                        break;
                    }
                    s.state = "reconnecting";
                }
                crate::log_at!(Info, "together", "share: receive {rx_id}: connecting again");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(2 * tries as u64)) => {}
                    c = cmds.recv() => if matches!(c, Some(RxCmd::Cancel) | None) {
                        let mut s = sh.lock().unwrap_or_else(|e| e.into_inner());
                        s.state = "cancelled";
                        break;
                    }
                }
            }
            RxEnd::Lost => {
                let mut s = sh.lock().unwrap_or_else(|e| e.into_inner());
                s.state = "failed";
                s.error = Some("The connection to the sender was lost.".into());
                break;
            }
            RxEnd::Done | RxEnd::Over => break,
        }
    }
    rx_finished(&rx_id);
}

enum Conn {
    Rtc(client::RtcLink),
    Lan(Box<client::WsLink>),
}

async fn connect_any(link: &client::ShareLink) -> Result<Conn, String> {
    let mut first_err = None;
    if link.hub.is_some() && link.fp.is_some() {
        match client::connect_rtc(link).await {
            Ok(l) => return Ok(Conn::Rtc(l)),
            Err(e) => first_err = Some(e),
        }
    }
    if let Some(lan) = &link.lan {
        match client::connect_lan(lan).await {
            Ok(l) => return Ok(Conn::Lan(Box::new(l))),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    Err(first_err.unwrap_or_else(|| "The link can't be opened.".into()))
}

/// A receive ended: its history entry, its activity ending and a note beside what came in.
fn rx_finished(rx_id: &str) {
    let info = with_rx(|v| {
        v.iter_mut().find(|r| r.id == rx_id).and_then(|r| {
            if r.recorded {
                return None;
            }
            r.recorded = true;
            Some((r.json(), r.started_ms))
        })
    });
    let Some((v, started)) = info else { return };
    let state = v["state"].as_str().unwrap_or("failed").to_string();
    let from = v["from"].as_str().unwrap_or("").to_string();
    let names: Vec<String> = v["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|i| {
                    v["accepted"]
                        .as_array()
                        .is_some_and(|acc| acc.contains(&i["id"]))
                })
                .filter_map(|i| i["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if state == "done" {
        if let Some(dest) = v["dest"].as_str() {
            let _ = std::fs::write(
                Path::new(dest).join("share.json"),
                serde_json::to_vec_pretty(&json!({
                    "from": from,
                    "title": v["title"],
                    "title_id": v["title_id"],
                    "items": v["items"],
                    "received": v["received"],
                    "at_ms": now_ms(),
                }))
                .unwrap_or_default(),
            );
        }
    }
    if matches!(state.as_str(), "done" | "failed" | "cancelled")
        && v["bytes_total"].as_u64().unwrap_or(0) > 0
    {
        crate::activity::share_ended(
            &format!("receive-{rx_id}"),
            "receive",
            &format!("Received from {from}"),
            state == "done",
            state == "cancelled",
            (state != "done").then(|| v["error"].as_str().unwrap_or(&state).to_string()),
            v["bytes_done"].as_u64().unwrap_or(0),
            now_ms().saturating_sub(started),
        );
    }
    if !from.is_empty() {
        history_add(json!({
            "at_ms": now_ms(),
            "dir": "received",
            "name": from,
            "items": names,
            "state": state,
        }));
    }
    crate::log_at!(Info, "together", "share: receive {rx_id} ended ({state})");
    event(json!({"receive": rx_id, "event": state}));
}

#[derive(Deserialize)]
pub struct ShareOpenBody {
    pub link: String,
    /// This side's name as the sender sees it.
    #[serde(default)]
    pub name: Option<String>,
}

/// `POST /api/share/receive {link, name}`: opens a Share link. Nothing is received until
/// `accept`; the answer has the receive's id (GET it for the offer).
pub async fn receive_open(Json(b): Json<ShareOpenBody>) -> Response {
    let link = match client::parse_link(&b.link) {
        Ok(l) => l,
        Err(e) => return bad(StatusCode::BAD_REQUEST, e),
    };
    let Some(id) = crate::join_net::random_b64(9) else {
        return bad(StatusCode::INTERNAL_SERVER_ERROR, "no_random");
    };
    let sh = Arc::new(Mutex::new(RxShared {
        state: "connecting",
        ..RxShared::default()
    }));
    let (tx, rx) = mpsc::unbounded_channel();
    let busy = with_rx(|v| {
        if v.len() >= MAX_RECEIVES {
            if let Some(i) = v.iter().position(|r| r.recorded) {
                v.remove(i);
            } else {
                return true;
            }
        }
        v.push(Rx {
            id: id.clone(),
            sh: sh.clone(),
            cmd: tx,
            started_ms: now_ms(),
            recorded: false,
        });
        false
    });
    if busy {
        return bad(StatusCode::TOO_MANY_REQUESTS, "too many receives at once");
    }
    let name: String = b
        .name
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    crate::log_at!(Info, "together", "share: opening a link (receive {id})");
    tokio::spawn(run_receive(link, name, sh, rx, id.clone()));
    Json(json!({"id": id, "state": "connecting"})).into_response()
}

/// `GET /api/share/receive/{id}`.
pub async fn receive_get(AxPath(id): AxPath<String>) -> Response {
    match with_rx(|v| v.iter().find(|r| r.id == id).map(Rx::json)) {
        Some(v) => Json(v).into_response(),
        None => bad(StatusCode::NOT_FOUND, "no such receive"),
    }
}

#[derive(Deserialize)]
pub struct ShareAcceptBody {
    pub items: Vec<String>,
    /// The folder to put things in (default: the Collection's first folder's `Received`).
    #[serde(default)]
    pub dest: Option<String>,
}

/// `POST /api/share/receive/{id}/accept {items, dest?}`: what to take; the sender is then asked.
pub async fn receive_accept(
    AxPath(id): AxPath<String>,
    Json(b): Json<ShareAcceptBody>,
) -> Response {
    let base = match b.dest.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => {
            let p = PathBuf::from(d);
            if !p.is_absolute() {
                return bad(StatusCode::BAD_REQUEST, "the folder must be a full path");
            }
            p
        }
        None => match default_dest() {
            Some(p) => p,
            None => {
                return bad(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "no folder to receive into",
                )
            }
        },
    };
    let r = with_rx(|v| {
        v.iter().find(|r| r.id == id).map(|r| {
            let s = r.sh.lock().unwrap_or_else(|e| e.into_inner());
            (s.state, s.from.clone(), s.title.clone(), r.cmd.clone())
        })
    });
    let Some((state, from, title, cmd)) = r else {
        return bad(StatusCode::NOT_FOUND, "no such receive");
    };
    if state != "offered" {
        return bad(StatusCode::CONFLICT, format!("this receive is {state}"));
    }
    // Its own folder: "<title> from <name> <date>".
    let stamp = chrono_like(now_ms());
    let label = if title.is_empty() {
        format!("From {from} {stamp}")
    } else {
        format!("{title} from {from} {stamp}")
    };
    let mut dest = base.join(wire::safe_name(&label));
    let mut k = 2;
    while dest.exists() {
        dest = base.join(format!("{} ({k})", wire::safe_name(&label)));
        k += 1;
    }
    if cmd
        .send(RxCmd::Accept {
            items: b.items,
            dest,
        })
        .is_err()
    {
        return bad(StatusCode::CONFLICT, "the connection ended");
    }
    Json(json!({"ok": true})).into_response()
}

/// `YYYY-MM-DD HHMM` (UTC) of a time in ms.
fn chrono_like(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let iso = crate::collection::iso_utc(secs);
    // "2026-11-09T14:05:00Z" → "2026-11-09 1405"
    let date = iso.get(..10).unwrap_or("");
    let time: String = iso
        .get(11..16)
        .unwrap_or("")
        .chars()
        .filter(|c| *c != ':')
        .collect();
    format!("{date} {time}").trim().to_string()
}

/// `DELETE /api/share/receive/{id}`: stops a receive (a file half-received stays as `.part`).
pub async fn receive_cancel(AxPath(id): AxPath<String>) -> Response {
    let r = with_rx(|v| v.iter().find(|r| r.id == id).map(|r| r.cmd.clone()));
    match r {
        Some(cmd) => {
            let _ = cmd.send(RxCmd::Cancel);
            Json(json!({"ok": true})).into_response()
        }
        None => bad(StatusCode::NOT_FOUND, "no such receive"),
    }
}

/// The local folder a received item is in, once the receive is done.
#[allow(clippy::result_large_err)]
fn received_dir(rx_id: &str, item: &str) -> Result<(PathBuf, String), Response> {
    let r = with_rx(|v| {
        v.iter().find(|r| r.id == rx_id).map(|r| {
            let s = r.sh.lock().unwrap_or_else(|e| e.into_inner());
            let kind = s
                .items
                .iter()
                .find(|i| i.id == item)
                .map(|i| i.kind.clone());
            (
                s.state,
                s.received
                    .iter()
                    .find(|(id, _)| id == item)
                    .map(|(_, p)| p.clone()),
                kind,
            )
        })
    });
    match r {
        None => Err(bad(StatusCode::NOT_FOUND, "no such receive")),
        Some((state, _, _)) if state != "done" => {
            Err(bad(StatusCode::CONFLICT, "not received yet"))
        }
        Some((_, Some(p), Some(kind))) => Ok((p, kind)),
        _ => Err(bad(StatusCode::NOT_FOUND, "no such item")),
    }
}

/// `GET /api/share/receive/{id}/data/{item}`: a received data item (controls, stream settings,
/// which cheats are on, a note) as JSON: `{kind, files: {name: content}}`.
pub async fn receive_data(AxPath((id, item)): AxPath<(String, String)>) -> Response {
    let (dir, kind) = match received_dir(&id, &item) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let mut files = serde_json::Map::new();
    for name in ["controls.json", "stream.json", "cheats-on.json", "note.txt"] {
        let p = dir.join(name);
        if std::fs::symlink_metadata(&p)
            .is_ok_and(|m| m.is_file() && m.len() <= manifest::MAX_INLINE as u64)
        {
            if let Ok(t) = std::fs::read_to_string(&p) {
                let v = if name.ends_with(".json") {
                    serde_json::from_str(&t).unwrap_or(Value::String(t))
                } else {
                    Value::String(t)
                };
                files.insert(name.to_string(), v);
            }
        }
    }
    Json(json!({"kind": kind, "path": dir, "files": files})).into_response()
}

#[derive(Deserialize)]
pub struct ShareUseSaveBody {
    #[serde(alias = "console", alias = "host")]
    pub addr: String,
    pub folder: String,
    #[serde(default)]
    pub user_id: Option<u32>,
    pub item: String,
}

/// `POST /api/share/receive/{id}/use-save {addr, folder, user_id?, item}`: a received save into
/// a user's own save of the same game (as Use a shared save does: save_copy.rs). The page warns
/// first that trophies and the account stay the user's own.
pub async fn receive_use_save(
    AxPath(id): AxPath<String>,
    Json(b): Json<ShareUseSaveBody>,
) -> Response {
    let (dir, kind) = match received_dir(&id, &b.item) {
        Ok(x) => x,
        Err(r) => return r,
    };
    if kind != "save" {
        return bad(StatusCode::BAD_REQUEST, "that item is not a save");
    }
    let zip = std::fs::read_dir(&dir).ok().and_then(|rd| {
        rd.filter_map(Result::ok).map(|e| e.path()).find(|p| {
            p.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip"))
                && std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file())
        })
    });
    let Some(zip) = zip else {
        return bad(StatusCode::NOT_FOUND, "the save file is not there");
    };
    let q = crate::save_copy::UseSharedQuery {
        addr: b.addr,
        folder: Some(b.folder),
        image: None,
        user_id: b.user_id,
    };
    match tokio::task::spawn_blocking(move || crate::save_copy::use_shared_file(&q, &zip)).await {
        Ok(Ok(parts)) => {
            crate::log_at!(Info, "together", "share: a received save was used");
            Json(json!({"ok": true, "parts": parts})).into_response()
        }
        Ok(Err(f)) => bad(StatusCode::CONFLICT, f.msg),
        Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── The activity ────────────────────────────────────────────────────────────────────────────────

/// Transfers going now, both ways, for `GET /api/activity`.
pub(crate) fn activity() -> Vec<crate::activity::Active> {
    let mut out = Vec::new();
    with_shares(|v| {
        for e in v.iter() {
            for r in e.m.requests.iter().filter(|r| r.state == ReqState::Sending) {
                let started = e
                    .started
                    .iter()
                    .find(|(x, _)| *x == r.rid)
                    .map_or(0, |x| x.1);
                out.push(crate::activity::Active {
                    id: format!("share-{}-{}", e.id, r.rid),
                    kind: "share".into(),
                    label: format!("Sharing with {}", r.name),
                    bytes_done: r.bytes_done,
                    bytes_total: r.bytes_total,
                    started_at_ms: started,
                });
            }
        }
    });
    with_rx(|v| {
        for r in v.iter() {
            let s = r.sh.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(s.state, "receiving" | "reconnecting") {
                out.push(crate::activity::Active {
                    id: format!("receive-{}", r.id),
                    kind: "receive".into(),
                    label: format!("Receiving from {}", s.from),
                    bytes_done: s.bytes_done,
                    bytes_total: s.bytes_total,
                    started_at_ms: r.started_ms,
                });
            }
        }
    });
    out
}
