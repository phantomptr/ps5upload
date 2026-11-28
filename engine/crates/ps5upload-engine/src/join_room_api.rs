//! Play Rooms: the host's room controls and the room views (a child of join.rs, so it reaches the
//! party's own fields). Design: play-rooms 5 and 10.1.
//!
//! Host routes (this computer, or a peer `PS5UPLOAD_ALLOW_IP` admits, like the rest of
//! `/api/join`):
//! * `POST /api/join/room {addr, name?, visibility?, size?, password? ("" removes), approval?,
//!   chat?, voice?, start_muted?, watchers?, lang?, region?, nearby?, preview?, lock?, game?,
//!   public_ok?}` → the room (`room` of `GET /api/join`); 409 `size_below_players` (with `in`),
//!   `public_needs_anywhere`, `password_short`, `password_long`, `name` (with `why`), 400 `bad`.
//! * `POST /api/join/kick {addr, player, block?: "room" | "always"}` → `{ok}`.
//! * `POST /api/join/unblock {addr?, tag}` → `{ok}`.
//! * Mutes and the chat/voice modes: join_comms.rs (`/api/join/mute`, `/api/join/comms`); the
//!   room's own settings route hands the modes over too.
//! * `POST /api/join/seat {addr, player, to: 2-4 | 1 | "back"}` → `{ok, player}`; 409 `not_free`,
//!   `stranger`, `no_stream_seat`, `no_guest`, `not_on_seat`, `failed`.
//! * `POST /api/join/close {addr, confirm_friends?}`: with friends in, 409 `friends_playing`
//!   (`friends`: their names) unless confirmed (`confirm_friends: true`, `?confirm_friends=1` or
//!   the `X-Confirm-Friends: 1` header).
//!
//! Public (the home network and Tailscale reach it, like `/api/join/ws`):
//! * `GET /api/join/preview` → the preview document (join_room.rs `preview`), 404
//!   `{"why":"no_room"}` when no room is open, 429 `{"why":"slow_down","retry_s"}` with
//!   `Retry-After`. Never an address.

use super::*;
use crate::join_room::{self as jr, Block, Visibility};

#[derive(serde::Deserialize)]
pub struct CloseBody {
    #[serde(alias = "console", alias = "host")]
    pub(crate) addr: String,
    #[serde(default, deserialize_with = "crate::together_guard::de_confirm")]
    pub(crate) confirm_friends: bool,
}

/// `POST /api/join/close`: closes the room; asks first while friends are in (the engine's
/// "friends playing" guard, together_guard.rs: 409 `friends_playing` with their names).
pub async fn close(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Query(cq): Query<crate::together_guard::ConfirmQuery>,
    Json(b): Json<CloseBody>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    let friends = friends_playing(&b.addr);
    if let Some(r) =
        crate::together_guard::check(Some(&b.addr), &headers, b.confirm_friends || cq.yes())
    {
        return r;
    }
    close_party(&b.addr);
    let mut v = state_json(&b.addr);
    v["left"] = friends.into();
    Json(v).into_response()
}

/// The lobby's listing as every open room shows it (join_net/lobby.rs).
pub(crate) fn set_listed(l: jr::Listed) {
    with_parties(|m| {
        for p in m.values_mut() {
            p.room.listed = l.clone();
        }
    });
}

/// What the lobby's card is made from (the first open room: one lobby per engine).
pub(crate) struct CardIn {
    pub console: String,
    pub host: String,
    pub locked: bool,
    pub host_away: bool,
    pub game: Option<jr::Game>,
    pub guests_in: u8,
    pub size: u8,
}

pub(crate) fn card_input() -> Option<CardIn> {
    let c = with_parties(|m| {
        let p = m.values().next()?;
        Some(CardIn {
            console: p.console.clone(),
            host: crate::join_names::effective(&p.host_name).to_string(),
            locked: p.room.locked,
            host_away: p.host_gone.is_some(),
            game: p.room.game.clone(),
            guests_in: p.guests_in() as u8,
            size: p.settings.max_players.clamp(1, MAX_GUESTS) + 1,
        })
    })?;
    let away = c.host_away || host_device_away(&c.console);
    Some(CardIn {
        host_away: away,
        ..c
    })
}

/// The preview of the first open room (the hub's relayed preview).
pub(crate) fn preview_any() -> Option<serde_json::Value> {
    let key = with_parties(|m| m.keys().next().cloned())?;
    preview_of(&key)
}

#[derive(serde::Deserialize, Default)]
pub struct RoomBody {
    #[serde(alias = "console", alias = "host")]
    addr: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    visibility: Option<Visibility>,
    #[serde(default)]
    size: Option<u8>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    approval: Option<jr::Approval>,
    #[serde(default)]
    chat: Option<jr::ChatMode>,
    #[serde(default)]
    voice: Option<jr::VoiceMode>,
    #[serde(default)]
    start_muted: Option<bool>,
    #[serde(default)]
    watchers: Option<u8>,
    /// "" or "any": any language.
    #[serde(default)]
    lang: Option<String>,
    /// A region, "hidden", or "" (from the time zone).
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    nearby: Option<bool>,
    #[serde(default)]
    preview: Option<jr::PreviewShows>,
    #[serde(default)]
    lock: Option<bool>,
    /// `{title, id}` picked before playing, or null to follow the console.
    #[serde(default)]
    game: Option<serde_json::Value>,
    #[serde(default)]
    public_ok: Option<bool>,
}

fn conflict(err: &str, extra: serde_json::Value) -> Response {
    let mut v = serde_json::json!({"error": err});
    if let serde_json::Value::Object(o) = extra {
        for (k, x) in o {
            v[k] = x;
        }
    }
    (StatusCode::CONFLICT, Json(v)).into_response()
}

/// Applies a room change to the settings (checked first, nothing applied on a refusal).
pub(crate) fn apply_room(b: &RoomBody) -> Result<(), Box<Response>> {
    apply_room_checked(b).map_err(Box::new)
}

#[allow(clippy::result_large_err)]
fn apply_room_checked(b: &RoomBody) -> Result<(), Response> {
    let mut s = settings();
    let mut room = s.room.clone();
    if let Some(n) = &b.name {
        room.name = if n.trim().is_empty() {
            None
        } else {
            Some(jr::room_name(n).map_err(|e| {
                conflict(
                    "name",
                    serde_json::json!({"why": format!("{e:?}").to_lowercase()}),
                )
            })?)
        };
    }
    if let Some(v) = b.visibility {
        if v != Visibility::Private && !s.internet_invites {
            return Err(conflict("public_needs_anywhere", serde_json::json!({})));
        }
        room.visibility = v;
    }
    if let Some(n) = b.size {
        if !(2..=jr::SIZE_MAX).contains(&n) {
            return Err(bad(StatusCode::BAD_REQUEST, "bad"));
        }
        let most_in = with_party(&host_key(&b.addr), |p| p.guests_in()).unwrap_or(0);
        if usize::from(n - 1) < most_in {
            return Err(conflict(
                "size_below_players",
                serde_json::json!({"in": most_in + 1}),
            ));
        }
        s.max_players = n - 1;
    }
    if let Some(a) = b.approval {
        room.approval = a;
    }
    if let Some(c) = b.chat {
        room.chat = Some(c);
    }
    if let Some(v) = b.voice {
        room.voice = Some(v);
    }
    if let Some(m) = b.start_muted {
        room.start_muted = Some(m);
    }
    if let Some(w) = b.watchers {
        room.watchers = w.min(jr::WATCHERS_MAX);
    }
    if let Some(l) = &b.lang {
        room.lang = match l.as_str() {
            "" | "any" => None,
            l if jr::LANGS.contains(&l) => Some(l.to_string()),
            _ => return Err(bad(StatusCode::BAD_REQUEST, "bad")),
        };
    }
    if let Some(r) = &b.region {
        room.region = match r.as_str() {
            "" | "auto" => None,
            r if jr::region_ok(r) => Some(r.to_string()),
            _ => return Err(bad(StatusCode::BAD_REQUEST, "bad")),
        };
    }
    if let Some(n) = b.nearby {
        room.nearby = n;
    }
    if let Some(p) = b.preview {
        room.preview = p;
    }
    if let Some(ok) = b.public_ok {
        room.public_ok = ok;
    }
    // The password last (it is saved apart, and only once everything else is fine).
    if let Some(pw) = &b.password {
        if let Err(e) = jr::set_password(Some(pw)) {
            return Err(conflict(
                match e {
                    crate::room_seal::PwErr::Short => "password_short",
                    crate::room_seal::PwErr::Long => "password_long",
                },
                serde_json::json!({"min": crate::room_seal::PW_MIN}),
            ));
        }
        if pw.is_empty() {
            // "The password lets people in without asking" goes with it (and says so: the
            // view shows approval.password false).
            room.approval.password = false;
        }
    }
    s.room = room;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(s.clone());
    save_invites(&s);
    SETTINGS_REV.fetch_add(1, Ordering::Relaxed);
    let key = host_key(&b.addr);
    with_parties(|m| {
        for (k, p) in m.iter_mut() {
            p.settings = s.clone();
            if *k == key {
                if let Some(l) = b.lock {
                    p.room.locked = l;
                }
                if let Some(g) = &b.game {
                    p.room.game = game_from(g);
                }
            }
        }
    });
    // Chat and voice follow the room's modes (join_comms.rs enforces them).
    let consoles: Vec<String> = with_parties(|m| m.values().map(|p| p.console.clone()).collect());
    for c in consoles {
        hand_modes(&c, &s.room);
    }
    crate::join_net::lobby::changed();
    Ok(())
}

/// The room's chat and voice modes to chat and voice (join_comms.rs).
pub(crate) fn hand_modes(console: &str, cfg: &jr::RoomCfg) {
    let public = cfg.visibility == Visibility::Public;
    crate::join_comms::set_modes(console, |m| {
        m.chat = cfg.chat();
        m.voice = cfg.voice();
        m.start_muted = cfg.start_muted();
        m.public = public;
    });
}

fn game_from(v: &serde_json::Value) -> Option<jr::Game> {
    let id = v.get("id")?.as_str()?;
    let title = v.get("title")?.as_str()?;
    let b = id.as_bytes();
    let ok = b.len() == 9
        && b[..4].iter().all(u8::is_ascii_uppercase)
        && b[4..].iter().all(u8::is_ascii_digit);
    let t = title.trim();
    (ok && !t.is_empty() && t.chars().count() <= 64).then(|| jr::Game {
        title: t.to_string(),
        id: id.to_string(),
        picked: true,
    })
}

/// `POST /api/join/room`.
pub async fn room(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<RoomBody>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    // PBKDF2 for a new password: off the async threads.
    let addr = b.addr.clone();
    let r = tokio::task::spawn_blocking(move || apply_room(&b)).await;
    match r {
        Ok(Ok(())) => Json(state_json(&addr)).into_response(),
        Ok(Err(resp)) => *resp,
        Err(_) => bad(StatusCode::INTERNAL_SERVER_ERROR, "failed"),
    }
}

/// The room as the host's views show it (`room` of `GET /api/join`).
pub(crate) fn room_view(p: &Party, now: Instant) -> serde_json::Value {
    let cfg = &p.settings.room;
    serde_json::json!({
        "name": cfg.name_for(&p.host_name),
        "name_custom": cfg.name.is_some(),
        "visibility": cfg.visibility.word(),
        "size": p.settings.max_players.clamp(1, MAX_GUESTS) + 1,
        "password": jr::has_password(),
        "approval": cfg.approval,
        "lock": p.room.locked,
        "chat": cfg.chat(),
        "voice": cfg.voice(),
        "start_muted": cfg.start_muted(),
        "watchers": cfg.watchers,
        "lang": cfg.lang,
        "region": cfg.region,
        "nearby": cfg.nearby,
        "preview_shows": cfg.preview,
        "public_ok": cfg.public_ok,
        "game": p.room.game,
        "listed": p.room.listed.json(),
        "blocked": p.room.blocked.iter().map(Block::json)
            .chain(jr::blocked_always_list().iter().map(Block::json))
            .collect::<Vec<_>>(),
        "many_wrong_passwords": jr::many_wrong_passwords(),
        "open_for_s": now.saturating_duration_since(p.room.opened_at).as_secs(),
        "host_tag": jr::host_tag(),
        "lobby": crate::join_net::lobby::lobby_id(),
        "proto": jr::PROTO,
        "caps": jr::caps(),
    })
}

/// What a guest's status carries about the room (`{"room": …}` beside `status`).
pub(crate) fn guest_room(p: &Party, id: u64, now: Instant) -> Option<serde_json::Value> {
    let me = p.guests.iter().find(|g| g.id == id && g.active())?;
    let cfg = &p.settings.room;
    let (_, tier, _) = me.route_tier(now);
    let mut players = vec![serde_json::json!({
        "name": crate::join_names::effective(&p.host_name), "player": 1, "seat": 1, "host": true, "you": false,
    })];
    players.extend(p.guests.iter().filter(|g| g.active()).map(|g| {
        serde_json::json!({
            "name": crate::join_names::effective(&g.name),
            "player": g.player,
            "seat": g.player,
            "id": g.id,
            "you": g.id == id,
        })
    }));
    Some(serde_json::json!({
        "name": cfg.name_for(&p.host_name),
        "size": p.settings.max_players.clamp(1, MAX_GUESTS) + 1,
        "players": players,
        "you": {"rtt_ms": me.stats.rtt_now(), "tier": tier},
        "chat": cfg.chat(),
        "voice": cfg.voice(),
        "locked": p.room.locked,
        "playing_for_min": now.saturating_duration_since(p.room.opened_at).as_secs() / 60,
        "proto": jr::PROTO,
        "caps": jr::caps(),
    }))
}

#[derive(serde::Deserialize)]
pub struct KickBody {
    #[serde(alias = "console", alias = "host")]
    pub(crate) addr: String,
    pub(crate) player: u8,
    /// "room": not back in this room; "always": not on this computer either.
    #[serde(default)]
    pub(crate) block: Option<String>,
}

/// `POST /api/join/kick {addr, player, block?}`.
pub async fn kick(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<KickBody>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    let block = match b.block.as_deref() {
        None | Some("") => None,
        Some("room") => Some(false),
        Some("always") => Some(true),
        Some(_) => return bad(StatusCode::BAD_REQUEST, "block"),
    };
    let ok = kick_and_block(&b.addr, b.player, block);
    Json(serde_json::json!({ "ok": ok })).into_response()
}

/// Removes Player `player` on `addr`; `block`: Some(false) not back in this room, Some(true) not
/// on this computer either (needs their device tag; their address and hub id are blocked for
/// the room's life too, so a reset tag doesn't get them back).
pub(crate) fn kick_and_block(addr: &str, player: u8, block: Option<bool>) -> bool {
    let now = Instant::now();
    let fx = with_party(&host_key(addr), |p| {
        let g = p.guests.iter().find(|g| g.active() && g.player == player)?;
        let id = g.id;
        if let Some(always) = block {
            let b = Block {
                tag_sha256: g.meta.who.tag_sha256.clone(),
                tag: g.meta.tag.clone(),
                addr: g.meta.who.addr.or(g.ip),
                cid: g.meta.who.cid.clone(),
                name: crate::join_names::effective(&g.name).to_string(),
                at_unix: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                always,
            };
            if always {
                jr::block_always(b.clone());
            }
            p.room.blocked.push(b);
        }
        Some(p.leave(id, "kicked", now))
    })
    .flatten();
    let ok = fx.is_some();
    apply(addr, fx.unwrap_or_default());
    ok
}

#[derive(serde::Deserialize)]
pub struct UnblockBody {
    #[serde(default, alias = "console", alias = "host")]
    addr: Option<String>,
    tag: String,
}

/// `POST /api/join/unblock {addr?, tag}`: lets them ask again (this room and this computer).
pub async fn unblock(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<UnblockBody>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    let ok = unblock_tag(b.addr.as_deref(), &b.tag);
    Json(serde_json::json!({ "ok": ok })).into_response()
}

/// Unblocks `tag` on this computer and in the room on `addr` (every room when None).
pub(crate) fn unblock_tag(addr: Option<&str>, tag: &str) -> bool {
    let mut ok = jr::unblock_always(tag);
    with_parties(|m| {
        for (k, p) in m.iter_mut() {
            if addr.is_none_or(|a| host_key(a) == *k) {
                let n = p.room.blocked.len();
                p.room.blocked.retain(|x| !jr::tag_is(x, tag));
                ok |= p.room.blocked.len() != n;
            }
        }
    });
    ok
}

#[derive(serde::Deserialize)]
pub struct SeatBody {
    #[serde(alias = "console", alias = "host")]
    addr: String,
    player: u8,
    /// 2-4: another pad; 1: give Player 1 (the stream's controller); "back": take Player 1 back.
    to: serde_json::Value,
}

/// `POST /api/join/seat`: Move to Player N, Give Player 1, Take Player 1 back.
pub async fn seat(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<SeatBody>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    match move_seat(&b.addr, b.player, &b.to).await {
        Ok(player) => Json(serde_json::json!({"ok": true, "player": player})).into_response(),
        Err(why) => conflict(why, serde_json::json!({})),
    }
}

/// What a seat change needs, checked under the lock.
enum SeatPlan {
    Back,
    Give(u64),
    Move { id: u64, from: u8, token: String },
}

pub(crate) async fn move_seat(
    addr: &str,
    player: u8,
    to: &serde_json::Value,
) -> Result<u8, &'static str> {
    let key = host_key(addr);
    let console = with_party(&key, |p| p.console.clone()).ok_or("not_open")?;
    let to_back = to.as_str() == Some("back");
    let to_n = to.as_u64().map(|n| n as u8);
    let seat_free = crate::rp_stream_api::stream_seat_free(&console);
    let plan = with_party(&key, |p| -> Result<SeatPlan, &'static str> {
        let g = p
            .guests
            .iter()
            .find(|g| g.active() && g.player == player)
            .ok_or("no_guest")?;
        if to_back {
            return if g.player == STREAM_SEAT {
                Ok(SeatPlan::Back)
            } else {
                Err("not_on_seat")
            };
        }
        let to = to_n.ok_or("bad")?;
        if to == STREAM_SEAT {
            if g.meta.stranger() {
                return Err("stranger");
            }
            if !p.settings.stream_seat {
                return Err("no_stream_seat");
            }
            if !seat_free
                || p.guests
                    .iter()
                    .any(|x| x.active() && x.player == STREAM_SEAT)
            {
                return Err("not_free");
            }
            return Ok(SeatPlan::Give(g.id));
        }
        let last = (vpad::FIRST_SLOT + p.settings.max_players.clamp(1, MAX_GUESTS) - 1)
            .min(vpad::LAST_SLOT);
        if !(vpad::FIRST_SLOT..=last).contains(&to)
            || p.guests.iter().any(|x| x.active() && x.player == to)
        {
            return Err("not_free");
        }
        if g.player == STREAM_SEAT {
            return Err("not_on_seat");
        }
        Ok(SeatPlan::Move {
            id: g.id,
            from: g.player,
            token: g.pad_token.clone(),
        })
    })
    .ok_or("not_open")??;
    match plan {
        SeatPlan::Back => {
            let fx =
                with_party(&key, |p| p.give_back_seat("the host took it back")).unwrap_or_default();
            apply(&console, fx);
            Ok(STREAM_SEAT)
        }
        SeatPlan::Give(id) => {
            let fx = with_party(&key, |p| {
                let g = p.guest_mut(id)?;
                let mut fx = Vec::new();
                if g.slot_open {
                    g.slot_open = false;
                    fx.push(Effect::Link(link::Cmd::Close {
                        slot: g.player,
                        token: g.pad_token.clone(),
                    }));
                }
                g.player = STREAM_SEAT;
                g.playing = true;
                g.cross_guard = false;
                fx.push(Effect::Seat(Some(id)));
                fx.push(Effect::Log(format!(
                    "join: the host gave the stream's controller on {console} to guest {id}"
                )));
                Some(fx)
            })
            .flatten()
            .ok_or("no_guest")?;
            apply(&console, fx);
            crate::rp_stream_api::guest_seat(&console, Some(id));
            Ok(STREAM_SEAT)
        }
        SeatPlan::Move { id, from, token } => {
            let to = to_n.unwrap_or(from);
            link::send(
                &console,
                link::Cmd::Close {
                    slot: from,
                    token: token.clone(),
                },
            );
            let seat = seat_profile(&console, to).filter(|s| {
                !(s.owner
                    && with_party(&key, |p| {
                        p.guests.iter().any(|g| g.id == id && g.meta.stranger())
                    })
                    .unwrap_or(true))
            });
            let r = open_slot(&console, to, &token, seat.as_ref()).await;
            let ok = r.is_ok();
            let fx = with_party(&key, |p| {
                let g = p.guest_mut(id)?;
                if ok {
                    g.player = to;
                    g.slot_open = true;
                    g.pad_made(&r, Some(seat.clone()));
                    let note = serde_json::json!({"seat": {"player": to, "was": from}});
                    g.conn
                        .as_ref()
                        .map(|c| vec![Effect::Say(c.tx.clone(), Out::Text(note))])
                } else {
                    g.slot_open = false;
                    None
                }
            })
            .flatten()
            .unwrap_or_default();
            apply(&console, fx);
            if ok {
                crate::log_at!(
                    Info,
                    "together",
                    "join: the host moved Player {from} to Player {to} on {console}"
                );
                Ok(to)
            } else {
                // Back where they were (best effort).
                let _ = open_slot(&console, from, &token, None).await;
                with_party(&key, |p| {
                    if let Some(g) = p.guest_mut(id) {
                        g.slot_open = true;
                    }
                });
                Err("failed")
            }
        }
    }
}

/// Previews answered (join_room.rs `PreviewLimit`).
static PREVIEWS: Mutex<Option<jr::PreviewLimit>> = Mutex::new(None);

/// The preview of the room on `key` (None: none open).
pub(crate) fn preview_of(key: &str) -> Option<serde_json::Value> {
    let now = Instant::now();
    let stream = live_codec_info(key);
    with_party(key, |p| {
        let mut players = vec![jr::PreviewPlayer {
            name: crate::join_names::effective(&p.host_name).to_string(),
            seat: 1,
            kind: "host",
            tier: None,
            rtt_ms: None,
        }];
        players.extend(p.guests.iter().filter(|g| g.active()).map(|g| {
            let (_, tier, _) = g.route_tier(now);
            jr::PreviewPlayer {
                name: crate::join_names::effective(&g.name).to_string(),
                seat: g.player,
                kind: match (g.client.kind.as_deref(), g.client.app.as_deref()) {
                    (Some("ps5"), _) => "ps5",
                    (_, Some(_)) => "app",
                    _ => "web",
                },
                tier,
                rtt_ms: g.stats.rtt_now(),
            }
        }));
        jr::preview(&jr::PreviewIn {
            host_name: crate::join_names::effective(&p.host_name).to_string(),
            host_tag: jr::host_tag(),
            cfg: p.settings.room.clone(),
            size: p.settings.max_players.clamp(1, MAX_GUESTS) + 1,
            password: jr::has_password(),
            game: p.room.game.clone(),
            stream: stream.clone(),
            host_link: None,
            players,
            asking: p.pending.len(),
            open_for: now.saturating_duration_since(p.room.opened_at),
            watchers: 0,
            locked: p.room.locked,
        })
    })
}

fn live_codec_info(key: &str) -> Option<serde_json::Value> {
    let console = with_party(key, |p| p.console.clone())?;
    let (codec, kbps) = crate::rp_stream_api::live_video(&console)?;
    Some(serde_json::json!({
        "codec": if codec == "h264" { "h264" } else { "hevc" },
        "hdr": codec == "h265_hdr",
        "kbps": kbps,
    }))
}

/// The open room a preview is for: the only one (or the one whose lobby id is `room`).
fn preview_key(room: Option<&str>) -> Option<String> {
    let lobby = crate::join_net::lobby::lobby_id();
    with_parties(|m| {
        if let Some(r) = room.filter(|r| !r.is_empty()) {
            if lobby.as_deref() != Some(r) {
                return None;
            }
        }
        (m.len() == 1).then(|| m.keys().next().cloned()).flatten()
    })
}

#[derive(serde::Deserialize, Default)]
pub struct PreviewQuery {
    #[serde(default)]
    room: Option<String>,
}

/// `GET /api/join/preview` (public on the home network and Tailscale).
pub async fn preview(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Query(q): Query<PreviewQuery>,
) -> Response {
    let now = Instant::now();
    if let Some(r) = crate::public_limit::PREVIEW.gate(peer.ip()) {
        return r;
    }
    if !peer.ip().is_loopback() {
        let r = PREVIEWS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(jr::PreviewLimit::default)
            .take(peer.ip(), now);
        if let Err(s) = r {
            return crate::public_limit::slow_down(s);
        }
    }
    let Some(key) = preview_key(q.room.as_deref()) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"why": "no_room"})),
        )
            .into_response();
    };
    match preview_of(&key) {
        Some(v) => Json(v).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"why": "no_room"})),
        )
            .into_response(),
    }
}

// ── The rooms list, through this engine (design 10.1 `GET /api/join/rooms`) ─────────────────────

/// The hub's HTTP base for its WebSocket address (`wss://hub/ws` → `https://hub`), this engine's
/// own built-in hub over loopback.
pub(crate) fn hub_http(ws_url: &str) -> Option<String> {
    let url = crate::builtin_hub::dial(ws_url).unwrap_or_else(|| ws_url.to_string());
    let (scheme, rest) = url
        .strip_prefix("wss://")
        .map(|r| ("https", r))
        .or_else(|| url.strip_prefix("ws://").map(|r| ("http", r)))?;
    let host = rest.split('/').next()?;
    (!host.is_empty()).then(|| format!("{scheme}://{host}"))
}

/// The hub said "slow down" until then: asks wait it out here instead of going out.
static HUB_WAIT: Mutex<Option<Instant>> = Mutex::new(None);
/// Answers kept a few seconds by query (the hub caches too; this spares the trip).
type HubCache = HashMap<String, (Instant, u16, String)>;
static HUB_CACHE: Mutex<Option<HubCache>> = Mutex::new(None);
const HUB_CACHE_FOR: Duration = Duration::from_secs(5);

#[derive(serde::Deserialize, Default)]
pub struct RoomsQuery {
    /// Friends lookup: lobby ids (comma separated, at most 16).
    #[serde(default)]
    status: Option<String>,
    /// Details through the hub: one lobby id.
    #[serde(default)]
    preview: Option<String>,
}

fn lobby_ids_ok(s: &str) -> bool {
    s.len() <= 16 * 23
        && s.split(',').all(|id| {
            id.len() == 22
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

/// The hub path for a proxied ask (None: a bad one).
pub(crate) fn rooms_path(q: &RoomsQuery, query: Option<&str>) -> Option<String> {
    if let Some(ids) = q.status.as_deref() {
        return lobby_ids_ok(ids).then(|| format!("/rooms/status?r={ids}"));
    }
    if let Some(id) = q.preview.as_deref() {
        return (lobby_ids_ok(id) && !id.contains(',')).then(|| format!("/rooms/{id}/preview"));
    }
    // The list's own query, passed on (only its known keys, each short).
    let keep: Vec<&str> = query
        .unwrap_or("")
        .split('&')
        .filter(|kv| {
            let k = kv.split('=').next().unwrap_or("");
            matches!(
                k,
                "sort"
                    | "game"
                    | "free"
                    | "nopw"
                    | "proto"
                    | "lang"
                    | "region"
                    | "near"
                    | "me"
                    | "page"
            ) && kv.len() <= 80
        })
        .collect();
    Some(if keep.is_empty() {
        "/rooms".to_string()
    } else {
        format!("/rooms?{}", keep.join("&"))
    })
}

/// `GET /api/join/rooms?<the hub's /rooms query>` (the list), `?status=<ids>` (friends lookup:
/// `/rooms/status`), `?preview=<id>` (`/rooms/<id>/preview`): the hub in the settings, for this
/// engine's own UIs (one place for the custom hub and caching). A hub's 429 comes back as it was
/// (`Retry-After`), and further asks wait it out here (with a little jitter) instead of going out.
pub async fn rooms_proxy(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    Query(q): Query<RoomsQuery>,
) -> Response {
    if let Some(r) = guard(&peer, &headers) {
        return r;
    }
    let Some(base) = hub_http(&settings().hub_url) else {
        return bad(StatusCode::BAD_GATEWAY, "hub");
    };
    let Some(path) = rooms_path(&q, uri.query()) else {
        return bad(StatusCode::BAD_REQUEST, "bad");
    };
    let now = Instant::now();
    if let Some(t) = *HUB_WAIT.lock().unwrap_or_else(|e| e.into_inner()) {
        if t > now {
            return crate::public_limit::slow_down(t.duration_since(now).as_secs().max(1));
        }
    }
    let cached = HUB_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&path).cloned());
    if let Some((at, code, body)) = cached {
        if now.saturating_duration_since(at) < HUB_CACHE_FOR {
            return hub_answer(code, body, None);
        }
    }
    let url = format!("{base}{path}");
    let got = tokio::task::spawn_blocking(move || {
        let cfg = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(6)))
            .build();
        let agent = ureq::Agent::new_with_config(cfg);
        let mut r = agent.get(&url).call().map_err(|e| e.to_string())?;
        let code = r.status().as_u16();
        let retry = r
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let body = r
            .body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_to_string()
            .map_err(|e| e.to_string())?;
        Ok::<_, String>((code, retry, body))
    })
    .await;
    match got {
        Ok(Ok((code, retry, body))) => {
            if code == 429 {
                let wait = Duration::from_secs(retry.unwrap_or(10).clamp(1, 600));
                let mut b = [0u8; 1];
                let _ = getrandom::fill(&mut b);
                let jit = Duration::from_millis(u64::from(b[0]) * 8);
                *HUB_WAIT.lock().unwrap_or_else(|e| e.into_inner()) = Some(now + wait + jit);
                return hub_answer(code, body, Some(wait.as_secs()));
            }
            if code == 200 {
                let mut g = HUB_CACHE.lock().unwrap_or_else(|e| e.into_inner());
                let m = g.get_or_insert_with(HashMap::new);
                if m.len() > 128 {
                    m.clear();
                }
                m.insert(path, (now, code, body.clone()));
            }
            hub_answer(code, body, None)
        }
        _ => bad(StatusCode::BAD_GATEWAY, "hub"),
    }
}

fn hub_answer(code: u16, body: String, retry: Option<u64>) -> Response {
    let mut r = (
        StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY),
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response();
    if let Some(s) = retry {
        if let Ok(v) = axum::http::HeaderValue::from_str(&s.to_string()) {
            r.headers_mut().insert(axum::http::header::RETRY_AFTER, v);
        }
    }
    r
}

// ── The hosted-by marker on the console (helper `play.room`, method 203) ────────────────────────

/// The helper keeps a room this long unless told again (`ttl_s`); it is sent on every change and
/// at least every third of it.
pub(crate) const MARKER_TTL_S: u64 = 90;
const MARKER_LOOK: Duration = Duration::from_secs(if cfg!(test) { 1 } else { 5 });
const MARKER_OLD_HELPER: Duration = Duration::from_secs(30 * 60);

fn bars(tier: Option<&str>) -> u8 {
    match tier {
        Some("excellent") => 4,
        Some("good") => 3,
        Some("playable") => 2,
        Some("poor") => 1,
        _ => 0,
    }
}

/// The room as the console's helper shows it to Minion and PS5Upload Play (`play.room` R: the
/// helper's contract in docs/ROOMS_DEVIATIONS.md). Never an address.
pub(crate) fn marker_json(p: &Party, now: Instant) -> serde_json::Value {
    let cfg = &p.settings.room;
    let comms = crate::join_comms::modes(&p.console);
    let players: Vec<serde_json::Value> = p
        .guests
        .iter()
        .filter(|g| g.active())
        .take(8)
        .map(|g| {
            let (route, tier, _) = g.route_tier(now);
            serde_json::json!({
                "name": crate::join_names::effective(&g.name),
                "ping_ms": g.stats.rtt_now(),
                "seat": g.player,
                "bars": bars(tier),
                "via": route,
            })
        })
        .collect();
    serde_json::json!({
        "name": cfg.name_for(&p.host_name),
        "visibility": cfg.visibility.word(),
        "locked": p.room.locked,
        "game": p.room.game.as_ref().map(|g| serde_json::json!({"name": g.title, "title_id": g.id})),
        "size": p.settings.max_players.clamp(1, MAX_GUESTS) + 1,
        "players": players,
        "playing": p.host_gone.is_none(),
        "asks": p.pending.len(),
        "chat": comms.chat.word(),
        "voice": comms.voice.word(),
    })
}

/// Keeps the helper on `console` told about its room while it is open, and `{room: null}` once
/// it closes. Runs beside the party's own watch; an older helper is asked again after a while.
pub(crate) fn spawn_marker(key: String, console: String) {
    if cfg!(test) {
        return;
    }
    tokio::spawn(async move {
        let mut sent: Option<(serde_json::Value, Instant)> = None;
        let mut old_until: Option<Instant> = None;
        let mut t = tokio::time::interval(MARKER_LOOK);
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            t.tick().await;
            let now = Instant::now();
            let room = with_party(&key, |p| marker_json(p, now));
            let open = room.is_some();
            if old_until.is_some_and(|u| u > now) {
                if !open {
                    return;
                }
                continue;
            }
            let due = match (&sent, &room) {
                (_, None) => true,
                (None, Some(_)) => true,
                (Some((v, at)), Some(r)) => {
                    v != r || now.saturating_duration_since(*at).as_secs() >= MARKER_TTL_S / 3
                }
            };
            if due {
                let body = serde_json::json!({"room": room, "ttl_s": MARKER_TTL_S});
                let c = console.clone();
                let r = tokio::task::spawn_blocking(move || {
                    ps5upload_core::mgmt::call(
                        &c,
                        ps5upload_core::mgmt::m::PLAY_ROOM,
                        body.to_string().as_bytes(),
                    )
                })
                .await;
                match r {
                    Ok(Ok(_)) => sent = room.clone().map(|v| (v, now)),
                    Ok(Err(e)) if ps5upload_core::console_files::is_unsupported(&e) => {
                        crate::log_at!(
                            Debug,
                            "together",
                            "room: the helper on {console} is too old for play.room"
                        );
                        old_until = Some(now + MARKER_OLD_HELPER);
                    }
                    Ok(Err(e)) => {
                        crate::log_at!(Debug, "together", "room: play.room to {console}: {e:#}");
                    }
                    Err(_) => {}
                }
            }
            if !open {
                return;
            }
        }
    });
}
