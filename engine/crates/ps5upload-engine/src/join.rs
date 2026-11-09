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
//!   "token": t, "hevc": bool, "track": bool}` (`track`: an internet guest's pictures come on the
//!   RTP video track its page offered, not on the channel; join_net/rtc.rs) or `{"stage": "refused", "why": "key" | "expired" | "full" |
//!   "denied" | "no_answer" (the host did not answer in time) | "closed" | "not_playing" |
//!   "old_helper" | "unreachable" | "slow_down" (5 wrong codes in a minute) | "seat_taken" (a
//!   resume to the stream's controller, which the host needs back, with no pad free)}`.
//! * guest → engine: binary pad frames ([`parse_pad`]), `{"ping": t}` (answered `{"pong": t}`),
//!   `{"leave": 1}`.
//! * engine → guest: the session's picture and sound exactly as the watch party gets them (same
//!   binary kinds, `rp_stream_api::add_player`), `{"status": {...}}` every second,
//!   `{"rumble": [l, r], "light": [r, g, b]}` when the game changes them, `{"bye": "kicked" |
//!   "closed" | "replaced" | "host_stopped" | "lost" (the console no longer has their
//!   controller) | "seat_taken" (the host needed the stream's controller back and no pad was
//!   free)}` (the socket then closes). The engine also sends a
//!   WebSocket ping every second; the browser's automatic pong gives the guest's round trip.
//!
//! **Internet guests** ([`crate::join_net`]) carry the same messages over a WebRTC data channel
//! labelled `"join"` instead of the socket: the guest session ([`guest_session`]) runs over
//! [`GuestSocket`], which both implement. Binary messages are cut into pieces there
//! (`join_net::chunk`), and a ping goes as a framed message the page answers.
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
pub(crate) const PLAYER_QUEUE: usize = 6;
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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Settings {
    /// Take PS and Create from guests who are playing (they keep them while picking a user).
    /// On unless the host turns it off: a friend's PS could quit the game, or update the console
    /// past its exploit (#649).
    pub block_system_buttons: bool,
    /// Guests at most (1-3).
    pub max_players: u8,
    /// A friend may get the stream's own controller (Player 1, signed in as the host's PS5 user)
    /// while this device shows no picture and no phone plays on it (#687). Off unless the host
    /// turns it on: a friend otherwise always gets their own controller and picks a user (#720).
    #[serde(default)]
    pub stream_seat: bool,
    /// The host may make internet invites (a room on the signalling hub, WebRTC to the guest:
    /// crate::join_net). On unless the host turns it off.
    #[serde(default = "yes")]
    pub internet_invites: bool,
    /// The signalling hub (`wss://`; `ws://` only to this computer).
    #[serde(default = "default_hub")]
    pub hub_url: String,
    /// STUN servers (`host:port`) the engine asks for its outside address.
    #[serde(default = "default_stun")]
    pub stun: Vec<String>,
    /// Ask the router for a port (UPnP-IGD, NAT-PMP / PCP) while an internet invite is open.
    #[serde(default = "yes")]
    pub upnp: bool,
    /// Internet guests whose page offers it get the picture as an RTP video track (the browser's
    /// own jitter buffer, retransmission and decoder: crate::join_net::rtc), not on the data
    /// channel. On unless the host turns it off; off, or a page that offers no track, keeps the
    /// data channel.
    #[serde(default = "yes")]
    pub video_track: bool,
}

fn yes() -> bool {
    true
}

fn default_hub() -> String {
    crate::join_net::DEFAULT_HUB.to_string()
}

fn default_stun() -> Vec<String> {
    crate::join_net::DEFAULT_STUN
        .iter()
        .map(|s| s.to_string())
        .collect()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            block_system_buttons: true,
            max_players: MAX_GUESTS,
            stream_seat: false,
            internet_invites: true,
            hub_url: default_hub(),
            stun: default_stun(),
            upnp: true,
            video_track: true,
        }
    }
}

static SETTINGS: Mutex<Option<Settings>> = Mutex::new(None);

/// Other modules' tests (share/tests.rs): the hub under test, and the internet invites switch.
#[cfg(test)]
pub(crate) fn set_test_hub(hub_url: &str, internet: bool) {
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings {
        hub_url: hub_url.to_string(),
        internet_invites: internet,
        stun: Vec::new(),
        upnp: false,
        ..Settings::default()
    });
}

pub(crate) fn settings() -> Settings {
    SETTINGS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

// ── "Invites work" (kept across restarts) ───────────────────────────────────────────────────────

/// Where the engine keeps "Invites work" (in its data folder). The other Play Together choices
/// last one engine run, as before.
const INVITES_FILE: &str = "play_together.json";

/// "Invites work" as the API says it: "anywhere" (the one link: the internet invite, which also
/// works on the Wi-Fi) or "wifi" (the home network's link only): `internet_invites` on or off.
pub(crate) fn invites_word(anywhere: bool) -> &'static str {
    if anywhere {
        "anywhere"
    } else {
        "wifi"
    }
}

/// The settings as the API shows them: every field, and `invites` (the same choice as
/// `internet_invites`, in the words of Settings › App › Play Together).
pub(crate) fn settings_view(s: &Settings) -> serde_json::Value {
    let mut v = serde_json::to_value(s).unwrap_or_default();
    v["invites"] = invites_word(s.internet_invites).into();
    v
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct StoredInvites {
    /// "anywhere" or "wifi".
    #[serde(default)]
    invites: Option<String>,
    /// The older name of the same choice (internet invites on or off), read too.
    #[serde(default, skip_serializing)]
    internet_invites: Option<bool>,
}

/// The saved choice (true: anywhere), None when the file says none.
fn read_invites(path: &std::path::Path) -> Option<bool> {
    let s: StoredInvites = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    match s.invites.as_deref() {
        Some("anywhere") => Some(true),
        Some("wifi") => Some(false),
        _ => s.internet_invites,
    }
}

fn write_invites(path: &std::path::Path, anywhere: bool) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| crate::state_io::describe("the Play Together setting", dir, &e))?;
    }
    let body = serde_json::to_vec_pretty(&StoredInvites {
        invites: Some(invites_word(anywhere).to_string()),
        internet_invites: None,
    })
    .map_err(|e| e.to_string())?;
    // Written aside and renamed over: a crash mid-write never leaves half a file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| crate::state_io::describe("the Play Together setting", path, &e))
}

fn invites_path() -> Option<std::path::PathBuf> {
    ps5upload_ava1::pool::data_dir().map(|d| d.join(INVITES_FILE))
}

/// "Invites work" as it was last set; called once as the engine starts. Nothing saved (every
/// engine before 12.1 kept Allow internet invites for one run only, on at each start): anywhere.
pub(crate) fn load_invites() {
    let Some(anywhere) = invites_path().and_then(|p| read_invites(&p)) else {
        return;
    };
    let mut g = SETTINGS.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = g.clone().unwrap_or_default();
    s.internet_invites = anywhere;
    *g = Some(s);
}

/// Saves "Invites work" (never from the tests: they would write the user's own data folder).
fn save_invites(anywhere: bool) {
    if cfg!(test) {
        return;
    }
    if let Some(p) = invites_path() {
        if let Err(e) = write_invites(&p, anywhere) {
            crate::log_at!(Warn, "together", "join: {e}");
        }
    }
}

/// "Guests play as": the console profile a guest on Player `player` gets, chosen by the host for
/// one console (user ids are that console's). The helper seats the guest's pad in it as it is made
/// (`vpad.open` with `bind_user`), so the console never asks "Who's using this controller?" and the
/// guest cannot pick another profile. No entry for a player: the console asks (as before).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SeatProfile {
    /// 2-4.
    pub player: u8,
    /// The console's user id (`user.list`).
    pub user: u32,
    /// The profile's name as the console showed it when the host chose it.
    #[serde(default)]
    pub name: String,
    /// The host confirmed this profile although it is their own (the one in front): the guest can
    /// use its saves and trophies. Without it the helper refuses that profile.
    #[serde(default)]
    pub owner: bool,
}

/// Each console's choices, by [`host_key`]. Kept apart from [`Settings`] (one set for every
/// console, and in bug reports): these name the host's profiles.
static SEAT_PROFILES: Mutex<Option<HashMap<String, Vec<SeatProfile>>>> = Mutex::new(None);

/// The host's choices for `console`.
pub(crate) fn seat_profiles(console: &str) -> Vec<SeatProfile> {
    SEAT_PROFILES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&host_key(console)).cloned())
        .unwrap_or_default()
}

/// The profile chosen for `player` on `console`.
fn seat_profile(console: &str, player: u8) -> Option<SeatProfile> {
    seat_profiles(console)
        .into_iter()
        .find(|p| p.player == player)
}

/// The [`vpad::Bind`] a new pad for `player` asks for: only a profile the host chose, and the
/// owner flag only as the host confirmed it.
#[cfg(test)]
fn seat_bind(console: &str, player: u8) -> Option<vpad::Bind> {
    seat_profile(console, player).as_ref().map(seat_bind_of)
}

/// Another profile (or none, or one where there was none): the pad must be seated again.
fn seat_changed(had: Option<&SeatProfile>, want: Option<&SeatProfile>) -> bool {
    had.map(seat_bind_of) != want.map(seat_bind_of)
}

fn seat_bind_of(p: &SeatProfile) -> vpad::Bind {
    vpad::Bind {
        user: p.user,
        owner: p.owner,
    }
}

fn set_seat_profiles(console: &str, v: Vec<SeatProfile>) {
    let mut g = SEAT_PROFILES.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if v.is_empty() {
        m.remove(&host_key(console));
    } else {
        m.insert(host_key(console), v);
    }
}

/// The choices as the host's request gives them: Player 2-4 each at most once, a profile at most
/// once, a real user id (not 0, not the console's "nobody"), names trimmed. The error names what is
/// wrong.
/// The host's own profile (the one in front) is chosen only with `owner`, their confirmation. When
/// the console does not say who is in front (it did not answer, or named nobody), any profile newly
/// chosen without that confirmation may be the host's own: refused as `owner_unknown`, which the app
/// asks about the same way. A choice kept as it was (`before`) keeps the confirmation it had.
fn owner_check(
    v: &[SeatProfile],
    users: Option<&[vpad::ConsoleUser]>,
    before: &[SeatProfile],
) -> Result<(), &'static str> {
    let front = users.and_then(|u| u.iter().find(|u| u.foreground).map(|u| u.id));
    match front {
        Some(front) => {
            if v.iter().any(|p| !p.owner && p.user == front) {
                return Err("owner_confirm");
            }
        }
        None => {
            let kept = |p: &SeatProfile| {
                before
                    .iter()
                    .any(|b| b.player == p.player && b.user == p.user)
            };
            if v.iter().any(|p| !p.owner && !kept(p)) {
                return Err("owner_unknown");
            }
        }
    }
    Ok(())
}

fn clean_seat_profiles(v: Vec<SeatProfile>) -> Result<Vec<SeatProfile>, &'static str> {
    let mut out: Vec<SeatProfile> = Vec::new();
    for p in v {
        if !(vpad::FIRST_SLOT..=vpad::LAST_SLOT).contains(&p.player) {
            return Err("player");
        }
        if p.user == 0 || p.user == u32::MAX {
            return Err("user");
        }
        if out.iter().any(|o| o.player == p.player || o.user == p.user) {
            return Err("duplicate");
        }
        out.push(SeatProfile {
            name: clean_name(Some(&p.name)),
            ..p
        });
    }
    out.sort_by_key(|p| p.player);
    Ok(out)
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
    /// The guest's keyframe asks that reached the console (at most one a [`KEY_ASK_GAP`]).
    key_asks: AtomicU64,
    /// What the guest's page said last about its picture (`{"stats": …}`, every 2 s at most).
    page: Mutex<Option<PageReport>>,
}

/// A guest page's report on its picture: totals since its picture started, kept as numbers only.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PageReport {
    /// Pictures drawn, dropped (waiting for a keyframe, or behind), arriving over 50 ms after
    /// the one before.
    frames: u64,
    dropped: u64,
    late: u64,
    /// Average decode time over the last report, ms.
    decode_ms: f64,
    /// Over 100 ms without a new picture: how often, in all, the longest (ms).
    stalls: u64,
    stall_ms: u64,
    worst_stall_ms: u64,
    /// Times the picture waited for a keyframe.
    key_waits: u64,
    /// "webcodecs" or "mse" (the picture on the data channel), "track" (the video track: the
    /// numbers are the browser's own, `getStats` inbound-rtp).
    decoder: Option<&'static str>,
    /// The video track's: the browser's jitter buffer delay (average, ms), and the NACKs and PLIs
    /// it sent.
    jitter_ms: Option<f64>,
    nacks: Option<u64>,
    plis: Option<u64>,
    /// What stops the picture on the page ("decode": its decoder failed, "no_hevc"), and the
    /// codec it set up (e.g. "avc1.640028"): a page that never drew a picture says why (#720).
    problem: Option<&'static str>,
    codec: Option<String>,
    /// The sound once it started (#720: an iPhone guest's sound stuttered and the host's log had
    /// nothing about it): its path ("webcodecs" or "wasm", decoded on the page and scheduled;
    /// "mse", the media player), and its counters: decoded, covered (a lost packet concealed),
    /// late (an underrun: it ran dry), dropped (too far ahead), and the media player's jumps.
    sound: Option<&'static str>,
    snd_decoded: Option<u64>,
    snd_concealed: Option<u64>,
    snd_underruns: Option<u64>,
    snd_dropped: Option<u64>,
    snd_jumps: Option<u64>,
    /// Stalled time since the report before (ms), and when this one came.
    stall_ms_recent: u64,
    at: Option<Instant>,
}

/// Reports closer together than this are ignored (the page sends one every 2 s).
const PAGE_REPORT_GAP: Duration = Duration::from_millis(1500);

impl PageReport {
    /// The page's `{"stats": …}`, its numbers taken (anything else ignored); None when it has none.
    fn from_page(v: &serde_json::Value) -> Option<PageReport> {
        let o = v.as_object()?;
        let n = |k: &str| {
            o.get(k)
                .and_then(|x| x.as_f64())
                .filter(|x| x.is_finite() && *x >= 0.0)
                .map(|x| x.min(1e12))
        };
        Some(PageReport {
            frames: n("frames")? as u64,
            dropped: n("dropped").unwrap_or(0.0) as u64,
            late: n("late").unwrap_or(0.0) as u64,
            decode_ms: (n("decode_ms").unwrap_or(0.0).min(10_000.0) * 10.0).round() / 10.0,
            stalls: n("stalls").unwrap_or(0.0) as u64,
            stall_ms: n("stall_ms").unwrap_or(0.0) as u64,
            worst_stall_ms: n("worst_stall_ms").unwrap_or(0.0) as u64,
            key_waits: n("key_waits").unwrap_or(0.0) as u64,
            decoder: match o.get("decoder").and_then(|d| d.as_str()) {
                Some("webcodecs") => Some("webcodecs"),
                Some("mse") => Some("mse"),
                Some("track") => Some("track"),
                _ => None,
            },
            jitter_ms: n("jitter_ms").map(|x| (x.min(60_000.0) * 10.0).round() / 10.0),
            nacks: n("nacks").map(|x| x as u64),
            plis: n("plis").map(|x| x as u64),
            problem: match o.get("problem").and_then(|d| d.as_str()) {
                Some("decode") => Some("decode"),
                Some("no_hevc") => Some("no_hevc"),
                _ => None,
            },
            codec: o
                .get("codec")
                .and_then(|d| d.as_str())
                .filter(|c| {
                    !c.is_empty()
                        && c.len() <= 40
                        && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
                })
                .map(str::to_string),
            sound: match o.get("snd").and_then(|d| d.as_str()) {
                Some("webcodecs") => Some("webcodecs"),
                Some("wasm") => Some("wasm"),
                Some("mse") => Some("mse"),
                _ => None,
            },
            snd_decoded: n("snd_decoded").map(|x| x as u64),
            snd_concealed: n("snd_concealed").map(|x| x as u64),
            snd_underruns: n("snd_underruns").map(|x| x as u64),
            snd_dropped: n("snd_dropped").map(|x| x as u64),
            snd_jumps: n("snd_jumps").map(|x| x as u64),
            ..PageReport::default()
        })
    }

    /// The sound's part of the "left" line, when the page said how its sound went.
    fn sound_line(&self) -> String {
        let Some(path) = self.sound else {
            return String::new();
        };
        let mut parts = Vec::new();
        for (v, what) in [
            (self.snd_decoded, "decoded"),
            (self.snd_concealed, "covered"),
            (self.snd_underruns, "ran dry"),
            (self.snd_dropped, "dropped"),
            (self.snd_jumps, "jumps"),
        ] {
            if let Some(v) = v {
                parts.push(format!("{v} {what}"));
            }
        }
        if parts.is_empty() {
            format!("; sound {path}")
        } else {
            format!("; sound {path}: {}", parts.join(", "))
        }
    }

    /// How the picture looks to the guest now: "smooth", "stutters" or "poor", from the time it
    /// stood still since the report before (a page that stopped reporting says nothing).
    fn quality(&self, now: Instant) -> Option<&'static str> {
        let at = self.at?;
        if now.saturating_duration_since(at) > Duration::from_secs(10) {
            return None;
        }
        Some(match self.stall_ms_recent {
            0..=150 => "smooth",
            151..=600 => "stutters",
            _ => "poor",
        })
    }

    fn json(&self, now: Instant) -> serde_json::Value {
        serde_json::json!({
            "frames": self.frames,
            "dropped": self.dropped,
            "late": self.late,
            "decode_ms": self.decode_ms,
            "stalls": self.stalls,
            "stall_ms": self.stall_ms,
            "worst_stall_ms": self.worst_stall_ms,
            "key_waits": self.key_waits,
            "decoder": self.decoder,
            "jitter_ms": self.jitter_ms,
            "nacks": self.nacks,
            "plis": self.plis,
            "problem": self.problem,
            "codec": self.codec,
            "sound": self.sound,
            "snd_underruns": self.snd_underruns,
            "snd_dropped": self.snd_dropped,
            "snd_concealed": self.snd_concealed,
            "snd_jumps": self.snd_jumps,
            "stall_ms_recent": self.stall_ms_recent,
            "age_s": self.at.map(|t| now.saturating_duration_since(t).as_secs()),
            "quality": self.quality(now),
        })
    }
}

impl Stats {
    /// A page report: kept unless the last came under [`PAGE_REPORT_GAP`] ago. A report whose
    /// totals went down is a new picture (a reconnect): its stalled time counts from zero.
    fn page_report(&self, v: &serde_json::Value, now: Instant) {
        let Some(mut r) = PageReport::from_page(v) else {
            return;
        };
        let mut g = self.page.lock().unwrap_or_else(|e| e.into_inner());
        let before = g.as_ref();
        if before
            .and_then(|b| b.at)
            .is_some_and(|t| now.saturating_duration_since(t) < PAGE_REPORT_GAP)
        {
            return;
        }
        r.stall_ms_recent = match before {
            Some(b) if r.stall_ms >= b.stall_ms && r.frames >= b.frames => r.stall_ms - b.stall_ms,
            _ => r.stall_ms,
        };
        r.at = Some(now);
        *g = Some(r);
    }

    fn page_json(&self, now: Instant) -> serde_json::Value {
        self.page
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|r| r.json(now))
            .unwrap_or(serde_json::Value::Null)
    }

    /// For the "left" line: how the page's picture went, from its last report.
    fn page_line(&self) -> String {
        let g = self.page.lock().unwrap_or_else(|e| e.into_inner());
        match g.as_ref() {
            None => "the page never reported its picture".into(),
            Some(r) => format!(
                "picture {} {}{}: {} drawn, {} stalls{}{}",
                r.decoder.unwrap_or("?"),
                r.codec.as_deref().unwrap_or("?"),
                r.problem.map(|p| format!(" ({p})")).unwrap_or_default(),
                r.frames,
                r.stalls,
                r.jitter_ms
                    .map(|j| format!(", jitter {j} ms"))
                    .unwrap_or_default(),
                r.sound_line(),
            ),
        }
    }

    fn quality(&self, now: Instant) -> Option<&'static str> {
        self.page
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|r| r.quality(now))
    }

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
    /// Seated in the profile the host chose ("Guests play as"): its id and name. The console
    /// does not ask, and PS and Create are blocked from the start.
    plays_as: Option<(u32, String)>,
    /// The host's choice their pad was last opened with (the link asks the helper for it on every
    /// reopen): a resume compares it with the choice now and seats the pad again when it changed.
    seat: Option<SeatProfile>,
    /// A profile was chosen for their player but not given to their pad (the console asks):
    /// why (`link::unseated`).
    seat_note: Option<&'static str>,
    /// On [`STREAM_SEAT`]: when their last state arrived, while one is held on the controller.
    seat_input_at: Option<Instant>,
    /// Leaving the stream's controller for a helper pad (the host's side needs it back).
    seat_move: SeatMove,
    /// The helper has the pad (closed while the host is away).
    slot_open: bool,
    joined_at: Instant,
    stats: Arc<Stats>,
    /// An internet guest's peer connection (their last one), None on the LAN.
    net: Option<Arc<crate::join_net::PeerInfo>>,
}

/// A guest on [`STREAM_SEAT`] giving it back to the host's side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SeatMove {
    #[default]
    No,
    /// The seat is let go; a helper pad is made once they are back (their socket is gone).
    Wanted,
    /// A helper pad is being made for them ([`move_off_seat`]).
    Running,
}

impl Guest {
    /// Their connection, as the host's panel and together_status show it: `path` ("lan",
    /// "tailscale", "internet") and `rtt_ms`. An internet guest's is its peer connection's (the
    /// pair ICE selected, with its numbers); a LAN guest's socket says only where it comes from
    /// (never the address itself).
    fn link_json(&self) -> serde_json::Value {
        match &self.net {
            Some(n) => n.link_json(),
            None => serde_json::json!({
                "path": self.ip.map(|ip| match crate::join_net::rtc::reach_rank(ip) {
                    1 => "tailscale",
                    0 => "lan",
                    _ => "internet",
                }),
                "rtt_ms": self.stats.rtt_now(),
            }),
        }
    }

    /// Their pad was made (or made again) as `r` says: seated in the profile the host chose, so
    /// they play at once (PS and Create blocked from the first frame when the host blocks them,
    /// no picking window); or the console asks them who they are, as without a choice (with the
    /// reason when a choice could not be used).
    /// `seat` is the choice the pad was opened with (None: a reopen, which asks for the same one).
    fn pad_made(&mut self, r: &link::OpenResult, seat: Option<Option<SeatProfile>>, now: Instant) {
        if let Some(seat) = seat {
            self.seat = seat;
        }
        match r {
            link::OpenResult::Seated(user) => {
                // The name of the choice it was seated in: the choice now may be another one.
                let name = self
                    .seat
                    .as_ref()
                    .filter(|p| p.user == *user)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                self.plays_as = Some((*user, name));
                self.seat_note = None;
                self.playing = true;
            }
            other => {
                self.plays_as = None;
                self.seat_note = match other {
                    link::OpenResult::Unseated(note) => Some(note),
                    _ => None,
                };
                self.playing = false;
                self.picking_since = now;
            }
        }
    }

    /// The pad keeps its profile (the same user), whose name may have changed since: the name
    /// shown is the choice's now.
    fn seat_renamed(&mut self, want: Option<SeatProfile>) {
        if let (Some((user, name)), Some(w)) = (self.plays_as.as_mut(), want.as_ref()) {
            if *user == w.user {
                name.clone_from(&w.name);
            }
        }
        if want.is_some() {
            self.seat = want;
        }
    }

    /// On the stream's controller, and keeping it.
    fn on_seat(&self) -> bool {
        self.active() && self.player == STREAM_SEAT && self.seat_move == SeatMove::No
    }
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
    /// Move this guest off the stream's controller onto a helper pad ([`move_off_seat`]).
    MoveOff(u64),
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
    /// The internet invite's room on the hub (closed when this goes).
    internet: Option<crate::join_net::hub::Room>,
    /// The helper's companion for "Guests play as" (`vpad::Companion`): its state when last kept up
    /// (None: no profile chosen, or not asked yet), and when.
    companion: Option<String>,
    companion_at: Option<Instant>,
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
            internet: None,
            companion: None,
            companion_at: None,
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

    /// The internet half of the invite `inv` (None without a room or an invite).
    fn internet_json(&self, inv: Option<&InviteInfo>) -> Option<serde_json::Value> {
        let inv = inv?;
        let room = self.internet.as_ref()?;
        // The home network's page, the link's hint for a guest who cannot reach the hub (and where
        // an app guest signals without it), and the same page on Tailscale (the Tailscale app's
        // address, when this computer has one).
        let all = bases(&self.console, &self.reach);
        let page = |b: &String| format!("{b}{}/join", crate::host_guard::base_path());
        let lan = all.first().map(page);
        let ts = crate::pad_link::tailscale_ip()
            .map(|ip| crate::pad_link::ip_base(ip, self.reach.port))
            .filter(|b| all.contains(b))
            .map(|b| page(&b));
        Some(room.json(&inv.key, lan.as_deref(), ts.as_deref()))
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

    /// The next guest's player number: the stream's controller when the host allows it
    /// ([`Settings::stream_seat`]) and `seat_free` says nobody on the host's side uses it, else a
    /// helper pad. Never more guests than the host allows.
    fn next_player(&self, tried: &[u8], seat_free: bool) -> Option<u8> {
        if self.guests_in() >= usize::from(self.settings.max_players.clamp(1, MAX_GUESTS)) {
            return None;
        }
        if seat_free
            && self.settings.stream_seat
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
            plays_as: None,
            seat: None,
            seat_note: None,
            seat_input_at: None,
            seat_move: SeatMove::No,
            slot_open: false,
            joined_at: now,
            stats: Arc::default(),
            net: None,
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
                "seat_taken" => Some("seat_taken"),
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
        // (One moving off it already let it go.)
        if g.player == STREAM_SEAT && g.seat_move == SeatMove::No {
            g.seat_input_at = None;
            fx.push(Effect::SeatPad(None));
            fx.push(Effect::Seat(false));
        }
        g.seat_move = SeatMove::No;
        let s = &g.stats;
        let n = s.rtt_n.load(Ordering::Relaxed);
        let avg = s.rtt_sum.load(Ordering::Relaxed).checked_div(n);
        fx.push(Effect::Log(format!(
            "join: Player {} left ({why}) after {}s: rtt avg {} ms, max {} ms; {} pictures skipped to a keyframe; {} pad frames; {}",
            g.player,
            now.duration_since(g.joined_at).as_secs(),
            avg.map(|a| a.to_string()).unwrap_or_else(|| "?".into()),
            s.rtt_max.load(Ordering::Relaxed),
            s.skipped.load(Ordering::Relaxed),
            s.pad_frames.load(Ordering::Relaxed),
            s.page_line(),
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

    /// The pad of `slot` was made anew (the helper restarted): seated in the host's choice again
    /// (`seat`), or the console asks the guest who they are again.
    fn reopened(&mut self, slot: u8, seat: &link::OpenResult, now: Instant) {
        if let Some(g) = self
            .guests
            .iter_mut()
            .find(|g| g.active() && g.player == slot)
        {
            g.pad_made(seat, None, now);
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
        if g.seat_move != SeatMove::No {
            return None; // between the stream's controller and their pad: nowhere to go yet
        }
        let strip = if self.settings.block_system_buttons && g.playing {
            vpad::button::SYSTEM
        } else {
            0
        };
        Some((g.player, strip))
    }

    /// The stream's controller goes back to the host's side, at once: the guest on it stops
    /// driving it, and moves to a helper pad of their own ([`move_off_seat`], now when their
    /// socket is up, else when they come back) or, with none free, is let go (`seat_taken`).
    fn give_back_seat(&mut self, why: &str) -> Vec<Effect> {
        let mut fx = Vec::new();
        let host_here = self.host_gone.is_none();
        let console = self.console.clone();
        for g in self.guests.iter_mut().filter(|g| g.on_seat()) {
            g.seat_input_at = None;
            g.playing = false;
            g.seat_move = if g.conn.is_some() && host_here {
                fx.push(Effect::MoveOff(g.id));
                SeatMove::Running
            } else {
                SeatMove::Wanted
            };
            fx.push(Effect::SeatPad(None));
            fx.push(Effect::Seat(false));
            fx.push(Effect::Log(format!(
                "join: the stream's controller on {console} goes back to the host ({why}); its guest moves to a pad of their own"
            )));
        }
        fx
    }

    /// Time passes; `live` is the host's running session on this console, if any. Returns what
    /// to do, and false once Play Together has closed.
    #[cfg(test)]
    pub(crate) fn tick(&mut self, now: Instant, live: Option<u64>) -> (Vec<Effect>, bool) {
        self.tick_seat(now, live, true)
    }

    /// [`Party::tick`], `seat_kept` saying the stream's controller is still free on the host's
    /// side ([`crate::rp_stream_api::stream_seat_kept`]): when it is not, a guest on it gives it
    /// back.
    pub(crate) fn tick_seat(
        &mut self,
        now: Instant,
        live: Option<u64>,
        seat_kept: bool,
    ) -> (Vec<Effect>, bool) {
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
                // A new session is the host's again: the stream's controller is not carried over
                // (their page attaches to it, #720).
                fx.extend(self.give_back_seat("a new session"));
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
        // The picture back on, a phone playing, or the host turned the seat off: it is theirs.
        if live.is_some() && (!seat_kept || !self.settings.stream_seat) {
            fx.extend(self.give_back_seat(if seat_kept {
                "the host turned it off"
            } else {
                "the host's side uses it"
            }));
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
        }
        // Held while a guest keeps it (also on a session that replaced the old one), and let go
        // when none does: a claim racing a give-back (the picture back on) heals here.
        if live.is_some() {
            fx.push(Effect::Seat(self.guests.iter().any(|g| g.on_seat())));
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
                    // On the stream's own controller: plays as the host's PS5 user.
                    "stream_seat": g.on_seat(),
                    // Seated in the profile the host chose ({id, name}), or null (the TV asks).
                    "plays_as": g.plays_as.as_ref().map(|(id, name)| serde_json::json!({"id": id, "name": name})),
                    // A chosen profile not given to their pad: why (the TV asks them instead).
                    "seat_note": g.seat_note,
                    // "lan", or "internet" with the connection's path ("direct", "failed", …)
                    // and ICE's own round trip.
                    "via": if g.net.is_some() { "internet" } else { "lan" },
                    "path": g.net.as_ref().map(|n| n.path()),
                    "ice_rtt_ms": g.net.as_ref().and_then(|n| n.rtt_ms()),
                    // An internet guest's connection now (round trip, rate, what waits to go).
                    "link": g.link_json(),
                    "key_asks": g.stats.key_asks.load(Ordering::Relaxed),
                    // What the guest's page says about its picture, and in a word ("smooth",
                    // "stutters", "poor"; null before its first report or from an older page).
                    "page": g.stats.page_json(now),
                    "quality": g.stats.quality(now),
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
            "settings": settings_view(&self.settings),
            "max_guests": MAX_GUESTS,
            // "Guests play as" on this console, and the helper's companion that binds them.
            "guest_profiles": seat_profiles(&self.console),
            "companion": self.companion,
            // Internet guests' connections, those not in yet (connecting, failed) too.
            "internet": self.internet.as_ref().map(|r| serde_json::json!({
                "room": r.id(),
                "peers": crate::join_net::peers_json(&r.id()),
            })),
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
            // On the stream's own controller: they play as the host's PS5 user, nothing to pick.
            "stream_seat": g.on_seat(),
            // The profile the host gave them (they play as it, nothing to pick), or null.
            "plays_as": g.plays_as.as_ref().map(|(_, name)| name),
            // The host chose a profile for them that could not be used: the TV asks who they are.
            "seat_note": g.seat_note,
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
                    "key_asks": s.key_asks.load(Ordering::Relaxed),
                    "via": if g.net.is_some() { "internet" } else { "lan" },
                    "link": g.link_json(),
                    "page": s.page_json(now),
                    "slot_open": g.slot_open,
                    "stream_seat": g.on_seat(),
                    "seat_move": format!("{:?}", g.seat_move),
                    // Seated in a chosen profile (never its name or id), or why not.
                    "seated": g.plays_as.is_some(),
                    "seat_note": g.seat_note,
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
            // How many players have a profile chosen (never which), and the companion's state.
            "seat_profiles": seat_profiles(&self.console).len(),
            "companion": self.companion,
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
            Effect::Log(s) => crate::log_at!(Info, "together", "{s}"),
            Effect::SeatPad(state) => crate::rp_stream_api::push_guest_pad(console, state),
            Effect::Seat(on) => crate::rp_stream_api::guest_seat(console, on),
            Effect::MoveOff(id) => match tokio::runtime::Handle::try_current() {
                Ok(h) => {
                    h.spawn(move_off_seat(console.to_string(), id));
                }
                Err(_) => {
                    // No runtime here: the guest waits for their next resume to be moved.
                    with_party(&host_key(console), |p| {
                        if let Some(g) = p.guest_mut(id) {
                            if g.seat_move == SeatMove::Running {
                                g.seat_move = SeatMove::Wanted;
                            }
                        }
                    });
                }
            },
        }
    }
}

/// The host's side needs the stream's controller now (their picture came back on): the guest on
/// it gives it back at once ([`Party::give_back_seat`]) instead of at the next look.
pub(crate) fn seat_needed(console: &str, why: &str) {
    let fx = with_party(&host_key(console), |p| p.give_back_seat(why)).unwrap_or_default();
    apply(console, fx);
}

/// What a move off the stream's controller tries next.
enum SeatStep {
    /// The guest is out (or no longer moving): nothing to do.
    Done,
    /// No helper pad is free for them.
    NoPad,
    /// Make Player `n`'s pad with this owner token.
    Try(u8, String),
}

/// A guest leaves the stream's controller for a helper pad of their own (the lowest free player,
/// skipping busy slots): the console asks them who they are, as for any guest. With none free,
/// or the helper unable, they are let go with `seat_taken`. True when they have a pad now.
async fn move_off_seat(console: String, id: u64) -> bool {
    let key = host_key(&console);
    let mut tried = Vec::new();
    loop {
        let step = with_party(&key, |p| {
            let free = p.free_player(&tried);
            let Some(g) = p.guest_mut(id).filter(|g| g.active()) else {
                return SeatStep::Done;
            };
            if g.seat_move != SeatMove::Running {
                return SeatStep::Done;
            }
            match free {
                // Taken now, so a guest let in meanwhile does not get the same one.
                Some(n) => {
                    g.player = n;
                    SeatStep::Try(n, g.pad_token.clone())
                }
                None => SeatStep::NoPad,
            }
        })
        .unwrap_or(SeatStep::Done);
        let (player, token) = match step {
            SeatStep::Done => return false,
            SeatStep::NoPad => break,
            SeatStep::Try(n, t) => (n, t),
        };
        let seat = seat_profile(&console, player);
        let r = open_slot(&console, player, &token, seat.as_ref()).await;
        if r == link::OpenResult::Busy {
            tried.push(player);
            continue;
        }
        if !r.is_ok() {
            break;
        }
        let now = Instant::now();
        let ok = with_party(&key, |p| {
            p.guest_mut(id)
                .filter(|g| g.active() && g.seat_move == SeatMove::Running)
                .map(|g| {
                    g.slot_open = true;
                    g.pad_made(&r, Some(seat.clone()), now);
                    g.seat_move = SeatMove::No;
                })
        })
        .flatten()
        .is_some();
        if !ok {
            // Out while the pad was made: nobody will close it later.
            link::send(&console, link::Cmd::Close { slot: player });
            return false;
        }
        crate::log_at!(
            Info,
            "together",
            "join: the guest who had the stream's controller on {console} is Player {player} now"
        );
        return true;
    }
    let now = Instant::now();
    let fx = with_party(&key, |p| {
        if let Some(g) = p.guest_mut(id).filter(|g| g.active()) {
            g.player = STREAM_SEAT; // the place they had, on the host's list
        }
        p.leave(id, "seat_taken", now)
    })
    .unwrap_or_default();
    apply(&console, fx);
    crate::log_at!(
        Info,
        "together",
        "join: no pad was free for the guest who had the stream's controller on {console}: let go"
    );
    false
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

#[cfg(test)]
static FAKE_SEAT_TAKEN: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);

/// The stream's controller on `console` is still free on the host's side (a guest may keep it).
fn seat_kept(console: &str) -> bool {
    #[cfg(test)]
    {
        let g = FAKE_SEAT_TAKEN.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().is_some_and(|s| s.contains(&host_key(console))) {
            return false;
        }
        if FAKE_LIVE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|m| m.contains_key(&host_key(console)))
        {
            return true;
        }
    }
    crate::rp_stream_api::stream_seat_kept(console)
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
        link::Event::Reopened { slot, seat } => {
            p.reopened(slot, &seat, now);
            if let link::OpenResult::Unseated(note) = seat {
                vec![Effect::Log(format!(
                    "join: Player {slot}'s pad was made again without the chosen profile ({note}): the console asks who they are"
                ))]
            } else {
                Vec::new()
            }
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
            let seat_kept = seat_kept(&console);
            let now = Instant::now();
            let r = with_parties(|m| {
                let p = m.get_mut(&key)?;
                let (fx, keep) = p.tick_seat(now, live, seat_kept);
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
            keep_companion(&key, &console, now);
        }
    });
}

/// How often an open Play Together with a profile chosen keeps the helper's companion up (each
/// ask lasts the helper a minute).
const COMPANION_EVERY: Duration = Duration::from_secs(20);

/// While Play Together is open on `console` and the host chose a profile for a guest's player,
/// the helper's companion is kept up, so a guest's pad is seated the moment it is made instead of
/// after the companion's cold start (~0.7 s, too late: the console asks within milliseconds).
/// Started when Play Together opens (or a profile is chosen), then every [`COMPANION_EVERY`].
fn keep_companion(key: &str, console: &str, now: Instant) {
    if seat_profiles(console).is_empty() {
        return;
    }
    let due = with_party(key, |p| {
        let due = p
            .companion_at
            .is_none_or(|t| now.duration_since(t) >= COMPANION_EVERY);
        if due {
            p.companion_at = Some(now);
        }
        due
    })
    .unwrap_or(false);
    if !due {
        return;
    }
    let (k, c) = (key.to_string(), console.to_string());
    tokio::spawn(async move {
        let c2 = c.clone();
        let r = tokio::task::spawn_blocking(move || link::port_for(&c2).companion(&c2)).await;
        let state = match r {
            Ok(Ok(Some(a))) if a.ok => a.companion,
            // A helper without the heartbeat (older) or without Play Together.
            Ok(Ok(_)) => "old_helper".to_string(),
            _ => "unreachable".to_string(),
        };
        let changed = with_party(&k, |p| {
            let changed = p.companion.as_deref() != Some(state.as_str());
            p.companion = Some(state.clone());
            changed
        })
        .unwrap_or(false);
        if changed {
            crate::log_at!(
                Info,
                "together",
                "join: the helper's pad-binding companion on {c}: {state}"
            );
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
pub(crate) fn connect_allowed(ip: IpAddr, now: Instant) -> bool {
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

/// Signalling sockets (`/api/join/signal`) per address per minute: an app guest opens one for
/// each way it tries (the home network, Tailscale) on every connect, kept apart from the guest
/// sockets' own limit.
static SIGNALS: ConnectLimit = ConnectLimit::new();
const SIGNALS_PER_MIN: usize = 30;

/// `room` is the internet room of an open Play Together here that still wants it.
pub(crate) fn internet_room_open(room: &str) -> bool {
    let keys: Vec<String> = with_parties(|m| {
        m.iter()
            .filter(|(_, p)| p.internet.as_ref().is_some_and(|r| r.id() == room))
            .map(|(k, _)| k.clone())
            .collect()
    });
    keys.iter().any(|k| internet_wanted(k))
}

/// `GET /api/join/signal`: an internet invite's signalling straight to this engine, for a guest
/// that reaches it on the home network or over Tailscale (join_net/local_signal.rs). The hub's
/// guest protocol, for this engine's own rooms only; the connection it sets up is the same one,
/// checked by the guest against the link's fingerprint.
pub async fn signal(
    upgrade: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
) -> Response {
    let ip = peer.ip();
    let limit = if ip.is_loopback() {
        LOOPBACK_CONNECTS_PER_MIN
    } else {
        SIGNALS_PER_MIN
    };
    if !SIGNALS.allow(ip, limit, Instant::now()) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "too many tries, wait a minute",
        )
            .into_response();
    }
    upgrade
        .max_message_size(crate::join_net::local_signal::FRAME_MAX)
        .max_frame_size(crate::join_net::local_signal::FRAME_MAX)
        .on_upgrade(|s| {
            crate::join_net::local_signal::session(s, internet_room_open, || {
                with_parties(|m| !m.is_empty())
            })
        })
}

#[derive(serde::Deserialize)]
pub struct RelayQuery {
    url: String,
}

/// What `/api/join/relay` may pass a socket to, and the largest message the guest's side sends
/// through: another engine's guest socket, or its signalling socket (an offer is several KiB).
pub(crate) fn relay_target(url: &str) -> Option<(String, usize)> {
    crate::pad_link::lan_ws_target(url, "api/join/ws")
        .map(|t| (t, GUEST_MSG_MAX))
        .or_else(|| {
            crate::pad_link::lan_ws_target(url, "api/join/signal")
                .map(|t| (t, crate::join_net::local_signal::FRAME_MAX))
        })
}

/// `GET /api/join/relay?url=ws://<host engine>:<port>/api/join/ws` (this computer only): an app
/// joining a friend's Play Together talks to its own engine (an app page may not open a plain
/// `ws://` to another machine), which passes the socket through unchanged. Only another engine's
/// `/api/join/ws`, or its `/api/join/signal` (an internet invite signalled without the hub), on a
/// private address.
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
    let Some((target, max)) = relay_target(&q.url) else {
        return upgrade.on_upgrade(|mut s| async move {
            let _ = s.send(refused("address")).await;
        });
    };
    // What the guest sends is held to a guest socket's limit (a signalling one's: the hub's); the
    // host's pictures coming back are read by the relay's own client socket, which keeps its
    // large one.
    upgrade
        .max_message_size(max)
        .max_frame_size(max)
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

/// What a guest session runs over: the LAN WebSocket (`/api/join/ws`), or an internet guest's
/// WebRTC data channel ([`crate::join_net`]), which carries the same messages. The session sees
/// WebSocket messages either way: text, binary, ping and pong, close.
pub(crate) trait GuestSocket: Send + 'static {
    /// Err when the guest is gone.
    fn send(&mut self, m: Message) -> impl std::future::Future<Output = Result<(), ()>> + Send;
    /// None (or an Err) when the guest is gone.
    fn recv(&mut self) -> impl std::future::Future<Output = Option<Result<Message, ()>>> + Send;
    /// An internet guest's connection, for the host's panel (None on the LAN).
    fn net(&self) -> Option<Arc<crate::join_net::PeerInfo>> {
        None
    }
    /// The session's pictures (`hevc`, at `stream_kbps`) go on an internet guest's RTP video
    /// track, which their page offered ([`Self::send_video`]); false: on this socket with the rest.
    fn video_track(&mut self, _hevc: bool, _stream_kbps: u32) -> bool {
        false
    }
    /// The sound on an Opus audio track beside the video track ([`Self::send_audio`]); false: on
    /// this socket.
    fn audio_track(&mut self) -> bool {
        false
    }
    /// A sound message (`[3][header]`, `[4][index][Opus]`) for the audio track.
    fn send_audio(
        &mut self,
        m: Message,
    ) -> impl std::future::Future<Output = Result<(), ()>> + Send {
        self.send(m)
    }
    /// A picture or the codec header for the video track.
    fn send_video(
        &mut self,
        m: Message,
    ) -> impl std::future::Future<Output = Result<(), ()>> + Send {
        self.send(m)
    }
}

impl GuestSocket for WebSocket {
    async fn send(&mut self, m: Message) -> Result<(), ()> {
        WebSocket::send(self, m).await.map_err(|_| ())
    }
    async fn recv(&mut self) -> Option<Result<Message, ()>> {
        WebSocket::recv(self).await.map(|r| r.map_err(|_| ()))
    }
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

async fn read_hello<S: GuestSocket>(socket: &mut S) -> Option<Hello> {
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
async fn closed<S: GuestSocket>(socket: &mut S) {
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

pub(crate) async fn guest_session<S: GuestSocket>(mut socket: S, ip: IpAddr) {
    let Some(hello) = read_hello(&mut socket).await else {
        let _ = socket.send(refused("hello")).await;
        return;
    };
    let name = clean_name(hello.name.as_deref());
    let t0 = Instant::now();
    // What the guest came with, never the credential itself.
    crate::log_at!(
        Debug,
        "together",
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
            crate::log_at!(
                Debug,
                "together",
                "join: guest {ip} ({name}) refused: {why} after {ms} ms"
            );
            let _ = socket.send(refused(why)).await;
            return;
        }
        Admit::Resume(c, id) => {
            crate::log_at!(
                Debug,
                "together",
                "join: guest {ip} ({name}) resumed on {c} after {ms} ms"
            );
            (c, id)
        }
        Admit::New(c, id) => {
            crate::log_at!(
                Debug,
                "together",
                "join: guest {ip} ({name}) let in on {c} after {ms} ms"
            );
            (c, id)
        }
    };
    play(socket, console, id, ip).await;
}

/// A guest coming back with `token`: None when the socket closed while waiting for the host.
async fn resume<S: GuestSocket>(socket: &mut S, token: &str) -> Option<Admit> {
    let found = with_parties(|m| {
        m.iter().find_map(|(k, p)| {
            p.by_token(token)
                .map(|id| (k.clone(), p.console.clone(), id))
        })
    });
    let Some((key, console, id)) = found else {
        crate::log_at!(
            Debug,
            "together",
            "join: a resume matched no guest (expired or never issued)"
        );
        return Some(Admit::Refused("expired"));
    };
    // While the host restarts Play, the guest waits here.
    if with_party(&key, |p| p.host_gone.is_some()).unwrap_or(false) {
        crate::log_at!(
            Debug,
            "together",
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
    // Coming back to the stream's controller: only while it is still free on the host's side
    // and the host still allows it (#720); else to a helper pad of their own.
    let kept = seat_kept(&console);
    let mv = with_party(&key, |p| {
        let allowed = p.settings.stream_seat;
        let g = p
            .guests
            .iter_mut()
            .find(|g| g.id == id && g.active() && g.player == STREAM_SEAT)?;
        let start =
            g.seat_move == SeatMove::Wanted || (g.seat_move == SeatMove::No && !(kept && allowed));
        if !start {
            return None;
        }
        let fx = if g.seat_move == SeatMove::No {
            g.seat_input_at = None;
            g.playing = false;
            vec![Effect::SeatPad(None), Effect::Seat(false)]
        } else {
            Vec::new()
        };
        g.seat_move = SeatMove::Running;
        Some(fx)
    })
    .flatten();
    if let Some(fx) = mv {
        apply(&console, fx);
        crate::log_at!(
            Debug,
            "together",
            "join: guest {id} resumes on {console}: the stream's controller is the host's, making a pad"
        );
        if !move_off_seat(console.clone(), id).await {
            return Some(Admit::Refused("seat_taken"));
        }
    } else {
        // A move started elsewhere (the tick, the picture coming on) still making their pad: wait
        // for it, so "joined" names the player they end up on, not Player 1.
        for _ in 0..150 {
            let running = with_party(&key, |p| {
                p.guests
                    .iter()
                    .any(|g| g.id == id && g.active() && g.seat_move == SeatMove::Running)
            })
            .unwrap_or(false);
            if !running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let Some((player, token, open, kept, had)) = with_party(&key, |p| {
        p.guests
            .iter()
            .find(|g| g.id == id && g.active())
            // A move off the stream's controller still running makes their pad itself.
            .map(|g| {
                (
                    g.player,
                    g.pad_token.clone(),
                    g.slot_open || g.seat_move != SeatMove::No,
                    g.slot_open && g.seat_move == SeatMove::No,
                    g.seat.clone(),
                )
            })
    })
    .flatten() else {
        return Some(Admit::Refused("expired"));
    };
    // The host may have chosen another profile for their player while they were away: their pad
    // is seated in the choice now (the link makes it again in that profile), not the old one.
    let want = seat_profile(&console, player);
    let rebind = kept && player != STREAM_SEAT && seat_changed(had.as_ref(), want.as_ref());
    if kept && !rebind {
        // The same profile: only its name may be new (it is the one shown).
        with_party(&key, |p| {
            if let Some(g) = p.guest_mut(id).filter(|g| g.active()) {
                g.seat_renamed(want.clone());
            }
        });
    }
    if (!open || rebind) && player != STREAM_SEAT {
        // The pad was removed while the host was away: make it again (the console asks who
        // they are again, unless the host chose a profile). Or seat it in the host's new choice.
        crate::log_at!(
            Debug,
            "together",
            "join: guest {id} resumes on {console}: {} Player {player}'s pad",
            if rebind {
                "seating again"
            } else {
                "making again"
            }
        );
        match open_slot(&console, player, &token, want.as_ref()).await {
            r if r.is_ok() => {
                let now = Instant::now();
                // Still in? A kick or a loss while the pad was made means nobody will close
                // it later, so it goes now.
                let ok = with_party(&key, |p| {
                    p.guest_mut(id).filter(|g| g.active()).map(|g| {
                        g.slot_open = true;
                        g.pad_made(&r, Some(want.clone()), now);
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

/// Makes Player `slot`'s pad for `token`, seated in the profile the host chose for that player
/// (if any: [`seat_bind`]). Only the host's choice is ever asked for, and the owner's profile only
/// as the host confirmed it; a seat the helper cannot give leaves the pad to the console's
/// question ([`link::OpenResult::Unseated`]).
async fn open_slot(
    console: &str,
    slot: u8,
    token: &str,
    seat: Option<&SeatProfile>,
) -> link::OpenResult {
    let (tx, rx) = oneshot::channel();
    let bind = seat.map(seat_bind_of);
    link::send(
        console,
        link::Cmd::Open {
            slot,
            token: token.to_string(),
            bind,
            reply: tx,
        },
    );
    let t0 = Instant::now();
    let r = match tokio::time::timeout(OPEN_WAIT, rx).await {
        Ok(Ok(r)) => r,
        _ => link::OpenResult::Failed("timeout".into()),
    };
    crate::log_at!(
        Debug,
        "together",
        "join: helper pad for Player {slot} on {console}: {r:?} in {} ms",
        t0.elapsed().as_millis()
    );
    match &r {
        link::OpenResult::Seated(_) => crate::log_at!(
            Info,
            "together",
            "join: Player {slot} on {console} plays as the profile the host chose"
        ),
        link::OpenResult::Unseated(note) => crate::log_at!(
            Info,
            "together",
            "join: Player {slot} on {console} could not get the profile the host chose ({note}): the TV asks who they are"
        ),
        _ => {}
    }
    if !r.is_ok() && r != link::OpenResult::Busy {
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
async fn ask<S: GuestSocket>(
    socket: &mut S,
    entry: Entry,
    name: &str,
    ip: IpAddr,
) -> Option<Admit> {
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
    crate::log_at!(Info, "together", "join: a guest asks to play on {console}");
    crate::log_at!(Debug, "together", "join: {name} asks to play on {console}");
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
    crate::log_at!(
        Debug,
        "together",
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
            crate::log_at!(
                Info,
                "together",
                "join: a guest has the stream's controller on {console}"
            );
            crate::log_at!(
                Debug,
                "together",
                "join: {name} has the stream's controller on {console}"
            );
            return Some(Admit::New(console, gid));
        }
        let seat = seat_profile(&console, player);
        let r = open_slot(&console, player, &pad_token, seat.as_ref()).await;
        let now = Instant::now();
        if r.is_ok() {
            let ok = with_party(&key, |p| {
                // Still in: a kick while the pad was made leaves nobody to close it.
                p.guest_mut(gid).filter(|g| g.active()).map(|g| {
                    g.slot_open = true;
                    // Seated in the host's choice: playing from the first frame.
                    g.pad_made(&r, Some(seat.clone()), now);
                    g.joined_at = now;
                })
            })
            .flatten()
            .is_some();
            if !ok {
                link::send(&console, link::Cmd::Close { slot: player });
                return Some(Admit::Refused("closed"));
            }
            crate::log_at!(
                Info,
                "together",
                "join: a guest is Player {player} on {console}"
            );
            crate::log_at!(
                Debug,
                "together",
                "join: {name} is Player {player} on {console}"
            );
            return Some(Admit::New(console, gid));
        }
        // Not made: this guest record goes (quietly: they never played).
        with_party(&key, |p| p.guests.retain(|g| g.id != gid));
        match r {
            link::OpenResult::Busy => {
                crate::log_at!(
                    Debug,
                    "together",
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
async fn play<S: GuestSocket>(mut socket: S, console: String, id: u64, ip: IpAddr) {
    static GEN: AtomicU64 = AtomicU64::new(1);
    let gen = GEN.fetch_add(1, Ordering::Relaxed);
    let key = host_key(&console);
    let (tx, mut rx) = mpsc::channel::<Out>(64);
    let net = socket.net();
    let internet = net.is_some();
    let Some((fx, player, token, stats)) = with_party(&key, |p| {
        let fx = p.attach(id, gen, tx);
        let g = p.guest_mut(id)?;
        g.ip = Some(ip);
        g.net = net;
        Some((fx, g.player, g.token.clone(), g.stats.clone()))
    })
    .flatten() else {
        let _ = socket.send(refused("closed")).await;
        return;
    };
    apply(&console, fx);
    let spect = SPECTATOR_BASE | gen;
    let feed = crate::rp_stream_api::add_player(
        spect,
        &console,
        PLAYER_QUEUE,
        stats.skipped.clone(),
        internet,
    );
    let hevc = feed.as_ref().map(|f| f.hevc).unwrap_or(false);
    // An internet guest whose page offered a video track gets the pictures there (join_net::rtc).
    let track = match &feed {
        Some(f) => socket.video_track(f.hevc, f.kbps),
        None => false,
    };
    // …and its sound beside them on an Opus audio track, when the page offered one too (#720).
    let audio = track && socket.audio_track();
    crate::log_at!(
        Debug,
        "together",
        "join: Player {player} on {console} connected (socket {gen}): picture {}, hevc {hevc}",
        match (&feed, track) {
            (None, _) => "none (no session)",
            (Some(_), true) if audio => "on the video track, sound on the audio track",
            (Some(_), true) => "on the video track",
            (Some(_), false) => "on",
        }
    );
    let (mut video, mut other) = match feed {
        Some(f) => (Some(f.video), Some(f.other)),
        None => (None, None),
    };
    let ended = socket
        .send(say(serde_json::json!({
            "stage": "joined", "player": player, "token": token, "hevc": hevc,
            // The pictures come on the video track (an internet page that offered one), not here.
            "track": track,
            // …and the sound on the audio track.
            "audio": audio,
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
                        /// The page's picture report (every 2 s at most), for the host's panel.
                        #[serde(default)] stats: Option<serde_json::Value>,
                    }
                    if let Ok(m) = serde_json::from_str::<In>(t.as_str()) {
                        if m.leave.is_some() {
                            left = true;
                            break;
                        }
                        // The guest's picture waits for a keyframe (#648): ask the console, at
                        // most once a KEY_ASK_GAP so a guest can't flood the host's stream.
                        if let Some(s) = &m.stats {
                            stats.page_report(s, Instant::now());
                        }
                        if m.keyframe {
                            if key_ask.take(Instant::now()) {
                                stats.key_asks.fetch_add(1, Ordering::Relaxed);
                                crate::log_at!(Debug, "together",
                                    "join: Player {player} on {console} asks for a keyframe ({} pictures skipped so far)",
                                    stats.skipped.load(Ordering::Relaxed)
                                );
                                crate::rp_stream_api::request_keyframe(&console, spect);
                            } else {
                                crate::log_at!(Debug, "together", "join: Player {player} on {console}: keyframe ask held back (one a second)");
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
                Some(m) => {
                    let sound = audio && matches!(&m, Message::Binary(b) if matches!(b.first(), Some(3 | 4)));
                    let sent = if sound { socket.send_audio(m).await } else { socket.send(m).await };
                    if sent.is_err() { break }
                }
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
                    let sent = if track { socket.send_video(m).await } else { socket.send(m).await };
                    if sent.is_err() { break }
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
    crate::log_at!(Debug, "together", 
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
    internet: Option<serde_json::Value>,
    now: Instant,
) -> serde_json::Value {
    let Some(inv) = inv else {
        return serde_json::Value::Null;
    };
    let prefix = crate::host_guard::base_path();
    // With this engine's DTLS fingerprint (`fp`): the page there joins over WebRTC signalled on
    // this engine's own socket (join_net/local_signal.rs) and checks the answer's certificate
    // against it, as an internet link's page does.
    let fp = crate::join_net::fingerprint()
        .map(|f| format!("&fp={f}"))
        .unwrap_or_default();
    let urls: Vec<String> = bases(console, reach)
        .iter()
        .map(|b| format!("{b}{prefix}/join#i={}{fp}", inv.key))
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
        // The one link to share (QR code and Copy): `share.url`, and where it works (`works`).
        "share": share_json(urls.first().map(String::as_str), internet.as_ref(), settings().internet_invites),
        // The internet invite, when the host made one: `url` (the link to share), `room`, `hub`,
        // `fp` (this engine's DTLS fingerprint), `state` on the hub, `offline` (the hub can't be
        // reached), `router` (the port the router mapped), `peers` (each internet guest's
        // connection).
        "internet": internet,
    })
}

/// The one link the host shares ("one invite"): the internet link, which works anywhere (on the
/// same Wi-Fi too: ICE keeps that connection on the home network, and the link names the home
/// network's page for a guest who cannot reach the hub), or the home network's own link when
/// there is no internet link: `works` "anywhere" or "wifi", and `why` it is the Wi-Fi's: "setting"
/// (Invites work: Only on my Wi-Fi), "offline" (the hub can't be reached: no internet here, or the
/// hub is down), "local" (none was asked for).
pub(crate) fn share_json(
    lan: Option<&str>,
    internet: Option<&serde_json::Value>,
    anywhere: bool,
) -> serde_json::Value {
    let net_url = internet
        .filter(|n| n["offline"] != true)
        .and_then(|n| n["url"].as_str());
    match net_url {
        Some(u) => serde_json::json!({"url": u, "works": "anywhere", "why": null}),
        None => serde_json::json!({
            "url": lan,
            "works": "wifi",
            "why": if !anywhere {
                "setting"
            } else if internet.is_some() {
                "offline"
            } else {
                "local"
            },
        }),
    }
}

fn state_json(console: &str) -> serde_json::Value {
    let key = host_key(console);
    let now = Instant::now();
    let snap = with_party(&key, |p| {
        let inv = p.current_invite(now);
        let net = p.internet_json(inv.as_ref());
        (
            p.console.clone(),
            p.reach.clone(),
            inv,
            net,
            p.snapshot(now, serde_json::Value::Null),
        )
    });
    match snap {
        Some((c, reach, inv, net, mut v)) => {
            v["invite"] = invite_json(&c, &reach, inv, net, now);
            // The next friend allowed in gets the stream's controller (Player 1, as the host's
            // PS5 user): the Allow prompt says so.
            let seat_next =
                v["settings"]["stream_seat"] == true && crate::rp_stream_api::stream_seat_free(&c);
            v["seat_next"] = seat_next.into();
            // Otherwise they get the next free pad, and the profile the host chose for it (if
            // any): the Allow prompt names it.
            v["next_profile"] = if seat_next {
                serde_json::Value::Null
            } else {
                with_party(&key, |p| p.free_player(&[]))
                    .flatten()
                    .and_then(|n| seat_profile(&c, n))
                    .map(|p| serde_json::json!({"player": p.player, "id": p.user, "name": p.name}))
                    .unwrap_or(serde_json::Value::Null)
            };
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
            "settings": settings_view(&settings()),
            "max_guests": MAX_GUESTS,
            "guest_profiles": seat_profiles(console),
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
        crate::log_at!(Info, "together", "join: Play Together open on {}", b.addr);
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
    /// Also an internet invite: a room on the signalling hub (`invite.internet.url`).
    #[serde(default)]
    internet: bool,
    /// The one link, as "Invites work" says: the internet invite when that is "anywhere" (and the
    /// hub address and certificate allow it), else the home network's; never refused for it.
    #[serde(default)]
    share: bool,
}

/// `POST /api/join/invite {addr, new?, internet?, share?}`: `{key, code, code_display, url, urls,
/// expires_in_s, share, internet}`. `share` is the one link to show (`share.url`, where it works:
/// `share.works` "anywhere" or "wifi", and `share.why` for the Wi-Fi: "setting", "offline" (the hub
/// can't be reached), "local"; see [`share_json`]). With `share` in the request the engine makes the
/// internet link when Settings › Play Together › Invites work says Anywhere, as `internet` does,
/// and quietly keeps to the home network's link otherwise. The same invite again until it expires, unless `new` (the old link and
/// code stop working). With `internet`, also a link for friends anywhere (`internet.url`): the
/// engine opens a room on the signalling hub in the settings and the guest's browser connects to
/// it peer to peer (409 `internet_off` when the settings turned internet invites off, 409
/// `hub_url` when the hub address is not a `wss://` one). The room stays open while the invite
/// lasts or an internet guest is in.
pub async fn invite(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<InviteBody>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    let s = settings();
    // The one link: the internet one when invites work anywhere and it can be made; otherwise the
    // home network's, never a refusal (`share` says which, and why).
    let internet = b.internet
        || (b.share
            && s.internet_invites
            && crate::join_net::hub_url_ok(&s.hub_url)
            && crate::join_net::fingerprint().is_some());
    if b.internet {
        if !s.internet_invites {
            return bad(StatusCode::CONFLICT, "internet_off");
        }
        if !crate::join_net::hub_url_ok(&s.hub_url) {
            return bad(StatusCode::CONFLICT, "hub_url");
        }
        if crate::join_net::fingerprint().is_none() {
            return bad(StatusCode::INTERNAL_SERVER_ERROR, "no_certificate");
        }
    }
    let now = Instant::now();
    touch_reach(&b.addr, &headers);
    let key = host_key(&b.addr);
    match with_party(&key, |p| {
        let inv = p.invite(b.new, now);
        if internet && inv.is_some() && p.internet.as_ref().is_none_or(|r| r.url() != s.hub_url) {
            let k = key.clone();
            p.internet = crate::join_net::hub::Room::start(
                crate::join_net::Kind::Join,
                Box::new(move || internet_wanted(&k)),
                s.hub_url.clone(),
            );
        }
        let net = p.internet_json(inv.as_ref());
        (inv, net, p.console.clone(), p.reach.clone())
    }) {
        None => bad(StatusCode::CONFLICT, "closed"),
        Some((None, ..)) => bad(StatusCode::INTERNAL_SERVER_ERROR, "no_random"),
        Some((inv, net, c, reach)) => Json(invite_json(&c, &reach, inv, net, now)).into_response(),
    }
}

/// The Play Together `key` still wants its internet room: open, with the invite current or an
/// internet guest still in (one coming back needs the hub to reconnect).
pub(crate) fn internet_wanted(key: &str) -> bool {
    let now = Instant::now();
    with_party(key, |p| {
        p.internet.is_some()
            && (p.current_invite(now).is_some()
                || p.guests.iter().any(|g| g.active() && g.net.is_some()))
    })
    .unwrap_or(false)
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

/// `GET /api/join/profiles?addr=`: the profiles signed in on that console (`users`: `{id, name,
/// foreground}`, `foreground` the one in front: the host's own), what the host chose for each
/// guest's player (`seats`: `{player, user, name, owner}`), and the helper's companion that seats
/// guests in them (`companion`: running, starting, no_bind, unavailable, off; null when not asked
/// yet). Only signed-in profiles can be chosen (502 `unreachable` when the console does not
/// answer).
pub async fn profiles(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<AddrQuery>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    let c = q.addr.clone();
    let users = match tokio::task::spawn_blocking(move || link::port_for(&c).users(&c)).await {
        Ok(Ok(u)) => u,
        Ok(Err(e)) => return bad(StatusCode::BAD_GATEWAY, &format!("unreachable: {e:#}")),
        Err(e) => return bad(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let companion = with_party(&host_key(&q.addr), |p| p.companion.clone()).flatten();
    Json(serde_json::json!({
        "users": users,
        "seats": seat_profiles(&q.addr),
        "companion": companion,
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
pub struct SettingsBody {
    /// The console `guest_profiles` are for (user ids are that console's).
    #[serde(default, alias = "console", alias = "host")]
    addr: Option<String>,
    /// "Guests play as": the profile for each guest's player on `addr` (`{player: 2-4, user,
    /// name, owner}`), replacing the last; empty: the TV asks every guest.
    #[serde(default)]
    guest_profiles: Option<Vec<SeatProfile>>,
    #[serde(default)]
    block_system_buttons: Option<bool>,
    #[serde(default)]
    max_players: Option<u8>,
    #[serde(default)]
    stream_seat: Option<bool>,
    #[serde(default)]
    internet_invites: Option<bool>,
    /// "Invites work": "anywhere" or "wifi" (the same choice as `internet_invites`; this one wins).
    #[serde(default)]
    invites: Option<String>,
    #[serde(default)]
    hub_url: Option<String>,
    #[serde(default)]
    stun: Option<Vec<String>>,
    #[serde(default)]
    upnp: Option<bool>,
    #[serde(default)]
    video_track: Option<bool>,
}

/// A STUN server as the settings take it: `host:port`, a name or an address, nothing else.
fn stun_server_ok(s: &str) -> bool {
    let Some((host, port)) = s.rsplit_once(':') else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    port.parse::<u16>().is_ok_and(|p| p > 0)
        && !host.is_empty()
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
}

/// `POST /api/join/settings {block_system_buttons?, max_players?, stream_seat?,
/// invites?, internet_invites?, hub_url?, stun?, upnp?, video_track?}`: the host's choices (every
/// console). Fewer players lets nobody go; it only stops new ones. `stream_seat` off moves a friend
/// on the stream's controller to a pad of their own. `invites` is "Invites work": "anywhere" (the
/// one link, through the hub) or "wifi" (the home network's link only; 400 `invites` otherwise),
/// the same choice as `internet_invites` on or off, and the one setting kept across restarts.
/// "wifi" closes the internet rooms (guests already in keep playing). `hub_url` is the signalling hub (`wss://…`; 400 `hub_url`
/// otherwise), `stun` the STUN servers (`host:port`, at most 8; empty: none), `upnp` whether the
/// router is asked for a port, `video_track` whether internet guests get the picture as a video
/// track (new connections; the data channel otherwise).
///
/// `guest_profiles` with `addr` ("Guests play as" on that console): each guest's player gets the
/// profile the host chose (`{player: 2-4, user, name, owner}`; a player left out: the TV asks).
/// Only signed-in profiles can be seated. The host's own profile (the one in front) needs `owner`
/// true, the host's confirmation that the guest may use its saves and trophies (409
/// `owner_confirm` otherwise; `owner_unknown` when the console does not say which one is in front,
/// for any profile newly chosen without `owner`); 400 `addr` without a console, 400 `player`, `user` or `duplicate`
/// for a list that is not one profile per player.
pub async fn set_settings(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<SettingsBody>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    let mut seats = None;
    if let Some(v) = b.guest_profiles {
        let Some(addr) = b.addr.clone().filter(|a| !a.trim().is_empty()) else {
            return bad(StatusCode::BAD_REQUEST, "addr");
        };
        let v = match clean_seat_profiles(v) {
            Ok(v) => v,
            Err(why) => return bad(StatusCode::BAD_REQUEST, why),
        };
        // The host's own profile only with their confirmation. The helper checks again as each pad
        // is made (the one in front may change).
        let c = addr.clone();
        let users = tokio::task::spawn_blocking(move || link::port_for(&c).users(&c))
            .await
            .ok()
            .and_then(Result::ok);
        if let Err(why) = owner_check(&v, users.as_deref(), &seat_profiles(&addr)) {
            return bad(StatusCode::CONFLICT, why);
        }
        seats = Some((addr, v));
    }
    let mut s = settings();
    if let Some(v) = b.block_system_buttons {
        s.block_system_buttons = v;
    }
    if let Some(v) = b.max_players {
        s.max_players = v.clamp(1, MAX_GUESTS);
    }
    if let Some(v) = b.stream_seat {
        s.stream_seat = v;
    }
    let was_anywhere = s.internet_invites;
    if let Some(v) = b.internet_invites {
        s.internet_invites = v;
    }
    match b.invites.as_deref() {
        None => {}
        Some("anywhere") => s.internet_invites = true,
        Some("wifi") => s.internet_invites = false,
        Some(_) => return bad(StatusCode::BAD_REQUEST, "invites"),
    }
    if let Some(v) = b.hub_url {
        let v = v.trim().to_string();
        if !crate::join_net::hub_url_ok(&v) {
            return bad(StatusCode::BAD_REQUEST, "hub_url");
        }
        s.hub_url = v;
    }
    if let Some(v) = b.stun {
        let v: Vec<String> = v
            .iter()
            .map(|x| x.trim().trim_start_matches("stun:").to_string())
            .filter(|x| !x.is_empty())
            .collect();
        if v.len() > 8 || v.iter().any(|x| !stun_server_ok(x)) {
            return bad(StatusCode::BAD_REQUEST, "stun");
        }
        s.stun = v;
    }
    if let Some(v) = b.upnp {
        s.upnp = v;
    }
    if let Some(v) = b.video_track {
        s.video_track = v;
    }
    // Everything checked: the profiles take effect with the rest (each pad made from now on).
    if let Some((addr, v)) = &seats {
        set_seat_profiles(addr, v.clone());
        crate::log_at!(
            Info,
            "together",
            "join: guests on {addr} play as {} chosen profile(s)",
            v.len()
        );
    }
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(s.clone());
    if (b.internet_invites.is_some() || b.invites.is_some()) && s.internet_invites != was_anywhere {
        save_invites(s.internet_invites);
    }
    let fx = with_parties(|m| {
        m.values_mut()
            .map(|p| {
                p.settings = s.clone();
                if !s.internet_invites {
                    p.internet = None;
                }
                let fx = if s.stream_seat {
                    Vec::new()
                } else {
                    p.give_back_seat("the host turned it off")
                };
                (p.console.clone(), fx)
            })
            .collect::<Vec<_>>()
    });
    for (console, fx) in fx {
        apply(&console, fx);
    }
    if !s.internet_invites {
        // The phone link's internet link too (phones already in keep playing).
        crate::pad_link::internet_off();
    }
    let mut out = serde_json::json!({ "settings": settings_view(&s) });
    if let Some(addr) = b.addr.as_deref().filter(|a| !a.trim().is_empty()) {
        out["guest_profiles"] = serde_json::json!(seat_profiles(addr));
    }
    Json(out).into_response()
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
        .route("/api/join/signal", get(signal))
        .route("/api/join", get(get_state))
        .route("/api/join/open", post(open))
        .route("/api/join/close", post(close))
        .route("/api/join/invite", post(invite))
        .route("/api/join/decide", post(decide))
        .route("/api/join/kick", post(kick))
        .route("/api/join/playing", post(playing))
        .route("/api/join/settings", post(set_settings))
        .route("/api/join/profiles", get(profiles))
}

// ── The helper link ──────────────────────────────────────────────────────────────────────────────

pub(crate) mod link {
    //! One thread per console sends the guests' pads to the helper (`vpad.*`).

    use std::collections::{BTreeMap, HashMap};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use anyhow::Result;
    use ps5upload_core::vpad::{
        self, bind_err, Ack, Bind, Companion, ConsoleUser, Opened, Output, PadState, StateItem,
    };
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
        /// `bind`: seat the pad in the user the host chose ([`vpad::Bind`]).
        fn open(
            &self,
            host: &str,
            slot: u8,
            token: &str,
            bind: Option<Bind>,
        ) -> Result<Option<Opened>>;
        fn states(&self, host: &str, items: &[StateItem]) -> Result<Option<Ack>>;
        fn hb(&self, host: &str, slot: u8, token: &str, live: bool) -> Result<Option<Ack>>;
        fn close(&self, host: &str, slot: u8, token: &str) -> Result<Option<Ack>>;
        fn output(&self, host: &str, slot: u8) -> Result<Option<Output>>;
        fn status(&self, host: &str) -> Result<Option<serde_json::Value>>;
        /// Keeps the helper's vinput companion up for binds (`vpad.hb {"companion": true}`).
        fn companion(&self, _host: &str) -> Result<Option<Companion>> {
            Ok(None)
        }
        /// The users signed in on the console.
        fn users(&self, _host: &str) -> Result<Vec<ConsoleUser>> {
            Ok(Vec::new())
        }
    }

    struct HelperPort;

    impl Port for HelperPort {
        fn open(
            &self,
            host: &str,
            slot: u8,
            token: &str,
            bind: Option<Bind>,
        ) -> Result<Option<Opened>> {
            vpad::open_with(host, slot, token, None, bind)
        }
        fn companion(&self, host: &str) -> Result<Option<Companion>> {
            vpad::companion(host)
        }
        fn users(&self, host: &str) -> Result<Vec<ConsoleUser>> {
            vpad::users(host)
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
        /// The pad is there (no profile was chosen for the player: the console asks who it is).
        Ok,
        /// The pad is there, seated in the profile the host chose (this user id): the console
        /// does not ask.
        Seated(u32),
        /// The pad is there, but the profile the host chose could not be given to it: the console
        /// asks who it is, as without a choice. Why: [`Unseated`] notes.
        Unseated(&'static str),
        /// Another owner holds the slot.
        Busy,
        /// The helper has no Play Together.
        OldHelper,
        Failed(String),
    }

    impl OpenResult {
        /// The pad is there.
        pub(crate) fn is_ok(&self) -> bool {
            matches!(
                self,
                OpenResult::Ok | OpenResult::Seated(_) | OpenResult::Unseated(_)
            )
        }

        /// The guest's refusal for it.
        pub(crate) fn why(&self) -> &'static str {
            match self {
                OpenResult::Ok | OpenResult::Seated(_) | OpenResult::Unseated(_) => "ok",
                OpenResult::Busy => "full",
                OpenResult::OldHelper => "old_helper",
                OpenResult::Failed(_) => "unreachable",
            }
        }
    }

    /// Why a chosen profile was not given to the pad ([`OpenResult::Unseated`]).
    pub(crate) mod unseated {
        /// The helper's companion was still starting after [`super::BIND_WAIT`].
        pub const STARTING: &str = "starting";
        /// The helper cannot bind (no companion, or its libSceMbus did not start).
        pub const UNAVAILABLE: &str = "unavailable";
        /// The profile is not signed in on the console.
        pub const SIGNED_OUT: &str = "signed_out";
        /// The console did not say who is signed in.
        pub const UNKNOWN: &str = "unknown";
        /// The profile is the one in front (the host's own) and the host did not confirm it.
        pub const OWNER: &str = "owner";
        /// The console did not take the bind.
        pub const FAILED: &str = "failed";
        /// The helper is too old to bind.
        pub const OLD_HELPER: &str = "old_helper";
    }

    /// The note for a seat the helper refused ([`bind_err`]).
    fn unseated_note(err: &str) -> &'static str {
        match err {
            bind_err::STARTING => unseated::STARTING,
            bind_err::SIGNED_OUT => unseated::SIGNED_OUT,
            bind_err::UNKNOWN => unseated::UNKNOWN,
            bind_err::OWNER => unseated::OWNER,
            _ => unseated::UNAVAILABLE,
        }
    }

    /// A seat the helper says is starting is asked again this often, for [`BIND_WAIT`] at most
    /// (then the pad is made without it: the console asks).
    const BIND_RETRY: Duration = Duration::from_millis(250);
    pub(crate) const BIND_WAIT: Duration = Duration::from_secs(if cfg!(test) { 1 } else { 3 });

    pub(crate) enum Cmd {
        /// Make (or keep) Player `slot`'s pad for `token`, seated in `bind`'s user when the host
        /// chose one.
        Open {
            slot: u8,
            token: String,
            bind: Option<Bind>,
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
        /// The pad was made anew (the helper restarted): `seat` says whether it is seated in the
        /// host's chosen profile again ([`OpenResult::Seated`]) or the console asks the guest who
        /// they are ([`OpenResult::Ok`], [`OpenResult::Unseated`]).
        Reopened { slot: u8, seat: OpenResult },
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
        /// The profile the host chose for this player: every open (a reopen too) asks for it.
        want: Option<Bind>,
        /// This open asks for `want` (false after the helper refused it: made without, the
        /// console asks).
        try_bind: bool,
        /// Why `want` was not given to the pad, while it is not.
        note: Option<&'static str>,
        /// The pad is seated in `want`'s user.
        seated: bool,
        /// A seat the helper said is starting is asked again until this.
        bind_until: Option<Instant>,
        /// The next open no sooner than this.
        open_at: Instant,
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
        fn new(token: String, want: Option<Bind>, now: Instant) -> Self {
            Slot {
                token,
                opened: false,
                handle: None,
                want,
                try_bind: want.is_some(),
                note: None,
                seated: false,
                bind_until: None,
                open_at: now,
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

        /// What an open of this pad answers.
        fn result(&self) -> OpenResult {
            match (self.want, self.seated, self.note) {
                (Some(b), true, _) => OpenResult::Seated(b.user),
                (Some(_), false, note) => OpenResult::Unseated(note.unwrap_or(unseated::FAILED)),
                (None, ..) => OpenResult::Ok,
            }
        }

        /// The next open asks for the chosen profile again (a reopen: the helper restarted).
        fn bind_again(&mut self) {
            self.try_bind = self.want.is_some();
            self.bind_until = None;
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
                Cmd::Open {
                    slot,
                    token,
                    bind,
                    reply,
                } => {
                    if let Some(s) = self.slots.get_mut(&slot) {
                        if s.token == token && s.want != bind {
                            // The same guest, another profile (the host changed it while they
                            // were away): the helper keeps a pad in the profile it was made for,
                            // so the pad goes and is made again in the new one.
                            if let Some(old) = self.slots.remove(&slot) {
                                for w in old.waiters {
                                    let _ = w.send(OpenResult::Failed("replaced".into()));
                                }
                                self.closing.push((slot, old.token));
                            }
                            let mut s = Slot::new(token, bind, now);
                            s.waiters.push(reply);
                            self.slots.insert(slot, s);
                            return;
                        }
                        if s.token == token {
                            if s.opened {
                                let _ = reply.send(s.result());
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
                    let mut s = Slot::new(token, bind, now);
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
                s.bind_again();
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
            let to_open: Vec<(u8, String, Option<Bind>)> = self
                .slots
                .iter()
                .filter(|(_, s)| !s.opened && s.open_at <= now)
                .map(|(k, s)| (*k, s.token.clone(), s.want.filter(|_| s.try_bind)))
                .collect();
            for (slot, token, bind) in to_open {
                self.calls += 1;
                let r = self.port.open(&self.host, slot, &token, bind);
                let Some(s) = self.slots.get_mut(&slot) else {
                    continue;
                };
                let first = !s.waiters.is_empty() || s.handle.is_none();
                match r {
                    Ok(Some(o)) if o.ok => {
                        // The seat: asked for and taken, asked for and not (the console asks),
                        // or not asked this time (refused before: the note says why).
                        if bind.is_some() {
                            s.seated = o.bound == Some(true);
                            s.note = match o.bound {
                                Some(true) => None,
                                Some(false) => Some(unseated::FAILED),
                                None => Some(unseated::OLD_HELPER),
                            };
                        } else {
                            s.seated = false;
                        }
                        s.bind_until = None;
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
                        let res = s.result();
                        let mut told = false;
                        for w in s.waiters.drain(..) {
                            told |= w.send(res.clone()).is_ok();
                        }
                        self.ok();
                        if waited && !told {
                            // made after its guest stopped waiting (a slow helper): nobody will
                            // play it, so it goes rather than being kept alive by heartbeats
                            self.remove(slot);
                            continue;
                        }
                        if reopened {
                            (self.events)(Event::Reopened { slot, seat: res });
                        }
                    }
                    Ok(Some(o))
                        if bind.is_some() && o.err.as_deref().is_some_and(bind_err::is_bind) =>
                    {
                        // The seat was refused with nothing made. Starting: asked again for a
                        // while. Otherwise (or too long): the pad is made without it (the console
                        // asks), at once.
                        let err = o.err.unwrap_or_default();
                        if err == bind_err::STARTING
                            && now < *s.bind_until.get_or_insert(now + BIND_WAIT)
                        {
                            s.open_at = now + BIND_RETRY;
                        } else {
                            s.try_bind = false;
                            s.seated = false;
                            s.note = Some(unseated_note(&err));
                            s.open_at = now;
                        }
                        self.ok();
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
                            s.bind_again();
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
                    s.bind_again();
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
                        min(s.open_at.max(now));
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
    use super::link::{unseated, Cmd, Event, LinkState, OpenResult, Port, SlotIo, HOLD};
    use super::*;

    #[test]
    fn a_guest_page_report_keeps_numbers_only_and_says_how_the_picture_looks() {
        let t0 = Instant::now();
        let s = Stats::default();
        assert_eq!(s.page_json(t0), serde_json::Value::Null);
        assert_eq!(s.quality(t0), None);
        // Without a frame count it is not a report; odd values are dropped or clamped.
        s.page_report(&serde_json::json!({"stalls": 3}), t0);
        assert_eq!(s.page_json(t0), serde_json::Value::Null);
        s.page_report(
            &serde_json::json!({
                "frames": 600, "dropped": -4, "late": "x", "decode_ms": 3.456, "stalls": 0,
                "stall_ms": 0, "decoder": "<script>", "name": "Ann"
            }),
            t0,
        );
        let v = s.page_json(t0);
        assert_eq!(v["frames"], 600);
        assert_eq!(v["dropped"], 0);
        assert_eq!(v["late"], 0);
        assert_eq!(v["decode_ms"], 3.5);
        assert_eq!(v["decoder"], serde_json::Value::Null);
        assert!(v.get("name").is_none());
        assert_eq!(s.quality(t0), Some("smooth"));
        // Another inside the gap is ignored.
        s.page_report(
            &serde_json::json!({"frames": 700, "stall_ms": 5000}),
            t0 + Duration::from_millis(500),
        );
        assert_eq!(s.page_json(t0)["frames"], 600);
        // Stalled 400 ms since the last: it stutters; 2 s more: poor; none: smooth again.
        let t1 = t0 + Duration::from_secs(2);
        s.page_report(
            &serde_json::json!({"frames": 700, "stalls": 2, "stall_ms": 400, "decoder": "webcodecs"}),
            t1,
        );
        assert_eq!(s.quality(t1), Some("stutters"));
        assert_eq!(s.page_json(t1)["decoder"], "webcodecs");
        let t2 = t1 + Duration::from_secs(2);
        s.page_report(&serde_json::json!({"frames": 720, "stall_ms": 2400}), t2);
        assert_eq!(s.quality(t2), Some("poor"));
        assert_eq!(s.page_json(t2)["stall_ms_recent"], 2000);
        let t3 = t2 + Duration::from_secs(2);
        s.page_report(&serde_json::json!({"frames": 840, "stall_ms": 2400}), t3);
        assert_eq!(s.quality(t3), Some("smooth"));
        // A new picture (a reconnect) starts its totals over: counted from zero, not negative.
        let t4 = t3 + Duration::from_secs(2);
        s.page_report(&serde_json::json!({"frames": 10, "stall_ms": 120}), t4);
        assert_eq!(s.page_json(t4)["stall_ms_recent"], 120);
        // A page that stopped reporting says nothing.
        assert_eq!(s.quality(t4 + Duration::from_secs(11)), None);
    }

    #[test]
    fn a_guest_page_that_never_drew_says_why_and_the_left_line_has_it() {
        // #720: an iPhone that showed no picture sent nothing, so the host's log could not say
        // which decoder failed.
        let t0 = Instant::now();
        let s = Stats::default();
        assert_eq!(s.page_line(), "the page never reported its picture");
        s.page_report(
            &serde_json::json!({
                "frames": 0, "decoder": "webcodecs", "problem": "decode", "codec": "avc1.640028"
            }),
            t0,
        );
        let v = s.page_json(t0);
        assert_eq!(v["problem"], "decode");
        assert_eq!(v["codec"], "avc1.640028");
        assert_eq!(
            s.page_line(),
            "picture webcodecs avc1.640028 (decode): 0 drawn, 0 stalls"
        );
        // Only known problems and plain codec strings are kept.
        let t1 = t0 + Duration::from_secs(2);
        s.page_report(
            &serde_json::json!({"frames": 5, "problem": "<b>", "codec": "avc1;<x>"}),
            t1,
        );
        let v = s.page_json(t1);
        assert_eq!(v["problem"], serde_json::Value::Null);
        assert_eq!(v["codec"], serde_json::Value::Null);
    }

    #[test]
    fn the_left_line_says_how_the_guests_sound_went() {
        // #720: an iPhone guest's sound stuttered and the host's log said nothing about it.
        let t0 = Instant::now();
        let s = Stats::default();
        s.page_report(
            &serde_json::json!({
                "frames": 300, "decoder": "track", "snd": "wasm", "snd_decoded": 900,
                "snd_concealed": 4, "snd_underruns": 12, "snd_dropped": 30
            }),
            t0,
        );
        assert_eq!(
            s.page_line(),
            "picture track ?: 300 drawn, 0 stalls; sound wasm: 900 decoded, 4 covered, 12 ran dry, 30 dropped"
        );
        assert_eq!(s.page_json(t0)["snd_underruns"], 12);
        let t1 = t0 + Duration::from_secs(2);
        s.page_report(
            &serde_json::json!({"frames": 10, "snd": "mse", "snd_jumps": 3}),
            t1,
        );
        assert!(s.page_line().ends_with("; sound mse: 3 jumps"));
        // An unknown path is not kept, and with it no sound part.
        let t2 = t1 + Duration::from_secs(2);
        s.page_report(
            &serde_json::json!({"frames": 20, "snd": "<x>", "snd_jumps": 3}),
            t2,
        );
        assert!(!s.page_line().contains("sound"));
    }

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
    use ps5upload_core::vpad::{
        bind_err, Ack, Bind, ConsoleUser, Opened, Output, SlotFailure, StateItem,
    };
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
        p.reopened(2, &OpenResult::Ok, t + PICKING_GRACE);
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
        assert!(!p.settings.stream_seat, "off unless the host turns it on");
        assert_eq!(
            p.next_player(&[], true),
            Some(2),
            "off: a friend gets their own controller even with it free"
        );
        p.settings.stream_seat = true;
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
        let mut p = seat_party();
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

    /// A party whose host allows the stream's controller.
    fn seat_party() -> Party {
        let mut p = party();
        p.settings.stream_seat = true;
        p
    }

    /// A guest on the stream's controller, playing (as `ask` leaves them), socket attached.
    fn seated(p: &mut Party, t: Instant) -> (u64, mpsc::Receiver<Out>) {
        let id = p.admit("a", STREAM_SEAT, t).unwrap().id;
        p.guest_mut(id).unwrap().playing = true;
        let (tx, rx) = conn();
        p.attach(id, 1, tx);
        (id, rx)
    }

    fn moves(fx: &[Effect]) -> Vec<u64> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::MoveOff(id) => Some(*id),
                _ => None,
            })
            .collect()
    }

    fn seat_claims(fx: &[Effect]) -> Vec<bool> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::Seat(on) => Some(*on),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_stream_seat_guest_has_ps_and_create_taken_when_the_block_is_on() {
        use ps5upload_core::rp_stream::button as rp;
        let t = Instant::now();
        let mut p = seat_party();
        assert!(p.settings.block_system_buttons);
        let (id, _rx) = seated(&mut p, t);
        let (slot, strip) = p.strip_mask(id).unwrap();
        assert_eq!(
            (slot, strip),
            (STREAM_SEAT, vpad::button::SYSTEM),
            "no user to pick: blocked from the start"
        );
        let mut st = PadState {
            buttons: vpad::button::PS | vpad::button::CREATE | vpad::button::CROSS,
            ..PadState::NEUTRAL
        };
        st.buttons &= !strip;
        assert_eq!(
            controller_state(&st).buttons,
            rp::CROSS,
            "PS and Create never reach the stream's controller"
        );
        p.settings.block_system_buttons = false;
        assert_eq!(p.strip_mask(id), Some((STREAM_SEAT, 0)));
    }

    #[test]
    fn the_stream_seat_goes_back_when_the_host_side_needs_it() {
        let t = Instant::now();
        let mut p = seat_party();
        let (id, _rx) = seated(&mut p, t);
        let s = p.session;
        let (fx, _) = p.tick_seat(t, Some(s), true);
        assert_eq!(seat_claims(&fx), vec![true], "free: they keep it");
        assert!(moves(&fx).is_empty());

        // The picture back on (or a phone playing): the host's controller plays again at once,
        // the guest moves to a pad of their own.
        let (fx, _) = p.tick_seat(t, Some(s), false);
        assert_eq!(moves(&fx), vec![id]);
        assert!(fx.iter().any(|e| matches!(e, Effect::SeatPad(None))));
        assert_eq!(
            seat_claims(&fx),
            vec![false, false],
            "let go, and not claimed again"
        );
        let g = p.guest_mut(id).unwrap();
        assert_eq!(g.seat_move, SeatMove::Running);
        assert!(!g.on_seat() && !g.playing);
        assert_eq!(
            p.strip_mask(id),
            None,
            "their input goes nowhere while moving"
        );
        assert_eq!(
            p.snapshot(t, serde_json::Value::Null)["guests"][0]["stream_seat"],
            false
        );
        // Once only.
        let (fx, _) = p.tick_seat(t, Some(s), false);
        assert!(moves(&fx).is_empty());
        assert_eq!(seat_claims(&fx), vec![false]);
    }

    #[test]
    fn the_stream_seat_goes_back_when_the_host_turns_it_off_or_restarts_play() {
        let t = Instant::now();
        let mut p = seat_party();
        let (id, _rx) = seated(&mut p, t);
        p.settings.stream_seat = false;
        let (fx, _) = p.tick_seat(t, Some(p.session), true);
        assert_eq!(moves(&fx), vec![id], "turned off: they move");

        // A new session (the host restarted Play): their socket goes, so the pad is made when
        // they come back (resume).
        let mut p = seat_party();
        let (id, _rx) = seated(&mut p, t);
        let (fx, _) = p.tick_seat(t, Some(p.session + 1), true);
        assert!(moves(&fx).is_empty());
        assert_eq!(byes(fx), vec!["host_stopped"]);
        let g = p.guest_mut(id).unwrap();
        assert_eq!(g.seat_move, SeatMove::Wanted);
        assert!(!g.on_seat());

        // A guest away when it is needed waits for their resume too.
        let mut p = seat_party();
        let (id, _rx) = seated(&mut p, t);
        p.detach(id, 1, t);
        let fx = p.give_back_seat("test");
        assert!(moves(&fx).is_empty());
        assert_eq!(p.guest_mut(id).unwrap().seat_move, SeatMove::Wanted);
        // Leaving then changes nothing on the stream's controller twice.
        let fx = p.leave(id, "left", t);
        assert!(seat_claims(&fx).is_empty());
    }

    #[test]
    fn a_guest_let_go_off_the_stream_seat_is_told_why() {
        let t = Instant::now();
        let mut p = seat_party();
        let (id, _rx) = seated(&mut p, t);
        let (tx, _rx2) = conn();
        p.attach(id, 2, tx);
        assert_eq!(byes(p.leave(id, "seat_taken", t)), vec!["seat_taken"]);
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
        /// "Guests play as", as the helper checks it: who is signed in, who is in front, how many
        /// seated opens answer "starting" first, whether the console refuses the bind
        signed_in: Mutex<Vec<u32>>,
        foreground: Mutex<u32>,
        bind_starting: std::sync::atomic::AtomicUsize,
        bind_fails: AtomicBool,
        /// every open's seat request, in order
        binds: Mutex<Vec<Option<Bind>>>,
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
        fn open(
            &self,
            _h: &str,
            slot: u8,
            _t: &str,
            bind: Option<Bind>,
        ) -> anyhow::Result<Option<Opened>> {
            self.up()?;
            let delay = *self.open_delay.lock().unwrap();
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            self.log.lock().unwrap().push(format!("open{slot}"));
            self.binds.lock().unwrap().push(bind);
            if self.old.load(Ordering::Relaxed) {
                return Ok(None);
            }
            // The helper's checks before a seated pad is made (vpad.c): refused, nothing made.
            if let Some(b) = bind {
                let refuse = |e: &str| {
                    Ok(Some(Opened {
                        err: Some(e.into()),
                        ..Default::default()
                    }))
                };
                let starting = self.bind_starting.load(Ordering::Relaxed);
                if starting > 0 {
                    self.bind_starting.store(starting - 1, Ordering::Relaxed);
                    return refuse(bind_err::STARTING);
                }
                if !self.signed_in.lock().unwrap().contains(&b.user) {
                    return refuse(bind_err::SIGNED_OUT);
                }
                if b.user == *self.foreground.lock().unwrap() && !b.owner {
                    return refuse(bind_err::OWNER);
                }
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
                bound: bind.map(|_| !self.bind_fails.load(Ordering::Relaxed)),
                ..Default::default()
            }))
        }
        fn users(&self, _h: &str) -> anyhow::Result<Vec<ConsoleUser>> {
            self.up()?;
            let fg = *self.foreground.lock().unwrap();
            Ok(self
                .signed_in
                .lock()
                .unwrap()
                .iter()
                .map(|&id| ConsoleUser {
                    id,
                    name: format!("user{id}"),
                    foreground: id == fg,
                })
                .collect())
        }
        fn companion(&self, _h: &str) -> anyhow::Result<Option<vpad::Companion>> {
            self.up()?;
            self.log.lock().unwrap().push("companion".into());
            Ok(Some(vpad::Companion {
                ok: true,
                companion: "running".into(),
                bind: true,
                err: None,
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
        open_bound_t(st, slot, token, None, t)
    }

    /// An open seated in `bind`'s user (the host's choice for that player).
    fn open_bound_t(
        st: &mut LinkState,
        slot: u8,
        token: &str,
        bind: Option<Bind>,
        t: Instant,
    ) -> oneshot::Receiver<OpenResult> {
        let (tx, rx) = oneshot::channel();
        st.on_cmd(
            Cmd::Open {
                slot,
                token: token.into(),
                bind,
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

    // ── "Guests play as" (the host's chosen profile per player) ──

    fn seat_helper(signed_in: &[u32], foreground: u32) -> Arc<FakeHelper> {
        let f = Arc::new(FakeHelper::default());
        *f.signed_in.lock().unwrap() = signed_in.to_vec();
        *f.foreground.lock().unwrap() = foreground;
        f
    }

    fn bind(user: u32, owner: bool) -> Option<Bind> {
        Some(Bind { user, owner })
    }

    #[test]
    fn a_chosen_profile_seats_the_pad_and_the_owners_only_when_the_host_confirmed_it() {
        let f = seat_helper(&[0x2000, 0x1000], 0x1000);
        let (mut st, _ev) = link_with(&f);
        let t = Instant::now();
        // a profile chosen for Player 2: seated, no question on the TV
        assert_eq!(
            open_bound_t(&mut st, 2, "a", bind(0x2000, false), t)
                .try_recv()
                .unwrap(),
            OpenResult::Seated(0x2000)
        );
        // the user in front (the host's own) without the host's confirmation: the helper refuses
        // it, and the pad is made without any profile (the console asks), never as the owner
        let mut rx = open_bound_t(&mut st, 3, "b", bind(0x1000, false), t);
        assert!(
            rx.try_recv().is_err(),
            "not answered before the pad is made"
        );
        st.tick(t);
        assert_eq!(
            rx.try_recv().unwrap(),
            OpenResult::Unseated(unseated::OWNER)
        );
        // confirmed by the host: seated
        assert_eq!(
            open_bound_t(&mut st, 4, "c", bind(0x1000, true), t)
                .try_recv()
                .unwrap(),
            OpenResult::Seated(0x1000)
        );
        // the same owner with the same choice gets what its pad has: nothing asked again
        assert_eq!(
            open_bound_t(&mut st, 2, "a", bind(0x2000, false), t)
                .try_recv()
                .unwrap(),
            OpenResult::Seated(0x2000),
            "the same owner again gets what its pad has"
        );
        let asked = f.binds.lock().unwrap().clone();
        assert_eq!(
            asked,
            vec![
                bind(0x2000, false),
                bind(0x1000, false),
                None, // the refused seat: the pad made without one
                bind(0x1000, true),
            ]
        );
    }

    /// A guest coming back after the host chose another profile for their player: the same owner
    /// asks with the new choice, and its pad goes and is made again in that profile (the helper
    /// keeps a pad in the profile it was made for).
    #[test]
    fn a_resume_with_another_choice_seats_the_pad_again() {
        let f = seat_helper(&[0x2000, 0x3000, 0x1000], 0x1000);
        let (mut st, _ev) = link_with(&f);
        let t = Instant::now();
        assert_eq!(
            open_bound_t(&mut st, 2, "a", bind(0x2000, false), t)
                .try_recv()
                .unwrap(),
            OpenResult::Seated(0x2000)
        );
        let mut rx = open_bound_t(&mut st, 2, "a", bind(0x3000, false), t);
        st.tick(t);
        assert_eq!(rx.try_recv().unwrap(), OpenResult::Seated(0x3000));
        let log = f.log();
        assert_eq!(log, vec!["open2", "close2", "open2"], "{log:?}");
        // the choice taken away: made again without one (the TV asks)
        let mut rx = open_bound_t(&mut st, 2, "a", None, t);
        st.tick(t);
        assert_eq!(rx.try_recv().unwrap(), OpenResult::Ok);
        assert_eq!(
            f.binds.lock().unwrap().clone(),
            vec![bind(0x2000, false), bind(0x3000, false), None]
        );
    }

    #[test]
    fn the_owner_check_asks_when_the_console_does_not_say_who_is_in_front() {
        let seat = |player: u8, user: u32, owner: bool| SeatProfile {
            player,
            user,
            name: String::new(),
            owner,
        };
        let users = |front: Option<u32>| {
            [0x1000u32, 0x2000]
                .iter()
                .map(|&id| vpad::ConsoleUser {
                    id,
                    name: String::new(),
                    foreground: Some(id) == front,
                })
                .collect::<Vec<_>>()
        };
        let known = users(Some(0x1000));
        // the one in front needs the host's yes; another profile does not
        assert_eq!(
            owner_check(&[seat(2, 0x1000, false)], Some(&known), &[]),
            Err("owner_confirm")
        );
        assert_eq!(
            owner_check(&[seat(2, 0x1000, true)], Some(&known), &[]),
            Ok(())
        );
        assert_eq!(
            owner_check(&[seat(2, 0x2000, false)], Some(&known), &[]),
            Ok(())
        );
        // the console did not answer, or named nobody in front: any new choice may be the
        // host's own, so it needs the same yes
        for users in [None, Some(users(None))] {
            assert_eq!(
                owner_check(&[seat(2, 0x2000, false)], users.as_deref(), &[]),
                Err("owner_unknown")
            );
            assert_eq!(
                owner_check(&[seat(2, 0x2000, true)], users.as_deref(), &[]),
                Ok(())
            );
            // a choice kept as it was keeps the answer it had
            assert_eq!(
                owner_check(
                    &[seat(2, 0x2000, false), seat(3, 0x1000, true)],
                    users.as_deref(),
                    &[seat(2, 0x2000, false)]
                ),
                Ok(())
            );
            assert_eq!(owner_check(&[], users.as_deref(), &[]), Ok(()));
        }
    }

    #[test]
    fn a_seat_that_cannot_be_given_leaves_the_pad_to_the_tv() {
        let t = Instant::now();
        // not signed in
        let f = seat_helper(&[0x2000], 0x1000);
        let (mut st, _ev) = link_with(&f);
        let mut rx = open_bound_t(&mut st, 2, "a", bind(0x3000, false), t);
        st.tick(t);
        assert_eq!(
            rx.try_recv().unwrap(),
            OpenResult::Unseated(unseated::SIGNED_OUT)
        );
        // the console refuses the bind: the pad stays, the TV asks
        let f = seat_helper(&[0x2000], 0x1000);
        f.bind_fails.store(true, Ordering::Relaxed);
        let (mut st, _ev) = link_with(&f);
        assert_eq!(
            open_bound_t(&mut st, 2, "a", bind(0x2000, false), t)
                .try_recv()
                .unwrap(),
            OpenResult::Unseated(unseated::FAILED)
        );
        // an older helper ignores the request: made, not seated
        let f = Arc::new(OldBindHelper(seat_helper(&[0x2000], 0x1000)));
        let ev: Events = Arc::default();
        let e2 = ev.clone();
        let mut st = LinkState::new("c", f, Arc::new(move |e| e2.lock().unwrap().push(e)));
        assert_eq!(
            open_bound_t(&mut st, 2, "a", bind(0x2000, false), t)
                .try_recv()
                .unwrap(),
            OpenResult::Unseated(unseated::OLD_HELPER)
        );
    }

    /// A helper from before "Guests play as": `bind_user` is not read, `bound` never answered.
    struct OldBindHelper(Arc<FakeHelper>);
    impl Port for OldBindHelper {
        fn open(
            &self,
            h: &str,
            slot: u8,
            t: &str,
            _bind: Option<Bind>,
        ) -> anyhow::Result<Option<Opened>> {
            self.0.open(h, slot, t, None)
        }
        fn states(&self, h: &str, items: &[StateItem]) -> anyhow::Result<Option<Ack>> {
            Port::states(&*self.0, h, items)
        }
        fn hb(&self, h: &str, slot: u8, t: &str, live: bool) -> anyhow::Result<Option<Ack>> {
            self.0.hb(h, slot, t, live)
        }
        fn close(&self, h: &str, slot: u8, t: &str) -> anyhow::Result<Option<Ack>> {
            self.0.close(h, slot, t)
        }
        fn output(&self, h: &str, slot: u8) -> anyhow::Result<Option<Output>> {
            self.0.output(h, slot)
        }
        fn status(&self, h: &str) -> anyhow::Result<Option<serde_json::Value>> {
            self.0.status(h)
        }
    }

    #[test]
    fn a_starting_companion_is_asked_again_for_a_while_then_the_tv_asks() {
        let t = Instant::now();
        let f = seat_helper(&[0x2000], 0x1000);
        f.bind_starting.store(2, Ordering::Relaxed);
        let (mut st, _ev) = link_with(&f);
        let mut rx = open_bound_t(&mut st, 2, "a", bind(0x2000, false), t);
        // not asked again before its time
        st.tick(t + Duration::from_millis(100));
        assert_eq!(f.binds.lock().unwrap().len(), 1);
        assert!(st.wait(t) > Duration::ZERO, "no busy loop while it starts");
        st.tick(t + Duration::from_millis(250));
        st.tick(t + Duration::from_millis(500));
        assert_eq!(rx.try_recv().unwrap(), OpenResult::Seated(0x2000));
        // one that never comes up: after BIND_WAIT, the pad is made without it
        let f = seat_helper(&[0x2000], 0x1000);
        f.bind_starting.store(1000, Ordering::Relaxed);
        let (mut st, _ev) = link_with(&f);
        let mut rx = open_bound_t(&mut st, 2, "a", bind(0x2000, false), t);
        let mut now = t;
        while now < t + link::BIND_WAIT + Duration::from_millis(600) {
            now += Duration::from_millis(250);
            st.tick(now);
        }
        assert_eq!(
            rx.try_recv().unwrap(),
            OpenResult::Unseated(unseated::STARTING)
        );
        assert_eq!(f.binds.lock().unwrap().last().unwrap(), &None);
    }

    #[test]
    fn a_restarted_helper_seats_the_pad_again() {
        let f = seat_helper(&[0x2000], 0x1000);
        let (mut st, ev) = link_with(&f);
        let t = Instant::now();
        open_bound_t(&mut st, 2, "a", bind(0x2000, false), t);
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
        st.tick(t + Duration::from_secs(5));
        let e = ev.lock().unwrap();
        assert!(
            e.contains(&Event::Reopened {
                slot: 2,
                seat: OpenResult::Seated(0x2000)
            }),
            "{e:?}"
        );
        assert_eq!(
            f.binds.lock().unwrap().last().unwrap(),
            &bind(0x2000, false)
        );
    }

    #[test]
    fn a_seated_guest_is_playing_at_once_and_one_not_seated_picks_a_user() {
        let mut p = party();
        let t = Instant::now();
        let id = admit(&mut p, "Sam", t);
        p.guest_mut(id)
            .unwrap()
            .pad_made(&OpenResult::Seated(0x2000), Some(None), t);
        // PS and Create taken from the first frame: no picking window
        assert_eq!(p.strip_mask(id), Some((2, vpad::button::SYSTEM)));
        assert_eq!(p.guest_status(id).unwrap()["status"]["block_system"], true);
        // a reopen not seated again: the TV asks, PS goes through while they pick
        p.reopened(2, &OpenResult::Unseated(unseated::FAILED), t);
        assert_eq!(p.strip_mask(id), Some((2, 0)));
        let s = p.guest_status(id).unwrap();
        assert_eq!(s["status"]["seat_note"], "failed");
        assert!(s["status"]["plays_as"].is_null());
        // seated again on the next reopen
        p.reopened(2, &OpenResult::Seated(0x2000), t);
        assert_eq!(p.strip_mask(id), Some((2, vpad::button::SYSTEM)));
        // no choice at all: as before
        p.reopened(2, &OpenResult::Ok, t);
        assert_eq!(p.strip_mask(id), Some((2, 0)));
        assert!(p.guest_status(id).unwrap()["status"]["seat_note"].is_null());
    }

    #[test]
    fn the_hosts_choices_are_one_profile_per_player() {
        let sp = |player, user| SeatProfile {
            player,
            user,
            name: " Guest\u{7}1 ".into(),
            owner: false,
        };
        assert_eq!(clean_seat_profiles(vec![sp(1, 5)]), Err("player"));
        assert_eq!(clean_seat_profiles(vec![sp(5, 5)]), Err("player"));
        assert_eq!(clean_seat_profiles(vec![sp(2, 0)]), Err("user"));
        assert_eq!(clean_seat_profiles(vec![sp(2, u32::MAX)]), Err("user"));
        assert_eq!(
            clean_seat_profiles(vec![sp(2, 5), sp(2, 6)]),
            Err("duplicate")
        );
        assert_eq!(
            clean_seat_profiles(vec![sp(2, 5), sp(3, 5)]),
            Err("duplicate")
        );
        let v = clean_seat_profiles(vec![sp(4, 7), sp(2, 0x9000_0001)]).unwrap();
        assert_eq!((v[0].player, v[1].player), (2, 4));
        assert_eq!(v[0].name, "Guest1");
        // only the host's choice is ever asked for, as they set it
        let console = "10.77.0.31";
        set_seat_profiles(console, v);
        assert_eq!(seat_bind(console, 2), bind(0x9000_0001, false));
        assert_eq!(seat_bind(console, 3), None);
        set_seat_profiles(console, Vec::new());
        assert_eq!(seat_bind(console, 2), None);
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
            bind: None,
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
                bind: None,
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
        let r = open_slot(console, 2, "0123456789abcdef0123456789abcdef", None).await;
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

    fn seat_settings() -> Settings {
        Settings {
            stream_seat: true,
            ..Settings::default()
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_guest_off_the_stream_seat_gets_a_pad_or_is_let_go() {
        let _turn = PARTY_TESTS.lock().await;
        let console = "10.77.0.31";
        let key = host_key(console);
        let f = Arc::new(FakeHelper::default());
        f.busy.lock().unwrap().push(2);
        set_helper(console, f.clone());
        let t = Instant::now();
        let mut p = Party::new(console, 1, "Desk".into(), seat_settings());
        let (a, _ra) = seated(&mut p, t);
        assert_eq!(moves(&p.give_back_seat("test")), vec![a]);
        with_parties(|m| m.insert(key.clone(), p));

        // Player 2 is held elsewhere: the next free one.
        assert!(move_off_seat(console.into(), a).await);
        let got = with_party(&key, |p| {
            let g = p.guest_mut(a).unwrap();
            (g.player, g.slot_open, g.seat_move, g.state())
        })
        .unwrap();
        assert_eq!(got, (3, true, SeatMove::No, "picking_user"));
        assert_eq!(
            with_party(&key, |p| p.strip_mask(a)).unwrap(),
            Some((3, 0)),
            "PS answers 'Who's using this controller?'"
        );
        assert!(f.log().contains(&"open3".to_string()), "{:?}", f.log());

        // No pad free (two guests at most, 2 busy, 3 is theirs): let go, and told why.
        let (b, mut rb) = with_party(&key, |p| {
            p.settings.max_players = 2;
            let (b, rb) = seated(p, t);
            p.give_back_seat("test");
            (b, rb)
        })
        .unwrap();
        assert!(!move_off_seat(console.into(), b).await);
        let why = with_party(&key, |p| p.guest_mut(b).unwrap().left.map(|(_, w)| w)).unwrap();
        assert_eq!(why, Some("seat_taken"));
        let mut byes_b = Vec::new();
        while let Ok(o) = rb.try_recv() {
            if let Out::Bye(w) = o {
                byes_b.push(w);
            }
        }
        assert_eq!(byes_b, vec!["seat_taken"]);
        with_parties(|m| m.remove(&key));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_guest_resuming_to_a_stream_seat_the_host_needs_gets_a_pad() {
        let _turn = PARTY_TESTS.lock().await;
        let console = "10.77.0.32";
        let key = host_key(console);
        let f = Arc::new(FakeHelper::default());
        set_helper(console, f.clone());
        set_live(console, 1);
        // The host's side uses the stream's controller now (picture on, or a phone playing).
        FAKE_SEAT_TAKEN
            .lock()
            .unwrap()
            .get_or_insert_with(Default::default)
            .insert(key.clone());
        let url = serve().await;
        let t = Instant::now();
        let mut p = Party::new(console, 1, "Desk".into(), seat_settings());
        let away = |p: &mut Party| {
            let id = p.admit("a", STREAM_SEAT, t).unwrap().id;
            let g = p.guest_mut(id).unwrap();
            g.playing = true;
            (id, g.token.clone())
        };
        let (a, token) = away(&mut p);
        with_parties(|m| m.insert(key.clone(), p));

        let mut g = guest(&url, serde_json::json!({"hello": {"resume": token}})).await;
        let j = next(&mut g).await;
        assert_eq!(j["stage"], "joined", "{j}");
        assert_eq!(
            j["player"], 2,
            "a pad of their own, not the host's controller"
        );
        assert!(f.log().contains(&"open2".to_string()));
        let st = with_party(&key, |p| p.guest_mut(a).unwrap().state()).unwrap();
        assert_eq!(st, "picking_user", "they pick a user on the TV now");

        // With no pad free, the resume is refused with the reason.
        f.busy.lock().unwrap().extend([3, 4]);
        let (_b, token_b) = with_party(&key, |p| away(p)).unwrap();
        let mut g2 = guest(&url, serde_json::json!({"hello": {"resume": token_b}})).await;
        assert_eq!(next(&mut g2).await["why"], "seat_taken");

        drop(g);
        with_parties(|m| m.remove(&key));
        FAKE_SEAT_TAKEN
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .remove(&key);
    }

    /// "Guests play as" end to end: the host's own profile needs their confirmation, a guest on a
    /// player with a chosen profile is seated in it with PS and Create taken from the first frame
    /// (no 60 s picking window, no "They're in"), the companion is kept up while Play Together is
    /// open, and a profile that cannot be used leaves the guest to the TV's question, said to the
    /// host and to them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_guest_plays_as_the_profile_the_host_chose() {
        let _turn = PARTY_TESTS.lock().await;
        *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings {
            block_system_buttons: true,
            ..Settings::default()
        });
        let console = "10.77.0.41";
        let f = seat_helper(&[0x2000, 0x1000], 0x1000);
        set_helper(console, f.clone());
        let url = serve().await;
        set_live(console, 1);
        async fn choose(console: &str, v: serde_json::Value) -> Response {
            let (ci, h) = local();
            set_settings(
                ci,
                h,
                Json(SettingsBody {
                    addr: Some(console.into()),
                    guest_profiles: Some(serde_json::from_value(v).unwrap()),
                    block_system_buttons: None,
                    max_players: None,
                    stream_seat: None,
                    internet_invites: None,
                    invites: None,
                    hub_url: None,
                    stun: None,
                    upnp: None,
                    video_track: None,
                }),
            )
            .await
        }
        // The host's own profile (in front) without their confirmation: refused.
        let r = choose(
            console,
            serde_json::json!([{"player": 2, "user": 0x1000, "name": "Me"}]),
        )
        .await;
        assert_eq!(r.status(), StatusCode::CONFLICT);
        assert_eq!(body(r).await["error"], "owner_confirm");
        assert!(seat_profiles(console).is_empty());
        // Player 2 as "Guest 1", Player 3 as a profile that is not signed in.
        let r = body(
            choose(
                console,
                serde_json::json!([
                    {"player": 2, "user": 0x2000, "name": "Guest 1"},
                    {"player": 3, "user": 0x3000, "name": "Away"},
                ]),
            )
            .await,
        )
        .await;
        assert_eq!(r["guest_profiles"][0]["name"], "Guest 1");
        // The settings page lists the signed-in profiles, the one in front marked.
        let (ci, h) = local();
        let p = body(
            profiles(
                ci,
                h,
                Query(AddrQuery {
                    addr: console.into(),
                }),
            )
            .await,
        )
        .await;
        assert_eq!(p["users"].as_array().unwrap().len(), 2);
        assert_eq!(p["users"][1]["foreground"], true);
        assert_eq!(p["seats"][1]["player"], 3);

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
        assert_eq!(
            s["next_profile"]["name"], "Guest 1",
            "the Allow prompt names it"
        );
        // The companion is kept up while Play Together is open with a profile chosen.
        until("the companion asked", || {
            f.log().contains(&"companion".to_string())
        })
        .await;

        // Sam is Player 2, seated in Guest 1: PS is taken from the very first frame.
        let mut sam = guest(
            &url,
            serde_json::json!({"hello": {"invite": key, "name": "Sam"}}),
        )
        .await;
        assert_eq!(next(&mut sam).await["stage"], "asking");
        allow_next(console, true).await;
        let j = next(&mut sam).await;
        assert_eq!((&j["stage"], &j["player"]), (&"joined".into(), &2.into()));
        assert_eq!(f.binds.lock().unwrap()[0], bind(0x2000, false));
        use vpad::{app_button as app, button as pad};
        sam.send(TMsg::Binary(
            frame(1, app::PS | app::CROSS, &[], None).into(),
        ))
        .await
        .unwrap();
        until("PS stripped at once", || {
            f.states()
                .iter()
                .any(|s| s.slot == 2 && s.state.buttons & pad::CROSS != 0)
        })
        .await;
        assert!(!f.states().iter().any(|s| s.state.buttons & pad::PS != 0));
        let s = host_state(console).await;
        let g = &s["guests"][0];
        assert_eq!(
            (&g["state"], &g["plays_as"]["name"]),
            (&"playing".into(), &"Guest 1".into())
        );

        // Ann is Player 3, whose profile is not signed in: her pad is made without it (never as
        // the foreground user), the TV asks her, and both sides are told.
        let mut ann = guest(
            &url,
            serde_json::json!({"hello": {"invite": key, "name": "Ann"}}),
        )
        .await;
        assert_eq!(next(&mut ann).await["stage"], "asking");
        allow_next(console, true).await;
        assert_eq!(next(&mut ann).await["player"], 3);
        let binds = f.binds.lock().unwrap().clone();
        assert_eq!(&binds[1..], &[bind(0x3000, false), None]);
        let s = host_state(console).await;
        let g = s["guests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["player"] == 3)
            .unwrap()
            .clone();
        assert_eq!(
            (&g["state"], &g["seat_note"]),
            (&"picking_user".into(), &"signed_out".into())
        );
        assert!(g["plays_as"].is_null());
        let st = with_party(&host_key(console), |p| {
            p.guest_status(g["id"].as_u64().unwrap())
        })
        .flatten()
        .unwrap();
        assert_eq!(st["status"]["seat_note"], "signed_out");
        assert_eq!(
            st["status"]["block_system"], false,
            "PS answers the TV's question"
        );
        ann.send(TMsg::Binary(frame(1, app::PS, &[], None).into()))
            .await
            .unwrap();
        until("PS reaches the TV", || {
            f.states()
                .iter()
                .any(|s| s.slot == 3 && s.state.buttons & pad::PS != 0)
        })
        .await;

        drop((sam, ann));
        let (ci, h) = local();
        close(
            ci,
            h,
            Json(AddrBody {
                addr: console.into(),
            }),
        )
        .await;
        set_seat_profiles(console, Vec::new());
        *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
        set_live(console, 0);
    }

    /// A guest who drops and comes back with their token after the host chose another profile for
    /// their player: their pad is seated in the new one (made again), and its name is the one
    /// shown, never blank. With the same choice, the pad they had is kept as it was.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_resume_takes_the_profile_the_host_chose_meanwhile() {
        let _turn = PARTY_TESTS.lock().await;
        let console = "10.77.0.42";
        let f = seat_helper(&[0x2000, 0x3000, 0x1000], 0x1000);
        set_helper(console, f.clone());
        let url = serve().await;
        set_live(console, 1);
        set_seat_profiles(
            console,
            vec![SeatProfile {
                player: 2,
                user: 0x2000,
                name: "Guest 1".into(),
                owner: false,
            }],
        );
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
        let mut sam = guest(
            &url,
            serde_json::json!({"hello": {"invite": key, "name": "Sam"}}),
        )
        .await;
        assert_eq!(next(&mut sam).await["stage"], "asking");
        allow_next(console, true).await;
        let j = next(&mut sam).await;
        assert_eq!(j["player"], 2);
        let token = j["token"].as_str().unwrap().to_string();
        let plays_as = || async {
            let s = host_state(console).await;
            s["guests"][0]["plays_as"]["name"].clone()
        };
        assert_eq!(plays_as().await, "Guest 1");

        // Same choice: back on the pad they had, nothing made again.
        drop(sam);
        until("Sam away", || {
            with_party(&host_key(console), |p| {
                p.guests.iter().any(|g| g.conn.is_none())
            })
            .unwrap_or(false)
        })
        .await;
        let mut sam = guest(&url, serde_json::json!({"hello": {"resume": token}})).await;
        assert_eq!(next(&mut sam).await["player"], 2);
        assert_eq!(f.binds.lock().unwrap().clone(), vec![bind(0x2000, false)]);

        // The host chooses Guest 2 for Player 2 while Sam is away.
        drop(sam);
        until("Sam away again", || {
            with_party(&host_key(console), |p| {
                p.guests.iter().any(|g| g.conn.is_none())
            })
            .unwrap_or(false)
        })
        .await;
        set_seat_profiles(
            console,
            vec![SeatProfile {
                player: 2,
                user: 0x3000,
                name: "Guest 2".into(),
                owner: false,
            }],
        );
        let mut sam = guest(&url, serde_json::json!({"hello": {"resume": token}})).await;
        assert_eq!(next(&mut sam).await["player"], 2);
        assert_eq!(
            f.binds.lock().unwrap().last().cloned(),
            Some(bind(0x3000, false)),
            "seated in the new choice"
        );
        assert!(f.log().contains(&"close2".to_string()), "{:?}", f.log());
        assert_eq!(plays_as().await, "Guest 2");

        drop(sam);
        let (ci, h) = local();
        close(
            ci,
            h,
            Json(AddrBody {
                addr: console.into(),
            }),
        )
        .await;
        set_seat_profiles(console, Vec::new());
        set_live(console, 0);
    }

    // Internet invites (join_net): the same session over a WebRTC data channel.
    mod internet {
        include!("join_net/join_tests.rs");
    }

    // The phone link over the internet (pad_link.rs on join_net): here for the settings and turns.
    mod pad_internet {
        include!("join_net/pad_tests.rs");
    }
}
