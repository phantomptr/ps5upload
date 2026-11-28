//! Play Rooms chat and voice in the host engine: who is in the room's talk, what the host allows,
//! and the per-person queues their sockets send from. The rules for a line are in join_chat.rs,
//! the voice frames and levels in join_voice.rs.
//!
//! Everything rides each person's existing connection: a guest's `/api/join/ws` socket (or their
//! internet data channel, with voice on its own unreliable `voice` channel when the page opened
//! one), and the host page's `/api/ps5/rp/ws` socket. **Nothing touches the hub. Nothing is
//! recorded, stored or logged** beyond counts: chat lines live in a 50-line ring in memory for the
//! room's life; voice frames live only in a listener's queue for at most 60 ms.
//!
//! **Messages, client → engine** (text JSON on the guest socket; the host page's socket takes the
//! same, with `room_mic` for `mic`, whose name it already uses for the console's own mic):
//!
//! - `{"notice": 1}`: the person accepted the responsibility notice of that version. Until they
//!   do, they send and get no chat or voice (a refusal says `"why": "notice"` and the version, which
//!   the client turns into the notice). Rooms with chat and voice off never need it.
//! - `{"chat": {"quick": 1-16}}` or `{"chat": {"text": "…"}}`.
//! - `{"mic": "on" | "off" | "ptt_down" | "ptt_up"}`: the mic switch (off on every join) and the
//!   push-to-talk key. Binary voice frames: join_voice.rs.
//! - `{"flag": sid}`: "Tell the host": the host's views get one private line, once per person
//!   per room.
//!
//! **Messages, engine → client:**
//!
//! - `{"comms": {"notice": 1, "accepted": bool, "chat": "off|quick|on", "voice": "off|ptt|open",
//!   "caps": ["chat", "voice"]}}`: on joining and whenever the host changes it.
//! - `{"voice": {"mode", "you": sid, "mic": "on|off", "ptt": bool, "paused": null | "poor",
//!   "stepped": bool, "kbps": 24|12|0, "frame_ms": 20|40|0, "speaking": [sid…],
//!   "people": [{"sid", "name", "player", "host", "muted", "paused", "mic"}]}}`: the full voice
//!   state, on joining and on every change; `{"voice": {"speaking": [sid…]}}` alone when only
//!   who speaks changed (at most 4 a second). The client encodes at `kbps` / `frame_ms`.
//! - `{"muted": {"voice": bool, "chat": bool}}`: the host's mutes on this person.
//! - `{"chat": {"id", "sid", "from", "player", "host", "t", "quick" | "text", "x"?}}`: a line
//!   (`x` > 1: the same line again, folded; replace the line with this `id`), to everyone who
//!   accepted the notice, its sender included. `{"chat_history": [line…]}` (the last 20) after
//!   accepting.
//! - `{"comms_refused": {"what": "chat" | "voice" | "mic" | "notice", "why", "notice"?,
//!   "retry_ms"?}}`: at most one per reason a second (voice: every 5 s).
//! - `{"flagged": {"by", "by_sid", "who", "who_sid"}}`: host views only.
//!
//! **Host controls** (`POST /api/join/comms`, `/api/join/mute`, below; enforced here: a muted
//! person's voice is dropped, their lines refused): Voice Off / Push-to-talk only / Open mic,
//! Chat Off / Quick messages only / On, new players start muted, mute all, per-player mutes.
//!
//! Hooks (kept small for the room model's merge): join.rs `play` attaches each guest with
//! [`attach`] and feeds [`Link`] its text, voice frames and tier; rp_stream_api.rs attaches the host
//! page the same way; join.rs calls [`room_closed`] when a room ends and [`extend_state`] at the end
//! of the host's `GET /api/join` answer. The room model sets the modes with [`set_modes`].

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::Message;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use tokio::sync::Notify;

use crate::join_chat::{self, Body, Bucket, ChatMode, Refusal, Ring, Speaker};
use crate::join_route::Tier;
use crate::join_voice::{self, Adapt, Frame, Level, Pending, Rate, Take};
use crate::vinput_api::host_key;

/// The responsibility notice's version: a person must have accepted this one (or later) before
/// they send or get chat or voice. Bumped when the notice's text changes.
pub(crate) const NOTICE_VERSION: u32 = 1;
/// The host's own person (every host page and the host API share it).
pub(crate) const HOST_PID: u64 = 1 << 61;
/// Text messages waiting for one link at most (the oldest goes).
const TEXT_QUEUE: usize = 64;
/// Refusals for the same reason at most this often (voice: [`VOICE_REFUSE_GAP`]).
const REFUSE_GAP: Duration = Duration::from_secs(1);
const VOICE_REFUSE_GAP: Duration = Duration::from_secs(5);
/// Who speaks is looked at this often while anyone does.
const SPEAKING_EVERY: Duration = Duration::from_millis(250);

/// What a room allows for voice (host setting).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum VoiceMode {
    Off,
    /// Only while someone holds their talk key (and 300 ms after).
    Ptt,
    /// Open mic allowed.
    Open,
}

impl VoiceMode {
    pub(crate) fn word(self) -> &'static str {
        match self {
            VoiceMode::Off => "off",
            VoiceMode::Ptt => "ptt",
            VoiceMode::Open => "open",
        }
    }
}

/// The host's chat and voice settings for a room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Modes {
    pub(crate) chat: ChatMode,
    pub(crate) voice: VoiceMode,
    /// New players join with their voice muted by the host.
    pub(crate) start_muted: bool,
    /// A Public room: invite codes are stripped from chat.
    pub(crate) public: bool,
}

impl Modes {
    /// The design's defaults: Public is push-to-talk, quick messages, new players muted; Private
    /// and Friends are open mic, chat on.
    pub(crate) fn defaults(public: bool) -> Modes {
        if public {
            Modes {
                chat: ChatMode::Quick,
                voice: VoiceMode::Ptt,
                start_muted: true,
                public: true,
            }
        } else {
            Modes {
                chat: ChatMode::On,
                voice: VoiceMode::Open,
                start_muted: false,
                public: false,
            }
        }
    }
}

/// One socket's outgoing queues.
#[derive(Default)]
struct LinkQ {
    pending: Pending,
    /// The speakers this listener hears now (a bundle with all of them goes at once).
    speakers: Vec<u8>,
    /// Their voice is at the Playable level: 40 ms bundles.
    low: bool,
    text: VecDeque<String>,
    text_dropped: u64,
    /// Someone speaks: look again at who in [`SPEAKING_EVERY`].
    watch: bool,
}

struct LinkShared {
    q: Mutex<LinkQ>,
    notify: Notify,
}

impl LinkShared {
    fn text(&self, v: &serde_json::Value) {
        {
            let mut q = lock(&self.q);
            if q.text.len() >= TEXT_QUEUE {
                q.text.pop_front();
                q.text_dropped += 1;
            }
            q.text.push_back(v.to_string());
        }
        self.notify.notify_one();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone, Debug, Default)]
struct Drops {
    /// Frames from a person the host muted, or with voice off, the mic off, no key held, no notice.
    refused: u64,
    /// Over their frame rate.
    rate: u64,
    /// The room's voice budget was full and they were the quietest.
    cap: u64,
    /// Paused (Poor).
    paused: u64,
}

struct Person {
    sid: u8,
    name: String,
    player: Option<u8>,
    host: bool,
    accepted: bool,
    mic_on: bool,
    ptt_down: bool,
    ptt_up_at: Option<Instant>,
    muted_voice: bool,
    muted_chat: bool,
    adapt: Adapt,
    chat_bucket: Bucket,
    frames_bucket: Bucket,
    refused_at: HashMap<&'static str, Instant>,
    /// Their last frame forwarded (speaking), and their last that passed the checks (talking,
    /// heard or not: the room's budget compares those).
    last_frame: Option<Instant>,
    last_try: Option<Instant>,
    level_avg: f32,
    /// The speakers this person hears (at most 3).
    slots: [Option<u8>; join_voice::MAX_SPEAKERS],
    flagged: HashSet<u8>,
    v_in: Rate,
    v_out: Rate,
    frames_in: u64,
    frames_out: u64,
    drops: Drops,
    lines: u64,
    links: Vec<(u64, Arc<LinkShared>)>,
}

impl Person {
    fn new(sid: u8, name: &str, player: Option<u8>, host: bool, now: Instant) -> Self {
        Person {
            sid,
            name: name.to_string(),
            player,
            host,
            accepted: false,
            mic_on: false,
            ptt_down: false,
            ptt_up_at: None,
            muted_voice: false,
            muted_chat: false,
            adapt: Adapt::default(),
            chat_bucket: Bucket::new(join_chat::CHAT_BURST, join_chat::CHAT_PER_S, now),
            frames_bucket: Bucket::new(join_voice::FRAMES_BURST, join_voice::FRAMES_PER_S, now),
            refused_at: HashMap::new(),
            last_frame: None,
            last_try: None,
            level_avg: 0.0,
            slots: [None; join_voice::MAX_SPEAKERS],
            flagged: HashSet::new(),
            v_in: Rate::default(),
            v_out: Rate::default(),
            frames_in: 0,
            frames_out: 0,
            drops: Drops::default(),
            lines: 0,
            links: Vec::new(),
        }
    }

    fn here(&self) -> bool {
        !self.links.is_empty()
    }

    fn say(&self, v: &serde_json::Value) {
        for (_, l) in &self.links {
            l.text(v);
        }
    }

    /// Speaking now: a frame went through within the hangover.
    fn speaking(&self, now: Instant) -> bool {
        self.last_frame
            .is_some_and(|t| now.saturating_duration_since(t) <= join_voice::HANGOVER)
    }

    fn paused(&self) -> bool {
        self.adapt.level() == Level::Paused
    }

    /// A refusal to tell this person, unless the same was told within the gap.
    fn refuse(&mut self, what: &'static str, r: Refusal, now: Instant) {
        let gap = if what == "voice" {
            VOICE_REFUSE_GAP
        } else {
            REFUSE_GAP
        };
        let key = r.word();
        if self
            .refused_at
            .get(key)
            .is_some_and(|t| now.saturating_duration_since(*t) < gap)
        {
            return;
        }
        self.refused_at.insert(key, now);
        self.say(&refusal_json(what, r));
    }
}

fn refusal_json(what: &str, r: Refusal) -> serde_json::Value {
    let mut v = serde_json::json!({"what": what, "why": r.word()});
    match r {
        Refusal::Notice => v["notice"] = NOTICE_VERSION.into(),
        Refusal::SlowDown(d) => v["retry_ms"] = (d.as_millis() as u64).max(1).into(),
        _ => {}
    }
    serde_json::json!({ "comms_refused": v })
}

struct Room {
    modes: Modes,
    /// Everyone in the room's talk by person id (guest id, or [`HOST_PID`]), kept for the room's
    /// life so a reconnect keeps its mutes, notice and speaker id.
    people: BTreeMap<u64, Person>,
    ring: Ring,
    out: Rate,
    t0: Instant,
    speaking: Vec<u8>,
    last_hk: Option<Instant>,
    next_sid: u8,
}

impl Room {
    fn new(now: Instant) -> Self {
        Room {
            modes: Modes::defaults(false),
            people: BTreeMap::new(),
            ring: Ring::new(now),
            out: Rate::default(),
            t0: now,
            speaking: Vec::new(),
            last_hk: None,
            next_sid: 1,
        }
    }

    fn sec(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.t0).as_secs()
    }

    /// A free speaker id for a guest (the host is 0): the next unused, else one of someone gone.
    fn new_sid(&mut self) -> u8 {
        for _ in 0..255 {
            let s = self.next_sid;
            self.next_sid = if self.next_sid == u8::MAX {
                1
            } else {
                self.next_sid + 1
            };
            if !self.people.values().any(|p| p.sid == s) {
                return s;
            }
        }
        // Every id is someone's: take one whose person left.
        let gone = self
            .people
            .iter()
            .find(|(_, p)| !p.here() && !p.host)
            .map(|(id, p)| (*id, p.sid));
        match gone {
            Some((id, s)) => {
                self.people.remove(&id);
                s
            }
            None => u8::MAX,
        }
    }

    fn comms_json(&self, p: &Person) -> serde_json::Value {
        serde_json::json!({"comms": {
            "notice": NOTICE_VERSION,
            "accepted": p.accepted,
            "chat": self.modes.chat.word(),
            "voice": self.modes.voice.word(),
            "caps": ["chat", "voice"],
        }})
    }

    fn people_json(&self) -> Vec<serde_json::Value> {
        self.people
            .values()
            .filter(|p| p.here())
            .map(|p| {
                serde_json::json!({
                    "sid": p.sid, "name": p.name, "player": p.player, "host": p.host,
                    "muted": p.muted_voice, "paused": p.paused(), "mic": p.mic_on,
                })
            })
            .collect()
    }

    fn voice_json(&self, p: &Person, now: Instant) -> serde_json::Value {
        let level = p.adapt.level();
        serde_json::json!({"voice": {
            "mode": self.modes.voice.word(),
            "you": p.sid,
            "mic": if p.mic_on { "on" } else { "off" },
            "ptt": p.ptt_down,
            "paused": if level == Level::Paused { Some("poor") } else { None },
            "stepped": p.adapt.stepped(now),
            "kbps": level.kbps(),
            "frame_ms": level.frame_ms(),
            "speaking": self.speaking,
            "people": self.people_json(),
        }})
    }

    fn muted_json(p: &Person) -> serde_json::Value {
        serde_json::json!({"muted": {"voice": p.muted_voice, "chat": p.muted_chat}})
    }

    /// Everyone's full voice state again (a mode, mute, mic or level changed, someone came or went).
    fn voice_to_all(&self, now: Instant) {
        for p in self.people.values().filter(|p| p.here()) {
            p.say(&self.voice_json(p, now));
        }
    }

    fn comms_to_all(&self) {
        for p in self.people.values().filter(|p| p.here()) {
            p.say(&self.comms_json(p));
        }
    }

    /// Sids speaking now (who passed a frame within the hangover).
    fn speaking_now(&self, now: Instant) -> Vec<u8> {
        self.people
            .values()
            .filter(|p| p.speaking(now))
            .map(|p| p.sid)
            .collect()
    }

    /// Who speaks changed: tell everyone who hears voice, and keep looking while anyone does.
    fn housekeep(&mut self, now: Instant) {
        self.last_hk = Some(now);
        let now_speaking = self.speaking_now(now);
        if now_speaking != self.speaking {
            self.speaking = now_speaking;
            let v = serde_json::json!({"voice": {"speaking": self.speaking}});
            for p in self.people.values().filter(|p| p.accepted && p.here()) {
                p.say(&v);
            }
        }
        let watch = !self.speaking.is_empty();
        for p in self.people.values() {
            for (_, l) in &p.links {
                let mut q = lock(&l.q);
                q.watch = watch;
                // The speakers each listener hears, for its early bundle.
                q.speakers = p
                    .slots
                    .iter()
                    .flatten()
                    .copied()
                    .filter(|s| self.speaking.contains(s))
                    .collect();
            }
        }
    }

    /// May this person's voice go now?
    fn voice_gate(&self, p: &Person, now: Instant) -> Result<(), Refusal> {
        if self.modes.voice == VoiceMode::Off {
            return Err(Refusal::Off);
        }
        if !p.accepted {
            return Err(Refusal::Notice);
        }
        if p.muted_voice {
            return Err(Refusal::Muted);
        }
        if !p.mic_on {
            return Err(Refusal::MicOff);
        }
        if self.modes.voice == VoiceMode::Ptt
            && !p.ptt_down
            && !p
                .ptt_up_at
                .is_some_and(|t| now.saturating_duration_since(t) <= join_voice::HANGOVER)
        {
            return Err(Refusal::PttOnly);
        }
        if p.paused() {
            return Err(Refusal::Poor);
        }
        Ok(())
    }

    /// A speaker's frame: checked, then to each listener who hears them (at most 3 speakers each).
    fn voice_in(&mut self, pid: u64, up: join_voice::Up<'_>, now: Instant) -> Result<(), Refusal> {
        let sec = self.sec(now);
        let Some(p) = self.people.get(&pid) else {
            return Ok(());
        };
        if let Err(r) = self.voice_gate(p, now) {
            let p = self.people.get_mut(&pid).expect("looked up above");
            if r == Refusal::Poor {
                p.drops.paused += 1;
            } else {
                p.drops.refused += 1;
            }
            return Err(r);
        }
        let p = self.people.get_mut(&pid).expect("looked up above");
        if let Err(d) = p.frames_bucket.take(now) {
            p.drops.rate += 1;
            return Err(Refusal::SlowDown(d));
        }
        let started = !p.speaking(now);
        let talking = p
            .last_try
            .is_some_and(|t| now.saturating_duration_since(t) <= join_voice::HANGOVER);
        if started {
            p.adapt.voice_started(now);
        }
        p.level_avg = if talking {
            p.level_avg * 0.8 + f32::from(up.level) * 0.2
        } else {
            f32::from(up.level)
        };
        p.last_try = Some(now);
        p.v_in.add(sec, up.opus.len() + 4);
        p.frames_in += 1;
        let sid = p.sid;
        // The room's budget: the quietest speaker goes first.
        if self.out.bps(sec) > join_voice::ROOM_CAP_BPS {
            let quietest = self
                .people
                .values()
                .filter(|q| {
                    q.last_try
                        .is_some_and(|t| now.saturating_duration_since(t) <= join_voice::HANGOVER)
                })
                .min_by(|a, b| a.level_avg.total_cmp(&b.level_avg))
                .map(|q| q.sid);
            if quietest == Some(sid) {
                let p = self.people.get_mut(&pid).expect("looked up above");
                p.drops.cap += 1;
                return Ok(());
            }
        }
        let p = self.people.get_mut(&pid).expect("looked up above");
        p.last_frame = Some(now);
        let active: HashSet<u8> = self
            .people
            .values()
            .filter(|q| q.speaking(now) && self.voice_gate(q, now).is_ok())
            .map(|q| q.sid)
            .collect();
        let frame = Frame {
            sid,
            seq: up.seq,
            opus: axum::body::Bytes::copy_from_slice(up.opus),
            at: now,
        };
        let bytes = up.opus.len() + join_voice::ENTRY_OVERHEAD;
        let mut sent = 0usize;
        for (id, l) in self.people.iter_mut() {
            if *id == pid || !l.accepted || !l.here() || l.paused() {
                continue;
            }
            if !l.slots.contains(&Some(sid)) {
                match l
                    .slots
                    .iter_mut()
                    .find(|s| s.is_none_or(|s| !active.contains(&s)))
                {
                    Some(s) => *s = Some(sid),
                    // Three others speak to them already: this one waits.
                    None => continue,
                }
            }
            let speakers: Vec<u8> = l
                .slots
                .iter()
                .flatten()
                .copied()
                .filter(|s| active.contains(s))
                .collect();
            let low = l.adapt.level() == Level::Low;
            for (_, link) in &l.links {
                {
                    let mut q = lock(&link.q);
                    q.pending.push(frame.clone());
                    q.speakers.clone_from(&speakers);
                    q.low = low;
                }
                link.notify.notify_one();
            }
            l.v_out.add(sec, bytes);
            l.frames_out += 1;
            sent += bytes;
        }
        self.out.add(sec, sent);
        if started {
            self.housekeep(now);
        }
        Ok(())
    }

    fn chat_in(&mut self, pid: u64, body: Body, now: Instant) -> Result<(), Refusal> {
        let modes = self.modes;
        let Some(p) = self.people.get_mut(&pid) else {
            return Ok(());
        };
        if !p.accepted {
            return Err(Refusal::Notice);
        }
        join_chat::allowed(modes.chat, &body)?;
        if p.muted_chat {
            return Err(Refusal::Muted);
        }
        let body = match body {
            Body::Text(t) => Body::Text(join_chat::clean_text(&t, modes.public)?),
            q => q,
        };
        p.chat_bucket.take(now).map_err(Refusal::SlowDown)?;
        p.lines += 1;
        let who = Speaker {
            pid,
            sid: p.sid,
            name: &p.name,
            player: p.player,
            host: p.host,
        };
        let line = self.ring.push(who, body, now).json();
        let v = serde_json::json!({ "chat": line });
        for q in self.people.values().filter(|q| q.accepted && q.here()) {
            q.say(&v);
        }
        Ok(())
    }

    fn mic(&mut self, pid: u64, word: &str, now: Instant) -> Result<(), Refusal> {
        let modes = self.modes;
        let Some(p) = self.people.get_mut(&pid) else {
            return Ok(());
        };
        let (on, ptt) = (p.mic_on, p.ptt_down);
        match word {
            "off" => {
                p.mic_on = false;
                p.ptt_down = false;
            }
            "on" | "ptt_down" => {
                if word == "ptt_down" && !p.mic_on {
                    return Err(Refusal::MicOff);
                }
                if modes.voice == VoiceMode::Off {
                    return Err(Refusal::Off);
                }
                if !p.accepted {
                    return Err(Refusal::Notice);
                }
                if p.muted_voice {
                    return Err(Refusal::Muted);
                }
                p.mic_on = true;
                if word == "ptt_down" {
                    p.ptt_down = true;
                }
            }
            "ptt_up" => {
                if p.ptt_down {
                    p.ptt_down = false;
                    p.ptt_up_at = Some(now);
                }
            }
            _ => return Ok(()),
        }
        let (on_now, ptt_now) = (p.mic_on, p.ptt_down);
        // The mic switch changes what the others see; the key only this person's own state.
        if on_now != on {
            self.voice_to_all(now);
        } else if ptt_now != ptt {
            let p = &self.people[&pid];
            p.say(&self.voice_json(p, now));
        }
        Ok(())
    }

    fn accept(&mut self, pid: u64, version: u32, now: Instant) -> Result<(), Refusal> {
        if version < NOTICE_VERSION {
            return Err(Refusal::Notice);
        }
        let Some(p) = self.people.get_mut(&pid) else {
            return Ok(());
        };
        if p.accepted {
            return Ok(());
        }
        p.accepted = true;
        let p = &self.people[&pid];
        p.say(&self.comms_json(p));
        p.say(&serde_json::json!({
            "chat_history": self.ring.last(join_chat::HISTORY_ON_JOIN)
        }));
        let _ = now;
        Ok(())
    }

    fn flag(&mut self, pid: u64, who: u8) {
        let Some(target) = self.people.values().find(|p| p.sid == who) else {
            return;
        };
        if target.host {
            return;
        }
        let (who_name, who_sid) = (target.name.clone(), target.sid);
        let Some(p) = self.people.get_mut(&pid) else {
            return;
        };
        if p.sid == who || !p.flagged.insert(who) {
            return;
        }
        let v = serde_json::json!({"flagged": {
            "by": p.name, "by_sid": p.sid, "who": who_name, "who_sid": who_sid,
        }});
        for h in self.people.values().filter(|h| h.host) {
            h.say(&v);
        }
    }

    fn tick(&mut self, pid: u64, tier: Option<Tier>, player: Option<u8>, now: Instant) {
        let Some(p) = self.people.get_mut(&pid) else {
            return;
        };
        let mut changed = false;
        if player.is_some() && p.player != player {
            p.player = player;
            changed = true;
        }
        if p.adapt.on_tier(tier, now) {
            changed = true;
            if p.paused() {
                // Nothing waits for someone paused.
                for (_, l) in &p.links {
                    lock(&l.q).pending.clear();
                }
            }
        }
        if changed {
            self.voice_to_all(now);
        }
    }
}

static ROOMS: Mutex<Option<HashMap<String, Room>>> = Mutex::new(None);

fn with_room<R>(console: &str, f: impl FnOnce(&mut Room) -> R) -> Option<R> {
    let mut g = lock(&ROOMS);
    g.get_or_insert_with(HashMap::new)
        .get_mut(&host_key(console))
        .map(f)
}

fn with_room_or_new<R>(console: &str, f: impl FnOnce(&mut Room) -> R) -> R {
    let mut g = lock(&ROOMS);
    let room = g
        .get_or_insert_with(HashMap::new)
        .entry(host_key(console))
        .or_insert_with(|| Room::new(Instant::now()));
    f(room)
}

/// One person's socket on the room's talk: what it sends next, and what it got.
pub(crate) struct Link {
    console: String,
    pid: u64,
    gen: u64,
    shared: Arc<LinkShared>,
    host: bool,
}

/// Puts a person's socket on the room's talk: a guest (`pid` = their guest id) or a host page
/// ([`HOST_PID`], `host`). The person keeps their mutes, notice and speaker id across reconnects
/// for the room's life. They are told the room's settings and their voice state at once.
pub(crate) fn attach(console: &str, pid: u64, name: &str, player: Option<u8>, host: bool) -> Link {
    static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let gen = GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let shared = Arc::new(LinkShared {
        q: Mutex::new(LinkQ::default()),
        notify: Notify::new(),
    });
    let now = Instant::now();
    with_room_or_new(console, |room| {
        if !room.people.contains_key(&pid) {
            let sid = if host { 0 } else { room.new_sid() };
            let mut p = Person::new(sid, name, player, host, now);
            p.muted_voice = !host && room.modes.start_muted;
            room.people.insert(pid, p);
        }
        let p = room.people.get_mut(&pid).expect("inserted above");
        p.links.push((gen, shared.clone()));
        if player.is_some() {
            p.player = player;
        }
        let p = &room.people[&pid];
        let guests = room.people.values().any(|q| !q.host && q.here());
        // A host page with no guests in hears nothing yet (Remote Play alone stays as it was);
        // the first guest brings it the room's state.
        if !host || guests {
            let first = [
                room.comms_json(p),
                Room::muted_json(p),
                room.voice_json(p, now),
            ];
            for v in &first {
                shared.text(v);
            }
            if p.accepted {
                shared.text(&serde_json::json!({
                    "chat_history": room.ring.last(join_chat::HISTORY_ON_JOIN)
                }));
            }
        }
        // The others see them in the voice list (host pages get the room's settings with it).
        for (id, q) in &room.people {
            if *id != pid && q.here() {
                if q.host && !host {
                    q.say(&room.comms_json(q));
                }
                q.say(&room.voice_json(q, now));
            }
        }
    });
    Link {
        console: console.to_string(),
        pid,
        gen,
        shared,
        host,
    }
}

impl Link {
    /// The next message for this socket: a voice bundle when one is due, else a text message.
    /// Waits until there is one (cancel-safe: nothing is taken until it returns). `congested`:
    /// the socket's picture queue is not empty (bundles go every 40 ms then).
    pub(crate) async fn next(&mut self, congested: bool) -> Message {
        loop {
            let now = Instant::now();
            let mut wake: Option<Instant> = None;
            let watch;
            {
                let mut q = lock(&self.shared.q);
                let LinkQ {
                    pending,
                    speakers,
                    low,
                    ..
                } = &mut *q;
                match pending.take(now, speakers, congested || *low) {
                    Take::Bundle(b) => return Message::Binary(b.into()),
                    Take::Wait(t) => wake = Some(t),
                    Take::Empty => {}
                }
                if let Some(t) = q.text.pop_front() {
                    return Message::Text(t.into());
                }
                watch = q.watch;
            }
            if watch {
                let t = now + SPEAKING_EVERY;
                wake = Some(wake.map_or(t, |w| w.min(t)));
            }
            let notified = self.shared.notify.notified();
            match wake {
                Some(t) => {
                    tokio::select! {
                        _ = notified => {}
                        _ = tokio::time::sleep_until(t.into()) => {
                            if watch {
                                housekeep(&self.console, Instant::now());
                            }
                        }
                    }
                }
                None => notified.await,
            }
        }
    }

    /// A text message from the socket: acts on the keys it knows (`notice`, `chat`, `mic` — on a
    /// host page `room_mic` — and `flag`), ignores the rest.
    pub(crate) fn on_text(&self, t: &str) {
        // Most messages are pings and pad states: only parse what may be ours.
        let mic_key = if self.host { "\"room_mic\"" } else { "\"mic\"" };
        if !(t.contains("\"notice\"")
            || t.contains("\"chat\"")
            || t.contains(mic_key)
            || t.contains("\"flag\""))
        {
            return;
        }
        #[derive(serde::Deserialize)]
        struct In {
            #[serde(default)]
            notice: Option<serde_json::Value>,
            #[serde(default)]
            chat: Option<ChatIn>,
            #[serde(default)]
            mic: Option<serde_json::Value>,
            #[serde(default)]
            room_mic: Option<serde_json::Value>,
            #[serde(default)]
            flag: Option<u8>,
        }
        let Ok(m) = serde_json::from_str::<In>(t) else {
            return;
        };
        let now = Instant::now();
        if let Some(n) = m.notice {
            let v = n.as_u64().unwrap_or(0).min(u64::from(u32::MAX)) as u32;
            self.accept(v, now);
        }
        if let Some(c) = m.chat {
            if let Some(body) = c.body() {
                self.chat(body, now);
            }
        }
        let mic = if self.host { m.room_mic } else { m.mic };
        if let Some(w) = mic.as_ref().and_then(|v| v.as_str()) {
            self.mic(w, now);
        }
        if let Some(s) = m.flag {
            with_room(&self.console, |r| r.flag(self.pid, s));
        }
    }

    /// A binary message: a voice frame (true), or not ours (false).
    pub(crate) fn on_binary(&self, b: &[u8]) -> bool {
        if b.first() != Some(&join_voice::KIND_VOICE_UP) {
            return false;
        }
        if let Some(up) = join_voice::parse_up(b) {
            let now = Instant::now();
            with_room(&self.console, |r| {
                if let Err(why) = r.voice_in(self.pid, up, now) {
                    if let Some(p) = r.people.get_mut(&self.pid) {
                        p.refuse("voice", why, now);
                    }
                }
            });
        }
        true
    }

    fn accept(&self, version: u32, now: Instant) {
        with_room(&self.console, |r| {
            if let Err(why) = r.accept(self.pid, version, now) {
                if let Some(p) = r.people.get_mut(&self.pid) {
                    p.refuse("notice", why, now);
                }
            }
        });
    }

    fn chat(&self, body: Body, now: Instant) {
        with_room(&self.console, |r| {
            if let Err(why) = r.chat_in(self.pid, body, now) {
                if let Some(p) = r.people.get_mut(&self.pid) {
                    p.refuse("chat", why, now);
                }
            }
        });
    }

    fn mic(&self, word: &str, now: Instant) {
        with_room(&self.console, |r| {
            if let Err(why) = r.mic(self.pid, word, now) {
                if let Some(p) = r.people.get_mut(&self.pid) {
                    p.refuse("mic", why, now);
                }
            }
        });
    }

    /// Once a second from the socket's loop: the person's route tier (join_route.rs words; None
    /// not known) and player number now.
    pub(crate) fn tick(&self, tier: Option<&str>, player: Option<u8>) {
        let tier = tier.and_then(Tier::from_word);
        with_room(&self.console, |r| {
            r.tick(self.pid, tier, player, Instant::now())
        });
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        let now = Instant::now();
        let mut g = lock(&ROOMS);
        let Some(rooms) = g.as_mut() else { return };
        let key = host_key(&self.console);
        let Some(room) = rooms.get_mut(&key) else {
            return;
        };
        let gone = room.people.get_mut(&self.pid).and_then(|p| {
            p.links.retain(|(g, _)| *g != self.gen);
            if p.here() {
                return None;
            }
            // Gone: the mic goes off (it is off again on their return).
            p.mic_on = false;
            p.ptt_down = false;
            p.last_frame = None;
            p.last_try = None;
            p.slots = [None; join_voice::MAX_SPEAKERS];
            Some(p.sid)
        });
        if let Some(sid) = gone {
            for p in room.people.values_mut() {
                for s in p.slots.iter_mut().filter(|s| **s == Some(sid)) {
                    *s = None;
                }
            }
        }
        if room.people.values().any(Person::here) {
            room.voice_to_all(now);
        } else if room.people.values().all(|p| p.host) {
            // Nobody left and no guest's settings to keep: forget it.
            rooms.remove(&key);
        }
    }
}

#[derive(serde::Deserialize)]
struct ChatIn {
    #[serde(default)]
    quick: Option<u8>,
    #[serde(default)]
    text: Option<String>,
}

impl ChatIn {
    fn body(self) -> Option<Body> {
        match (self.quick, self.text) {
            (Some(n), _) => Some(Body::Quick(n)),
            (None, Some(t)) => Some(Body::Text(t)),
            _ => None,
        }
    }
}

fn housekeep(console: &str, now: Instant) {
    with_room(console, |r| {
        if r.last_hk
            .is_some_and(|t| now.saturating_duration_since(t) < SPEAKING_EVERY * 4 / 5)
        {
            return;
        }
        r.housekeep(now);
    });
}

/// The room on `console` ended (Play Together closed): its chat, mutes, flags and guests' state
/// go; a host page still attached stays with the defaults (and hears nothing until a room opens).
pub(crate) fn room_closed(console: &str) {
    let now = Instant::now();
    let mut g = lock(&ROOMS);
    let Some(rooms) = g.as_mut() else { return };
    let key = host_key(console);
    let Some(room) = rooms.get_mut(&key) else {
        return;
    };
    room.people.retain(|_, p| p.host && p.here());
    room.ring = Ring::new(now);
    room.modes = Modes::defaults(false);
    room.speaking.clear();
    for p in room.people.values_mut() {
        p.mic_on = false;
        p.ptt_down = false;
        p.slots = [None; join_voice::MAX_SPEAKERS];
        p.flagged.clear();
        for (_, l) in &p.links {
            let mut q = lock(&l.q);
            q.pending.clear();
            q.speakers.clear();
        }
    }
    if room.people.is_empty() {
        rooms.remove(&key);
    } else {
        room.comms_to_all();
        room.voice_to_all(now);
    }
}

/// Changes the host's settings for the room on `console` (made if none yet). Voice off turns
/// every mic off; mute changes from "start muted" apply to people who join from now on.
pub(crate) fn set_modes(console: &str, f: impl FnOnce(&mut Modes)) -> Modes {
    let now = Instant::now();
    with_room_or_new(console, |r| {
        let was = r.modes;
        f(&mut r.modes);
        if r.modes.voice == VoiceMode::Off {
            for p in r.people.values_mut() {
                p.mic_on = false;
                p.ptt_down = false;
            }
        }
        if r.modes != was {
            r.comms_to_all();
            r.voice_to_all(now);
        }
        r.modes
    })
}

/// The room's settings now (the defaults when none was made): for the room model's own views.
#[allow(dead_code)]
pub(crate) fn modes(console: &str) -> Modes {
    with_room(console, |r| r.modes).unwrap_or(Modes::defaults(false))
}

/// Whom a host mute is for.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Target {
    Player(u8),
    Sid(u8),
    /// Every guest.
    All,
}

/// The host mutes (or unmutes) voice and/or chat. How many people it changed; Err when nobody
/// matches ("no_player") or the target is the host ("host").
pub(crate) fn mute(
    console: &str,
    target: Target,
    voice: Option<bool>,
    chat: Option<bool>,
) -> Result<usize, &'static str> {
    let now = Instant::now();
    with_room_or_new(console, |r| {
        let ids: Vec<u64> = r
            .people
            .iter()
            .filter(|(_, p)| match target {
                Target::Player(n) => p.player == Some(n) && p.here(),
                Target::Sid(s) => p.sid == s,
                Target::All => !p.host,
            })
            .map(|(id, _)| *id)
            .collect();
        if ids.iter().any(|id| r.people[id].host) {
            return Err("host");
        }
        if ids.is_empty() && !matches!(target, Target::All) {
            return Err("no_player");
        }
        if matches!(target, Target::All) {
            if let Some(v) = voice {
                // New players follow "mute all" too, until "unmute all".
                r.modes.start_muted = v;
            }
        }
        let mut n = 0;
        for id in &ids {
            let p = r.people.get_mut(id).expect("listed above");
            let before = (p.muted_voice, p.muted_chat);
            if let Some(v) = voice {
                p.muted_voice = v;
                if v {
                    p.mic_on = false;
                    p.ptt_down = false;
                }
            }
            if let Some(c) = chat {
                p.muted_chat = c;
            }
            if (p.muted_voice, p.muted_chat) != before {
                n += 1;
                p.say(&Room::muted_json(p));
            }
        }
        if n > 0 {
            r.voice_to_all(now);
        }
        Ok(n)
    })
}

/// The room's talk for the host's `GET /api/join` answer: under `room` (made if missing) `chat`,
/// `voice`, `start_muted`, `notice`, `voice_kbps: {in, out}`, `speaking`, `chat_log` (the last
/// 50 lines), `host_voice`; and on each guest (by `id`) `muted: {voice, chat}` and `voice: {sid,
/// mic, speaking, paused, stepped, kbps, in_kbps, out_kbps, frames_in, frames_out, dropped}`.
/// Call it last: it adds to what is there.
pub(crate) fn extend_state(console: &str, v: &mut serde_json::Value) {
    let now = Instant::now();
    let Some((room, per)) = with_room(console, |r| {
        let sec = r.sec(now);
        let mut in_k = 0;
        let mut out_k = 0;
        let mut per: HashMap<u64, serde_json::Value> = HashMap::new();
        let speaking = r.speaking_now(now);
        let mut host_voice = serde_json::Value::Null;
        for (id, p) in r.people.iter_mut() {
            let (ik, ok) = (p.v_in.kbps(sec), p.v_out.kbps(sec));
            in_k += ik;
            out_k += ok;
            let j = serde_json::json!({
                "sid": p.sid,
                "mic": p.mic_on,
                "speaking": p.speaking(now),
                "paused": p.paused(),
                "stepped": p.adapt.stepped(now),
                "kbps": p.adapt.level().kbps(),
                "in_kbps": ik,
                "out_kbps": ok,
                "frames_in": p.frames_in,
                "frames_out": p.frames_out,
                "dropped": {"refused": p.drops.refused, "rate": p.drops.rate, "cap": p.drops.cap, "paused": p.drops.paused},
                "accepted": p.accepted,
            });
            if p.host {
                host_voice = j;
            } else {
                per.insert(
                    *id,
                    serde_json::json!({
                        "muted": {"voice": p.muted_voice, "chat": p.muted_chat},
                        "voice": j,
                    }),
                );
            }
        }
        let room = serde_json::json!({
            "chat": r.modes.chat.word(),
            "voice": r.modes.voice.word(),
            "start_muted": r.modes.start_muted,
            "notice": NOTICE_VERSION,
            "voice_kbps": {"in": in_k, "out": out_k},
            "speaking": speaking,
            "chat_log": r.ring.last(join_chat::RING),
            "host_voice": host_voice,
        });
        (room, per)
    }) else {
        let d = Modes::defaults(false);
        merge_room(
            v,
            serde_json::json!({
                "chat": d.chat.word(), "voice": d.voice.word(), "start_muted": d.start_muted,
                "notice": NOTICE_VERSION, "voice_kbps": {"in": 0, "out": 0}, "speaking": [],
                "chat_log": [], "host_voice": null,
            }),
        );
        return;
    };
    merge_room(v, room);
    if let Some(guests) = v.get_mut("guests").and_then(|g| g.as_array_mut()) {
        for g in guests {
            let Some(extra) = g["id"].as_u64().and_then(|id| per.get(&id)) else {
                continue;
            };
            if let (Some(g), Some(e)) = (g.as_object_mut(), extra.as_object()) {
                for (k, x) in e {
                    g.insert(k.clone(), x.clone());
                }
            }
        }
    }
}

fn merge_room(v: &mut serde_json::Value, add: serde_json::Value) {
    if !v.is_object() {
        return;
    }
    if !v["room"].is_object() {
        v["room"] = serde_json::json!({});
    }
    if let (Some(r), Some(a)) = (v["room"].as_object_mut(), add.as_object()) {
        for (k, x) in a {
            r.insert(k.clone(), x.clone());
        }
    }
}

/// What a bug report carries: counts only, never a name or a line.
pub(crate) fn diag() -> serde_json::Value {
    let g = lock(&ROOMS);
    let rooms: Vec<_> = g
        .iter()
        .flat_map(|m| m.values())
        .map(|r| {
            serde_json::json!({
                "chat": r.modes.chat.word(),
                "voice": r.modes.voice.word(),
                "start_muted": r.modes.start_muted,
                "people": r.people.len(),
                "here": r.people.values().filter(|p| p.here()).count(),
                "accepted": r.people.values().filter(|p| p.accepted).count(),
                "mics_on": r.people.values().filter(|p| p.mic_on).count(),
                "muted": r.people.values().filter(|p| p.muted_voice || p.muted_chat).count(),
                "chat_lines": r.people.values().map(|p| p.lines).sum::<u64>(),
                "voice_bytes_out": r.out.total,
                "frames_in": r.people.values().map(|p| p.frames_in).sum::<u64>(),
                "dropped": r.people.values().map(|p| p.drops.refused + p.drops.rate + p.drops.cap + p.drops.paused).sum::<u64>(),
            })
        })
        .collect();
    serde_json::json!({ "rooms": rooms })
}

// ── Host API ─────────────────────────────────────────────────────────────────────────────────────

fn party_open(console: &str) -> bool {
    let k = host_key(console);
    crate::join::open_consoles()
        .iter()
        .any(|c| host_key(c) == k)
}

fn refused_response(r: Refusal) -> Response {
    let mut all = refusal_json("host", r);
    let mut v = all["comms_refused"].take();
    v["error"] = r.word().into();
    (StatusCode::CONFLICT, Json(v)).into_response()
}

/// The host's own person on `console` (made on first use).
fn with_host<R>(console: &str, f: impl FnOnce(&mut Room) -> R) -> R {
    with_room_or_new(console, |r| {
        r.people.entry(HOST_PID).or_insert_with(|| {
            Person::new(
                0,
                &crate::mesh_api::device_name(),
                None,
                true,
                Instant::now(),
            )
        });
        f(r)
    })
}

#[derive(serde::Deserialize)]
pub struct CommsBody {
    #[serde(alias = "console", alias = "host")]
    addr: String,
    /// "off" | "quick" | "on" (or "text").
    #[serde(default)]
    chat: Option<ChatMode>,
    /// "off" | "ptt" | "open".
    #[serde(default)]
    voice: Option<VoiceMode>,
    #[serde(default)]
    start_muted: Option<bool>,
    /// A Public room (invite codes are stripped from chat).
    #[serde(default)]
    public: Option<bool>,
    /// The notice version the host accepted on this device.
    #[serde(default)]
    notice: Option<u32>,
}

/// `POST /api/join/comms {addr, chat?, voice?, start_muted?, public?, notice?}`: the host's chat
/// and voice settings for the room on `addr` (`chat` "off" | "quick" | "on", `voice` "off" | "ptt" |
/// "open"; voice off turns every mic off), and the notice the host accepted. Answers with the
/// room's talk as `GET /api/join` has it under `room`.
pub async fn set_comms(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<CommsBody>,
) -> Response {
    if let Some(r) = crate::join::host_only(&peer, &headers) {
        return r;
    }
    if let Some(n) = b.notice {
        let now = Instant::now();
        if let Err(r) = with_host(&b.addr, |room| room.accept(HOST_PID, n, now)) {
            return refused_response(r);
        }
    }
    set_modes(&b.addr, |m| {
        if let Some(c) = b.chat {
            m.chat = c;
        }
        if let Some(v) = b.voice {
            m.voice = v;
        }
        if let Some(s) = b.start_muted {
            m.start_muted = s;
        }
        if let Some(p) = b.public {
            m.public = p;
        }
    });
    let mut v = serde_json::json!({"ok": true});
    extend_state(&b.addr, &mut v);
    Json(v).into_response()
}

#[derive(serde::Deserialize)]
pub struct MuteBody {
    #[serde(alias = "console", alias = "host")]
    addr: String,
    #[serde(default)]
    player: Option<u8>,
    #[serde(default)]
    sid: Option<u8>,
    /// Every guest (and new players, until unmuted).
    #[serde(default)]
    all: bool,
    /// Mute (true) or unmute their voice. `mute` is the same.
    #[serde(default, alias = "mute")]
    voice: Option<bool>,
    /// Mute (true) or unmute their chat.
    #[serde(default)]
    chat: Option<bool>,
}

/// `POST /api/join/mute {addr, player? | sid? | all: true, voice? (or mute?), chat?}`: the host
/// mutes or unmutes a player's voice and/or chat (enforced here: their frames are dropped and their
/// lines refused, and they are told `{"muted": …}`). `all` mutes every guest and those who join
/// after. 404 `no_player`, 409 `host` (the host can't be muted), 400 `nothing`.
pub async fn set_mute(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<MuteBody>,
) -> Response {
    if let Some(r) = crate::join::host_only(&peer, &headers) {
        return r;
    }
    if b.voice.is_none() && b.chat.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "nothing"})),
        )
            .into_response();
    }
    let target = match (b.all, b.player, b.sid) {
        (true, _, _) => Target::All,
        (_, Some(p), _) => Target::Player(p),
        (_, None, Some(s)) => Target::Sid(s),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "no_player"})),
            )
                .into_response()
        }
    };
    match mute(&b.addr, target, b.voice, b.chat) {
        Ok(n) => Json(serde_json::json!({"ok": true, "changed": n})).into_response(),
        Err(e) => {
            let code = if e == "host" {
                StatusCode::CONFLICT
            } else {
                StatusCode::NOT_FOUND
            };
            (code, Json(serde_json::json!({"error": e}))).into_response()
        }
    }
}

#[derive(serde::Deserialize)]
pub struct ChatBody {
    #[serde(alias = "console", alias = "host")]
    addr: String,
    #[serde(default)]
    quick: Option<u8>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    notice: Option<u32>,
}

/// `POST /api/join/chat {addr, quick? (1-16) | text?, notice?}`: the host's own chat line, to
/// everyone in the room. 409 `{error: why, notice?, retry_ms?}` as a guest's refusal says it
/// (`notice`: accept the responsibility notice first; `not_open`: no room open).
pub async fn host_chat(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<ChatBody>,
) -> Response {
    if let Some(r) = crate::join::host_only(&peer, &headers) {
        return r;
    }
    if !party_open(&b.addr) {
        return refused_response(Refusal::NotOpen);
    }
    let body = match (b.quick, b.text) {
        (Some(n), _) => Body::Quick(n),
        (None, Some(t)) => Body::Text(t),
        _ => return refused_response(Refusal::Empty),
    };
    let now = Instant::now();
    let r = with_host(&b.addr, |room| {
        if let Some(n) = b.notice {
            room.accept(HOST_PID, n, now)?;
        }
        room.chat_in(HOST_PID, body, now)
    });
    match r {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(why) => refused_response(why),
    }
}

#[derive(serde::Deserialize)]
pub struct VoiceBody {
    #[serde(alias = "console", alias = "host")]
    addr: String,
    /// "on" | "off" (or true / false).
    mic: serde_json::Value,
    #[serde(default)]
    notice: Option<u32>,
}

/// `POST /api/join/voice {addr, mic: "on" | "off", notice?}`: the host's mic switch for the room.
/// The host's voice frames ride the host page's `/api/ps5/rp/ws` socket (binary `[0x0A]…`, as a
/// guest's), its talk key there too (`{"room_mic": "ptt_down" | "ptt_up"}`).
pub async fn host_voice(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<VoiceBody>,
) -> Response {
    if let Some(r) = crate::join::host_only(&peer, &headers) {
        return r;
    }
    let word = match &b.mic {
        serde_json::Value::Bool(true) => "on",
        serde_json::Value::Bool(false) => "off",
        serde_json::Value::String(s) if s == "on" || s == "off" => s.as_str(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "mic"})),
            )
                .into_response()
        }
    };
    if word == "on" && !party_open(&b.addr) {
        return refused_response(Refusal::NotOpen);
    }
    let now = Instant::now();
    let r = with_host(&b.addr, |room| {
        if let Some(n) = b.notice {
            room.accept(HOST_PID, n, now)?;
        }
        room.mic(HOST_PID, word, now)
    });
    match r {
        Ok(()) => Json(serde_json::json!({"ok": true, "mic": word})).into_response(),
        Err(why) => refused_response(why),
    }
}

/// The host routes (each checks this computer or `PS5UPLOAD_ALLOW_IP`, as the rest of the host API);
/// lib.rs registers the same ones.
#[cfg(test)]
pub(crate) fn router<S: Clone + Send + Sync + 'static>() -> axum::Router<S> {
    use axum::routing::post;
    axum::Router::new()
        .route("/api/join/comms", post(set_comms))
        .route("/api/join/mute", post(set_mute))
        .route("/api/join/chat", post(host_chat))
        .route("/api/join/voice", post(host_voice))
}

#[cfg(test)]
#[path = "join_comms_tests.rs"]
mod tests;
