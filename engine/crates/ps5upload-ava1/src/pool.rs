//! One AVA1 session per console, shared by every job (SPEC.md §6 limits a peer to 12
//! connections per IP; one control connection plus at most 8 lanes is one session).
//!
//! A console keeps one session per identity (SPEC.md §8): two engine processes sharing an
//! identity evict each other. `Churn` notices the symptom and says so.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::session::{connect_expecting, Session, Timing};
use ava1::Ava1Error;
use ps5upload_core::ddp::Power;

/// `host:9120` — the AVA1 default port. The lab can override an address on
/// its own Pool; a process environment variable cannot redirect engine jobs.
/// The address to dial for the console's AVA1 port: its own, or the engine's tailnet mapping
/// of it when the console is away (`ps5upload_core::route`). The pinned key is checked either way.
fn routed(addr: &str) -> String {
    ps5upload_core::route::dial_str(addr, crate::gen::DEFAULT_PORT)
}

pub fn ava1_addr(console: &str) -> String {
    format!("{}:{}", host_of(console), crate::gen::DEFAULT_PORT)
}

/// The console string without its port (the pool's key).
pub(crate) use ps5upload_core::events::console_addr as host_of;

/// `ERR_BUSY` on a `JobOpen` is retried this many times (jittered doubling backoff, 250 ms to 5 s: about 45 s in
/// all) before the transfer fails with a clear reason. The console says BUSY while it recovers a job's files or
/// has no room for another job; it is never a verdict on the transfer.
pub(crate) const DEFAULT_BUSY_TRIES: u32 = 12;

/// Told the console's address each time a new session to it opens (on its own thread). The
/// engine says who it is there: a console lists every device by its sessions, and one that never
/// said hello shows as an app from before 8.0.0 even when it runs the newest version.
type SessionHook = Box<dyn Fn(&str) + Send + Sync>;
static ON_NEW_SESSION: std::sync::OnceLock<SessionHook> = std::sync::OnceLock::new();

/// Sets the new-session hook (once per process; a later call is ignored).
pub fn on_new_session(f: impl Fn(&str) + Send + Sync + 'static) {
    let _ = ON_NEW_SESSION.set(Box::new(f));
}

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
    /// Asks a console's DDP status after a connection failed (rest mode or off): the host,
    /// without a port, to the reply's code. Tests replace it so they need no network.
    power_probe: PowerProbe,
    /// How long a failure DDP explained (rest mode, or nothing answering) is repeated.
    probed_ttl: Duration,
}

/// The DDP probe a pool uses: host (no port) to the reply's code (`200` awake, `620` rest
/// mode), `None` for no reply. Blocking.
pub type PowerProbe = Arc<dyn Fn(&str) -> Option<u16> + Send + Sync>;

/// The real probe: two SRCH datagrams within about 800 ms. A console is never on loopback, so
/// a loopback host (tests, the lab) is not asked and costs nothing.
fn ddp_power_probe() -> PowerProbe {
    Arc::new(|host: &str| {
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let loopback = bare.eq_ignore_ascii_case("localhost")
            || bare
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if loopback {
            return None;
        }
        ps5upload_core::ddp::standby_probe(
            bare,
            ps5upload_core::ddp::STANDBY_PROBE_BUDGET,
            ps5upload_core::ddp::STANDBY_PROBE_SENDS,
        )
    })
}

/// A remembered session failure: when it happened, the reason/detail to repeat, and what DDP
/// said about the console's power when it was asked.
#[derive(Clone)]
struct Refusal {
    at: Instant,
    reason: String,
    detail: String,
    power: Option<Power>,
    /// When a quick connect last asked whether the console answers again (see
    /// [`Pool::recheck_due`]); `None`: not yet.
    rechecked: Option<Instant>,
}

/// A remembered failure may be looked past (one quick connect to the console's port) once it is
/// this old, and then at most this often: a console that wakes is seen in seconds, not after the
/// whole TTL, and a sleeping one costs one refused connect per period.
pub const RECHECK_EVERY: Duration = Duration::from_secs(2);

/// How long a failed session attempt is repeated without trying again.
pub const REFUSAL_TTL: Duration = Duration::from_secs(5);
/// How long a failure DDP explained (rest mode, or nothing answering at all) is repeated: it
/// outlasts the client's 10 s status poll, so a sleeping console costs one UDP probe (and no
/// TCP connects) per period.
pub const PROBED_REFUSAL_TTL: Duration = Duration::from_secs(12);
/// How long a `ava1_not_paired` refusal is repeated: it outlasts the client's 10 s status poll.
/// A pairing attempt (the dialog opening, a confirm, a dismissal) resets it.
pub const NOT_PAIRED_TTL: Duration = Duration::from_secs(30);

/// Whether the console answering at a new address is the one paired at an old one
/// (`Pool::same_console`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleMatch {
    /// It proved the key pinned for the old address.
    Same,
    /// It answered with another key: a different console.
    Different,
    /// Nothing to compare: the old address has no pin, or nothing answered at the new one.
    Unknown,
}

/// What a console says of itself before a handshake goes further (`Pool::peek_console`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peeked {
    /// Its static AVA1 key: the same on every address it answers at.
    pub key: [u8; 32],
    /// The name it sends (the helper sends "PS5").
    pub name: String,
    /// Its `CAP_*` bits.
    pub caps: u64,
}

/// Hs1, then read Hs2 and stop (see `Pool::peek_console`).
async fn peek(dial: &str, me: &Identity) -> Result<Peeked, Ava1Error> {
    use ava1::conn::{FrameReader, FrameWriter};
    use ava1::gen::{HelloInfo, Hs1, Hs2, ServerInfo};
    use ava1::wire::{FrameMessage, Message};
    let stream = tokio::net::TcpStream::connect(dial).await?;
    stream.set_nodelay(true)?;
    let (rh, wh) = stream.into_split();
    let (mut r, mut w) = (FrameReader::new(rh), FrameWriter::new(wh));
    let mut hs = ava1::keys::Handshake::initiator(me)?;
    let v = ava1::gen::PROTOCOL_VERSION;
    let hello = HelloInfo {
        version_min: v,
        version_max: v,
        caps: ava1::gen::CAP_DATA_PLANE,
    }
    .to_bytes()?;
    let noise = hs.write(&hello)?;
    w.send_msg(0, &Hs1 { noise }).await?;
    let f = r.recv().await?;
    if f.ty == ava1::gen::Error::TYPE {
        let e: ava1::gen::Error = f.decode()?;
        return Err(Ava1Error::Refused {
            code: e.code,
            message: e.message,
        });
    }
    let m2: Hs2 = f.decode()?;
    let info = ServerInfo::decode(&hs.read(&m2.noise)?)?;
    let key = hs.remote_static().ok_or(Ava1Error::WeakKey)?;
    // Dropping the stream here ends it: the console waits for message 3, which never comes.
    Ok(Peeked {
        key,
        name: info.name.unwrap_or_default(),
        caps: info.caps,
    })
}

/// The hosts in a pins file (`host hexkey` lines) pinned to `key`.
fn pinned_hosts(text: &str, key: &[u8; 32]) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((h, k)) = line.split_once(' ') else {
            continue;
        };
        let Some(k) = ava1::hex::decode(k.trim()) else {
            continue;
        };
        if k.as_slice() == key.as_slice() && !out.iter().any(|x: &String| x == h) {
            out.push(h.to_string());
        }
    }
    out
}

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
    /// The console's owner blocked this device (8.0.0 device list).
    Blocked,
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
    /// When the console last proved this session alive (a new Pong, or a reply), in wall-clock
    /// ms. Wall clock on purpose: a computer that slept for an hour must not read the hour as
    /// "just heard from it" (the monotonic clock stops during sleep on macOS).
    alive_at: Arc<AtomicU64>,
}

/// Wall-clock ms now (0 before 1970: never in practice).
fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
            power_probe: ddp_power_probe(),
            probed_ttl: PROBED_REFUSAL_TTL,
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
            power_probe: ddp_power_probe(),
            probed_ttl: PROBED_REFUSAL_TTL,
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

    /// Test seam: how long a failure DDP explained (rest mode, nothing answering) is remembered.
    pub fn with_probed_refusal_ttl(mut self, ttl: Duration) -> Pool {
        self.probed_ttl = ttl;
        self
    }

    /// Test seam: what DDP answers for this pool's consoles (host to code, `None` = silent).
    pub fn with_power_probe(
        mut self,
        probe: impl Fn(&str) -> Option<u16> + Send + Sync + 'static,
    ) -> Pool {
        self.power_probe = Arc::new(probe);
        self
    }

    /// The console's DDP code (`200` awake, `620` rest mode), asked now; `None` when it did not
    /// answer. Asked at the address sessions connect to. Blocking (about 800 ms at most).
    pub(crate) fn ddp_code(&self, console: &str) -> Option<u16> {
        (self.power_probe)(&host_of(&self.addr_for(console)))
    }

    /// The remembered failure of `console` while it is fresh: the not-paired TTL for an
    /// unpaired console, the probed TTL for one DDP explained, the plain TTL otherwise.
    fn recent(&self, console: &str) -> Option<Refusal> {
        let host = host_of(console);
        let mut m = self.refusals.lock().unwrap_or_else(|e| e.into_inner());
        match m.get(&host) {
            Some(r)
                if r.at.elapsed()
                    < if r.reason == "ava1_not_paired" || r.reason == crate::console::NOT_PAIRED {
                        self.not_paired_ttl
                    } else if matches!(r.power, Some(Power::Rest | Power::Unreachable)) {
                        self.probed_ttl
                    } else {
                        self.refusal_ttl
                    } =>
            {
                Some(r.clone())
            }
            Some(_) => {
                m.remove(&host);
                None
            }
            None => None,
        }
    }

    /// The failure (`reason`, `detail`) of a session attempt to `console` within the last
    /// [`REFUSAL_TTL`] ([`PROBED_REFUSAL_TTL`] when DDP explained it), if any.
    pub(crate) fn recent_refusal(&self, console: &str) -> Option<(String, String)> {
        self.recent(console).map(|r| (r.reason, r.detail))
    }

    /// What DDP said about `console`'s power when its last session attempt failed, while that
    /// failure is remembered. `None`: no fresh failure, or DDP was not asked.
    pub fn recent_power(&self, console: &str) -> Option<Power> {
        self.recent(console).and_then(|r| r.power)
    }

    pub(crate) fn note_refusal(&self, console: &str, reason: &str, detail: &str) {
        self.note_verdict(console, reason, detail, None);
    }

    /// [`Pool::note_refusal`] with what DDP said about the console's power.
    pub(crate) fn note_verdict(
        &self,
        console: &str,
        reason: &str,
        detail: &str,
        power: Option<Power>,
    ) {
        self.refusals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                host_of(console),
                Refusal {
                    at: Instant::now(),
                    reason: reason.to_string(),
                    detail: detail.to_string(),
                    power,
                    rechecked: None,
                },
            );
    }

    /// Whether a remembered failure of `console` may be looked past now with one quick connect:
    /// it is at least [`RECHECK_EVERY`] old, the last recheck as long ago, and it is not "not
    /// paired" (a session attempt then would show the console's pairing code again). Taking it
    /// marks the recheck done.
    pub(crate) fn recheck_due(&self, console: &str) -> bool {
        let mut m = self.refusals.lock().unwrap_or_else(|e| e.into_inner());
        let Some(r) = m.get_mut(&host_of(console)) else {
            return false;
        };
        if r.reason == "ava1_not_paired" || r.reason == crate::console::NOT_PAIRED {
            return false;
        }
        let since = r.rechecked.unwrap_or(r.at);
        if since.elapsed() < RECHECK_EVERY {
            return false;
        }
        r.rechecked = Some(Instant::now());
        true
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

    /// This engine's public key (the owner of the jobs it starts), when it has an identity.
    pub fn identity_key(&self) -> Option<[u8; 32]> {
        self.me.as_ref().ok().map(|me| me.public())
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
    pub(crate) fn addr_for(&self, console: &str) -> String {
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

    /// Is the console at `new` the one paired at `old`? For a console whose address changed
    /// (DHCP): the app offers the new address only when this says `Same`. It checks the key
    /// without pinning anything or keeping a session; the first ordinary connect to the new
    /// address pins it, as for any address. A console that does not trust this engine yet is
    /// still matched by its key, and no code is confirmed.
    pub async fn same_console(&self, old: &str, new: &str) -> ConsoleMatch {
        let Some(want) = self.pinned(&host_of(old)) else {
            return ConsoleMatch::Unknown;
        };
        if let Some(have) = self.pinned(&host_of(new)) {
            return if have == want {
                ConsoleMatch::Same
            } else {
                ConsoleMatch::Different
            };
        }
        let Ok(me) = self.me.clone() else {
            return ConsoleMatch::Unknown;
        };
        match connect_expecting(
            &routed(&self.addr_for(new)),
            Some(want),
            me,
            self.peers.clone(),
            "ps5upload",
            Timing::default(),
        )
        .await
        {
            Ok(s) => {
                s.close().await;
                ConsoleMatch::Same
            }
            Err(Ava1Error::WrongPeer) => ConsoleMatch::Different,
            Err(_) => ConsoleMatch::Unknown,
        }
    }

    /// Who answers AVA1 at `dial` (an `ip:port` to connect to as it is, no routing): the first
    /// two handshake messages only (SPEC.md §5 steps 1–2), which carry the console's static key
    /// and name. The handshake is then dropped before this side says who it is, so the console
    /// never admits, pairs or lists this device for it, and no code is shown anywhere. Nothing
    /// is pinned. For finding consoles on a tailnet: the key tells the same console at two
    /// addresses.
    pub async fn peek_console(&self, dial: &str, limit: Duration) -> Result<Peeked, Ava1Error> {
        let me = self
            .me
            .clone()
            .map_err(|why| Ava1Error::Io(io::Error::other(why)))?;
        tokio::time::timeout(limit, peek(dial, &me))
            .await
            .map_err(|_| Ava1Error::Timeout)?
    }

    /// The addresses whose pinned key is `key` (the same console, met at each of them).
    pub fn hosts_pinned_to(&self, key: &[u8; 32]) -> Vec<String> {
        let text = std::fs::read_to_string(self.dir.join("consoles")).unwrap_or_default();
        pinned_hosts(&text, key)
    }

    /// Does this engine trust the console with this key (paired, or launched by it)?
    pub fn knows_key(&self, key: &[u8; 32]) -> bool {
        self.peers.lock().map(|p| p.contains(key)).unwrap_or(false)
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
            &routed(&self.addr_for(console)),
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
                Err(Ava1Error::Refused { code, message })
                    if code == crate::gen::ERR_PAIRING_CLOSED =>
                {
                    return Ok(if message.contains("blocked") {
                        Pairing::Blocked
                    } else {
                        Pairing::Closed
                    });
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
                if code == crate::gen::ERR_PAIRING_CODE
                    || code == crate::gen::ERR_PAIRING_CLOSED =>
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
            // The handshake that just finished is the first proof of life.
            let alive_at = Arc::new(AtomicU64::new(wall_ms()));
            map.insert(
                host.clone(),
                Cached {
                    session: s.clone(),
                    counted: counted.clone(),
                    alive_at: alive_at.clone(),
                },
            );
            drop(map);
            if let Some(hook) = ON_NEW_SESSION.get() {
                let console = console.to_string();
                std::thread::spawn(move || hook(&console));
            }
            // Count how often this session ends on its own, so a second engine on the same
            // identity (which the console resolves by ending the older session) is named.
            // Polls through a `Weak`: a watcher must not keep a session (and its console
            // connections) alive once every user has dropped it.
            let (watched, churn) = (Arc::downgrade(&s), self.churn.clone());
            tokio::spawn(async move {
                // Each answered Ping changes the round-trip figure: a change is the console
                // talking (`silent_for` reads it). The link sends a Ping every 2 s.
                let mut last_rtt = None;
                loop {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let Some(s) = watched.upgrade() else { return };
                    let rtt = s.rtt();
                    if rtt.is_some() && rtt != last_rtt {
                        last_rtt = rtt;
                        alive_at.store(wall_ms(), Ordering::Relaxed);
                    }
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

    /// How long the cached session `s` has gone without a word from the console (no new Pong,
    /// no reply), by the wall clock. `None`: `s` is not the cached session (another took its
    /// place, or it was dropped).
    pub async fn silent_for(&self, console: &str, s: &Arc<Session>) -> Option<Duration> {
        let map = self.sessions.lock().await;
        let c = map
            .get(&host_of(console))
            .filter(|c| Arc::ptr_eq(&c.session, s))?;
        Some(Duration::from_millis(
            wall_ms().saturating_sub(c.alive_at.load(Ordering::Relaxed)),
        ))
    }

    /// A reply on `console`'s cached session: it is alive now.
    pub async fn note_alive(&self, console: &str) {
        if let Some(c) = self.sessions.lock().await.get(&host_of(console)) {
            c.alive_at.store(wall_ms(), Ordering::Relaxed);
        }
    }

    /// Whether a live session to `console` is cached (a call now reuses it, no handshake).
    pub async fn has_session(&self, console: &str) -> bool {
        self.sessions
            .lock()
            .await
            .get(&host_of(console))
            .is_some_and(|c| !c.session.is_closed())
    }

    /// The address a connection to `console`'s AVA1 port goes to now: its own, the pool's
    /// override, or the engine's tailnet mapping of it.
    pub(crate) fn dial_addr(&self, console: &str) -> String {
        routed(&self.addr_for(console))
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
/// The app's data directory (`PS5UPLOAD_DATA`, else `~/.ps5upload`): the pool, caches, traces.
pub fn data_dir() -> Option<PathBuf> {
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
mod peek_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// A fake helper: the server side of the AVA1 handshake. Records whether it ever got far
    /// enough to admit (the point where a real one could pair, list or show a code).
    async fn fake_helper(key: Arc<Identity>) -> (String, Arc<AtomicBool>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let admitted = Arc::new(AtomicBool::new(false));
        let a2 = admitted.clone();
        tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let (rh, wh) = s.into_split();
            let mut r = ava1::conn::FrameReader::new(rh);
            let mut w = ava1::conn::FrameWriter::new(wh);
            let hs1 = r.recv().await.unwrap();
            let _ = ava1::handshake::server(
                &mut r,
                &mut w,
                hs1,
                &key,
                "PS5",
                |_| {
                    a2.store(true, Ordering::SeqCst);
                    ava1::handshake::Admission::Known
                },
                ava1::gen::CAP_DATA_PLANE,
            )
            .await;
        });
        (addr, admitted)
    }

    #[tokio::test]
    async fn a_peek_reads_the_key_and_name_and_never_gets_admitted() {
        let d = std::env::temp_dir().join(format!("p5a-peek-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let pool = Pool::new(d.join("ava"));
        let console = Arc::new(Identity::generate().unwrap());
        let (addr, admitted) = fake_helper(console.clone()).await;
        let got = pool
            .peek_console(&addr, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(got.key, console.public());
        assert_eq!(got.name, "PS5");
        assert_eq!(got.caps, ava1::gen::CAP_DATA_PLANE);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !admitted.load(Ordering::SeqCst),
            "the console never admitted us"
        );
        assert!(pool.hosts_pinned_to(&got.key).is_empty(), "nothing pinned");
        assert!(!pool.knows_key(&got.key));
        // Nothing listening: an error, quickly.
        let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = dead.local_addr().unwrap().to_string();
        drop(dead);
        assert!(pool
            .peek_console(&dead_addr, Duration::from_secs(2))
            .await
            .is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn pinned_hosts_are_found_by_key() {
        let k = [7u8; 32];
        let other = [8u8; 32];
        let text = format!(
            "192.168.1.20 {}\n100.64.0.9 {}\n10.0.0.2 {}\nbad line\n192.168.1.20 {}\n",
            ava1::hex::encode(&k),
            ava1::hex::encode(&k),
            ava1::hex::encode(&other),
            ava1::hex::encode(&k)
        );
        assert_eq!(pinned_hosts(&text, &k), vec!["192.168.1.20", "100.64.0.9"]);
        assert_eq!(pinned_hosts(&text, &other), vec!["10.0.0.2"]);
        assert!(pinned_hosts("", &k).is_empty());
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

#[cfg(test)]
mod not_paired_ttl_tests {
    use super::*;

    /// The reason a refused pairing is stored under is `not_paired` (console.rs NOT_PAIRED): it
    /// is remembered for the longer not-paired time, not the plain refusal time.
    #[test]
    fn a_not_paired_refusal_is_remembered_for_its_own_time() {
        let d = std::env::temp_dir().join(format!("p5a-np-{}", std::process::id()));
        let pool = Pool::new(d.join("ava"))
            .with_refusal_ttl(Duration::from_millis(1))
            .with_not_paired_ttl(Duration::from_secs(60));
        pool.note_refusal("192.0.2.9", crate::console::NOT_PAIRED, "not paired");
        pool.note_refusal("192.0.2.10", "ava1_unreachable", "gone");
        std::thread::sleep(Duration::from_millis(20));
        assert!(pool.recent_refusal("192.0.2.9").is_some());
        assert!(pool.recent_refusal("192.0.2.10").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
