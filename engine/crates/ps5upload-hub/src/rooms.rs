//! Play Rooms on the hub: room cards, the public list, the friends lookup, relayed previews and
//! knocks (design: play-rooms section 9; wire contract: `PROTOCOL.md`, "Rooms").
//!
//! Everything is in memory and bounded. A card lives on its host engine's lobby socket and dies
//! with it; the list is fetched on demand, cached per query shape and paged by bytes; previews
//! reach a host at most once per room per `preview_ttl`; knocks are counted, never read (the
//! password inside is sealed to the host engine's key: opaque bytes, size-capped). No response
//! ever holds an address, and no log line holds anything a client sent.
//!
//! Protection is layered cheapest first: blocklist and per-client token buckets
//! (`throttle.rs`, sized for people) before parsing, parsing before the one-hash proof of work,
//! the cache before any scan; under global load the hub goes into cut-back mode on its own; the
//! kill switches and the blocklist change at run time without a redeploy.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{ConnectInfo, Path, RawQuery, State as AxState};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use serde_json::value::RawValue;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::config::{Cidr, RoomsConfig};
use crate::geo::{country_region, est_ms, refine, Geo, Region};
use crate::hub::{client_ip, is_b64url, random_b64, Out, State, GID_LEN, ROOM_LEN};
use crate::limits::IpKey;
use crate::names::clean_name;
use crate::throttle::{Bucket, Class, Throttle, Verdict};
use crate::{Field, Shared};

/// What a lobby host is told the hub can do (`hosted.caps`).
pub const HUB_CAPS: [&str; 5] = ["ads", "cid", "knock", "preview", "status"];

/// What the proof of work hashes before the nonce and the answer.
const POW_DOMAIN: &[u8] = b"ps5upload-hub ad\0";
/// What the per-room client id hashes before the room.
const CID_DOMAIN: &[u8] = b"ps5upload-hub cid\0";
/// Bytes a response costs on the wire beyond its body (headers, TLS, TCP): an estimate used for
/// the traffic budget.
const HTTP_OVERHEAD: u64 = 160;
const WS_OVERHEAD: u64 = 30;
/// The 21 app languages a card may name.
const LANGS: [&str; 21] = [
    "ar", "bn", "de", "en", "es", "fa", "fr", "hi", "hu", "id", "it", "ja", "ko", "pl", "pt-BR",
    "ru", "th", "tr", "vi", "zh-CN", "zh-TW",
];

// ── Counters ────────────────────────────────────────────────────────────────────────────────────

/// Counters by a fixed set of names (labels are fixed strings: nothing a client sent).
pub struct Counters<const N: usize> {
    names: [&'static str; N],
    vals: [AtomicU64; N],
}

impl<const N: usize> Counters<N> {
    const fn new(names: [&'static str; N]) -> Self {
        Self {
            names,
            vals: [const { AtomicU64::new(0) }; N],
        }
    }

    pub fn add(&self, name: &'static str, n: u64) {
        match self.names.iter().position(|x| *x == name) {
            Some(i) => self.vals[i].fetch_add(n, Ordering::Relaxed),
            None => {
                debug_assert!(false, "unknown counter {name}");
                0
            }
        };
    }

    pub fn inc(&self, name: &'static str) {
        self.add(name, 1);
    }

    pub fn get(&self, name: &str) -> u64 {
        self.names
            .iter()
            .position(|x| *x == name)
            .map(|i| self.vals[i].load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub fn total(&self) -> u64 {
        self.vals.iter().map(|v| v.load(Ordering::Relaxed)).sum()
    }

    fn render(&self, s: &mut String, metric: &str, label: &str, help: &str) {
        use std::fmt::Write as _;
        let _ = writeln!(s, "# HELP {metric} {help}\n# TYPE {metric} counter");
        for (i, n) in self.names.iter().enumerate() {
            let _ = writeln!(
                s,
                "{metric}{{{label}=\"{n}\"}} {}",
                self.vals[i].load(Ordering::Relaxed)
            );
        }
    }
}

/// Rooms counters (`/metrics`, next to the hub's own).
pub struct RoomMetrics {
    pub ads: Counters<3>,
    pub ads_removed: Counters<7>,
    pub ads_refused: Counters<11>,
    pub list: Counters<5>,
    pub status: Counters<4>,
    pub previews: Counters<8>,
    pub knocks: Counters<7>,
    pub out_bytes: Counters<6>,
    pub throttled: Counters<6>,
    /// Test hook: a resident size to use instead of the real one (0: the real one).
    mem_override: AtomicU64,
}

impl Default for RoomMetrics {
    fn default() -> Self {
        Self {
            ads: Counters::new(["created", "updated", "renewed"]),
            ads_removed: Counters::new([
                "unad", "closed", "expired", "stale", "off", "blocked", "old",
            ]),
            ads_refused: Counters::new([
                "name", "game", "limit", "busy", "pow", "off", "blocked", "bad", "old", "rate",
                "no_lobby",
            ]),
            list: Counters::new(["hit", "miss", "limited", "off", "blocked"]),
            status: Counters::new(["ok", "limited", "off", "blocked"]),
            previews: Counters::new([
                "relayed", "answered", "cached", "timeout", "off", "limited", "none", "refused",
            ]),
            knocks: Counters::new([
                "forwarded",
                "pending",
                "client",
                "ip",
                "not_approved",
                "off",
                "bad",
            ]),
            out_bytes: Counters::new(["list", "status", "preview", "ws", "admin", "refused"]),
            throttled: Counters::new(["list", "status", "preview", "knock", "connect", "socket"]),
            mem_override: AtomicU64::new(0),
        }
    }
}

impl RoomMetrics {
    /// Counts `body` bytes of `class` going out, plus the wire overhead of one message.
    pub fn out(&self, class: &'static str, body: usize) {
        let over = if class == "ws" {
            WS_OVERHEAD
        } else {
            HTTP_OVERHEAD
        };
        self.out_bytes.add(class, body as u64 + over);
    }

    /// Tests: pretend the process holds `bytes` (0: measure it).
    pub fn set_resident_for_test(&self, bytes: u64) {
        self.mem_override.store(bytes, Ordering::Relaxed);
    }

    fn resident(&self) -> Option<u64> {
        match self.mem_override.load(Ordering::Relaxed) {
            0 => resident_bytes(),
            n => Some(n),
        }
    }
}

/// The process's resident memory (Linux; elsewhere unknown, so no memory-based cut-back).
pub fn resident_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * 4096)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

// ── Control: kill switches and the blocklist ────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutbackMode {
    Auto,
    On,
    Off,
}

/// Control lines as read (env, file or admin): every field optional, lists added together.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ControlInput {
    pub rooms: Option<bool>,
    pub preview: Option<bool>,
    pub knocks: Option<bool>,
    pub min_proto: Option<u32>,
    pub latest_proto: Option<u32>,
    pub app_latest: Option<String>,
    pub cutback: Option<CutbackMode>,
    pub tags: Vec<String>,
    pub ips: Vec<Cidr>,
    pub words: Vec<String>,
}

/// Parses control lines (`key value`, separated by new lines or `;`, `#` comments):
/// `rooms on|off`, `preview on|off`, `knocks on|off`, `min_proto N`, `latest_proto N`,
/// `app_latest 13.0.0`, `cutback auto|on|off`, `tag 4F2A`, `ip 203.0.113.0/24`, `word xyz`.
/// Returns the input and how many lines were not understood (never their text).
pub fn parse_control(text: &str) -> (ControlInput, usize) {
    let mut c = ControlInput::default();
    let mut bad = 0;
    let flag = |v: &str| match v {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    };
    for line in text.split(['\n', ';']) {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (k, v) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let v = v.trim();
        let ok = match k.to_ascii_lowercase().as_str() {
            "rooms" => flag(v).map(|f| c.rooms = Some(f)).is_some(),
            "preview" => flag(v).map(|f| c.preview = Some(f)).is_some(),
            "knocks" => flag(v).map(|f| c.knocks = Some(f)).is_some(),
            "min_proto" => v.parse().map(|n| c.min_proto = Some(n)).is_ok(),
            "latest_proto" => v.parse().map(|n| c.latest_proto = Some(n)).is_ok(),
            "app_latest" => {
                let ok = valid_ver(v);
                if ok {
                    c.app_latest = Some(v.to_string());
                }
                ok
            }
            "cutback" => {
                let m = match v {
                    "auto" => Some(CutbackMode::Auto),
                    "on" => Some(CutbackMode::On),
                    "off" => Some(CutbackMode::Off),
                    _ => None,
                };
                m.map(|m| c.cutback = Some(m)).is_some()
            }
            "tag" => {
                let ok = v.len() == 4 && v.bytes().all(|b| b.is_ascii_hexdigit());
                if ok {
                    c.tags.push(v.to_ascii_uppercase());
                }
                ok
            }
            "ip" => v.parse::<Cidr>().map(|x| c.ips.push(x)).is_ok(),
            "word" => {
                let ok = !v.is_empty() && v.chars().count() <= 32;
                if ok {
                    c.words.push(v.to_lowercase());
                }
                ok
            }
            _ => false,
        };
        if !ok {
            bad += 1;
        }
    }
    (c, bad)
}

/// The switches in force: the config, then `HUB_BLOCKLIST`, then the run-time overlay.
#[derive(Clone, Debug)]
pub struct Control {
    pub rooms: bool,
    pub preview: bool,
    pub knocks: bool,
    pub min_proto: u32,
    pub latest_proto: u32,
    pub app_latest: Option<String>,
    pub cutback: CutbackMode,
    pub tags: HashSet<String>,
    pub ips: Vec<Cidr>,
    pub words: Vec<String>,
}

impl Control {
    pub fn merge(cfg: &RoomsConfig, base: &ControlInput, over: Option<&ControlInput>) -> Control {
        let mut c = Control {
            rooms: cfg.enabled,
            preview: cfg.previews,
            knocks: cfg.knocks,
            min_proto: cfg.min_proto,
            latest_proto: cfg.latest_proto,
            app_latest: cfg.app_latest.clone(),
            cutback: CutbackMode::Auto,
            tags: HashSet::new(),
            ips: Vec::new(),
            words: Vec::new(),
        };
        for i in std::iter::once(base).chain(over) {
            if let Some(v) = i.rooms {
                c.rooms = v;
            }
            if let Some(v) = i.preview {
                c.preview = v;
            }
            if let Some(v) = i.knocks {
                c.knocks = v;
            }
            if let Some(v) = i.min_proto {
                c.min_proto = v;
            }
            if let Some(v) = i.latest_proto {
                c.latest_proto = v;
            }
            if let Some(v) = &i.app_latest {
                c.app_latest = Some(v.clone());
            }
            if let Some(v) = i.cutback {
                c.cutback = v;
            }
            c.tags.extend(i.tags.iter().cloned());
            c.ips.extend(i.ips.iter().copied());
            c.words.extend(i.words.iter().cloned());
        }
        c.latest_proto = c.latest_proto.max(c.min_proto);
        c
    }

    pub fn ip_blocked(&self, key: IpKey) -> bool {
        if self.ips.is_empty() {
            return false;
        }
        let addr = key_addr(key);
        self.ips.iter().any(|c| c.contains(addr))
    }
}

/// The address an [`IpKey`] stands for (an IPv6 /64 as its first address).
fn key_addr(key: IpKey) -> IpAddr {
    match key {
        IpKey::V4(v) => IpAddr::V4(v.into()),
        IpKey::V6Prefix(p) => IpAddr::V6((u128::from(p) << 64).into()),
    }
}

// ── Cards ───────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegionPref {
    /// The host hid it.
    Hidden,
    /// From the connection's country, refined by the engine's own (time-zone) region if given.
    Auto(Option<Region>),
}

/// A checked card: only these fields ever reach the list.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub listed: bool,
    pub locked: bool,
    pub name: String,
    pub host: String,
    pub tag: String,
    pub game: Option<(String, String)>,
    pub proto: u32,
    pub ver: String,
    pub caps: Vec<String>,
    pub slots: (u8, u8),
    pub pw: bool,
    pub lang: Option<&'static str>,
    region: RegionPref,
    pub pk: Option<String>,
}

pub(crate) struct Ad {
    conn: u64,
    owner: IpKey,
    pub card: Card,
    /// Shown region, worked out at create (the address is not kept for it).
    region: Option<Region>,
    from_country: Option<Region>,
    created: Instant,
    created_unix: u64,
    renewed: Instant,
    updates: Bucket,
}

fn valid_ver(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    v.len() <= 16
        && parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 4 && p.bytes().all(|b| b.is_ascii_digit()))
}

fn valid_title_id(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 9
        && b[..4].iter().all(u8::is_ascii_uppercase)
        && b[4..].iter().all(u8::is_ascii_digit)
}

/// A game title from the console: printable, at most 64 characters, no links or markup.
fn clean_title(t: &str) -> Option<String> {
    let t = t.trim();
    let n = t.chars().count();
    if n == 0 || n > 64 {
        return None;
    }
    if t.chars().any(|c| {
        c.is_control() || matches!(c, '<' | '>' | '@' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
    }) {
        return None;
    }
    let l = t.to_lowercase();
    if l.contains("://") || l.contains("www.") {
        return None;
    }
    Some(t.to_string())
}

/// Checks an engine's card; the error is the `ad_refused` reason.
pub fn parse_card(v: &Value, words: &[String]) -> Result<Card, &'static str> {
    let o = v.as_object().ok_or("bad")?;
    let boolean = |k: &str, d: bool| -> Result<bool, &'static str> {
        match o.get(k) {
            None => Ok(d),
            Some(Value::Bool(b)) => Ok(*b),
            _ => Err("bad"),
        }
    };
    let text = |k: &str| o.get(k).and_then(Value::as_str);
    let listed = boolean("listed", true)?;
    let locked = boolean("locked", false)?;
    let pw = boolean("pw", false)?;
    let name = clean_name(text("name").ok_or("name")?, 3, 32, words).map_err(|_| "name")?;
    let host = clean_name(text("host").ok_or("name")?, 1, 24, words).map_err(|_| "name")?;
    let tag = text("tag").ok_or("bad")?;
    if tag.len() != 4 || !tag.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("bad");
    }
    let game = match o.get("game") {
        None | Some(Value::Null) => None,
        Some(Value::Object(g)) => {
            let id = g.get("id").and_then(Value::as_str).ok_or("game")?;
            let title = g.get("title").and_then(Value::as_str).ok_or("game")?;
            if !valid_title_id(id) {
                return Err("game");
            }
            Some((clean_title(title).ok_or("game")?, id.to_string()))
        }
        _ => return Err("game"),
    };
    let proto = o
        .get("proto")
        .and_then(Value::as_u64)
        .filter(|p| (1..=999).contains(p))
        .ok_or("bad")? as u32;
    let ver = text("ver")
        .filter(|v| valid_ver(v))
        .ok_or("bad")?
        .to_string();
    let caps = match o.get("caps") {
        None => Vec::new(),
        Some(Value::Array(a)) if a.len() <= 16 => a
            .iter()
            .map(|c| {
                c.as_str()
                    .filter(|s| {
                        !s.is_empty()
                            && s.len() <= 16
                            && s.bytes()
                                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                    })
                    .map(str::to_string)
                    .ok_or("bad")
            })
            .collect::<Result<_, _>>()?,
        _ => return Err("bad"),
    };
    let slots = match o.get("slots").and_then(Value::as_array).map(Vec::as_slice) {
        Some([a, b]) => {
            let (a, b) = (a.as_u64().ok_or("bad")?, b.as_u64().ok_or("bad")?);
            if !(1..=8).contains(&b) || a > b {
                return Err("bad");
            }
            (a as u8, b as u8)
        }
        _ => return Err("bad"),
    };
    let lang = match o.get("lang") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(*LANGS.iter().find(|l| **l == s).ok_or("bad")?),
        _ => return Err("bad"),
    };
    let region = match o.get("region") {
        None | Some(Value::Null) => RegionPref::Auto(None),
        Some(Value::String(s)) if s == "hidden" => RegionPref::Hidden,
        Some(Value::String(s)) if s == "auto" => RegionPref::Auto(None),
        Some(Value::String(s)) => RegionPref::Auto(Some(Region::parse(s).ok_or("bad")?)),
        _ => return Err("bad"),
    };
    let pk = match o.get("pk") {
        None | Some(Value::Null) => None,
        Some(Value::String(s))
            if (40..=100).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') =>
        {
            Some(s.clone())
        }
        _ => return Err("bad"),
    };
    Ok(Card {
        listed,
        locked,
        name,
        host,
        tag: tag.to_ascii_uppercase(),
        game,
        proto,
        ver,
        caps,
        slots,
        pw,
        lang,
        region,
        pk,
    })
}

// ── Proof of work and client ids ────────────────────────────────────────────────────────────────

fn leading_zero_bits(h: &[u8]) -> u32 {
    let mut n = 0;
    for b in h {
        if *b == 0 {
            n += 8;
            continue;
        }
        n += b.leading_zeros();
        break;
    }
    n
}

/// True when SHA-256(domain ‖ nonce ‖ answer) starts with `bits` zero bits.
pub fn pow_ok(nonce: &str, answer: &str, bits: u32) -> bool {
    let h = Sha256::new()
        .chain_update(POW_DOMAIN)
        .chain_update(nonce.as_bytes())
        .chain_update(answer.as_bytes())
        .finalize();
    leading_zero_bits(&h) >= bits
}

/// An answer for [`pow_ok`] (what an engine does; decimal counters).
pub fn solve_pow(nonce: &str, bits: u32) -> String {
    (0u64..)
        .map(|i| i.to_string())
        .find(|a| pow_ok(nonce, a, bits))
        .expect("an answer exists")
}

fn hmac(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for (i, k) in key.iter().enumerate() {
        ipad[i] ^= k;
        opad[i] ^= k;
    }
    let mut inner = Sha256::new().chain_update(ipad);
    for p in parts {
        inner.update(p);
    }
    Sha256::new()
        .chain_update(opad)
        .chain_update(inner.finalize())
        .finalize()
        .into()
}

/// A client's id in one room: 8 base64url characters of a keyed hash of the room and the client
/// key (IPv4 or IPv6 /64). Rooms see different ids for one client; nobody can get the address
/// back, and the key is new on every start.
pub(crate) fn cid(key: &[u8; 32], room: &str, ip: IpKey) -> String {
    let ipb: Vec<u8> = match ip {
        IpKey::V4(v) => [&[4u8][..], &v.to_be_bytes()].concat(),
        IpKey::V6Prefix(p) => [&[6u8][..], &p.to_be_bytes()].concat(),
    };
    let h = hmac(key, &[CID_DOMAIN, room.as_bytes(), &[0], &ipb]);
    let mut s = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&h[..6]);
    s.truncate(8);
    s
}

// ── Knocks ──────────────────────────────────────────────────────────────────────────────────────

/// A lobby guest's knock state.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Knock {
    pending: bool,
    approved: bool,
    sealed: u8,
}

/// What to do with a lobby guest's signal.
pub(crate) enum KnockVerdict {
    Forward,
    /// Not forwarded; this (non-fatal) answer goes back to the guest.
    Reply(String),
}

enum KnockKind {
    New,
    Sealed,
    Other,
    None,
}

fn knock_kind(data: &str) -> KnockKind {
    let Ok(m) = serde_json::from_str::<HashMap<String, &RawValue>>(data) else {
        return KnockKind::Other;
    };
    let Some(k) = m.get("knock") else {
        return KnockKind::None;
    };
    let t = k.get();
    if t.starts_with('{') {
        KnockKind::New
    } else if t == "\"sealed\"" {
        KnockKind::Sealed
    } else {
        KnockKind::Other
    }
}

fn soft_error(why: &str) -> String {
    json!({"t": "error", "why": why}).to_string()
}

fn slow_down(retry: u64) -> String {
    json!({"t": "error", "why": "rate_limited", "retry_s": retry}).to_string()
}

/// A lobby guest's signal: only knocks until its host lets it in (sends `{"knock":"ok"}` or any
/// signal that is not a knock answer). One pending knock per guest; knocks per client per room
/// and per client in all on gentle buckets. Too-large data before approval is a protocol error.
#[allow(clippy::too_many_arguments)]
pub(crate) fn guest_signal(
    sh: &Shared,
    st: &mut RoomsState,
    knock: &mut Knock,
    client_buckets: &mut HashMap<IpKey, Bucket>,
    bucket_cap: usize,
    ip: IpKey,
    data: &str,
    now: Instant,
) -> Result<KnockVerdict, crate::metrics::ErrorKind> {
    let cfg = &sh.config.rooms;
    let m = &sh.rooms;
    if knock.approved {
        return Ok(KnockVerdict::Forward);
    }
    if data.len() > cfg.knock_max_bytes {
        return Err(crate::metrics::ErrorKind::BadData);
    }
    match knock_kind(data) {
        KnockKind::None => {
            m.knocks.inc("not_approved");
            st.throttle.strike(ip, now);
            Ok(KnockVerdict::Reply(soft_error("not_approved")))
        }
        KnockKind::Other => {
            m.knocks.inc("bad");
            st.throttle.strike(ip, now);
            Ok(KnockVerdict::Reply(soft_error("bad_knock")))
        }
        KnockKind::Sealed => {
            if !knock.pending || knock.sealed >= 2 {
                m.knocks.inc("bad");
                return Ok(KnockVerdict::Reply(soft_error("bad_knock")));
            }
            knock.sealed += 1;
            Ok(KnockVerdict::Forward)
        }
        KnockKind::New => {
            if !st.control.knocks {
                m.knocks.inc("off");
                return Ok(KnockVerdict::Reply(soft_error("knocks_off")));
            }
            if knock.pending {
                m.knocks.inc("pending");
                return Ok(KnockVerdict::Reply(soft_error("knock_pending")));
            }
            if !client_buckets.contains_key(&ip) && client_buckets.len() >= bucket_cap {
                client_buckets.retain(|_, b| !b.is_full(cfg.knock_client, now));
            }
            let b = client_buckets
                .entry(ip)
                .or_insert_with(|| Bucket::full(cfg.knock_client, now));
            if let Err(wait) = b.take(cfg.knock_client, now) {
                m.knocks.inc("client");
                st.throttle.strike(ip, now);
                return Ok(KnockVerdict::Reply(slow_down(
                    Verdict::Wait(wait).retry_secs(),
                )));
            }
            let v = st.throttle.check(ip, Class::Knock, now);
            if !v.is_ok() {
                m.knocks.inc("ip");
                m.throttled.inc("knock");
                return Ok(KnockVerdict::Reply(slow_down(v.retry_secs())));
            }
            knock.pending = true;
            knock.sealed = 0;
            m.knocks.inc("forwarded");
            Ok(KnockVerdict::Forward)
        }
    }
}

/// The host answered a lobby guest: `{"knock":"ok"}` (or any non-knock signal, such as an offer)
/// lets it in; `{"knock":"no"}` ends its knock; `wait`/`challenge` keep it pending.
pub(crate) fn host_signal(knock: &mut Knock, data: &str) {
    let Ok(m) = serde_json::from_str::<HashMap<String, &RawValue>>(data) else {
        return;
    };
    match m.get("knock").map(|k| k.get()) {
        None | Some("\"ok\"") => {
            knock.approved = true;
            knock.pending = false;
        }
        Some("\"no\"") => knock.pending = false,
        _ => {}
    }
}

// ── State ───────────────────────────────────────────────────────────────────────────────────────

struct Cached {
    at: Instant,
    body: Arc<str>,
}

enum PreviewSlot {
    Pending {
        pid: String,
        since: Instant,
        tx: watch::Sender<Option<Arc<str>>>,
    },
    /// An answer (or none: the host did not answer in time), shared until `at + preview_ttl`.
    Done { at: Instant, body: Option<Arc<str>> },
}

/// Outbound bytes over time, for the monthly projection.
struct Traffic {
    start: Instant,
    minutes: [u64; 60],
    minute: u64,
    last_total: u64,
}

impl Traffic {
    fn new(now: Instant) -> Self {
        Self {
            start: now,
            minutes: [0; 60],
            minute: 0,
            last_total: 0,
        }
    }

    fn add(&mut self, now: Instant, total: u64) {
        let m = now.duration_since(self.start).as_secs() / 60;
        while self.minute < m {
            self.minute += 1;
            self.minutes[(self.minute % 60) as usize] = 0;
        }
        let d = total.saturating_sub(self.last_total);
        self.last_total = total;
        self.minutes[(self.minute % 60) as usize] += d;
    }

    /// Bytes a month at the rate of the last hour (at least one minute observed: a burst at start
    /// does not count as a whole month's worth).
    fn projected(&self, now: Instant) -> u64 {
        let window = now
            .duration_since(self.start)
            .as_secs_f64()
            .clamp(60.0, 3600.0);
        let bytes: u64 = self.minutes.iter().sum();
        (bytes as f64 / window * 30.0 * 86_400.0) as u64
    }
}

/// Why cut-back mode is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cutback {
    Memory,
    Traffic,
    Manual,
}

impl Cutback {
    fn as_str(self) -> &'static str {
        match self {
            Cutback::Memory => "memory",
            Cutback::Traffic => "traffic",
            Cutback::Manual => "manual",
        }
    }
}

/// The rooms part of the hub's state (inside its one mutex).
pub(crate) struct RoomsState {
    pub(crate) ads: HashMap<String, Ad>,
    ads_per_ip: HashMap<IpKey, usize>,
    creates: HashMap<IpKey, Bucket>,
    pub(crate) throttle: Throttle,
    /// Messages per WebSocket (by connection number), dropped when the connection ends.
    conn_rate: HashMap<u64, Bucket>,
    cache: HashMap<String, Cached>,
    previews: HashMap<String, PreviewSlot>,
    pub(crate) control: Control,
    base: ControlInput,
    overlay: Option<ControlInput>,
    file_seen: Option<(SystemTime, u64)>,
    pub(crate) cutback: Option<Cutback>,
    /// New sockets are refused (memory past `mem_refuse_bytes`).
    pub(crate) refuse_sockets: bool,
    traffic: Traffic,
    projected: u64,
    resident: Option<u64>,
    geo: Arc<Geo>,
    geo_failed: bool,
    key: [u8; 32],
    max_tracked: usize,
}

impl RoomsState {
    pub(crate) fn new(cfg: &RoomsConfig, max_tracked: usize) -> Self {
        let (base, _) = parse_control(&cfg.blocklist);
        let (geo, geo_failed) = match &cfg.geo_file {
            Some(p) => match Geo::load(p) {
                Ok((g, _)) => (g, false),
                Err(_) => (Geo::default(), true),
            },
            None => (Geo::default(), false),
        };
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).expect("OS randomness");
        Self {
            ads: HashMap::new(),
            ads_per_ip: HashMap::new(),
            creates: HashMap::new(),
            throttle: Throttle::new(cfg.throttle, cfg.escalation, max_tracked),
            conn_rate: HashMap::new(),
            cache: HashMap::new(),
            previews: HashMap::new(),
            control: Control::merge(cfg, &base, None),
            base,
            overlay: None,
            file_seen: None,
            cutback: None,
            refuse_sockets: false,
            traffic: Traffic::new(Instant::now()),
            projected: 0,
            resident: None,
            geo: Arc::new(geo),
            geo_failed,
            key,
            max_tracked,
        }
    }

    /// Tests and tools: a geo table in place of `HUB_GEO_FILE`.
    pub(crate) fn set_geo(&mut self, g: Geo) {
        self.geo = Arc::new(g);
    }

    pub(crate) fn cid(&self, room: &str, ip: IpKey) -> String {
        cid(&self.key, room, ip)
    }

    fn region_of(&self, ip: IpKey) -> Option<Region> {
        self.geo.country(ip).and_then(country_region)
    }

    /// One message on connection `conn`: false past its bucket (a flood, never an app).
    pub(crate) fn conn_message(&mut self, cfg: &RoomsConfig, conn: u64, now: Instant) -> bool {
        self.conn_rate
            .entry(conn)
            .or_insert_with(|| Bucket::full(cfg.conn_messages, now))
            .take(cfg.conn_messages, now)
            .is_ok()
    }

    pub(crate) fn conn_closed(&mut self, conn: u64) {
        self.conn_rate.remove(&conn);
    }

    fn remove_ad(&mut self, room: &str, why: &'static str, m: &RoomMetrics) -> Option<Ad> {
        let ad = self.ads.remove(room)?;
        if let Some(n) = self.ads_per_ip.get_mut(&ad.owner) {
            *n -= 1;
            if *n == 0 {
                self.ads_per_ip.remove(&ad.owner);
            }
        }
        self.previews.remove(room);
        m.ads_removed.inc(why);
        Some(ad)
    }

    fn effective_control(&mut self, cfg: &RoomsConfig) {
        self.control = Control::merge(cfg, &self.base, self.overlay.as_ref());
    }
}

/// The host socket `conn` of `room` is gone (closed, replaced or the room removed): its card
/// goes with it.
pub(crate) fn drop_conn(st: &mut State, sh: &Shared, room: &str, conn: Option<u64>) {
    let mine = st
        .lobby
        .ads
        .get(room)
        .is_some_and(|a| conn.is_none_or(|c| a.conn == c));
    if mine {
        st.lobby.remove_ad(room, "closed", &sh.rooms);
    }
}

/// The room is being removed: its card (if any) goes too.
pub(crate) fn drop_ad_of_room(st: &mut State, room: &str, m: &RoomMetrics) {
    st.lobby.remove_ad(room, "closed", m);
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn pow_json(nonce: &str, bits: u32) -> Value {
    json!({"n": nonce, "bits": bits})
}

/// The `hosted` reply for a lobby room.
pub(crate) fn hosted_lobby(room: &str, nonce: &str, bits: u32) -> String {
    json!({"t": "hosted", "room": room, "caps": HUB_CAPS, "lobby": true,
           "ad_pow": pow_json(nonce, bits)})
    .to_string()
}

fn refused(sh: &Shared, why: &'static str) -> Vec<String> {
    sh.rooms.ads_refused.inc(why);
    vec![json!({"t": "ad_refused", "why": why}).to_string()]
}

fn gone_text(why: &str, nonce: &str, bits: u32) -> String {
    json!({"t": "ad_gone", "why": why, "ad_pow": pow_json(nonce, bits)}).to_string()
}

/// `{"t":"ad"}` on a host socket: create (with `ad` and `pow`), update (with `ad`) or renew (no
/// `ad`). Refusals are answers (`ad_refused`), never closes.
pub(crate) fn on_ad(
    sh: &Shared,
    st: &mut State,
    room_id: &str,
    conn: u64,
    ip: IpKey,
    ad: Option<&str>,
    pow: Option<&str>,
) -> Vec<String> {
    let cfg = &sh.config.rooms;
    let m = &sh.rooms;
    let now = Instant::now();
    let Some(room) = st.rooms.get(room_id) else {
        return refused(sh, "bad");
    };
    if !room.lobby {
        return refused(sh, "no_lobby");
    }
    let nonce = room.pow.clone();
    let lobby = &mut st.lobby;
    if !lobby.control.rooms {
        return refused(sh, "off");
    }
    if lobby.control.ip_blocked(ip) {
        return refused(sh, "blocked");
    }
    // Someone else's card on this room (the previous socket of a reclaimed room): gone.
    if lobby.ads.get(room_id).is_some_and(|a| a.conn != conn) {
        lobby.remove_ad(room_id, "closed", m);
    }
    let parse = |lobby: &RoomsState, text: &str| -> Result<Card, &'static str> {
        if text.len() > cfg.ad_max_bytes {
            return Err("bad");
        }
        let v: Value = serde_json::from_str(text).map_err(|_| "bad")?;
        let card = parse_card(&v, &lobby.control.words)?;
        if card.proto < lobby.control.min_proto {
            return Err("old");
        }
        if lobby.control.tags.contains(&card.tag) {
            return Err("blocked");
        }
        Ok(card)
    };
    if let Some(existing) = lobby.ads.get_mut(room_id) {
        // Alive either way: a renewal past the rate still says the host is there.
        existing.renewed = now;
        if existing.updates.take(cfg.ad_updates, now).is_err() {
            return refused(sh, "rate");
        }
        let until = existing.created_unix + cfg.ad_max_age.as_secs();
        match ad {
            None => m.ads.inc("renewed"),
            Some(text) => {
                let card = match parse(lobby, text) {
                    Ok(c) => c,
                    Err(why) => {
                        // A card that may no longer be shown goes; a bad update keeps the old.
                        if why == "blocked" || why == "old" {
                            lobby.remove_ad(room_id, why, m);
                        }
                        return refused(sh, why);
                    }
                };
                if let Some(existing) = lobby.ads.get_mut(room_id) {
                    existing.region = shown_region(existing.from_country, card.region);
                    existing.card = card;
                }
                m.ads.inc("updated");
            }
        }
        return vec![json!({"t": "ad_ok", "until": until}).to_string()];
    }
    // Create: cheapest checks first, the hash, then the parse.
    let Some(text) = ad else {
        return refused(sh, "bad");
    };
    if lobby.cutback.is_some() || lobby.ads.len() >= cfg.max_ads {
        return refused(sh, "busy");
    }
    if lobby.ads_per_ip.get(&ip).copied().unwrap_or(0) >= cfg.max_ads_per_ip {
        return refused(sh, "limit");
    }
    let creates = per_hour(cfg.creates_per_hour);
    if lobby
        .creates
        .get(&ip)
        .is_some_and(|b| !b.clone().take(creates, now).is_ok())
    {
        return refused(sh, "limit");
    }
    match pow {
        Some(p) if p.len() <= 64 && pow_ok(&nonce, p, cfg.pow_bits) => {}
        _ => return refused(sh, "pow"),
    }
    let card = match parse(lobby, text) {
        Ok(c) => c,
        Err(why) => return refused(sh, why),
    };
    if !lobby.creates.contains_key(&ip) && lobby.creates.len() >= lobby.max_tracked {
        let mt = creates;
        lobby.creates.retain(|_, b| !b.is_full(mt, now));
    }
    let _ = lobby
        .creates
        .entry(ip)
        .or_insert_with(|| Bucket::full(creates, now))
        .take(creates, now);
    let from_country = lobby.region_of(ip);
    let created_unix = now_unix();
    let until = created_unix + cfg.ad_max_age.as_secs();
    lobby.ads.insert(
        room_id.to_string(),
        Ad {
            conn,
            owner: ip,
            region: shown_region(from_country, card.region),
            from_country,
            card,
            created: now,
            created_unix,
            renewed: now,
            updates: Bucket::full(cfg.ad_updates, now),
        },
    );
    *lobby.ads_per_ip.entry(ip).or_insert(0) += 1;
    m.ads.inc("created");
    // The nonce is spent: the next create (after this card ends) needs a new one, sent then.
    if let Some(r) = st.rooms.get_mut(room_id) {
        r.pow = random_b64(16);
    }
    vec![json!({"t": "ad_ok", "until": until}).to_string()]
}

/// `n` at once, refilled at `n` an hour.
fn per_hour(n: u32) -> crate::throttle::Rule {
    crate::throttle::Rule::per_min(f64::from(n), f64::from(n) / 60.0)
}

fn shown_region(from_country: Option<Region>, pref: RegionPref) -> Option<Region> {
    match pref {
        RegionPref::Hidden => None,
        RegionPref::Auto(own) => refine(from_country, own),
    }
}

/// `{"t":"unad"}`: the card goes now.
pub(crate) fn on_unad(sh: &Shared, st: &mut State, room_id: &str, conn: u64) -> Vec<String> {
    let bits = sh.config.rooms.pow_bits;
    let mine = st.lobby.ads.get(room_id).is_some_and(|a| a.conn == conn);
    if mine {
        st.lobby.remove_ad(room_id, "unad", &sh.rooms);
    }
    let nonce = st
        .rooms
        .get(room_id)
        .map(|r| r.pow.clone())
        .unwrap_or_default();
    vec![gone_text("unad", &nonce, bits)]
}

/// `{"t":"preview","pid","data"}` from a host: the answer to a relayed preview.
pub(crate) fn on_preview_answer(
    sh: &Shared,
    st: &mut State,
    room_id: &str,
    pid: &str,
    data: Option<&str>,
) -> Vec<String> {
    let cfg = &sh.config.rooms;
    let m = &sh.rooms;
    let now = Instant::now();
    let waiting = matches!(st.lobby.previews.get(room_id),
        Some(PreviewSlot::Pending { pid: want, .. }) if want == pid);
    if !waiting {
        return Vec::new();
    }
    let ok =
        data.filter(|d| d.starts_with('{') && d.len() <= cfg.preview_max_bytes && !has_address(d));
    let body: Option<Arc<str>> = ok.map(Arc::from);
    if body.is_some() {
        m.previews.inc("answered");
    } else {
        m.previews.inc("refused");
    }
    let old = st.lobby.previews.insert(
        room_id.to_string(),
        PreviewSlot::Done {
            at: now,
            body: body.clone(),
        },
    );
    if let Some(PreviewSlot::Pending { tx, .. }) = old {
        let _ = tx.send(Some(body.unwrap_or_else(|| Arc::from(""))));
    }
    Vec::new()
}

/// True when `text` holds something shaped like an IP address (a preview must never carry one;
/// the host engine leaves them out, the hub refuses it if one slips through).
pub fn has_address(text: &str) -> bool {
    let v4 = text
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .any(|t| {
            t.matches('.').count() == 3 && t.trim_matches('.').parse::<std::net::Ipv4Addr>().is_ok()
        });
    if v4 {
        return true;
    }
    text.split(|c: char| !(c.is_ascii_hexdigit() || c == ':' || c == '.'))
        .filter(|t| t.matches(':').count() >= 2)
        .any(|t| t.parse::<std::net::Ipv6Addr>().is_ok())
}

/// The sweeper's work for rooms, every `tick`: cards past their age or renewal, the switches and
/// the blocklist, budgets and cut-back mode, caches and buckets.
pub(crate) async fn sweeper(sh: Arc<Shared>) {
    let cfg = &sh.config.rooms;
    let mut tick = tokio::time::interval(cfg.tick);
    let mut shutdown = sh.shutdown.subscribe();
    let mut logged_geo = false;
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = shutdown.wait_for(|v| *v) => return,
        }
        let file = read_control_file(cfg);
        let resident = sh.rooms.resident();
        let mut st = sh.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.lobby.geo_failed && !logged_geo {
            logged_geo = true;
            sh.log("geo_file_unreadable", &[]);
        }
        if let Some((seen, text)) = file {
            if st.lobby.file_seen != Some(seen) {
                st.lobby.file_seen = Some(seen);
                let (input, bad) = parse_control(&text);
                apply_overlay(&sh, &mut st, Some(input), bad, "file");
            }
        }
        tick_rooms(&sh, &mut st, resident, Instant::now());
    }
}

fn read_control_file(cfg: &RoomsConfig) -> Option<((SystemTime, u64), String)> {
    let p = cfg.control_file.as_ref()?;
    let md = std::fs::metadata(p).ok()?;
    let seen = (md.modified().ok()?, md.len());
    if md.len() > 1 << 20 {
        return None;
    }
    Some((seen, std::fs::read_to_string(p).ok()?))
}

/// A new run-time overlay (control file or admin); applied at once.
fn apply_overlay(
    sh: &Shared,
    st: &mut State,
    input: Option<ControlInput>,
    bad: usize,
    from: &'static str,
) {
    let cfg = &sh.config.rooms;
    st.lobby.overlay = input;
    st.lobby.effective_control(cfg);
    let c = &st.lobby.control;
    sh.log(
        "control",
        &[
            ("from", Field::S(from)),
            ("rooms", Field::S(if c.rooms { "on" } else { "off" })),
            ("preview", Field::S(if c.preview { "on" } else { "off" })),
            ("knocks", Field::S(if c.knocks { "on" } else { "off" })),
            ("min_proto", Field::N(c.min_proto.into())),
            ("tags", Field::N(c.tags.len() as u64)),
            ("ips", Field::N(c.ips.len() as u64)),
            ("words", Field::N(c.words.len() as u64)),
            ("bad_lines", Field::N(bad as u64)),
        ],
    );
    withdraw_disallowed(sh, st);
    st.lobby.cache.clear();
}

/// Cards the switches or the blocklist no longer allow: gone, their hosts told.
fn withdraw_disallowed(sh: &Shared, st: &mut State) {
    let c = &st.lobby.control;
    let gone: Vec<(String, &'static str)> = st
        .lobby
        .ads
        .iter()
        .filter_map(|(id, a)| {
            let why = if !c.rooms {
                "off"
            } else if c.tags.contains(&a.card.tag) || c.ip_blocked(a.owner) {
                "blocked"
            } else if a.card.proto < c.min_proto {
                "old"
            } else {
                return None;
            };
            Some((id.clone(), why))
        })
        .collect();
    for (id, why) in gone {
        tell_gone(sh, st, &id, why);
    }
}

fn tell_gone(sh: &Shared, st: &mut State, id: &str, why: &'static str) {
    if st.lobby.remove_ad(id, why, &sh.rooms).is_none() {
        return;
    }
    if let Some(r) = st.rooms.get(id) {
        if let Some(h) = &r.host {
            let text = gone_text(why, &r.pow, sh.config.rooms.pow_bits);
            sh.rooms.out("ws", text.len());
            h.send(Out::Text(text), &sh.metrics);
        }
    }
}

/// One tick (also called by tests with a chosen `now`).
pub(crate) fn tick_rooms(sh: &Shared, st: &mut State, resident: Option<u64>, now: Instant) {
    let cfg = &sh.config.rooms;
    // Cards past their lifetime or their renewal.
    let old: Vec<(String, &'static str)> = st
        .lobby
        .ads
        .iter()
        .filter_map(|(id, a)| {
            if now.duration_since(a.created) >= cfg.ad_max_age {
                Some((id.clone(), "expired"))
            } else if now.duration_since(a.renewed) >= cfg.ad_renew {
                Some((id.clone(), "stale"))
            } else {
                None
            }
        })
        .collect();
    for (id, why) in old {
        tell_gone(sh, st, &id, why);
    }
    withdraw_disallowed(sh, st);
    // Budgets.
    let lobby = &mut st.lobby;
    lobby.traffic.add(now, sh.rooms.out_bytes.total());
    lobby.projected = lobby.traffic.projected(now);
    lobby.resident = resident;
    let mem = resident.unwrap_or(0);
    lobby.refuse_sockets = resident.is_some() && mem >= cfg.mem_refuse_bytes;
    let want = match lobby.control.cutback {
        CutbackMode::On => Some(Cutback::Manual),
        CutbackMode::Off => None,
        CutbackMode::Auto => {
            let mem_hi = resident.is_some() && mem >= cfg.mem_cutback_bytes;
            let traffic_hi = lobby.projected >= cfg.out_budget_month;
            // Leave only well below the line (no flapping).
            let mem_ok = mem < cfg.mem_cutback_bytes / 10 * 9;
            let traffic_ok = lobby.projected < cfg.out_budget_month / 10 * 8;
            if mem_hi {
                Some(Cutback::Memory)
            } else if traffic_hi {
                Some(Cutback::Traffic)
            } else if lobby.cutback.is_some() && !(mem_ok && traffic_ok) {
                lobby.cutback
            } else {
                None
            }
        }
    };
    if want.is_some() != lobby.cutback.is_some() {
        lobby.cache.clear();
        let why = want
            .or(lobby.cutback)
            .map(Cutback::as_str)
            .unwrap_or("none");
        sh.log(
            "cutback",
            &[
                ("on", Field::N(want.is_some().into())),
                ("why", Field::S(why)),
                ("resident_mib", Field::N(mem >> 20)),
                (
                    "projected_gb_month",
                    Field::N(lobby.projected / 1_000_000_000),
                ),
            ],
        );
    }
    lobby.cutback = want;
    // Caches and buckets.
    let ttl = if lobby.cutback.is_some() {
        cfg.list_cache_cutback
    } else {
        cfg.list_cache
    };
    lobby.cache.retain(|_, c| now.duration_since(c.at) < ttl);
    lobby.previews.retain(|_, p| match p {
        PreviewSlot::Pending { since, .. } => now.duration_since(*since) < cfg.preview_wait * 2,
        PreviewSlot::Done { at, .. } => now.duration_since(*at) < cfg.preview_ttl,
    });
    let creates = per_hour(cfg.creates_per_hour);
    lobby.creates.retain(|_, b| !b.is_full(creates, now));
    lobby.throttle.sweep(now);
    for r in st.rooms.values_mut() {
        r.knock_clients
            .retain(|_, b| !b.is_full(cfg.knock_client, now));
    }
}

// ── HTTP ────────────────────────────────────────────────────────────────────────────────────────

fn json_response(
    status: StatusCode,
    body: String,
    origin: Option<&HeaderValue>,
    max_age: u64,
) -> Response {
    let mut r = Response::new(Body::from(body));
    *r.status_mut() = status;
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_str(&format!("public, max-age={max_age}")).expect("ascii"),
    );
    h.insert(header::VARY, HeaderValue::from_static("origin"));
    if let Some(o) = origin {
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, o.clone());
        h.insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static("retry-after"),
        );
    }
    r
}

fn too_many(retry: u64, origin: Option<&HeaderValue>) -> Response {
    let mut r = json_response(
        StatusCode::TOO_MANY_REQUESTS,
        json!({"why": "slow_down", "retry_s": retry}).to_string(),
        origin,
        0,
    );
    r.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from_str(&retry.to_string()).expect("digits"),
    );
    r
}

/// Who asks and whether they may: CORS (only our origins; no Origin = an app), the blocklist,
/// then the client's bucket for `class`. Nothing heavier runs before these.
fn gate(
    sh: &Shared,
    peer: SocketAddr,
    headers: &HeaderMap,
    class: Class,
) -> Result<(IpKey, Option<HeaderValue>), Box<Response>> {
    let origin = match headers.get(header::ORIGIN) {
        None => None,
        Some(o) => {
            let ok = o
                .to_str()
                .map(|o| sh.config.allowed_origins.iter().any(|a| a == o))
                .unwrap_or(false);
            if !ok {
                sh.rooms.out("refused", 20);
                return Err(Box::new(
                    (StatusCode::FORBIDDEN, "origin not allowed\n").into_response(),
                ));
            }
            Some(o.clone())
        }
    };
    let ip = IpKey::of(client_ip(&sh.config, peer.ip(), headers));
    let mut st = sh.state.lock().unwrap_or_else(|e| e.into_inner());
    if st.lobby.control.ip_blocked(ip) {
        sh.rooms.out("refused", 20);
        return Err(Box::new(json_response(
            StatusCode::FORBIDDEN,
            "{\"why\":\"blocked\"}".into(),
            origin.as_ref(),
            0,
        )));
    }
    let v = st.lobby.throttle.check(ip, class, Instant::now());
    drop(st);
    match v {
        Verdict::Ok => Ok((ip, origin)),
        Verdict::Wait(_) => {
            sh.rooms.throttled.inc(class.as_str());
            sh.rooms.out("refused", 40);
            Err(Box::new(too_many(v.retry_secs(), origin.as_ref())))
        }
        // A client blocked for a sustained flood gets the least the hub can send: no body, and
        // the connection closed (it costs egress, so a flood should cost as little as possible).
        Verdict::Blocked(_) => {
            sh.rooms.throttled.inc(class.as_str());
            sh.rooms.out("refused", 0);
            let mut r = Response::new(Body::empty());
            *r.status_mut() = StatusCode::TOO_MANY_REQUESTS;
            let h = r.headers_mut();
            h.insert(
                header::RETRY_AFTER,
                HeaderValue::from_str(&v.retry_secs().to_string()).expect("digits"),
            );
            h.insert(header::CONNECTION, HeaderValue::from_static("close"));
            Err(Box::new(r))
        }
    }
}

/// One parsed list query, normalised (unknown keys ignored, so random parameters all hit one
/// cache entry).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListQuery {
    pub sort: &'static str,
    pub game: Option<String>,
    pub free: bool,
    pub nopw: bool,
    pub proto: Option<u32>,
    pub lang: Option<&'static str>,
    pub region: Option<Region>,
    pub near: bool,
    pub me: Option<Region>,
    pub page: usize,
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' => {
                match (
                    b.get(i + 1).copied().and_then(hex),
                    b.get(i + 2).copied().and_then(hex),
                ) {
                    (Some(h), Some(l)) => {
                        out.push(h << 4 | l);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_list_query(q: &str) -> ListQuery {
    let mut l = ListQuery {
        sort: "best",
        ..Default::default()
    };
    for kv in q.split('&').take(16) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        if v.len() > 96 {
            continue;
        }
        let v = pct_decode(v);
        let on = v == "1" || v == "true";
        match k {
            "sort" => {
                l.sort = ["best", "latency", "game", "players", "space", "newest"]
                    .into_iter()
                    .find(|s| *s == v)
                    .unwrap_or("best")
            }
            "game" => {
                let g: String = v.trim().chars().take(32).collect();
                if !g.is_empty() {
                    l.game = Some(if valid_title_id(&g) {
                        g
                    } else {
                        g.to_lowercase()
                    });
                }
            }
            "free" => l.free = on,
            "nopw" => l.nopw = on,
            "near" => l.near = on,
            "proto" => l.proto = v.parse().ok().filter(|p| *p <= 999),
            "lang" => l.lang = LANGS.iter().find(|x| **x == v).copied(),
            "region" => l.region = Region::parse(&v),
            "me" => l.me = Region::parse(&v),
            "page" => l.page = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    l
}

fn list_key(q: &ListQuery, viewer: Option<Region>) -> String {
    format!(
        "{}|{}|{}|{}|{:?}|{}|{}|{}|{}|{}",
        q.sort,
        q.game.as_deref().unwrap_or(""),
        q.free as u8,
        q.nopw as u8,
        q.proto,
        q.lang.unwrap_or(""),
        q.region.map(Region::as_str).unwrap_or(""),
        q.near as u8,
        viewer.map(Region::as_str).unwrap_or(""),
        q.page
    )
}

/// The cached part of a list response (everything after `"you"`).
fn build_list(
    cfg: &RoomsConfig,
    lobby: &RoomsState,
    q: &ListQuery,
    viewer: Option<Region>,
    now: Instant,
) -> String {
    let c = &lobby.control;
    let est = |a: &Ad| match (viewer, a.region) {
        (Some(v), Some(h)) => Some(est_ms(v, h)),
        _ => None,
    };
    let compatible = |a: &Ad| match q.proto {
        Some(p) => a.card.proto.abs_diff(p) <= cfg.proto_window,
        None => true,
    };
    let mut rows: Vec<(&String, &Ad)> = lobby
        .ads
        .iter()
        .filter(|(_, a)| {
            let k = &a.card;
            k.listed
                && !k.locked
                && k.proto >= c.min_proto
                && !c.tags.contains(&k.tag)
                && (!q.free || k.slots.0 < k.slots.1)
                && (!q.nopw || !k.pw)
                && compatible(a)
                && (q.lang.is_none() || k.lang == q.lang)
                && (q.region.is_none() || a.region == q.region)
                && (!q.near || matches!((viewer, a.region), (Some(v), Some(h)) if v.near(h)))
                && match &q.game {
                    None => true,
                    Some(g) => k.game.as_ref().is_some_and(|(title, id)| {
                        id == g || title.to_lowercase().contains(g.as_str())
                    }),
                }
        })
        .collect();
    let space = |a: &Ad| a.card.slots.1.saturating_sub(a.card.slots.0);
    let lat = |a: &Ad| est(a).unwrap_or(u32::MAX);
    match q.sort {
        "latency" => {
            rows.sort_by(|x, y| lat(x.1).cmp(&lat(y.1)).then(y.1.created.cmp(&x.1.created)))
        }
        "game" => rows.sort_by(|x, y| {
            let t = |a: &Ad| a.card.game.as_ref().map(|g| g.0.to_lowercase());
            match (t(x.1), t(y.1)) {
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            .then(lat(x.1).cmp(&lat(y.1)))
        }),
        "players" => rows.sort_by(|x, y| {
            y.1.card
                .slots
                .0
                .cmp(&x.1.card.slots.0)
                .then(y.1.created.cmp(&x.1.created))
        }),
        "space" => rows.sort_by(|x, y| {
            space(y.1)
                .cmp(&space(x.1))
                .then(y.1.created.cmp(&x.1.created))
        }),
        "newest" => rows.sort_by_key(|x| std::cmp::Reverse(x.1.created)),
        _ => rows.sort_by(|x, y| {
            (space(y.1) > 0)
                .cmp(&(space(x.1) > 0))
                .then(lat(x.1).cmp(&lat(y.1)))
                .then(compatible(y.1).cmp(&compatible(x.1)))
                .then(y.1.created.cmp(&x.1.created))
        }),
    }
    let cut = lobby.cutback.is_some();
    let items = if cut {
        cfg.page_items_cutback
    } else {
        cfg.page_items
    };
    let start = q.page.min(rows.len()).min(cfg.max_offset);
    let mut out = Vec::new();
    let mut bytes = 0;
    let mut taken = 0;
    for (id, a) in rows.iter().skip(start) {
        if taken >= items {
            break;
        }
        let k = &a.card;
        let mut e = Map::new();
        e.insert("room".into(), json!(id));
        e.insert("name".into(), json!(k.name));
        e.insert("host".into(), json!(k.host));
        e.insert("tag".into(), json!(k.tag));
        if let Some((title, gid)) = &k.game {
            e.insert("game".into(), json!({"title": title, "id": gid}));
        }
        e.insert("proto".into(), json!(k.proto));
        e.insert("ver".into(), json!(k.ver));
        e.insert("caps".into(), json!(k.caps));
        e.insert("slots".into(), json!([k.slots.0, k.slots.1]));
        e.insert("pw".into(), json!(k.pw));
        if let Some(l) = k.lang {
            e.insert("lang".into(), json!(l));
        }
        if let Some(r) = a.region {
            e.insert("region".into(), json!(r.as_str()));
        }
        if let Some(ms) = est(a) {
            e.insert("est_ms".into(), json!(ms));
        }
        e.insert(
            "age_s".into(),
            json!(now.duration_since(a.created).as_secs()),
        );
        let s = Value::Object(e).to_string();
        // A page is at most `page_bytes` (one room at least).
        if taken > 0 && bytes + s.len() + 1 > cfg.page_bytes {
            break;
        }
        bytes += s.len() + 1;
        out.push(s);
        taken += 1;
    }
    let next_at = start + taken;
    let next = if next_at < rows.len() && next_at < cfg.max_offset {
        Value::from(next_at)
    } else {
        Value::Null
    };
    format!(
        "{}\"proto\":{},\"app\":{},\"rooms\":[{}],\"next\":{}}}",
        if cut { "\"cutback\":true," } else { "" },
        json!({"min": c.min_proto, "latest": c.latest_proto}),
        match &c.app_latest {
            Some(v) => json!({"latest": v}),
            None => json!({}),
        },
        out.join(","),
        next
    )
}

/// `GET /rooms`: the public list (see `PROTOCOL.md`).
pub(crate) async fn list_handler(
    AxState(sh): AxState<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    RawQuery(q): RawQuery,
) -> Response {
    let m = &sh.rooms;
    let (ip, origin) = match gate(&sh, peer, &headers, Class::List) {
        Ok(x) => x,
        Err(r) => {
            m.list.inc(if r.status() == StatusCode::FORBIDDEN {
                "blocked"
            } else {
                "limited"
            });
            return *r;
        }
    };
    let cfg = &sh.config.rooms;
    // An overlong query is ignored whole (the plain list), not parsed.
    let raw = q.as_deref().unwrap_or("");
    let q = parse_list_query(if raw.len() > 1024 { "" } else { raw });
    let now = Instant::now();
    let mut st = sh.state.lock().unwrap_or_else(|e| e.into_inner());
    let lobby = &mut st.lobby;
    let ttl = if lobby.cutback.is_some() {
        cfg.list_cache_cutback
    } else {
        cfg.list_cache
    };
    let country = lobby.geo.country(ip);
    let viewer = refine(country.and_then(country_region), q.me);
    let mut you = Map::new();
    if let Some(c) = country {
        you.insert("country".into(), json!(String::from_utf8_lossy(&c)));
    }
    if let Some(r) = viewer {
        you.insert("region".into(), json!(r.as_str()));
    }
    let tail: Arc<str> = if !lobby.control.rooms {
        m.list.inc("off");
        Arc::from(format!(
            "\"off\":true,\"proto\":{},\"rooms\":[],\"next\":null}}",
            json!({"min": lobby.control.min_proto, "latest": lobby.control.latest_proto})
        ))
    } else {
        let key = list_key(&q, viewer);
        match lobby.cache.get(&key) {
            Some(c) if now.duration_since(c.at) < ttl => {
                m.list.inc("hit");
                c.body.clone()
            }
            _ => {
                m.list.inc("miss");
                let body: Arc<str> = Arc::from(build_list(cfg, lobby, &q, viewer, now));
                if lobby.cache.len() >= cfg.list_cache_entries {
                    lobby.cache.retain(|_, c| now.duration_since(c.at) < ttl);
                    if lobby.cache.len() >= cfg.list_cache_entries {
                        if let Some(k) = lobby
                            .cache
                            .iter()
                            .min_by_key(|(_, c)| c.at)
                            .map(|(k, _)| k.clone())
                        {
                            lobby.cache.remove(&k);
                        }
                    }
                }
                lobby.cache.insert(
                    key,
                    Cached {
                        at: now,
                        body: body.clone(),
                    },
                );
                body
            }
        }
    };
    drop(st);
    let body = format!("{{\"you\":{},{}", Value::Object(you), tail);
    m.out("list", body.len());
    json_response(StatusCode::OK, body, origin.as_ref(), ttl.as_secs())
}

/// `GET /rooms/status?r=<id>,<id>`: is each room (the ids a host gave its friends) open, and its
/// card's state. Ids without a card are left out.
pub(crate) async fn status_handler(
    AxState(sh): AxState<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    RawQuery(q): RawQuery,
) -> Response {
    let m = &sh.rooms;
    let (_, origin) = match gate(&sh, peer, &headers, Class::Status) {
        Ok(x) => x,
        Err(r) => {
            m.status.inc(if r.status() == StatusCode::FORBIDDEN {
                "blocked"
            } else {
                "limited"
            });
            return *r;
        }
    };
    let cfg = &sh.config.rooms;
    let q = q.unwrap_or_default();
    let ids: Vec<String> = q
        .split('&')
        .filter_map(|kv| kv.strip_prefix("r="))
        .flat_map(|v| {
            pct_decode(v)
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|id| is_b64url(id, ROOM_LEN))
        .take(cfg.status_max_ids)
        .collect();
    let st = sh.state.lock().unwrap_or_else(|e| e.into_inner());
    let lobby = &st.lobby;
    if !lobby.control.rooms {
        drop(st);
        m.status.inc("off");
        return json_response(
            StatusCode::OK,
            "{\"off\":true,\"rooms\":{}}".into(),
            origin.as_ref(),
            60,
        );
    }
    let every = if lobby.cutback.is_some() {
        cfg.status_every_cutback
    } else {
        cfg.status_every
    };
    let mut rooms = Map::new();
    for id in ids {
        let Some(a) = lobby.ads.get(&id) else {
            continue;
        };
        let k = &a.card;
        let mut e = Map::new();
        e.insert("listed".into(), json!(k.listed));
        e.insert("locked".into(), json!(k.locked));
        e.insert("slots".into(), json!([k.slots.0, k.slots.1]));
        e.insert("pw".into(), json!(k.pw));
        e.insert("proto".into(), json!(k.proto));
        if let Some((title, gid)) = &k.game {
            e.insert("game".into(), json!({"title": title, "id": gid}));
        }
        if let Some(pk) = &k.pk {
            e.insert("pk".into(), json!(pk));
        }
        rooms.insert(id, Value::Object(e));
    }
    drop(st);
    m.status.inc("ok");
    let body = json!({"every_s": every.as_secs(), "rooms": rooms}).to_string();
    m.out("status", body.len());
    json_response(StatusCode::OK, body, origin.as_ref(), 0)
}

/// `GET /rooms/<id>/preview`: the host engine's preview of a room with a card, relayed once per
/// room per `preview_ttl` and shared.
pub(crate) async fn preview_handler(
    AxState(sh): AxState<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let m = &sh.rooms;
    let (_, origin) = match gate(&sh, peer, &headers, Class::Preview) {
        Ok(x) => x,
        Err(r) => {
            m.previews.inc("limited");
            return *r;
        }
    };
    let cfg = &sh.config.rooms;
    let answer = |status: StatusCode, body: String, max_age: u64| {
        m.out("preview", body.len());
        json_response(status, body, origin.as_ref(), max_age)
    };
    if !is_b64url(&id, ROOM_LEN) {
        m.previews.inc("none");
        return answer(StatusCode::NOT_FOUND, "{\"why\":\"no_room\"}".into(), 0);
    }
    let now = Instant::now();
    let mut rx = {
        let mut st = sh.state.lock().unwrap_or_else(|e| e.into_inner());
        let st = &mut *st;
        if !st.lobby.control.preview || st.lobby.cutback.is_some() || !st.lobby.control.rooms {
            m.previews.inc("off");
            return answer(
                StatusCode::SERVICE_UNAVAILABLE,
                "{\"why\":\"off\"}".into(),
                0,
            );
        }
        if !st.lobby.ads.contains_key(&id) {
            m.previews.inc("none");
            return answer(StatusCode::NOT_FOUND, "{\"why\":\"no_room\"}".into(), 0);
        }
        match st.lobby.previews.get(&id) {
            Some(PreviewSlot::Done { at, body }) if now.duration_since(*at) < cfg.preview_ttl => {
                m.previews.inc("cached");
                let left = (cfg.preview_ttl - now.duration_since(*at)).as_secs().max(1);
                return match body {
                    Some(b) => answer(StatusCode::OK, b.to_string(), left),
                    None => answer(
                        StatusCode::GATEWAY_TIMEOUT,
                        "{\"why\":\"no_answer\"}".into(),
                        left,
                    ),
                };
            }
            Some(PreviewSlot::Pending { tx, .. }) => {
                m.previews.inc("cached");
                tx.subscribe()
            }
            _ => {
                let Some(host) = st.rooms.get(&id).and_then(|r| r.host.clone()) else {
                    m.previews.inc("none");
                    return answer(StatusCode::NOT_FOUND, "{\"why\":\"no_room\"}".into(), 0);
                };
                let pid = random_b64(GID_LEN * 3 / 4);
                let text = json!({"t": "preview", "pid": pid}).to_string();
                m.out("ws", text.len());
                host.send(Out::Text(text), &sh.metrics);
                let (tx, rx) = watch::channel(None);
                st.lobby.previews.insert(
                    id.clone(),
                    PreviewSlot::Pending {
                        pid,
                        since: now,
                        tx,
                    },
                );
                m.previews.inc("relayed");
                rx
            }
        }
    };
    let got = tokio::time::timeout(cfg.preview_wait, rx.wait_for(|v| v.is_some()))
        .await
        .ok()
        .and_then(|r| r.ok().and_then(|v| v.clone()));
    match got {
        Some(b) if !b.is_empty() => {
            answer(StatusCode::OK, b.to_string(), cfg.preview_ttl.as_secs())
        }
        Some(_) => answer(
            StatusCode::BAD_GATEWAY,
            "{\"why\":\"bad_answer\"}".into(),
            0,
        ),
        None => {
            let mut st = sh.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(PreviewSlot::Pending { since, .. }) = st.lobby.previews.get(&id) {
                if now.duration_since(*since) < cfg.preview_wait * 2 {
                    st.lobby.previews.insert(
                        id.clone(),
                        PreviewSlot::Done {
                            at: Instant::now(),
                            body: None,
                        },
                    );
                }
            }
            drop(st);
            m.previews.inc("timeout");
            answer(
                StatusCode::GATEWAY_TIMEOUT,
                "{\"why\":\"no_answer\"}".into(),
                0,
            )
        }
    }
}

// ── Admin (private metrics listener only) ───────────────────────────────────────────────────────

fn admin_ok(sh: &Shared, headers: &HeaderMap) -> bool {
    let Some(token) = &sh.config.rooms.admin_token else {
        return false;
    };
    let Some(got) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return false;
    };
    let a = Sha256::digest(token.as_bytes());
    let b = Sha256::digest(got.as_bytes());
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn control_view(st: &State) -> String {
    let c = &st.lobby.control;
    json!({
        "rooms": c.rooms, "preview": c.preview, "knocks": c.knocks,
        "min_proto": c.min_proto, "latest_proto": c.latest_proto,
        "cutback": match c.cutback { CutbackMode::Auto => "auto", CutbackMode::On => "on", CutbackMode::Off => "off" },
        "cutback_active": st.lobby.cutback.map(Cutback::as_str),
        "blocked_tags": c.tags.len(), "blocked_ips": c.ips.len(), "words": c.words.len(),
        "overlay": st.lobby.overlay.is_some(),
    })
    .to_string()
}

/// `GET /admin/control`: the switches in force (counts only).
pub(crate) async fn admin_get(AxState(sh): AxState<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !admin_ok(&sh, &headers) {
        return (StatusCode::NOT_FOUND, "not found\n").into_response();
    }
    let st = sh.state.lock().unwrap_or_else(|e| e.into_inner());
    let body = control_view(&st);
    drop(st);
    sh.rooms.out("admin", body.len());
    json_response(StatusCode::OK, body, None, 0)
}

/// `POST /admin/control` with control lines: they replace the run-time overlay (an empty body
/// clears it) and apply at once.
pub(crate) async fn admin_post(
    AxState(sh): AxState<Arc<Shared>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !admin_ok(&sh, &headers) {
        return (StatusCode::NOT_FOUND, "not found\n").into_response();
    }
    if body.len() > 1 << 20 {
        return (StatusCode::PAYLOAD_TOO_LARGE, "too large\n").into_response();
    }
    let (input, bad) = parse_control(&body);
    let mut st = sh.state.lock().unwrap_or_else(|e| e.into_inner());
    let input = (!body.trim().is_empty()).then_some(input);
    apply_overlay(&sh, &mut st, input, bad, "admin");
    let out = control_view(&st);
    drop(st);
    json_response(StatusCode::OK, out, None, 0)
}

// ── Metrics ─────────────────────────────────────────────────────────────────────────────────────

/// The rooms part of `/metrics`.
pub(crate) fn render(sh: &Shared) -> String {
    use std::fmt::Write as _;
    let m = &sh.rooms;
    let mut s = String::new();
    let (
        listed,
        friends,
        lobbies,
        blocked,
        cut,
        resident,
        projected,
        cache,
        geo,
        tags,
        ips,
        blocks,
    ) = {
        let st = sh.state.lock().unwrap_or_else(|e| e.into_inner());
        let l = &st.lobby;
        (
            l.ads.values().filter(|a| a.card.listed).count(),
            l.ads.values().filter(|a| !a.card.listed).count(),
            st.rooms.values().filter(|r| r.lobby).count(),
            l.throttle.blocked_count(Instant::now()),
            l.cutback.is_some(),
            l.resident.unwrap_or(0),
            l.projected,
            l.cache.len(),
            l.geo.len(),
            l.control.tags.len(),
            l.control.ips.len(),
            l.throttle.blocks_total,
        )
    };
    let mut gauge = |name: &str, help: &str, v: u64| {
        let _ = writeln!(s, "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {v}");
    };
    gauge(
        "hub_lobby_rooms",
        "Lobby rooms (rooms that may carry a card).",
        lobbies as u64,
    );
    gauge("hub_ads_listed", "Public room cards.", listed as u64);
    gauge(
        "hub_ads_friends",
        "Friends-only room cards (status lookups only).",
        friends as u64,
    );
    gauge(
        "hub_clients_blocked",
        "Clients temporarily blocked for a sustained flood.",
        blocked as u64,
    );
    gauge("hub_cutback", "1 while cut-back mode is on.", cut as u64);
    gauge(
        "hub_resident_bytes",
        "Resident memory (0: unknown on this system).",
        resident,
    );
    gauge(
        "hub_out_projected_bytes_month",
        "Outbound bytes a month at the last hour's rate.",
        projected,
    );
    gauge(
        "hub_rooms_list_cache_entries",
        "Cached list responses.",
        cache as u64,
    );
    gauge(
        "hub_geo_ranges",
        "IP ranges in the country table.",
        geo as u64,
    );
    gauge("hub_blocklist_tags", "Blocked host tags.", tags as u64);
    gauge("hub_blocklist_ips", "Blocked address blocks.", ips as u64);
    m.ads.render(
        &mut s,
        "hub_ads_total",
        "kind",
        "Room cards created, updated and renewed.",
    );
    m.ads_removed.render(
        &mut s,
        "hub_ads_removed_total",
        "why",
        "Room cards removed, by why.",
    );
    m.ads_refused.render(
        &mut s,
        "hub_ads_refused_total",
        "why",
        "Room cards refused, by why.",
    );
    m.list.render(
        &mut s,
        "hub_rooms_list_total",
        "result",
        "Room list requests.",
    );
    m.status.render(
        &mut s,
        "hub_rooms_status_total",
        "result",
        "Friends lookups.",
    );
    m.previews
        .render(&mut s, "hub_previews_total", "result", "Relayed previews.");
    m.knocks.render(
        &mut s,
        "hub_knocks_total",
        "result",
        "Knocks forwarded and held back, by why.",
    );
    m.out_bytes.render(
        &mut s,
        "hub_out_bytes_total",
        "class",
        "Outbound bytes (estimate with headers), by class.",
    );
    m.throttled.render(
        &mut s,
        "hub_throttled_total",
        "class",
        "Requests slowed down (429), by class.",
    );
    let _ = writeln!(
        s,
        "# HELP hub_temp_blocks_total Temporary blocks given.\n# TYPE hub_temp_blocks_total counter\nhub_temp_blocks_total {blocks}"
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    pub(crate) fn card() -> Value {
        json!({"listed": true, "name": "Couch chaos", "host": "Sam", "tag": "4f2a",
               "game": {"title": "It Takes Two", "id": "PPSA01234"},
               "proto": 2, "ver": "13.0.0", "caps": ["web", "ps5app", "preview"],
               "slots": [1, 4], "pw": true, "lang": "en", "region": "eu-west",
               "pk": "A".repeat(44)})
    }

    #[test]
    fn a_good_card_parses_and_is_normalised() {
        let c = parse_card(&card(), &[]).unwrap();
        assert_eq!(c.tag, "4F2A");
        assert_eq!(c.slots, (1, 4));
        assert_eq!(c.region, RegionPref::Auto(Some(Region::EuWest)));
        let mut v = card();
        v["region"] = json!("hidden");
        v["name"] = json!("  Couch   chaos ");
        let c = parse_card(&v, &[]).unwrap();
        assert_eq!(c.region, RegionPref::Hidden);
        assert_eq!(c.name, "Couch chaos");
    }

    #[test]
    fn every_bad_field_is_refused_with_its_reason() {
        let cases: Vec<(&str, Value, &str)> = vec![
            ("name", json!("ab"), "name"),
            ("name", json!("visit www x"), "name"),
            ("host", json!("Admin"), "name"),
            ("host", json!(""), "name"),
            ("tag", json!("XYZW"), "bad"),
            ("tag", json!("4F2"), "bad"),
            ("game", json!({"title": "x", "id": "ppsa01234"}), "game"),
            (
                "game",
                json!({"title": "<b>x</b>", "id": "PPSA01234"}),
                "game",
            ),
            (
                "game",
                json!({"title": "x".repeat(65), "id": "PPSA01234"}),
                "game",
            ),
            ("game", json!("PPSA01234"), "game"),
            ("proto", json!(0), "bad"),
            ("proto", json!("2"), "bad"),
            ("ver", json!("13.0"), "bad"),
            ("ver", json!("13.0.0-evil"), "bad"),
            ("caps", json!(["UPPER"]), "bad"),
            ("caps", json!(vec!["x"; 17]), "bad"),
            ("slots", json!([5, 4]), "bad"),
            ("slots", json!([1, 9]), "bad"),
            ("slots", json!([1]), "bad"),
            ("pw", json!("yes"), "bad"),
            ("lang", json!("xx"), "bad"),
            ("region", json!("mars"), "bad"),
            ("pk", json!("short"), "bad"),
            ("listed", json!(1), "bad"),
        ];
        for (k, v, why) in cases {
            let mut c = card();
            c[k] = v.clone();
            assert_eq!(parse_card(&c, &[]).map(|_| ()), Err(why), "{k}={v}");
        }
        assert_eq!(parse_card(&json!([1]), &[]).map(|_| ()), Err("bad"));
    }

    #[test]
    fn pow_checks_bits() {
        let n = "nonce";
        let a = solve_pow(n, 12);
        assert!(pow_ok(n, &a, 12));
        // A random answer rarely has 8 zero bits.
        let mut fails = 0;
        for i in 0..50 {
            if !pow_ok(&format!("n{i}"), "0", 8) {
                fails += 1;
            }
        }
        assert!(fails > 40);
        assert_eq!(leading_zero_bits(&[0, 0, 0x0f]), 20);
    }

    #[test]
    fn cids_are_per_room_and_short() {
        let k = [7u8; 32];
        let ip = IpKey::V4(0xc000_0201);
        let a = cid(&k, "room-a", ip);
        assert_eq!(a.len(), 8);
        assert_eq!(a, cid(&k, "room-a", ip));
        assert_ne!(a, cid(&k, "room-b", ip));
        assert_ne!(a, cid(&[8u8; 32], "room-a", ip));
        assert!(!a.contains("192"));
    }

    #[test]
    fn addresses_are_found_in_text() {
        assert!(has_address(r#"{"x":"192.0.2.1"}"#));
        assert!(has_address(r#"{"x":"at 10.0.0.7:9000"}"#));
        assert!(has_address(r#"{"x":"2001:db8::1"}"#));
        assert!(has_address(r#"{"x":"fe80::1%eth0"}"#));
        assert!(!has_address(
            r#"{"ver":"13.0.0","res":"1080p","t":"12:30"}"#
        ));
        assert!(!has_address(r#"{"rtt_ms":34,"name":"Sam"}"#));
    }

    #[test]
    fn control_lines_parse_and_merge() {
        let (c, bad) = parse_control(
            "# comment\nrooms off\npreview on; knocks off\nmin_proto 3\ntag 4f2a\n\
             ip 203.0.113.0/24\nword meanie\ncutback on\nnonsense\ntag XYZ\n",
        );
        assert_eq!(bad, 2);
        assert_eq!(c.rooms, Some(false));
        assert_eq!(c.knocks, Some(false));
        assert_eq!(c.tags, vec!["4F2A".to_string()]);
        let cfg = RoomsConfig::default();
        let (base, _) = parse_control("tag 1111");
        let m = Control::merge(&cfg, &base, Some(&c));
        assert!(!m.rooms && m.preview && !m.knocks);
        assert_eq!(m.min_proto, 3);
        assert_eq!(m.latest_proto, 3, "latest never below min");
        assert!(m.tags.contains("1111") && m.tags.contains("4F2A"));
        assert!(m.ip_blocked(IpKey::of("203.0.113.9".parse().unwrap())));
        assert!(!m.ip_blocked(IpKey::of("198.51.100.1".parse().unwrap())));
        assert_eq!(m.cutback, CutbackMode::On);
        let (v6, _) = parse_control("ip 2001:db8::/32");
        let m = Control::merge(&cfg, &v6, None);
        assert!(m.ip_blocked(IpKey::of("2001:db8:5:6::1".parse().unwrap())));
    }

    #[test]
    fn list_queries_normalise() {
        let q = parse_list_query("sort=latency&game=It%20Takes&free=1&nopw=true&proto=2&lang=en&near=1&me=na-west&page=12&junk=9");
        assert_eq!(q.sort, "latency");
        assert_eq!(q.game.as_deref(), Some("it takes"));
        assert!(q.free && q.nopw && q.near);
        assert_eq!(q.proto, Some(2));
        assert_eq!(q.lang, Some("en"));
        assert_eq!(q.me, Some(Region::NaWest));
        assert_eq!(q.page, 12);
        let q = parse_list_query("game=PPSA01234&sort=evil");
        assert_eq!(q.game.as_deref(), Some("PPSA01234"));
        assert_eq!(q.sort, "best");
        // Unknown keys do not change the cache key.
        assert_eq!(
            list_key(&parse_list_query("a=1"), None),
            list_key(&parse_list_query("b=2&c=3"), None)
        );
        assert_eq!(pct_decode("a%2"), "a%2");
    }

    #[test]
    fn traffic_projection_uses_the_last_hour() {
        let t0 = Instant::now();
        let mut t = Traffic::new(t0);
        t.add(t0 + Duration::from_secs(10), 1_000);
        // One minute at least: 1,000 bytes a minute.
        assert_eq!(
            t.projected(t0 + Duration::from_secs(10)),
            1_000 * 60 * 24 * 30
        );
        // Two hours on: the first minute has left the window; 1,000 more bytes over the hour.
        t.add(t0 + Duration::from_secs(7200), 2_000);
        assert_eq!(t.projected(t0 + Duration::from_secs(7200)), 720_000);
    }
}
