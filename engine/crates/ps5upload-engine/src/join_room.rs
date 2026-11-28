//! Play Rooms (13.0.0; design: ps5upload-docs `2026-11-28-play-rooms-design.md`): today's Play
//! Together party for one console simply **is** "your room". This module holds the room's types,
//! rules and stores; join.rs (`Party`) owns the room's life and calls in here.
//!
//! * **Settings** ([`RoomCfg`], one set for every console like the rest of Play Together's): name,
//!   visibility (Private / Friends / Public), approvals, chat and voice modes (agent B's
//!   `join_chat.rs` / `join_voice.rs` read them), watchers, language, region, Nearby, what the
//!   preview shows. Saved as the optional `room` object of `play_together.json`, only the fields
//!   that differ from the defaults, read field by field (a 12.6 engine skips the key).
//! * **Secrets** ([`Secrets`], `play_room_secret.json`, mode 0600, never in a bug report): the
//!   password's hash, the lobby secret (the hub room for knocks) and the room key (P-256, sealed
//!   knocks: room_seal.rs). **Blocks** "on this computer" in `play_together_blocked.json` (0600):
//!   SHA-256 of the device tag, the name at the time, when; at most [`MAX_BLOCKS`].
//! * **Who may ask** ([`gate`]): the table of design 3.2 as one function, with the refusal words
//!   ([`Refusal`]) every surface shows the same way.
//! * **Wrong passwords** ([`PwTries`]): 5 per address/cid per 10 min then `locked_out` for 10 min;
//!   20 per room in 10 min pauses password knocks and tells the host.
//! * **Protocol and capabilities** ([`PROTO`], [`CAPS`]): in the hello, the card, the preview and
//!   the room push; peers two versions back are taken, features degrade by capability.
//! * **The room bus** ([`bus`]): the hook points chat and voice (agent B) use: each guest's own
//!   voice and chat queues (below the picture and sound, which keep rp_stream_api's queues; input
//!   is never queued), messages from guests the core does not handle, and the host's mutes,
//!   enforced before anything reaches a handler.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::room_seal::{self, PwHash, RoomKey};

/// The rooms protocol (design 10.5): 1 = 12.6 Play Together (implicit, no `proto` field), 2 = 13.0
/// rooms. Each later protocol-changing release bumps it.
pub(crate) const PROTO: u32 = 2;
/// Peers this many versions back are still taken.
pub(crate) const PROTO_WINDOW: u32 = 2;
/// What this engine's rooms can do, in the hello answer, the card, the preview and the room push.
/// A feature is used only when both sides list it. `chat`/`voice`/`ready` are added by the modules
/// that serve them ([`bus::add_caps`]).
pub(crate) const CAPS: &[&str] = &[
    "rooms",
    "knock",
    "sealed",
    "preview",
    "seat_move",
    "watch",
    "hevc",
    "track",
    "ps5app",
    "names",
];

/// The capabilities this engine says it has now (the base ones and those modules added).
pub(crate) fn caps() -> Vec<String> {
    let mut v: Vec<String> = CAPS.iter().map(|s| s.to_string()).collect();
    for c in bus::extra_caps() {
        if !v.contains(&c) {
            v.push(c);
        }
    }
    v
}

/// A peer that says protocol `p` (None: a 12.6 page or app, protocol 1) may play here.
pub(crate) fn proto_ok(p: Option<u32>) -> bool {
    proto_ok_at(p, PROTO)
}

/// [`proto_ok`] for an engine at protocol `current`.
pub(crate) fn proto_ok_at(p: Option<u32>, current: u32) -> bool {
    p.unwrap_or(1).saturating_add(PROTO_WINDOW) >= current
}

/// Most people in a room, the host included (the PS5 takes four controllers).
pub(crate) const SIZE_MAX: u8 = 4;
/// Wrong passwords per address/cid before it waits [`PW_LOCKOUT`].
pub(crate) const PW_TRIES: usize = 5;
pub(crate) const PW_WINDOW: Duration = Duration::from_secs(10 * 60);
pub(crate) const PW_LOCKOUT: Duration = Duration::from_secs(10 * 60);
/// Wrong passwords in one room within [`PW_WINDOW`] that pause password knocks for
/// [`PW_LOCKOUT`] (the host is told: "Many wrong passwords. Change it?").
pub(crate) const PW_ROOM_TRIES: usize = 20;
/// Blocks kept "on this computer".
pub(crate) const MAX_BLOCKS: usize = 256;
/// Watchers (spectators) at most.
pub(crate) const WATCHERS_MAX: u8 = 2;
/// The 21 app languages a room may name.
pub(crate) const LANGS: [&str; 21] = [
    "ar", "bn", "de", "en", "es", "fa", "fr", "hi", "hu", "id", "it", "ja", "ko", "pl", "pt-BR",
    "ru", "th", "tr", "vi", "zh-CN", "zh-TW",
];
/// The coarse regions (design 3.1), as the hub spells them (ps5upload-hub PROTOCOL.md, Rooms).
pub(crate) const REGIONS: [&str; 12] = [
    "na-west",
    "na-east",
    "central-am",
    "south-am",
    "eu-west",
    "eu-east",
    "middle-east",
    "africa",
    "asia-south",
    "asia-east",
    "asia-se",
    "oceania",
];

// ── Settings ────────────────────────────────────────────────────────────────────────────────────

/// Who can see the room (asking paths are separate and never widened silently).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Visibility {
    /// Invites, Play again, Nearby. Shown nowhere (today's Play Together).
    #[default]
    Private,
    /// As Private, plus people you've played with see "Alex's room is open".
    Friends,
    /// As Friends, plus a card in Rooms.
    Public,
}

impl Visibility {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Visibility::Private => "private",
            Visibility::Friends => "friends",
            Visibility::Public => "public",
        }
    }
}

/// Room chat (served by join_chat.rs; the engine enforces it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ChatMode {
    Off,
    /// Quick messages only (ids, shown in each person's language).
    Quick,
    On,
}

/// Room voice (served by join_voice.rs; the engine enforces it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum VoiceMode {
    Off,
    Ptt,
    Open,
}

/// Who gets in without the host answering. The host always decides otherwise ("Ask me").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Approval {
    /// People you've played with (a friend pass) get in without asking. On (scenario audit).
    #[serde(default = "yes")]
    pub friends: bool,
    /// The password lets people in without asking (only with a password).
    #[serde(default)]
    pub password: bool,
    /// PS5s of this household (consoles this computer knows) get in without asking. Off.
    #[serde(default)]
    pub household: bool,
}

impl Default for Approval {
    fn default() -> Self {
        Approval {
            friends: true,
            password: false,
            household: false,
        }
    }
}

/// What the preview (Details before joining) shows: each line has a switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PreviewShows {
    #[serde(default = "yes")]
    pub names: bool,
    #[serde(default = "yes")]
    pub pings: bool,
    #[serde(default = "yes")]
    pub stream: bool,
    #[serde(default = "yes")]
    pub time: bool,
    /// "Let people check their connection before asking": a real ping for public strangers over
    /// the internet (off: they see an estimate; friends and the home network always measure).
    #[serde(default)]
    pub check: bool,
}

impl Default for PreviewShows {
    fn default() -> Self {
        PreviewShows {
            names: true,
            pings: true,
            stream: true,
            time: true,
            check: false,
        }
    }
}

fn yes() -> bool {
    true
}

/// The room's settings (saved; no secrets: the password is in [`Secrets`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RoomCfg {
    /// None: "<host>'s room" (made from the host's name when shown).
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub visibility: Visibility,
    #[serde(default)]
    pub approval: Approval,
    /// None: by visibility (Public: quick; else on).
    #[serde(default)]
    pub chat: Option<ChatMode>,
    /// None: by visibility (Public: push-to-talk; else open mic allowed).
    #[serde(default)]
    pub voice: Option<VoiceMode>,
    /// None: by visibility (Public: on).
    #[serde(default)]
    pub start_muted: Option<bool>,
    #[serde(default = "two")]
    pub watchers: u8,
    /// One of [`LANGS`]; None: any.
    #[serde(default)]
    pub lang: Option<String>,
    /// One of [`REGIONS`], "hidden", or None (from the time zone; only sent for Friends/Public).
    #[serde(default)]
    pub region: Option<String>,
    /// Show on this network (Nearby asks).
    #[serde(default = "yes")]
    pub nearby: bool,
    #[serde(default)]
    pub preview: PreviewShows,
    /// The first time Public was confirmed (design 5.1): the UI asks only once.
    #[serde(default)]
    pub public_ok: bool,
}

fn two() -> u8 {
    WATCHERS_MAX
}

impl Default for RoomCfg {
    fn default() -> Self {
        RoomCfg {
            name: None,
            visibility: Visibility::Private,
            approval: Approval::default(),
            chat: None,
            voice: None,
            start_muted: None,
            watchers: WATCHERS_MAX,
            lang: None,
            region: None,
            nearby: true,
            preview: PreviewShows::default(),
            public_ok: false,
        }
    }
}

impl RoomCfg {
    pub(crate) fn chat(&self) -> ChatMode {
        self.chat.unwrap_or(match self.visibility {
            Visibility::Public => ChatMode::Quick,
            _ => ChatMode::On,
        })
    }

    pub(crate) fn voice(&self) -> VoiceMode {
        self.voice.unwrap_or(match self.visibility {
            Visibility::Public => VoiceMode::Ptt,
            _ => VoiceMode::Open,
        })
    }

    pub(crate) fn start_muted(&self) -> bool {
        self.start_muted
            .unwrap_or(self.visibility == Visibility::Public)
    }

    /// The room's name as shown: the saved one, else "<host>'s room".
    pub(crate) fn name_for(&self, host: &str) -> String {
        match &self.name {
            Some(n) => n.clone(),
            None => default_name(host),
        }
    }

    /// The `room` object to keep in `play_together.json`: only what differs from the defaults
    /// (None: nothing to keep).
    pub(crate) fn stored(&self) -> Option<Value> {
        let d = Value::Object(Default::default());
        let mine = serde_json::to_value(self).ok()?;
        let def = serde_json::to_value(RoomCfg::default()).ok()?;
        let (Value::Object(m), Value::Object(dm)) = (mine, def) else {
            return None;
        };
        let out: serde_json::Map<String, Value> = m
            .into_iter()
            .filter(|(k, v)| dm.get(k) != Some(v))
            .collect();
        (out != *d.as_object()?).then_some(Value::Object(out))
    }

    /// The saved `room` object, each field on its own (one that doesn't parse keeps its default).
    pub(crate) fn from_stored(v: Option<&Value>) -> RoomCfg {
        let mut c = RoomCfg::default();
        let Some(Value::Object(o)) = v else {
            return c;
        };
        fn f<T: serde::de::DeserializeOwned>(
            o: &serde_json::Map<String, Value>,
            k: &str,
        ) -> Option<T> {
            serde_json::from_value(o.get(k)?.clone()).ok()
        }
        if let Some(n) = f::<String>(o, "name").and_then(|n| room_name(&n).ok()) {
            c.name = Some(n);
        }
        if let Some(v) = f(o, "visibility") {
            c.visibility = v;
        }
        if let Some(v) = f(o, "approval") {
            c.approval = v;
        }
        c.chat = f(o, "chat");
        c.voice = f(o, "voice");
        c.start_muted = f(o, "start_muted");
        if let Some(w) = f::<u8>(o, "watchers").filter(|w| *w <= WATCHERS_MAX) {
            c.watchers = w;
        }
        c.lang = f::<String>(o, "lang").filter(|l| LANGS.contains(&l.as_str()));
        c.region = f::<String>(o, "region").filter(|r| region_ok(r));
        if let Some(v) = f(o, "nearby") {
            c.nearby = v;
        }
        if let Some(v) = f(o, "preview") {
            c.preview = v;
        }
        if let Some(v) = f(o, "public_ok") {
            c.public_ok = v;
        }
        c
    }
}

pub(crate) fn region_ok(r: &str) -> bool {
    r == "hidden" || REGIONS.contains(&r)
}

/// "<host>'s room", cut to 32 characters.
pub(crate) fn default_name(host: &str) -> String {
    let host = crate::join_names::effective(host.trim());
    let head: String = host.chars().take(24).collect();
    format!("{}'s room", head.trim_end())
}

/// A room name the host typed, cleaned (3–32 characters, the hub's own name rules).
pub(crate) fn room_name(raw: &str) -> Result<String, ps5upload_hub::names::NameError> {
    ps5upload_hub::names::clean_name(raw, 3, 32, &[])
}

/// A guest's name as the room keeps it: printable, 1–24 characters, the same rules as room names;
/// a name that breaks them is kept only with its letters and digits (never refused: the host's
/// app shows "Friend" for an empty one).
pub(crate) fn guest_name(raw: &str) -> String {
    let raw: String = raw.chars().filter(|c| !c.is_control()).take(64).collect();
    if raw.trim().is_empty() {
        return String::new();
    }
    match ps5upload_hub::names::clean_name(&raw, 1, crate::join_names::NAME_MAX, &[]) {
        Ok(n) => n,
        Err(ps5upload_hub::names::NameError::Length) => raw
            .trim()
            .chars()
            .take(crate::join_names::NAME_MAX)
            .collect::<String>()
            .trim()
            .to_string(),
        // Rude words, links and codes: not a name in this room.
        Err(_) => String::new(),
    }
}

// ── Secrets and blocks ──────────────────────────────────────────────────────────────────────────

const SECRET_FILE: &str = "play_room_secret.json";
const BLOCKED_FILE: &str = "play_together_blocked.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct SecretFile {
    #[serde(default, flatten, skip_serializing_if = "Option::is_none")]
    pw: Option<PwHash>,
    #[serde(default)]
    lobby_secret: String,
    #[serde(default)]
    room_key_p256: String,
}

/// The room's secrets (one set for this engine).
pub(crate) struct Secrets {
    path: Option<PathBuf>,
    pw: Option<PwHash>,
    /// The hub lobby room's secret (its id is made from it).
    pub lobby_secret: String,
    pub key: RoomKey,
}

fn write_private(path: &std::path::Path, body: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write;
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o.open(&tmp)?;
        f.write_all(body)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

impl Secrets {
    fn load(path: Option<PathBuf>) -> Option<Secrets> {
        let file: SecretFile = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let key = RoomKey::from_b64(&file.room_key_p256).or_else(RoomKey::generate)?;
        let lobby_secret = if file.lobby_secret.len() == 43 {
            file.lobby_secret
        } else {
            crate::join_net::random_b64(32)?
        };
        let s = Secrets {
            path,
            pw: file.pw,
            lobby_secret,
            key,
        };
        s.save();
        Some(s)
    }

    fn save(&self) {
        let Some(p) = &self.path else {
            return;
        };
        let body = SecretFile {
            pw: self.pw.clone(),
            lobby_secret: self.lobby_secret.clone(),
            room_key_p256: self.key.to_b64(),
        };
        if let Ok(b) = serde_json::to_vec_pretty(&body) {
            if let Err(e) = write_private(p, &b) {
                crate::log_at!(
                    Warn,
                    "together",
                    "room: the room's secrets were not saved: {e}"
                );
            }
        }
    }

    pub(crate) fn has_password(&self) -> bool {
        self.pw.is_some()
    }

    pub(crate) fn password_ok(&self, pw: &str) -> bool {
        self.pw
            .as_ref()
            .is_some_and(|h| room_seal::password_ok(h, pw))
    }

    /// The lobby room's id on the hub (made from its secret).
    pub(crate) fn lobby_id(&self) -> String {
        ps5upload_hub::hub::room_for_secret(&self.lobby_secret)
    }
}

static SECRETS: Mutex<Option<Secrets>> = Mutex::new(None);

fn data_path(name: &str) -> Option<PathBuf> {
    if cfg!(test) {
        return None; // never the user's own data folder
    }
    ps5upload_ava1::pool::data_dir().map(|d| d.join(name))
}

/// The room's secrets, made on first use.
pub(crate) fn with_secrets<R>(f: impl FnOnce(&mut Secrets) -> R) -> Option<R> {
    let mut g = SECRETS.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Secrets::load(data_path(SECRET_FILE));
    }
    g.as_mut().map(f)
}

/// Sets (Some) or removes (None or "") the room password.
pub(crate) fn set_password(pw: Option<&str>) -> Result<(), room_seal::PwErr> {
    let h = match pw.filter(|p| !p.is_empty()) {
        Some(p) => Some(room_seal::hash_password(p)?),
        None => None,
    };
    with_secrets(|s| {
        s.pw = h;
        s.save();
    });
    PW_TRIES_ALL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    Ok(())
}

pub(crate) fn has_password() -> bool {
    with_secrets(|s| s.has_password()).unwrap_or(false)
}

/// A device tag's stored form: SHA-256 hex of the tag source.
pub(crate) fn tag_hash(device: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(device.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A guest device's id from the hello (`device`: 16 random bytes, base64url; 8–64 characters of
/// base64url taken): its 4-character tag shown to people ("Sam · 4F2A") and its hash for blocks.
pub(crate) fn device_tag(device: Option<&str>) -> Option<(String, String)> {
    let d = device?.trim();
    if !(8..=64).contains(&d.len())
        || !d
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let h = tag_hash(d);
    Some((h[..4].to_ascii_uppercase(), h))
}

/// Somebody blocked from the room (by the host's Remove … "Don't let them back in this room"), or
/// on this computer ("Block on this computer").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Block {
    /// SHA-256 of their device id (None: their app sent none).
    #[serde(default)]
    pub tag_sha256: Option<String>,
    /// The 4 characters people saw.
    #[serde(default)]
    pub tag: Option<String>,
    /// Their address key (this run only; never saved, never shown).
    #[serde(skip)]
    pub addr: Option<IpAddr>,
    /// Their hub client id in the lobby room (this run only).
    #[serde(skip)]
    pub cid: Option<String>,
    pub name: String,
    pub at_unix: u64,
    /// Kept on this computer (file), not only for the room's life.
    #[serde(default)]
    pub always: bool,
}

impl Block {
    pub(crate) fn matches(&self, who: &Who) -> bool {
        (self.tag_sha256.is_some() && self.tag_sha256 == who.tag_sha256)
            || (self.addr.is_some()
                && who.addr.map(crate::join::limit_key) == self.addr.map(crate::join::limit_key))
            || (self.cid.is_some() && self.cid == who.cid)
    }

    pub(crate) fn json(&self) -> Value {
        json!({"name": self.name, "tag": self.tag, "at_unix": self.at_unix, "always": self.always})
    }
}

/// Who is asking, as blocks and limits see them (never shown).
#[derive(Clone, Debug, Default)]
pub(crate) struct Who {
    pub tag_sha256: Option<String>,
    pub addr: Option<IpAddr>,
    pub cid: Option<String>,
}

impl Who {
    /// The key wrong passwords count under: the hub cid, else the address key.
    fn limit_key(&self) -> String {
        match (&self.cid, self.addr) {
            (Some(c), _) => format!("cid:{c}"),
            (None, Some(a)) => format!("ip:{}", crate::join::limit_key(a)),
            (None, None) => "none".into(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct BlockedFile {
    blocked: Vec<Block>,
}

static BLOCKED: Mutex<Option<Vec<Block>>> = Mutex::new(None);

fn with_blocked<R>(f: impl FnOnce(&mut Vec<Block>) -> R) -> R {
    let mut g = BLOCKED.lock().unwrap_or_else(|e| e.into_inner());
    let v = g.get_or_insert_with(|| {
        data_path(BLOCKED_FILE)
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<BlockedFile>(&b).ok())
            .map(|f| f.blocked)
            .unwrap_or_default()
    });
    f(v)
}

fn save_blocked(v: &[Block]) {
    if let Some(p) = data_path(BLOCKED_FILE) {
        let body = serde_json::to_vec_pretty(&BlockedFile {
            blocked: v.to_vec(),
        })
        .unwrap_or_default();
        if let Err(e) = write_private(&p, &body) {
            crate::log_at!(
                Warn,
                "together",
                "room: the blocked list was not saved: {e}"
            );
        }
    }
}

/// Blocks on this computer (a device tag is needed: an address changes).
pub(crate) fn block_always(b: Block) {
    if b.tag_sha256.is_none() {
        return;
    }
    with_blocked(|v| {
        v.retain(|x| x.tag_sha256 != b.tag_sha256);
        v.push(Block { always: true, ..b });
        while v.len() > MAX_BLOCKS {
            v.remove(0);
        }
        save_blocked(v);
    });
}

/// Unblocks a tag (the 4 characters or the full hash) on this computer: true when one went.
pub(crate) fn unblock_always(tag: &str) -> bool {
    with_blocked(|v| {
        let n = v.len();
        v.retain(|b| !tag_is(b, tag));
        let gone = v.len() != n;
        if gone {
            save_blocked(v);
        }
        gone
    })
}

pub(crate) fn tag_is(b: &Block, tag: &str) -> bool {
    b.tag
        .as_deref()
        .is_some_and(|t| t.eq_ignore_ascii_case(tag))
        || b.tag_sha256.as_deref() == Some(tag)
}

pub(crate) fn blocked_always(who: &Who) -> bool {
    who.tag_sha256.is_some() && with_blocked(|v| v.iter().any(|b| b.matches(who)))
}

pub(crate) fn blocked_always_list() -> Vec<Block> {
    with_blocked(|v| v.clone())
}

#[cfg(test)]
pub(crate) fn test_clear_blocks() {
    *BLOCKED.lock().unwrap_or_else(|e| e.into_inner()) = Some(Vec::new());
}

// ── Wrong passwords ─────────────────────────────────────────────────────────────────────────────

/// Wrong passwords: per address/cid and per room.
#[derive(Debug, Default)]
pub(crate) struct PwTries {
    by: HashMap<String, (VecDeque<Instant>, Option<Instant>)>,
    room: VecDeque<Instant>,
    paused_until: Option<Instant>,
    /// The host has been told about many wrong passwords (cleared by a new password).
    pub warned: bool,
}

/// The result of a password check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PwCheck {
    /// No password needed (none set, or an invite).
    NotNeeded,
    Ok,
    /// None given while one is needed.
    Missing,
    /// Wrong; tries left before the wait.
    Wrong(usize),
    /// Too many wrong; seconds to wait.
    LockedOut(u64),
}

static PW_TRIES_ALL: Mutex<Option<PwTries>> = Mutex::new(None);

fn prune(q: &mut VecDeque<Instant>, now: Instant) {
    while q
        .front()
        .is_some_and(|t| now.saturating_duration_since(*t) >= PW_WINDOW)
    {
        q.pop_front();
    }
}

impl PwTries {
    fn wait(&mut self, key: &str, now: Instant) -> Option<u64> {
        if let Some(t) = self.paused_until.filter(|t| *t > now) {
            return Some(t.duration_since(now).as_secs().max(1));
        }
        let (_, until) = self.by.get(key)?;
        until
            .filter(|t| *t > now)
            .map(|t| t.duration_since(now).as_secs().max(1))
    }

    fn wrong(&mut self, key: &str, now: Instant) -> PwCheck {
        if self.by.len() > 4096 {
            self.by.retain(|_, (q, until)| {
                prune(q, now);
                !q.is_empty() || until.is_some_and(|t| t > now)
            });
        }
        let (q, until) = self.by.entry(key.to_string()).or_default();
        prune(q, now);
        q.push_back(now);
        prune(&mut self.room, now);
        self.room.push_back(now);
        if self.room.len() >= PW_ROOM_TRIES {
            self.paused_until = Some(now + PW_LOCKOUT);
            self.room.clear();
            self.warned = true;
        }
        if q.len() >= PW_TRIES {
            q.clear();
            *until = Some(now + PW_LOCKOUT);
            return PwCheck::LockedOut(PW_LOCKOUT.as_secs());
        }
        PwCheck::Wrong(PW_TRIES - q.len())
    }
}

/// Checks a knock's password: `given` (None: none sent) for `who`, when the room has one and the
/// way in needs it.
pub(crate) fn check_password(
    needed: bool,
    given: Option<&str>,
    who: &Who,
    now: Instant,
) -> PwCheck {
    if !needed || !has_password() {
        return PwCheck::NotNeeded;
    }
    let key = who.limit_key();
    let mut g = PW_TRIES_ALL.lock().unwrap_or_else(|e| e.into_inner());
    let t = g.get_or_insert_with(PwTries::default);
    if let Some(s) = t.wait(&key, now) {
        return PwCheck::LockedOut(s);
    }
    let Some(pw) = given.filter(|p| !p.is_empty()) else {
        return PwCheck::Missing;
    };
    // Off the lock: PBKDF2 takes a while.
    drop(g);
    if with_secrets(|s| s.password_ok(pw)).unwrap_or(false) {
        return PwCheck::Ok;
    }
    PW_TRIES_ALL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(PwTries::default)
        .wrong(&key, now)
}

/// Many wrong passwords lately (the host is asked "Change it?").
pub(crate) fn many_wrong_passwords() -> bool {
    PW_TRIES_ALL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|t| t.warned)
}

// ── Who may ask ─────────────────────────────────────────────────────────────────────────────────

/// How someone came to the room.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Via {
    /// The host's invite link, QR or short code: a personal invitation (never a password).
    Invite,
    /// Play again (a friend pass).
    Again,
    /// The same network (no code), "Show on this network" on.
    Nearby,
    /// Rooms › Friends ("Alex's room is open").
    Friends,
    /// The public Rooms list.
    Rooms,
    /// Another controller of a player already in (many controllers on one PS5).
    Group,
}

impl Via {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Via::Invite => "invite",
            Via::Again => "again",
            Via::Nearby => "nearby",
            Via::Friends => "friends",
            Via::Rooms => "rooms",
            Via::Group => "group",
        }
    }

    pub(crate) fn from_word(w: &str) -> Option<Via> {
        Some(match w {
            "invite" => Via::Invite,
            "again" => Via::Again,
            "nearby" => Via::Nearby,
            "friends" => Via::Friends,
            "rooms" => Via::Rooms,
            "group" => Via::Group,
            _ => return None,
        })
    }

    /// A stranger: came from the public list (never Player 1, never the host's profile, PS &
    /// Create always blocked).
    pub(crate) fn stranger(self) -> bool {
        self == Via::Rooms
    }

    /// This way in needs the room password when one is set.
    pub(crate) fn needs_password(self) -> bool {
        matches!(self, Via::Again | Via::Nearby | Via::Friends | Via::Rooms)
    }
}

/// Why someone can't ask (the same words on every surface; design 3.2, 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The host said no — also what a blocked person hears.
    No,
    Full {
        allowed: u8,
        in_: u8,
    },
    Locked,
    /// Wrong or missing password; tries left before the wait (None: none sent yet).
    Password(Option<usize>),
    LockedOut(u64),
    HostAway,
    NotListed,
    Old,
}

impl Refusal {
    /// The `why` word (old pages know `denied` and `full`; the rest are new).
    pub(crate) fn word(self) -> &'static str {
        match self {
            Refusal::No => "denied",
            Refusal::Full { .. } => "full",
            Refusal::Locked => "locked",
            Refusal::Password(_) => "password",
            Refusal::LockedOut(_) => "locked_out",
            Refusal::HostAway => "host_away",
            Refusal::NotListed => "not_listed",
            Refusal::Old => "old",
        }
    }

    /// The refusal as the guest gets it (`{"stage": "refused", "why", …}`).
    pub(crate) fn json(self) -> Value {
        let mut v = json!({"stage": "refused", "why": self.word()});
        match self {
            Refusal::Full { allowed, in_ } => {
                v["allowed"] = allowed.into();
                v["in"] = in_.into();
            }
            Refusal::Password(left) => v["tries_left"] = left.into(),
            Refusal::LockedOut(s) => v["wait_s"] = s.into(),
            Refusal::Old => {
                v["needs"] = json!({"proto": PROTO - PROTO_WINDOW, "version": "13.0.0"})
            }
            _ => {}
        }
        v
    }
}

/// The room as the gate sees it now.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RoomNow {
    pub visibility: Visibility,
    pub nearby: bool,
    pub locked: bool,
    pub approval: Approval,
    pub password_set: bool,
    /// Guests allowed (room size − 1).
    pub allowed: u8,
    pub guests_in: u8,
    pub pending: usize,
    /// The host's device is away (no host page): nobody can answer an ask.
    pub host_away: bool,
}

/// The one asking, as the gate sees them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Asker {
    pub via: Via,
    pub blocked: bool,
    pub password: PwCheck,
    /// Holds a friend pass of this computer.
    pub friend: bool,
    /// A PS5 this computer knows (its roster).
    pub household: bool,
    pub proto: Option<u32>,
}

/// What happens to an ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gate {
    /// The host decides.
    Ask,
    /// In without asking (an approval the host turned on).
    LetIn,
    Refuse(Refusal),
}

/// Asks waiting at once at most (pending asks don't count against the room size).
pub(crate) const MAX_PENDING: usize = 4;

/// Design 3.2 as one function.
pub(crate) fn gate(r: &RoomNow, a: &Asker) -> Gate {
    if !proto_ok(a.proto) {
        return Gate::Refuse(Refusal::Old);
    }
    // Blocked: told the same words as a plain no.
    if a.blocked {
        return Gate::Refuse(Refusal::No);
    }
    if r.locked {
        return Gate::Refuse(Refusal::Locked);
    }
    let path_open = match a.via {
        Via::Invite | Via::Again | Via::Group => true,
        Via::Nearby => r.nearby,
        Via::Friends => r.visibility != Visibility::Private,
        Via::Rooms => r.visibility == Visibility::Public,
    };
    if !path_open {
        return Gate::Refuse(Refusal::NotListed);
    }
    if r.guests_in >= r.allowed {
        return Gate::Refuse(Refusal::Full {
            allowed: r.allowed,
            in_: r.guests_in,
        });
    }
    let pw_ok = match a.password {
        PwCheck::NotNeeded => None,
        PwCheck::Ok => Some(true),
        PwCheck::Missing => return Gate::Refuse(Refusal::Password(None)),
        PwCheck::Wrong(left) => return Gate::Refuse(Refusal::Password(Some(left))),
        PwCheck::LockedOut(s) => return Gate::Refuse(Refusal::LockedOut(s)),
    };
    let auto =
        match a.via {
            Via::Invite | Via::Group => false,
            Via::Again | Via::Nearby | Via::Friends => {
                (a.friend && r.approval.friends) || (a.household && r.approval.household)
            }
            Via::Rooms => false,
        } || (pw_ok == Some(true) && r.password_set && r.approval.password && a.via != Via::Invite);
    if auto {
        return Gate::LetIn;
    }
    if r.host_away {
        return Gate::Refuse(Refusal::HostAway);
    }
    if r.pending >= MAX_PENDING {
        return Gate::Refuse(Refusal::Full {
            allowed: r.allowed,
            in_: r.guests_in,
        });
    }
    Gate::Ask
}

// ── Per room, while open ────────────────────────────────────────────────────────────────────────

/// A game the room is about (auto from the console, or picked before the room was made).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Game {
    pub title: String,
    pub id: String,
    /// The host picked it ("Playing something else?"); else it follows the console.
    #[serde(default)]
    pub picked: bool,
}

/// The host's mutes on one guest (enforced in the engine, [`bus`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Muted {
    pub voice: bool,
    pub chat: bool,
}

/// The room's listing on the hub (Friends/Public), as the host's panel shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Listed {
    /// Private: no card.
    #[default]
    Off,
    Listing,
    /// Known to the hub until (unix seconds).
    Listed(u64),
    /// Not listed, and why (`internet_off`, `hub`, `name`, `busy`, `limit`, `old_hub`, `locked`,
    /// `host_away`, …).
    Not(String),
}

impl Listed {
    pub(crate) fn json(&self) -> Value {
        match self {
            Listed::Off => json!({"state": "off", "why": null}),
            Listed::Listing => json!({"state": "listing", "why": null}),
            Listed::Listed(u) => json!({"state": "listed", "until_unix": u, "why": null}),
            Listed::Not(w) => json!({"state": "not_listed", "why": w}),
        }
    }
}

/// The room's run-time state (not saved).
#[derive(Debug)]
pub(crate) struct RoomRun {
    pub locked: bool,
    pub opened_at: Instant,
    pub game: Option<Game>,
    pub blocked: Vec<Block>,
    /// Host mutes by guest id.
    pub muted: HashMap<u64, Muted>,
    pub listed: Listed,
    /// Past invites (key, code) and when they stopped, so a lapsed link hears `expired` (a wrong
    /// one `key`, a wrong code `code`). The last few only.
    pub old_invites: VecDeque<(String, String, Instant)>,
}

/// Past invites remembered at most, and for how long.
pub(crate) const OLD_INVITES: usize = 8;
pub(crate) const OLD_INVITE_FOR: Duration = Duration::from_secs(24 * 3600);

impl RoomRun {
    pub(crate) fn new(now: Instant) -> RoomRun {
        RoomRun {
            locked: false,
            opened_at: now,
            game: None,
            blocked: Vec::new(),
            muted: HashMap::new(),
            listed: Listed::Off,
            old_invites: VecDeque::new(),
        }
    }

    /// Blocked from this room, or on this computer.
    pub(crate) fn blocked(&self, who: &Who) -> bool {
        self.blocked.iter().any(|b| b.matches(who)) || blocked_always(who)
    }

    pub(crate) fn remember_invite(&mut self, key: &str, code: &str, now: Instant) {
        self.old_invites
            .push_back((key.to_string(), code.to_string(), now));
        while self.old_invites.len() > OLD_INVITES {
            self.old_invites.pop_front();
        }
    }

    /// `key` (or a typed `code`) was one of this room's invites, now lapsed or replaced.
    pub(crate) fn was_invite(&self, key: Option<&str>, code: Option<&str>, now: Instant) -> bool {
        self.old_invites.iter().any(|(k, c, at)| {
            now.saturating_duration_since(*at) < OLD_INVITE_FOR
                && (key.is_some_and(|x| crate::pad_link::same_key(k, x))
                    || code.is_some_and(|x| crate::pad_link::same_key(c, x)))
        })
    }
}

// ── The preview ─────────────────────────────────────────────────────────────────────────────────

/// One player as the preview shows them (never an address, a way, or a guest's tag).
#[derive(Clone, Debug)]
pub(crate) struct PreviewPlayer {
    pub name: String,
    pub seat: u8,
    /// "host", "ps5", "web", "app", "phone".
    pub kind: &'static str,
    pub tier: Option<&'static str>,
    pub rtt_ms: Option<u32>,
}

/// What the preview is made from (join.rs fills it under the party's lock).
#[derive(Clone, Debug)]
pub(crate) struct PreviewIn {
    pub host_name: String,
    pub host_tag: String,
    pub cfg: RoomCfg,
    pub size: u8,
    pub password: bool,
    pub game: Option<Game>,
    /// `res`, `fps`, `codec`, `kbps`, `hdr` of the stream (None: no session).
    pub stream: Option<Value>,
    pub host_link: Option<(u32, &'static str)>,
    pub players: Vec<PreviewPlayer>,
    pub asking: usize,
    pub open_for: Duration,
    pub watchers: usize,
    pub locked: bool,
}

/// The preview document (design 6.4). The host's switches decide which lines show.
pub(crate) fn preview(p: &PreviewIn) -> Value {
    let show = p.cfg.preview;
    let approval = if p.cfg.approval.password && p.password {
        "password"
    } else {
        "ask"
    };
    let players: Vec<Value> = p
        .players
        .iter()
        .map(|pl| {
            let mut v = json!({"seat": pl.seat, "kind": pl.kind});
            if show.names {
                v["name"] = pl.name.clone().into();
            }
            if show.pings {
                v["tier"] = pl.tier.into();
                v["rtt_ms"] = pl.rtt_ms.into();
            }
            v
        })
        .collect();
    let region = match p.cfg.region.as_deref() {
        Some("hidden") => None,
        r => r,
    };
    json!({
        "proto": PROTO,
        "caps": caps(),
        "host": {
            "name": p.host_name,
            "tag": p.host_tag,
            "ver": env!("CARGO_PKG_VERSION"),
            "playing_for_min": show.time.then(|| p.open_for.as_secs() / 60),
        },
        "room": {
            "name": p.cfg.name_for(&p.host_name),
            "visibility": p.cfg.visibility.word(),
            "size": p.size,
            "in": p.players.len(),
            "pw": p.password,
            "locked": p.locked,
            "approval": approval,
            "chat": p.cfg.chat(),
            "voice": p.cfg.voice(),
            "lang": p.cfg.lang,
            "region": region,
            "watchers": p.watchers,
            "check": show.check,
        },
        "game": p.game.as_ref().map(|g| json!({"id": g.id, "title": g.title})),
        "stream": if show.stream { p.stream.clone().unwrap_or(Value::Null) } else { Value::Null },
        "host_link": p.host_link.map(|(rtt, tier)| json!({"rtt_ms": rtt, "tier": tier})),
        "players": players,
        "asking": p.asking,
    })
}

/// Previews answered per address at most once a [`PREVIEW_GAP`], and [`PREVIEW_PER_MIN`] a minute
/// in all (the same document goes to the hub's relayed preview, cached there).
pub(crate) const PREVIEW_GAP: Duration = Duration::from_secs(10);
pub(crate) const PREVIEW_PER_MIN: usize = 30;

#[derive(Default)]
pub(crate) struct PreviewLimit {
    by: HashMap<IpAddr, Instant>,
    all: VecDeque<Instant>,
}

impl PreviewLimit {
    /// Ok, or the seconds to wait (Retry-After).
    pub(crate) fn take(&mut self, ip: IpAddr, now: Instant) -> Result<(), u64> {
        let k = crate::join::limit_key(ip);
        if self.by.len() > 4096 {
            self.by
                .retain(|_, t| now.saturating_duration_since(*t) < PREVIEW_GAP);
        }
        if let Some(t) = self.by.get(&k) {
            let since = now.saturating_duration_since(*t);
            if since < PREVIEW_GAP {
                return Err((PREVIEW_GAP - since).as_secs().max(1));
            }
        }
        while self
            .all
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= Duration::from_secs(60))
        {
            self.all.pop_front();
        }
        if self.all.len() >= PREVIEW_PER_MIN {
            let first = *self.all.front().expect("full");
            return Err(
                (Duration::from_secs(60) - now.saturating_duration_since(first))
                    .as_secs()
                    .max(1),
            );
        }
        self.by.insert(k, now);
        self.all.push_back(now);
        Ok(())
    }
}

// ── The card on the hub ─────────────────────────────────────────────────────────────────────────

/// The ad for the hub (design 9.1): `listed:false` for Friends (only the friends lookup answers
/// it); None for Private.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ad(
    cfg: &RoomCfg,
    host: &str,
    tag: &str,
    game: Option<&Game>,
    guests_in: u8,
    size: u8,
    password: bool,
    locked: bool,
    pk: &str,
) -> Option<Value> {
    if cfg.visibility == Visibility::Private {
        return None;
    }
    let mut caps = vec!["web", "ps5app"];
    caps.extend(["preview", "knock", "sealed"]);
    for c in bus::extra_caps() {
        if let Some(s) = ["chat", "voice", "ready"].into_iter().find(|x| *x == c) {
            caps.push(s);
        }
    }
    let host: String = crate::join_names::effective(host)
        .chars()
        .take(24)
        .collect();
    Some(json!({
        "listed": cfg.visibility == Visibility::Public,
        "locked": locked,
        "name": cfg.name_for(&host),
        "host": host,
        "tag": tag,
        "game": game.map(|g| json!({"title": g.title, "id": g.id})),
        "proto": PROTO,
        "ver": env!("CARGO_PKG_VERSION"),
        "caps": caps,
        // People in (the host counts) and the room size.
        "slots": [guests_in.saturating_add(1).min(size), size],
        "pw": password,
        "lang": cfg.lang,
        "region": cfg.region.clone().unwrap_or_else(|| "auto".into()),
        "pk": pk,
    }))
}

/// This engine's 4-character host tag (from its engine id).
pub(crate) fn host_tag() -> String {
    let id = crate::mesh_api::id_hex().unwrap_or_default();
    tag_hash(&id)[..4].to_ascii_uppercase()
}

// ── The room bus (hook points for chat and voice) ───────────────────────────────────────────────

/// What chat and voice (join_chat.rs / join_voice.rs) hook into. The core keeps one voice queue
/// and one chat queue per guest connection beside the picture and sound: the guest's writer takes
/// input-related replies first, then the picture and sound, then voice (oldest dropped when
/// full, never waiting behind video), then chat.
// The hook points chat and voice (join_chat.rs / join_voice.rs) call; some are used only by them.
#[allow(dead_code)]
pub(crate) mod bus {
    use super::*;
    use axum::extract::ws::Message;
    use tokio::sync::mpsc;

    /// Voice bundles queued per guest (3 × 20 ms); past it a bundle is dropped, never waited for.
    pub(crate) const VOICE_QUEUE: usize = 3;
    /// A guest's voice frame: `[6][seq u16][level u8][Opus]` (design 7A.1).
    pub(crate) const KIND_VOICE: u8 = 6;
    /// The largest voice frame taken from a guest.
    pub(crate) const VOICE_MAX: usize = 512;
    /// Chat lines queued per guest.
    pub(crate) const CHAT_QUEUE: usize = 16;

    /// A guest's lower-priority queues.
    #[derive(Clone)]
    pub(crate) struct Queues {
        pub voice: mpsc::Sender<Message>,
        pub chat: mpsc::Sender<Value>,
    }

    /// What a guest sent that the core does not handle itself.
    #[derive(Clone, Debug)]
    pub(crate) enum FromGuest {
        /// A text message (`chat`, `mic`, `ready`, …), already checked against the host's mutes.
        Text(Value),
        /// A binary voice frame (`[6][seq u16][level u8][Opus]`), from an unmuted guest.
        Voice(Vec<u8>),
    }

    /// Who sent it.
    #[derive(Clone, Debug)]
    pub(crate) struct From {
        /// The console's key (`vinput_api::host_key`).
        pub room: String,
        pub guest: u64,
        pub player: u8,
        pub name: String,
    }

    pub(crate) type Handler = fn(&From, &FromGuest);

    static QUEUES: Mutex<Option<HashMap<(String, u64), Queues>>> = Mutex::new(None);
    static HANDLERS: Mutex<Vec<Handler>> = Mutex::new(Vec::new());
    static CAPS_EXTRA: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// The host's mutes per room (console key) and guest id, mirrored from the party.
    static MUTES: Mutex<Option<HashMap<(String, u64), Muted>>> = Mutex::new(None);

    /// A module that serves `chat`/`voice`/`ready` says so (once, at engine start).
    pub(crate) fn add_caps(c: &[&str]) {
        let mut g = CAPS_EXTRA.lock().unwrap_or_else(|e| e.into_inner());
        for x in c {
            if !g.iter().any(|y| y == x) {
                g.push((*x).to_string());
            }
        }
    }

    pub(crate) fn extra_caps() -> Vec<String> {
        CAPS_EXTRA.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Hears every guest message the core does not handle (once, at engine start).
    pub(crate) fn on_guest(h: Handler) {
        let mut g = HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
        if !g.iter().any(|x| std::ptr::fn_addr_eq(*x, h)) {
            g.push(h);
        }
    }

    /// Text messages the core handles itself (never passed on).
    const CORE: &[&str] = &["ping", "leave", "keyframe", "stats", "hello"];

    /// A guest's message the core did not handle: dropped when the host muted that kind
    /// (`muted` tells the guest once), else passed to every handler. Returns what to say back to
    /// the guest (`{"muted": {...}}`), if anything.
    pub(crate) fn from_guest(from: &From, m: FromGuest) -> Option<Value> {
        let mute = muted(&from.room, from.guest);
        match &m {
            FromGuest::Voice(_) if mute.voice => return None,
            FromGuest::Text(v) => {
                let o = v.as_object()?;
                if o.keys().all(|k| CORE.contains(&k.as_str())) {
                    return None;
                }
                if mute.chat && o.contains_key("chat") {
                    return Some(json!({"muted": mute}));
                }
                if mute.voice && o.contains_key("mic") {
                    return Some(json!({"muted": mute}));
                }
            }
            _ => {}
        }
        let hs: Vec<Handler> = HANDLERS.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for h in hs {
            h(from, &m);
        }
        None
    }

    pub(crate) fn register(room: &str, guest: u64, q: Queues) {
        QUEUES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert((room.to_string(), guest), q);
    }

    pub(crate) fn unregister(room: &str, guest: u64) {
        if let Some(m) = QUEUES.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            m.remove(&(room.to_string(), guest));
        }
    }

    /// The guests (ids) with a connection in `room` now.
    pub(crate) fn guests(room: &str) -> Vec<u64> {
        QUEUES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|m| {
                m.keys()
                    .filter(|(r, _)| r == room)
                    .map(|(_, g)| *g)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn queues(room: &str, guest: u64) -> Option<Queues> {
        QUEUES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()?
            .get(&(room.to_string(), guest))
            .cloned()
    }

    /// One voice bundle to a guest: never waits; false when their queue is full (dropped) or
    /// they are not connected.
    pub(crate) fn voice_to(room: &str, guest: u64, m: Message) -> bool {
        queues(room, guest).is_some_and(|q| q.voice.try_send(m).is_ok())
    }

    /// One chat message to a guest (false: not connected, or their queue is full).
    pub(crate) fn chat_to(room: &str, guest: u64, v: Value) -> bool {
        queues(room, guest).is_some_and(|q| q.chat.try_send(v).is_ok())
    }

    /// A chat message to everyone in `room` but `except`.
    pub(crate) fn chat_all(room: &str, v: &Value, except: Option<u64>) -> usize {
        guests(room)
            .into_iter()
            .filter(|g| Some(*g) != except)
            .filter(|g| chat_to(room, *g, v.clone()))
            .count()
    }

    /// The host's mutes on a guest (chat and voice modules check before forwarding).
    pub(crate) fn muted(room: &str, guest: u64) -> Muted {
        MUTES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|m| m.get(&(room.to_string(), guest)).copied())
            .unwrap_or_default()
    }

    pub(crate) fn set_muted(room: &str, guest: u64, m: Muted) {
        let mut g = MUTES.lock().unwrap_or_else(|e| e.into_inner());
        let map = g.get_or_insert_with(HashMap::new);
        if m == Muted::default() {
            map.remove(&(room.to_string(), guest));
        } else {
            map.insert((room.to_string(), guest), m);
        }
    }

    /// The room closed: its mutes go.
    pub(crate) fn forget_room(room: &str) {
        if let Some(m) = MUTES.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            m.retain(|(r, _), _| r != room);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> RoomNow {
        RoomNow {
            visibility: Visibility::Private,
            nearby: true,
            locked: false,
            approval: Approval::default(),
            password_set: false,
            allowed: 3,
            guests_in: 0,
            pending: 0,
            host_away: false,
        }
    }

    fn asker(via: Via) -> Asker {
        Asker {
            via,
            blocked: false,
            password: PwCheck::NotNeeded,
            friend: false,
            household: false,
            proto: Some(PROTO),
        }
    }

    /// Design 3.2: who can ask, by path and visibility.
    #[test]
    fn paths_by_visibility() {
        use Visibility::*;
        let table = [
            (Via::Invite, [true, true, true]),
            (Via::Again, [true, true, true]),
            (Via::Nearby, [true, true, true]),
            (Via::Friends, [false, true, true]),
            (Via::Rooms, [false, false, true]),
            (Via::Group, [true, true, true]),
        ];
        for (via, want) in table {
            for (vis, ok) in [Private, Friends, Public].into_iter().zip(want) {
                let r = RoomNow {
                    visibility: vis,
                    ..room()
                };
                let g = gate(&r, &asker(via));
                if ok {
                    assert_eq!(g, Gate::Ask, "{via:?} {vis:?}");
                } else {
                    assert_eq!(g, Gate::Refuse(Refusal::NotListed), "{via:?} {vis:?}");
                }
            }
        }
        // Nearby off: no nearby asks.
        let r = RoomNow {
            nearby: false,
            ..room()
        };
        assert_eq!(
            gate(&r, &asker(Via::Nearby)),
            Gate::Refuse(Refusal::NotListed)
        );
    }

    #[test]
    fn locked_full_blocked_and_old() {
        let r = RoomNow {
            locked: true,
            ..room()
        };
        // Locked: every path, invites included.
        assert_eq!(gate(&r, &asker(Via::Invite)), Gate::Refuse(Refusal::Locked));
        let r = RoomNow {
            guests_in: 3,
            ..room()
        };
        assert_eq!(
            gate(&r, &asker(Via::Invite)),
            Gate::Refuse(Refusal::Full { allowed: 3, in_: 3 })
        );
        assert_eq!(
            Refusal::Full { allowed: 3, in_: 3 }.json(),
            json!({"stage": "refused", "why": "full", "allowed": 3, "in": 3})
        );
        // Pending asks don't count against the size, but at most MAX_PENDING wait.
        let r = RoomNow {
            guests_in: 2,
            pending: 3,
            ..room()
        };
        assert_eq!(gate(&r, &asker(Via::Invite)), Gate::Ask);
        let r = RoomNow {
            pending: MAX_PENDING,
            ..room()
        };
        assert!(matches!(
            gate(&r, &asker(Via::Invite)),
            Gate::Refuse(Refusal::Full { .. })
        ));
        // Blocked people hear a plain no.
        let b = Asker {
            blocked: true,
            ..asker(Via::Invite)
        };
        assert_eq!(gate(&room(), &b), Gate::Refuse(Refusal::No));
        assert_eq!(Refusal::No.word(), "denied");
        // Two versions back still plays; three is told to update.
        assert_eq!(
            gate(
                &room(),
                &Asker {
                    proto: None,
                    ..asker(Via::Invite)
                }
            ),
            Gate::Ask
        );
        assert!(proto_ok(Some(PROTO - PROTO_WINDOW)));
        assert!(!proto_ok_at(Some(1), 4));
        assert!(proto_ok_at(Some(2), 4));
    }

    #[test]
    fn passwords_and_approvals() {
        let r = RoomNow {
            visibility: Visibility::Public,
            password_set: true,
            ..room()
        };
        // Invites never need the password.
        assert_eq!(gate(&r, &asker(Via::Invite)), Gate::Ask);
        let with = |pw| Asker {
            password: pw,
            ..asker(Via::Rooms)
        };
        assert_eq!(
            gate(&r, &with(PwCheck::Missing)),
            Gate::Refuse(Refusal::Password(None))
        );
        assert_eq!(
            gate(&r, &with(PwCheck::Wrong(4))),
            Gate::Refuse(Refusal::Password(Some(4)))
        );
        assert_eq!(
            gate(&r, &with(PwCheck::LockedOut(600))),
            Gate::Refuse(Refusal::LockedOut(600))
        );
        assert_eq!(gate(&r, &with(PwCheck::Ok)), Gate::Ask);
        // "The password lets people in without asking".
        let auto = RoomNow {
            approval: Approval {
                password: true,
                ..Approval::default()
            },
            ..r
        };
        assert_eq!(gate(&auto, &with(PwCheck::Ok)), Gate::LetIn);
        // Friends get in without asking by default; strangers from Rooms never do by that.
        let friend = Asker {
            friend: true,
            ..asker(Via::Again)
        };
        assert_eq!(gate(&room(), &friend), Gate::LetIn);
        let off = RoomNow {
            approval: Approval {
                friends: false,
                ..Approval::default()
            },
            ..room()
        };
        assert_eq!(gate(&off, &friend), Gate::Ask);
        let stranger = Asker {
            friend: true,
            ..asker(Via::Rooms)
        };
        let public = RoomNow {
            visibility: Visibility::Public,
            ..room()
        };
        assert_eq!(gate(&public, &stranger), Gate::Ask);
        // Household PS5s only when the host opted in.
        let home = Asker {
            household: true,
            ..asker(Via::Nearby)
        };
        assert_eq!(gate(&room(), &home), Gate::Ask);
        let opted = RoomNow {
            approval: Approval {
                household: true,
                ..Approval::default()
            },
            ..room()
        };
        assert_eq!(gate(&opted, &home), Gate::LetIn);
    }

    #[test]
    fn host_away_refuses_asks_but_auto_still_admits() {
        let r = RoomNow {
            host_away: true,
            ..room()
        };
        assert_eq!(
            gate(&r, &asker(Via::Invite)),
            Gate::Refuse(Refusal::HostAway)
        );
        let friend = Asker {
            friend: true,
            ..asker(Via::Again)
        };
        assert_eq!(gate(&r, &friend), Gate::LetIn);
    }

    #[test]
    fn settings_keep_only_what_differs_and_read_field_by_field() {
        assert_eq!(RoomCfg::default().stored(), None);
        let c = RoomCfg {
            name: Some("Couch chaos".into()),
            visibility: Visibility::Friends,
            ..RoomCfg::default()
        };
        let s = c.stored().unwrap();
        assert_eq!(s, json!({"name": "Couch chaos", "visibility": "friends"}));
        assert_eq!(RoomCfg::from_stored(Some(&s)), c);
        // A broken field keeps its default; the others load.
        let bad = json!({"name": "ab", "visibility": "secret", "watchers": 9, "lang": "xx",
                         "region": "mars", "nearby": false});
        let r = RoomCfg::from_stored(Some(&bad));
        assert_eq!(
            r,
            RoomCfg {
                nearby: false,
                ..RoomCfg::default()
            }
        );
        assert_eq!(RoomCfg::from_stored(None), RoomCfg::default());
        // Defaults by visibility.
        let p = RoomCfg {
            visibility: Visibility::Public,
            ..RoomCfg::default()
        };
        assert_eq!(
            (p.chat(), p.voice(), p.start_muted()),
            (ChatMode::Quick, VoiceMode::Ptt, true)
        );
        assert_eq!(
            (
                RoomCfg::default().chat(),
                RoomCfg::default().voice(),
                RoomCfg::default().start_muted()
            ),
            (ChatMode::On, VoiceMode::Open, false)
        );
        assert_eq!(RoomCfg::default().name_for("Sam"), "Sam's room");
        assert_eq!(RoomCfg::default().name_for(""), "Friend's room");
    }

    #[test]
    fn names_follow_the_rules() {
        assert_eq!(room_name("  Couch  chaos ").unwrap(), "Couch chaos");
        assert!(room_name("ab").is_err());
        assert!(room_name("PS5Upload night").is_err());
        assert_eq!(guest_name("Sam"), "Sam");
        assert_eq!(guest_name("  "), "");
        assert_eq!(guest_name("visit www site"), "");
        assert_eq!(guest_name(&"x".repeat(40)).chars().count(), 24);
        assert_eq!(default_name(&"N".repeat(40)).chars().count(), 31);
    }

    #[test]
    fn wrong_passwords_lock_out_after_five() {
        let mut t = PwTries::default();
        let now = Instant::now();
        for left in (1..PW_TRIES).rev() {
            assert_eq!(t.wrong("ip:a", now), PwCheck::Wrong(left));
        }
        assert_eq!(
            t.wrong("ip:a", now),
            PwCheck::LockedOut(PW_LOCKOUT.as_secs())
        );
        assert!(t.wait("ip:a", now).is_some());
        assert_eq!(t.wait("ip:b", now), None, "another address may still try");
        assert_eq!(t.wait("ip:a", now + PW_LOCKOUT), None, "the wait ends");
        // 20 wrong in a room pause password knocks for everyone.
        let mut t = PwTries::default();
        for i in 0..PW_ROOM_TRIES {
            t.wrong(&format!("ip:{i}"), now);
        }
        assert!(t.warned);
        assert!(t.wait("ip:new", now).is_some());
    }

    #[test]
    fn preview_never_holds_addresses_or_tags_and_follows_the_switches() {
        let p = PreviewIn {
            host_name: "Sam".into(),
            host_tag: "4F2A".into(),
            cfg: RoomCfg::default(),
            size: 4,
            password: true,
            game: Some(Game {
                title: "ASTRO BOT".into(),
                id: "PPSA01234".into(),
                picked: false,
            }),
            stream: Some(
                json!({"res": "1080p", "fps": 60, "codec": "hevc", "kbps": 15000, "hdr": false}),
            ),
            host_link: Some((2, "excellent")),
            players: vec![
                PreviewPlayer {
                    name: "Sam".into(),
                    seat: 1,
                    kind: "host",
                    tier: None,
                    rtt_ms: None,
                },
                PreviewPlayer {
                    name: "Mia".into(),
                    seat: 2,
                    kind: "ps5",
                    tier: Some("good"),
                    rtt_ms: Some(31),
                },
            ],
            asking: 1,
            open_for: Duration::from_secs(42 * 60 + 5),
            watchers: 1,
            locked: false,
        };
        let v = preview(&p);
        assert_eq!(v["proto"], PROTO);
        assert_eq!(v["host"]["playing_for_min"], 42);
        assert_eq!(v["players"][1]["name"], "Mia");
        assert_eq!(v["players"][1]["rtt_ms"], 31);
        assert_eq!(v["room"]["name"], "Sam's room");
        assert_eq!(v["stream"]["codec"], "hevc");
        let text = v.to_string();
        for never in ["192.168", "\"lan\"", "tailscale", "internet", "tag_sha"] {
            assert!(!text.contains(never), "{never} in {text}");
        }
        // The host hides names, pings, stream and time.
        let hidden = PreviewIn {
            cfg: RoomCfg {
                preview: PreviewShows {
                    names: false,
                    pings: false,
                    stream: false,
                    time: false,
                    check: false,
                },
                ..RoomCfg::default()
            },
            ..p
        };
        let v = preview(&hidden);
        assert!(v["players"][1].get("name").is_none());
        assert!(v["players"][1].get("rtt_ms").is_none());
        assert!(v["stream"].is_null());
        assert!(v["host"]["playing_for_min"].is_null());
    }

    #[test]
    fn preview_limits_by_address_and_in_all() {
        let mut l = PreviewLimit::default();
        let now = Instant::now();
        let a: IpAddr = "192.168.1.20".parse().unwrap();
        assert!(l.take(a, now).is_ok());
        assert!(l.take(a, now + Duration::from_secs(3)).is_err());
        assert!(l.take(a, now + PREVIEW_GAP).is_ok());
        for i in 0..PREVIEW_PER_MIN - 2 {
            let ip: IpAddr = format!("10.0.{}.1", i + 1).parse().unwrap();
            assert!(l.take(ip, now + PREVIEW_GAP).is_ok());
        }
        let late: IpAddr = "10.9.9.9".parse().unwrap();
        assert!(l.take(late, now + PREVIEW_GAP).is_err());
    }

    #[test]
    fn ads_only_for_friends_and_public() {
        let pk = "A".repeat(87);
        assert!(ad(
            &RoomCfg::default(),
            "Sam",
            "4F2A",
            None,
            0,
            4,
            false,
            false,
            &pk
        )
        .is_none());
        let c = RoomCfg {
            visibility: Visibility::Friends,
            ..RoomCfg::default()
        };
        let a = ad(&c, "Sam", "4F2A", None, 1, 4, true, false, &pk).unwrap();
        assert_eq!(a["listed"], false);
        assert_eq!(a["slots"], json!([2, 4]));
        assert_eq!(a["proto"], PROTO);
        assert_eq!(a["region"], "auto");
        let p = RoomCfg {
            visibility: Visibility::Public,
            ..c
        };
        assert_eq!(
            ad(&p, "Sam", "4F2A", None, 1, 4, true, false, &pk).unwrap()["listed"],
            true
        );
    }

    #[test]
    fn device_tags_are_short_and_checked() {
        let (tag, h) = device_tag(Some("abcdefghijklmnopqrstuv")).unwrap();
        assert_eq!(tag.len(), 4);
        assert_eq!(h.len(), 64);
        assert!(device_tag(Some("short")).is_none());
        assert!(device_tag(Some("has spaces in it!!")).is_none());
        assert!(device_tag(None).is_none());
    }

    #[test]
    fn blocks_match_by_tag_address_or_cid() {
        let b = Block {
            tag_sha256: Some("h1".into()),
            tag: Some("ABCD".into()),
            addr: Some("203.0.113.5".parse().unwrap()),
            cid: Some("cid1".into()),
            name: "Lee".into(),
            at_unix: 0,
            always: false,
        };
        let w = |t: Option<&str>, a: Option<&str>, c: Option<&str>| Who {
            tag_sha256: t.map(str::to_string),
            addr: a.map(|a| a.parse().unwrap()),
            cid: c.map(str::to_string),
        };
        assert!(b.matches(&w(Some("h1"), None, None)));
        assert!(
            b.matches(&w(Some("new"), Some("203.0.113.5"), None)),
            "a reset tag"
        );
        assert!(b.matches(&w(None, None, Some("cid1"))));
        assert!(!b.matches(&w(Some("h2"), Some("203.0.113.6"), Some("cid2"))));
        assert!(tag_is(&b, "abcd"));
    }

    #[test]
    fn the_bus_drops_what_the_host_muted() {
        static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        fn h(_: &bus::From, m: &bus::FromGuest) {
            HEARD.lock().unwrap().push(format!("{m:?}"));
        }
        bus::on_guest(h);
        let from = bus::From {
            room: "bus-test".into(),
            guest: 7,
            player: 2,
            name: "Mia".into(),
        };
        // The core's own messages never go to handlers.
        assert!(bus::from_guest(&from, bus::FromGuest::Text(json!({"ping": 1}))).is_none());
        bus::from_guest(&from, bus::FromGuest::Text(json!({"chat": {"quick": 3}})));
        bus::set_muted(
            "bus-test",
            7,
            Muted {
                voice: true,
                chat: true,
            },
        );
        let said = bus::from_guest(&from, bus::FromGuest::Text(json!({"chat": {"quick": 3}})));
        assert_eq!(said, Some(json!({"muted": {"voice": true, "chat": true}})));
        assert!(bus::from_guest(&from, bus::FromGuest::Voice(vec![6, 0, 1])).is_none());
        let heard: Vec<String> = HEARD
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.contains("quick") || s.contains("Voice"))
            .cloned()
            .collect();
        assert_eq!(heard.len(), 1, "{heard:?}");
        bus::forget_room("bus-test");
        assert_eq!(bus::muted("bus-test", 7), Muted::default());
    }

    #[test]
    fn caps_and_extra_caps() {
        bus::add_caps(&["chat"]);
        assert!(caps().iter().any(|c| c == "chat"));
        assert!(caps().iter().any(|c| c == "preview"));
    }
}
