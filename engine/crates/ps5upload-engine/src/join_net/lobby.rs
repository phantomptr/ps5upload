//! The Play Room's lobby on the hub (ps5upload-hub PROTOCOL.md, "Rooms"; design: play-rooms 9).
//!
//! A Friends or Public room, and Play again over the internet (friend passes exist), keep one more
//! hub room: a bound room made from the room's own `lobby_secret` (join_room.rs), apart from
//! invite rooms so a card never carries an invite. On its host socket the engine:
//!
//! * **publishes the card** (`{"t":"ad"}`): create with the hub's proof of work (`ad_pow`,
//!   `ps5upload_hub::rooms::solve_pow`), updates on a real change at most every 10 s, a renewal
//!   every 100 s (the hub drops a card not renewed for 150 s), `unad` when the room turns Private,
//!   locks, or the host's device is away; `ad_gone` carries the next nonce and the card is made
//!   again when it is still wanted. The host's panel shows the state (`room.listed`).
//! * **answers relayed previews** (`{"t":"preview","pid"}` → within 3 s, at most 2 KiB, nothing
//!   shaped like an address: `ps5upload_hub::rooms::has_address`).
//! * **runs knocks** before any SDP. A guest from Rooms, Friends or Play again signals
//!   `{"knock": {"via", "name", "device"?, "proto"?, "caps"?, "client"?, "remember"?}}`; when the
//!   way in needs a secret (the room password, or a friend pass for `via: "again"`) the engine
//!   answers `{"knock": "challenge", "c", "pk"}` and the guest seals it (`{"knock": "sealed",
//!   "blob"}`, room_seal.rs). The knock then runs the very guest session of `/api/join/ws`
//!   (join.rs) over [`KnockSocket`]: the room's gate, the host's answer, the seat. Answers:
//!   `{"knock": "wait"}` (asking the host), `{"knock": "no", "why", …}` (`no`, `full` with
//!   `allowed`/`in`, `locked`, `password` with `tries_left`, `locked_out` with `wait_s`,
//!   `host_away`, `not_listed`, `old`, `name_taken` with `suggest`, `closed`, `not_playing`), or
//!   `{"knock": "ok", "token", "friend"?, "lobby"}`. Only an approved guest's offer and
//!   candidates reach WebRTC; on the data channel its hello is `{"hello": {"resume": token}}` (the
//!   seat is held for the resume window meanwhile).
//!
//! Address exchange only after approval: nothing about this engine's addresses goes to a guest
//! before `ok`.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::Message;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, watch, Notify};
use tokio_tungstenite::tungstenite::Message as WsMsg;

use super::rtc::Cmd;
use super::Kind;
use crate::join_room::{self as jr, Listed, Visibility};

/// What a knock settled for one guest of the lobby (by hub room and gid).
#[derive(Clone, Debug)]
pub(crate) struct Approved {
    /// How they came (for the debug view of the lobby).
    #[allow(dead_code)]
    pub via: jr::Via,
    pub at: Instant,
}

/// An approved gid has this long to connect (design 9.3 #5; the seat waits the resume window).
pub(crate) const APPROVED_FOR: Duration = Duration::from_secs(60);
/// Card renewals (the hub drops one not renewed for 150 s).
const RENEW_EVERY: Duration = Duration::from_secs(100);
/// Card updates at most this often (the hub's own limit is about one every 10 s).
const UPDATE_GAP: Duration = Duration::from_secs(11);
const CHECK_EVERY: Duration = Duration::from_secs(if cfg!(test) { 1 } else { 5 });
const PING_EVERY: Duration = Duration::from_secs(30);
const QUIET_MAX: Duration = Duration::from_secs(70);
/// How long the game on the card is trusted before asking the console again.
const GAME_EVERY: Duration = Duration::from_secs(60);
/// The largest preview the hub takes.
pub(crate) const PREVIEW_MAX: usize = 2048;
/// Knocks waiting at once on this lobby at most (the hub caps a lobby at 16 guests).
const KNOCKS_MAX: usize = 16;

#[derive(Default)]
struct Book {
    /// The hub's per-room client id of each guest (`{"t":"guest","gid","cid"}`).
    cids: HashMap<(String, String), String>,
    approved: HashMap<(String, String), Approved>,
}

static BOOK: Mutex<Option<Book>> = Mutex::new(None);

fn with_book<R>(f: impl FnOnce(&mut Book) -> R) -> R {
    let mut g = BOOK.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Book::default))
}

/// The hub's client id for `gid` in `room` (lobby rooms only).
pub(crate) fn cid_of(room: &str, gid: &str) -> Option<String> {
    with_book(|b| b.cids.get(&(room.to_string(), gid.to_string())).cloned())
}

pub(crate) fn set_cid(room: &str, gid: &str, cid: &str) {
    if cid.len() > 16 || gid.len() > 64 {
        return;
    }
    with_book(|b| {
        if b.cids.len() > 1024 {
            b.cids.clear();
        }
        b.cids
            .insert((room.to_string(), gid.to_string()), cid.to_string());
    });
}

pub(crate) fn forget_guest(room: &str, gid: &str) {
    with_book(|b| {
        let k = (room.to_string(), gid.to_string());
        b.cids.remove(&k);
    });
}

pub(crate) fn approve(room: &str, gid: &str, a: Approved) {
    with_book(|b| {
        let now = Instant::now();
        b.approved
            .retain(|_, x| now.saturating_duration_since(x.at) < APPROVED_FOR);
        b.approved.insert((room.to_string(), gid.to_string()), a);
    });
}

/// `gid` may send SDP/ICE in `room`: approved by a knock (lobby rooms take nothing else).
pub(crate) fn may_offer(room: &str, gid: &str) -> bool {
    with_book(|b| {
        b.approved
            .get(&(room.to_string(), gid.to_string()))
            .is_some_and(|a| a.at.elapsed() < APPROVED_FOR)
    })
}

/// A stand-in address for a lobby guest whose address the engine must not learn before
/// approval: a private IPv6 /64 made from its hub client id (so limits and blocks key on it like
/// an address), never shown.
pub(crate) fn knock_ip(cid: Option<&str>, gid: &str) -> IpAddr {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(cid.unwrap_or(gid).as_bytes());
    let w = |i: usize| u16::from_be_bytes([h[i], h[i + 1]]);
    IpAddr::V6(Ipv6Addr::new(
        0xfd00 | (w(0) & 0xff),
        w(2),
        w(4),
        w(6),
        0,
        0,
        0,
        1,
    ))
}

// ── Running ─────────────────────────────────────────────────────────────────────────────────────

struct Running {
    id: String,
    url: String,
    notify: Arc<Notify>,
    _stop: watch::Sender<()>,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

/// The lobby room's id while the lobby runs (handed to friends with their pass: `joined.lobby`).
pub(crate) fn lobby_id() -> Option<String> {
    RUNNING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|r| r.id.clone())
}

/// The hub the lobby would use, when the room wants one now.
fn wanted() -> Option<String> {
    let s = crate::join::settings();
    if !s.internet_invites || crate::join::open_consoles().is_empty() {
        return None;
    }
    let listed = s.room.visibility != Visibility::Private;
    (listed || crate::join_friends::count() > 0).then_some(s.hub_url)
}

/// Something about the room changed (opened, closed, settings, lock, people in): the lobby starts,
/// stops, or publishes again.
pub(crate) fn changed() {
    if cfg!(test) && !TEST_LOBBY.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let want = wanted();
    let mut g = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    match (want, g.as_ref()) {
        (None, Some(_)) => {
            let ended = g.take(); // the task ends with its stop sender
                                  // The parties' lock is taken by set_listed: never while RUNNING is held (a guest's
                                  // status push reads lobby_id() with the parties locked, the other way round).
            drop(g);
            drop(ended);
            crate::join::room_api::set_listed(Listed::Off);
        }
        (Some(url), Some(r)) if r.url == url => r.notify.notify_one(),
        (Some(url), _) => {
            let Ok(h) = tokio::runtime::Handle::try_current() else {
                return;
            };
            let Some((id, secret)) = jr::with_secrets(|s| (s.lobby_id(), s.lobby_secret.clone()))
            else {
                return;
            };
            let notify = Arc::new(Notify::new());
            let (stop, stop_rx) = watch::channel(());
            h.spawn(run(
                url.clone(),
                id.clone(),
                secret,
                notify.clone(),
                stop_rx,
            ));
            *g = Some(Running {
                id,
                url,
                notify,
                _stop: stop,
            });
        }
        (None, None) => {}
    }
}

/// Tests turn the lobby on (it never starts otherwise under test).
pub(crate) static TEST_LOBBY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

async fn run(
    url: String,
    id: String,
    secret: String,
    notify: Arc<Notify>,
    mut stop: watch::Receiver<()>,
) {
    let Some(net) = super::rtc::net().await else {
        crate::join::room_api::set_listed(Listed::Not("hub".into()));
        return;
    };
    let _ = net.cmd.send(Cmd::RoomOpen(id.clone(), Kind::Join));
    let mut backoff = Duration::from_secs(1);
    loop {
        let end = match super::hub::connect(&url).await {
            Ok(ws) => session(ws, &id, &secret, &notify, &mut stop, &net.cmd).await,
            Err(e) => {
                crate::log_at!(Debug, "together", "room: the rooms hub is unreachable: {e}");
                crate::join::room_api::set_listed(Listed::Not("hub".into()));
                End::Again(None)
            }
        };
        let wait = match end {
            End::Stop => break,
            End::Again(Some(w)) => w,
            End::Again(None) => {
                let w = backoff;
                backoff = (backoff * 2).min(Duration::from_secs(30));
                jitter(w)
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            r = stop.changed() => if r.is_err() { break },
        }
    }
    let _ = net.cmd.send(Cmd::RoomClose(id));
    crate::log_at!(Debug, "together", "room: lobby closed");
}

fn jitter(d: Duration) -> Duration {
    let mut b = [0u8; 1];
    let _ = getrandom::fill(&mut b);
    d.mul_f64(0.75 + f64::from(b[0]) / 255.0 * 0.5)
}

enum End {
    Stop,
    /// Again after this (None: the backoff).
    Again(Option<Duration>),
}

/// The card's state on this connection.
#[derive(Default)]
struct Card {
    pow: Option<(String, u32)>,
    /// The hub has the card.
    live: bool,
    /// A create was sent and not answered yet.
    creating: bool,
    sent: Option<Value>,
    sent_at: Option<Instant>,
    /// A refusal that waits for the card to change (`name`, `game`, `bad`, `old`, `blocked`,
    /// `off`), or until this instant (`busy`, `limit`).
    held: Option<(Value, Option<Instant>)>,
    game: Option<(Option<jr::Game>, Instant)>,
}

/// One knock in progress.
struct KnockSt {
    req: Value,
    challenge: Option<String>,
    left: Option<oneshot::Sender<()>>,
}

fn frame(v: Value) -> WsMsg {
    WsMsg::Text(v.to_string().into())
}

async fn session(
    mut ws: super::hub::Ws,
    id: &str,
    secret: &str,
    notify: &Notify,
    stop: &mut watch::Receiver<()>,
    cmd: &mpsc::UnboundedSender<Cmd>,
) -> End {
    let hello = json!({"v": 1, "t": "host", "room": id, "secret": secret, "lobby": true});
    if ws.send(frame(hello)).await.is_err() {
        return End::Again(None);
    }
    let (sig_tx, mut sig_rx) = mpsc::unbounded_channel::<(String, Value)>();
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut check = tokio::time::interval(CHECK_EVERY);
    let mut heard = Instant::now();
    let mut card = Card::default();
    let mut hosted = false;
    let mut knocks: HashMap<String, KnockSt> = HashMap::new();
    loop {
        tokio::select! {
            r = stop.changed() => if r.is_err() {
                if card.live {
                    let _ = ws.send(frame(json!({"t": "unad"}))).await;
                }
                let _ = ws.send(frame(json!({"v": 1, "t": "leave"}))).await;
                let _ = ws.close(None).await;
                return End::Stop;
            },
            _ = notify.notified() => {
                if hosted && publish(&mut ws, &mut card, false).await.is_err() {
                    return End::Again(None);
                }
            }
            _ = check.tick() => {
                if heard.elapsed() > QUIET_MAX {
                    return End::Again(None);
                }
                if hosted && publish(&mut ws, &mut card, false).await.is_err() {
                    return End::Again(None);
                }
            }
            _ = ping.tick() => {
                if ws.send(WsMsg::Ping(Vec::new().into())).await.is_err() {
                    return End::Again(None);
                }
            }
            s = sig_rx.recv() => if let Some((to, data)) = s {
                if ws.send(frame(json!({"v": 1, "t": "signal", "to": to, "data": data}))).await.is_err() {
                    return End::Again(None);
                }
            },
            m = ws.next() => {
                heard = Instant::now();
                let t = match m {
                    Some(Ok(WsMsg::Text(t))) => t,
                    Some(Ok(WsMsg::Close(_))) | None | Some(Err(_)) => return End::Again(None),
                    Some(Ok(_)) => continue,
                };
                let Ok(v) = serde_json::from_str::<Value>(t.as_str()) else { continue };
                let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                match s("t").as_str() {
                    "hosted" => {
                        if v.get("lobby") != Some(&Value::Bool(true)) || v.get("caps").is_none() {
                            // A hub from before rooms (self-hosted): invites still work. It
                            // opened a plain room for this host; close it for good (`leave`) so
                            // it does not wait 2 minutes for a reclaim and count toward this
                            // computer's 8 rooms there.
                            crate::join::room_api::set_listed(Listed::Not("old_hub".into()));
                            let _ = ws.send(frame(json!({"v": 1, "t": "leave"}))).await;
                            let _ = ws.close(None).await;
                            return End::Again(Some(Duration::from_secs(600)));
                        }
                        hosted = true;
                        card.pow = pow_of(&v);
                        crate::log_at!(Info, "together", "room: lobby open on the hub");
                        if publish(&mut ws, &mut card, true).await.is_err() {
                            return End::Again(None);
                        }
                    }
                    "ad_ok" => {
                        card.live = true;
                        card.creating = false;
                        let until = v.get("until").and_then(Value::as_u64).unwrap_or(0);
                        crate::join::room_api::set_listed(Listed::Until(until));
                    }
                    "ad_refused" => {
                        card.creating = false;
                        let why = s("why");
                        crate::log_at!(Info, "together", "room: the hub did not list the room: {why}");
                        if why != "rate" {
                            let until = matches!(why.as_str(), "busy" | "limit" | "pow")
                                .then(|| Instant::now() + Duration::from_secs(60));
                            card.held = card.sent.clone().map(|c| (c, until));
                            if !card.live {
                                crate::join::room_api::set_listed(Listed::Not(why));
                            }
                        }
                    }
                    "ad_gone" => {
                        card.live = false;
                        card.creating = false;
                        card.pow = pow_of(&v);
                        let why = s("why");
                        if matches!(why.as_str(), "off" | "blocked" | "old") {
                            card.held = card.sent.clone().map(|c| (c, None));
                            crate::join::room_api::set_listed(Listed::Not(why));
                        } else if why != "unad" && publish(&mut ws, &mut card, true).await.is_err() {
                            return End::Again(None);
                        }
                    }
                    "preview" => {
                        let pid = s("pid");
                        if !pid.is_empty() && pid.len() <= 32 {
                            let data = preview_for_hub();
                            let f = frame(json!({"t": "preview", "pid": pid, "data": data}));
                            if ws.send(f).await.is_err() {
                                return End::Again(None);
                            }
                        }
                    }
                    "guest" => {
                        let (gid, cid) = (s("gid"), s("cid"));
                        if !gid.is_empty() && !cid.is_empty() {
                            set_cid(id, &gid, &cid);
                        }
                    }
                    "left" => {
                        let gid = s("gid");
                        if let Some(mut k) = knocks.remove(&gid) {
                            if let Some(l) = k.left.take() {
                                let _ = l.send(());
                            }
                        }
                        forget_guest(id, &gid);
                        let _ = cmd.send(Cmd::Left { room: id.to_string(), gid });
                    }
                    "signal" => {
                        let gid = s("from");
                        if gid.is_empty() || gid.len() > 64 {
                            continue;
                        }
                        let data = v.get("data").cloned().unwrap_or_default();
                        on_signal(id, &gid, data, &mut knocks, &sig_tx, cmd);
                    }
                    "error" => {
                        let why = s("why");
                        match why.as_str() {
                            "no_peer" | "not_approved" => {}
                            "no_lobby" => {
                                crate::join::room_api::set_listed(Listed::Not("off".into()));
                                return End::Again(Some(Duration::from_secs(300)));
                            }
                            "rate_limited" => {
                                let retry = v.get("retry_s").and_then(Value::as_u64).unwrap_or(30);
                                return End::Again(Some(jitter(Duration::from_secs(retry.clamp(1, 600)))));
                            }
                            _ => {
                                crate::log_at!(Debug, "together", "room: the hub says {why}");
                                return End::Again(None);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn pow_of(v: &Value) -> Option<(String, u32)> {
    let p = v.get("ad_pow")?;
    let n = p.get("n")?.as_str()?.to_string();
    let bits = p.get("bits")?.as_u64()? as u32;
    (bits <= 28 && n.len() <= 64).then_some((n, bits))
}

/// The card now (None: none wanted, with why for the host's panel).
fn card_now(card: &mut Card) -> Result<Value, Listed> {
    let s = crate::join::settings();
    if s.room.visibility == Visibility::Private {
        return Err(Listed::Off);
    }
    let Some(r) = crate::join::room_api::card_input() else {
        return Err(Listed::Off);
    };
    if r.locked {
        return Err(Listed::Not("locked".into()));
    }
    if r.host_away {
        return Err(Listed::Not("host_away".into()));
    }
    // The game: picked by the host, else the console's running title (asked once a minute).
    let game = match r.game {
        Some(g) => Some(g),
        None => match &card.game {
            Some((g, at)) if at.elapsed() < GAME_EVERY => g.clone(),
            _ => {
                let g = running_game(&r.console);
                card.game = Some((g.clone(), Instant::now()));
                g
            }
        },
    };
    let host = ps5upload_hub::names::clean_name(&r.host, 1, 24, &[])
        .unwrap_or_else(|_| "Host".to_string());
    let mut cfg = s.room.clone();
    if cfg
        .name
        .as_deref()
        .is_none_or(|n| jr::room_name(n).is_err())
    {
        cfg.name = Some(
            jr::room_name(&jr::default_name(&host)).unwrap_or_else(|_| "Play Room".to_string()),
        );
    }
    let pk = jr::with_secrets(|x| x.key.public()).unwrap_or_default();
    jr::ad(
        &cfg,
        &host,
        &jr::host_tag(),
        game.as_ref(),
        r.guests_in,
        r.size,
        jr::has_password(),
        r.locked,
        &pk,
    )
    .ok_or(Listed::Off)
}

/// The title running on the console, with its name (None: nothing, or the console didn't say).
fn running_game(console: &str) -> Option<jr::Game> {
    if cfg!(test) {
        return None;
    }
    let ids = crate::save_backup_server::running_titles(console).ok()?;
    let id = ids.into_iter().next()?;
    let title = crate::title_name(console, &id)?;
    Some(jr::Game {
        title: title.chars().take(64).collect(),
        id,
        picked: false,
    })
}

/// Sends what the card needs now (create, update, renew, withdraw).
async fn publish(ws: &mut super::hub::Ws, card: &mut Card, force: bool) -> Result<(), ()> {
    let now = Instant::now();
    let want = card_now(card);
    match want {
        Err(listed) => {
            if card.live || card.creating {
                ws.send(frame(json!({"t": "unad"}))).await.map_err(|_| ())?;
                card.live = false;
                card.creating = false;
            }
            card.sent = None;
            crate::join::room_api::set_listed(listed);
            Ok(())
        }
        Ok(ad) => {
            if let Some((held, until)) = &card.held {
                let still = *held == ad && until.is_none_or(|t| t > now);
                if still {
                    return Ok(());
                }
                card.held = None;
            }
            if !card.live {
                if card.creating {
                    return Ok(());
                }
                let Some((n, bits)) = card.pow.clone() else {
                    return Ok(()); // the next nonce comes with ad_gone / hosted
                };
                crate::join::room_api::set_listed(Listed::Listing);
                let answer =
                    tokio::task::spawn_blocking(move || ps5upload_hub::rooms::solve_pow(&n, bits))
                        .await
                        .map_err(|_| ())?;
                ws.send(frame(json!({"t": "ad", "ad": ad, "pow": answer})))
                    .await
                    .map_err(|_| ())?;
                card.creating = true;
                card.sent = Some(ad);
                card.sent_at = Some(now);
                return Ok(());
            }
            let since = card.sent_at.map(|t| now.saturating_duration_since(t));
            if card.sent.as_ref() != Some(&ad) && (force || since.is_none_or(|s| s >= UPDATE_GAP)) {
                ws.send(frame(json!({"t": "ad", "ad": ad})))
                    .await
                    .map_err(|_| ())?;
                card.sent = Some(ad);
                card.sent_at = Some(now);
            } else if since.is_none_or(|s| s >= RENEW_EVERY) {
                ws.send(frame(json!({"t": "ad"}))).await.map_err(|_| ())?;
                card.sent_at = Some(now);
            }
            Ok(())
        }
    }
}

/// The preview for the hub: at most [`PREVIEW_MAX`] bytes and nothing shaped like an address
/// (a name that looks like one is shown as "Friend"; past the size, names then players go).
pub(crate) fn preview_for_hub() -> Value {
    let Some(mut v) = crate::join::room_api::preview_any() else {
        return json!({"none": true});
    };
    fit_preview(&mut v);
    v
}

pub(crate) fn fit_preview(v: &mut Value) {
    use ps5upload_hub::rooms::has_address;
    if let Some(ps) = v.get_mut("players").and_then(Value::as_array_mut) {
        for p in ps {
            if p.get("name")
                .and_then(Value::as_str)
                .is_some_and(has_address)
            {
                p["name"] = "Friend".into();
            }
        }
    }
    for (a, b) in [("room", "name"), ("host", "name")] {
        if v[a][b].as_str().is_some_and(has_address) {
            v[a][b] = "Play Room".into();
        }
    }
    if v.to_string().len() > PREVIEW_MAX {
        if let Some(ps) = v.get_mut("players").and_then(Value::as_array_mut) {
            for p in ps.iter_mut() {
                if let Some(o) = p.as_object_mut() {
                    o.remove("name");
                }
            }
        }
    }
    if v.to_string().len() > PREVIEW_MAX || has_address(&v.to_string()) {
        v["players"] = json!([]);
        v["game"] = Value::Null;
    }
}

/// A lobby guest's signal: knocks before approval, then its offer and candidates.
fn on_signal(
    room: &str,
    gid: &str,
    data: Value,
    knocks: &mut HashMap<String, KnockSt>,
    sig: &mpsc::UnboundedSender<(String, Value)>,
    cmd: &mpsc::UnboundedSender<Cmd>,
) {
    if let Some(k) = data.get("knock") {
        if k.is_object() {
            if knocks.contains_key(gid) || knocks.len() >= KNOCKS_MAX {
                let _ = sig.send((gid.to_string(), json!({"knock": "no", "why": "busy"})));
                return;
            }
            // Over the hub a knock is never from this home network: "nearby" is not taken from
            // it (it would open a Private room's Nearby path to anyone with the lobby id).
            let via = k
                .get("via")
                .and_then(Value::as_str)
                .and_then(jr::Via::from_word)
                .filter(|v| matches!(v, jr::Via::Rooms | jr::Via::Friends | jr::Via::Again))
                .unwrap_or(jr::Via::Rooms);
            let needs = (jr::has_password() && via.needs_password()) || via == jr::Via::Again;
            let mut st = KnockSt {
                req: k.clone(),
                challenge: None,
                left: None,
            };
            if needs {
                let Some(c) = crate::room_seal::challenge() else {
                    return;
                };
                let pk = jr::with_secrets(|s| s.key.public()).unwrap_or_default();
                st.challenge = Some(c.clone());
                let _ = sig.send((
                    gid.to_string(),
                    json!({"knock": "challenge", "c": c, "pk": pk}),
                ));
                knocks.insert(gid.to_string(), st);
            } else {
                start_knock(room, gid, &mut st, None, sig);
                knocks.insert(gid.to_string(), st);
            }
            return;
        }
        if k.as_str() == Some("sealed") {
            let Some(st) = knocks.get_mut(gid) else {
                return;
            };
            let Some(c) = st.challenge.take() else {
                return; // one sealed answer per challenge
            };
            let blob = data.get("blob").and_then(Value::as_str).unwrap_or("");
            let opened = jr::with_secrets(|s| s.key.open(blob, gid, &c)).flatten();
            start_knock(room, gid, st, Some(opened.unwrap_or_default()), sig);
        }
        return;
    }
    if !may_offer(room, gid) {
        return; // nothing but knocks before approval
    }
    let field = |k: &str| {
        data.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };
    match field("type").as_str() {
        "offer" => {
            let _ = cmd.send(Cmd::Offer {
                room: room.to_string(),
                gid: gid.to_string(),
                sdp: field("sdp"),
                signal: sig.clone(),
            });
        }
        "candidate" => {
            let _ = cmd.send(Cmd::Remote {
                room: room.to_string(),
                gid: gid.to_string(),
                candidate: field("candidate"),
            });
        }
        _ => {}
    }
}

/// Runs the guest session for a knock (the room's gate, the host's answer, the seat).
fn start_knock(
    room: &str,
    gid: &str,
    st: &mut KnockSt,
    sealed: Option<crate::room_seal::Sealed>,
    sig: &mpsc::UnboundedSender<(String, Value)>,
) {
    let r = &st.req;
    let via = r
        .get("via")
        .and_then(Value::as_str)
        .unwrap_or("rooms")
        .to_string();
    let mut hello = json!({
        "name": r.get("name").and_then(Value::as_str).unwrap_or(""),
        "device": r.get("device").cloned().unwrap_or(Value::Null),
        "proto": r.get("proto").cloned().unwrap_or(json!(jr::PROTO)),
        "caps": r.get("caps").cloned().unwrap_or(json!([])),
        "client": r.get("client").cloned().unwrap_or(Value::Null),
        "remember": r.get("remember").and_then(Value::as_bool).unwrap_or(false),
        "knock": {"via": via},
    });
    // The one sealed secret: the friend pass for Play again, else the room password.
    if let Some(sl) = sealed.filter(|x| !x.secret.is_empty()) {
        if via == "again" {
            hello["again"] = sl.secret.into();
        } else {
            hello["knock"]["password"] = sl.secret.into();
        }
    }
    if via == "again" && hello.get("again").is_none() {
        // A pass is the way in; without one it is a plain Rooms knock.
        hello["knock"]["via"] = "friends".into();
    }
    let (left_tx, left_rx) = oneshot::channel();
    st.left = Some(left_tx);
    let cid = cid_of(room, gid);
    let ip = knock_ip(cid.as_deref(), gid);
    let sock = KnockSocket {
        room: room.to_string(),
        gid: gid.to_string(),
        sig: sig.clone(),
        hello: Some(json!({"hello": hello}).to_string()),
        left: Some(left_rx),
        via: jr::Via::from_word(&via).unwrap_or(jr::Via::Rooms),
        // The room the card showed: the lobby's, whatever other console hosts here too.
        console: crate::join::room_api::lobby_console(),
    };
    tokio::spawn(crate::join::guest_session(sock, ip));
}

/// A knock as a guest socket: the hello it was given, the session's answers turned into knock
/// signals, and "gone" when the guest leaves the hub.
pub(crate) struct KnockSocket {
    room: String,
    gid: String,
    sig: mpsc::UnboundedSender<(String, Value)>,
    hello: Option<String>,
    left: Option<oneshot::Receiver<()>>,
    via: jr::Via,
    /// The console of the room the lobby stood for when the knock came.
    console: Option<String>,
}

impl KnockSocket {
    /// The knock answer for one of the session's messages (None: nothing to tell).
    pub(crate) fn answer(&self, v: &Value) -> Option<Value> {
        match v.get("stage").and_then(Value::as_str)? {
            "asking" => Some(json!({"knock": "wait"})),
            "refused" => {
                let mut a = v.clone();
                let o = a.as_object_mut()?;
                o.remove("stage");
                let why = o.get("why").and_then(Value::as_str).unwrap_or("no");
                let why = if why == "denied" { "no" } else { why }.to_string();
                o.insert("why".into(), why.into());
                o.insert("knock".into(), "no".into());
                Some(a)
            }
            "knocked" => Some(json!({
                "knock": "ok",
                "token": v.get("token"),
                "friend": v.get("friend"),
                "lobby": self.room,
                // The engine's certificate, for a bridge to check the answer against (no link here).
                "fp": super::fingerprint(),
            })),
            _ => None,
        }
    }
}

impl crate::join::GuestSocket for KnockSocket {
    async fn send(&mut self, m: Message) -> Result<(), ()> {
        let Message::Text(t) = m else {
            return Ok(());
        };
        let Ok(v) = serde_json::from_str::<Value>(t.as_str()) else {
            return Ok(());
        };
        if let Some(a) = self.answer(&v) {
            if a.get("knock").and_then(Value::as_str) == Some("ok") {
                approve(
                    &self.room,
                    &self.gid,
                    Approved {
                        via: self.via,
                        at: Instant::now(),
                    },
                );
            }
            self.sig.send((self.gid.clone(), a)).map_err(|_| ())?;
        }
        Ok(())
    }

    async fn recv(&mut self) -> Option<Result<Message, ()>> {
        if let Some(h) = self.hello.take() {
            return Some(Ok(Message::Text(h.into())));
        }
        match self.left.take() {
            Some(l) => {
                let _ = l.await;
                None
            }
            None => None,
        }
    }

    fn knock_only(&self) -> bool {
        true
    }

    fn lobby_console(&self) -> Option<String> {
        self.console.clone()
    }

    fn hub_cid(&self) -> Option<String> {
        cid_of(&self.room, &self.gid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approvals_lapse_and_cids_are_kept_per_room() {
        approve(
            "lobbyA",
            "g1",
            Approved {
                via: jr::Via::Rooms,
                at: Instant::now(),
            },
        );
        assert!(may_offer("lobbyA", "g1"));
        assert!(!may_offer("lobbyA", "g2"));
        assert!(!may_offer("lobbyB", "g1"));
        approve(
            "lobbyA",
            "g3",
            Approved {
                via: jr::Via::Rooms,
                at: Instant::now() - APPROVED_FOR,
            },
        );
        assert!(!may_offer("lobbyA", "g3"));
        set_cid("lobbyA", "g1", "abcdEFGH");
        assert_eq!(cid_of("lobbyA", "g1").as_deref(), Some("abcdEFGH"));
        forget_guest("lobbyA", "g1");
        assert_eq!(cid_of("lobbyA", "g1"), None);
    }

    /// A hub from before rooms (12.6.46 and older) ignores `lobby` and opens a plain room: the
    /// lobby closes that room at once (`leave`, so it neither waits for a reclaim nor counts
    /// toward the computer's rooms there) and tries again only after 10 minutes.
    #[tokio::test]
    async fn an_older_hub_gets_its_room_back_and_a_long_wait() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let old_hub = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            let mut got = Vec::new();
            while let Some(Ok(m)) = ws.next().await {
                match m {
                    WsMsg::Text(t) => {
                        let v: Value = serde_json::from_str(t.as_str()).unwrap();
                        if v["t"] == "host" {
                            // What 12.6.46's hub answers: no `lobby`, no `caps`.
                            let _ = ws
                                .send(frame(json!({"t": "hosted", "room": v["room"]})))
                                .await;
                        }
                        got.push(v);
                    }
                    WsMsg::Close(_) => break,
                    _ => {}
                }
            }
            got
        });
        let ws = super::super::hub::connect(&format!("ws://{addr}/ws"))
            .await
            .unwrap();
        let (_stop_tx, mut stop) = watch::channel(());
        let (cmd, _cmd_rx) = mpsc::unbounded_channel();
        let room = ps5upload_hub::hub::room_for_secret(&"s".repeat(43));
        let end = tokio::time::timeout(
            Duration::from_secs(10),
            session(ws, &room, &"s".repeat(43), &Notify::new(), &mut stop, &cmd),
        )
        .await
        .expect("the lobby gives up on an old hub at once");
        assert!(
            matches!(end, End::Again(Some(d)) if d >= Duration::from_secs(600)),
            "a long wait before the next try"
        );
        let got = tokio::time::timeout(Duration::from_secs(5), old_hub)
            .await
            .unwrap()
            .unwrap();
        let kinds: Vec<&str> = got.iter().map(|v| v["t"].as_str().unwrap_or("")).collect();
        assert_eq!(kinds, ["host", "leave"], "{got:?}");
        assert_eq!(got[0]["lobby"], true);
    }

    #[test]
    fn knock_addresses_are_private_and_per_client() {
        let a = knock_ip(Some("cidA"), "g1");
        let b = knock_ip(Some("cidA"), "g2");
        let c = knock_ip(Some("cidB"), "g1");
        assert_eq!(a, b, "one client, one key");
        assert_ne!(
            crate::join::limit_key(a),
            crate::join::limit_key(c),
            "two clients, two keys"
        );
        let IpAddr::V6(v6) = a else { panic!() };
        assert_eq!(v6.segments()[0] & 0xff00, 0xfd00, "a private (ULA) address");
    }

    #[test]
    fn previews_for_the_hub_hold_no_address_and_fit() {
        let mut v = json!({"room": {"name": "Couch"}, "host": {"name": "10.0.0.5"},
            "players": [{"name": "192.168.1.9", "seat": 2}, {"name": "Mia", "seat": 3}],
            "game": {"id": "PPSA01234", "title": "x"}});
        fit_preview(&mut v);
        assert!(!ps5upload_hub::rooms::has_address(&v.to_string()), "{v}");
        assert_eq!(v["players"][0]["name"], "Friend");
        assert_eq!(v["players"][1]["name"], "Mia");
        let mut big = json!({"players": (0..60).map(|i| json!({"name": format!("Player name {i}"), "seat": 2})).collect::<Vec<_>>()});
        fit_preview(&mut big);
        assert!(
            big.to_string().len() <= PREVIEW_MAX,
            "{}",
            big.to_string().len()
        );
    }

    #[test]
    fn knock_answers_follow_the_protocol() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let k = KnockSocket {
            room: "lobbyA".into(),
            gid: "g1".into(),
            sig: tx,
            hello: None,
            left: None,
            via: jr::Via::Rooms,
            console: None,
        };
        assert_eq!(
            k.answer(&json!({"stage": "asking"})),
            Some(json!({"knock": "wait"}))
        );
        assert_eq!(
            k.answer(&json!({"stage": "refused", "why": "denied"})),
            Some(json!({"knock": "no", "why": "no"}))
        );
        assert_eq!(
            k.answer(&json!({"stage": "refused", "why": "password", "tries_left": 4})),
            Some(json!({"knock": "no", "why": "password", "tries_left": 4}))
        );
        let ok = k
            .answer(&json!({"stage": "knocked", "token": "t1", "friend": null}))
            .unwrap();
        assert_eq!(ok["knock"], "ok");
        assert_eq!(ok["token"], "t1");
        assert_eq!(ok["lobby"], "lobbyA");
        assert_eq!(k.answer(&json!({"status": {}})), None);
    }
}
