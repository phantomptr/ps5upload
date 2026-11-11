//! What the two sides of a share say to each other, over a WebRTC data channel labelled
//! `"share"` (the hub only introduced them) or, on the home network, the sender's
//! `/api/share/ws`. Text messages are JSON; the file data goes as binary messages.
//!
//! Receiver → sender:
//! * `{"hello": {"v": 1, "room", "name", "resume"?}}` first. `resume` is the token an earlier
//!   Allow gave: the transfer carries on without asking again.
//! * `{"accept": {"items": [id, …]}}`: what they want of the offer.
//! * `{"get": {"n", "item", "path", "offset"}}`: one file of an accepted item, from `offset`.
//! * `{"done": {}}` when they have everything; `{"cancel": {}}` to stop.
//!
//! Sender → receiver:
//! * `{"offer": {"from", "title", "title_id", "items": [{id, kind, name, title_id, format, size,
//!   files, build?: {read, marks}}], "expires_in_s", "one_use"}}` (`build`: an app's, retail.rs), or `{"refused": {"why"}}` (`expired`, `used`,
//!   `cancelled`, `busy`, `no_share`).
//! * `{"asked": {}}` once the sender has been told; then `{"allowed": {"token", "files": [{item,
//!   path, size}]}}` or `{"declined": {"why"?}}`.
//! * For a `get`: binary messages `[n: u32 LE][offset: u64 LE][bytes]`, then `{"eof": {"n",
//!   "size"}}`; or `{"error": {"n", "why"}}` (`not_in_manifest`, `unsafe_path`, `symlink`,
//!   `changed`, `read`).
//! * `{"cancelled": {}}` when the sender cancels.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::Message;
use futures_util::FutureExt;
use serde_json::{json, Value};

use super::machine::ReqState;
use super::manifest::{self, Item};
use crate::join::GuestSocket;

/// The bytes of a file in one binary message.
pub(crate) const PIECE: usize = 128 * 1024;
/// The binary message header: `n` and `offset`.
pub(crate) const HEADER: usize = 12;
/// The first message must come this soon.
const HELLO_WAIT: Duration = Duration::from_secs(15);
/// A receiver that stopped asking for files for this long has gone.
const IDLE_WAIT: Duration = Duration::from_secs(120);
/// A sender that said nothing for this long while a file was coming, or before its offer, has
/// gone: the receive ends as a lost connection (and connects again with its token, if it has one).
pub(crate) const RX_IDLE_WAIT: Duration = Duration::from_secs(90);
/// The longest file or folder name written, in bytes: under the 255 every file system takes,
/// with room for `.part` and a ` (2)`.
pub(crate) const NAME_MAX: usize = 200;
/// The receiver's longest text message (a `get` with a long path).
pub(crate) const TEXT_MAX: usize = 4096;

pub(crate) fn text(v: Value) -> Message {
    Message::Text(v.to_string().into())
}

fn data_header(n: u32, offset: u64) -> [u8; HEADER] {
    let mut h = [0u8; HEADER];
    h[..4].copy_from_slice(&n.to_le_bytes());
    h[4..].copy_from_slice(&offset.to_le_bytes());
    h
}

/// `(n, offset, bytes)` of a data message.
pub(crate) fn parse_data(b: &[u8]) -> Option<(u32, u64, &[u8])> {
    if b.len() < HEADER {
        return None;
    }
    let n = u32::from_le_bytes(b[..4].try_into().ok()?);
    let off = u64::from_le_bytes(b[4..12].try_into().ok()?);
    Some((n, off, &b[HEADER..]))
}

async fn next_text<S: GuestSocket>(s: &mut S, wait: Duration) -> Result<Value, &'static str> {
    loop {
        match tokio::time::timeout(wait, s.recv()).await {
            Err(_) => return Err("timeout"),
            Ok(None) | Ok(Some(Err(()))) => return Err("lost"),
            Ok(Some(Ok(Message::Text(t)))) => {
                if t.len() > TEXT_MAX {
                    return Err("protocol");
                }
                return serde_json::from_str(t.as_str()).map_err(|_| "protocol");
            }
            Ok(Some(Ok(Message::Close(_)))) => return Err("lost"),
            Ok(Some(Ok(_))) => continue,
        }
    }
}

/// What the sending side's session needs of the share registry (share/mod.rs implements it;
/// so do the tests).
pub(crate) trait Shares: Send + Sync + 'static {
    /// The share of `room` (None: `room` from a LAN socket's hello). Its id.
    fn find(&self, room: &str) -> Option<String>;
    /// The offer a hello gets, or why not.
    fn hello(&self, id: &str) -> Result<Value, &'static str>;
    fn ask(&self, id: &str, name: &str, wants: &[String]) -> Result<u64, &'static str>;
    fn resume(&self, id: &str, token: &str) -> Result<u64, &'static str>;
    /// The request's state now, and its token.
    fn state(&self, id: &str, rid: u64) -> Option<(ReqState, Option<String>)>;
    /// Waits until something about share `id` changed (or a second passed).
    fn changed(&self, id: &str) -> impl std::future::Future<Output = ()> + Send;
    /// The items and what this request accepted.
    fn items(&self, id: &str, rid: u64) -> Option<(Arc<Vec<Item>>, Vec<String>)>;
    fn sending(&self, id: &str, rid: u64, total: u64) -> Result<(), &'static str>;
    fn progress(&self, id: &str, rid: u64, done: u64);
    fn done(&self, id: &str, rid: u64);
    fn gone(&self, id: &str, rid: u64, cancelled: bool, why: &str);
    /// Where the connection goes (`lan`, `tailscale`, `internet`), for the panel.
    fn path(&self, id: &str, rid: u64, path: Option<&'static str>);
}

/// One receiver's session on the sending side, start to end.
pub(crate) async fn sender_session<S: GuestSocket, R: Shares>(
    mut s: S,
    reg: Arc<R>,
    room: Option<String>,
) {
    let hello = match next_text(&mut s, HELLO_WAIT).await {
        Ok(v) => v,
        Err(_) => return,
    };
    let h = &hello["hello"];
    let hello_room = h["room"].as_str().unwrap_or("");
    let room = room.unwrap_or_else(|| hello_room.to_string());
    if room.is_empty() || (!hello_room.is_empty() && hello_room != room) {
        let _ = s.send(text(json!({"refused": {"why": "no_share"}}))).await;
        return;
    }
    let Some(id) = reg.find(&room) else {
        let _ = s.send(text(json!({"refused": {"why": "no_share"}}))).await;
        return;
    };
    let name: String = h["name"]
        .as_str()
        .unwrap_or("")
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    let name = if name.is_empty() {
        "A friend".to_string()
    } else {
        name
    };
    let resume = h["resume"].as_str().unwrap_or("");
    let rid = if !resume.is_empty() {
        match reg.resume(&id, resume) {
            Ok(rid) => rid,
            Err(why) => {
                let _ = s.send(text(json!({"refused": {"why": why}}))).await;
                return;
            }
        }
    } else {
        let offer = match reg.hello(&id) {
            Ok(o) => o,
            Err(why) => {
                let _ = s.send(text(json!({"refused": {"why": why}}))).await;
                return;
            }
        };
        if s.send(text(json!({ "offer": offer }))).await.is_err() {
            return;
        }
        // Their choice, or they leave.
        let wants: Vec<String> = loop {
            let m = tokio::select! {
                _ = reg.changed(&id) => {
                    // Cancelled, expired or used up while they looked.
                    if let Err(why) = reg.hello(&id) {
                        let _ = s.send(text(json!({"refused": {"why": why}}))).await;
                        return;
                    }
                    continue;
                }
                m = s.recv() => m,
            };
            let v: Value = match m {
                Some(Ok(Message::Text(t))) if t.len() <= TEXT_MAX => {
                    serde_json::from_str(t.as_str()).unwrap_or_default()
                }
                Some(Ok(Message::Text(_))) | Some(Ok(Message::Close(_))) | None | Some(Err(())) => {
                    return
                }
                Some(Ok(_)) => continue,
            };
            if v.get("cancel").is_some() {
                return;
            }
            if let Some(a) = v.get("accept") {
                break a["items"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .take(64)
                            .collect()
                    })
                    .unwrap_or_default();
            }
        };
        let rid = match reg.ask(&id, &name, &wants) {
            Ok(rid) => rid,
            Err(why) => {
                let _ = s.send(text(json!({"refused": {"why": why}}))).await;
                return;
            }
        };
        if s.send(text(json!({"asked": {}}))).await.is_err() {
            reg.gone(&id, rid, false, "lost");
            return;
        }
        // The answer. Meanwhile the receiver may cancel or go.
        loop {
            match reg.state(&id, rid) {
                Some((ReqState::Allowed, _)) => break,
                Some((ReqState::Asking, _)) => {}
                Some((ReqState::Declined, _)) => {
                    let _ = s.send(text(json!({"declined": {}}))).await;
                    return;
                }
                Some((ReqState::Cancelled, _)) => {
                    let _ = s.send(text(json!({"cancelled": {}}))).await;
                    return;
                }
                Some((ReqState::Left, _)) => {
                    let _ = s.send(text(json!({"refused": {"why": "expired"}}))).await;
                    return;
                }
                _ => return,
            }
            tokio::select! {
                _ = reg.changed(&id) => {}
                m = s.recv() => match m {
                    Some(Ok(Message::Text(t))) => {
                        if t.contains("\"cancel\"") {
                            reg.gone(&id, rid, true, "receiver");
                            return;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(())) => {
                        reg.gone(&id, rid, false, "left");
                        return;
                    }
                    _ => {}
                }
            }
        }
        rid
    };
    serve(&mut s, &*reg, &id, rid).await;
}

/// The allowed receiver's files.
async fn serve<S: GuestSocket, R: Shares>(s: &mut S, reg: &R, id: &str, rid: u64) {
    let Some((items, wants)) = reg.items(id, rid) else {
        return;
    };
    let Some((_, token)) = reg.state(id, rid) else {
        return;
    };
    reg.path(id, rid, s.net().and_then(|p| p.net_path()));
    let files: Vec<Value> = items
        .iter()
        .filter(|i| wants.contains(&i.id))
        .flat_map(|i| {
            i.files
                .iter()
                .map(move |f| json!({"item": i.id, "path": f.path, "size": f.size}))
        })
        .collect();
    let total: u64 = items
        .iter()
        .filter(|i| wants.contains(&i.id))
        .map(|i| i.size())
        .sum();
    if let Err(why) = reg.sending(id, rid, total) {
        let _ = s.send(text(json!({"refused": {"why": why}}))).await;
        return;
    }
    if s.send(text(json!({"allowed": {"token": token, "files": files}})))
        .await
        .is_err()
    {
        reg.gone(id, rid, false, "lost");
        return;
    }
    // How far each file got, for the progress (a resumed file starts where the receiver was).
    let mut marks: HashMap<(String, String), u64> = HashMap::new();
    let still = |reg: &R| matches!(reg.state(id, rid), Some((ReqState::Sending, _)));
    loop {
        let v = match next_text(s, IDLE_WAIT).await {
            Ok(v) => v,
            Err(why) => {
                if still(reg) {
                    reg.gone(id, rid, false, why);
                }
                return;
            }
        };
        if !still(reg) {
            let _ = s.send(text(json!({"cancelled": {}}))).await;
            return;
        }
        if v.get("done").is_some() {
            reg.done(id, rid);
            let _ = s.send(text(json!({"bye": {}}))).await;
            return;
        }
        if v.get("cancel").is_some() {
            reg.gone(id, rid, true, "receiver");
            return;
        }
        let Some(g) = v.get("get") else { continue };
        let n = g["n"].as_u64().unwrap_or(0) as u32;
        let item_id = g["item"].as_str().unwrap_or("");
        let path = g["path"].as_str().unwrap_or("");
        let offset = g["offset"].as_u64().unwrap_or(0);
        let (item, f) = match manifest::lookup(&items, &wants, item_id, path) {
            Ok(x) => (x.0.clone(), x.1.clone()),
            Err(r) => {
                crate::log_at!(
                    Info,
                    "together",
                    "share: refused a request outside the share ({})",
                    r.word()
                );
                if s.send(text(json!({"error": {"n": n, "why": r.word()}})))
                    .await
                    .is_err()
                {
                    reg.gone(id, rid, false, "lost");
                    return;
                }
                continue;
            }
        };
        let key = (item.id.clone(), f.path.clone());
        let mut at = offset.min(f.size);
        let item = Arc::new(item);
        let f = Arc::new(f);
        while at < f.size {
            let (it, fe) = (item.clone(), f.clone());
            let read = tokio::task::spawn_blocking(move || manifest::read_at(&it, &fe, at, PIECE))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            let bytes = match read {
                Ok(b) if !b.is_empty() => b,
                Ok(_) => break,
                Err(e) => {
                    let why = if e == "symlink" || e == "changed" || e == "unsafe_path" {
                        e
                    } else {
                        crate::log_at!(Info, "together", "share: a read failed: {e}");
                        "read".to_string()
                    };
                    let _ = s.send(text(json!({"error": {"n": n, "why": why}}))).await;
                    break;
                }
            };
            let mut msg = Vec::with_capacity(HEADER + bytes.len());
            msg.extend_from_slice(&data_header(n, at));
            msg.extend_from_slice(&bytes);
            if s.send(Message::Binary(msg.into())).await.is_err() {
                reg.gone(id, rid, false, "lost");
                return;
            }
            at += bytes.len() as u64;
            marks.insert(key.clone(), at);
            reg.progress(id, rid, marks.values().sum());
            // The receiver's cancel, or the sender's, between pieces.
            if !still(reg) {
                let _ = s.send(text(json!({"cancelled": {}}))).await;
                return;
            }
            if let Some(m) = s.recv().now_or_never() {
                match m {
                    Some(Ok(Message::Text(t))) if t.contains("\"cancel\"") => {
                        reg.gone(id, rid, true, "receiver");
                        return;
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(())) => {
                        reg.gone(id, rid, false, "lost");
                        return;
                    }
                    _ => {}
                }
            }
        }
        if at >= f.size {
            marks.insert(key, f.size);
            reg.progress(id, rid, marks.values().sum());
            if s.send(text(json!({"eof": {"n": n, "size": f.size}})))
                .await
                .is_err()
            {
                reg.gone(id, rid, false, "lost");
                return;
            }
        }
    }
}

// ── The receiving side ──────────────────────────────────────────────────────────────────────────

/// A message to the receiver.
#[derive(Debug)]
pub(crate) enum In {
    Text(String),
    Binary(Vec<u8>),
}

/// The receiver's connection to the sender: a WebRTC data channel or the LAN socket.
pub(crate) trait Link: Send + 'static {
    fn send_text(&mut self, t: String) -> impl std::future::Future<Output = Result<(), ()>> + Send;
    /// None when it closed.
    fn recv(&mut self) -> impl std::future::Future<Output = Option<In>> + Send;
    /// Which way it goes (`lan`, `tailscale`, `internet`), once known.
    fn path(&self) -> Option<&'static str>;
}

/// One offered item as the receiver keeps it.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct Offered {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub title_id: Option<String>,
    pub format: Option<String>,
    pub size: u64,
    pub files: u64,
    /// Why this side won't take it (a retail app: the receiver checks too).
    pub refused: Option<String>,
}

/// Reads an offer's items; an app whose build the offer shows to be retail (or unread) is marked
/// refused.
pub(crate) fn offered(v: &Value) -> Vec<Offered> {
    v["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .take(64)
                .map(|i| {
                    let s = |k: &str| {
                        i[k].as_str()
                            .map(|x| x.chars().take(200).collect::<String>())
                    };
                    let kind = s("kind").unwrap_or_default();
                    let title_id = s("title_id");
                    // The sender's reading of how the app was built, checked again here. An
                    // older sender says nothing: it applied its own, stricter rule.
                    let refused = (kind == "app")
                        .then(|| super::retail::Build::from_offer(&i["build"], title_id.as_deref()))
                        .flatten()
                        .and_then(|b| super::retail::check(&b).err())
                        .map(|r| r.message());
                    Offered {
                        id: s("id").unwrap_or_default(),
                        kind,
                        name: s("name").unwrap_or_default(),
                        title_id,
                        format: s("format"),
                        size: i["size"].as_u64().unwrap_or(0),
                        files: i["files"].as_u64().unwrap_or(0),
                        refused,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One file the sender listed on Allow.
#[derive(Debug, Clone)]
pub(crate) struct Listed {
    pub item: String,
    pub path: String,
    pub size: u64,
}

/// The receiver's view of the transfer, shared with the API.
#[derive(Debug, Default)]
pub(crate) struct RxShared {
    pub state: &'static str,
    pub from: String,
    pub title: String,
    pub title_id: Option<String>,
    pub items: Vec<Offered>,
    pub accepted: Vec<String>,
    pub expires_in_s: Option<u64>,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub error: Option<String>,
    pub path: Option<&'static str>,
    /// Where each received item now is.
    pub received: Vec<(String, PathBuf)>,
    pub token: Option<String>,
    pub dest: Option<PathBuf>,
}

/// What the person does about the offer.
#[derive(Debug)]
pub(crate) enum RxCmd {
    Accept { items: Vec<String>, dest: PathBuf },
    Cancel,
}

/// How a receiving session ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RxEnd {
    Done,
    /// The connection went mid-way: connect again with the token.
    Lost,
    /// Anything else: the state says what.
    Over,
}

fn set(sh: &Mutex<RxShared>, f: impl FnOnce(&mut RxShared)) {
    f(&mut sh.lock().unwrap_or_else(|e| e.into_inner()));
}

/// Characters no file system takes, and control characters, as `_`; trimmed, and no longer than
/// [`NAME_MAX`] bytes. A longer name loses the end of its stem, never its extension: `.pkg` cut
/// off made the file something else, and two names alike in their first 80 characters became one.
pub(crate) fn safe_name(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let t = t.trim().trim_matches('.').trim();
    let t = if t.len() > NAME_MAX {
        // The extension: after the last dot, when short enough to be one.
        let (stem, ext) = match t.rfind('.') {
            Some(i) if i > 0 && t.len() - i <= 16 => t.split_at(i),
            _ => (t, ""),
        };
        let mut cut = NAME_MAX - ext.len();
        while !stem.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}{ext}", stem[..cut].trim_end_matches(['.', ' ']))
    } else {
        t.to_string()
    };
    if t.is_empty() {
        "item".into()
    } else {
        t
    }
}

/// `name` with ` (k)` before its extension, still within [`NAME_MAX`].
fn numbered(name: &str, k: usize) -> String {
    let tag = format!(" ({k})");
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && name.len() - i <= 16 => name.split_at(i),
        _ => (name, ""),
    };
    let mut cut = stem
        .len()
        .min(NAME_MAX.saturating_sub(ext.len() + tag.len()));
    while !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{tag}{ext}", &stem[..cut])
}

/// What two targets that are one file on this computer have in common: Windows and macOS do
/// not tell `A.bin` from `a.bin`.
fn target_key(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

/// Where each listed file goes, in the listing's order (so a resumed transfer plans the same).
/// Two files that would be one here (names alike once made safe, or but for their case) each get
/// their own: the later one a ` (2)`. Before, the second was taken for the first already
/// received, or written over it. `Err` is the path that cannot be written under its item.
pub(crate) fn plan_targets(
    files: Vec<Listed>,
    dirs: &[(String, PathBuf)],
) -> Result<Vec<(Listed, PathBuf)>, PlanRefused> {
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut plan = Vec::with_capacity(files.len());
    for f in files {
        let Some(dir) = dirs.iter().find(|(id, _)| *id == f.item).map(|d| &d.1) else {
            return Err(PlanRefused::NotAccepted);
        };
        let Some(first) = target(dir, &f.path) else {
            return Err(PlanRefused::Unsafe);
        };
        let mut t = first.clone();
        let mut k = 2;
        while !taken.insert(target_key(&t)) {
            let name = first.file_name().and_then(|n| n.to_str()).unwrap_or("file");
            t = first.with_file_name(numbered(name, k));
            k += 1;
        }
        plan.push((f, t));
    }
    Ok(plan)
}

/// Why a listing cannot be written.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PlanRefused {
    NotAccepted,
    Unsafe,
}

/// The listing's size, when it adds up and is no more than the offer said (a little slack for a
/// file that grew). The sizes are the sender's: a sum that wraps would pass as a small one.
pub(crate) fn listed_total(sizes: impl IntoIterator<Item = u64>, offered: u64) -> Option<u64> {
    let total = sizes.into_iter().try_fold(0u64, u64::checked_add)?;
    (total <= offered.saturating_add(offered / 10 + (1 << 20))).then_some(total)
}

/// Where item `id` goes under `dest`: its own folder, named after it.
pub(crate) fn item_dirs(
    items: &[Offered],
    accepted: &[String],
    dest: &Path,
) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for i in items.iter().filter(|i| accepted.contains(&i.id)) {
        let base = safe_name(&i.name);
        let mut name = base.clone();
        let mut k = 2;
        while out
            .iter()
            .any(|(_, p)| p.file_name().and_then(|n| n.to_str()) == Some(&name))
        {
            name = format!("{base} ({k})");
            k += 1;
        }
        out.push((i.id.clone(), dest.join(name)));
    }
    out
}

/// Where a listed file goes: below its item's folder, each part made safe; never a link on the
/// way. None when it can't be written there.
pub(crate) fn target(dir: &Path, rel: &str) -> Option<PathBuf> {
    if !manifest::rel_ok(rel) {
        return None;
    }
    let mut p = dir.to_path_buf();
    for c in rel.split('/') {
        p.push(safe_name(c));
        if std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
            return None;
        }
    }
    Some(p)
}

/// One receiving session over `link`, from the hello to the end. On a reconnect, `resume` is the
/// token and the accepted items and folder are already in `sh`.
pub(crate) async fn receive<L: Link>(
    link: L,
    room: &str,
    name: &str,
    sh: Arc<Mutex<RxShared>>,
    cmds: &mut tokio::sync::mpsc::UnboundedReceiver<RxCmd>,
) -> RxEnd {
    receive_with(link, room, name, sh, cmds, RX_IDLE_WAIT).await
}

/// [`receive`], giving up on a sender silent for `idle`.
pub(crate) async fn receive_with<L: Link>(
    mut link: L,
    room: &str,
    name: &str,
    sh: Arc<Mutex<RxShared>>,
    cmds: &mut tokio::sync::mpsc::UnboundedReceiver<RxCmd>,
    idle: Duration,
) -> RxEnd {
    let token = sh.lock().unwrap_or_else(|e| e.into_inner()).token.clone();
    let hello = json!({"hello": {"v": 1, "room": room, "name": name, "resume": token}});
    if link.send_text(hello.to_string()).await.is_err() {
        return RxEnd::Lost;
    }
    set(&sh, |s| s.path = link.path());
    let fail = |sh: &Mutex<RxShared>, state: &'static str, why: Option<String>| {
        set(sh, |s| {
            s.state = state;
            s.error = why;
        });
        RxEnd::Over
    };
    let resuming = token.is_some();
    // The offer (a first connection), then the person's choice.
    let mut dest: Option<PathBuf> = sh.lock().unwrap_or_else(|e| e.into_inner()).dest.clone();
    if !resuming {
        // The offer comes at once; Cancel is heard while it does not.
        let first = loop {
            tokio::select! {
                c = cmds.recv() => if matches!(c, Some(RxCmd::Cancel) | None) {
                    let _ = link.send_text(json!({"cancel": {}}).to_string()).await;
                    return fail(&sh, "cancelled", None);
                },
                m = tokio::time::timeout(idle, link.recv()) => break m.ok().flatten(),
            }
        };
        let v = match first {
            Some(In::Text(t)) => serde_json::from_str::<Value>(&t).unwrap_or_default(),
            _ => {
                return fail(
                    &sh,
                    "failed",
                    Some("The sender closed the connection.".into()),
                )
            }
        };
        if let Some(r) = v.get("refused") {
            return fail(&sh, "refused", r["why"].as_str().map(str::to_string));
        }
        let Some(o) = v.get("offer") else {
            return fail(
                &sh,
                "failed",
                Some("The sender did not offer anything.".into()),
            );
        };
        set(&sh, |s| {
            s.state = "offered";
            s.from = o["from"]
                .as_str()
                .unwrap_or("A friend")
                .chars()
                .take(40)
                .collect();
            s.title = o["title"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(120)
                .collect();
            s.title_id = o["title_id"].as_str().map(str::to_string);
            s.items = offered(o);
            s.expires_in_s = o["expires_in_s"].as_u64();
        });
        // Nothing goes until the person accepts.
        let (items, d) = loop {
            tokio::select! {
                c = cmds.recv() => match c {
                    Some(RxCmd::Accept { items, dest }) => break (items, dest),
                    Some(RxCmd::Cancel) | None => {
                        let _ = link.send_text(json!({"cancel": {}}).to_string()).await;
                        return fail(&sh, "cancelled", None);
                    }
                },
                m = link.recv() => match m {
                    Some(In::Text(t)) if t.contains("\"cancelled\"") => {
                        return fail(&sh, "cancelled", Some("The sender cancelled the share.".into()));
                    }
                    Some(_) => {}
                    None => return fail(&sh, "failed", Some("The connection to the sender was lost.".into())),
                }
            }
        };
        let ok: Vec<String> = {
            let g = sh.lock().unwrap_or_else(|e| e.into_inner());
            items
                .into_iter()
                .filter(|id| g.items.iter().any(|i| &i.id == id && i.refused.is_none()))
                .collect()
        };
        if ok.is_empty() {
            let _ = link.send_text(json!({"cancel": {}}).to_string()).await;
            return fail(&sh, "cancelled", Some("Nothing was chosen.".into()));
        }
        set(&sh, |s| {
            s.accepted = ok.clone();
            s.dest = Some(d.clone());
            s.state = "asking";
        });
        dest = Some(d);
        if link
            .send_text(json!({"accept": {"items": ok}}).to_string())
            .await
            .is_err()
        {
            return fail(
                &sh,
                "failed",
                Some("The connection to the sender was lost.".into()),
            );
        }
    }
    let Some(dest) = dest else {
        return fail(&sh, "failed", None);
    };
    // The answer.
    let files: Vec<Listed> = loop {
        let m = tokio::select! {
            c = cmds.recv() => {
                if matches!(c, Some(RxCmd::Cancel) | None) {
                    let _ = link.send_text(json!({"cancel": {}}).to_string()).await;
                    return fail(&sh, "cancelled", None);
                }
                continue;
            }
            m = link.recv() => m,
        };
        let v = match m {
            Some(In::Text(t)) => serde_json::from_str::<Value>(&t).unwrap_or_default(),
            Some(In::Binary(_)) => continue,
            None if resuming => return RxEnd::Lost,
            None => {
                return fail(
                    &sh,
                    "failed",
                    Some("The connection to the sender was lost.".into()),
                )
            }
        };
        if v.get("asked").is_some() {
            continue;
        }
        if v.get("declined").is_some() {
            return fail(&sh, "declined", None);
        }
        if v.get("cancelled").is_some() {
            return fail(
                &sh,
                "cancelled",
                Some("The sender cancelled the share.".into()),
            );
        }
        if let Some(r) = v.get("refused") {
            return fail(&sh, "refused", r["why"].as_str().map(str::to_string));
        }
        if let Some(a) = v.get("allowed") {
            // No more files than the accepted items can have.
            let accepted = sh.lock().unwrap_or_else(|e| e.into_inner()).accepted.len();
            let max_listed = manifest::MAX_FILES.saturating_mul(accepted.max(1));
            let token = a["token"].as_str().map(str::to_string);
            let list: Vec<Listed> = a["files"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .take(max_listed + 1)
                        .map(|f| Listed {
                            item: f["item"].as_str().unwrap_or("").to_string(),
                            path: f["path"].as_str().unwrap_or("").to_string(),
                            size: f["size"].as_u64().unwrap_or(0),
                        })
                        .collect()
                })
                .unwrap_or_default();
            if list.len() > max_listed {
                let _ = link.send_text(json!({"cancel": {}}).to_string()).await;
                return fail(
                    &sh,
                    "failed",
                    Some("The sender listed more files than it offered.".into()),
                );
            }
            set(&sh, |s| s.token = token);
            break list;
        }
    };
    // Checked before anything is written: every file of an accepted item, a safe path, and no
    // more than the offer said (a little slack for a file that grew).
    let (accepted, offered_items) = {
        let g = sh.lock().unwrap_or_else(|e| e.into_inner());
        (g.accepted.clone(), g.items.clone())
    };
    let dirs = item_dirs(&offered_items, &accepted, &dest);
    let offered_size = offered_items
        .iter()
        .filter(|i| accepted.contains(&i.id))
        .fold(0u64, |sum, i| sum.saturating_add(i.size));
    let Some(total) = listed_total(files.iter().map(|f| f.size), offered_size) else {
        let _ = link.send_text(json!({"cancel": {}}).to_string()).await;
        return fail(
            &sh,
            "failed",
            Some("The sender sent more than it offered.".into()),
        );
    };
    let plan = match plan_targets(files, &dirs) {
        Ok(plan) => plan,
        Err(why) => {
            let _ = link.send_text(json!({"cancel": {}}).to_string()).await;
            return fail(
                &sh,
                "failed",
                Some(
                    match why {
                        PlanRefused::NotAccepted => "The sender listed something not accepted.",
                        PlanRefused::Unsafe => "The sender listed a path that isn't safe.",
                    }
                    .into(),
                ),
            );
        }
    };
    set(&sh, |s| {
        s.state = "receiving";
        s.bytes_total = total;
        s.received = dirs.clone();
    });
    let mut done_bytes: u64 = 0;
    for (n, (f, t)) in plan.iter().enumerate() {
        let n = n as u32 + 1;
        if std::fs::metadata(t).is_ok_and(|m| m.is_file() && m.len() == f.size) {
            done_bytes += f.size;
            set(&sh, |s| s.bytes_done = done_bytes);
            continue;
        }
        if let Some(parent) = t.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                let _ = link.send_text(json!({"cancel": {}}).to_string()).await;
                return fail(&sh, "failed", Some(format!("Can't write there: {e}")));
            }
        }
        let part = t.with_file_name(format!(
            "{}.part",
            t.file_name().and_then(|n| n.to_str()).unwrap_or("file")
        ));
        if std::fs::symlink_metadata(&part).is_ok_and(|m| m.file_type().is_symlink()) {
            return fail(&sh, "failed", Some("A link is in the way.".into()));
        }
        let mut have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if have > f.size {
            have = 0;
        }
        let mut file = match std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&part)
        {
            Ok(fh) => fh,
            Err(e) => {
                let _ = link.send_text(json!({"cancel": {}}).to_string()).await;
                return fail(&sh, "failed", Some(format!("Can't write there: {e}")));
            }
        };
        let _ = file.set_len(have);
        let get = json!({"get": {"n": n, "item": f.item, "path": f.path, "offset": have}});
        if link.send_text(get.to_string()).await.is_err() {
            return RxEnd::Lost;
        }
        let mut at = have;
        set(&sh, |s| s.bytes_done = done_bytes + at);
        loop {
            // Cancel is heard while the sender is silent, and a sender silent for too long is
            // a lost connection: before, either left the receive (and its slot) waiting for
            // a message that never came.
            let m = tokio::select! {
                biased;
                c = cmds.recv() => {
                    if matches!(c, Some(RxCmd::Cancel) | None) {
                        let _ = link.send_text(json!({"cancel": {}}).to_string()).await;
                        return fail(&sh, "cancelled", None);
                    }
                    continue;
                }
                m = tokio::time::timeout(idle, link.recv()) => match m {
                    Ok(m) => m,
                    Err(_) => return RxEnd::Lost,
                },
            };
            match m {
                Some(In::Binary(b)) => {
                    let Some((bn, off, data)) = parse_data(&b) else {
                        continue;
                    };
                    if bn != n {
                        continue;
                    }
                    if off != at || at + data.len() as u64 > f.size {
                        let _ = link.send_text(json!({"cancel": {}}).to_string()).await;
                        return fail(&sh, "failed", Some("The data came out of order.".into()));
                    }
                    use std::io::{Seek, SeekFrom, Write};
                    if file
                        .seek(SeekFrom::Start(at))
                        .and_then(|_| file.write_all(data))
                        .is_err()
                    {
                        let _ = link.send_text(json!({"cancel": {}}).to_string()).await;
                        return fail(
                            &sh,
                            "failed",
                            Some("Writing the file failed (is the disk full?)".into()),
                        );
                    }
                    at += data.len() as u64;
                    set(&sh, |s| s.bytes_done = done_bytes + at);
                }
                Some(In::Text(t)) => {
                    let v: Value = serde_json::from_str(&t).unwrap_or_default();
                    if let Some(e) = v.get("eof") {
                        if e["n"].as_u64() != Some(n as u64) {
                            continue;
                        }
                        if at != f.size {
                            return fail(&sh, "failed", Some("A file ended early.".into()));
                        }
                        break;
                    }
                    if let Some(e) = v.get("error") {
                        let why = e["why"].as_str().unwrap_or("error").to_string();
                        return fail(
                            &sh,
                            "failed",
                            Some(format!("The sender couldn't send {}: {why}", f.path)),
                        );
                    }
                    if v.get("cancelled").is_some() {
                        return fail(
                            &sh,
                            "cancelled",
                            Some("The sender cancelled the share.".into()),
                        );
                    }
                }
                None => return RxEnd::Lost,
            }
        }
        drop(file);
        if let Err(e) = std::fs::rename(&part, t) {
            return fail(&sh, "failed", Some(format!("Can't finish {}: {e}", f.path)));
        }
        done_bytes += f.size;
        set(&sh, |s| s.bytes_done = done_bytes);
    }
    let _ = link.send_text(json!({"done": {}}).to_string()).await;
    // The bye, briefly, so the sender has the done before the connection closes.
    let _ = tokio::time::timeout(Duration::from_secs(2), link.recv()).await;
    set(&sh, |s| {
        s.state = "done";
        s.bytes_done = s.bytes_total;
    });
    RxEnd::Done
}

/// A connected pair of in-memory sockets: the sender's side is a [`GuestSocket`], the receiver's a
/// [`Link`]. The tests' transport, and the shape every other one has.
#[cfg(test)]
pub(crate) fn mem_pair() -> (MemSocket, MemLink) {
    let (a_tx, a_rx) = tokio::sync::mpsc::channel(64);
    let (b_tx, b_rx) = tokio::sync::mpsc::channel(64);
    (
        MemSocket { tx: a_tx, rx: b_rx },
        MemLink { tx: b_tx, rx: a_rx },
    )
}

#[cfg(test)]
pub(crate) struct MemSocket {
    tx: tokio::sync::mpsc::Sender<Message>,
    rx: tokio::sync::mpsc::Receiver<Message>,
}

#[cfg(test)]
impl GuestSocket for MemSocket {
    async fn send(&mut self, m: Message) -> Result<(), ()> {
        self.tx.send(m).await.map_err(|_| ())
    }
    async fn recv(&mut self) -> Option<Result<Message, ()>> {
        self.rx.recv().await.map(Ok)
    }
}

#[cfg(test)]
pub(crate) struct MemLink {
    tx: tokio::sync::mpsc::Sender<Message>,
    rx: tokio::sync::mpsc::Receiver<Message>,
}

#[cfg(test)]
impl MemLink {
    /// Sends a raw message, as a misbehaving receiver would.
    pub(crate) async fn raw(&mut self, v: Value) {
        let _ = self.tx.send(text(v)).await;
    }
}

#[cfg(test)]
impl Link for MemLink {
    async fn send_text(&mut self, t: String) -> Result<(), ()> {
        self.tx.send(Message::Text(t.into())).await.map_err(|_| ())
    }
    async fn recv(&mut self) -> Option<In> {
        loop {
            match self.rx.recv().await? {
                Message::Text(t) => return Some(In::Text(t.to_string())),
                Message::Binary(b) => return Some(In::Binary(b.to_vec())),
                _ => continue,
            }
        }
    }
    fn path(&self) -> Option<&'static str> {
        Some("lan")
    }
}

/// When a receiving session started, for the activity.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
