//! PS5Upload Play's channel: the native app on the host's PS5 sees Play Together and the session
//! live, and acts on them from the couch.
//!
//! **The socket** (`GET /api/play/app`, a WebSocket the app dials; the app never listens). It is
//! reachable from the LAN, like the guest socket, and over Tailscale through the engine's own node
//! (its guest port, `crate::tailnet_guest`, which serves only these two sockets and gives the
//! handler the peer's 100.x address), so everything past a minimal status needs the app's token:
//! * app → engine, first: `{"hello": {"token"?: t, "app_id"?: id, "grant"?: g, "v"?: version}}`.
//!   `app_id` is the app's own install id (made once on the console: 32 hex characters, or 22–64
//!   base64url ones; anything else is ignored), kept with a pairing made from then on.
//! * **Paired through the helper, no code:** while this engine has an AVA1 session to a console on
//!   its list, it hands the helper a one-time grant (`play.grant` `{engine_id, engine_name, grant,
//!   ttl_s, port}`: 32 random bytes, base64url, good for 10 minutes and handed again, renewed,
//!   every 5 while the session lasts and on every new session). The helper serves it to the app on
//!   loopback only (`GET 127.0.0.1:9181/v1/play/grants`). An app whose token does not pair it here
//!   and whose hello carries the grant, from that console (its address, or another address with
//!   the same AVA1 key), is paired exactly as a typed code pairs it: `{"paired": {"id", "token"}}`.
//!   The grant is used up by any try; one that does not pair gets `{"auth": "grant_invalid",
//!   "why": "unknown" | "expired" | "wrong_console"}`. An older helper ("unknown method") is left
//!   alone (asked again in 30 minutes).
//! * without a valid token the app gets only `{"doc": {"engine": true, "version", "paired":
//!   false}}` (no invite, no names), plus `{"auth": "invalid" | "other_console" | "revoked" |
//!   "confirm_move"}` when the token it sent does not work here, or stopped working.
//!   **A console that moved** keeps its pairing: a token made at another address acts here at once
//!   when the console proves the same AVA1 key there (the key this engine pinned for the old
//!   address; logged, nothing asked). With no key to compare, a hello whose `app_id` is the one
//!   kept with the pairing gets `confirm_move` (the app shows "Confirm on <computer>") and
//!   PS5Upload's feed a `play` event `{"moved": {"console", "id", "name", "addr_old",
//!   "addr_new"}}`; one click there (`POST /api/play/app/moved`) re-binds the token, and the app's
//!   socket is told `{"auth": "moved"}` and gets the full status (or `{"auth": "other_console"}`
//!   when PS5Upload says it is not that console). It may ask to pair: `{"pair": {}}`
//!   → `{"pairing": {"code", "code_display", "expires_in_s"}}`, a code it shows on the TV. The
//!   user types it into PS5Upload (`POST /api/play/app/pair`, below); the socket then gets
//!   `{"paired": {"id", "token"}}` once, and the full status from then on. The token is the app's
//!   to keep; the engine keeps only its SHA-256.
//! * with a valid token (made for this console's address: a token only ever acts on the console
//!   that paired it), the status document: `{"doc": {"rev", "paired": true, "engine", "console",
//!   "session", "together", "remote_play", "helper", "keep_awake", "app"}}`, pushed when it
//!   changes (round trips and the invite's time left refresh every 10 s at most). `helper` is
//!   `{"present", "version", "app_api"}`: the helper's version from its hello answer, and whether
//!   it serves the app's loopback API (what it said, else version 13.0.0 or newer); null when not
//!   known.
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
//! `POST /api/play/app/unpair {id}`, `GET /api/play/app/paired` (the paired consoles, those
//! waiting for their code to be typed, and the consoles that moved waiting to be confirmed),
//! `POST /api/play/app/moved {console_old?, addr, ok}` (the one click for a console that moved),
//! `POST /api/play/app/pair_confirm {console}` (pairs the app whose code waits on that console's
//! TV with one click, exactly as typing the code does: PS5Upload showed the code to compare).
//! While a code waits, PS5Upload's own feed gets a `play` event `{"pair_waiting": {"console",
//! "name", "code_display"}}`, and `{"pair_ended": {"console"}}` once it no longer waits.
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
/// A console that moved waits this long for PS5Upload to confirm it, and so many wait at once.
const MOVE_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_PENDING_MOVES: usize = 8;
/// How long the console that moved has to prove its AVA1 key at the new address (a dial).
const MOVE_PROOF_WAIT: Duration = Duration::from_secs(3);
/// A grant handed to a console's helper (`play.grant`) works this long; it is handed again,
/// renewed, this often while the AVA1 session to the console lasts.
const GRANT_TTL: Duration = Duration::from_secs(10 * 60);
const GRANT_RENEW: Duration = Duration::from_secs(5 * 60);
/// A helper too old for `play.grant` is asked again after this long.
const GRANT_OLD_HELPER: Duration = Duration::from_secs(30 * 60);
/// How often the consoles with a session are looked at for a grant due.
const GRANT_LOOK: Duration = Duration::from_secs(60);

// ── Paired apps ──────────────────────────────────────────────────────────────────────────────────

/// One paired app: the console it was paired from, and its token's SHA-256 (never the token).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct PairedApp {
    /// A short id, for the list and Unpair.
    id: String,
    token_sha256: String,
    /// The console's address (`host_key`) the app paired from: the only one its token acts on
    /// (until the console proves it moved, below).
    console: String,
    /// The console's name on the list when it paired, if it had one.
    #[serde(default)]
    name: Option<String>,
    paired_unix: u64,
    #[serde(default)]
    last_seen_unix: Option<u64>,
    /// The console's AVA1 key (hex) this engine pinned for that address, when it had one: the
    /// token follows the console to a new address that proves the same key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    console_key: Option<String>,
    /// The app's own install id (its hello's `app_id`): a new address with the same id is
    /// offered to PS5Upload to confirm with one click.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    app_id: Option<String>,
}

/// An app's install id as its hello sends it: 128 bits or more, as 32 hex characters or
/// base64url (22 to 64 characters of `A-Z a-z 0-9 - _`).
fn valid_app_id(s: &str) -> bool {
    (22..=64).contains(&s.len())
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

#[cfg(test)]
static TEST_KEYS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// The AVA1 key (hex) this engine pinned for the console at `addr`, if it met it there. The tests
/// name the keys themselves ([`TEST_KEYS`]), never the user's own pins.
fn key_at(addr: &str) -> Option<String> {
    #[cfg(test)]
    {
        let want = host_key(addr);
        TEST_KEYS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(a, _)| *a == want)
            .map(|(_, k)| k.clone())
    }
    #[cfg(not(test))]
    {
        ps5upload_ava1::pool()
            .pinned_key(addr)
            .map(|k| k.iter().map(|x| format!("{x:02x}")).collect())
    }
}

/// Whether the console answering AVA1 at `new` holds the key pinned for `old` (a dial, as
/// `/api/ava1/same-console` does): None when it cannot be told in [`MOVE_PROOF_WAIT`].
async fn proves_key(old: &str, new: &str) -> Option<bool> {
    #[cfg(test)]
    {
        let _ = (old, new, MOVE_PROOF_WAIT);
        None
    }
    #[cfg(not(test))]
    {
        let r = tokio::time::timeout(
            MOVE_PROOF_WAIT,
            ps5upload_ava1::pool().same_console(old, new),
        )
        .await
        .ok()?;
        match r {
            ps5upload_ava1::ConsoleMatch::Same => Some(true),
            ps5upload_ava1::ConsoleMatch::Different => Some(false),
            ps5upload_ava1::ConsoleMatch::Unknown => None,
        }
    }
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

    /// A new app paired from `console` (with its AVA1 key and the app's install id, when known):
    /// its id and its token (shown once, to the app). The same app paired before (the same
    /// install id) is replaced: one install, one pairing.
    fn add(
        &mut self,
        console: &str,
        name: Option<String>,
        now: u64,
        console_key: Option<String>,
        app_id: Option<String>,
    ) -> anyhow::Result<(String, String)> {
        let token = random_hex(32).ok_or_else(|| anyhow::anyhow!("no randomness"))?;
        let id = random_hex(4).ok_or_else(|| anyhow::anyhow!("no randomness"))?;
        let before = self.apps.clone();
        if app_id.is_some() {
            self.apps.retain(|a| a.app_id != app_id);
        }
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
            console_key,
            app_id,
        });
        if let Err(e) = self.save() {
            self.apps = before;
            return Err(e);
        }
        Ok((id, token))
    }

    /// App `id`'s console moved to `console` (and has the key `key` there, if known). false: no
    /// such app (unpaired meanwhile).
    fn rebind(&mut self, id: &str, console: &str, key: Option<String>) -> anyhow::Result<bool> {
        let Some(a) = self.apps.iter_mut().find(|a| a.id == id) else {
            return Ok(false);
        };
        let before = a.clone();
        a.console = host_key(console);
        if key.is_some() {
            a.console_key = key;
        }
        if let Err(e) = self.save() {
            if let Some(a) = self.apps.iter_mut().find(|a| a.id == id) {
                *a = before;
            }
            return Err(e);
        }
        Ok(true)
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

    /// App `id` connected: when, and what it did not have yet (a pairing from before this
    /// engine kept the console's key and the app's install id learns them now).
    fn seen(&mut self, id: &str, now: u64, key: Option<String>, app_id: Option<String>) {
        if let Some(a) = self.apps.iter_mut().find(|a| a.id == id) {
            a.last_seen_unix = Some(now);
            if a.console_key.is_none() {
                a.console_key = key;
            }
            if a.app_id.is_none() {
                a.app_id = app_id;
            }
            if let Err(e) = self.save() {
                crate::log_at!(Debug, "together", "play app: {e:#}");
            }
        }
    }
}

#[cfg(unix)]
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
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
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
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
    /// PS5Upload confirmed its console's new address: its token works here now.
    Moved {
        id: String,
    },
    /// PS5Upload said the console at the new address is not the one that paired.
    MoveRefused,
}

/// A paired app's console at a new address that nothing proved (no AVA1 key to compare), waiting
/// for PS5Upload to confirm it (`POST /api/play/app/moved`).
struct PendingMove {
    /// The paired app's id.
    id: String,
    name: Option<String>,
    old: String,
    new: String,
    until: Instant,
    /// The socket that asked, told the answer (it may have closed meanwhile).
    tx: mpsc::Sender<ToApp>,
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

    /// The code waiting on `console`'s TV, confirmed with one click in PS5Upload (it showed the
    /// code to compare). Err: pairing is locked (`Locked`), or nothing waits there (`Wrong(_)`;
    /// a click is not a guess and does not count toward the lock).
    fn take(&mut self, console: &str, now: Instant) -> Result<PendingPair, ConfirmErr> {
        self.prune(now);
        if let Some(s) = self.locked_s(now) {
            return Err(ConfirmErr::Locked(s));
        }
        let want = host_key(console);
        let at = self
            .pending
            .iter()
            .position(|p| p.console == want)
            .ok_or(ConfirmErr::Wrong(BAD_CODES_BEFORE_LOCK - self.bad))?;
        self.bad = 0;
        Ok(self.pending.remove(at))
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
    /// The app's install id, from its hello.
    app_id: Option<String>,
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
    /// Consoles at a new address waiting for PS5Upload's confirmation.
    moves: Vec<PendingMove>,
    /// By console (`host_key`): the grant its helper holds for its PS5Upload Play.
    grants: HashMap<String, Grant>,
    /// By console: its helper is too old for `play.grant` (asked again after this).
    grant_old_helper: HashMap<String, Instant>,
}

/// A one-time grant this engine handed a console's helper: the console's PS5Upload Play sends it
/// in its hello and is paired without a code (the helper's AVA1 session already proves the
/// console). Kept in memory only.
struct Grant {
    /// 32 random bytes, base64url.
    secret: String,
    /// The console (`host_key`) and its AVA1 key, when pinned: the app must connect from there,
    /// or from an address that has the same key (its tailnet one).
    console: String,
    key: Option<String>,
    until: Instant,
    /// When the helper last took it.
    pushed: Option<Instant>,
}

/// Why a grant did not pair.
#[derive(Debug, PartialEq, Eq)]
enum GrantErr {
    Unknown,
    Expired,
    WrongConsole,
}

impl GrantErr {
    fn word(&self) -> &'static str {
        match self {
            GrantErr::Unknown => "unknown",
            GrantErr::Expired => "expired",
            GrantErr::WrongConsole => "wrong_console",
        }
    }
}

/// A new grant: 32 random bytes as 64 hex characters (the helper's `play.grant` takes that form).
fn new_grant_secret() -> Option<String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).ok()?;
    Some(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// A grant as an app's hello carries it: base64url, 16 to 128 characters.
fn valid_grant(s: &str) -> bool {
    (16..=128).contains(&s.len())
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

impl Hub {
    fn store(&mut self) -> &mut Store {
        self.store.get_or_insert_with(|| Store::open(store_path()))
    }

    fn prune_moves(&mut self, now: Instant) {
        self.moves.retain(|m| m.until > now);
    }

    /// App `m.id`'s console asks to be confirmed at `m.new`. true: a new ask (PS5Upload is told);
    /// the same ask again only refreshes it.
    fn ask_move(&mut self, m: PendingMove, now: Instant) -> bool {
        self.prune_moves(now);
        if let Some(p) = self
            .moves
            .iter_mut()
            .find(|p| p.id == m.id && p.new == m.new)
        {
            p.until = m.until;
            p.tx = m.tx;
            return false;
        }
        self.moves.retain(|p| p.id != m.id);
        if self.moves.len() >= MAX_PENDING_MOVES {
            self.moves.remove(0);
        }
        self.moves.push(m);
        true
    }

    /// The grant for `console` (with its AVA1 key `key`, when pinned) to hand its helper now:
    /// the one it holds, renewed for [`GRANT_TTL`], or a new one. `(grant, ttl_s)`.
    fn grant_for(
        &mut self,
        console: &str,
        key: Option<String>,
        now: Instant,
    ) -> Option<(String, u64)> {
        self.grants.retain(|_, g| g.until > now);
        let until = now + GRANT_TTL;
        let c = host_key(console);
        if let Some(g) = self.grants.get_mut(&c) {
            g.until = until;
            if key.is_some() {
                g.key = key;
            }
            return Some((g.secret.clone(), GRANT_TTL.as_secs()));
        }
        let secret = new_grant_secret()?;
        self.grants.insert(
            c.clone(),
            Grant {
                secret: secret.clone(),
                console: c,
                key,
                until,
                pushed: None,
            },
        );
        Some((secret, GRANT_TTL.as_secs()))
    }

    /// Whether `console`'s helper should be handed its grant again.
    fn grant_due(&self, console: &str, now: Instant) -> bool {
        let c = host_key(console);
        if self.grant_old_helper.get(&c).is_some_and(|t| now < *t) {
            return false;
        }
        self.grants
            .get(&c)
            .and_then(|g| g.pushed)
            .is_none_or(|t| now.saturating_duration_since(t) >= GRANT_RENEW)
    }

    /// The grant an app on `console` (whose pinned AVA1 key is `here_key`) sent. Every grant is
    /// compared, in constant time each; the one that matches is used up whatever the outcome
    /// (Ok: the console it was handed to).
    fn take_grant(
        &mut self,
        grant: &str,
        console: &str,
        here_key: Option<&str>,
        now: Instant,
    ) -> Result<String, GrantErr> {
        let mut found = None;
        for (c, g) in &self.grants {
            if same_key(&g.secret, grant) {
                found = Some(c.clone());
            }
        }
        let g = found
            .and_then(|c| self.grants.remove(&c))
            .ok_or(GrantErr::Unknown)?;
        if g.until <= now {
            return Err(GrantErr::Expired);
        }
        let same_key_there =
            matches!((g.key.as_deref(), here_key), (Some(a), Some(b)) if same_key(a, b));
        if g.console != host_key(console) && !same_key_there {
            return Err(GrantErr::WrongConsole);
        }
        Ok(g.console)
    }
}

static HUB: Mutex<Option<Hub>> = Mutex::new(None);

fn with_hub<R>(f: impl FnOnce(&mut Hub) -> R) -> R {
    let mut g = HUB.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Hub::default))
}

#[cfg(test)]
static TEST_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// A PS5Upload Play pairing for `console`, made in a fresh in-memory hub (the real-engine e2e with
/// the app's own code, join.rs `play_cli`): its token.
#[cfg(test)]
pub(crate) fn test_pair(console: &str) -> String {
    *TEST_DIR.lock().unwrap() = None;
    *HUB.lock().unwrap() = None;
    with_hub(|h| {
        h.store()
            .add(console, Some("Test PS5".into()), unix_now(), None, None)
    })
    .unwrap()
    .1
}

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

/// Whether an app on `console` is paired with this engine (the discovery answer's `you.paired`).
pub(crate) fn is_paired(console: &str) -> bool {
    let want = host_key(console);
    with_hub(|h| h.store().apps.iter().any(|a| a.console == want))
}

/// The console an app socket comes from: its address (an IPv4 one as itself, not mapped).
pub(crate) fn console_of(ip: IpAddr) -> String {
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
    // The PS5 user the stream plays as (the console's user in front, else the paired user the
    // session signed in), and who owns the console (the first paired user with a name here, else
    // the stream's user): the TV app offers ending and loosening actions only to them.
    let user =
        crate::join::front_user(console).or_else(|| crate::rp_stream_api::session_user(console));
    let owner = crate::consoles_api::owner_name(console).or_else(|| user.clone());
    let session = match crate::rp_stream_api::live_video(console) {
        Some((codec, kbps)) => {
            json!({"state": "playing", "codec": codec, "bitrate_kbps": kbps, "user": user})
        }
        None => json!({"state": "idle", "user": null}),
    };
    let app = with_hub(|h| h.reports.get(&host_key(console)).cloned()).unwrap_or_default();
    let (helper_version, app_api) = crate::mesh_api::helper_info(console);
    json!({
        "engine": {
            "version": env!("CARGO_PKG_VERSION"),
            "name": crate::mesh_api::device_name(),
        },
        "console": {"addr": console, "name": name, "owner": owner},
        "session": session,
        "together": crate::join::app_view(console, secrets),
        // Remote Play paired with this console on this engine; the helper heard from lately.
        "remote_play": {"paired": crate::consoles_api::rp_paired(console)},
        // `version` and `app_api` (it serves the app's 127.0.0.1:9181) from its last hello
        // answer, null when not known: the app tells "no helper" from "an older helper".
        "helper": {
            "present": crate::mesh_api::helper_seen_s(console)
                .map(|s| s < HELPER_SEEN.as_secs()),
            "version": helper_version,
            "app_api": app_api,
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
    // A gentle per-client bucket first (crate::public_limit), then the per-minute tries.
    if let Some(r) = crate::public_limit::APP.gate(ip) {
        return r;
    }
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
        return crate::public_limit::slow_down(20);
    }
    upgrade
        .max_message_size(APP_MSG_MAX)
        .max_frame_size(APP_MSG_MAX)
        .on_upgrade(move |s| session(s, ip))
}

fn say(v: Value) -> Message {
    Message::Text(v.to_string().into())
}

/// What the app's hello says: its token and its install id, each when it sent a usable one.
#[derive(Clone, Debug, Default, PartialEq)]
struct Hello {
    token: Option<String>,
    app_id: Option<String>,
    /// A grant the console's helper handed it (`GET 127.0.0.1:9181/v1/play/grants`).
    grant: Option<String>,
}

fn parse_hello(v: &Value) -> Option<Hello> {
    let h = v.get("hello")?;
    let text = |k: &str, ok: fn(&str) -> bool| {
        h.get(k)
            .and_then(Value::as_str)
            .filter(|t| ok(t))
            .map(str::to_string)
    };
    Some(Hello {
        token: text("token", |t| !t.is_empty() && t.len() <= 128),
        app_id: text("app_id", valid_app_id),
        grant: text("grant", valid_grant),
    })
}

/// The app's hello (None: no hello in time).
async fn read_hello(socket: &mut WebSocket) -> Option<Hello> {
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
    parse_hello(&v)
}

/// What a token presented from `console` is.
#[derive(Debug, PartialEq)]
enum Auth {
    Paired(String),
    None,
    /// Not a token this engine knows.
    Invalid,
    /// A token made for another console.
    OtherConsole,
    /// A token made for this app at the console's old address, which nothing here can prove
    /// moved: PS5Upload confirms it first (one click).
    ConfirmMove,
}

impl Auth {
    /// The word the app is told, when it is told one.
    fn word(&self) -> Option<&'static str> {
        match self {
            Auth::Invalid => Some("invalid"),
            Auth::OtherConsole => Some("other_console"),
            Auth::ConfirmMove => Some("confirm_move"),
            Auth::Paired(_) | Auth::None => None,
        }
    }
}

/// A valid token made for another address than the one it comes from.
struct Moved {
    id: String,
    name: Option<String>,
    old: String,
    /// The key kept with the pairing, and the one pinned for the new address.
    key: Option<String>,
    here_key: Option<String>,
    /// The hello's install id is the one kept with the pairing.
    same_app: bool,
}

enum Looked {
    Done(Auth),
    Moved(Moved),
}

/// The token as the store has it (blocking: it may read the store and the pins).
fn look_token(hello: &Hello, console: &str) -> Looked {
    let Some(token) = hello.token.as_deref() else {
        return Looked::Done(Auth::None);
    };
    let here_key = key_at(console);
    let here = host_key(console);
    with_hub(|h| {
        let Some(a) = h.store().check(token).cloned() else {
            return Looked::Done(Auth::Invalid);
        };
        if a.console == here {
            h.store()
                .seen(&a.id, unix_now(), here_key, hello.app_id.clone());
            return Looked::Done(Auth::Paired(a.id));
        }
        Looked::Moved(Moved {
            same_app: a.app_id.is_some() && a.app_id == hello.app_id,
            id: a.id,
            name: a.name,
            old: a.console,
            key: a.console_key,
            here_key,
        })
    })
}

/// The token presented from `console` (socket `tx`). A token made at another address acts here
/// when the console proves the AVA1 key kept with it (it moved: the pairing follows it, silently);
/// a console with no key to compare whose app sends the install id kept with the pairing is
/// offered to PS5Upload to confirm (`confirm_move`, and a `play` event `{"moved": …}`).
async fn check_token(hello: &Hello, console: &str, tx: &mpsc::Sender<ToApp>) -> Auth {
    let h = hello.clone();
    let c = console.to_string();
    let m = match tokio::task::spawn_blocking(move || look_token(&h, &c)).await {
        Ok(Looked::Done(a)) => return a,
        Ok(Looked::Moved(m)) => m,
        Err(_) => return Auth::None,
    };
    let same = match (&m.key, &m.here_key) {
        (Some(k), Some(h)) => Some(same_key(k, h)),
        (Some(_), None) => proves_key(&m.old, console).await,
        _ => None,
    };
    match same {
        Some(true) => {
            let (id, c, key) = (m.id.clone(), console.to_string(), m.key.clone());
            let r =
                tokio::task::spawn_blocking(move || with_hub(|h| h.store().rebind(&id, &c, key)))
                    .await;
            match r {
                Ok(Ok(true)) => {
                    crate::log_at!(
                        Info,
                        "together",
                        "play app: the console paired at {} answers at {console} now (the same AVA1 key); its app's pairing moved with it",
                        m.old
                    );
                    Auth::Paired(m.id)
                }
                Ok(Ok(false)) => Auth::Invalid,
                _ => Auth::None,
            }
        }
        Some(false) => Auth::OtherConsole,
        None if m.same_app => {
            let now = Instant::now();
            let name = m.name.clone();
            let asked = with_hub(|h| {
                h.ask_move(
                    PendingMove {
                        id: m.id.clone(),
                        name: m.name.clone(),
                        old: m.old.clone(),
                        new: host_key(console),
                        until: now + MOVE_TTL,
                        tx: tx.clone(),
                    },
                    now,
                )
            });
            if asked {
                crate::log_at!(
                    Info,
                    "together",
                    "play app: the app paired on {} connects from {console}; waiting for PS5Upload to confirm the console moved",
                    m.old
                );
                crate::event_bus::bus().publish_json(
                    "play",
                    &json!({"moved": {
                        "console": host_key(console),
                        "id": m.id,
                        "name": name,
                        "addr_old": m.old,
                        "addr_new": host_key(console),
                    }}),
                );
            }
            Auth::ConfirmMove
        }
        None => Auth::OtherConsole,
    }
}

/// The grant `grant` sent by the app on `console`: a new pairing, exactly as a typed code makes
/// one (`(id, token)`), and the grant is used up.
async fn redeem(
    grant: &str,
    console: &str,
    app_id: Option<String>,
) -> Result<(String, String), GrantErr> {
    let (g, c) = (grant.to_string(), console.to_string());
    let taken = tokio::task::spawn_blocking(move || {
        let here_key = key_at(&c);
        with_hub(|h| h.take_grant(&g, &c, here_key.as_deref(), Instant::now()))
    })
    .await
    .map_err(|_| GrantErr::Unknown)?;
    if let Err(e) = taken {
        crate::log_at!(
            Info,
            "together",
            "play app: a grant from the app on {console} did not pair ({})",
            e.word()
        );
        return Err(e);
    }
    match issue(console, app_id).await {
        Ok((id, token, _)) => {
            // Paired anew here: a move this console waited on is moot.
            let here = host_key(console);
            with_hub(|h| h.moves.retain(|m| m.new != here));
            crate::log_at!(
                Info,
                "together",
                "play app: paired PS5Upload Play on {console} (through the helper, id {id})"
            );
            Ok((id, token))
        }
        Err(e) => {
            crate::log_at!(Warn, "together", "play app: pairing not saved: {e:#}");
            Err(GrantErr::Unknown)
        }
    }
}

async fn session(mut socket: WebSocket, ip: IpAddr) {
    let Some(hello) = read_hello(&mut socket).await else {
        let _ = socket
            .send(say(json!({"error": "hello", "for": "hello"})))
            .await;
        return;
    };
    let console = console_of(ip);
    let conn = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = mpsc::channel::<ToApp>(4);
    let auth = check_token(&hello, &console, &tx).await;
    // Not paired by its token: a grant from the console's helper pairs it at once.
    let granted = match (&auth, &hello.grant) {
        (Auth::Paired(_), _) | (_, None) => None,
        (_, Some(g)) => Some(redeem(g, &console, hello.app_id.clone()).await),
    };
    let mut paired = match (auth, granted) {
        (Auth::Paired(id), _) => Some(id),
        (_, Some(Ok((id, token)))) => {
            if socket
                .send(say(json!({"paired": {"id": id, "token": token}})))
                .await
                .is_err()
            {
                return;
            }
            Some(id)
        }
        (a, g) => {
            if let Some(w) = a.word() {
                let _ = socket.send(say(json!({ "auth": w }))).await;
            }
            if let Some(Err(e)) = g {
                let _ = socket
                    .send(say(json!({"auth": "grant_invalid", "why": e.word()})))
                    .await;
            }
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
                app_id: hello.app_id.clone(),
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
                Some(ToApp::Moved { id }) => {
                    if socket.send(say(json!({"auth": "moved"}))).await.is_err() {
                        break;
                    }
                    with_hub(|h| {
                        if let Some(s) = h.sockets.get_mut(&conn) {
                            s.paired = Some(id.clone());
                        }
                    });
                    paired = Some(id);
                    last = None;
                }
                Some(ToApp::MoveRefused) => {
                    if socket.send(say(json!({"auth": "other_console"}))).await.is_err() {
                        break;
                    }
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
            // true / false, "group" (let every player from that PS5 in: its further controllers
            // get in without asking), "me" (the host's own device: Player 1).
            let d = crate::join::decision_from(&b["allow"]).ok_or("allow")?;
            crate::join::decide_on(console, ask, d).map_err(str::to_string)?;
            Ok(json!({}))
        }
        "kick" => {
            let player = b["player"]
                .as_u64()
                .and_then(|p| u8::try_from(p).ok())
                .ok_or("player")?;
            // Play Rooms: "Don't let them back in this room" / "Block on this computer".
            let block = match b["block"].as_str() {
                None | Some("") => None,
                Some("room") => Some(false),
                Some("always") => Some(true),
                Some(_) => return Err("block".into()),
            };
            if crate::join::room_api::kick_and_block(console, player, block) {
                Ok(json!({}))
            } else {
                Err("no_player".into())
            }
        }
        // Play Rooms (My room on the TV): the room's settings and lock, as `POST /api/join/room`.
        "room" => {
            // `{"room": {"set": {…}}}` or the fields themselves (`{"room": {"lock": true}}`).
            let mut set = b.get("set").cloned().unwrap_or_else(|| b.clone());
            if !set.is_object() {
                return Err("set".into());
            }
            set["addr"] = console.into();
            let body: crate::join::room_api::RoomBody =
                serde_json::from_value(set).map_err(|_| "set".to_string())?;
            tokio::task::spawn_blocking(move || crate::join::room_api::apply_room(&body))
                .await
                .map_err(|_| "failed".to_string())?
                .map_err(|_| "refused".to_string())?;
            Ok(json!({}))
        }
        // The host's mutes (chat and voice, join_comms.rs): one player, or everyone.
        "mute" => {
            let target = if b["all"].as_bool().is_some() {
                crate::join_comms::Target::All
            } else {
                crate::join_comms::Target::Player(
                    b["player"]
                        .as_u64()
                        .and_then(|p| u8::try_from(p).ok())
                        .ok_or("player")?,
                )
            };
            let voice = b["voice"].as_bool().or(b["all"].as_bool());
            let chat = b["chat"].as_bool();
            let all = matches!(target, crate::join_comms::Target::All);
            crate::join_comms::mute(console, target, voice, chat).map_err(str::to_string)?;
            if let (true, Some(v)) = (all, voice) {
                crate::join::room_api::keep_mute_all(console, v).await;
            }
            Ok(json!({}))
        }
        // Move to Player N, Give Player 1, Take Player 1 back.
        "seat" => {
            let player = b["player"]
                .as_u64()
                .and_then(|p| u8::try_from(p).ok())
                .ok_or("player")?;
            let to = crate::join::room_api::move_seat(console, player, &b["to"])
                .await
                .map_err(str::to_string)?;
            Ok(json!({"player": to}))
        }
        // The host's own chat line (join_comms.rs; the notice accepted on the TV when `notice`).
        "chat" => {
            let quick = b["quick"].as_u64().and_then(|n| u8::try_from(n).ok());
            let text = b["text"].as_str().map(str::to_string);
            let notice = b["notice"].as_u64().and_then(|n| u32::try_from(n).ok());
            crate::join_comms::host_says(console, quick, text, notice).map_err(str::to_string)?;
            Ok(json!({}))
        }
        // The room's ready check (everyone in gets "Ready?" for 30 s).
        "ready" => {
            if b["start"].as_bool() != Some(true) {
                return Err("start".into());
            }
            let v = crate::join::room_api::ready_start(console).map_err(str::to_string)?;
            Ok(json!({"ready": v}))
        }
        "unblock" => {
            let tag = b["tag"].as_str().ok_or("tag")?;
            Ok(json!({"ok": crate::join::room_api::unblock_tag(Some(console), tag)}))
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
        "close" => {
            // With friends in, the app asks first and says so (`confirm_friends: true`).
            let friends = crate::join::friends_playing(console);
            if !friends.is_empty() && b["confirm_friends"].as_bool() != Some(true) {
                return Err("friends_playing".into());
            }
            Ok(json!({"was_open": crate::join::close_party(console), "left": friends}))
        }
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
    finish_pair(p).await
}

#[derive(Deserialize)]
pub struct PairConfirmBody {
    /// The console whose TV shows the code (the `pair_waiting` event's `console`).
    console: String,
}

/// `POST /api/play/app/pair_confirm {console}`: pairs the app waiting on that console's TV with one
/// click, exactly as typing its code does (`/api/play/app/pair`): PS5Upload showed the code
/// (`pair_waiting`, or `waiting` in `/api/play/app/paired`) and the user saw the same one on the
/// TV. `{ok, id, console, name}`; 404 `not_waiting` when no code waits there (expired, or the app
/// closed), 429 `locked` with `retry_s`, 409 `app_gone`.
pub async fn pair_confirm(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<PairConfirmBody>,
) -> Response {
    if let Some(r) = crate::join::host_only(&peer, &headers) {
        return r;
    }
    let now = Instant::now();
    match with_hub(|h| h.pairing.take(&b.console, now)) {
        Ok(p) => finish_pair(p).await,
        Err(ConfirmErr::Locked(s)) => bad(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"error": "locked", "retry_s": s}),
        ),
        Err(ConfirmErr::Wrong(_)) => bad(StatusCode::NOT_FOUND, json!({"error": "not_waiting"})),
    }
}

/// A new pairing for the app on `console` (with its install id, when it sent one): saved, with
/// the console's AVA1 key when pinned. `(id, token, name)`.
async fn issue(
    console: &str,
    app_id: Option<String>,
) -> anyhow::Result<(String, String, Option<String>)> {
    let c = console.to_string();
    tokio::task::spawn_blocking(move || {
        let name = crate::consoles_api::console_brief(&c).and_then(|(n, _)| n);
        let key = key_at(&c);
        let (id, token) = with_hub(|h| h.store().add(&c, name.clone(), unix_now(), key, app_id))?;
        Ok((id, token, name))
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?
}

/// The code of `p` was confirmed (typed, or one click): the app is paired and told its token.
async fn finish_pair(p: PendingPair) -> Response {
    let console = p.console.clone();
    let app_id = with_hub(|h| h.sockets.get(&p.conn).and_then(|s| s.app_id.clone()));
    let issued = issue(&console, app_id).await;
    let (id, token, name) = match issued {
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
                h.moves.retain(|m| m.id != id);
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

#[derive(Deserialize)]
pub struct MovedBody {
    /// The address the app paired at (the `moved` event's `addr_old`); optional.
    #[serde(default)]
    console_old: Option<String>,
    /// The console's new address (`addr_new`).
    addr: String,
    /// true: it is the same console; false: it is not (the app is told `other_console`).
    ok: bool,
}

/// `POST /api/play/app/moved {console_old?, addr, ok}`: PS5Upload confirms (or refuses) that the
/// console waiting at `addr` is the one its app paired from (the `play` event `{"moved": …}`).
/// `{"ok": true, "moved": true, "id", "console", "name"}` (the app's token works at `addr` from
/// now on, and its socket is told `moved`) or `{"ok": true, "moved": false}`; 404 `not_waiting`
/// when nothing waits there (or it waited too long), 404 `app_gone` when that app was unpaired
/// meanwhile.
pub async fn moved(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<MovedBody>,
) -> Response {
    if let Some(r) = crate::join::host_only(&peer, &headers) {
        return r;
    }
    let new = host_key(&b.addr);
    let old = b.console_old.as_deref().map(host_key);
    let now = Instant::now();
    let found = with_hub(|h| {
        h.prune_moves(now);
        let at = h
            .moves
            .iter()
            .position(|m| m.new == new && old.as_ref().is_none_or(|o| *o == m.old))?;
        Some(h.moves.remove(at))
    });
    let Some(m) = found else {
        return bad(StatusCode::NOT_FOUND, json!({"error": "not_waiting"}));
    };
    if !b.ok {
        let _ = m.tx.try_send(ToApp::MoveRefused);
        crate::log_at!(
            Info,
            "together",
            "play app: PS5Upload said the console at {new} is not the one paired at {}",
            m.old
        );
        return Json(json!({"ok": true, "moved": false})).into_response();
    }
    let (id, c) = (m.id.clone(), new.clone());
    let r = tokio::task::spawn_blocking(move || {
        let key = key_at(&c);
        with_hub(|h| h.store().rebind(&id, &c, key))
    })
    .await;
    match r {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => return bad(StatusCode::NOT_FOUND, json!({"error": "app_gone"})),
        _ => {
            return bad(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": "save_failed"}),
            )
        }
    }
    let _ = m.tx.try_send(ToApp::Moved { id: m.id.clone() });
    crate::log_at!(
        Info,
        "together",
        "play app: PS5Upload confirmed the console paired at {} is at {new} now; its app's pairing moved with it",
        m.old
    );
    Json(json!({"ok": true, "moved": true, "id": m.id, "console": new, "name": m.name}))
        .into_response()
}

/// `GET /api/play/app/paired`: `{apps: [{id, console, name, paired_unix, last_seen_unix,
/// connected}], waiting: [{console, name, code_display, expires_in_s}], moves: [{id, name,
/// addr_old, addr_new, expires_in_s}], locked_s}`. Never a token. A waiting console's code is
/// listed (this computer's own view) so PS5Upload can ask "Does your TV show KMX-4R7?" and pair
/// it with one click (`POST /api/play/app/pair_confirm`).
pub async fn paired(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = crate::join::host_only(&peer, &headers) {
        return r;
    }
    let now = Instant::now();
    let v = tokio::task::spawn_blocking(move || {
        let mut v = with_hub(|h| {
            h.pairing.prune(now);
            h.prune_moves(now);
            let moves: Vec<Value> = h
                .moves
                .iter()
                .map(|m| {
                    json!({
                        "id": m.id,
                        "name": m.name,
                        "addr_old": m.old,
                        "addr_new": m.new,
                        "expires_in_s": m.until.saturating_duration_since(now).as_secs(),
                    })
                })
                .collect();
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
                        "code_display": crate::join::code_display(&p.code),
                        "expires_in_s": p.until.saturating_duration_since(now).as_secs(),
                    })
                })
                .collect();
            json!({
                "apps": apps,
                "waiting": waiting,
                "moves": moves,
                "locked_s": h.pairing.locked_s(now),
            })
        });
        // The consoles' names, read with the hub let go.
        if let Some(w) = v["waiting"].as_array_mut() {
            for e in w {
                let name = e["console"]
                    .as_str()
                    .and_then(crate::consoles_api::console_brief)
                    .and_then(|(n, _)| n);
                e["name"] = json!(name);
            }
        }
        v
    })
    .await
    .unwrap_or(Value::Null);
    Json(v).into_response()
}

// ── Grants through the helper ────────────────────────────────────────────────────────────────────

/// This engine's id as the Play discovery answer gives it (16 hex characters). The tests never
/// make an identity in the user's data folder.
fn engine_id() -> String {
    #[cfg(test)]
    {
        "0123456789abcdef".to_string()
    }
    #[cfg(not(test))]
    {
        crate::mesh_api::id_hex()
            .map(|h| h.chars().take(16).collect())
            .unwrap_or_default()
    }
}

/// A new AVA1 session to the console at `addr` (its helper may have restarted and forgotten the
/// grant, or been updated): hands it the grant now. Blocking.
pub(crate) fn push_grant(addr: &str) {
    with_hub(|h| h.grant_old_helper.remove(&host_key(addr)));
    send_grant(addr, Instant::now());
}

/// Hands the helper on `addr` this engine's grant for the console's PS5Upload Play (`play.grant`
/// `{engine_id, engine_name, grant, ttl_s, port}`): only for a console on this engine's list,
/// when the consoles can reach this engine. An older helper ("unknown method") is noted and asked
/// again after [`GRANT_OLD_HELPER`]. Blocking. true: the helper took it.
fn send_grant(addr: &str, now: Instant) -> bool {
    let port = crate::mesh_api::play_port_now();
    if port == 0 || crate::consoles_api::console_brief(addr).is_none() {
        return false;
    }
    let console = host_key(addr);
    let key = key_at(addr);
    let Some((grant, ttl_s)) = with_hub(|h| h.grant_for(&console, key, now)) else {
        return false;
    };
    let body = json!({
        "engine_id": engine_id(),
        "engine_name": crate::mesh_api::device_name(),
        "grant": grant,
        "ttl_s": ttl_s,
        "port": port,
    });
    let r = ps5upload_core::mgmt::call(
        addr,
        ps5upload_core::mgmt::m::PLAY_GRANT,
        body.to_string().as_bytes(),
    );
    match r {
        Ok(_) => {
            with_hub(|h| {
                if let Some(g) = h.grants.get_mut(&console) {
                    g.pushed = Some(now);
                }
            });
            true
        }
        Err(e) => {
            if ps5upload_core::console_files::is_unsupported(&e) {
                with_hub(|h| h.grant_old_helper.insert(console, now + GRANT_OLD_HELPER));
                crate::log_at!(
                    Debug,
                    "together",
                    "play app: the helper on {addr} is too old for play.grant"
                );
            } else {
                crate::log_at!(Debug, "together", "play app: play.grant to {addr}: {e:#}");
            }
            false
        }
    }
}

/// Keeps each console's grant fresh while this engine has an AVA1 session to it: handed again,
/// renewed, every [`GRANT_RENEW`] (one look a minute). A console with no session lets its grant
/// run out.
pub(crate) fn spawn_grants() {
    tokio::spawn(async {
        let mut t = tokio::time::interval(GRANT_LOOK);
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            t.tick().await;
            for addr in crate::mesh_api::greeted_consoles() {
                if !with_hub(|h| h.grant_due(&addr, Instant::now())) {
                    continue;
                }
                if !ps5upload_ava1::pool().has_session(&addr).await {
                    continue;
                }
                let _ =
                    tokio::task::spawn_blocking(move || send_grant(&addr, Instant::now())).await;
            }
        }
    });
}

// ── The event feed ───────────────────────────────────────────────────────────────────────────────

/// Publishes a `play` event on the engine's feed whenever a console's status changes, for the
/// consoles with Play Together open or their app connected (one look a second).
pub(crate) fn spawn_feed() {
    tokio::spawn(async move {
        let mut last: HashMap<String, Value> = HashMap::new();
        let mut waiting: HashMap<String, String> = HashMap::new();
        let mut t = tokio::time::interval(Duration::from_secs(1));
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            t.tick().await;
            let r = tokio::task::spawn_blocking(move || {
                feed_round(&mut last);
                waiting_round(&mut waiting, Instant::now());
                (last, waiting)
            })
            .await;
            (last, waiting) = r.unwrap_or_default();
        }
    });
}

/// One look at the pairing codes on the TVs: PS5Upload's own feed gets `{"pair_waiting":
/// {"console", "name", "code_display"}}` for a new code (it shows "Does your TV show KMX-4R7?
/// [Pair]") and `{"pair_ended": {"console"}}` when that code no longer waits (paired, expired, the
/// app gone, or pairing locked). `last`: by console, the code shown last time.
fn waiting_round(last: &mut HashMap<String, String>, now: Instant) {
    let waiting: HashMap<String, String> = with_hub(|h| {
        h.pairing.prune(now);
        h.pairing
            .pending
            .iter()
            .map(|p| (p.console.clone(), crate::join::code_display(&p.code)))
            .collect()
    });
    for (c, code) in &waiting {
        if last.get(c) != Some(code) {
            let name = crate::consoles_api::console_brief(c).and_then(|(n, _)| n);
            crate::event_bus::bus().publish_json(
                "play",
                &json!({"pair_waiting": {"console": c, "name": name, "code_display": code}}),
            );
        }
    }
    for c in last.keys().filter(|c| !waiting.contains_key(*c)) {
        crate::event_bus::bus().publish_json("play", &json!({"pair_ended": {"console": c}}));
    }
    *last = waiting;
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
        .route("/api/play/app/moved", post(moved))
        .route("/api/play/app/pair_confirm", post(pair_confirm))
}

#[cfg(test)]
#[path = "play_app_tests.rs"]
mod tests;
