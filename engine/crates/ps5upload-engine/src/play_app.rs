//! PS5Upload Play's channel: the native app on the host's PS5 sees Play Together and the session
//! live, and acts on them from the couch.
//!
//! **The socket** (`GET /api/play/app`, a WebSocket the app dials; the app never listens). It is
//! reachable from the LAN, like the guest socket, so everything past a minimal status needs the
//! app's token:
//! * app → engine, first: `{"hello": {"token"?: t, "v"?: version}}`.
//! * without a valid token the app gets only `{"doc": {"engine": true, "version", "paired":
//!   false}}` (no invite, no names), plus `{"auth": "invalid" | "other_console" | "revoked"}` when
//!   the token it sent does not work here, or stopped working. It may ask to pair: `{"pair": {}}`
//!   → `{"pairing": {"code", "code_display", "expires_in_s"}}`, a code it shows on the TV. The
//!   user types it into PS5Upload (`POST /api/play/app/pair`, below); the socket then gets
//!   `{"paired": {"id", "token"}}` once, and the full status from then on. The token is the app's
//!   to keep; the engine keeps only its SHA-256.
//! * with a valid token (made for this console's address: a token only ever acts on the console
//!   that paired it), the status document: `{"doc": {"rev", "paired": true, "engine", "console",
//!   "session", "together", "remote_play", "helper", "keep_awake", "app"}}`, pushed when it
//!   changes (round trips and the invite's time left refresh every 10 s at most).
//! * intents, each with an optional `"req"` number echoed in the answer `{"done": kind, "req",
//!   "ok", "error"?}`: `{"decide": {"ask", "allow"}}` (the first answer to an ask wins, from here
//!   or PS5Upload: a later one gets `already_answered`), `{"kick": {"player"}}`, `{"invite":
//!   {"new"?}}` (opens Play Together first when the console is being played), `{"close": {}}`,
//!   `{"settings": {"rev"?, "set": {"block_system_buttons"?, "max_players"?, "stream_seat"?,
//!   "invites"?}}}` (the last change wins; the answer has the new `rev`, and `stale` when the app's
//!   `rev` was behind), `{"keep_awake": {"mode": "off" | "transfers" | "always"}}`, `{"app":
//!   {"open", "foreground", "v"}}` (the app's own state, in the status for PS5Upload), `{"ping":
//!   t}` (answered `{"pong": t}`).
//!
//! **Host routes** (this computer, or a peer `PS5UPLOAD_ALLOW_IP` admits, as the rest of the host
//! API): `POST /api/play/app/pair {code}` (the code on the TV: 400 `code` when it is not one, 400
//! `wrong_code` with `tries_left`, 429 `locked` with `retry_s` after 10 wrong codes (every code
//! then stops working for 10 minutes), 409 `app_gone` when the app that asked closed meanwhile),
//! `POST /api/play/app/unpair {id}`, `GET /api/play/app/paired` (the paired consoles, and those
//! waiting for their code to be typed, never the code itself).
//!
//! **The engine's event feed** gets a `play` event (`{"console", "doc"}`, the status without the
//! invite's code and link) when the status of a console with Play Together open or the app on it
//! changes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use crate::join::{ConnectLimit, CODE_ALPHABET, CODE_LEN};
use crate::pad_link::same_key;
use crate::vinput_api::host_key;

/// The paired apps, in the engine's data folder (mode 0600).
const PAIRED_FILE: &str = "play_app_paired.json";
/// A pairing code works this long.
const PAIR_CODE_TTL: Duration = Duration::from_secs(5 * 60);
/// Pairing codes an address may ask for per minute.
const PAIR_ASKS_PER_MIN: usize = 3;
/// Codes waiting to be typed, on every console together.
const MAX_PENDING_PAIRS: usize = 8;
/// Wrong codes typed before pairing locks, and for how long.
const BAD_CODES_BEFORE_LOCK: u32 = 10;
const PAIR_LOCK: Duration = Duration::from_secs(10 * 60);
/// Paired apps kept at most (the one seen longest ago goes first).
const MAX_PAIRED: usize = 16;
/// The largest message the app may send.
const APP_MSG_MAX: usize = 4096;
/// The app's hello comes within this.
const HELLO_WAIT: Duration = Duration::from_secs(10);
/// How often the status is looked at for changes, and the longest a sent one goes unrefreshed.
const DOC_EVERY: Duration = Duration::from_millis(if cfg!(test) { 100 } else { 1000 });
const DOC_REFRESH: Duration = Duration::from_secs(10);
/// A ping this often; a socket with nothing back for [`QUIET_CLOSE`] is gone.
const PING_EVERY: Duration = Duration::from_secs(10);
const QUIET_CLOSE: Duration = Duration::from_secs(45);
/// Sockets per address per minute (this computer's own: more).
const CONNECTS_PER_MIN: usize = 20;
const LOOPBACK_CONNECTS_PER_MIN: usize = 120;
/// Sockets open at once from one address (this computer's own: no limit). One console runs one
/// app; a few cover a reconnect racing the old socket's end.
const SOCKETS_PER_ADDRESS: usize = 4;
/// A helper that answered a hello this recently counts as there.
const HELPER_SEEN: Duration = Duration::from_secs(10 * 60);

// ── Paired apps ──────────────────────────────────────────────────────────────────────────────────

/// One paired app: the console it was paired from, and its token's SHA-256 (never the token).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct PairedApp {
    /// A short id, for the list and Unpair.
    id: String,
    token_sha256: String,
    /// The console's address (`host_key`) the app paired from: the only one its token acts on.
    console: String,
    /// The console's name on the list when it paired, if it had one.
    #[serde(default)]
    name: Option<String>,
    paired_unix: u64,
    #[serde(default)]
    last_seen_unix: Option<u64>,
}

#[derive(Serialize, Deserialize, Default)]
struct PairedFile {
    version: u32,
    apps: Vec<PairedApp>,
}

/// The paired apps, and where they are kept (None: in memory only, a data folder that can't be
/// used, or the tests).
struct Store {
    path: Option<PathBuf>,
    apps: Vec<PairedApp>,
}

fn sha256_hex(s: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn random_hex(n: usize) -> Option<String> {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).ok()?;
    Some(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Store {
    /// The apps kept at `path`. A file that does not read is set aside (`.bad`), not written over.
    fn open(path: Option<PathBuf>) -> Store {
        let apps = match path.as_deref().map(std::fs::read) {
            Some(Ok(b)) => match serde_json::from_slice::<PairedFile>(&b) {
                Ok(f) => f.apps,
                Err(e) => {
                    if let Some(p) = path.as_deref() {
                        crate::log_at!(
                            Warn,
                            "together",
                            "play app: the paired apps' file did not read ({e}); set aside"
                        );
                        let _ = std::fs::rename(p, p.with_extension("bad"));
                    }
                    Vec::new()
                }
            },
            _ => Vec::new(),
        };
        Store { path, apps }
    }

    fn save(&self) -> anyhow::Result<()> {
        let Some(path) = self.path.as_deref() else {
            return Ok(());
        };
        let body = serde_json::to_vec_pretty(&PairedFile {
            version: 1,
            apps: self.apps.clone(),
        })?;
        // Whole or not at all, and readable by this user only.
        let tmp = path.with_extension("json.tmp");
        write_private(&tmp, &body)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// A new app paired from `console`: its id and its token (shown once, to the app).
    fn add(
        &mut self,
        console: &str,
        name: Option<String>,
        now: u64,
    ) -> anyhow::Result<(String, String)> {
        let token = random_hex(32).ok_or_else(|| anyhow::anyhow!("no randomness"))?;
        let id = random_hex(4).ok_or_else(|| anyhow::anyhow!("no randomness"))?;
        if self.apps.len() >= MAX_PAIRED {
            // The one seen longest ago makes room.
            if let Some(i) = self
                .apps
                .iter()
                .enumerate()
                .min_by_key(|(_, a)| a.last_seen_unix.unwrap_or(a.paired_unix))
                .map(|(i, _)| i)
            {
                self.apps.remove(i);
            }
        }
        self.apps.push(PairedApp {
            id: id.clone(),
            token_sha256: sha256_hex(&token),
            console: host_key(console),
            name,
            paired_unix: now,
            last_seen_unix: None,
        });
        if let Err(e) = self.save() {
            self.apps.retain(|a| a.id != id);
            return Err(e);
        }
        Ok((id, token))
    }

    /// The app holding `token`. Every record is compared, in constant time each.
    fn check(&self, token: &str) -> Option<&PairedApp> {
        if token.is_empty() || token.len() > 128 {
            return None;
        }
        let h = sha256_hex(token);
        let mut found = None;
        for a in &self.apps {
            if same_key(&a.token_sha256, &h) {
                found = Some(a);
            }
        }
        found
    }

    fn remove(&mut self, id: &str) -> anyhow::Result<bool> {
        let before = self.apps.len();
        self.apps.retain(|a| a.id != id);
        if self.apps.len() == before {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }

    fn seen(&mut self, id: &str, now: u64) {
        if let Some(a) = self.apps.iter_mut().find(|a| a.id == id) {
            a.last_seen_unix = Some(now);
            if let Err(e) = self.save() {
                crate::log_at!(Debug, "together", "play app: {e:#}");
            }
        }
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    std::fs::write(path, bytes)?;
    Ok(())
}

// ── Pairing codes ────────────────────────────────────────────────────────────────────────────────

/// What the app on the TV gets told when its code is typed in.
#[derive(Debug)]
enum ToApp {
    Paired {
        id: String,
        token: String,
    },
    /// Its token was unpaired.
    Revoked,
}

/// A code shown on a console's TV, waiting to be typed into PS5Upload.
struct PendingPair {
    code: String,
    console: String,
    until: Instant,
    conn: u64,
    tx: mpsc::Sender<ToApp>,
}

/// Why a typed code did not pair.
#[derive(Debug, PartialEq, Eq)]
enum ConfirmErr {
    /// Too many wrong codes: try again in this many seconds.
    Locked(u64),
    /// Not a code waiting (wrong, or expired): this many tries before the lock.
    Wrong(u32),
}

#[derive(Default)]
struct Pairing {
    pending: Vec<PendingPair>,
    /// Wrong codes since the last pairing or lock.
    bad: u32,
    locked_until: Option<Instant>,
    asks: HashMap<IpAddr, VecDeque<Instant>>,
}

impl Pairing {
    fn prune(&mut self, now: Instant) {
        self.pending.retain(|p| p.until > now && !p.tx.is_closed());
        if self.locked_until.is_some_and(|t| t <= now) {
            self.locked_until = None;
        }
        self.asks.retain(|_, q| {
            while q
                .front()
                .is_some_and(|t| now.saturating_duration_since(*t) >= Duration::from_secs(60))
            {
                q.pop_front();
            }
            !q.is_empty()
        });
    }

    /// The app on `console` (socket `conn`, from `ip`) asks for a code: a new one (its socket's or
    /// its console's earlier one stops working). Err: "slow_down" (too many asks from that
    /// address), "busy" (too many codes waiting), "locked", "no_random".
    fn request(
        &mut self,
        console: &str,
        ip: IpAddr,
        conn: u64,
        tx: mpsc::Sender<ToApp>,
        now: Instant,
    ) -> Result<(String, Instant), &'static str> {
        self.prune(now);
        if self.locked_until.is_some() {
            return Err("locked");
        }
        let q = self.asks.entry(crate::join::limit_key(ip)).or_default();
        if q.len() >= PAIR_ASKS_PER_MIN && !ip.is_loopback() {
            return Err("slow_down");
        }
        q.push_back(now);
        self.pending
            .retain(|p| p.conn != conn && p.console != host_key(console));
        if self.pending.len() >= MAX_PENDING_PAIRS {
            return Err("busy");
        }
        let code = loop {
            let c = crate::join::new_code().ok_or("no_random")?;
            if !self.pending.iter().any(|p| p.code == c) {
                break c;
            }
        };
        let until = now + PAIR_CODE_TTL;
        self.pending.push(PendingPair {
            code: code.clone(),
            console: host_key(console),
            until,
            conn,
            tx,
        });
        Ok((code, until))
    }

    /// The code typed in PS5Upload (normalized). The 10th wrong one locks pairing for
    /// [`PAIR_LOCK`] and every code waiting stops working.
    fn confirm(&mut self, code: &str, now: Instant) -> Result<PendingPair, ConfirmErr> {
        self.prune(now);
        if let Some(t) = self.locked_until {
            return Err(ConfirmErr::Locked(
                t.saturating_duration_since(now).as_secs().max(1),
            ));
        }
        let mut at = None;
        for (i, p) in self.pending.iter().enumerate() {
            if same_key(&p.code, code) {
                at = Some(i);
            }
        }
        match at {
            Some(i) => {
                self.bad = 0;
                Ok(self.pending.remove(i))
            }
            None => {
                self.bad += 1;
                if self.bad >= BAD_CODES_BEFORE_LOCK {
                    self.bad = 0;
                    self.locked_until = Some(now + PAIR_LOCK);
                    self.pending.clear();
                    crate::log_at!(
                        Warn,
                        "together",
                        "play app: {BAD_CODES_BEFORE_LOCK} wrong pairing codes; pairing is locked for {} minutes",
                        PAIR_LOCK.as_secs() / 60
                    );
                    return Err(ConfirmErr::Locked(PAIR_LOCK.as_secs()));
                }
                Err(ConfirmErr::Wrong(BAD_CODES_BEFORE_LOCK - self.bad))
            }
        }
    }

    fn locked_s(&self, now: Instant) -> Option<u64> {
        self.locked_until
            .filter(|t| *t > now)
            .map(|t| t.saturating_duration_since(now).as_secs().max(1))
    }
}

// ── The engine's side of every app ───────────────────────────────────────────────────────────────

/// An app's socket.
struct Sock {
    console: String,
    /// Where it comes from ([`crate::join::limit_key`]), for [`SOCKETS_PER_ADDRESS`].
    from: IpAddr,
    /// The paired app's id, once it showed a valid token or paired here.
    paired: Option<String>,
    tx: mpsc::Sender<ToApp>,
}

/// What the app said about itself (`{"app": …}`).
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
struct AppReport {
    open: bool,
    foreground: bool,
    v: Option<String>,
}

#[derive(Default)]
struct Hub {
    store: Option<Store>,
    pairing: Pairing,
    sockets: HashMap<u64, Sock>,
    /// By console (`host_key`): what its app said last.
    reports: HashMap<String, AppReport>,
}

impl Hub {
    fn store(&mut self) -> &mut Store {
        self.store.get_or_insert_with(|| Store::open(store_path()))
    }
}

static HUB: Mutex<Option<Hub>> = Mutex::new(None);

fn with_hub<R>(f: impl FnOnce(&mut Hub) -> R) -> R {
    let mut g = HUB.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Hub::default))
}

#[cfg(test)]
static TEST_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Where the paired apps are kept. The tests never write the user's own data folder.
fn store_path() -> Option<PathBuf> {
    #[cfg(test)]
    {
        TEST_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|d| d.join(PAIRED_FILE))
    }
    #[cfg(not(test))]
    {
        ps5upload_ava1::pool::data_dir().map(|d| d.join(PAIRED_FILE))
    }
}

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);
static CONNECTS: ConnectLimit = ConnectLimit::new();

/// The console an app socket comes from: its address (an IPv4 one as itself, not mapped).
fn console_of(ip: IpAddr) -> String {
    let ip = match ip {
        IpAddr::V6(a) => a.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    };
    host_key(&ip.to_string())
}

// ── The status document ──────────────────────────────────────────────────────────────────────────

/// What an app without a valid token is told: the engine is there, and its version.
fn minimal_doc() -> Value {
    json!({"doc": {"engine": true, "version": env!("CARGO_PKG_VERSION"), "paired": false}})
}

/// The status of `console`: the session, Play Together (`secrets`: with the invite's code and
/// link, for its paired app), Remote Play, the helper, keep awake, and its app.
fn status(console: &str, secrets: bool) -> Value {
    let brief = crate::consoles_api::console_brief(console);
    let (name, settings) = brief.unwrap_or_default();
    let session = match crate::rp_stream_api::live_video(console) {
        Some((codec, kbps)) => json!({"state": "playing", "codec": codec, "bitrate_kbps": kbps}),
        None => json!({"state": "idle"}),
    };
    let app = with_hub(|h| h.reports.get(&host_key(console)).cloned()).unwrap_or_default();
    json!({
        "engine": {
            "version": env!("CARGO_PKG_VERSION"),
            "name": crate::mesh_api::device_name(),
        },
        "console": {"addr": console, "name": name},
        "session": session,
        "together": crate::join::app_view(console, secrets),
        // Remote Play paired with this console on this engine; the helper heard from lately.
        "remote_play": {"paired": crate::consoles_api::rp_paired(console)},
        "helper": {
            "present": crate::mesh_api::helper_seen_s(console)
                .map(|s| s < HELPER_SEEN.as_secs()),
        },
        "keep_awake": settings.get("keep_awake_mode").cloned().unwrap_or(Value::Null),
        "app": app,
    })
}

/// The status without what changes all the time (round trips, the invite's time left): what
/// "changed" is judged on.
fn steady(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(k, _)| !matches!(k.as_str(), "rtt_ms" | "expires_in_s"))
                .map(|(k, v)| (k.clone(), steady(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(steady).collect()),
        other => other.clone(),
    }
}

// ── The socket ───────────────────────────────────────────────────────────────────────────────────

/// `GET /api/play/app`: PS5Upload Play's socket.
pub async fn ws(
    upgrade: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
) -> Response {
    let ip = peer.ip();
    let limit = if ip.is_loopback() {
        LOOPBACK_CONNECTS_PER_MIN
    } else {
        CONNECTS_PER_MIN
    };
    let from = crate::join::limit_key(ip);
    let open = with_hub(|h| h.sockets.values().filter(|s| s.from == from).count());
    if !CONNECTS.allow(from, limit, Instant::now())
        || (!ip.is_loopback() && open >= SOCKETS_PER_ADDRESS)
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "too many tries, wait a minute",
        )
            .into_response();
    }
    upgrade
        .max_message_size(APP_MSG_MAX)
        .max_frame_size(APP_MSG_MAX)
        .on_upgrade(move |s| session(s, ip))
}

fn say(v: Value) -> Message {
    Message::Text(v.to_string().into())
}

/// The app's hello: its token, if it sent one (None: no hello in time).
async fn read_hello(socket: &mut WebSocket) -> Option<Option<String>> {
    let m = tokio::time::timeout(HELLO_WAIT, async {
        loop {
            match socket.recv().await {
                Some(Ok(Message::Text(t))) => return Some(t),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                _ => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()?;
    let v: Value = serde_json::from_str(m.as_str()).ok()?;
    let h = v.get("hello")?;
    Some(
        h.get("token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty() && t.len() <= 128)
            .map(str::to_string),
    )
}

/// What a token presented from `console` is.
enum Auth {
    Paired(String),
    None,
    /// Not a token this engine knows.
    Invalid,
    /// A token made for another console.
    OtherConsole,
}

fn check_token(token: Option<&str>, console: &str) -> Auth {
    let Some(token) = token else {
        return Auth::None;
    };
    with_hub(|h| {
        let found = h
            .store()
            .check(token)
            .map(|a| (a.id.clone(), a.console.clone()));
        match found {
            None => Auth::Invalid,
            Some((_, c)) if c != host_key(console) => Auth::OtherConsole,
            Some((id, _)) => {
                h.store().seen(&id, unix_now());
                Auth::Paired(id)
            }
        }
    })
}

async fn session(mut socket: WebSocket, ip: IpAddr) {
    let Some(token) = read_hello(&mut socket).await else {
        let _ = socket
            .send(say(json!({"error": "hello", "for": "hello"})))
            .await;
        return;
    };
    let console = console_of(ip);
    let conn = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = mpsc::channel::<ToApp>(4);
    let c = console.clone();
    let auth = tokio::task::spawn_blocking(move || check_token(token.as_deref(), &c))
        .await
        .unwrap_or(Auth::None);
    let mut paired = match auth {
        Auth::Paired(id) => Some(id),
        Auth::None => None,
        Auth::Invalid => {
            let _ = socket.send(say(json!({"auth": "invalid"}))).await;
            None
        }
        Auth::OtherConsole => {
            let _ = socket.send(say(json!({"auth": "other_console"}))).await;
            None
        }
    };
    crate::log_at!(
        Info,
        "together",
        "play app: the app on {console} connected ({})",
        if paired.is_some() {
            "paired"
        } else {
            "not paired"
        }
    );
    with_hub(|h| {
        h.sockets.insert(
            conn,
            Sock {
                console: console.clone(),
                from: crate::join::limit_key(ip),
                paired: paired.clone(),
                tx: tx.clone(),
            },
        )
    });
    if paired.is_none() && socket.send(say(minimal_doc())).await.is_err() {
        end(conn, &console, false);
        return;
    }
    let mut tick = tokio::time::interval(DOC_EVERY);
    let mut ping = tokio::time::interval(PING_EVERY);
    let mut heard = Instant::now();
    let mut last: Option<(Value, Instant)> = None;
    // An action of this app's shows at once.
    let mut force = false;
    let mut rev = 0u64;
    loop {
        tokio::select! {
            m = rx.recv() => match m {
                Some(ToApp::Paired { id, token }) => {
                    if socket.send(say(json!({"paired": {"id": id, "token": token}}))).await.is_err() {
                        break;
                    }
                    with_hub(|h| {
                        if let Some(s) = h.sockets.get_mut(&conn) {
                            s.paired = Some(id.clone());
                        }
                    });
                    crate::log_at!(Info, "together", "play app: the app on {console} is paired");
                    paired = Some(id);
                    last = None;
                }
                Some(ToApp::Revoked) => {
                    paired = None;
                    last = None;
                    if socket.send(say(json!({"auth": "revoked"}))).await.is_err()
                        || socket.send(say(minimal_doc())).await.is_err()
                    {
                        break;
                    }
                }
                None => break,
            },
            inc = socket.recv() => match inc {
                Some(Ok(Message::Text(t))) => {
                    heard = Instant::now();
                    let Ok(Value::Object(m)) = serde_json::from_str::<Value>(t.as_str()) else {
                        continue;
                    };
                    let reply = handle(&m, &console, ip, conn, &tx, paired.is_some()).await;
                    if let Some(r) = reply {
                        if socket.send(say(r)).await.is_err() {
                            break;
                        }
                    }
                    force = true;
                    tick.reset_immediately();
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => heard = Instant::now(),
            },
            _ = tick.tick() => {
                if paired.is_none() {
                    continue;
                }
                let c = console.clone();
                let Ok(doc) = tokio::task::spawn_blocking(move || status(&c, true)).await else {
                    continue;
                };
                let now = Instant::now();
                let st = steady(&doc);
                let due = force
                    || match &last {
                        None => true,
                        Some((s, at)) => *s != st || now.saturating_duration_since(*at) >= DOC_REFRESH,
                    };
                force = false;
                if due {
                    rev += 1;
                    let mut d = doc;
                    d["rev"] = rev.into();
                    d["paired"] = true.into();
                    if socket.send(say(json!({"doc": d}))).await.is_err() {
                        break;
                    }
                    last = Some((st, now));
                }
            },
            _ = ping.tick() => {
                if heard.elapsed() >= QUIET_CLOSE {
                    break;
                }
                let _ = socket.send(Message::Ping(Vec::new().into())).await;
            },
        }
    }
    end(conn, &console, paired.is_some());
}

/// The socket is gone: its codes stop working; a paired app counts as closed.
fn end(conn: u64, console: &str, paired: bool) {
    with_hub(|h| {
        h.sockets.remove(&conn);
        h.pairing.pending.retain(|p| p.conn != conn);
        let still = h
            .sockets
            .values()
            .any(|s| s.paired.is_some() && s.console == host_key(console));
        if paired && !still {
            if let Some(r) = h.reports.get_mut(&host_key(console)) {
                r.open = false;
                r.foreground = false;
            }
        }
    });
    crate::log_at!(
        Debug,
        "together",
        "play app: the app on {console} disconnected"
    );
}

fn done(kind: &str, req: Option<&Value>, r: Result<Value, String>) -> Value {
    let mut v = match r {
        Ok(extra) => {
            let mut v = json!({"done": kind, "ok": true});
            if let (Value::Object(m), Value::Object(e)) = (&mut v, extra) {
                m.extend(e);
            }
            v
        }
        Err(e) => json!({"done": kind, "ok": false, "error": e}),
    };
    if let Some(r) = req.filter(|r| r.is_u64()) {
        v["req"] = r.clone();
    }
    v
}

/// The error word in an answer from the host API.
async fn outcome(r: Response) -> Result<Value, String> {
    let ok = r.status().is_success();
    let b = axum::body::to_bytes(r.into_body(), 1 << 20)
        .await
        .unwrap_or_default();
    let v: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
    if ok {
        Ok(v)
    } else {
        Err(v["error"]
            .as_str()
            .map(|e| e.split(':').next().unwrap_or(e).to_string())
            .unwrap_or_else(|| "failed".into()))
    }
}

/// One message from the app: the answer to send, if any.
async fn handle(
    m: &Map<String, Value>,
    console: &str,
    ip: IpAddr,
    conn: u64,
    tx: &mpsc::Sender<ToApp>,
    paired: bool,
) -> Option<Value> {
    let req = m.get("req");
    if let Some(t) = m.get("ping") {
        return Some(json!({"pong": t}));
    }
    if m.contains_key("pair") {
        if paired {
            return Some(done("pair", req, Err("already_paired".into())));
        }
        let now = Instant::now();
        let r = with_hub(|h| h.pairing.request(console, ip, conn, tx.clone(), now));
        return Some(match r {
            Ok((code, until)) => {
                crate::log_at!(
                    Info,
                    "together",
                    "play app: the app on {console} shows a pairing code"
                );
                json!({"pairing": {
                    "code": code,
                    "code_display": crate::join::code_display(&code),
                    "expires_in_s": until.saturating_duration_since(now).as_secs(),
                }})
            }
            Err(e) => done("pair", req, Err(e.into())),
        });
    }
    let (kind, body) = m.iter().find(|(k, _)| *k != "req")?;
    let kind = kind.as_str();
    if !paired {
        return Some(done(kind, req, Err("not_paired".into())));
    }
    let r = act(kind, body, console).await;
    Some(done(kind, req, r))
}

/// An intent from a paired app on `console`.
async fn act(kind: &str, b: &Value, console: &str) -> Result<Value, String> {
    match kind {
        "decide" => {
            let ask = b["ask"].as_u64().ok_or("ask")?;
            let allow = b["allow"].as_bool().ok_or("allow")?;
            crate::join::decide_on(console, ask, allow).map_err(str::to_string)?;
            Ok(json!({}))
        }
        "kick" => {
            let player = b["player"]
                .as_u64()
                .and_then(|p| u8::try_from(p).ok())
                .ok_or("player")?;
            if crate::join::kick_player(console, player) {
                Ok(json!({}))
            } else {
                Err("no_player".into())
            }
        }
        "invite" => {
            match crate::join::open_party(console).await {
                Ok(()) => {}
                Err(crate::join::OpenErr::NotPlaying) => return Err("not_playing".into()),
                Err(crate::join::OpenErr::Refused(_, why)) => {
                    return Err(why.split(':').next().unwrap_or("failed").to_string())
                }
            }
            let new = b["new"].as_bool().unwrap_or(false);
            outcome(crate::join::make_invite(console, new, false, true))
                .await
                .map(|_| json!({}))
        }
        "close" => Ok(json!({"was_open": crate::join::close_party(console)})),
        "settings" => {
            let set = b.get("set").and_then(Value::as_object).ok_or("set")?;
            let flag = |k: &str| set.get(k).and_then(Value::as_bool);
            let max_players = match set.get("max_players") {
                None => None,
                Some(v) => Some(
                    v.as_u64()
                        .and_then(|n| u8::try_from(n).ok())
                        .ok_or("max_players")?,
                ),
            };
            let invites = match set.get("invites") {
                None => None,
                Some(v) => Some(v.as_str().ok_or("invites")?.to_string()),
            };
            let before = crate::join::settings_rev();
            let v = outcome(
                crate::join::app_settings(
                    flag("block_system_buttons"),
                    max_players,
                    flag("stream_seat"),
                    invites,
                )
                .await,
            )
            .await?;
            let stale = b["rev"].as_u64().is_some_and(|r| r < before);
            Ok(json!({"rev": v["rev"], "stale": stale}))
        }
        "keep_awake" => {
            let mode = b["mode"].as_str().ok_or("mode")?.to_string();
            if !matches!(mode.as_str(), "off" | "transfers" | "always") {
                return Err("mode".into());
            }
            let c = console.to_string();
            let mut changes = Map::new();
            changes.insert("keep_awake_mode".into(), Value::String(mode));
            tokio::task::spawn_blocking(move || {
                crate::consoles_api::set_console_settings(&c, &changes)
            })
            .await
            .map_err(|e| e.to_string())??;
            Ok(json!({}))
        }
        "app" => {
            let v = b["v"]
                .as_str()
                .filter(|v| {
                    !v.is_empty()
                        && v.len() <= 24
                        && v.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'.')
                })
                .map(str::to_string);
            let r = AppReport {
                open: b["open"].as_bool().unwrap_or(true),
                foreground: b["foreground"].as_bool().unwrap_or(false),
                v,
            };
            with_hub(|h| h.reports.insert(host_key(console), r));
            Ok(json!({}))
        }
        _ => Err("unknown".into()),
    }
}

// ── Host routes ──────────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct PairBody {
    code: String,
}

fn bad(code: StatusCode, v: Value) -> Response {
    (code, Json(v)).into_response()
}

/// `POST /api/play/app/pair {code}`: the code PS5Upload Play shows on the TV, typed in here.
pub async fn pair(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<PairBody>,
) -> Response {
    if let Some(r) = crate::join::host_only(&peer, &headers) {
        return r;
    }
    let code = crate::join::normalize_code(&b.code);
    if code.len() != CODE_LEN || !code.bytes().all(|c| CODE_ALPHABET.contains(&c)) {
        return bad(StatusCode::BAD_REQUEST, json!({"error": "code"}));
    }
    let now = Instant::now();
    let found = with_hub(|h| h.pairing.confirm(&code, now));
    let p = match found {
        Ok(p) => p,
        Err(ConfirmErr::Locked(s)) => {
            return bad(
                StatusCode::TOO_MANY_REQUESTS,
                json!({"error": "locked", "retry_s": s}),
            )
        }
        Err(ConfirmErr::Wrong(left)) => {
            return bad(
                StatusCode::BAD_REQUEST,
                json!({"error": "wrong_code", "tries_left": left}),
            )
        }
    };
    let console = p.console.clone();
    let c = console.clone();
    let name = tokio::task::spawn_blocking(move || {
        crate::consoles_api::console_brief(&c).and_then(|(n, _)| n)
    })
    .await
    .ok()
    .flatten();
    let n = name.clone();
    let c = console.clone();
    let issued =
        tokio::task::spawn_blocking(move || with_hub(|h| h.store().add(&c, n, unix_now())))
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .and_then(|r| r);
    let (id, token) = match issued {
        Ok(v) => v,
        Err(e) => {
            crate::log_at!(Warn, "together", "play app: pairing not saved: {e:#}");
            return bad(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": "save_failed"}),
            );
        }
    };
    // The token goes to the app on its socket, once; an app that left meanwhile is not paired.
    if p.tx
        .try_send(ToApp::Paired {
            id: id.clone(),
            token,
        })
        .is_err()
    {
        let i = id.clone();
        let _ = tokio::task::spawn_blocking(move || with_hub(|h| h.store().remove(&i))).await;
        return bad(StatusCode::CONFLICT, json!({"error": "app_gone"}));
    }
    crate::log_at!(
        Info,
        "together",
        "play app: paired the app on {console} (id {id})"
    );
    Json(json!({"ok": true, "id": id, "console": console, "name": name})).into_response()
}

#[derive(Deserialize)]
pub struct UnpairBody {
    id: String,
}

/// `POST /api/play/app/unpair {id}`: that app's token stops working (an app connected with it
/// is told `revoked`).
pub async fn unpair(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<UnpairBody>,
) -> Response {
    if let Some(r) = crate::join::host_only(&peer, &headers) {
        return r;
    }
    let id = b.id.clone();
    let r = tokio::task::spawn_blocking(move || {
        with_hub(|h| {
            let gone = h.store().remove(&id)?;
            if gone {
                for s in h.sockets.values_mut() {
                    if s.paired.as_deref() == Some(id.as_str()) {
                        s.paired = None;
                        let _ = s.tx.try_send(ToApp::Revoked);
                    }
                }
            }
            anyhow::Ok(gone)
        })
    })
    .await;
    match r {
        Ok(Ok(ok)) => {
            if ok {
                crate::log_at!(Info, "together", "play app: unpaired app {}", b.id);
            }
            Json(json!({"ok": ok})).into_response()
        }
        _ => bad(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "save_failed"}),
        ),
    }
}

/// `GET /api/play/app/paired`: `{apps: [{id, console, name, paired_unix, last_seen_unix,
/// connected}], waiting: [{console, expires_in_s}], locked_s}`. Never a token or a code.
pub async fn paired(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = crate::join::host_only(&peer, &headers) {
        return r;
    }
    let now = Instant::now();
    let v = tokio::task::spawn_blocking(move || {
        with_hub(|h| {
            h.pairing.prune(now);
            let live: HashSet<String> = h
                .sockets
                .values()
                .filter_map(|s| s.paired.clone())
                .collect();
            let apps: Vec<Value> = h
                .store()
                .apps
                .iter()
                .map(|a| {
                    json!({
                        "id": a.id,
                        "console": a.console,
                        "name": a.name,
                        "paired_unix": a.paired_unix,
                        "last_seen_unix": a.last_seen_unix,
                        "connected": live.contains(&a.id),
                    })
                })
                .collect();
            let waiting: Vec<Value> = h
                .pairing
                .pending
                .iter()
                .map(|p| {
                    json!({
                        "console": p.console,
                        "expires_in_s": p.until.saturating_duration_since(now).as_secs(),
                    })
                })
                .collect();
            json!({"apps": apps, "waiting": waiting, "locked_s": h.pairing.locked_s(now)})
        })
    })
    .await
    .unwrap_or(Value::Null);
    Json(v).into_response()
}

// ── The event feed ───────────────────────────────────────────────────────────────────────────────

/// Publishes a `play` event on the engine's feed whenever a console's status changes, for the
/// consoles with Play Together open or their app connected (one look a second).
pub(crate) fn spawn_feed() {
    tokio::spawn(async move {
        let mut last: HashMap<String, Value> = HashMap::new();
        let mut t = tokio::time::interval(Duration::from_secs(1));
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            t.tick().await;
            let r = tokio::task::spawn_blocking(move || {
                feed_round(&mut last);
                last
            })
            .await;
            match r {
                Ok(l) => last = l,
                Err(_) => last = HashMap::new(),
            }
        }
    });
}

/// One look: the consoles whose status changed since `last` get a `play` event.
fn feed_round(last: &mut HashMap<String, Value>) {
    let mut consoles: HashSet<String> = crate::join::open_consoles()
        .iter()
        .map(|c| host_key(c))
        .collect();
    with_hub(|h| {
        consoles.extend(h.sockets.values().map(|s| s.console.clone()));
    });
    // A console no longer followed gets its last word (Play Together closed, the app gone).
    consoles.extend(last.keys().cloned());
    for c in consoles {
        let doc = status(&c, false);
        let st = steady(&doc);
        let followed = doc["together"]["open"] == true
            || with_hub(|h| h.sockets.values().any(|s| s.console == c));
        if last.get(&c) != Some(&st) {
            crate::event_bus::bus().publish_json("play", &json!({"console": c, "doc": doc}));
        }
        if followed {
            last.insert(c, st);
        } else {
            last.remove(&c);
        }
    }
}

/// The routes (lib.rs registers the same ones).
#[cfg(test)]
pub(crate) fn router<S: Clone + Send + Sync + 'static>() -> axum::Router<S> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/play/app", get(ws))
        .route("/api/play/app/pair", post(pair))
        .route("/api/play/app/unpair", post(unpair))
        .route("/api/play/app/paired", get(paired))
}

#[cfg(test)]
#[path = "play_app_tests.rs"]
mod tests;
