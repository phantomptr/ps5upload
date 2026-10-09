//! One AVA1 session per console, shared by every job (SPEC.md §6 limits a peer to 12
//! connections per IP; one control connection plus at most 8 lanes is one session).
//!
//! A console keeps one session per identity (SPEC.md §8): two engine processes sharing an
//! identity evict each other. `Churn` notices the symptom and says so.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::session::{connect_expecting, Session, Timing};
use ava1::Ava1Error;

/// `host:9120` — the AVA1 default port. The lab can override an address on
/// its own Pool; a process environment variable cannot redirect engine jobs.
pub fn ava1_addr(console: &str) -> String {
    format!("{}:{}", host_of(console), ava1::gen::DEFAULT_PORT)
}

/// The console string without its port (the pool's key).
pub(crate) fn host_of(console: &str) -> String {
    // `[v6]` and `[v6]:port` keep their brackets; `host:port` loses the port; a bare IPv6
    // literal (several colons, no brackets) has no port to lose.
    if let Some(rest) = console.strip_prefix('[') {
        if let Some(i) = rest.find(']') {
            return format!("[{}]", &rest[..i]);
        }
        return console.to_string();
    }
    match console.split_once(':') {
        Some((h, port)) if !port.contains(':') => h.to_string(),
        _ => console.to_string(),
    }
}

/// `ERR_BUSY` on a `JobOpen` is retried this many times (jittered doubling backoff, 250 ms to 5 s: about 45 s in
/// all) before the transfer fails with a clear reason. The console says BUSY while it recovers a job's files or
/// has no room for another job; it is never a verdict on the transfer.
pub(crate) const DEFAULT_BUSY_TRIES: u32 = 12;

pub struct Pool {
    dir: PathBuf,
    /// No identity is an error, not a panic (C4): `session()` turns the reason into an
    /// `Ava1Error::Io` — "not paired" would misdescribe a missing identity file, so
    /// the honest error wins.
    me: Result<Arc<Identity>, String>,
    peers: Arc<Mutex<PeerStore>>,
    sessions: Arc<tokio::sync::Mutex<HashMap<String, Cached>>>,
    /// Sessions that ended under us, for the one-session-per-identity warning.
    churn: Arc<Churn>,
    /// Test/lab override: every console resolves to this address (A1). Never set in
    /// the engine.
    addr: Option<String>,
    /// Connection attempts so far. A test seam (A4): the negative cache's hit path is
    /// pinned by counting, never by sleeping.
    attempts: AtomicUsize,
    /// Job directories (hex job ids) of the jobs running in this process, with a count each:
    /// the journal sweep never touches them (SPEC.md §14.3).
    live: Mutex<HashMap<String, usize>>,
    /// One connect at a time per console: the console keeps one session per identity, so a
    /// second concurrent connect would end the first one's session (SPEC.md §8). Callers that
    /// find no session wait here and then find the winner's.
    connecting: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Connections whose pairing a person has not confirmed yet, one per console: the code on
    /// screen belongs to that handshake, so it is held (not redone) until `confirm_pairing`.
    pending: tokio::sync::Mutex<HashMap<String, Session>>,
    /// Recent session failures by console host, so a client that polls many endpoints does not
    /// open a handshake (an unpaired console may show a pairing code per handshake, and the
    /// console caps connections per IP) or block on an unreachable console for every call.
    refusals: Mutex<HashMap<String, Refusal>>,
    refusal_ttl: Duration,
    /// How long "this console is not paired" is remembered: longer than any status poll, so a
    /// poll never opens a handshake (and a console pop-up) for a console known to need a code.
    not_paired_ttl: Duration,
    /// How many times a `JobOpen` answered `ERR_BUSY` is retried before the transfer gives up.
    busy_tries: u32,
    /// Overrides the JobOpenAck timeout (tests).
    open_ack_timeout: Option<Duration>,
    /// Overrides how an upload learns the destination's free space (tests); `None` asks the
    /// console (`fs.volumes`).
    room_probe: Option<crate::space::RoomProbe>,
}

/// A remembered session failure: when it happened and the reason/detail to repeat.
#[derive(Clone)]
struct Refusal {
    at: Instant,
    reason: String,
    detail: String,
}

/// How long a failed session attempt is repeated without trying again.
pub const REFUSAL_TTL: Duration = Duration::from_secs(5);
/// How long a `ava1_not_paired` refusal is repeated: it outlasts the client's 10 s status poll.
/// A pairing attempt (the dialog opening, a confirm, a dismissal) resets it.
pub const NOT_PAIRED_TTL: Duration = Duration::from_secs(30);

/// Where a console stands for the pairing dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pairing {
    /// The console trusts this engine (or was launched by it): nothing to enter.
    Paired,
    /// A person must read the code off the console's screen and type it in the app (SPEC.md
    /// §5.5). This side does not know the code: it exists only on the console's screen.
    Code { peer_name: String },
    /// The console refused the code that was typed. A new handshake is pending and the console
    /// shows a new code: the user reads it and types again.
    WrongCode { peer_name: String },
    /// A different console answered at this address than the one this engine pinned.
    WrongConsole,
    /// The console is not accepting new pairings (its window is closed).
    Closed,
}

/// Sessions that ended under us this many times within `Churn::window` mean something else
/// keeps taking them: almost always a second engine using the same identity.
const CHURN_DEATHS: usize = 3;
const CHURN_WINDOW: Duration = Duration::from_secs(120);
/// A session that ends this soon after `forget` is the engine's own doing, not an eviction.
const CHURN_FORGET_GRACE: Duration = Duration::from_secs(5);

/// The shared message, also quoted by SPEC.md §8.
pub const SUPERSEDED_WARNING: &str = "another ps5upload engine using the same identity is connected to this console; the console keeps one session per identity, so the two engines keep evicting each other (give each engine its own data directory, i.e. its own identity)";

/// A cached session and whether its end has been counted yet (the watcher and the lookup
/// that finds it closed both may see it first).
struct Cached {
    session: Arc<Session>,
    counted: Arc<std::sync::atomic::AtomicBool>,
}

struct Churn {
    deaths_to_warn: usize,
    window: Duration,
    deaths: Mutex<HashMap<String, VecDeque<Instant>>>,
    forgotten: Mutex<HashMap<String, Instant>>,
    warned: Mutex<HashMap<String, Instant>>,
    warnings: AtomicUsize,
}

impl Default for Churn {
    fn default() -> Self {
        Self::new(CHURN_DEATHS, CHURN_WINDOW)
    }
}

impl Churn {
    fn new(deaths_to_warn: usize, window: Duration) -> Self {
        Churn {
            deaths_to_warn,
            window,
            deaths: Mutex::default(),
            forgotten: Mutex::default(),
            warned: Mutex::default(),
            warnings: AtomicUsize::new(0),
        }
    }

    fn forgot(&self, host: &str) {
        let mut f = self.forgotten.lock().unwrap_or_else(|e| e.into_inner());
        f.insert(host.to_string(), Instant::now());
    }

    /// A cached session for `host` ended on its own (`why` is what the link reported).
    fn died(&self, host: &str, why: &str) {
        let now = Instant::now();
        let ours = self
            .forgotten
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(host)
            .is_some_and(|t| now.duration_since(*t) < CHURN_FORGET_GRACE);
        if ours {
            return;
        }
        ps5upload_core::events::emit(
            ps5upload_core::events::Cat::Connection,
            ps5upload_core::events::Level::Warn,
            "session_died",
            Some(host),
            why.to_string(),
        );
        let n = {
            let mut d = self.deaths.lock().unwrap_or_else(|e| e.into_inner());
            let q = d.entry(host.to_string()).or_default();
            q.push_back(now);
            while q
                .front()
                .is_some_and(|t| now.duration_since(*t) > self.window)
            {
                q.pop_front();
            }
            q.len()
        };
        if n < self.deaths_to_warn {
            return;
        }
        {
            // At most once per window per console: the warning must not become the spam.
            let mut w = self.warned.lock().unwrap_or_else(|e| e.into_inner());
            if w.get(host)
                .is_some_and(|t| now.duration_since(*t) < self.window)
            {
                return;
            }
            w.insert(host.to_string(), now);
        }
        self.warnings.fetch_add(1, Ordering::Relaxed);
        use std::io::Write;
        // Never `eprintln!`: a closed stderr panics it.
        let _ = writeln!(
            std::io::stderr(),
            "ava1: warning: {SUPERSEDED_WARNING} (console {host}: its session ended {n} times in {}s; last reason: {why})",
            self.window.as_secs()
        );
    }
}

/// A job running in this process (see `Pool::live_job`).
pub struct LiveJob<'a> {
    pool: &'a Pool,
    name: String,
}

impl Drop for LiveJob<'_> {
    fn drop(&mut self) {
        let mut l = self.pool.live.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = l.get_mut(&self.name) {
            *n -= 1;
            if *n == 0 {
                l.remove(&self.name);
            }
        }
    }
}

/// SPEC.md §14.3: a job directory idle for more than this is removed.
pub const JOURNAL_MAX_AGE_S: u64 = 7 * 24 * 3600;
/// How often the engine sweeps (and once at start).
pub const JOURNAL_GC_EVERY: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

impl Pool {
    pub(crate) fn unavailable() -> Pool {
        Pool {
            dir: PathBuf::new(),
            me: Err("no PS5Upload data directory; AVA1 identity unavailable".into()),
            peers: Arc::new(Mutex::new(PeerStore::in_memory())),
            sessions: Arc::default(),
            churn: Arc::new(Churn::default()),
            addr: None,
            attempts: AtomicUsize::new(0),
            live: Mutex::default(),
            connecting: Mutex::default(),
            pending: Default::default(),
            refusals: Mutex::default(),
            refusal_ttl: REFUSAL_TTL,
            not_paired_ttl: NOT_PAIRED_TTL,
            busy_tries: DEFAULT_BUSY_TRIES,
            open_ack_timeout: None,
            room_probe: None,
        }
    }

    /// Test seam: where an upload's free-space check gets the destination's room.
    pub fn with_room_probe(mut self, probe: crate::space::RoomProbe) -> Pool {
        self.room_probe = Some(probe);
        self
    }

    pub(crate) fn room_probe(&self) -> crate::space::RoomProbe {
        self.room_probe
            .clone()
            .unwrap_or_else(crate::space::volumes_probe)
    }

    /// Overrides how many times a BUSY `JobOpen` is retried (tests; the default suits a console that is
    /// finishing another job's files).
    pub fn with_busy_tries(mut self, n: u32) -> Pool {
        self.busy_tries = n;
        self
    }

    /// Overrides how long an upload waits for a JobOpenAck before retrying it (tests).
    pub fn with_open_ack_timeout(mut self, t: Duration) -> Pool {
        self.open_ack_timeout = Some(t);
        self
    }

    pub(crate) fn open_ack_timeout(&self) -> Option<Duration> {
        self.open_ack_timeout
    }

    pub(crate) fn busy_tries(&self) -> u32 {
        self.busy_tries
    }

    pub fn new(dir: PathBuf) -> Pool {
        let _ = std::fs::create_dir_all(&dir);
        let me = Identity::load_or_create(&dir.join("identity"))
            .map(Arc::new)
            .map_err(|e| {
                format!(
                    "no AVA1 identity at {}: {e}",
                    dir.join("identity").display()
                )
            });
        // C3: an unreadable peers file is kept as the fact (every write refuses and
        // `unreadable()` says why), never silently replaced by an empty store.
        let peers = PeerStore::load_or_unreadable(&dir.join("peers"));
        Pool {
            dir,
            me,
            peers: Arc::new(Mutex::new(peers)),
            sessions: Arc::default(),
            churn: Arc::new(Churn::default()),
            addr: None,
            attempts: AtomicUsize::new(0),
            live: Mutex::default(),
            connecting: Mutex::default(),
            pending: Default::default(),
            refusals: Mutex::default(),
            refusal_ttl: REFUSAL_TTL,
            not_paired_ttl: NOT_PAIRED_TTL,
            busy_tries: DEFAULT_BUSY_TRIES,
            open_ack_timeout: None,
            room_probe: None,
        }
    }

    /// Test/lab override: every console resolves to this address. Never set in the
    /// engine (A1).
    pub fn with_addr(mut self, addr: impl Into<String>) -> Pool {
        self.addr = Some(addr.into());
        self
    }

    /// Test seam: how long a not-paired refusal is remembered.
    pub fn with_not_paired_ttl(mut self, ttl: Duration) -> Pool {
        self.not_paired_ttl = ttl;
        self
    }

    /// Test seam: how long a failed session attempt is remembered.
    pub fn with_refusal_ttl(mut self, ttl: Duration) -> Pool {
        self.refusal_ttl = ttl;
        self
    }

    /// The failure (`reason`, `detail`) of a session attempt to `console` within the last
    /// [`REFUSAL_TTL`], if any.
    pub(crate) fn recent_refusal(&self, console: &str) -> Option<(String, String)> {
        let host = host_of(console);
        let mut m = self.refusals.lock().unwrap_or_else(|e| e.into_inner());
        match m.get(&host) {
            Some(r)
                if r.at.elapsed()
                    < if r.reason == "ava1_not_paired" {
                        self.not_paired_ttl
                    } else {
                        self.refusal_ttl
                    } =>
            {
                Some((r.reason.clone(), r.detail.clone()))
            }
            Some(_) => {
                m.remove(&host);
                None
            }
            None => None,
        }
    }

    pub(crate) fn note_refusal(&self, console: &str, reason: &str, detail: &str) {
        self.refusals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                host_of(console),
                Refusal {
                    at: Instant::now(),
                    reason: reason.to_string(),
                    detail: detail.to_string(),
                },
            );
    }

    /// Forgets a remembered failure: a pairing or any successful session just proved the
    /// console usable, so the next call must not repeat the old answer.
    pub fn clear_refusal(&self, console: &str) {
        self.refusals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&host_of(console));
    }

    pub fn ava_dir(&self) -> &Path {
        &self.dir
    }

    /// Marks the job with this id as running here until the guard drops: its directories
    /// under `jobs/` and `send/` are not swept meanwhile.
    pub fn live_job(&self, id: &[u8; 16]) -> LiveJob<'_> {
        let name = ava1::hex::encode(id);
        *self
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(name.clone())
            .or_insert(0) += 1;
        LiveJob { pool: self, name }
    }

    /// SPEC.md §14.3: removes job directories under `<ava dir>/jobs` (receiver journals) and
    /// `<ava dir>/send` (sender outboards) idle for more than `max_age_s` as of `now_unix`,
    /// except jobs running in this process. Returns how many were removed. Blocking file I/O:
    /// call it off the async runtime.
    pub fn gc_journals(&self, now_unix: u64, max_age_s: u64) -> usize {
        if self.dir.as_os_str().is_empty() {
            return 0; // an unavailable pool has no directory
        }
        let live = |name: &str| {
            self.live
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(name)
        };
        ["jobs", "send"]
            .iter()
            .map(|sub| {
                ava1::journal::gc_except(&self.dir.join(sub), now_unix, max_age_s, &live)
                    .unwrap_or(0)
            })
            .sum()
    }

    pub fn has_identity(&self) -> bool {
        self.me.is_ok()
    }

    /// The first eight hex digits of this engine's public key, for the startup line (`none`
    /// when there is no identity).
    pub fn identity_prefix(&self) -> String {
        match &self.me {
            Ok(me) => ava1::hex::encode(&me.public())[..8].to_string(),
            Err(_) => "none".to_string(),
        }
    }

    /// How many consoles have accepted this engine (its paired peers).
    pub fn paired_count(&self) -> usize {
        self.peers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .list()
            .len()
    }

    /// The address `session()` connects to: the per-pool override first (A1: the
    /// engine's pools never carry one), then the console's own address.
    fn addr_for(&self, console: &str) -> String {
        match &self.addr {
            Some(a) => a.clone(),
            None => ava1_addr(console),
        }
    }

    /// The key pinned for this console's address, if one was recorded. A *changed* key
    /// at a known address is refused by `connect_expecting` (`WrongPeer`), never
    /// re-pinned silently — the identity pinning's whole point.
    fn pinned(&self, host: &str) -> Option<[u8; 32]> {
        let text = std::fs::read_to_string(self.dir.join("consoles")).ok()?;
        for line in text.lines() {
            // Tolerant parser: unattributable lines are skipped.
            let Some((h, k)) = line.split_once(' ') else {
                continue;
            };
            if h != host {
                continue;
            }
            if let Some(k) = ava1::hex::decode(k.trim()) {
                if let Ok(k) = k.try_into() {
                    return Some(k);
                }
            }
        }
        None
    }

    /// Records `host → key`, first pin only: a known host's line is replaced with the
    /// same key, never re-pinned to a different one (a changed key there is refused by
    /// `connect_expecting`, and this only runs after that passed). Written through a
    /// temp file + rename, the same shape the peer store uses.
    fn pin(&self, host: &str, key: [u8; 32]) {
        self.rewrite_pins(|lines| {
            lines.retain(|l| l.split_once(' ').map(|(h, _)| h) != Some(host));
            lines.push(format!("{host} {}", ava1::hex::encode(&key)));
        });
    }

    /// Read-modify-write of the pins file under its advisory lock, through a temp file no
    /// other writer shares: the engine and the desktop app may both pin.
    fn rewrite_pins(&self, change: impl FnOnce(&mut Vec<String>)) {
        let p = self.dir.join("consoles");
        let Ok(_lock) = ava1::fslock::lock(&p) else {
            return;
        };
        let mut lines: Vec<String> = std::fs::read_to_string(&p)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect();
        change(&mut lines);
        let body = if lines.is_empty() {
            String::new()
        } else {
            lines.join("\n") + "\n"
        };
        let tmp = ava1::fslock::TmpGuard::new(ava1::fslock::unique_tmp(&p));
        if std::fs::write(tmp.path(), body).is_ok() && std::fs::rename(tmp.path(), &p).is_ok() {
            tmp.disarm();
        }
    }

    /// Removes the pin for `host` (see `forget_console_key`).
    fn unpin(&self, host: &str) {
        self.rewrite_pins(|lines| {
            lines.retain(|l| l.split_once(' ').map(|(h, _)| h) != Some(host));
        });
    }

    /// One handshake with the console, pairing not yet settled; and the key it was pinned to.
    async fn connect_raw(&self, console: &str) -> Result<(Session, Option<[u8; 32]>), Ava1Error> {
        let me = self
            .me
            .clone()
            .map_err(|why| Ava1Error::Io(io::Error::other(why)))?;
        let pin = self.pinned(&host_of(console));
        self.attempts.fetch_add(1, Ordering::Relaxed);
        let s = connect_expecting(
            &self.addr_for(console),
            pin,
            me,
            self.peers.clone(),
            "ps5upload",
            Timing::default(),
        )
        .await?;
        Ok((s, pin))
    }

    /// For the pairing dialog: is this console paired, and if a person must compare codes,
    /// which code? The handshake that produced the code is kept until `confirm_pairing`, so
    /// asking again shows the same code (the console's screen shows the one it made).
    pub async fn pairing_status(&self, console: &str) -> Result<Pairing, Ava1Error> {
        let host = host_of(console);
        // The user is looking at the dialog: whatever the polls remembered is stale.
        self.clear_refusal(console);
        let one_at_a_time = self
            .connecting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(host.clone())
            .or_default()
            .clone();
        {
            let _connecting = one_at_a_time.lock().await;
            let mut pending = self.pending.lock().await;
            if let Some(s) = pending.get(&host) {
                if !s.is_closed() && s.pairing_pending() {
                    return Ok(Pairing::Code {
                        peer_name: s.peer_name().to_string(),
                    });
                }
                pending.remove(&host);
            }
            drop(pending);
            let live = {
                let map = self.sessions.lock().await;
                map.get(&host).is_some_and(|c| !c.session.is_closed())
            };
            if live {
                return Ok(Pairing::Paired);
            }
            match self.connect_raw(console).await {
                Ok((s, _)) if s.needs_user_pairing() => {
                    let peer_name = s.peer_name().to_string();
                    self.pending.lock().await.insert(host.clone(), s);
                    return Ok(Pairing::Code { peer_name });
                }
                Ok((s, _)) => s.close().await, // already trusted: the cached path below
                Err(Ava1Error::Refused { code, .. }) if code == ava1::gen::ERR_PAIRING_CLOSED => {
                    return Ok(Pairing::Closed)
                }
                Err(Ava1Error::WrongPeer) => return Ok(Pairing::WrongConsole),
                Err(e) => return Err(e),
            }
        }
        self.session(console).await?;
        Ok(Pairing::Paired)
    }

    /// What the pool already knows about pairing with `console`, with no handshake and no
    /// other side effect (the read-only `GET`): `Paired` for a live session, `Code` while a
    /// dialog's handshake is pending, `Closed` is never guessed. `None`: nothing in progress
    /// (starting one is `pairing_status`, a `POST`).
    pub async fn peek_pairing(&self, console: &str) -> Option<Pairing> {
        let host = host_of(console);
        if let Some(s) = self.pending.lock().await.get(&host) {
            if !s.is_closed() && s.pairing_pending() {
                return Some(Pairing::Code {
                    peer_name: s.peer_name().to_string(),
                });
            }
        }
        let map = self.sessions.lock().await;
        map.get(&host)
            .is_some_and(|c| !c.session.is_closed())
            .then_some(Pairing::Paired)
    }

    /// The user typed the code the console shows (passkey entry, SPEC.md §5.5): the app checks
    /// it against its own derivation, the console against its own, and only then is the
    /// console's key stored (and ours told to it). Then opens the session every job will
    /// share. A typo is caught by the app and keeps the handshake (the console is not told);
    /// a code the console refused uses the session up.
    pub async fn confirm_pairing(&self, console: &str, typed: u32) -> Result<(), Ava1Error> {
        let host = host_of(console);
        let one_at_a_time = self
            .connecting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(host.clone())
            .or_default()
            .clone();
        {
            let _connecting = one_at_a_time.lock().await;
            let taken = self.pending.lock().await.remove(&host);
            let Some(mut s) = taken.filter(|s| !s.is_closed()) else {
                return Err(Ava1Error::NotPaired);
            };
            if let Err(e) = s.confirm_pairing(typed).await {
                // The console refused the code (or could not prove its own): the session is
                // used up. A retry is a new handshake with a new code on the console.
                s.close().await;
                return Err(e);
            }
            if self.pinned(&host).is_none() {
                self.pin(&host, s.peer_key());
            }
            s.close().await;
        }
        // The console now trusts this engine: an ordinary connect, no code.
        self.session(console).await?;
        Ok(())
    }

    /// A confirm that never fails just because nothing is pending: a late or concurrent
    /// confirm (the handshake was already confirmed, or it timed out) answers with the
    /// console's current state (`Paired`, a fresh `Code`, or `Closed`) instead of an error.
    /// A code the console refused starts a new handshake at once and answers `WrongCode` (a
    /// new code is on its screen), or `Closed` when five wrong codes shut its window. This side
    /// cannot tell a typo from a wrong guess: only the console knows the code.
    pub async fn confirm_or_status(&self, console: &str, typed: u32) -> Result<Pairing, Ava1Error> {
        match self.confirm_pairing(console, typed).await {
            Ok(()) => Ok(Pairing::Paired),
            Err(Ava1Error::NotPaired) => self.pairing_status(console).await,
            Err(Ava1Error::Refused { code, .. })
                if code == ava1::gen::ERR_PAIRING_CODE || code == ava1::gen::ERR_PAIRING_CLOSED =>
            {
                match self.pairing_status(console).await? {
                    Pairing::Code { peer_name, .. } => Ok(Pairing::WrongCode { peer_name }),
                    other => Ok(other),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// The dialog was dismissed: closes the pending handshake (it holds one of the console's
    /// two unconfirmed places until its 60 s deadline) and remembers "not paired" so the
    /// status poll does not open another. True when something was pending.
    pub async fn cancel_pairing(&self, console: &str) -> bool {
        let host = host_of(console);
        let one_at_a_time = self
            .connecting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(host.clone())
            .or_default()
            .clone();
        let _connecting = one_at_a_time.lock().await;
        let taken = self.pending.lock().await.remove(&host);
        self.note_refusal(
            console,
            "ava1_not_paired",
            "the devices are not paired yet (pairing was dismissed)",
        );
        match taken {
            Some(s) => {
                s.close().await;
                true
            }
            None => false,
        }
    }

    /// "Forget this console's key": a different console answered at this address (a DHCP
    /// swap, a replacement PS5), and the pin made the old one's key the only one accepted
    /// here. Removes the pin, the old console's stored key, and every cached session,
    /// pending handshake and remembered failure for the address; the next pairing pins
    /// whichever console answers. True when a pin was removed.
    pub async fn forget_console_key(&self, console: &str) -> bool {
        let host = host_of(console);
        let one_at_a_time = self
            .connecting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(host.clone())
            .or_default()
            .clone();
        let _connecting = one_at_a_time.lock().await;
        let old = self.pinned(&host);
        self.unpin(&host);
        if let Some(key) = old {
            let _ = self
                .peers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
        }
        if let Some(s) = self.pending.lock().await.remove(&host) {
            s.close().await;
        }
        let cached = self.sessions.lock().await.remove(&host);
        if let Some(c) = cached {
            self.churn.forgot(&host);
            drop(c);
        }
        self.clear_refusal(console);
        old.is_some()
    }

    /// One live session for the console, connecting when there is none. C17: the lock
    /// is never held across the handshake (up to `Timing::handshake`) — one
    /// unreachable console must not stall every other console's transfer. If another
    /// caller won the race, theirs is kept and the loser closed: SPEC §6 caps a peer
    /// at 12 connections per IP, so a leaked session is not free.
    pub async fn session(&self, console: &str) -> Result<Arc<Session>, Ava1Error> {
        let host = host_of(console);
        let one_at_a_time = self
            .connecting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(host.clone())
            .or_default()
            .clone();
        let _connecting = one_at_a_time.lock().await;
        {
            let mut map = self.sessions.lock().await;
            if let Some(c) = map.get(&host) {
                if c.session.is_closed() {
                    let c = map.remove(&host).expect("just seen");
                    if !c.counted.swap(true, Ordering::SeqCst) {
                        self.churn.died(&host, "connection closed");
                    }
                } else {
                    return Ok(c.session.clone());
                }
            }
        }
        // C4: no identity is an `Io` error, not `NotPaired` — the latter's message
        // ("the devices are not paired yet") would misdescribe a missing identity
        // file, so the honest error wins.
        // A dialog is waiting on a code: a poll must not open another handshake (it would take
        // one of the console's unconfirmed places and show another pop-up).
        if self
            .pending
            .lock()
            .await
            .get(&host)
            .is_some_and(|p| !p.is_closed())
        {
            return Err(Ava1Error::NotPaired);
        }
        let (mut s, pin) = self.connect_raw(console).await?;
        if s.needs_user_pairing() {
            // A person must compare codes (SPEC.md §5), not a transfer.
            return Err(Ava1Error::NotPaired);
        }
        if s.pairing_pending() {
            // The console already trusts us (it was launched by us): record its key.
            s.confirm_trusted()?;
        }
        if pin.is_none() {
            self.pin(&host, s.peer_key());
        }
        let s = Arc::new(s);
        self.clear_refusal(&host);
        let mut map = self.sessions.lock().await;
        let kept = match map.get(&host) {
            Some(kept) if !kept.session.is_closed() => Some(kept.session.clone()),
            _ => None,
        };
        if let Some(kept) = kept {
            drop(map);
            if let Some(loser) = Arc::into_inner(s) {
                loser.close().await;
            }
            Ok(kept)
        } else {
            let counted = Arc::new(std::sync::atomic::AtomicBool::new(false));
            map.insert(
                host.clone(),
                Cached {
                    session: s.clone(),
                    counted: counted.clone(),
                },
            );
            drop(map);
            // Count how often this session ends on its own, so a second engine on the same
            // identity (which the console resolves by ending the older session) is named.
            // Polls through a `Weak`: a watcher must not keep a session (and its console
            // connections) alive once every user has dropped it.
            let (watched, churn) = (Arc::downgrade(&s), self.churn.clone());
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let Some(s) = watched.upgrade() else { return };
                    if s.is_closed() {
                        let why = s.closed().await;
                        if !counted.swap(true, Ordering::SeqCst) {
                            churn.died(&host, &why);
                        }
                        return;
                    }
                }
            });
            Ok(s)
        }
    }

    /// Drops the cached session only when it is `failed` (the one the caller just saw break) or
    /// is already closed; a newer healthy session that another job cached meanwhile stays
    /// (final review #4). Every job and management call to a console shares one session and the
    /// console ends the older one when a new handshake arrives (SPEC section 8), so an
    /// unconditional forget by a job that lost session S1 would evict the S2 another job had
    /// just made, and the two would keep ending each other's. Whether to `close()` the
    /// session is the caller's decision. Async: it takes the sessions lock (C2).
    pub async fn forget_if(&self, console: &str, failed: &Arc<Session>) {
        let host = host_of(console);
        let mut map = self.sessions.lock().await;
        let drop_it = map
            .get(&host)
            .is_some_and(|c| Arc::ptr_eq(&c.session, failed) || c.session.is_closed());
        if drop_it {
            self.churn.forgot(&host);
            map.remove(&host);
        }
    }

    /// Drops the cached session whichever it is: a deliberate reset (a bench's cold start, a
    /// test), never a failure path, which must use [`Pool::forget_if`].
    pub async fn forget(&self, console: &str) {
        let host = host_of(console);
        self.churn.forgot(&host);
        self.sessions.lock().await.remove(&host);
    }

    /// Test seam: warn after `deaths` unexpected session ends within `window`.
    pub fn with_churn(mut self, deaths: usize, window: Duration) -> Pool {
        self.churn = Arc::new(Churn::new(deaths, window));
        self
    }

    /// How many times the one-session-per-identity warning was logged (test seam).
    pub fn superseded_warnings(&self) -> usize {
        self.churn.warnings.load(Ordering::Relaxed)
    }

    /// Connection attempts so far (test seam, A4).
    pub fn attempts(&self) -> usize {
        self.attempts.load(Ordering::Relaxed)
    }
}

/// `<data dir>/ava` — the same rules as the engine's `remote::store::data_dir()`.
/// Without a data directory no identity is created in the current directory.
fn data_dir() -> Option<PathBuf> {
    let data = std::env::var("PS5UPLOAD_DATA_DIR").ok();
    let home = std::env::var("HOME").ok();
    let profile = std::env::var("USERPROFILE").ok();
    data_dir_from(data.as_deref(), home.as_deref(), profile.as_deref())
}

fn data_dir_from(data: Option<&str>, home: Option<&str>, profile: Option<&str>) -> Option<PathBuf> {
    data.filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|v| !v.trim().is_empty())
                .or_else(|| profile.filter(|v| !v.trim().is_empty()))
                .map(|v| PathBuf::from(v).join(".ps5upload"))
        })
}

/// The process's pool (identity, peers and pins under `<data dir>/ava`).
pub fn pool() -> &'static Pool {
    static P: OnceLock<Pool> = OnceLock::new();
    P.get_or_init(|| match data_dir() {
        Some(dir) => Pool::new(dir.join("ava")),
        None => {
            // Once per process: this initialiser runs once. Never `eprintln!` (a
            // closed stderr panics it).
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr(),
                "ava1: no PS5Upload data directory (set PS5UPLOAD_DATA_DIR or HOME); AVA1 is unavailable"
            );
            Pool::unavailable()
        }
    })
}

#[cfg(test)]
mod identity_summary_tests {
    use super::*;

    #[test]
    fn the_startup_summary_names_the_key_and_the_paired_count() {
        let d = std::env::temp_dir().join(format!("p5a-summary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let p = Pool::new(d.join("ava"));
        assert_eq!(p.identity_prefix().len(), 8);
        assert_eq!(p.paired_count(), 0);
        assert_eq!(Pool::unavailable().identity_prefix(), "none");
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod host_of_tests {
    use super::host_of;

    #[test]
    fn the_pool_key_is_the_host_for_every_spelling() {
        assert_eq!(host_of("10.0.0.2"), "10.0.0.2");
        assert_eq!(host_of("10.0.0.2:9113"), "10.0.0.2");
        assert_eq!(host_of("ps5.lan:9120"), "ps5.lan");
        assert_eq!(host_of("[::1]"), "[::1]");
        assert_eq!(host_of("[::1]:9113"), "[::1]");
        assert_eq!(host_of("fe80::1"), "fe80::1");
    }
}

#[cfg(test)]
mod data_dir_tests {
    use super::data_dir_from;

    #[test]
    fn no_data_directory_never_falls_back_to_the_current_directory() {
        assert_eq!(data_dir_from(None, None, None), None);
        assert_eq!(data_dir_from(Some(" "), Some(""), None), None);
    }
}

#[cfg(test)]
mod gc_tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ps5u-poolgc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn age(dir: &Path, secs: u64) {
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
        for p in [dir.to_path_buf(), dir.join("journal")] {
            if let Ok(f) = std::fs::File::open(&p) {
                f.set_modified(t).unwrap();
            }
        }
    }

    #[test]
    fn gc_sweeps_jobs_and_send_after_seven_days_but_never_a_live_job() {
        let base = tmp("sweep");
        let pool = Pool::new(base.join("ava"));
        let d = pool.ava_dir().to_path_buf();
        let old_job = [1u8; 16];
        let live_job = [2u8; 16];
        let fresh_job = [3u8; 16];
        for sub in ["jobs", "send"] {
            for id in [&old_job, &live_job, &fresh_job] {
                let p = d.join(sub).join(ava1::hex::encode(id));
                std::fs::create_dir_all(&p).unwrap();
                std::fs::write(p.join("journal"), b"x").unwrap();
            }
        }
        let eight_days = 8 * 24 * 3600;
        for sub in ["jobs", "send"] {
            for id in [&old_job, &live_job] {
                age(&d.join(sub).join(ava1::hex::encode(id)), eight_days);
            }
        }
        let guard = pool.live_job(&live_job);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(
            pool.gc_journals(now, JOURNAL_MAX_AGE_S),
            2,
            "one per directory"
        );
        for sub in ["jobs", "send"] {
            assert!(!d.join(sub).join(ava1::hex::encode(&old_job)).exists());
            assert!(d.join(sub).join(ava1::hex::encode(&live_job)).exists());
            assert!(d.join(sub).join(ava1::hex::encode(&fresh_job)).exists());
        }
        drop(guard);
        assert_eq!(
            pool.gc_journals(now, JOURNAL_MAX_AGE_S),
            2,
            "the finished job expires too"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
