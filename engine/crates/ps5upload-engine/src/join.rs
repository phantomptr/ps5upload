//! Play Together (#641): friends join the host's Remote Play as Player 2-4.
//!
//! The host (the computer running the Remote Play session) opens Play Together for the console
//! being played and hands out an invite link (`http://<this computer>:<port>/join#i=<key>`). A
//! guest's browser opens `GET /join` (the guest page) and connects `GET /api/join/ws`; the host
//! allows them, and they get Player N: their own virtual DualSense on the console (the helper's
//! `vpad.*`, `ps5upload_core::vpad`), the session's picture and sound, and their controller's
//! rumble and light bar.
//!
//! **Guest socket** (`/api/join/ws`, the only Play Together route a LAN peer reaches):
//! * first message `{"hello": {"invite"?: key, "code"?: short code, "resume"?: token, "name":
//!   "…"}}` (`invite`: the link; `code`: typed, case and dashes ignored; neither: a "Playing
//!   nearby" join from the mesh presence; every new guest asks the host); answers
//!   `{"stage": "asking"}` (waiting for the host), `{"stage": "waiting", "why": "host_stopped"}`
//!   (a guest coming back while the host restarts Play), then `{"stage": "joined", "player": n,
//!   "token": t, "hevc": bool}` or `{"stage": "refused", "why": "key" | "expired" | "full" |
//!   "denied" | "no_answer" (the host did not answer in time) | "closed" | "not_playing" |
//!   "old_helper" | "unreachable" | "slow_down" (5 wrong codes in a minute)}`.
//! * guest → engine: binary pad frames ([`parse_pad`]), `{"ping": t}` (answered `{"pong": t}`),
//!   `{"leave": 1}`.
//! * engine → guest: the session's picture and sound exactly as the watch party gets them (same
//!   binary kinds, `rp_stream_api::add_player`), `{"status": {...}}` every second,
//!   `{"rumble": [l, r], "light": [r, g, b]}` when the game changes them, `{"bye": "kicked" |
//!   "closed" | "replaced" | "host_stopped" | "lost" (the console no longer has their
//!   controller)}` (the socket then closes). The engine also sends a
//!   WebSocket ping every second; the browser's automatic pong gives the guest's round trip.
//!
//! The `token` is the guest's resume credential: a guest whose socket drops keeps their player
//! (and the console keeps their controller and their signed-in user: the engine keeps heart-
//! beating the helper's slot) for [`RESUME_WINDOW`], and comes back with `{"hello": {"resume":
//! token}}`, no approval needed. One socket per token: a second one replaces the first.
//!
//! Play Together is tied to the host's running session on that console: a guest cannot join
//! while none runs, and when it ends every guest is told `host_stopped`, their pads are removed
//! after [`HOST_GRACE`], and Play Together closes unless the host plays again within
//! [`HOST_RESTART_WINDOW`] (then guests come back with their token).
//!
//! **Host API** (this computer by a loopback name, or a peer `PS5UPLOAD_ALLOW_IP` admits: the web
//! UI of a NAS engine): `GET /api/join?addr=`, `POST /api/join/open | close | invite | decide |
//! kick | playing | settings` (see [`router`]), and `GET /api/join/relay?url=` (an app joining a
//! friend through its own engine).
//!
//! **To the helper**, one link thread per console ([`link`]) sends each player's latest state
//! (coalesced, but a press is never lost: [`link::SlotIo::plan`]), one call in flight, a heartbeat
//! every [`vpad::HEARTBEAT`], reopens the pads after a helper restart, and polls the rumble and
//! light bar.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use ps5upload_core::rp_stream::ControllerState;
use ps5upload_core::vpad::{self, Motion, PadState, Touch};
use tokio::sync::{mpsc, oneshot};

use crate::pad_link::{random_key, same_key};
use crate::vinput_api::host_key;

/// An invite link works this long (and until Play Together closes).
const INVITE_TTL: Duration = Duration::from_secs(30 * 60);
/// A guest whose socket dropped keeps their player this long.
pub(crate) const RESUME_WINDOW: Duration = Duration::from_secs(60);
/// After the host's session ends, the guests' pads stay this long (a quick restart keeps them).
const HOST_GRACE: Duration = Duration::from_secs(5);
/// After the host's session ends, Play Together closes unless the host plays again this soon.
const HOST_RESTART_WINDOW: Duration = Duration::from_secs(60);
/// Guests at most (the PS5 takes four controllers; Player 1 is the host's).
pub(crate) const MAX_GUESTS: u8 = 3;
/// Guests waiting for the host's answer at once.
const MAX_PENDING: usize = 4;
/// How long a guest waits for the host's answer.
const ASK_WAIT: Duration = Duration::from_secs(90);
/// A guest must say hello this soon after connecting.
const HELLO_WAIT: Duration = Duration::from_secs(10);
/// How long the helper may take to make a guest's pad.
const OPEN_WAIT: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 5 });
/// How often a guest hears its status (and the engine pings it).
const STATUS_EVERY: Duration = Duration::from_secs(1);
/// Pictures queued for a player before it skips to the next keyframe (the host page's queue).
const PLAYER_QUEUE: usize = 6;
/// A guest may press PS this long after their pad is made (to answer "Who's using this
/// controller?"), unless the host says they are in sooner.
const PICKING_GRACE: Duration = Duration::from_secs(60);
/// A guest who left stays on the host's list this long.
const LEFT_SHOWN: Duration = Duration::from_secs(30);
/// Guest connects per address per minute.
const CONNECTS_PER_MIN: usize = 10;
/// This computer's own guest sockets per minute (its browser, its tests): more, but not unbounded.
const LOOPBACK_CONNECTS_PER_MIN: usize = 120;
/// The largest message a guest's socket takes. Theirs are a hello (a key, a token, a name), a
/// ping, a leave and pad frames (at most 51 bytes): all well under 1 KiB. The host's way out
/// (pictures, sound, status) is not limited by it.
pub(crate) const GUEST_MSG_MAX: usize = 4096;
/// A guest's pad frames reach the helper link at most this many a second on average (a pad
/// sends 60-120), with bursts of [`PAD_BURST`] (frames bunched up by the network); more are
/// folded into the latest, never queued.
const PAD_RATE: f64 = 250.0;
const PAD_BURST: f64 = 32.0;
/// How often each open Play Together checks the host's session, expiries and resumes.
const WATCH_EVERY: Duration = Duration::from_millis(250);
/// The player who uses the stream's own controller instead of a helper pad (#687): with the picture
/// off on the host's device that controller sits idle and still takes one of the console's four
/// places. No slot, no "Who's using this controller?": it plays as the user the stream signed in.
const STREAM_SEAT: u8 = 1;
/// Guest → engine binary kind: one pad state.
pub(crate) const KIND_PAD: u8 = 0x01;
/// The optional motion block's tag inside a pad frame.
const PAD_MOTION: u8 = 0x02;
/// Spectator ids for players (pad_link's phones count up from 1).
const SPECTATOR_BASE: u64 = 1 << 62;
/// A guest's gyro is held to this (rad/s: a DualSense reads up to 2000 deg/s, 34.9 rad/s).
const GYRO_MAX: f32 = 40.0;
/// A guest's accelerometer is held to this (g: a DualSense reads up to 8 g).
const ACCEL_MAX: f32 = 16.0;
/// Motion beyond this is no reading at all: the motion block is dropped.
const MOTION_ABSURD: f32 = 1e5;

// ── Guest pad frames ─────────────────────────────────────────────────────────────────────────────

/// A guest's binary pad frame: `[0x01][u32 LE seq][u32 LE buttons][u8 lx ly rx ry l2 r2]
/// [u8 ntouch][ntouch × (u8 id, u16 LE x, u16 LE y)]` and optionally `[0x02][6 × f32 LE: gyro
/// x y z, accel x y z]`. None when malformed (more than two fingers, a short or long frame,
/// motion that is not a finite number). Motion beyond [`MOTION_ABSURD`] is dropped (the frame's
/// buttons, sticks and touch still count); the rest is held to what a DualSense reads
/// ([`GYRO_MAX`], [`ACCEL_MAX`]), so the helper never refuses a state for it. `buttons` are the
/// apps' bits (`PadButton`, the same as Remote Play's), turned into the helper's (libScePad's,
/// `vpad::from_app`); sticks 0-255 (128 at rest), touch in 1920 × 1080, gyro in rad/s, accel in
/// g. `seq` is the guest's (it may start anywhere): the link numbers what it sends to the helper
/// itself.
pub(crate) fn parse_pad(b: &[u8]) -> Option<(u32, PadState)> {
    if b.len() < 16 || b[0] != KIND_PAD {
        return None;
    }
    let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let seq = u32_at(1);
    let buttons = u32_at(5);
    let n = b[15] as usize;
    if n > vpad::MAX_TOUCH {
        return None;
    }
    let touch_end = 16 + n * 5;
    if b.len() < touch_end {
        return None;
    }
    let touch = (0..n)
        .map(|i| {
            let o = 16 + i * 5;
            Touch {
                id: b[o],
                x: u16::from_le_bytes([b[o + 1], b[o + 2]]),
                y: u16::from_le_bytes([b[o + 3], b[o + 4]]),
            }
        })
        .collect();
    let motion = match &b[touch_end..] {
        [] => None,
        [PAD_MOTION, rest @ ..] if rest.len() == 24 => {
            let f = |i: usize| {
                f32::from_le_bytes([
                    rest[i * 4],
                    rest[i * 4 + 1],
                    rest[i * 4 + 2],
                    rest[i * 4 + 3],
                ])
            };
            let v: Vec<f32> = (0..6).map(f).collect();
            if v.iter().any(|x| !x.is_finite()) {
                return None;
            }
            if v.iter().any(|x| x.abs() > MOTION_ABSURD) {
                // no sensor reads that: the motion block goes, the rest of the frame stays
                None
            } else {
                let g = |x: f32| x.clamp(-GYRO_MAX, GYRO_MAX);
                let a = |x: f32| x.clamp(-ACCEL_MAX, ACCEL_MAX);
                Some(Motion {
                    gyro: [g(v[0]), g(v[1]), g(v[2])],
                    accel: [a(v[3]), a(v[4]), a(v[5])],
                })
            }
        }
        _ => return None,
    };
    Some((
        seq,
        PadState {
            buttons: vpad::from_app(buttons, b[13], b[14]),
            lx: b[9],
            ly: b[10],
            rx: b[11],
            ry: b[12],
            l2: b[13],
            r2: b[14],
            touch,
            motion,
        },
    ))
}

// ── State ────────────────────────────────────────────────────────────────────────────────────────

/// The host's choices (one set for every console).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Settings {
    /// Take PS and Create from guests who are playing (they keep them while picking a user).
    /// On unless the host turns it off: a friend's PS could quit the game, or update the console
    /// past its exploit (#649).
    pub block_system_buttons: bool,
    /// Guests at most (1-3).
    pub max_players: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            block_system_buttons: true,
            max_players: MAX_GUESTS,
        }
    }
}

static SETTINGS: Mutex<Option<Settings>> = Mutex::new(None);

fn settings() -> Settings {
    SETTINGS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .unwrap_or_default()
}

/// What goes to a guest's socket.
#[derive(Debug)]
pub(crate) enum Out {
    Text(serde_json::Value),
    /// `{"bye": why}`, then the socket closes.
    Bye(&'static str),
}

/// A guest's socket: its generation (a newer socket replaces it) and its queue.
struct Conn {
    gen: u64,
    tx: mpsc::Sender<Out>,
}

/// One guest's health, written by its socket without the lock.
#[derive(Default)]
pub(crate) struct Stats {
    /// The last round trip in ms (0 = not measured yet).
    rtt_ms: AtomicU32,
    rtt_max: AtomicU32,
    rtt_sum: AtomicU64,
    rtt_n: AtomicU64,
    /// Pictures skipped to the next keyframe (a full queue).
    skipped: Arc<AtomicU64>,
    /// Pictures waiting in its queue.
    queue: AtomicUsize,
    pad_frames: AtomicU64,
    /// Pictures were skipped in the last few seconds: this guest's connection can't keep up.
    behind: AtomicBool,
}

impl Stats {
    fn rtt(&self, ms: u32) {
        self.rtt_ms.store(ms.max(1), Ordering::Relaxed);
        self.rtt_max.fetch_max(ms, Ordering::Relaxed);
        self.rtt_sum.fetch_add(ms as u64, Ordering::Relaxed);
        self.rtt_n.fetch_add(1, Ordering::Relaxed);
    }
    fn rtt_now(&self) -> Option<u32> {
        Some(self.rtt_ms.load(Ordering::Relaxed)).filter(|&v| v > 0)
    }
}

pub(crate) struct Guest {
    id: u64,
    name: String,
    player: u8,
    /// The guest's resume credential (sent to the guest only).
    token: String,
    /// The helper slot's owner token (sent to the console only).
    pad_token: String,
    conn: Option<Conn>,
    /// The address of their last socket (never shown or logged): see [`holds_a_place`].
    ip: Option<IpAddr>,
    /// Since when the guest's socket is gone (Reconnecting).
    away_since: Option<Instant>,
    left: Option<(Instant, &'static str)>,
    /// The guest picked a user on the TV (or the grace ran out, or the host said so).
    playing: bool,
    /// Since when the console has asked "Who's using this controller?" (the pad was made).
    picking_since: Instant,
    /// On [`STREAM_SEAT`]: when their last state arrived, while one is held on the controller.
    seat_input_at: Option<Instant>,
    /// The helper has the pad (closed while the host is away).
    slot_open: bool,
    joined_at: Instant,
    stats: Arc<Stats>,
}

impl Guest {
    fn state(&self) -> &'static str {
        if self.left.is_some() {
            "left"
        } else if self.conn.is_none() {
            "reconnecting"
        } else if self.playing {
            "playing"
        } else {
            "picking_user"
        }
    }
    fn active(&self) -> bool {
        self.left.is_none()
    }
}

/// The host's answer to an ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Allow,
    Deny,
    /// The host did not answer in time.
    NoAnswer,
    /// The host stopped playing while the guest waited.
    HostStopped,
    /// Play Together closed while the guest waited.
    Closed,
}

struct Ask {
    id: u64,
    name: String,
    answer: oneshot::Sender<Decision>,
}

struct Invite {
    key: String,
    /// The short code (6 characters of [`CODE_ALPHABET`]) a guest may type instead of the link.
    code: String,
    until: Instant,
}

/// An invite as the host's page and the presence show it.
#[derive(Clone, Debug)]
pub(crate) struct InviteInfo {
    key: String,
    code: String,
    until: Instant,
}

/// Short codes use no look-alikes (no I, L, O, 0, 1).
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
/// Characters in a short code.
const CODE_LEN: usize = 6;
/// Wrong short codes per address per minute before it must wait.
const WRONG_CODES_PER_MIN: usize = 5;

/// A new short code (uniform over the alphabet).
fn new_code() -> Option<String> {
    let n = CODE_ALPHABET.len();
    let limit = 256 - 256 % n; // bytes at or past this would favour the first letters
    let mut out = String::with_capacity(CODE_LEN);
    while out.len() < CODE_LEN {
        let mut b = [0u8; 16];
        getrandom::fill(&mut b).ok()?;
        for x in b {
            if (x as usize) < limit && out.len() < CODE_LEN {
                out.push(CODE_ALPHABET[x as usize % n] as char);
            }
        }
    }
    Some(out)
}

/// A typed code as the invite keeps it: case, dashes and spaces do not matter.
fn normalize_code(c: &str) -> String {
    c.chars()
        .filter(|ch| !matches!(ch, '-' | ' ' | '\u{2010}'..='\u{2015}'))
        .flat_map(char::to_uppercase)
        .take(16)
        .collect()
}

/// `K7M2QX` as people read it: `K7M-2QX`.
fn code_display(c: &str) -> String {
    if c.len() == CODE_LEN {
        format!("{}-{}", &c[..3], &c[3..])
    } else {
        c.to_string()
    }
}

/// Something to do once the lock is let go.
pub(crate) enum Effect {
    Say(mpsc::Sender<Out>, Out),
    Link(link::Cmd),
    Log(String),
    /// The guest on [`STREAM_SEAT`]'s controller state (None: let go).
    SeatPad(Option<ControllerState>),
    /// Whether a guest has the stream's controller, so the host's page is kept off it.
    Seat(bool),
}

/// Play Together for one console.
pub(crate) struct Party {
    console: String,
    /// The host's Remote Play session it belongs to.
    session: u64,
    /// Since when the host's session is gone.
    host_gone: Option<Instant>,
    host_name: String,
    invite: Option<Invite>,
    pending: Vec<Ask>,
    guests: Vec<Guest>,
    settings: Settings,
    /// Where guests reach this engine, as the host's last request said.
    pub(crate) reach: Reach,
}

/// How guests reach this engine.
#[derive(Clone, Debug, Default)]
pub(crate) struct Reach {
    /// The port guests use at this engine's own addresses.
    pub port: u16,
    /// Scheme, name and port the host's browser used, when it is not this computer's loopback (the
    /// web UI of an engine on a NAS or behind a proxy): guests can likely use it too.
    pub web_base: Option<String>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

impl Party {
    pub(crate) fn new(console: &str, session: u64, host_name: String, settings: Settings) -> Self {
        Party {
            console: console.to_string(),
            session,
            host_gone: None,
            host_name,
            invite: None,
            pending: Vec::new(),
            guests: Vec::new(),
            settings,
            reach: Reach::default(),
        }
    }

    /// The invite (a new one when there is none, it expired, or `renew`).
    fn invite(&mut self, renew: bool, now: Instant) -> Option<InviteInfo> {
        if renew || self.invite.as_ref().is_none_or(|i| i.until <= now) {
            self.invite = Some(Invite {
                key: random_key()?,
                code: new_code()?,
                until: now + INVITE_TTL,
            });
        }
        self.current_invite(now)
    }

    fn current_invite(&self, now: Instant) -> Option<InviteInfo> {
        self.invite
            .as_ref()
            .filter(|i| i.until > now)
            .map(|i| InviteInfo {
                key: i.key.clone(),
                code: i.code.clone(),
                until: i.until,
            })
    }

    fn key_ok(&self, key: &str, now: Instant) -> bool {
        self.invite
            .as_ref()
            .is_some_and(|i| i.until > now && same_key(&i.key, key))
    }

    /// `code` (already normalized) is the invite's short code.
    fn code_ok(&self, code: &str, now: Instant) -> bool {
        self.invite
            .as_ref()
            .is_some_and(|i| i.until > now && same_key(&i.code, code))
    }

    fn guests_in(&self) -> usize {
        self.guests.iter().filter(|g| g.active()).count()
    }

    /// The lowest free player number, skipping `tried` (slots the helper said are busy).
    fn free_player(&self, tried: &[u8]) -> Option<u8> {
        let last = (vpad::FIRST_SLOT + self.settings.max_players.clamp(1, MAX_GUESTS) - 1)
            .min(vpad::LAST_SLOT);
        (vpad::FIRST_SLOT..=last).find(|p| {
            !tried.contains(p) && !self.guests.iter().any(|g| g.active() && g.player == *p)
        })
    }

    /// The next guest's player number: the stream's controller when `seat_free` says nobody on
    /// the host's side uses it, else a helper pad. Never more guests than the host allows.
    fn next_player(&self, tried: &[u8], seat_free: bool) -> Option<u8> {
        if self.guests_in() >= usize::from(self.settings.max_players.clamp(1, MAX_GUESTS)) {
            return None;
        }
        if seat_free
            && !self
                .guests
                .iter()
                .any(|g| g.active() && g.player == STREAM_SEAT)
        {
            return Some(STREAM_SEAT);
        }
        self.free_player(tried)
    }

    fn guest_mut(&mut self, id: u64) -> Option<&mut Guest> {
        self.guests.iter_mut().find(|g| g.id == id)
    }

    /// The guest holding `token` (still in), compared in constant time.
    fn by_token(&self, token: &str) -> Option<u64> {
        let mut found = None;
        for g in &self.guests {
            if same_key(&g.token, token) && g.active() {
                found = Some(g.id);
            }
        }
        found
    }

    /// A new guest takes Player `player` (its pad not made yet).
    fn admit(&mut self, name: &str, player: u8, now: Instant) -> Option<&Guest> {
        let token = random_key()?;
        let pad_token = random_key()?;
        self.guests.retain(|g| g.active() || g.player != player);
        self.guests.push(Guest {
            id: next_id(),
            name: name.to_string(),
            player,
            token,
            pad_token,
            conn: None,
            ip: None,
            away_since: Some(now),
            left: None,
            playing: false,
            picking_since: now,
            seat_input_at: None,
            slot_open: false,
            joined_at: now,
            stats: Arc::default(),
        });
        self.guests.last()
    }

    /// The guest's socket `conn` is theirs now: an older socket is told it was replaced.
    fn attach(&mut self, id: u64, gen: u64, tx: mpsc::Sender<Out>) -> Vec<Effect> {
        let mut fx = Vec::new();
        if let Some(g) = self.guest_mut(id) {
            if let Some(old) = g.conn.replace(Conn { gen, tx }) {
                fx.push(Effect::Say(old.tx, Out::Bye("replaced")));
            }
            g.away_since = None;
        }
        fx
    }

    /// The guest's socket `gen` closed: their pad goes neutral, their player waits for them.
    fn detach(&mut self, id: u64, gen: u64, now: Instant) -> Vec<Effect> {
        let mut fx = Vec::new();
        if let Some(g) = self.guest_mut(id) {
            if g.conn.as_ref().is_some_and(|c| c.gen == gen) {
                g.conn = None;
                g.away_since = Some(now);
                if g.slot_open {
                    fx.push(Effect::Link(link::Cmd::Neutral { slot: g.player }));
                }
                if g.player == STREAM_SEAT {
                    g.seat_input_at = None;
                    fx.push(Effect::SeatPad(None));
                }
            }
        }
        fx
    }

    /// The guest is out (`why`: left, kicked, timeout, closed, lost): their socket is told, their
    /// pad removed, their token no longer works.
    fn leave(&mut self, id: u64, why: &'static str, now: Instant) -> Vec<Effect> {
        let mut fx = Vec::new();
        let Some(g) = self.guest_mut(id) else {
            return fx;
        };
        if g.left.is_some() {
            return fx;
        }
        g.left = Some((now, why));
        g.away_since = None;
        if let Some(c) = g.conn.take() {
            let bye = match why {
                "kicked" => Some("kicked"),
                "closed" => Some("closed"),
                "lost" => Some("lost"),
                _ => None,
            };
            if let Some(b) = bye {
                fx.push(Effect::Say(c.tx, Out::Bye(b)));
            }
        }
        if g.slot_open {
            g.slot_open = false;
            fx.push(Effect::Link(link::Cmd::Close { slot: g.player }));
        }
        if g.player == STREAM_SEAT {
            g.seat_input_at = None;
            fx.push(Effect::SeatPad(None));
            fx.push(Effect::Seat(false));
        }
        let s = &g.stats;
        let n = s.rtt_n.load(Ordering::Relaxed);
        let avg = s.rtt_sum.load(Ordering::Relaxed).checked_div(n);
        fx.push(Effect::Log(format!(
            "join: Player {} left ({why}) after {}s: rtt avg {} ms, max {} ms; {} pictures skipped to a keyframe; {} pad frames",
            g.player,
            now.duration_since(g.joined_at).as_secs(),
            avg.map(|a| a.to_string()).unwrap_or_else(|| "?".into()),
            s.rtt_max.load(Ordering::Relaxed),
            s.skipped.load(Ordering::Relaxed),
            s.pad_frames.load(Ordering::Relaxed),
        )));
        fx
    }

    /// Everyone out, every ask answered: Play Together closes.
    fn close(&mut self, now: Instant) -> Vec<Effect> {
        let mut fx = Vec::new();
        for id in self.guests.iter().map(|g| g.id).collect::<Vec<_>>() {
            fx.extend(self.leave(id, "closed", now));
        }
        for a in self.pending.drain(..) {
            let _ = a.answer.send(Decision::Closed);
        }
        self.invite = None;
        fx
    }

    /// The pad of `slot` was made anew (the helper restarted): the console asks the guest who
    /// they are again.
    fn reopened(&mut self, slot: u8, now: Instant) {
        if let Some(g) = self
            .guests
            .iter_mut()
            .find(|g| g.active() && g.player == slot)
        {
            g.playing = false;
            g.picking_since = now;
        }
    }

    /// Is socket `gen` still this guest's? A replaced socket whose "replaced" notice was lost
    /// (its queue full on a stalled connection) must not drive the pad or hear the status.
    fn is_current(&self, id: u64, gen: u64) -> bool {
        self.guests
            .iter()
            .any(|g| g.id == id && g.conn.as_ref().is_some_and(|c| c.gen == gen))
    }

    /// The bits to take from the guest's input now (None: they are not in).
    fn strip_mask(&self, id: u64) -> Option<(u8, u32)> {
        let g = self.guests.iter().find(|g| g.id == id && g.active())?;
        let strip = if self.settings.block_system_buttons && g.playing {
            vpad::button::SYSTEM
        } else {
            0
        };
        Some((g.player, strip))
    }

    /// Time passes; `live` is the host's running session on this console, if any. Returns what
    /// to do, and false once Play Together has closed.
    pub(crate) fn tick(&mut self, now: Instant, live: Option<u64>) -> (Vec<Effect>, bool) {
        let mut fx = Vec::new();
        match (live, self.host_gone) {
            (Some(s), None) if s == self.session => {}
            (Some(s), gone) => {
                // The host plays again (or a new session replaced the old one between looks:
                // its guests' pictures ended with the old one, so they come back to the new).
                self.session = s;
                self.host_gone = None;
                for g in self.guests.iter_mut().filter(|g| g.active()) {
                    if let Some(c) = g.conn.take() {
                        fx.push(Effect::Say(c.tx, Out::Bye("host_stopped")));
                    }
                    g.away_since = Some(now);
                }
                if gone.is_some() {
                    fx.push(Effect::Log(format!(
                        "join: the host plays {} again; guests may come back",
                        self.console
                    )));
                }
            }
            (None, None) => {
                // The host's session ended: every guest is told, asks are dropped.
                self.host_gone = Some(now);
                for g in self.guests.iter_mut().filter(|g| g.active()) {
                    if let Some(c) = g.conn.take() {
                        fx.push(Effect::Say(c.tx, Out::Bye("host_stopped")));
                    }
                    g.away_since = Some(now);
                    if g.slot_open {
                        fx.push(Effect::Link(link::Cmd::Neutral { slot: g.player }));
                    }
                }
                for a in self.pending.drain(..) {
                    let _ = a.answer.send(Decision::HostStopped);
                }
                fx.push(Effect::Log(format!(
                    "join: the host stopped playing {}; Play Together waits {}s for them",
                    self.console,
                    HOST_RESTART_WINDOW.as_secs()
                )));
            }
            (None, Some(t)) => {
                let gone = now.duration_since(t);
                if gone >= HOST_RESTART_WINDOW {
                    fx.extend(self.close(now));
                    fx.push(Effect::Log(format!(
                        "join: Play Together on {} closed (the host did not play again)",
                        self.console
                    )));
                    return (fx, false);
                }
                if gone >= HOST_GRACE {
                    for g in self.guests.iter_mut().filter(|g| g.active() && g.slot_open) {
                        g.slot_open = false;
                        fx.push(Effect::Link(link::Cmd::Close { slot: g.player }));
                    }
                }
            }
        }
        if self.invite.as_ref().is_some_and(|i| i.until <= now) {
            self.invite = None;
        }
        let mut gone = Vec::new();
        for g in self.guests.iter_mut().filter(|g| g.active()) {
            if self.host_gone.is_none()
                && g.away_since
                    .is_some_and(|t| now.duration_since(t) >= RESUME_WINDOW)
            {
                gone.push(g.id);
            }
            // The helper does not say when the console has signed a user in on the pad, which
            // would let PS be blocked at once: the grace or the host's "They're in" decides.
            if !g.playing && now.duration_since(g.picking_since) >= PICKING_GRACE {
                g.playing = true;
            }
            // Gone quiet on the stream's controller: nothing stays held (the helper does this
            // for its pads).
            if g.seat_input_at
                .is_some_and(|t| now.duration_since(t) >= vpad::NEUTRAL_AFTER)
            {
                g.seat_input_at = None;
                fx.push(Effect::SeatPad(None));
            }
            // Also claims it on a session that replaced the old one.
            if g.player == STREAM_SEAT && live.is_some() {
                fx.push(Effect::Seat(true));
            }
        }
        for id in gone {
            fx.extend(self.leave(id, "timeout", now));
        }
        self.guests.retain(|g| {
            g.left
                .is_none_or(|(t, _)| now.duration_since(t) < LEFT_SHOWN)
        });
        (fx, true)
    }

    /// What the host's page shows.
    fn snapshot(&self, now: Instant, invite: serde_json::Value) -> serde_json::Value {
        let guests: Vec<_> = self
            .guests
            .iter()
            .map(|g| {
                serde_json::json!({
                    "id": g.id,
                    "player": g.player,
                    "name": g.name,
                    "state": g.state(),
                    "connected": g.conn.is_some(),
                    "reconnect_left_s": g.away_since.filter(|_| self.host_gone.is_none()).map(|t| RESUME_WINDOW.saturating_sub(now.duration_since(t)).as_secs()),
                    "rtt_ms": g.stats.rtt_now(),
                    "queue": g.stats.queue.load(Ordering::Relaxed),
                    "skipped": g.stats.skipped.load(Ordering::Relaxed),
                    "behind": g.stats.behind.load(Ordering::Relaxed),
                    "since_s": now.duration_since(g.joined_at).as_secs(),
                    "why": g.left.map(|(_, w)| w),
                })
            })
            .collect();
        serde_json::json!({
            "open": true,
            "console": self.console,
            "playing": self.host_gone.is_none(),
            "host_away_s": self.host_gone.map(|t| now.duration_since(t).as_secs()),
            "invite": invite,
            "players_in": self.guests_in(),
            "guests": guests,
            "pending": self.pending.iter().map(|a| serde_json::json!({"id": a.id, "name": a.name, "state": "asking"})).collect::<Vec<_>>(),
            "settings": self.settings,
            "max_guests": MAX_GUESTS,
        })
    }

    /// What a guest hears every second.
    fn guest_status(&self, id: u64) -> Option<serde_json::Value> {
        let g = self.guests.iter().find(|g| g.id == id && g.active())?;
        Some(serde_json::json!({"status": {
            "player": g.player,
            "state": g.state(),
            "rtt_ms": g.stats.rtt_now(),
            "host": self.host_name,
            "block_system": self.settings.block_system_buttons && g.playing,
            "queue": g.stats.queue.load(Ordering::Relaxed),
            "skipped": g.stats.skipped.load(Ordering::Relaxed),
        }}))
    }

    /// What a bug report carries: counts, stages and health, never a name, invite key, code or
    /// token (the guests' names are the host's friends; a report is posted in public).
    fn diag(&self, now: Instant) -> serde_json::Value {
        let guests: Vec<_> = self
            .guests
            .iter()
            .map(|g| {
                let s = &g.stats;
                let n = s.rtt_n.load(Ordering::Relaxed);
                serde_json::json!({
                    "player": g.player,
                    "state": g.state(),
                    "why": g.left.map(|(_, w)| w),
                    "since_s": now.duration_since(g.joined_at).as_secs(),
                    "rtt_ms": s.rtt_now(),
                    "rtt_avg_ms": s.rtt_sum.load(Ordering::Relaxed).checked_div(n),
                    "rtt_max_ms": s.rtt_max.load(Ordering::Relaxed),
                    "queue": s.queue.load(Ordering::Relaxed),
                    "skipped_pictures": s.skipped.load(Ordering::Relaxed),
                    "pad_frames": s.pad_frames.load(Ordering::Relaxed),
                    "slot_open": g.slot_open,
                })
            })
            .collect();
        serde_json::json!({
            "open": true,
            "host_playing": self.host_gone.is_none(),
            "host_away_s": self.host_gone.map(|t| now.duration_since(t).as_secs()),
            "invite_open": self.current_invite(now).is_some(),
            "players_in": self.guests_in(),
            "asking": self.pending.len(),
            "guests": guests,
            "settings": self.settings,
            "reach_web_host": self.reach.web_base.is_some(),
        })
    }
}

/// Play Together on every console, for a bug report (`/api/diag/features`): see [`Party::diag`].
pub(crate) fn diag_summary() -> serde_json::Value {
    let now = Instant::now();
    let parties: Vec<_> = with_parties(|m| m.values().map(|p| p.diag(now)).collect());
    serde_json::json!({
        "parties": parties,
        "settings": settings(),
    })
}

/// Play Together on `console` needs the Remote Play session: friends are in, or an invite is out
/// (it lasts [`INVITE_TTL`], so a session nobody can reach does not run for ever).
pub(crate) fn needs_session(console: &str) -> bool {
    let now = Instant::now();
    with_party(&host_key(console), |p| {
        p.guests.iter().any(|g| g.active()) || p.invite.as_ref().is_some_and(|i| i.until > now)
    })
    .unwrap_or(false)
}

/// A party with an invite out, for other modules' tests.
#[cfg(test)]
pub(crate) fn test_open_party(console: &str) {
    let mut p = Party::new(console, 1, "Desk".into(), Settings::default());
    p.invite(false, Instant::now());
    with_parties(|m| m.insert(host_key(console), p));
}

#[cfg(test)]
pub(crate) fn test_close_party(console: &str) {
    with_parties(|m| m.remove(&host_key(console)));
}

/// Play Together, by console (`host_key`).
static PARTIES: Mutex<Option<HashMap<String, Party>>> = Mutex::new(None);

fn with_parties<R>(f: impl FnOnce(&mut HashMap<String, Party>) -> R) -> R {
    let mut g = PARTIES.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

fn with_party<R>(key: &str, f: impl FnOnce(&mut Party) -> R) -> Option<R> {
    with_parties(|m| m.get_mut(key).map(f))
}

fn apply(console: &str, fx: Vec<Effect>) {
    for e in fx {
        match e {
            Effect::Say(tx, out) => {
                let _ = tx.try_send(out);
            }
            Effect::Link(c) => link::send(console, c),
            Effect::Log(s) => crate::log_info!("{s}"),
            Effect::SeatPad(state) => crate::rp_stream_api::push_guest_pad(console, state),
            Effect::Seat(on) => crate::rp_stream_api::guest_seat(console, on),
        }
    }
}

// ── Seams: the host's session and the helper ─────────────────────────────────────────────────────

#[cfg(test)]
static FAKE_LIVE: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

/// The host's running session on `console`.
fn session_of(console: &str) -> Option<u64> {
    #[cfg(test)]
    {
        let g = FAKE_LIVE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = g.as_ref() {
            if let Some(v) = m.get(&host_key(console)) {
                return Some(*v).filter(|&s| s != 0);
            }
        }
    }
    crate::rp_stream_api::live_session(console)
}

/// What the stream's controller rumbles goes to the guest who has it.
pub(crate) fn stream_rumble(console: &str, left: u8, right: u8) {
    let key = host_key(console);
    let fx = with_party(&key, |p| {
        p.guests
            .iter()
            .find(|g| g.active() && g.player == STREAM_SEAT)
            .and_then(|g| g.conn.as_ref())
            .map(|c| {
                vec![Effect::Say(
                    c.tx.clone(),
                    Out::Text(serde_json::json!({"rumble": [left, right]})),
                )]
            })
            .unwrap_or_default()
    });
    apply(console, fx.unwrap_or_default());
}

/// Whether the console's helper has Play Together: `Ok(false)` for an older helper.
fn helper_has_vpad(console: &str) -> anyhow::Result<bool> {
    Ok(link::port_for(console).status(console)?.is_some())
}

// ── Link events (from the helper link) ───────────────────────────────────────────────────────────

fn on_link_event(console: &str, ev: link::Event) {
    let key = host_key(console);
    let now = Instant::now();
    let fx = with_party(&key, |p| match ev {
        link::Event::Output { slot, out } => p
            .guests
            .iter()
            .find(|g| g.active() && g.player == slot)
            .and_then(|g| g.conn.as_ref())
            .map(|c| {
                vec![Effect::Say(
                    c.tx.clone(),
                    Out::Text(serde_json::json!({"rumble": out.motors, "light": out.color})),
                )]
            })
            .unwrap_or_default(),
        link::Event::Reopened { slot } => {
            p.reopened(slot, now);
            Vec::new()
        }
        link::Event::Lost { slot } => {
            let id = p
                .guests
                .iter()
                .find(|g| g.active() && g.player == slot)
                .map(|g| g.id);
            match id {
                Some(id) => {
                    if let Some(g) = p.guest_mut(id) {
                        g.slot_open = false; // the helper no longer has it for us
                    }
                    p.leave(id, "lost", now)
                }
                None => Vec::new(),
            }
        }
    })
    .unwrap_or_default();
    apply(console, fx);
}

// ── Watching each open Play Together ─────────────────────────────────────────────────────────────

fn spawn_watch(key: String) {
    tokio::spawn(async move {
        let mut t = tokio::time::interval(WATCH_EVERY);
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            t.tick().await;
            let console = match with_party(&key, |p| p.console.clone()) {
                Some(c) => c,
                None => return,
            };
            let live = session_of(&console);
            let now = Instant::now();
            let r = with_parties(|m| {
                let p = m.get_mut(&key)?;
                let (fx, keep) = p.tick(now, live);
                if !keep {
                    m.remove(&key);
                }
                Some((fx, keep))
            });
            let Some((fx, keep)) = r else { return };
            apply(&console, fx);
            if !keep {
                return;
            }
        }
    });
}

// ── Guest socket ─────────────────────────────────────────────────────────────────────────────────

/// Sockets per address per minute: what a socket costs before it has said who it is.
pub(crate) struct ConnectLimit(Mutex<Option<HashMap<IpAddr, VecDeque<Instant>>>>);

impl ConnectLimit {
    pub(crate) const fn new() -> Self {
        ConnectLimit(Mutex::new(None))
    }

    /// One more socket from `ip`, unless it has had `limit` this minute.
    pub(crate) fn allow(&self, ip: IpAddr, limit: usize, now: Instant) -> bool {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let m = g.get_or_insert_with(HashMap::new);
        let minute = Duration::from_secs(60);
        m.retain(|_, q| {
            while q.front().is_some_and(|t| now.duration_since(*t) >= minute) {
                q.pop_front();
            }
            !q.is_empty()
        });
        let q = m.entry(ip).or_default();
        if q.len() >= limit {
            return false;
        }
        q.push_back(now);
        true
    }
}

/// This address's limit: this computer has the host API anyway.
pub(crate) fn connects_per_min(ip: IpAddr) -> usize {
    if ip.is_loopback() {
        LOOPBACK_CONNECTS_PER_MIN
    } else {
        CONNECTS_PER_MIN
    }
}

static CONNECTS: ConnectLimit = ConnectLimit::new();

/// A guest on a flaky link reconnects every 4 s or so, and each try counted against the limit
/// would lock them out of the place the engine is keeping for them.
fn holds_a_place(ip: IpAddr) -> bool {
    with_parties(|m| {
        m.values()
            .any(|p| p.guests.iter().any(|g| g.active() && g.ip == Some(ip)))
    })
}

/// At most [`CONNECTS_PER_MIN`] guest sockets per address per minute (this computer itself, or an
/// address with a guest in the game, [`LOOPBACK_CONNECTS_PER_MIN`]).
fn connect_allowed(ip: IpAddr, now: Instant) -> bool {
    connect_allowed_in(&CONNECTS, ip, now)
}

fn connect_allowed_in(table: &ConnectLimit, ip: IpAddr, now: Instant) -> bool {
    let limit = if holds_a_place(ip) {
        LOOPBACK_CONNECTS_PER_MIN
    } else {
        connects_per_min(ip)
    };
    table.allow(ip, limit, now)
}

/// `GET /api/join/ws`: a guest's socket (reached from the LAN; the socket checks the invite or
/// the resume token itself).
pub async fn ws(
    upgrade: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
) -> Response {
    if !connect_allowed(peer.ip(), Instant::now()) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "too many tries, wait a minute",
        )
            .into_response();
    }
    let ip = peer.ip();
    upgrade
        .max_message_size(GUEST_MSG_MAX)
        .max_frame_size(GUEST_MSG_MAX)
        .on_upgrade(move |s| guest_session(s, ip))
}

#[derive(serde::Deserialize)]
pub struct RelayQuery {
    url: String,
}

/// `GET /api/join/relay?url=ws://<host engine>:<port>/api/join/ws` (this computer only): an app
/// joining a friend's Play Together talks to its own engine (an app page may not open a plain
/// `ws://` to another machine), which passes the socket through unchanged. Only another engine's
/// `/api/join/ws` on a private address.
pub async fn relay(
    upgrade: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<RelayQuery>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    // Said on the socket: a page's WebSocket cannot read the body of a refused upgrade.
    let Some(target) = crate::pad_link::lan_ws_target(&q.url, "api/join/ws") else {
        return upgrade.on_upgrade(|mut s| async move {
            let _ = s.send(refused("address")).await;
        });
    };
    // What the guest sends is held to a guest socket's limit; the host's pictures coming back
    // are read by the relay's own client socket, which keeps its large one.
    upgrade
        .max_message_size(GUEST_MSG_MAX)
        .max_frame_size(GUEST_MSG_MAX)
        .on_upgrade(move |s| async move { crate::pad_link::relay_to(s, &target).await })
}

/// `GET /join`: the guest page.
pub async fn page() -> impl IntoResponse {
    Html(include_str!("join_page.html"))
}

fn say(v: serde_json::Value) -> Message {
    Message::Text(v.to_string().into())
}

fn refused(why: &str) -> Message {
    say(serde_json::json!({"stage": "refused", "why": why}))
}

#[derive(serde::Deserialize, Default)]
struct Hello {
    #[serde(default)]
    invite: Option<String>,
    #[serde(default)]
    resume: Option<String>,
    /// The invite's short code, typed or from a nearby app's presence (always asks the host).
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(serde::Deserialize)]
struct HelloMsg {
    hello: Hello,
}

/// A guest's name as they gave it: printable, short. Empty when they gave none: the host's own
/// app says "Friend" in its language.
fn clean_name(n: Option<&str>) -> String {
    let n: String = n
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(32)
        .collect();
    n.trim().to_string()
}

async fn read_hello(socket: &mut WebSocket) -> Option<Hello> {
    let deadline = tokio::time::sleep(HELLO_WAIT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => return None,
            m = socket.recv() => match m {
                Some(Ok(Message::Text(t))) => {
                    return serde_json::from_str::<HelloMsg>(t.as_str()).ok().map(|h| h.hello);
                }
                // nothing binary comes before the hello (pad frames start once in)
                Some(Ok(Message::Binary(_))) => return None,
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return None,
                _ => {}
            },
        }
    }
}

/// Waits for the socket to close (or anything that ends it), for selects that only need that.
async fn closed(socket: &mut WebSocket) {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
            _ => {}
        }
    }
}

/// How a new guest came: the link's key, the short code, or nothing (they saw Play Together in
/// the mesh presence). Every one of them asks the host.
enum Entry {
    Key(String),
    Code(String),
    Nearby,
}

static WRONG_CODES: Mutex<Option<HashMap<IpAddr, VecDeque<Instant>>>> = Mutex::new(None);

fn recent(q: &mut VecDeque<Instant>, now: Instant) {
    while q
        .front()
        .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
    {
        q.pop_front();
    }
}

/// The address may try a short code (fewer than [`WRONG_CODES_PER_MIN`] wrong ones this minute).
fn code_tries_left(ip: IpAddr, now: Instant) -> bool {
    if ip.is_loopback() {
        return true; // this computer has the host API anyway
    }
    let mut g = WRONG_CODES.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    m.retain(|_, q| {
        recent(q, now);
        !q.is_empty()
    });
    m.get(&ip).is_none_or(|q| q.len() < WRONG_CODES_PER_MIN)
}

fn code_wrong(ip: IpAddr, now: Instant) {
    let mut g = WRONG_CODES.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(HashMap::new)
        .entry(ip)
        .or_default()
        .push_back(now);
}

/// Where a guest token or invite leads.
enum Admit {
    /// A guest coming back: their console and id.
    Resume(String, u64),
    Refused(&'static str),
    /// A new guest allowed in: console and id.
    New(String, u64),
}

async fn guest_session(mut socket: WebSocket, ip: IpAddr) {
    let Some(hello) = read_hello(&mut socket).await else {
        let _ = socket.send(refused("hello")).await;
        return;
    };
    let name = clean_name(hello.name.as_deref());
    let t0 = Instant::now();
    // What the guest came with, never the credential itself.
    crate::log_detail!(
        "join: guest {ip} ({name}) says hello: resume={} invite={} code={}",
        hello.resume.as_deref().is_some_and(|t| !t.is_empty()),
        hello.invite.as_deref().is_some_and(|k| !k.is_empty()),
        hello.code.as_deref().is_some_and(|c| !c.trim().is_empty()),
    );
    let mut admit = None;
    if let Some(tok) = hello.resume.as_deref().filter(|t| !t.is_empty()) {
        admit = resume(&mut socket, tok).await;
        if admit.is_none() {
            return; // the socket closed while waiting
        }
        if matches!(admit, Some(Admit::Refused("expired")))
            && (hello.invite.is_some() || hello.code.is_some())
        {
            admit = None; // their spot is gone, but the invite may still let them in again
        }
    }
    let entry = match (hello.invite.as_deref(), hello.code.as_deref()) {
        (Some(k), _) if !k.is_empty() => Some(Entry::Key(k.to_string())),
        (_, Some(c)) if !c.trim().is_empty() => Some(Entry::Code(normalize_code(c))),
        // a "Playing nearby" join (the mesh presence): no credential, the host decides
        _ if hello.resume.is_none() => Some(Entry::Nearby),
        _ => None,
    };
    let admit = match admit {
        Some(a) => a,
        None => match entry {
            Some(e) => match ask(&mut socket, e, &name, ip).await {
                Some(a) => a,
                None => return,
            },
            None => Admit::Refused("key"),
        },
    };
    let ms = t0.elapsed().as_millis();
    let (console, id) = match admit {
        Admit::Refused(why) => {
            crate::log_detail!("join: guest {ip} ({name}) refused: {why} after {ms} ms");
            let _ = socket.send(refused(why)).await;
            return;
        }
        Admit::Resume(c, id) => {
            crate::log_detail!("join: guest {ip} ({name}) resumed on {c} after {ms} ms");
            (c, id)
        }
        Admit::New(c, id) => {
            crate::log_detail!("join: guest {ip} ({name}) let in on {c} after {ms} ms");
            (c, id)
        }
    };
    play(socket, console, id, ip).await;
}

/// A guest coming back with `token`: None when the socket closed while waiting for the host.
async fn resume(socket: &mut WebSocket, token: &str) -> Option<Admit> {
    let found = with_parties(|m| {
        m.iter().find_map(|(k, p)| {
            p.by_token(token)
                .map(|id| (k.clone(), p.console.clone(), id))
        })
    });
    let Some((key, console, id)) = found else {
        crate::log_detail!("join: a resume matched no guest (expired or never issued)");
        return Some(Admit::Refused("expired"));
    };
    // While the host restarts Play, the guest waits here.
    if with_party(&key, |p| p.host_gone.is_some()).unwrap_or(false) {
        crate::log_detail!(
            "join: guest {id} resumes on {console}; waiting for the host to play again"
        );
        if socket
            .send(say(
                serde_json::json!({"stage": "waiting", "why": "host_stopped"}),
            ))
            .await
            .is_err()
        {
            return None;
        }
        loop {
            let st = with_party(&key, |p| {
                p.guests.iter().any(|g| g.id == id && g.active()) && p.host_gone.is_some()
            });
            match st {
                None => return Some(Admit::Refused("closed")),
                Some(false) => break,
                Some(true) => {}
            }
            tokio::select! {
                _ = tokio::time::sleep(WATCH_EVERY) => {}
                _ = closed(socket) => return None,
            }
        }
    }
    let Some((player, token, open)) = with_party(&key, |p| {
        p.guests
            .iter()
            .find(|g| g.id == id && g.active())
            .map(|g| (g.player, g.pad_token.clone(), g.slot_open))
    })
    .flatten() else {
        return Some(Admit::Refused("expired"));
    };
    if !open && player != STREAM_SEAT {
        // The pad was removed while the host was away: make it again (the console asks who
        // they are again).
        crate::log_detail!(
            "join: guest {id} resumes on {console}: making Player {player}'s pad again"
        );
        match open_slot(&console, player, &token).await {
            link::OpenResult::Ok => {
                let now = Instant::now();
                // Still in? A kick or a loss while the pad was made means nobody will close
                // it later, so it goes now.
                let ok = with_party(&key, |p| {
                    p.guest_mut(id).filter(|g| g.active()).map(|g| {
                        g.slot_open = true;
                        g.playing = false;
                        g.picking_since = now;
                    })
                })
                .flatten()
                .is_some();
                if !ok {
                    link::send(&console, link::Cmd::Close { slot: player });
                    return Some(Admit::Refused("expired"));
                }
            }
            r => {
                let now = Instant::now();
                let fx = with_party(&key, |p| p.leave(id, "lost", now)).unwrap_or_default();
                apply(&console, fx);
                return Some(Admit::Refused(r.why()));
            }
        }
    }
    Some(Admit::Resume(console, id))
}

async fn open_slot(console: &str, slot: u8, token: &str) -> link::OpenResult {
    let (tx, rx) = oneshot::channel();
    link::send(
        console,
        link::Cmd::Open {
            slot,
            token: token.to_string(),
            reply: tx,
        },
    );
    let t0 = Instant::now();
    let r = match tokio::time::timeout(OPEN_WAIT, rx).await {
        Ok(Ok(r)) => r,
        _ => link::OpenResult::Failed("timeout".into()),
    };
    crate::log_detail!(
        "join: helper pad for Player {slot} on {console}: {r:?} in {} ms",
        t0.elapsed().as_millis()
    );
    if !matches!(r, link::OpenResult::Ok | link::OpenResult::Busy) {
        // Not ours to play: a pad the link still makes (a slow helper) or has goes again, so it
        // is not kept alive by heartbeats with nobody on it. Only while `token` owns the slot (a
        // busy slot is another owner's).
        link::send(
            console,
            link::Cmd::Abandon {
                slot,
                token: token.to_string(),
            },
        );
    }
    r
}

/// A new guest with an invite: checks, asks the host, makes their pad. None when the socket
/// closed while asking.
async fn ask(socket: &mut WebSocket, entry: Entry, name: &str, ip: IpAddr) -> Option<Admit> {
    let now = Instant::now();
    if matches!(entry, Entry::Code(_)) && !code_tries_left(ip, now) {
        return Some(Admit::Refused("slow_down"));
    }
    let found = with_parties(|m| {
        if m.is_empty() {
            return Err("closed");
        }
        if matches!(entry, Entry::Nearby) {
            // only when there is no doubt which: one Play Together open on this engine
            return match m.len() {
                1 => m
                    .iter()
                    .next()
                    .map(|(k, p)| (k.clone(), p.console.clone()))
                    .ok_or("closed"),
                _ => Err("key"),
            };
        }
        m.iter()
            .find(|(_, p)| match &entry {
                Entry::Key(k) => p.key_ok(k, now),
                Entry::Code(c) => p.code_ok(c, now),
                Entry::Nearby => false,
            })
            .map(|(k, p)| (k.clone(), p.console.clone()))
            .ok_or("key")
    });
    if found.is_err() && matches!(entry, Entry::Code(_)) {
        code_wrong(ip, now);
    }
    let (key, console) = match found {
        Ok(v) => v,
        Err(why) => return Some(Admit::Refused(why)),
    };
    let playing = with_party(&key, |p| p.host_gone.is_none()).unwrap_or(false);
    if !playing || session_of(&console).is_none() {
        return Some(Admit::Refused("not_playing"));
    }
    let c = console.clone();
    match tokio::task::spawn_blocking(move || helper_has_vpad(&c)).await {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => return Some(Admit::Refused("old_helper")),
        _ => return Some(Admit::Refused("unreachable")),
    }
    let id = next_id();
    let (tx, rx) = oneshot::channel();
    let queued = with_party(&key, |p| {
        let max = p.settings.max_players.clamp(1, MAX_GUESTS) as usize;
        if p.guests_in() + p.pending.len() >= max {
            return Err("full");
        }
        if p.pending.len() >= MAX_PENDING {
            return Err("full");
        }
        p.pending.push(Ask {
            id,
            name: name.to_string(),
            answer: tx,
        });
        Ok(())
    })
    .unwrap_or(Err("closed"));
    if let Err(why) = queued {
        return Some(Admit::Refused(why));
    }
    // By number in the log: a bug report is posted in public, and names are the host's friends.
    crate::log_info!("join: a guest asks to play on {console}");
    crate::log_detail!("join: {name} asks to play on {console}");
    if socket
        .send(say(serde_json::json!({"stage": "asking"})))
        .await
        .is_err()
    {
        with_party(&key, |p| p.pending.retain(|a| a.id != id));
        return None;
    }
    let mut gave_up = false;
    let d = tokio::select! {
        // Err: Play Together closed and the asks were dropped with it
        a = rx => a.ok(),
        _ = tokio::time::sleep(ASK_WAIT) => Some(Decision::NoAnswer),
        _ = closed(socket) => { gave_up = true; Some(Decision::Deny) }
    };
    with_party(&key, |p| p.pending.retain(|a| a.id != id));
    crate::log_detail!(
        "join: {name}'s ask on {console}: {} after {} ms",
        if gave_up {
            "the guest gave up".to_string()
        } else {
            format!("{d:?}")
        },
        now.elapsed().as_millis()
    );
    if gave_up {
        return None;
    }
    match d {
        None => return Some(Admit::Refused("closed")),
        Some(Decision::Deny) => return Some(Admit::Refused("denied")),
        Some(Decision::NoAnswer) => return Some(Admit::Refused("no_answer")),
        Some(Decision::HostStopped) => return Some(Admit::Refused("not_playing")),
        Some(Decision::Closed) => return Some(Admit::Refused("closed")),
        Some(Decision::Allow) => {}
    }
    // A player number, and the helper's pad for it (a slot another engine holds is skipped).
    let mut tried = Vec::new();
    loop {
        let now = Instant::now();
        let seat_free = crate::rp_stream_api::stream_seat_free(&console);
        let g = with_party(&key, |p| {
            let player = p.next_player(&tried, seat_free)?;
            p.admit(name, player, now)
                .map(|g| (g.id, g.player, g.pad_token.clone()))
        })
        .flatten();
        let Some((gid, player, pad_token)) = g else {
            return Some(Admit::Refused("full"));
        };
        if player == STREAM_SEAT {
            // Nothing to make, nobody to pick: the stream's user is already signed in.
            let ok = with_party(&key, |p| {
                p.guest_mut(gid).filter(|g| g.active()).map(|g| {
                    g.playing = true;
                    g.joined_at = now;
                })
            })
            .flatten()
            .is_some();
            if !ok {
                return Some(Admit::Refused("closed"));
            }
            crate::rp_stream_api::guest_seat(&console, true);
            crate::log_info!("join: a guest has the stream's controller on {console}");
            crate::log_detail!("join: {name} has the stream's controller on {console}");
            return Some(Admit::New(console, gid));
        }
        let r = open_slot(&console, player, &pad_token).await;
        let now = Instant::now();
        if r == link::OpenResult::Ok {
            let ok = with_party(&key, |p| {
                // Still in: a kick while the pad was made leaves nobody to close it.
                p.guest_mut(gid).filter(|g| g.active()).map(|g| {
                    g.slot_open = true;
                    g.picking_since = now;
                    g.joined_at = now;
                })
            })
            .flatten()
            .is_some();
            if !ok {
                link::send(&console, link::Cmd::Close { slot: player });
                return Some(Admit::Refused("closed"));
            }
            crate::log_info!("join: a guest is Player {player} on {console}");
            crate::log_detail!("join: {name} is Player {player} on {console}");
            return Some(Admit::New(console, gid));
        }
        // Not made: this guest record goes (quietly: they never played).
        with_party(&key, |p| p.guests.retain(|g| g.id != gid));
        match r {
            link::OpenResult::Busy => {
                crate::log_detail!(
                    "join: Player {player} on {console} is held elsewhere; trying the next"
                );
                tried.push(player)
            }
            r => return Some(Admit::Refused(r.why())),
        }
    }
}

/// Holds a guest's pad frames to [`PAD_RATE`] (bursts of [`PAD_BURST`]): one over it waits as the
/// latest (a newer one replaces it), so a flooding guest costs the link what a normal one does.
struct PadPacer<T> {
    tokens: f64,
    at: Instant,
    held: Option<T>,
}

impl<T> PadPacer<T> {
    fn new(now: Instant) -> Self {
        PadPacer {
            tokens: PAD_BURST,
            at: now,
            held: None,
        }
    }

    fn refill(&mut self, now: Instant) {
        let dt = now.saturating_duration_since(self.at).as_secs_f64();
        self.tokens = (self.tokens + dt * PAD_RATE).min(PAD_BURST);
        self.at = now;
    }

    /// A frame: back to send now, or None (it is held as the latest).
    fn offer(&mut self, now: Instant, v: T) -> Option<T> {
        self.refill(now);
        if self.held.is_none() && self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return Some(v);
        }
        self.held = Some(v);
        None
    }

    /// When the held frame may go.
    fn due(&self) -> Option<Instant> {
        self.held.as_ref()?;
        let wait = ((1.0 - self.tokens).max(0.0) / PAD_RATE).min(1.0);
        Some(self.at + Duration::from_secs_f64(wait))
    }

    /// The held frame, once it may go.
    fn take(&mut self, now: Instant) -> Option<T> {
        self.refill(now);
        if self.held.is_some() && self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return self.held.take();
        }
        None
    }
}

/// A guest's pad state as the stream's controller takes it.
fn controller_state(st: &PadState) -> ControllerState {
    use ps5upload_core::rp_stream::button as rp;
    use ps5upload_core::rp_stream::feedback::{MotionSample, TOUCHPAD_H};
    const MAP: [(u32, u32); 16] = [
        (vpad::button::CROSS, rp::CROSS),
        (vpad::button::CIRCLE, rp::CIRCLE),
        (vpad::button::SQUARE, rp::SQUARE),
        (vpad::button::TRIANGLE, rp::TRIANGLE),
        (vpad::button::LEFT, rp::DPAD_LEFT),
        (vpad::button::RIGHT, rp::DPAD_RIGHT),
        (vpad::button::UP, rp::DPAD_UP),
        (vpad::button::DOWN, rp::DPAD_DOWN),
        (vpad::button::L1, rp::L1),
        (vpad::button::R1, rp::R1),
        (vpad::button::L3, rp::L3),
        (vpad::button::R3, rp::R3),
        (vpad::button::OPTIONS, rp::OPTIONS),
        (vpad::button::CREATE, rp::SHARE),
        (vpad::button::TOUCHPAD, rp::TOUCHPAD),
        (vpad::button::PS, rp::PS),
    ];
    let axis = |v: u8| ((i32::from(v) - 128) * 32767 / 127).clamp(-32768, 32767) as i16;
    let milli = |v: [f32; 3]| v.map(|x| (x * 1000.0) as i32);
    ControllerState {
        buttons: MAP
            .iter()
            .filter(|(pad, _)| st.buttons & pad != 0)
            .fold(0, |acc, (_, b)| acc | b),
        l2: st.l2,
        r2: st.r2,
        lx: axis(st.lx),
        ly: axis(st.ly),
        rx: axis(st.rx),
        ry: axis(st.ry),
        // The guest's touch is in 1920 x 1080, the stream's touchpad is shorter.
        touch: st
            .touch
            .first()
            .map(|t| [t.x, (u32::from(t.y) * u32::from(TOUCHPAD_H) / 1080) as u16]),
        motion: st.motion.map(|m| MotionSample {
            gyro: milli(m.gyro),
            accel: milli(m.accel),
            t: crate::rp_stream_api::motion_clock_us(),
        }),
        turbo: 0,
    }
}

/// A guest's pad state to their pad (PS and Create taken while they play).
fn forward_pad(key: &str, console: &str, id: u64, gen: u64, mut st: PadState) {
    let now = Instant::now();
    let mask = with_party(key, |p| {
        let m = p.is_current(id, gen).then(|| p.strip_mask(id)).flatten();
        if let Some((STREAM_SEAT, _)) = m {
            if let Some(g) = p.guest_mut(id) {
                g.seat_input_at = Some(now);
            }
        }
        m
    });
    if let Some((slot, strip)) = mask.flatten() {
        st.buttons &= !strip;
        if slot == STREAM_SEAT {
            crate::rp_stream_api::push_guest_pad(console, Some(controller_state(&st)));
        } else {
            link::send(console, link::Cmd::State { slot, state: st });
        }
    }
}

/// A skipped picture says a guest is behind for this many status ticks (the host's panel).
const BEHIND_FOR_TICKS: u32 = 5;

/// The least time between a guest's keyframe requests that reach the console.
const KEY_ASK_GAP: Duration = Duration::from_millis(1000);

/// A guest's keyframe requests, at most one per KEY_ASK_GAP.
#[derive(Default)]
struct KeyAsk {
    last: Option<Instant>,
}

impl KeyAsk {
    /// True when a request now may go to the console.
    fn take(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|t| now.saturating_duration_since(t) < KEY_ASK_GAP)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

fn now_ms() -> u64 {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// A guest who is in: picture and sound out, pad states in, until they leave or are let go.
async fn play(mut socket: WebSocket, console: String, id: u64, ip: IpAddr) {
    static GEN: AtomicU64 = AtomicU64::new(1);
    let gen = GEN.fetch_add(1, Ordering::Relaxed);
    let key = host_key(&console);
    let (tx, mut rx) = mpsc::channel::<Out>(64);
    let Some((fx, player, token, stats)) = with_party(&key, |p| {
        let fx = p.attach(id, gen, tx);
        let g = p.guest_mut(id)?;
        g.ip = Some(ip);
        Some((fx, g.player, g.token.clone(), g.stats.clone()))
    })
    .flatten() else {
        let _ = socket.send(refused("closed")).await;
        return;
    };
    apply(&console, fx);
    let spect = SPECTATOR_BASE | gen;
    let feed =
        crate::rp_stream_api::add_player(spect, &console, PLAYER_QUEUE, stats.skipped.clone());
    let hevc = feed.as_ref().map(|f| f.hevc).unwrap_or(false);
    crate::log_detail!(
        "join: Player {player} on {console} connected (socket {gen}): picture {}, hevc {hevc}",
        if feed.is_some() {
            "on"
        } else {
            "none (no session)"
        }
    );
    let (mut video, mut other) = match feed {
        Some(f) => (Some(f.video), Some(f.other)),
        None => (None, None),
    };
    let ended = socket
        .send(say(serde_json::json!({
            "stage": "joined", "player": player, "token": token, "hevc": hevc,
        })))
        .await
        .is_err();
    let mut tick = tokio::time::interval(STATUS_EVERY);
    let mut left = false;
    let mut pacer = PadPacer::new(Instant::now());
    let mut key_ask = KeyAsk::default();
    let mut skipped_seen = stats.skipped.load(Ordering::Relaxed);
    let mut behind_for = 0u32;
    loop {
        if ended {
            break; // the joined message could not go: the socket is gone
        }
        tokio::select! {
            biased;
            out = rx.recv() => match out {
                Some(Out::Text(v)) => if socket.send(say(v)).await.is_err() { break },
                Some(Out::Bye(why)) => {
                    let _ = socket.send(say(serde_json::json!({"bye": why}))).await;
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
                None => break,
            },
            _ = async {
                match pacer.due() {
                    Some(t) => tokio::time::sleep_until(t.into()).await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some(st) = pacer.take(Instant::now()) {
                    forward_pad(&key, &console, id, gen, st);
                }
            },
            inc = socket.recv() => match inc {
                Some(Ok(Message::Binary(b))) => {
                    if let Some((_seq, st)) = parse_pad(&b) {
                        stats.pad_frames.fetch_add(1, Ordering::Relaxed);
                        if let Some(st) = pacer.offer(Instant::now(), st) {
                            forward_pad(&key, &console, id, gen, st);
                        }
                    }
                }
                Some(Ok(Message::Text(t))) => {
                    #[derive(serde::Deserialize)]
                    struct In {
                        #[serde(default)] ping: Option<serde_json::Value>,
                        #[serde(default)] leave: Option<serde_json::Value>,
                        #[serde(default)] keyframe: bool,
                    }
                    if let Ok(m) = serde_json::from_str::<In>(t.as_str()) {
                        if m.leave.is_some() {
                            left = true;
                            break;
                        }
                        // The guest's picture waits for a keyframe (#648): ask the console, at
                        // most once a KEY_ASK_GAP so a guest can't flood the host's stream.
                        if m.keyframe {
                            if key_ask.take(Instant::now()) {
                                crate::log_detail!(
                                    "join: Player {player} on {console} asks for a keyframe ({} pictures skipped so far)",
                                    stats.skipped.load(Ordering::Relaxed)
                                );
                                crate::rp_stream_api::request_keyframe(&console, spect);
                            } else {
                                crate::log_detail!("join: Player {player} on {console}: keyframe ask held back (one a second)");
                            }
                        }
                        if let Some(p) = m.ping {
                            if socket.send(say(serde_json::json!({"pong": p}))).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                Some(Ok(Message::Pong(p))) if p.len() == 8 => {
                    let mut b = [0u8; 8];
                    b.copy_from_slice(&p);
                    let rtt = now_ms().saturating_sub(u64::from_le_bytes(b));
                    if rtt < 60_000 {
                        stats.rtt(rtt as u32);
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            // Sound before pictures, as the host's page gets
            // them (rp_stream_api's ctl queue goes first): on a link the pictures fill, a picture
            // first left the sound waiting behind every one of them, late and then dropped.
            f = async {
                match other.as_mut() {
                    Some(r) => r.recv().await,
                    None => std::future::pending().await,
                }
            } => match f {
                Some(m) => if socket.send(m).await.is_err() { break },
                None => other = None,
            },
            f = async {
                match video.as_mut() {
                    Some(r) => r.recv().await,
                    None => std::future::pending().await,
                }
            } => match f {
                Some(m) => {
                    if let Some(r) = video.as_ref() {
                        stats.queue.store(r.len(), Ordering::Relaxed);
                    }
                    if socket.send(m).await.is_err() { break }
                }
                None => video = None,
            },
            _ = tick.tick() => {
                let skipped = stats.skipped.load(Ordering::Relaxed);
                behind_for = if skipped != skipped_seen { BEHIND_FOR_TICKS } else { behind_for.saturating_sub(1) };
                skipped_seen = skipped;
                stats.behind.store(behind_for > 0, Ordering::Relaxed);
                let _ = socket.send(Message::Ping(now_ms().to_le_bytes().to_vec().into())).await;
                let status = with_party(&key, |p| {
                    p.is_current(id, gen).then(|| p.guest_status(id)).flatten()
                });
                match status.flatten() {
                    Some(v) => if socket.send(say(v)).await.is_err() { break },
                    None => {
                        // Let go (kicked, closed, replaced): the notice saying why is sent
                        // just after, outside the lock, so give it a moment to arrive.
                        let until = tokio::time::Instant::now() + Duration::from_secs(1);
                        while let Ok(Some(out)) = tokio::time::timeout_at(until, rx.recv()).await {
                            if let Out::Bye(why) = out {
                                let _ = socket.send(say(serde_json::json!({"bye": why}))).await;
                                let _ = socket.send(Message::Close(None)).await;
                                break;
                            }
                        }
                        break;
                    }
                }
            },
        }
    }
    crate::rp_stream_api::remove_spectator(spect);
    crate::log_detail!(
        "join: Player {player} on {console} socket {gen} closed ({}): rtt now {:?} ms, {} pictures skipped, {} pad frames",
        if left { "left" } else { "disconnected" },
        stats.rtt_now(),
        stats.skipped.load(Ordering::Relaxed),
        stats.pad_frames.load(Ordering::Relaxed),
    );
    let now = Instant::now();
    let fx = with_party(&key, |p| {
        if left {
            p.leave(id, "left", now)
        } else {
            p.detach(id, gen, now)
        }
    })
    .unwrap_or_default();
    apply(&console, fx);
}

// ── Host API ─────────────────────────────────────────────────────────────────────────────────────

/// The host API answers this computer (a loopback peer, which must also address it by a
/// loopback name: a re-pointed name, DNS rebinding, carries its own) and the peers
/// `PS5UPLOAD_ALLOW_IP` admits (the web UI of an engine on a NAS or in Docker, reached from a
/// browser on the LAN: loopback_guard in lib.rs let only those through, as for every other API).
fn host_allowed(peer: IpAddr, headers: &HeaderMap) -> bool {
    !peer.is_loopback()
        || crate::vinput_api::host_is_local(headers)
        // A reverse proxy on this computer, under the name its operator listed.
        || crate::host_guard::host_named_by_operator(headers)
}

fn guard(peer: &std::net::SocketAddr, headers: &HeaderMap) -> Option<Response> {
    (!host_allowed(peer.ip(), headers)).then(crate::vinput_api::host_not_local)
}

/// What the host's request says about how guests reach this engine.
fn reach_from(headers: &HeaderMap) -> (u16, Option<String>) {
    (
        crate::pad_link::lan_port(headers),
        crate::pad_link::web_base(headers),
    )
}

/// This computer's addresses guests can use: home network, then a VPN's (WireGuard…), then
/// Tailscale's (pad_link::reach_ips).
fn ip_bases(console: &str, reach: &Reach) -> Vec<String> {
    crate::pad_link::reach_ips(Some(console))
        .0
        .into_iter()
        .map(|ip| crate::pad_link::ip_base(ip, reach.port))
        .collect()
}

/// The engine's addresses guests can use, best first: what the host's browser used (a web
/// UI), then this computer's LAN addresses toward the console.
fn bases(console: &str, reach: &Reach) -> Vec<String> {
    let mut out: Vec<String> = reach.web_base.iter().cloned().collect();
    for b in ip_bases(console, reach) {
        if !out.contains(&b) {
            out.push(b);
        }
    }
    out
}

fn bad(code: StatusCode, err: &str) -> Response {
    (code, Json(serde_json::json!({"error": err}))).into_response()
}

/// The invite as the host's page shows it: `key` (in the link), `code` / `code_display` (the
/// short code to read out), `url` (best first in `urls`), `expires_in_s`.
fn invite_json(
    console: &str,
    reach: &Reach,
    inv: Option<InviteInfo>,
    now: Instant,
) -> serde_json::Value {
    let Some(inv) = inv else {
        return serde_json::Value::Null;
    };
    let prefix = crate::host_guard::base_path();
    let urls: Vec<String> = bases(console, reach)
        .iter()
        .map(|b| format!("{b}{prefix}/join#i={}", inv.key))
        .collect();
    serde_json::json!({
        "key": inv.key,
        "code": inv.code,
        "code_display": code_display(&inv.code),
        "url": urls.first(),
        "urls": urls,
        // This computer's Tailscale address (one of `urls`), for the page to label.
        "tailscale": crate::pad_link::tailscale_ip().map(|ip| ip.to_string()),
        "expires_in_s": inv.until.saturating_duration_since(now).as_secs(),
    })
}

fn state_json(console: &str) -> serde_json::Value {
    let key = host_key(console);
    let now = Instant::now();
    let snap = with_party(&key, |p| {
        (
            p.console.clone(),
            p.reach.clone(),
            p.current_invite(now),
            p.snapshot(now, serde_json::Value::Null),
        )
    });
    match snap {
        Some((c, reach, inv, mut v)) => {
            v["invite"] = invite_json(&c, &reach, inv, now);
            // the LAN socket guests connect to (nearby apps get it from the mesh presence): an
            // address when there is one, which an app's relay can use as it is
            let prefix = crate::host_guard::base_path();
            v["ws"] = ip_bases(&c, &reach)
                .first()
                .cloned()
                .or_else(|| reach.web_base.clone())
                .map(|b| format!("{}{prefix}/api/join/ws", b.replacen("http", "ws", 1)))
                .into();
            v
        }
        None => serde_json::json!({
            "open": false,
            "console": console,
            "playing": session_of(console).is_some(),
            "invite": null,
            "players_in": 0,
            "guests": [],
            "pending": [],
            "settings": settings(),
            "max_guests": MAX_GUESTS,
        }),
    }
}

#[derive(serde::Deserialize)]
pub struct AddrQuery {
    #[serde(alias = "console", alias = "host")]
    addr: String,
}

/// `GET /api/join?addr=`: Play Together on that console (`open` false when it is not).
pub async fn get_state(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<AddrQuery>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    touch_reach(&q.addr, &headers);
    Json(state_json(&q.addr)).into_response()
}

#[derive(serde::Deserialize)]
pub struct AddrBody {
    #[serde(alias = "console", alias = "host")]
    addr: String,
}

/// The host's request: how guests reach this engine now.
fn touch_reach(console: &str, headers: &HeaderMap) {
    let (port, web_base) = reach_from(headers);
    with_party(&host_key(console), |p| {
        p.reach.port = port;
        p.reach.web_base = web_base;
    });
}

/// `POST /api/join/open {addr}`: open Play Together for the session being played on `addr`,
/// with an invite ready (409 `not_playing` when none runs, 409 `old_helper` when the helper has
/// no guest controllers: "Update the helper to play together"; 502 when the console does not
/// answer). Opening an open one changes nothing. (Nearby apps learn of it from the host app's
/// mesh presence, which the app publishes from `GET /api/join`.)
pub async fn open(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<AddrBody>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    let Some(session) = session_of(&b.addr) else {
        return bad(StatusCode::CONFLICT, "not_playing");
    };
    let key = host_key(&b.addr);
    if with_party(&key, |_| ()).is_none() {
        // An older helper has no guest controllers: the host is told before anyone is invited.
        let c = b.addr.clone();
        match tokio::task::spawn_blocking(move || helper_has_vpad(&c)).await {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => return bad(StatusCode::CONFLICT, "old_helper"),
            Ok(Err(e)) => return bad(StatusCode::BAD_GATEWAY, &format!("unreachable: {e:#}")),
            Err(e) => return bad(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        }
    }
    let host_name = crate::mesh_api::device_name();
    let created = with_parties(|m| {
        if m.contains_key(&key) {
            return false;
        }
        m.insert(
            key.clone(),
            Party::new(&b.addr, session, host_name, settings()),
        );
        true
    });
    touch_reach(&b.addr, &headers);
    if created {
        let now = Instant::now();
        with_party(&key, |p| p.invite(false, now));
        crate::log_info!("join: Play Together open on {}", b.addr);
        spawn_watch(key);
    }
    Json(state_json(&b.addr)).into_response()
}

/// `POST /api/join/close {addr}`: every guest out, invites void.
pub async fn close(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<AddrBody>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    let key = host_key(&b.addr);
    let now = Instant::now();
    let fx = with_parties(|m| m.remove(&key).map(|mut p| p.close(now))).unwrap_or_default();
    apply(&b.addr, fx);
    Json(state_json(&b.addr)).into_response()
}

#[derive(serde::Deserialize)]
pub struct InviteBody {
    #[serde(alias = "console", alias = "host")]
    addr: String,
    /// A new key (the old link stops working).
    #[serde(default)]
    new: bool,
}

/// `POST /api/join/invite {addr, new?}`: `{key, code, code_display, url, urls, expires_in_s}`.
/// The same invite again until it expires, unless `new` (the old link and code stop working).
pub async fn invite(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<InviteBody>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    let now = Instant::now();
    touch_reach(&b.addr, &headers);
    match with_party(&host_key(&b.addr), |p| {
        (p.invite(b.new, now), p.console.clone(), p.reach.clone())
    }) {
        None => bad(StatusCode::CONFLICT, "closed"),
        Some((None, _, _)) => bad(StatusCode::INTERNAL_SERVER_ERROR, "no_random"),
        Some((inv, c, reach)) => Json(invite_json(&c, &reach, inv, now)).into_response(),
    }
}

#[derive(serde::Deserialize)]
pub struct DecideBody {
    id: u64,
    allow: bool,
}

/// `POST /api/join/decide {id, allow}`: answer a guest who asks.
pub async fn decide(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<DecideBody>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    let found = with_parties(|m| {
        m.values_mut().find_map(|p| {
            let i = p.pending.iter().position(|a| a.id == b.id)?;
            Some(p.pending.remove(i))
        })
    });
    let ok = found.is_some();
    if let Some(a) = found {
        let _ = a.answer.send(if b.allow {
            Decision::Allow
        } else {
            Decision::Deny
        });
    }
    Json(serde_json::json!({ "ok": ok })).into_response()
}

#[derive(serde::Deserialize)]
pub struct PlayerBody {
    #[serde(alias = "console", alias = "host")]
    addr: String,
    player: u8,
}

/// `POST /api/join/kick {addr, player}`: let a guest go (their token stops working).
pub async fn kick(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<PlayerBody>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    let now = Instant::now();
    let fx = with_party(&host_key(&b.addr), |p| {
        let id = p
            .guests
            .iter()
            .find(|g| g.active() && g.player == b.player)
            .map(|g| g.id)?;
        Some(p.leave(id, "kicked", now))
    })
    .flatten();
    let ok = fx.is_some();
    apply(&b.addr, fx.unwrap_or_default());
    Json(serde_json::json!({ "ok": ok })).into_response()
}

/// `POST /api/join/playing {addr, player}`: the host saw the guest pick a user ("They're in"):
/// PS and Create are blocked for them from now (when the setting is on).
pub async fn playing(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<PlayerBody>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    let ok = with_party(&host_key(&b.addr), |p| {
        p.guests
            .iter_mut()
            .find(|g| g.active() && g.player == b.player)
            .map(|g| g.playing = true)
            .is_some()
    })
    .unwrap_or(false);
    Json(serde_json::json!({ "ok": ok })).into_response()
}

#[derive(serde::Deserialize)]
pub struct SettingsBody {
    #[serde(default)]
    block_system_buttons: Option<bool>,
    #[serde(default)]
    max_players: Option<u8>,
}

/// `POST /api/join/settings {block_system_buttons?, max_players?}`: the host's choices (every
/// console). Fewer players lets nobody go; it only stops new ones.
pub async fn set_settings(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<SettingsBody>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    let mut s = settings();
    if let Some(v) = b.block_system_buttons {
        s.block_system_buttons = v;
    }
    if let Some(v) = b.max_players {
        s.max_players = v.clamp(1, MAX_GUESTS);
    }
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
    with_parties(|m| {
        for p in m.values_mut() {
            p.settings = s;
        }
    });
    Json(serde_json::json!({ "settings": s })).into_response()
}

/// The routes (lib.rs registers the same ones): the host API (this computer only) and the
/// guest's page and socket (the LAN).
#[cfg(test)]
pub(crate) fn router<S: Clone + Send + Sync + 'static>() -> axum::Router<S> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/join", get(page))
        .route("/api/join/ws", get(ws))
        .route("/api/join/relay", get(relay))
        .route("/api/join", get(get_state))
        .route("/api/join/open", post(open))
        .route("/api/join/close", post(close))
        .route("/api/join/invite", post(invite))
        .route("/api/join/decide", post(decide))
        .route("/api/join/kick", post(kick))
        .route("/api/join/playing", post(playing))
        .route("/api/join/settings", post(set_settings))
}

// ── The helper link ──────────────────────────────────────────────────────────────────────────────

pub(crate) mod link {
    //! One thread per console sends the guests' pads to the helper (`vpad.*`).

    use std::collections::{BTreeMap, HashMap};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use anyhow::Result;
    use ps5upload_core::vpad::{self, Ack, Opened, Output, PadState, StateItem};
    use tokio::sync::oneshot;

    use crate::vinput_api::host_key;

    /// A change of a button is held at least this long before it is undone (a quick tap still
    /// reaches the game, which reads the pad once a frame).
    pub(crate) const HOLD: Duration = Duration::from_millis(16);
    /// A link with no slots ends after this long.
    const IDLE_END: Duration = Duration::from_secs(5);
    /// Commands taken before the link does what is due (states, heartbeats).
    const CMDS_PER_ROUND: usize = 64;
    /// Helper failures wait this long, doubling up to [`BACKOFF_MAX`].
    const BACKOFF_MIN: Duration = Duration::from_millis(250);
    const BACKOFF_MAX: Duration = Duration::from_secs(4);
    /// A pad the helper had and now refuses to make again (a user signed out, say) is given up
    /// after this long: the guest is told, rather than staying "playing" with dead input.
    pub(crate) const REOPEN_GIVE_UP: Duration = Duration::from_secs(30);
    /// The rumble and light bar are read this often while they change, else every
    /// [`OUTPUT_IDLE`].
    const OUTPUT_FAST: Duration = Duration::from_millis(50);
    const OUTPUT_IDLE: Duration = Duration::from_millis(250);
    /// After a change of rumble or light, reading stays fast this long.
    const OUTPUT_FAST_FOR: Duration = Duration::from_secs(1);

    /// The helper's answer to a body it could not read.
    const ERR_BAD_REQUEST: &str = "bad_request";

    /// A refusal about one pad (not the helper as a whole).
    fn slot_refusal(err: Option<&str>) -> bool {
        matches!(
            err,
            Some("vpad_not_open") | Some("vpad_not_owner") | Some(ERR_BAD_REQUEST)
        )
    }

    /// The helper (a test seam).
    pub(crate) trait Port: Send + Sync + 'static {
        fn open(&self, host: &str, slot: u8, token: &str) -> Result<Option<Opened>>;
        fn states(&self, host: &str, items: &[StateItem]) -> Result<Option<Ack>>;
        fn hb(&self, host: &str, slot: u8, token: &str, live: bool) -> Result<Option<Ack>>;
        fn close(&self, host: &str, slot: u8, token: &str) -> Result<Option<Ack>>;
        fn output(&self, host: &str, slot: u8) -> Result<Option<Output>>;
        fn status(&self, host: &str) -> Result<Option<serde_json::Value>>;
    }

    struct HelperPort;

    impl Port for HelperPort {
        fn open(&self, host: &str, slot: u8, token: &str) -> Result<Option<Opened>> {
            vpad::open(host, slot, token, None)
        }
        fn states(&self, host: &str, items: &[StateItem]) -> Result<Option<Ack>> {
            vpad::states(host, items)
        }
        fn hb(&self, host: &str, slot: u8, token: &str, live: bool) -> Result<Option<Ack>> {
            vpad::hb(host, slot, token, live)
        }
        fn close(&self, host: &str, slot: u8, token: &str) -> Result<Option<Ack>> {
            vpad::close(host, slot, token)
        }
        fn output(&self, host: &str, slot: u8) -> Result<Option<Output>> {
            vpad::output(host, slot)
        }
        fn status(&self, host: &str) -> Result<Option<serde_json::Value>> {
            vpad::status(host)
        }
    }

    #[cfg(test)]
    pub(crate) static TEST_PORTS: Mutex<Option<HashMap<String, Arc<dyn Port>>>> = Mutex::new(None);

    /// The helper port for `console` (a test may put a fake one in).
    pub(crate) fn port_for(console: &str) -> Arc<dyn Port> {
        #[cfg(test)]
        {
            let g = TEST_PORTS.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(p) = g.as_ref().and_then(|m| m.get(&host_key(console))) {
                return p.clone();
            }
        }
        let _ = console;
        Arc::new(HelperPort)
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) enum OpenResult {
        Ok,
        /// Another owner holds the slot.
        Busy,
        /// The helper has no Play Together.
        OldHelper,
        Failed(String),
    }

    impl OpenResult {
        /// The guest's refusal for it.
        pub(crate) fn why(&self) -> &'static str {
            match self {
                OpenResult::Ok => "ok",
                OpenResult::Busy => "full",
                OpenResult::OldHelper => "old_helper",
                OpenResult::Failed(_) => "unreachable",
            }
        }
    }

    pub(crate) enum Cmd {
        /// Make (or keep) Player `slot`'s pad for `token`.
        Open {
            slot: u8,
            token: String,
            reply: oneshot::Sender<OpenResult>,
        },
        /// The guest's latest state.
        State { slot: u8, state: PadState },
        /// Let go of everything (the guest's socket closed).
        Neutral { slot: u8 },
        /// Remove the pad.
        Close { slot: u8 },
        /// Remove the pad if `token` still owns it (an open its guest stopped waiting for).
        Abandon { slot: u8, token: String },
    }

    /// What the link tells join.rs.
    #[derive(Debug, PartialEq)]
    pub(crate) enum Event {
        /// The game changed Player `slot`'s rumble or light bar.
        Output { slot: u8, out: Output },
        /// The pad was made anew (the helper restarted).
        Reopened { slot: u8 },
        /// The helper no longer has the slot for us and cannot make it again.
        Lost { slot: u8 },
    }

    pub(crate) type EventFn = Arc<dyn Fn(Event) + Send + Sync>;

    /// One slot's input: the latest state from the guest, the last one the console got, and the
    /// presses and releases since (so none is lost to coalescing).
    #[derive(Clone, Debug)]
    pub(crate) struct SlotIo {
        pub sent: PadState,
        pub latest: PadState,
        /// Buttons that went down / up in the guest's states since the last send.
        down: u32,
        up: u32,
        /// When each button last changed in what was sent.
        changed_at: [Option<Instant>; 32],
        /// Send even if equal (a new pad starts neutral and must get the latest).
        force: bool,
    }

    impl Default for SlotIo {
        fn default() -> Self {
            SlotIo {
                sent: PadState::NEUTRAL,
                latest: PadState::NEUTRAL,
                down: 0,
                up: 0,
                changed_at: [None; 32],
                force: false,
            }
        }
    }

    impl SlotIo {
        pub(crate) fn push(&mut self, s: PadState) {
            self.down |= s.buttons & !self.latest.buttons;
            self.up |= self.latest.buttons & !s.buttons;
            self.latest = s;
        }

        /// What to send now (None: nothing new, or a hold must run out first), and when to look
        /// again for a change still held back.
        ///
        /// Latest wins, except for the buttons: a button pressed and released between two sends
        /// is sent pressed first (and released at least [`HOLD`] later); one released and
        /// pressed again is sent released first. A button the console just saw change keeps its
        /// state for [`HOLD`] before it changes back; sticks, triggers and touch still go at once.
        pub(crate) fn plan(&self, now: Instant) -> (Option<PadState>, Option<Instant>) {
            let latest = self.latest.buttons;
            let sent = self.sent.buttons;
            let mut b = latest;
            b |= self.down & !latest & !sent; // pressed and released since: send the press
            b &= !(self.up & latest & sent); // released and pressed again: send the release
            let mut later: Option<Instant> = None;
            let diff = b ^ sent;
            for bit in 0..32 {
                let m = 1u32 << bit;
                if diff & m == 0 {
                    continue;
                }
                if let Some(t) = self.changed_at[bit] {
                    let until = t + HOLD;
                    if until > now {
                        b ^= m; // keep what the console has until the hold runs out
                        later = Some(later.map_or(until, |l: Instant| l.min(until)));
                    }
                }
            }
            if b != latest && later.is_none() {
                // an edge goes now, the latest follows once it has been held
                later = Some(now + HOLD);
            }
            let mut cand = self.latest.clone();
            cand.buttons = b;
            let send = (self.force || cand != self.sent).then_some(cand);
            (send, later)
        }

        /// `s` reached the console.
        pub(crate) fn commit(&mut self, s: PadState, now: Instant) {
            let diff = s.buttons ^ self.sent.buttons;
            for bit in 0..32 {
                if diff & (1 << bit) != 0 {
                    self.changed_at[bit] = Some(now);
                }
            }
            self.sent = s;
            self.down = 0;
            self.up = 0;
            self.force = false;
        }
    }

    struct Slot {
        token: String,
        opened: bool,
        handle: Option<i64>,
        waiters: Vec<oneshot::Sender<OpenResult>>,
        seq: u32,
        io: SlotIo,
        /// Since when the helper has refused to make this pad again.
        refused_since: Option<Instant>,
        /// The guest is connected and sending: heartbeats say `live` (the helper keeps a held
        /// stick held). False while they reconnect (the pad stays, neutral).
        live: bool,
        last_call: Instant,
        last_input: Instant,
        out_last: Option<Output>,
        out_due: Instant,
        out_fast_until: Instant,
    }

    impl Slot {
        fn new(token: String, now: Instant) -> Self {
            Slot {
                token,
                opened: false,
                handle: None,
                waiters: Vec::new(),
                seq: 0,
                io: SlotIo::default(),
                refused_since: None,
                live: false,
                last_call: now,
                last_input: now,
                out_last: None,
                out_due: now,
                out_fast_until: now,
            }
        }
    }

    pub(crate) struct LinkState {
        host: String,
        port: Arc<dyn Port>,
        events: EventFn,
        slots: BTreeMap<u8, Slot>,
        /// Pads to remove: (slot, token).
        closing: Vec<(u8, String)>,
        retry_at: Option<Instant>,
        backoff: Duration,
        /// Look again at this time for held-back input.
        wake_at: Option<Instant>,
        pub(crate) calls: u64,
    }

    impl LinkState {
        pub(crate) fn new(host: &str, port: Arc<dyn Port>, events: EventFn) -> Self {
            LinkState {
                host: host.to_string(),
                port,
                events,
                slots: BTreeMap::new(),
                closing: Vec::new(),
                retry_at: None,
                backoff: Duration::ZERO,
                wake_at: None,
                calls: 0,
            }
        }

        pub(crate) fn idle(&self) -> bool {
            self.slots.is_empty() && self.closing.is_empty()
        }

        pub(crate) fn on_cmd(&mut self, c: Cmd, now: Instant) {
            match c {
                Cmd::Open { slot, token, reply } => {
                    if let Some(s) = self.slots.get_mut(&slot) {
                        if s.token == token {
                            if s.opened {
                                let _ = reply.send(OpenResult::Ok);
                            } else {
                                s.waiters.push(reply);
                            }
                            return;
                        }
                        // a new owner for the slot: the old pad goes first
                        if let Some(old) = self.slots.remove(&slot) {
                            self.closing.push((slot, old.token));
                        }
                    }
                    let mut s = Slot::new(token, now);
                    s.waiters.push(reply);
                    self.slots.insert(slot, s);
                }
                Cmd::State { slot, state } => {
                    if let Some(s) = self.slots.get_mut(&slot) {
                        s.io.push(state);
                        s.last_input = now;
                        s.live = true;
                    }
                }
                Cmd::Neutral { slot } => {
                    if let Some(s) = self.slots.get_mut(&slot) {
                        s.io.push(PadState::NEUTRAL);
                        s.last_input = now;
                        s.live = false;
                    }
                }
                Cmd::Close { slot } => self.remove(slot),
                Cmd::Abandon { slot, token } => {
                    if self.slots.get(&slot).is_some_and(|s| s.token == token) {
                        self.remove(slot);
                    }
                }
            }
        }

        fn remove(&mut self, slot: u8) {
            if let Some(s) = self.slots.remove(&slot) {
                for w in s.waiters {
                    let _ = w.send(OpenResult::Failed("closed".into()));
                }
                self.closing.push((slot, s.token));
            }
        }

        fn ok(&mut self) {
            self.backoff = Duration::ZERO;
            self.retry_at = None;
        }

        /// The helper could not be reached: wait, then open every pad again (a no-op for a pad
        /// it still has; a restarted helper makes them anew).
        fn fail(&mut self, now: Instant) {
            self.backoff = (self.backoff * 2).clamp(BACKOFF_MIN, BACKOFF_MAX);
            self.retry_at = Some(now + self.backoff);
            for s in self.slots.values_mut() {
                s.opened = false;
            }
        }

        /// Does what is due: opens, closes, states, guest silence, heartbeats, rumble and light.
        pub(crate) fn tick(&mut self, now: Instant) {
            if self.retry_at.is_some_and(|t| t > now) {
                return;
            }
            self.wake_at = None;
            // Closes first (best effort): a slot let go and taken by a new guest in the same
            // round is free on the helper before the new pad is opened.
            for (slot, token) in std::mem::take(&mut self.closing) {
                self.calls += 1;
                let _ = self.port.close(&self.host, slot, &token);
            }
            // Opens.
            let to_open: Vec<(u8, String)> = self
                .slots
                .iter()
                .filter(|(_, s)| !s.opened)
                .map(|(k, s)| (*k, s.token.clone()))
                .collect();
            for (slot, token) in to_open {
                self.calls += 1;
                let r = self.port.open(&self.host, slot, &token);
                let Some(s) = self.slots.get_mut(&slot) else {
                    continue;
                };
                let first = !s.waiters.is_empty() || s.handle.is_none();
                match r {
                    Ok(Some(o)) if o.ok => {
                        // a new pad where we had one: the helper restarted (or dropped it)
                        let had = s.handle.is_some();
                        let reopened = had
                            && (o.resumed == Some(false)
                                || (o.resumed.is_none() && s.handle != Some(o.handle)));
                        s.opened = true;
                        s.refused_since = None;
                        s.handle = Some(o.handle);
                        s.last_call = now;
                        // every open (and resume) starts the helper's seq for the slot again
                        s.seq = 0;
                        if reopened {
                            // a new pad starts neutral
                            s.io.sent = PadState::NEUTRAL;
                        }
                        if reopened || (had && o.resumed == Some(true)) {
                            // It gets the latest at once: a new pad starts neutral, and a
                            // resumed one may have been let go by the watchdog in the gap.
                            s.io.force = true;
                        }
                        let waited = !s.waiters.is_empty();
                        let mut told = false;
                        for w in s.waiters.drain(..) {
                            told |= w.send(OpenResult::Ok).is_ok();
                        }
                        self.ok();
                        if waited && !told {
                            // made after its guest stopped waiting (a slow helper): nobody will
                            // play it, so it goes rather than being kept alive by heartbeats
                            self.remove(slot);
                            continue;
                        }
                        if reopened {
                            (self.events)(Event::Reopened { slot });
                        }
                    }
                    Ok(Some(o)) => {
                        let busy = o.err.as_deref() == Some(vpad::ERR_BUSY);
                        if !busy && !first {
                            // a reopen the helper refused for now: try again later, for a while
                            let since = *s.refused_since.get_or_insert(now);
                            if now.saturating_duration_since(since) < REOPEN_GIVE_UP {
                                self.fail(now);
                                return;
                            }
                        }
                        let res = if busy {
                            OpenResult::Busy
                        } else {
                            OpenResult::Failed(o.err.unwrap_or_else(|| "refused".into()))
                        };
                        self.ok();
                        self.drop_slot(slot, res, first);
                    }
                    Ok(None) => {
                        self.ok();
                        self.drop_slot(slot, OpenResult::OldHelper, first);
                    }
                    Err(e) => {
                        if first {
                            self.drop_slot(slot, OpenResult::Failed(format!("{e:#}")), true);
                        }
                        self.fail(now);
                        return;
                    }
                }
            }
            // Guest silence: the helper keeps a pad we heartbeat, so a guest that went quiet is
            // let go here.
            for s in self.slots.values_mut() {
                if s.live && now.duration_since(s.last_input) >= vpad::NEUTRAL_AFTER {
                    s.io.push(PadState::NEUTRAL);
                    s.live = false;
                }
            }
            // States: every slot with something to send, in one call.
            let mut items = Vec::new();
            let mut wake: Option<Instant> = None;
            for (slot, s) in self.slots.iter().filter(|(_, s)| s.opened) {
                let (send, later) = s.io.plan(now);
                if let Some(l) = later {
                    wake = Some(wake.map_or(l, |w: Instant| w.min(l)));
                }
                if let Some(st) = send {
                    items.push(StateItem {
                        slot: *slot,
                        token: s.token.clone(),
                        seq: s.seq.wrapping_add(1),
                        state: st,
                    });
                }
            }
            self.wake_at = wake;
            if !items.is_empty() {
                self.calls += 1;
                match self.port.states(&self.host, &items) {
                    Ok(Some(a)) if a.ok => {
                        self.ok();
                        for it in items {
                            let err = a.failed.iter().find(|f| f.slot == it.slot);
                            self.sent(it, err.map(|f| f.err.as_str()), now);
                        }
                    }
                    Ok(Some(a)) if a.err.as_deref() == Some(ERR_BAD_REQUEST) => {
                        // The helper could not read one item and refused the batch: that is
                        // no lost link (no backoff, no reopening, heartbeats go on). Each goes
                        // alone, so one guest's state cannot hold back the others'.
                        self.ok();
                        if !self.one_by_one(items, now) {
                            self.fail(now);
                            return;
                        }
                    }
                    Ok(Some(a)) if items.len() == 1 && slot_refusal(a.err.as_deref()) => {
                        // one state (`vpad.state`) refused for its pad alone
                        self.ok();
                        if let Some(it) = items.into_iter().next() {
                            self.sent(it, a.err.as_deref(), now);
                        }
                    }
                    Ok(Some(_)) | Ok(None) => {
                        // the helper does not know a slot (restarted): open them again, after
                        // a pause so a helper that keeps refusing is not asked in a loop
                        self.fail(now);
                        return;
                    }
                    Err(_) => {
                        self.fail(now);
                        return;
                    }
                }
            }
            // Heartbeats.
            let due: Vec<(u8, String, bool)> = self
                .slots
                .iter()
                .filter(|(_, s)| s.opened && now.duration_since(s.last_call) >= vpad::HEARTBEAT)
                .map(|(k, s)| (*k, s.token.clone(), s.live))
                .collect();
            for (slot, token, live) in due {
                self.calls += 1;
                match self.port.hb(&self.host, slot, &token, live) {
                    Ok(Some(a)) if a.ok => {
                        self.ok();
                        if let Some(s) = self.slots.get_mut(&slot) {
                            s.last_call = now;
                            if a.neutral == Some(true) {
                                // The helper let go after a gap (this thread was held up by
                                // another call or the network): what the guest still holds
                                // (a stick, R2) goes again, though it hasn't changed.
                                s.io.force = true;
                            }
                        }
                    }
                    Ok(Some(a)) if slot_refusal(a.err.as_deref()) => {
                        // this one pad (not the helper): open it again, the others go on
                        self.ok();
                        if let Some(s) = self.slots.get_mut(&slot) {
                            s.opened = false;
                            s.last_call = now;
                        }
                    }
                    Ok(_) | Err(_) => {
                        self.fail(now);
                        return;
                    }
                }
            }
            // Rumble and light bar.
            let due: Vec<u8> = self
                .slots
                .iter()
                .filter(|(_, s)| s.opened && s.out_due <= now)
                .map(|(k, _)| *k)
                .collect();
            for slot in due {
                self.calls += 1;
                let r = self.port.output(&self.host, slot);
                let Some(s) = self.slots.get_mut(&slot) else {
                    continue;
                };
                match r {
                    Ok(Some(o)) => {
                        if s.out_last != Some(o) {
                            s.out_last = Some(o);
                            s.out_fast_until = now + OUTPUT_FAST_FOR;
                            (self.events)(Event::Output { slot, out: o });
                        }
                        s.out_due = now
                            + if now < s.out_fast_until {
                                OUTPUT_FAST
                            } else {
                                OUTPUT_IDLE
                            };
                    }
                    Ok(None) => s.out_due = now + Duration::from_secs(5),
                    Err(_) => {
                        self.fail(now);
                        return;
                    }
                }
            }
        }

        /// The helper's answer for one state: `err` its refusal of that item (None: taken).
        fn sent(&mut self, it: StateItem, err: Option<&str>, now: Instant) {
            let Some(s) = self.slots.get_mut(&it.slot) else {
                return;
            };
            match err {
                None => {
                    s.seq = it.seq;
                    s.io.commit(it.state, now);
                    s.last_call = now;
                }
                Some(e) if e == ERR_BAD_REQUEST => {
                    // a state it cannot read is let go (the guest's next change goes) rather
                    // than sent again and again; the pad itself is fine
                    s.seq = it.seq;
                    s.io.commit(it.state, now);
                }
                Some(_) => {
                    // not open there (the helper restarted) or not ours: open it again (the
                    // state goes once it is)
                    s.opened = false;
                }
            }
        }

        /// A batch the helper refused: each state alone (one it still cannot read goes once more
        /// without its motion, then is let go). False when the helper could not be reached.
        fn one_by_one(&mut self, items: Vec<StateItem>, now: Instant) -> bool {
            for it in items {
                let mut it = it;
                let mut tries = 0;
                loop {
                    tries += 1;
                    self.calls += 1;
                    match self.port.states(&self.host, std::slice::from_ref(&it)) {
                        Ok(Some(a)) if a.ok => {
                            let err = a.failed.iter().find(|f| f.slot == it.slot);
                            let err = err.map(|f| f.err.clone());
                            self.sent(it, err.as_deref(), now);
                        }
                        Ok(Some(a)) if a.err.as_deref() == Some(ERR_BAD_REQUEST) => {
                            if tries == 1 && it.state.motion.is_some() {
                                it.state.motion = None;
                                continue;
                            }
                            self.sent(it, Some(ERR_BAD_REQUEST), now);
                        }
                        Ok(Some(a)) => {
                            let err = a.err.unwrap_or_else(|| "refused".into());
                            self.sent(it, Some(&err), now);
                        }
                        Ok(None) | Err(_) => return false,
                    }
                    break;
                }
            }
            true
        }

        fn drop_slot(&mut self, slot: u8, res: OpenResult, first: bool) {
            if let Some(s) = self.slots.remove(&slot) {
                for w in s.waiters {
                    let _ = w.send(res.clone());
                }
                if !first {
                    (self.events)(Event::Lost { slot });
                }
            }
        }

        /// How long the thread may sleep.
        pub(crate) fn wait(&self, now: Instant) -> Duration {
            if self.slots.is_empty() {
                return if self.closing.is_empty() {
                    Duration::from_secs(1)
                } else {
                    Duration::ZERO
                };
            }
            let mut at = now + Duration::from_millis(250);
            let mut min = |t: Instant| {
                if t < at {
                    at = t;
                }
            };
            if let Some(t) = self.retry_at {
                min(t);
            } else {
                if let Some(t) = self.wake_at {
                    min(t);
                }
                for s in self.slots.values() {
                    if !s.opened {
                        min(now);
                        continue;
                    }
                    min(s.last_call + vpad::HEARTBEAT);
                    min(s.out_due);
                    if s.live {
                        min(s.last_input + vpad::NEUTRAL_AFTER);
                    }
                }
            }
            at.saturating_duration_since(now)
        }
    }

    type LinkTx = (u64, mpsc::Sender<Cmd>);
    static LINKS: Mutex<Option<HashMap<String, LinkTx>>> = Mutex::new(None);

    fn spawn_link(host: &str) -> LinkTx {
        static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let gen = GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = mpsc::channel::<Cmd>();
        let h = host.to_string();
        std::thread::Builder::new()
            .name("ps5upload-vpad".into())
            .spawn(move || run_link(h, gen, rx))
            .ok();
        (gen, tx)
    }

    fn run_link(host: String, gen: u64, rx: mpsc::Receiver<Cmd>) {
        let key = host_key(&host);
        let console = host.clone();
        let events: EventFn = Arc::new(move |e| super::on_link_event(&console, e));
        let mut st = LinkState::new(&host, port_for(&host), events);
        let mut idle_since: Option<Instant> = None;
        loop {
            match rx.recv_timeout(st.wait(Instant::now())) {
                Ok(c) => {
                    st.on_cmd(c, Instant::now());
                    // What is already waiting goes in the same round (latest wins), but no more
                    // than CMDS_PER_ROUND: a busy channel never holds off the states and
                    // heartbeats of the others.
                    for _ in 1..CMDS_PER_ROUND {
                        match rx.try_recv() {
                            Ok(c) => st.on_cmd(c, Instant::now()),
                            Err(_) => break,
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            st.tick(Instant::now());
            if !st.idle() {
                idle_since = None;
                continue;
            }
            let since = *idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() > IDLE_END {
                if try_end(&mut st, &rx, &key, gen, Instant::now()) {
                    return;
                }
                idle_since = None;
                st.tick(Instant::now());
            }
        }
    }

    /// An idle link leaves [`LINKS`], unless a command came in at the last moment. All under the
    /// lock [`send`] sends under: what was sent is in the channel, and nothing can be sent after.
    /// Removed first and looked at afterwards, an open that came between them left a pad nobody
    /// heartbeats or closes, with the only sender gone and the next look ending this thread.
    pub(crate) fn try_end(
        st: &mut LinkState,
        rx: &mpsc::Receiver<Cmd>,
        key: &str,
        gen: u64,
        now: Instant,
    ) -> bool {
        let mut g = LINKS.lock().unwrap_or_else(|e| e.into_inner());
        while let Ok(c) = rx.try_recv() {
            st.on_cmd(c, now);
        }
        if !st.idle() {
            return false;
        }
        if let Some(map) = g.as_mut() {
            if map.get(key).is_some_and(|(g2, _)| *g2 == gen) {
                map.remove(key);
            }
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn test_put_link(key: &str, gen: u64, tx: mpsc::Sender<Cmd>) {
        let mut g = LINKS.lock().unwrap_or_else(|e| e.into_inner());
        g.get_or_insert_with(HashMap::new)
            .insert(key.to_string(), (gen, tx));
    }

    #[cfg(test)]
    pub(crate) fn test_has_link(key: &str) -> bool {
        let g = LINKS.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref().is_some_and(|m| m.contains_key(key))
    }

    /// Sends `cmd` to `console`'s link; an open starts one when none runs.
    pub(crate) fn send(console: &str, cmd: Cmd) {
        let key = host_key(console);
        let mut g = LINKS.lock().unwrap_or_else(|e| e.into_inner());
        let map = g.get_or_insert_with(HashMap::new);
        let cmd = match map.get(&key) {
            Some((_, tx)) => match tx.send(cmd) {
                Ok(()) => return,
                Err(mpsc::SendError(c)) => c, // that link ended: start another
            },
            None => cmd,
        };
        if !matches!(cmd, Cmd::Open { .. }) {
            return; // nothing open on a console without a link
        }
        // the link's thread talks to the console by the address the host gave
        let (gen, tx) = spawn_link(console);
        let _ = tx.send(cmd);
        map.insert(key, (gen, tx));
    }
}

#[cfg(test)]
mod tests {
    use super::link::{Cmd, Event, LinkState, OpenResult, Port, SlotIo, HOLD};
    use super::*;

    #[test]
    fn guest_keyframe_asks_are_spaced() {
        let t0 = Instant::now();
        let mut k = KeyAsk::default();
        assert!(k.take(t0));
        assert!(!k.take(t0 + Duration::from_millis(400)));
        assert!(!k.take(t0 + KEY_ASK_GAP - Duration::from_millis(1)));
        assert!(k.take(t0 + KEY_ASK_GAP));
        assert!(!k.take(t0 + KEY_ASK_GAP + Duration::from_millis(10)));
    }
    use ps5upload_core::vpad::{Ack, Opened, Output, SlotFailure, StateItem};
    use std::sync::atomic::{AtomicBool, AtomicI64};

    // ── pad frames ──

    fn frame(
        seq: u32,
        buttons: u32,
        touch: &[(u8, u16, u16)],
        motion: Option<[f32; 6]>,
    ) -> Vec<u8> {
        let mut b = vec![KIND_PAD];
        b.extend_from_slice(&seq.to_le_bytes());
        b.extend_from_slice(&buttons.to_le_bytes());
        b.extend_from_slice(&[10, 20, 30, 40, 50, 60]);
        b.push(touch.len() as u8);
        for (id, x, y) in touch {
            b.push(*id);
            b.extend_from_slice(&x.to_le_bytes());
            b.extend_from_slice(&y.to_le_bytes());
        }
        if let Some(m) = motion {
            b.push(PAD_MOTION);
            for f in m {
                b.extend_from_slice(&f.to_le_bytes());
            }
        }
        b
    }

    #[test]
    fn a_pad_frame_reads_as_the_guest_page_writes_it() {
        let app = vpad::app_button::PS | vpad::app_button::CROSS;
        let (seq, s) = parse_pad(&frame(u32::MAX - 1, app, &[], None)).unwrap();
        assert_eq!(seq, u32::MAX - 1, "a clock-based seq is fine");
        let pad = vpad::button::PS | vpad::button::CROSS | vpad::button::L2 | vpad::button::R2;
        assert_eq!(s.buttons, pad);
        assert_eq!(
            (s.lx, s.ly, s.rx, s.ry, s.l2, s.r2),
            (10, 20, 30, 40, 50, 60)
        );
        assert!(s.touch.is_empty() && s.motion.is_none());
        let (_, s) = parse_pad(&frame(
            1,
            0,
            &[(0, 100, 200), (1, 1900, 1000)],
            Some([0.5, 0.0, -1.0, 0.0, 1.0, 0.25]),
        ))
        .unwrap();
        assert_eq!(s.touch.len(), 2);
        assert_eq!(s.touch[1].x, 1900);
        assert_eq!(s.motion.unwrap().accel, [0.0, 1.0, 0.25]);
        // malformed: three fingers, short, trailing bytes, bad motion, wrong kind
        assert!(parse_pad(&frame(1, 0, &[(0, 0, 0); 3], None)).is_none());
        let f = frame(1, 0, &[(0, 1, 1)], None);
        assert!(parse_pad(&f[..f.len() - 1]).is_none());
        let mut f = frame(1, 0, &[], None);
        f.push(9);
        assert!(parse_pad(&f).is_none());
        assert!(parse_pad(&frame(1, 0, &[], Some([f32::NAN, 0., 0., 0., 0., 0.]))).is_none());
        assert!(parse_pad(&frame(1, 0, &[], Some([f32::INFINITY, 0., 0., 0., 0., 0.]))).is_none());
        let mut f = frame(1, 0, &[], None);
        f[0] = 8;
        assert!(parse_pad(&f).is_none());
    }

    #[test]
    fn out_of_range_motion_is_held_to_a_real_pads_or_dropped_never_the_whole_frame() {
        let cross = vpad::app_button::CROSS;
        // beyond what a DualSense reads: held to its range
        let (_, s) = parse_pad(&frame(
            1,
            cross,
            &[],
            Some([500.0, -500.0, 1.0, 99.0, -99.0, 0.5]),
        ))
        .unwrap();
        let m = s.motion.unwrap();
        assert_eq!(m.gyro, [GYRO_MAX, -GYRO_MAX, 1.0]);
        assert_eq!(m.accel, [ACCEL_MAX, -ACCEL_MAX, 0.5]);
        // no reading at all (the helper refuses beyond 1e6): the motion goes, the press stays
        for bad in [1e6_f32, -2e5, 3.4e38] {
            let (_, s) = parse_pad(&frame(
                2,
                cross,
                &[(0, 5, 6)],
                Some([0.0, 0.0, 0.0, 0.0, bad, 0.0]),
            ))
            .unwrap();
            assert!(s.motion.is_none(), "{bad}");
            assert_eq!(s.buttons & vpad::button::CROSS, vpad::button::CROSS);
            assert_eq!(s.touch.len(), 1);
        }
        // everything that reaches the helper is well inside its own limit
        for x in [-1e5_f32, 1e5, 12345.0] {
            let (_, s) = parse_pad(&frame(3, 0, &[], Some([x; 6]))).unwrap();
            let m = s.motion.unwrap();
            assert!(m.gyro.iter().chain(m.accel.iter()).all(|v| v.abs() <= 40.0));
        }
    }

    // ── short codes ──

    #[test]
    fn short_codes_are_unambiguous_and_typed_any_way() {
        for _ in 0..50 {
            let c = new_code().unwrap();
            assert_eq!(c.len(), CODE_LEN);
            assert!(c.bytes().all(|b| CODE_ALPHABET.contains(&b)), "{c}");
            assert!(!c.contains(['I', 'L', 'O', '0', '1']));
        }
        assert_eq!(normalize_code("k7m-2qx"), "K7M2QX");
        assert_eq!(normalize_code(" K7M 2QX "), "K7M2QX");
        assert_eq!(code_display("K7M2QX"), "K7M-2QX");
    }

    #[test]
    fn wrong_codes_are_limited_per_address_but_not_this_computer() {
        let ip: IpAddr = "192.0.2.77".parse().unwrap();
        let t = Instant::now();
        for _ in 0..WRONG_CODES_PER_MIN {
            assert!(code_tries_left(ip, t));
            code_wrong(ip, t);
        }
        assert!(!code_tries_left(ip, t));
        assert!(code_tries_left("192.0.2.78".parse().unwrap(), t));
        assert!(code_tries_left(ip, t + Duration::from_secs(61)));
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        for _ in 0..20 {
            code_wrong(lo, t);
        }
        assert!(code_tries_left(lo, t));
    }

    #[test]
    fn connects_are_limited_per_address() {
        // Its own table: `allow` prunes every address by the time it is given, so another test
        // asking at t + 61 s would empty a shared one under this test.
        let table = ConnectLimit::new();
        let ip: IpAddr = "192.0.2.90".parse().unwrap();
        let t = Instant::now();
        for _ in 0..CONNECTS_PER_MIN {
            assert!(connect_allowed_in(&table, ip, t));
        }
        assert!(!connect_allowed_in(&table, ip, t));
        assert!(connect_allowed_in(&table, ip, t + Duration::from_secs(61)));
    }

    /// A flaky guest comes back every few seconds: those tries must not lock them out of the
    /// place the engine is keeping for them.
    #[test]
    fn an_address_with_a_place_may_come_back_often() {
        // Its own table: `allow` prunes every address by the time it is given, so another test
        // asking at t + 61 s would empty a shared one under this test.
        let table = ConnectLimit::new();
        let ip: IpAddr = "192.0.2.91".parse().unwrap();
        let t = Instant::now();
        let mut p = party();
        let id = admit(&mut p, "Ann", t);
        p.guest_mut(id).unwrap().ip = Some(ip);
        let key = "192.0.2.200-place".to_string();
        with_parties(|m| m.insert(key.clone(), p));
        for _ in 0..(CONNECTS_PER_MIN * 2) {
            assert!(connect_allowed_in(&table, ip, t));
        }
        // Their place gone: the usual limit.
        with_parties(|m| m.remove(&key));
        assert!(!connect_allowed_in(&table, ip, t));
    }

    #[test]
    fn this_computer_may_connect_more_often_but_not_without_end() {
        // Its own table: `allow` prunes every address by the time it is given, so another test
        // asking at t + 61 s would empty a shared one under this test.
        let table = ConnectLimit::new();
        let ip: IpAddr = "127.0.0.2".parse().unwrap();
        let t = Instant::now();
        for _ in 0..LOOPBACK_CONNECTS_PER_MIN {
            assert!(connect_allowed_in(&table, ip, t));
        }
        assert!(!connect_allowed_in(&table, ip, t));
        assert!(connect_allowed_in(&table, ip, t + Duration::from_secs(61)));
        const { assert!(LOOPBACK_CONNECTS_PER_MIN > CONNECTS_PER_MIN) };
    }

    #[test]
    fn a_flood_of_pad_frames_is_held_to_the_rate_and_the_latest_is_kept() {
        let t = Instant::now();
        let mut p = PadPacer::new(t);
        // a burst goes at once, the rest is folded into the latest
        let sent: Vec<u32> = (0..1000).filter_map(|i| p.offer(t, i)).collect();
        assert_eq!(sent.len(), PAD_BURST as usize);
        let due = p.due().unwrap();
        assert!(
            due > t && due <= t + Duration::from_millis(5),
            "{:?}",
            due - t
        );
        assert_eq!(p.take(t), None, "not before it is due");
        assert_eq!(p.take(due), Some(999), "the latest, never an older one");
        assert_eq!(p.due(), None);
        // a second of flooding at 10 000 a second passes about PAD_RATE of them
        let mut n = 0;
        for i in 1..=10_000u32 {
            let now = t + Duration::from_micros(100 * i as u64);
            if p.offer(now, i).is_some() {
                n += 1;
            }
            if p.due().is_some_and(|d| d <= now) && p.take(now).is_some() {
                n += 1;
            }
        }
        assert!((240..=260).contains(&n), "{n}");
        // a normal pad (120 a second) is never held
        let mut p = PadPacer::new(t);
        for i in 0..1200u32 {
            let now = t + Duration::from_micros(8333 * i as u64);
            assert_eq!(p.offer(now, i), Some(i));
        }
    }

    #[test]
    fn the_host_api_is_this_computer_by_name_or_an_admitted_peer() {
        let h = |v: &str| {
            let mut m = HeaderMap::new();
            m.insert("host", v.parse().unwrap());
            m
        };
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        assert!(host_allowed(lo, &h("127.0.0.1:19113")));
        assert!(host_allowed(lo, &h("localhost:19113")));
        // a re-pointed name reaching loopback (DNS rebinding) is refused
        assert!(!host_allowed(lo, &h("evil.example:19113")));
        // a peer loopback_guard admitted (PS5UPLOAD_ALLOW_IP: the web UI on a NAS)
        assert!(host_allowed(lan, &h("nas.local:19113")));
        // and its name is where guests are sent first
        let (port, web) = reach_from(&h("nas.local:19113"));
        assert_eq!(
            (port, web.as_deref()),
            (19113, Some("http://nas.local:19113"))
        );
        assert_eq!(reach_from(&h("127.0.0.1:19113")).1, None);
        assert_eq!(reach_from(&h("localhost:19113")).1, None);
        assert_eq!(reach_from(&h("bad host/x")).1, None);
    }

    // ── the party ──

    fn party() -> Party {
        Party::new("10.0.0.5", 1, "Desk".into(), Settings::default())
    }

    fn admit(p: &mut Party, name: &str, now: Instant) -> u64 {
        let player = p.free_player(&[]).unwrap();
        let id = p.admit(name, player, now).unwrap().id;
        let g = p.guest_mut(id).unwrap();
        g.slot_open = true;
        id
    }

    fn conn() -> (mpsc::Sender<Out>, mpsc::Receiver<Out>) {
        mpsc::channel(16)
    }

    fn links(fx: &[Effect]) -> Vec<String> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::Link(Cmd::Close { slot }) => Some(format!("close{slot}")),
                Effect::Link(Cmd::Neutral { slot }) => Some(format!("neutral{slot}")),
                Effect::Link(_) => Some("other".into()),
                _ => None,
            })
            .collect()
    }

    fn byes(fx: Vec<Effect>) -> Vec<&'static str> {
        fx.into_iter()
            .filter_map(|e| match e {
                Effect::Say(_, Out::Bye(w)) => Some(w),
                _ => None,
            })
            .collect()
    }

    /// A bug report gets counts, stages and health, never the invite key or code, a guest's
    /// resume or pad token, or a guest's name.
    #[test]
    fn the_bug_report_summary_has_no_keys_tokens_or_names() {
        let mut p = party();
        let t = Instant::now();
        let inv = p.invite(false, t).unwrap();
        let id = admit(&mut p, "Zelda Friend", t);
        p.guests[0].stats.rtt(42);
        p.guests[0].stats.skipped.fetch_add(3, Ordering::Relaxed);
        let (token, pad_token) = {
            let g = p.guest_mut(id).unwrap();
            (g.token.clone(), g.pad_token.clone())
        };
        let v = p.diag(t);
        let text = v.to_string();
        for secret in [
            &inv.key,
            &inv.code,
            &code_display(&inv.code),
            &token,
            &pad_token,
        ] {
            assert!(
                !text.contains(secret.as_str()),
                "a key, code or token is in the summary"
            );
        }
        assert!(!text.contains("Zelda"), "a guest's name in {text}");
        assert!(!text.contains("Desk"), "the host's name in {text}");
        assert_eq!(v["invite_open"], true);
        assert_eq!(v["players_in"], 1);
        assert_eq!(v["guests"][0]["state"], "reconnecting");
        assert_eq!(v["guests"][0]["rtt_ms"], 42);
        assert_eq!(v["guests"][0]["skipped_pictures"], 3);
    }

    #[test]
    fn players_are_the_lowest_free_numbers_up_to_the_cap() {
        let mut p = party();
        let t = Instant::now();
        let a = admit(&mut p, "A", t);
        let b = admit(&mut p, "B", t);
        assert_eq!(p.guest_mut(a).unwrap().player, 2);
        assert_eq!(p.guest_mut(b).unwrap().player, 3);
        assert_eq!(p.free_player(&[]), Some(4));
        assert_eq!(p.free_player(&[4]), None, "a busy slot is skipped");
        p.leave(a, "left", t);
        assert_eq!(
            p.free_player(&[]),
            Some(2),
            "a guest who left frees their player"
        );
        p.settings.max_players = 2;
        assert_eq!(p.free_player(&[]), Some(2));
        admit(&mut p, "C", t);
        assert_eq!(p.free_player(&[]), None, "max_players caps the guests");
        assert_eq!(p.guests_in(), 2);
    }

    #[test]
    fn invites_expire_and_renew_and_a_code_works_while_the_invite_does() {
        let mut p = party();
        let t = Instant::now();
        let i = p.invite(false, t).unwrap();
        assert_eq!(i.key.len(), 32);
        assert!(p.key_ok(&i.key, t));
        assert!(p.code_ok(&i.code, t));
        assert!(!p.key_ok("00", t));
        assert_eq!(
            p.invite(false, t).unwrap().key,
            i.key,
            "the same link again"
        );
        let j = p.invite(true, t).unwrap();
        assert_ne!(j.key, i.key);
        assert!(!p.key_ok(&i.key, t), "a new invite voids the old link");
        assert!(!p.code_ok(&i.code, t) || i.code == j.code);
        let later = t + INVITE_TTL;
        assert!(!p.key_ok(&j.key, later) && !p.code_ok(&j.code, later));
        p.tick(later, Some(1));
        assert!(p.invite.is_none());
    }

    #[test]
    fn a_dropped_guest_keeps_their_player_for_the_window_then_is_let_go() {
        let mut p = party();
        let t = Instant::now();
        let id = admit(&mut p, "A", t);
        let token = p.guest_mut(id).unwrap().token.clone();
        let (tx, _rx) = conn();
        assert!(p.attach(id, 1, tx).is_empty());
        let fx = p.detach(id, 1, t);
        assert_eq!(links(&fx), vec!["neutral2"], "their pad lets go at once");
        assert_eq!(p.guest_mut(id).unwrap().state(), "reconnecting");
        assert_eq!(p.by_token(&token), Some(id), "the token still works");
        // the helper slot is kept (the link keeps heart-beating it): no close yet
        let (fx, keep) = p.tick(t + RESUME_WINDOW - Duration::from_secs(1), Some(1));
        assert!(keep && links(&fx).is_empty());
        let (fx, _) = p.tick(t + RESUME_WINDOW, Some(1));
        assert_eq!(links(&fx), vec!["close2"]);
        assert_eq!(
            p.by_token(&token),
            None,
            "the window passed: the token is void"
        );
        assert!(fx
            .iter()
            .any(|e| matches!(e, Effect::Log(l) if l.contains("left (timeout)"))));
    }

    #[test]
    fn a_second_socket_replaces_the_first_and_a_stale_close_changes_nothing() {
        let mut p = party();
        let t = Instant::now();
        let id = admit(&mut p, "A", t);
        let (a, _ra) = conn();
        let (b, _rb) = conn();
        p.attach(id, 1, a);
        assert_eq!(byes(p.attach(id, 2, b)), vec!["replaced"]);
        // the first socket's close is not the guest leaving
        assert!(p.detach(id, 1, t).is_empty());
        assert_eq!(p.guest_mut(id).unwrap().state(), "picking_user");
    }

    #[test]
    fn a_kick_tells_them_removes_the_pad_and_voids_the_token() {
        let mut p = party();
        let t = Instant::now();
        let id = admit(&mut p, "A", t);
        let token = p.guest_mut(id).unwrap().token.clone();
        let (tx, _rx) = conn();
        p.attach(id, 1, tx);
        let fx = p.leave(id, "kicked", t);
        assert_eq!(links(&fx), vec!["close2"]);
        assert_eq!(byes(fx), vec!["kicked"]);
        assert_eq!(p.by_token(&token), None);
        assert_eq!(p.guest_mut(id).unwrap().state(), "left");
        // shown a while, then gone
        p.tick(t + LEFT_SHOWN, Some(1));
        assert!(p.guests.is_empty());
    }

    #[test]
    fn ps_and_create_are_blocked_once_playing_not_while_picking_a_user() {
        let mut p = party();
        // On by default: a friend's PS could quit the game or update the console.
        assert!(p.settings.block_system_buttons);
        let t = Instant::now();
        let id = admit(&mut p, "A", t);
        let (tx, _rx) = conn();
        p.attach(id, 1, tx);
        assert_eq!(p.strip_mask(id), Some((2, 0)), "PS answers 'Who's using'");
        p.tick(t + PICKING_GRACE, Some(1));
        assert_eq!(p.strip_mask(id), Some((2, vpad::button::SYSTEM)));
        p.settings.block_system_buttons = false;
        assert_eq!(p.strip_mask(id), Some((2, 0)));
        p.settings.block_system_buttons = true;
        // a new pad (helper restart) asks who they are again
        p.reopened(2, t + PICKING_GRACE);
        assert_eq!(p.strip_mask(id), Some((2, 0)));
        p.leave(id, "left", t);
        assert_eq!(p.strip_mask(id), None);
    }

    #[test]
    fn the_host_stopping_tells_everyone_then_closes_unless_they_play_again() {
        let mut p = party();
        let t = Instant::now();
        let id = admit(&mut p, "A", t);
        let (tx, _rx) = conn();
        p.attach(id, 1, tx);
        let (atx, mut arx) = oneshot::channel();
        p.pending.push(Ask {
            id: 99,
            name: "B".into(),
            answer: atx,
        });
        let (fx, keep) = p.tick(t, None);
        assert!(keep);
        assert_eq!(links(&fx), vec!["neutral2"]);
        assert_eq!(byes(fx), vec!["host_stopped"]);
        assert_eq!(arx.try_recv().unwrap(), Decision::HostStopped);
        assert_eq!(p.guests_in(), 1, "still counted: they may come back");
        // the pads go after the grace
        let (fx, _) = p.tick(t + HOST_GRACE, None);
        assert_eq!(links(&fx), vec!["close2"]);
        // the guest is not timed out while the host is away (only the whole party is)
        let (_, keep) = p.tick(t + RESUME_WINDOW - Duration::from_millis(1), None);
        assert!(keep && p.guests_in() == 1);
        // the host plays again: a new session, the guest may come back
        let (_, keep) = p.tick(t + Duration::from_secs(50), Some(2));
        assert!(keep);
        assert_eq!(p.session, 2);
        assert!(p.host_gone.is_none());
        assert!(
            !p.guest_mut(id).unwrap().slot_open,
            "their pad is made again on resume"
        );
        // and if the host never comes back: closed
        let mut q = party();
        q.tick(t, None);
        let (_, keep) = q.tick(t + HOST_RESTART_WINDOW, None);
        assert!(!keep);
    }

    #[test]
    fn the_first_guest_takes_the_stream_controller_only_when_it_is_free() {
        let now = Instant::now();
        let mut p = party();
        assert_eq!(
            p.next_player(&[], false),
            Some(2),
            "the host's page uses it"
        );
        assert_eq!(p.next_player(&[], true), Some(STREAM_SEAT));
        p.admit("a", STREAM_SEAT, now);
        assert_eq!(p.next_player(&[], true), Some(2), "one guest has it");
        p.admit("b", 2, now);
        assert_eq!(p.next_player(&[], true), Some(3));
        p.admit("c", 3, now);
        assert_eq!(p.next_player(&[], true), None, "no more than max_players");
    }

    #[test]
    fn a_guest_on_the_stream_controller_lets_go_when_quiet_or_gone() {
        let t = Instant::now();
        let mut p = party();
        let id = p.admit("a", STREAM_SEAT, t).unwrap().id;
        let session = p.session;
        let seat_pads = |fx: &[Effect]| {
            fx.iter()
                .filter(|e| matches!(e, Effect::SeatPad(None)))
                .count()
        };
        p.guest_mut(id).unwrap().seat_input_at = Some(t);
        let (fx, _) = p.tick(t + Duration::from_millis(100), Some(session));
        assert_eq!(seat_pads(&fx), 0, "still fresh");
        assert!(fx.iter().any(|e| matches!(e, Effect::Seat(true))));
        let (fx, _) = p.tick(t + vpad::NEUTRAL_AFTER, Some(session));
        assert_eq!(seat_pads(&fx), 1, "quiet: nothing stays held");
        let (fx, _) = p.tick(t + vpad::NEUTRAL_AFTER * 2, Some(session));
        assert_eq!(seat_pads(&fx), 0, "and only once");
        let fx = p.leave(id, "left", t);
        assert_eq!(seat_pads(&fx), 1);
        assert!(fx.iter().any(|e| matches!(e, Effect::Seat(false))));
        assert!(
            !fx.iter().any(|e| matches!(e, Effect::Link(_))),
            "no helper pad was ever made for it"
        );
    }

    #[test]
    fn a_guest_state_maps_onto_the_stream_controller() {
        use ps5upload_core::rp_stream::button as rp;
        let st = PadState {
            buttons: vpad::button::CROSS | vpad::button::CREATE | vpad::button::L2,
            lx: 0,
            ly: 128,
            rx: 255,
            ry: 128,
            l2: 200,
            r2: 0,
            touch: vec![Touch {
                id: 0,
                x: 1919,
                y: 1079,
            }],
            motion: None,
        };
        let c = controller_state(&st);
        assert_eq!(
            c.buttons,
            rp::CROSS | rp::SHARE,
            "L2 is the trigger's value here"
        );
        assert_eq!(c.l2, 200);
        assert_eq!((c.lx, c.ly, c.rx), (-32768, 0, 32767));
        assert_eq!(c.touch, Some([1919, 941]));
    }

    #[test]
    fn a_session_swapped_between_looks_sends_guests_to_the_new_picture() {
        let mut p = party();
        let t = Instant::now();
        let id = admit(&mut p, "A", t);
        let (tx, _rx) = conn();
        p.attach(id, 1, tx);
        let (fx, keep) = p.tick(t, Some(5));
        assert!(keep);
        assert_eq!(byes(fx), vec!["host_stopped"]);
        assert_eq!(p.session, 5);
    }

    #[test]
    fn closing_answers_every_ask_and_lets_everyone_go() {
        let mut p = party();
        let t = Instant::now();
        let id = admit(&mut p, "A", t);
        let (tx, _rx) = conn();
        p.attach(id, 1, tx);
        let (atx, mut arx) = oneshot::channel();
        p.pending.push(Ask {
            id: 1,
            name: "B".into(),
            answer: atx,
        });
        p.invite(false, t);
        let fx = p.close(t);
        assert_eq!(links(&fx), vec!["close2"]);
        assert_eq!(byes(fx), vec!["closed"]);
        assert_eq!(arx.try_recv().unwrap(), Decision::Closed);
        assert!(p.invite.is_none());
    }

    // ── coalescing ──

    fn ps(buttons: u32, lx: u8) -> PadState {
        PadState {
            buttons,
            lx,
            ..PadState::NEUTRAL
        }
    }

    #[test]
    fn latest_wins_for_sticks_but_a_quick_tap_is_never_lost() {
        let t = Instant::now();
        let mut io = SlotIo::default();
        // sticks: only the latest goes
        io.push(ps(0, 10));
        io.push(ps(0, 20));
        let (send, _) = io.plan(t);
        assert_eq!(send, Some(ps(0, 20)));
        io.commit(ps(0, 20), t);
        // a press and release between two sends: the press goes first...
        io.push(ps(0x20, 20));
        io.push(ps(0, 30));
        let (send, later) = io.plan(t);
        assert_eq!(
            send,
            Some(ps(0x20, 30)),
            "the down state, with the latest sticks"
        );
        assert!(later.is_some());
        io.commit(ps(0x20, 30), t);
        // ...and is held at least HOLD before the release
        let (send, later) = io.plan(t + Duration::from_millis(5));
        assert_eq!(send, None);
        assert_eq!(later, Some(t + HOLD));
        // sticks still go during the hold, with the button kept down
        io.push(ps(0, 40));
        let (send, _) = io.plan(t + Duration::from_millis(6));
        assert_eq!(send, Some(ps(0x20, 40)));
        io.commit(ps(0x20, 40), t + Duration::from_millis(6));
        let (send, _) = io.plan(t + HOLD);
        assert_eq!(send, Some(ps(0, 40)), "then the release");
        io.commit(ps(0, 40), t + HOLD);
        assert_eq!(io.plan(t + HOLD).0, None);
    }

    #[test]
    fn a_release_and_press_again_between_sends_is_sent_released_first() {
        let t = Instant::now();
        let mut io = SlotIo::default();
        io.push(ps(0x40, 128));
        io.commit(ps(0x40, 128), t);
        io.push(ps(0, 128));
        io.push(ps(0x40, 128));
        let later = t + Duration::from_millis(100);
        assert_eq!(io.plan(later).0, Some(ps(0, 128)));
        io.commit(ps(0, 128), later);
        assert_eq!(io.plan(later).0, None, "held released");
        assert_eq!(io.plan(later + HOLD).0, Some(ps(0x40, 128)));
    }

    // ── the helper link ──

    #[derive(Default)]
    struct FakeHelper {
        log: Mutex<Vec<String>>,
        states: Mutex<Vec<StateItem>>,
        handle: AtomicI64,
        /// The next open is a new pad (a restarted helper).
        restarted: AtomicBool,
        down: AtomicBool,
        old: AtomicBool,
        busy: Mutex<Vec<u8>>,
        /// states answer this slot as not open
        not_open: Mutex<Option<u8>>,
        /// states refuse a whole batch holding this slot (an item it cannot read)
        unreadable: Mutex<Option<u8>>,
        /// opens are refused with this (the helper says no, for a pad it had)
        refuse_open: Mutex<Option<String>>,
        /// opens take this long (a slow helper)
        open_delay: Mutex<Duration>,
        out: Mutex<Output>,
        /// the next heartbeat says the watchdog let the pad go
        rested: AtomicBool,
    }

    impl FakeHelper {
        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
        fn states(&self) -> Vec<StateItem> {
            self.states.lock().unwrap().clone()
        }
        fn up(&self) -> Result<(), anyhow::Error> {
            if self.down.load(Ordering::Relaxed) {
                anyhow::bail!("connection refused")
            }
            Ok(())
        }
    }

    impl Port for FakeHelper {
        fn open(&self, _h: &str, slot: u8, _t: &str) -> anyhow::Result<Option<Opened>> {
            self.up()?;
            let delay = *self.open_delay.lock().unwrap();
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            self.log.lock().unwrap().push(format!("open{slot}"));
            if self.old.load(Ordering::Relaxed) {
                return Ok(None);
            }
            if self.busy.lock().unwrap().contains(&slot) {
                return Ok(Some(Opened {
                    err: Some("vpad_busy".into()),
                    ..Default::default()
                }));
            }
            if let Some(e) = self.refuse_open.lock().unwrap().clone() {
                return Ok(Some(Opened {
                    err: Some(e),
                    ..Default::default()
                }));
            }
            *self.not_open.lock().unwrap() = None;
            // the first open, or one after a restart, makes a new pad
            let fresh = self.restarted.swap(false, Ordering::Relaxed)
                || self.handle.load(Ordering::Relaxed) == 0;
            if fresh {
                self.handle.fetch_add(1, Ordering::Relaxed);
            }
            Ok(Some(Opened {
                ok: true,
                slot,
                handle: self.handle.load(Ordering::Relaxed),
                resumed: Some(!fresh),
                ..Default::default()
            }))
        }
        fn states(&self, _h: &str, items: &[StateItem]) -> anyhow::Result<Option<Ack>> {
            self.up()?;
            self.log
                .lock()
                .unwrap()
                .push(format!("states{}", items.len()));
            if let Some(u) = *self.unreadable.lock().unwrap() {
                if items
                    .iter()
                    .any(|it| it.slot == u && it.state.motion.is_some())
                {
                    return Ok(Some(Ack {
                        ok: false,
                        err: Some("bad_request".into()),
                        ..Default::default()
                    }));
                }
            }
            let bad = *self.not_open.lock().unwrap();
            let mut failed = Vec::new();
            for it in items {
                if Some(it.slot) == bad {
                    failed.push(SlotFailure {
                        slot: it.slot,
                        err: "vpad_not_open".into(),
                    });
                } else {
                    self.states.lock().unwrap().push(it.clone());
                }
            }
            Ok(Some(Ack {
                ok: true,
                failed,
                ..Default::default()
            }))
        }
        fn hb(&self, _h: &str, slot: u8, _t: &str, live: bool) -> anyhow::Result<Option<Ack>> {
            self.up()?;
            self.log
                .lock()
                .unwrap()
                .push(format!("hb{slot}{}", if live { "live" } else { "" }));
            Ok(Some(Ack {
                ok: true,
                neutral: Some(self.rested.swap(false, Ordering::Relaxed)),
                ..Default::default()
            }))
        }
        fn close(&self, _h: &str, slot: u8, _t: &str) -> anyhow::Result<Option<Ack>> {
            self.up()?;
            self.log.lock().unwrap().push(format!("close{slot}"));
            Ok(Some(Ack {
                ok: true,
                ..Default::default()
            }))
        }
        fn output(&self, _h: &str, _slot: u8) -> anyhow::Result<Option<Output>> {
            self.up()?;
            Ok(Some(*self.out.lock().unwrap()))
        }
        fn status(&self, _h: &str) -> anyhow::Result<Option<serde_json::Value>> {
            self.up()?;
            if self.old.load(Ordering::Relaxed) {
                return Ok(None);
            }
            Ok(Some(serde_json::json!({"slots": []})))
        }
    }

    type Events = Arc<Mutex<Vec<Event>>>;

    fn reopens(ev: &Events) -> usize {
        ev.lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, Event::Reopened { .. }))
            .count()
    }

    fn link_with(f: &Arc<FakeHelper>) -> (LinkState, Events) {
        let ev: Events = Arc::default();
        let e2 = ev.clone();
        let st = LinkState::new(
            "c",
            f.clone(),
            Arc::new(move |e| e2.lock().unwrap().push(e)),
        );
        (st, ev)
    }

    fn open_slot_t(
        st: &mut LinkState,
        slot: u8,
        token: &str,
        t: Instant,
    ) -> oneshot::Receiver<OpenResult> {
        let (tx, rx) = oneshot::channel();
        st.on_cmd(
            Cmd::Open {
                slot,
                token: token.into(),
                reply: tx,
            },
            t,
        );
        st.tick(t);
        rx
    }

    #[test]
    fn a_pad_opens_states_go_in_one_call_and_heartbeats_say_whether_the_guest_is_there() {
        let f = Arc::new(FakeHelper::default());
        let (mut st, _ev) = link_with(&f);
        let t = Instant::now();
        assert_eq!(
            open_slot_t(&mut st, 2, "a", t).try_recv().unwrap(),
            OpenResult::Ok
        );
        assert_eq!(
            open_slot_t(&mut st, 3, "b", t).try_recv().unwrap(),
            OpenResult::Ok
        );
        // two guests' states in one call, each with its own seq from 1 after the open
        st.on_cmd(
            Cmd::State {
                slot: 2,
                state: st_(0x20),
            },
            t,
        );
        st.on_cmd(
            Cmd::State {
                slot: 3,
                state: st_(0x40),
            },
            t,
        );
        st.tick(t);
        let s = f.states();
        assert!(f.log().contains(&"states2".to_string()));
        assert_eq!((s[0].slot, s[0].seq, s[0].state.buttons), (2, 1, 0x20));
        assert_eq!((s[1].slot, s[1].seq), (3, 1));
        // nothing new: a live heartbeat after 200 ms keeps a held button held
        let t2 = t + vpad::HEARTBEAT;
        st.on_cmd(
            Cmd::State {
                slot: 2,
                state: st_(0x20),
            },
            t2,
        );
        st.on_cmd(Cmd::Neutral { slot: 3 }, t2);
        st.tick(t2);
        let log = f.log();
        assert!(log.contains(&"hb2live".to_string()), "{log:?}");
        // the guest of 3 left: neutral, then plain heartbeats keep the pad (and their user)
        assert_eq!(f.states().last().unwrap().state, PadState::NEUTRAL);
        let t3 = t2 + vpad::HEARTBEAT;
        st.on_cmd(
            Cmd::State {
                slot: 2,
                state: st_(0x20),
            },
            t3,
        );
        st.tick(t3);
        assert!(f.log().contains(&"hb3".to_string()), "{:?}", f.log());
    }

    fn st_(buttons: u32) -> PadState {
        PadState {
            buttons,
            ..PadState::NEUTRAL
        }
    }

    #[test]
    fn a_held_stick_goes_again_after_the_helper_let_the_pad_go() {
        let f = Arc::new(FakeHelper::default());
        let (mut st, _ev) = link_with(&f);
        let t = Instant::now();
        open_slot_t(&mut st, 2, "a", t);
        let held = ps(0, 255);
        st.on_cmd(
            Cmd::State {
                slot: 2,
                state: held.clone(),
            },
            t,
        );
        st.tick(t);
        let sent = |f: &FakeHelper| f.states().iter().filter(|i| i.state == held).count();
        assert_eq!(sent(&f), 1);
        // the link was held up past 400 ms: the helper rested the pad and says so
        f.rested.store(true, Ordering::Relaxed);
        let t2 = t + vpad::HEARTBEAT;
        st.on_cmd(
            Cmd::State {
                slot: 2,
                state: held.clone(),
            },
            t2,
        );
        st.tick(t2);
        assert_eq!(sent(&f), 1, "nothing new yet: a heartbeat");
        let t3 = t2 + Duration::from_millis(20);
        st.on_cmd(
            Cmd::State {
                slot: 2,
                state: held.clone(),
            },
            t3,
        );
        st.tick(t3);
        assert_eq!(sent(&f), 2, "the unchanged stick goes again");
    }

    #[test]
    fn a_quiet_guest_is_let_go_after_400_ms() {
        let f = Arc::new(FakeHelper::default());
        let (mut st, _ev) = link_with(&f);
        let t = Instant::now();
        open_slot_t(&mut st, 2, "a", t);
        st.on_cmd(
            Cmd::State {
                slot: 2,
                state: ps(0, 255),
            },
            t,
        );
        st.tick(t);
        st.tick(t + vpad::NEUTRAL_AFTER);
        assert_eq!(f.states().last().unwrap().state, PadState::NEUTRAL);
        assert!(st.wait(t + vpad::NEUTRAL_AFTER) <= vpad::HEARTBEAT);
    }

    #[test]
    fn a_tap_between_two_sends_reaches_the_helper_as_a_press_then_a_release() {
        let f = Arc::new(FakeHelper::default());
        let (mut st, _ev) = link_with(&f);
        let t = Instant::now();
        open_slot_t(&mut st, 2, "a", t);
        st.tick(t);
        let n = f.states().len();
        // down and up while a call was in flight: both arrive in one batch
        st.on_cmd(
            Cmd::State {
                slot: 2,
                state: st_(0x20),
            },
            t,
        );
        st.on_cmd(
            Cmd::State {
                slot: 2,
                state: st_(0),
            },
            t,
        );
        st.tick(t);
        assert!(st.wait(t) <= HOLD);
        st.tick(t + HOLD);
        let got: Vec<u32> = f.states()[n..].iter().map(|s| s.state.buttons).collect();
        assert_eq!(got, vec![0x20, 0]);
        let seqs: Vec<u32> = f.states()[n..].iter().map(|s| s.seq).collect();
        assert!(seqs[1] > seqs[0]);
    }

    #[test]
    fn a_restarted_helper_gets_the_pad_again_and_the_guest_is_asked_who_they_are() {
        let f = Arc::new(FakeHelper::default());
        let (mut st, ev) = link_with(&f);
        let t = Instant::now();
        open_slot_t(&mut st, 2, "a", t);
        st.on_cmd(
            Cmd::State {
                slot: 2,
                state: st_(0x20),
            },
            t,
        );
        st.tick(t);
        // the helper restarted: it does not know slot 2
        *f.not_open.lock().unwrap() = Some(2);
        st.on_cmd(
            Cmd::State {
                slot: 2,
                state: st_(0x40),
            },
            t,
        );
        st.tick(t);
        *f.not_open.lock().unwrap() = None;
        f.restarted.store(true, Ordering::Relaxed);
        let t2 = t + Duration::from_secs(5);
        st.tick(t2);
        assert_eq!(reopens(&ev), 1);
        // the new pad gets the latest state, its seq from 1 again
        let last = f.states().last().unwrap().clone();
        assert_eq!((last.state.buttons, last.seq), (0x40, 1));
    }

    #[test]
    fn an_unreachable_helper_is_tried_again_with_backoff_and_the_pads_reopened() {
        let f = Arc::new(FakeHelper::default());
        let (mut st, ev) = link_with(&f);
        let t = Instant::now();
        open_slot_t(&mut st, 2, "a", t);
        f.down.store(true, Ordering::Relaxed);
        st.on_cmd(
            Cmd::State {
                slot: 2,
                state: st_(0x20),
            },
            t,
        );
        st.tick(t);
        let calls = st.calls;
        st.tick(t + Duration::from_millis(100));
        assert_eq!(st.calls, calls, "waits before trying again");
        f.down.store(false, Ordering::Relaxed);
        st.tick(t + Duration::from_millis(300));
        let log = f.log();
        assert_eq!(log.iter().filter(|l| *l == "open2").count(), 2, "{log:?}");
        assert_eq!(
            f.states().last().unwrap().state.buttons,
            0x20,
            "nothing lost"
        );
        assert!(reopens(&ev) == 0, "the same pad: no new user prompt");
    }

    /// Only a pad that was playing: refused for good at the first open, a guest is told at once.
    #[test]
    fn a_pad_the_helper_keeps_refusing_ends_in_lost_not_a_loop() {
        let f = Arc::new(FakeHelper::default());
        let (mut st, ev) = link_with(&f);
        let t = Instant::now();
        open_slot_t(&mut st, 2, "a", t);
        let lost = |ev: &Events| {
            ev.lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, Event::Lost { slot: 2 }))
        };
        // The helper restarted, and now refuses (nobody signed in).
        *f.not_open.lock().unwrap() = Some(2);
        *f.refuse_open.lock().unwrap() = Some("vpad_no_user".into());
        st.on_cmd(
            Cmd::State {
                slot: 2,
                state: st_(0x40),
            },
            t,
        );
        st.tick(t);
        let mut now = t;
        while now < t + link::REOPEN_GIVE_UP - Duration::from_secs(1) {
            now += Duration::from_millis(500);
            st.tick(now);
        }
        assert!(!lost(&ev), "still trying: it may come back");
        // It comes back in time: nothing lost, and the clock starts over.
        *f.refuse_open.lock().unwrap() = None;
        now += Duration::from_secs(5);
        st.tick(now);
        assert!(!lost(&ev));
        // Refused again, and this time it stays that way.
        *f.not_open.lock().unwrap() = Some(2);
        *f.refuse_open.lock().unwrap() = Some("vpad_failed".into());
        st.on_cmd(
            Cmd::State {
                slot: 2,
                state: st_(0x80),
            },
            now,
        );
        st.tick(now);
        let t2 = now;
        while now < t2 + link::REOPEN_GIVE_UP + Duration::from_secs(10) {
            now += Duration::from_millis(500);
            st.tick(now);
        }
        assert!(lost(&ev), "the guest is told");
        assert!(st.idle());
    }

    /// The party's side of that: their socket is told why, and the token is void.
    #[test]
    fn a_lost_pad_is_said_to_the_guest() {
        let t = Instant::now();
        let mut p = party();
        let id = admit(&mut p, "Ann", t);
        let (tx, _rx) = conn();
        p.attach(id, 1, tx);
        assert_eq!(byes(p.leave(id, "lost", t)), vec!["lost"]);
        // A guest whose time ran out has no socket to say it on.
        let mut p = party();
        let id = admit(&mut p, "Ann", t);
        assert!(byes(p.leave(id, "timeout", t)).is_empty());
    }

    /// An open that arrives as an idle link is ending keeps the link alive; without one it goes.
    #[test]
    fn an_open_at_idle_shutdown_keeps_the_link() {
        let f = Arc::new(FakeHelper::default());
        let (mut st, _ev) = link_with(&f);
        let t = Instant::now();
        let key = "192.0.2.201-idle";
        let (tx, rx) = std::sync::mpsc::channel();
        link::test_put_link(key, 77, tx.clone());
        let (reply, _r) = oneshot::channel();
        tx.send(Cmd::Open {
            slot: 2,
            token: "a".into(),
            reply,
        })
        .unwrap();
        assert!(!link::try_end(&mut st, &rx, key, 77, t), "a pad to serve");
        assert!(link::test_has_link(key), "still reachable for its Close");
        st.on_cmd(Cmd::Close { slot: 2 }, t);
        st.tick(t);
        assert!(st.idle());
        assert!(link::try_end(&mut st, &rx, key, 77, t));
        assert!(!link::test_has_link(key));
    }

    #[test]
    fn a_guest_without_a_name_is_named_by_the_host_app() {
        assert_eq!(clean_name(None), "");
        assert_eq!(clean_name(Some(" \u{7} ")), "");
        assert_eq!(clean_name(Some(" Ann ")), "Ann");
    }

    #[test]
    fn a_busy_slot_and_an_older_helper_are_said() {
        let f = Arc::new(FakeHelper::default());
        f.busy.lock().unwrap().push(3);
        let (mut st, _ev) = link_with(&f);
        let t = Instant::now();
        assert_eq!(
            open_slot_t(&mut st, 3, "a", t).try_recv().unwrap(),
            OpenResult::Busy
        );
        assert!(st.idle());
        f.old.store(true, Ordering::Relaxed);
        assert_eq!(
            open_slot_t(&mut st, 2, "a", t).try_recv().unwrap(),
            OpenResult::OldHelper
        );
        f.old.store(false, Ordering::Relaxed);
        f.down.store(true, Ordering::Relaxed);
        assert!(matches!(
            open_slot_t(&mut st, 2, "a", t + Duration::from_secs(10))
                .try_recv()
                .unwrap(),
            OpenResult::Failed(_)
        ));
    }

    #[test]
    fn rumble_and_light_are_told_only_when_they_change() {
        let f = Arc::new(FakeHelper::default());
        let (mut st, ev) = link_with(&f);
        let t = Instant::now();
        open_slot_t(&mut st, 2, "a", t);
        st.tick(t + Duration::from_millis(300));
        *f.out.lock().unwrap() = Output {
            motors: [200, 10],
            color: [0, 0, 255],
        };
        st.tick(t + Duration::from_millis(600));
        st.tick(t + Duration::from_millis(900));
        let e = ev.lock().unwrap();
        assert_eq!(e.len(), 2, "{e:?}");
        assert_eq!(
            e[1],
            Event::Output {
                slot: 2,
                out: Output {
                    motors: [200, 10],
                    color: [0, 0, 255]
                }
            }
        );
    }

    #[test]
    fn one_unreadable_state_does_not_stop_the_others_or_the_heartbeats() {
        let f = Arc::new(FakeHelper::default());
        let (mut st, ev) = link_with(&f);
        let t = Instant::now();
        open_slot_t(&mut st, 2, "a", t);
        open_slot_t(&mut st, 3, "b", t);
        open_slot_t(&mut st, 4, "c", t);
        // Player 2's state is one the helper refuses the whole batch for
        *f.unreadable.lock().unwrap() = Some(2);
        let mut odd = st_(0x20);
        odd.motion = Some(vpad::Motion {
            gyro: [1.0, 0.0, 0.0],
            accel: [0.0, 1.0, 0.0],
        });
        st.on_cmd(
            Cmd::State {
                slot: 2,
                state: odd,
            },
            t,
        );
        st.on_cmd(
            Cmd::State {
                slot: 3,
                state: st_(0x40),
            },
            t,
        );
        st.on_cmd(
            Cmd::State {
                slot: 4,
                state: st_(0x80),
            },
            t,
        );
        st.tick(t);
        // no backoff, nothing reopened: the others' states went, Player 2's press without motion
        assert!(st.wait(t) < Duration::from_millis(250));
        let got: Vec<(u8, u32, bool)> = f
            .states()
            .iter()
            .map(|s| (s.slot, s.state.buttons, s.state.motion.is_some()))
            .collect();
        assert!(got.contains(&(3, 0x40, false)), "{got:?}");
        assert!(got.contains(&(4, 0x80, false)), "{got:?}");
        assert!(got.contains(&(2, 0x20, false)), "{got:?}");
        let log = f.log();
        assert_eq!(
            log.iter().filter(|l| l.starts_with("open")).count(),
            3,
            "{log:?}"
        );
        assert_eq!(reopens(&ev), 0);
        // the odd guest keeps sending it: still no backoff, the heartbeats go on
        for i in 1..=4u32 {
            let ti = t + vpad::HEARTBEAT * i;
            let mut odd = st_(if i % 2 == 0 { 0x20 } else { 0 });
            odd.motion = Some(vpad::Motion {
                gyro: [2.0, 0.0, 0.0],
                accel: [0.0, 1.0, 0.0],
            });
            st.on_cmd(
                Cmd::State {
                    slot: 2,
                    state: odd,
                },
                ti,
            );
            st.tick(ti);
        }
        let log = f.log();
        assert!(log.iter().any(|l| l == "hb3live"), "{log:?}");
        assert!(log.iter().any(|l| l == "hb4live"), "{log:?}");
        assert_eq!(
            log.iter().filter(|l| l.starts_with("open")).count(),
            3,
            "{log:?}"
        );
    }

    #[test]
    fn a_pad_made_after_its_guest_stopped_waiting_is_removed() {
        let f = Arc::new(FakeHelper::default());
        let (mut st, _ev) = link_with(&f);
        let t = Instant::now();
        let (tx, rx) = oneshot::channel();
        st.on_cmd(
            Cmd::Open {
                slot: 2,
                token: "a".into(),
                reply: tx,
            },
            t,
        );
        drop(rx); // open_slot gave up
        st.tick(t);
        st.tick(t);
        assert_eq!(f.log(), vec!["open2", "close2"]);
        assert!(st.idle());
        st.tick(t + vpad::HEARTBEAT * 3);
        assert!(
            !f.log().iter().any(|l| l.starts_with("hb")),
            "{:?}",
            f.log()
        );
        // an abandon only removes the pad its token owns
        open_slot_t(&mut st, 3, "b", t);
        st.on_cmd(
            Cmd::Abandon {
                slot: 3,
                token: "someone-else".into(),
            },
            t,
        );
        st.tick(t);
        assert!(!st.idle());
        st.on_cmd(
            Cmd::Abandon {
                slot: 3,
                token: "b".into(),
            },
            t,
        );
        st.tick(t);
        assert!(st.idle() && f.log().contains(&"close3".to_string()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_slow_open_is_given_up_and_its_pad_never_lives_on() {
        let console = "10.77.0.9";
        let f = Arc::new(FakeHelper::default());
        *f.open_delay.lock().unwrap() = OPEN_WAIT + Duration::from_millis(700);
        set_helper(console, f.clone());
        let r = open_slot(console, 2, "0123456789abcdef0123456789abcdef").await;
        assert_eq!(r, OpenResult::Failed("timeout".into()));
        until("the late pad to be removed", || {
            f.log().contains(&"close2".to_string())
        })
        .await;
        tokio::time::sleep(vpad::HEARTBEAT * 3).await;
        let log = f.log();
        assert!(!log.iter().any(|l| l.starts_with("hb")), "{log:?}");
        assert_eq!(log, vec!["open2", "close2"]);
    }

    #[test]
    fn closing_removes_the_pad() {
        let f = Arc::new(FakeHelper::default());
        let (mut st, _ev) = link_with(&f);
        let t = Instant::now();
        open_slot_t(&mut st, 2, "a", t);
        st.on_cmd(Cmd::Close { slot: 2 }, t);
        st.tick(t);
        assert!(f.log().contains(&"close2".to_string()));
        assert!(st.idle());
    }

    // ── end to end: a fake helper, real guest sockets ──

    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as TMsg;

    type Guest_ = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    /// The tests that open Play Together take turns: parties are engine-wide, and a "Playing
    /// nearby" join only goes through while exactly one is open.
    static PARTY_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn set_live(console: &str, session: u64) {
        FAKE_LIVE
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(host_key(console), session);
    }

    fn set_helper(console: &str, f: Arc<FakeHelper>) {
        link::TEST_PORTS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(host_key(console), f);
    }

    fn local() -> (ConnectInfo<std::net::SocketAddr>, HeaderMap) {
        let mut h = HeaderMap::new();
        h.insert("host", "127.0.0.1:19113".parse().unwrap());
        (ConnectInfo("127.0.0.1:5555".parse().unwrap()), h)
    }

    async fn body(r: Response) -> serde_json::Value {
        let b = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&b).unwrap()
    }

    async fn host_state(console: &str) -> serde_json::Value {
        let (ci, h) = local();
        body(
            get_state(
                ci,
                h,
                Query(AddrQuery {
                    addr: console.into(),
                }),
            )
            .await,
        )
        .await
    }

    async fn serve() -> String {
        let app = router::<()>();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                l,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        format!("ws://{addr}/api/join/ws")
    }

    async fn guest(url: &str, hello: serde_json::Value) -> Guest_ {
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        ws.send(TMsg::Text(hello.to_string().into())).await.unwrap();
        ws
    }

    /// The next message that has `key` (other messages, such as the host's light or a stage
    /// change racing ahead, are skipped), within 5 s each.
    async fn next_with(ws: &mut Guest_, key: &str) -> serde_json::Value {
        for _ in 0..20 {
            let v = next(ws).await;
            if v.get(key).is_some() {
                return v;
            }
        }
        panic!("no message with {key}");
    }

    /// The next text message that is not a status, within 5 s.
    async fn next(ws: &mut Guest_) -> serde_json::Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let m = tokio::time::timeout_at(deadline, ws.next())
                .await
                .expect("a message in time");
            match m {
                Some(Ok(TMsg::Text(t))) => {
                    let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                    if v.get("status").is_none() {
                        return v;
                    }
                }
                Some(Ok(_)) => {}
                other => return serde_json::json!({"closed": format!("{other:?}")}),
            }
        }
    }

    async fn until(what: &str, f: impl Fn() -> bool) {
        // 10 s: some waits outlast a 5 s grace in the code under test, on a slow CI runner too.
        for _ in 0..400 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out waiting for {what}");
    }

    async fn allow_next(console: &str, allow: bool) {
        let mut id = None;
        for _ in 0..200 {
            let s = host_state(console).await;
            if let Some(a) = s["pending"].as_array().and_then(|a| a.first()) {
                id = a["id"].as_u64();
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let (ci, h) = local();
        let r = body(
            decide(
                ci,
                h,
                Json(DecideBody {
                    id: id.unwrap(),
                    allow,
                }),
            )
            .await,
        )
        .await;
        assert_eq!(r["ok"], true);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn guests_join_play_come_back_and_are_let_go() {
        let _turn = PARTY_TESTS.lock().await;
        // The host turned blocking on (off by default): PS and Create are taken once playing.
        *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings {
            block_system_buttons: true,
            ..Settings::default()
        });
        let console = "10.77.0.1";
        let f = Arc::new(FakeHelper::default());
        set_helper(console, f.clone());
        let url = serve().await;

        // Not playing: nothing to open.
        let (ci, h) = local();
        let r = open(
            ci,
            h,
            Json(AddrBody {
                addr: console.into(),
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::CONFLICT);
        set_live(console, 1);
        let (ci, h) = local();
        let s = body(
            open(
                ci,
                h,
                Json(AddrBody {
                    addr: console.into(),
                }),
            )
            .await,
        )
        .await;
        assert_eq!(s["open"], true);
        // The names the app's panel reads (client/src/screens/Play/playTogetherState.ts and
        // its test's copy of this answer): renaming one here breaks the panel.
        for k in ["pending", "guests", "settings", "ws", "playing"] {
            assert!(!s[k].is_null(), "{k} missing: {s}");
        }
        for k in ["url", "urls", "code", "expires_in_s"] {
            assert!(!s["invite"][k].is_null(), "invite.{k} missing: {s}");
        }
        let key = s["invite"]["key"].as_str().unwrap().to_string();
        let code = s["invite"]["code"].as_str().unwrap().to_string();
        assert_eq!(code.len(), CODE_LEN);
        assert!(s["ws"].as_str().is_none_or(|w| w.ends_with("/api/join/ws")));

        // A wrong key is refused at once.
        let mut g = guest(
            &url,
            serde_json::json!({"hello": {"invite": "00ff", "name": "X"}}),
        )
        .await;
        assert_eq!(next(&mut g).await["why"], "key");

        // Sam asks with the link and is allowed: Player 2.
        let mut sam = guest(
            &url,
            serde_json::json!({"hello": {"invite": key, "name": "Sam"}}),
        )
        .await;
        assert_eq!(next(&mut sam).await["stage"], "asking");
        allow_next(console, true).await;
        let j = next(&mut sam).await;
        assert_eq!(j["stage"], "joined", "{j}");
        assert_eq!(j["player"], 2);
        let token = j["token"].as_str().unwrap().to_string();
        assert!(f.log().contains(&"open2".to_string()));

        // While picking a user, PS reaches the console (the app's bits become the helper's; the
        // frame's triggers 50/60 press L2/R2 too).
        use vpad::{app_button as app, button as pad};
        let trig = pad::L2 | pad::R2;
        sam.send(TMsg::Binary(
            frame(1, app::PS | app::RIGHT, &[], None).into(),
        ))
        .await
        .unwrap();
        until("PS sent", || {
            f.states()
                .iter()
                .any(|s| s.slot == 2 && s.state.buttons == pad::PS | pad::RIGHT | trig)
        })
        .await;
        // Once they are in, PS and Create are taken.
        let (ci, h) = local();
        body(
            playing(
                ci,
                h,
                Json(PlayerBody {
                    addr: console.into(),
                    player: 2,
                }),
            )
            .await,
        )
        .await;
        sam.send(TMsg::Binary(
            frame(2, app::PS | app::SHARE | app::CROSS, &[], None).into(),
        ))
        .await
        .unwrap();
        until("PS stripped", || {
            f.states()
                .iter()
                .any(|s| s.slot == 2 && s.state.buttons == pad::CROSS | trig)
        })
        .await;
        assert!(!f
            .states()
            .iter()
            .any(|s| s.state.buttons & pad::CREATE != 0));

        // Ping is answered with its own value.
        sam.send(TMsg::Text(r#"{"ping":42}"#.into())).await.unwrap();
        assert_eq!(next_with(&mut sam, "pong").await["pong"], 42);

        // A code join (any case, with a dash) always asks; the host says no.
        let typed = format!("{}-{}", &code[..3], &code[3..]).to_lowercase();
        let mut ann = guest(
            &url,
            serde_json::json!({"hello": {"code": typed, "name": "Ann"}}),
        )
        .await;
        assert_eq!(next(&mut ann).await["stage"], "asking");
        allow_next(console, false).await;
        assert_eq!(next(&mut ann).await["why"], "denied");

        // A "Playing nearby" join carries no credential: it asks too.
        let mut bo = guest(&url, serde_json::json!({"hello": {"name": "Bo"}})).await;
        assert_eq!(next(&mut bo).await["stage"], "asking");
        allow_next(console, false).await;
        assert_eq!(next(&mut bo).await["why"], "denied");

        // Sam's Wi-Fi drops: the pad lets go, the player waits for them.
        drop(sam);
        until("neutral", || {
            f.states()
                .last()
                .is_some_and(|s| s.slot == 2 && s.state == PadState::NEUTRAL)
        })
        .await;
        until("reconnecting", || {
            with_party(&host_key(console), |p| p.guests.first().map(|g| g.state())).flatten()
                == Some("reconnecting")
        })
        .await;
        assert_eq!(host_state(console).await["players_in"], 1);
        until("plain heartbeat", || f.log().contains(&"hb2".to_string())).await;

        // Sam comes back with the token: the same player, no asking.
        let mut sam = guest(
            &url,
            serde_json::json!({"hello": {"resume": token, "name": "Sam"}}),
        )
        .await;
        let j = next(&mut sam).await;
        assert_eq!(
            (j["stage"].as_str(), j["player"].as_u64()),
            (Some("joined"), Some(2)),
            "{j}"
        );
        // A second socket with the same token replaces the first.
        let mut sam2 = guest(&url, serde_json::json!({"hello": {"resume": token}})).await;
        assert_eq!(next(&mut sam2).await["stage"], "joined");
        assert_eq!(next_with(&mut sam, "bye").await["bye"], "replaced");

        // The host stops: Sam is told; then plays again within the minute and Sam comes back.
        set_live(console, 0);
        assert_eq!(next_with(&mut sam2, "bye").await["bye"], "host_stopped");
        let s = host_state(console).await;
        assert_eq!(
            (s["playing"].as_bool(), s["players_in"].as_u64()),
            (Some(false), Some(1))
        );
        let mut sam3 = guest(&url, serde_json::json!({"hello": {"resume": token}})).await;
        assert_eq!(next(&mut sam3).await["stage"], "waiting");
        until("pad removed after the grace", || {
            f.log().contains(&"close2".to_string())
        })
        .await;
        set_live(console, 2);
        let j = next(&mut sam3).await;
        assert_eq!(j["stage"], "joined", "{j}");
        assert_eq!(
            f.log().iter().filter(|l| *l == "open2").count(),
            2,
            "the pad is made again"
        );

        // Kicked: told, the pad removed, the token void.
        let n_close = f.log().iter().filter(|l| *l == "close2").count();
        let (ci, h) = local();
        let r = body(
            kick(
                ci,
                h,
                Json(PlayerBody {
                    addr: console.into(),
                    player: 2,
                }),
            )
            .await,
        )
        .await;
        assert_eq!(r["ok"], true);
        assert_eq!(next_with(&mut sam3, "bye").await["bye"], "kicked");
        until("pad closed", || {
            f.log().iter().filter(|l| *l == "close2").count() > n_close
        })
        .await;
        let mut again = guest(&url, serde_json::json!({"hello": {"resume": token}})).await;
        assert_eq!(next(&mut again).await["why"], "expired");

        // Closed: invites stop working, nearby apps no longer see it.
        let (ci, h) = local();
        let s = body(
            close(
                ci,
                h,
                Json(AddrBody {
                    addr: console.into(),
                }),
            )
            .await,
        )
        .await;
        assert_eq!(s["open"], false);
        let mut late = guest(&url, serde_json::json!({"hello": {"invite": key}})).await;
        let why = next(&mut late).await["why"].clone();
        assert!(why == "key" || why == "closed", "{why}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_huge_or_binary_hello_is_refused_at_once() {
        let url = serve().await;
        // a message over the guest limit: the socket ends now, not after the hello wait
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let big = format!(
            r#"{{"hello":{{"name":"{}"}}}}"#,
            "x".repeat(GUEST_MSG_MAX * 4)
        );
        let _ = ws.send(TMsg::Text(big.into())).await;
        let v = next(&mut ws).await;
        assert!(v["why"] == "hello" || v.get("closed").is_some(), "{v}");
        // a binary message before the hello
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        ws.send(TMsg::Binary(vec![1u8; 64].into())).await.unwrap();
        let v = next(&mut ws).await;
        assert!(v["why"] == "hello" || v.get("closed").is_some(), "{v}");
        // a hello of the largest real size is fine
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let hello = serde_json::json!({"hello": {
            "invite": "0".repeat(64), "resume": "", "code": "ABC-DEF", "name": "é".repeat(32)
        }});
        assert!(hello.to_string().len() < 1024);
        ws.send(TMsg::Text(hello.to_string().into())).await.unwrap();
        let v = next(&mut ws).await;
        assert!(v["stage"] == "refused" && v["why"] != "hello", "{v}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_flooding_guest_does_not_hold_up_another_or_the_heartbeats() {
        let _turn = PARTY_TESTS.lock().await;
        let console = "10.77.0.11";
        let f = Arc::new(FakeHelper::default());
        set_helper(console, f.clone());
        set_live(console, 1);
        let url = serve().await;
        let (ci, h) = local();
        let s = body(
            open(
                ci,
                h,
                Json(AddrBody {
                    addr: console.into(),
                }),
            )
            .await,
        )
        .await;
        let key = s["invite"]["key"].as_str().unwrap().to_string();
        let join = |name: &'static str| {
            let url = url.clone();
            let key = key.clone();
            async move {
                let mut g = guest(
                    &url,
                    serde_json::json!({"hello": {"invite": key, "name": name}}),
                )
                .await;
                assert_eq!(next(&mut g).await["stage"], "asking");
                allow_next(console, true).await;
                let j = next(&mut g).await;
                assert_eq!(j["stage"], "joined", "{j}");
                g
            }
        };
        let flood = join("Flood").await;
        let mut kim = join("Kim").await;
        // Flood sends pad frames as fast as the socket takes them
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let flooder = tokio::spawn(async move {
            let mut flood = flood;
            let mut n = 0u32;
            while !stop2.load(Ordering::Relaxed) {
                n += 1;
                let b = (n % 2) * vpad::app_button::CROSS;
                if flood
                    .send(TMsg::Binary(frame(n, b, &[], None).into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            (flood, n)
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        let start = Instant::now();
        let before = f.states().iter().filter(|s| s.slot == 2).count();
        // Kim's press reaches the console promptly, and keeps being heartbeat
        kim.send(TMsg::Binary(
            frame(1, vpad::app_button::CIRCLE, &[], None).into(),
        ))
        .await
        .unwrap();
        until("Kim's press", || {
            f.states()
                .iter()
                .any(|s| s.slot == 3 && s.state.buttons & vpad::button::CIRCLE != 0)
        })
        .await;
        let took = start.elapsed();
        assert!(took < Duration::from_millis(500), "{took:?}");
        let hbs = || f.log().iter().filter(|l| l.starts_with("hb3")).count();
        let h0 = hbs();
        until("Kim's heartbeats", || hbs() >= h0 + 3).await;
        stop.store(true, Ordering::Relaxed);
        let (mut flood, sent) = flooder.await.unwrap();
        let secs = start.elapsed().as_secs_f64();
        let got = f.states().iter().filter(|s| s.slot == 2).count() - before;
        // far fewer than were sent reached the helper: at most the pad rate (and a burst)
        assert!(sent as usize > got, "{sent} sent, {got} reached");
        assert!(
            (got as f64) <= PAD_RATE * (secs + 0.1) + PAD_BURST,
            "{got} in {secs}s"
        );
        let _ = flood.close(None).await;
        let _ = kim.close(None).await;
        let (ci, h) = local();
        close(
            ci,
            h,
            Json(AddrBody {
                addr: console.into(),
            }),
        )
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_older_helper_is_said_before_the_host_is_asked() {
        let _turn = PARTY_TESTS.lock().await;
        let console = "10.77.0.2";
        let f = Arc::new(FakeHelper::default());
        f.old.store(true, Ordering::Relaxed);
        set_helper(console, f.clone());
        set_live(console, 1);
        let url = serve().await;
        // the host is told first
        let (ci, h) = local();
        let r = open(
            ci,
            h,
            Json(AddrBody {
                addr: console.into(),
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::CONFLICT);
        assert_eq!(body(r).await["error"], "old_helper");
        // opened with a newer helper that was then replaced by an older one: guests are told
        f.old.store(false, Ordering::Relaxed);
        let (ci, h) = local();
        let s = body(
            open(
                ci,
                h,
                Json(AddrBody {
                    addr: console.into(),
                }),
            )
            .await,
        )
        .await;
        let key = s["invite"]["key"].as_str().unwrap().to_string();
        f.old.store(true, Ordering::Relaxed);
        let mut g = guest(&url, serde_json::json!({"hello": {"invite": key}})).await;
        assert_eq!(next(&mut g).await["why"], "old_helper");
        assert!(host_state(console).await["pending"]
            .as_array()
            .unwrap()
            .is_empty());
        let (ci, h) = local();
        close(
            ci,
            h,
            Json(AddrBody {
                addr: console.into(),
            }),
        )
        .await;
    }
}
