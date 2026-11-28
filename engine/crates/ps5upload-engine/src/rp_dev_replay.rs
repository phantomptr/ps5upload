//! A recorded Remote Play session served as a live Play Together host, so a guest (PS5Upload Play
//! on a PS5, a browser) can be tested with one console. For developers only: a test engine started
//! with environment variables turns it on; nothing in any UI shows or starts it.
//!
//! * `PS5UPLOAD_DEV_REPLAY_HOST=<file>`: a recording in instant replay's form (`PSRP1`,
//!   rp_replay.rs: what `GET /api/ps5/rp/replay` saves), or one made from any clip with
//!   `scripts/make-psrp1.py`. It plays in a loop as the running Remote Play session of a
//!   console that is not there, through the same fan-out a live session uses
//!   ([`crate::rp_stream_api::DevSession`]).
//! * `PS5UPLOAD_DEV_REPLAY_CONSOLE`: that console's address (default [`DEFAULT_CONSOLE`], a
//!   documentation address nothing answers on). Its helper is a stand-in ([`StandIn`]): guests'
//!   pads are made at once and every button change is logged ("together" category, Info), a
//!   guest's controller rumbles for a second every `PS5UPLOAD_DEV_REPLAY_RUMBLE_S` seconds
//!   (default 10; 0: never) and its light bar changes colour each time, and TV notices are logged.
//! * `PS5UPLOAD_DEV_REPLAY_SEAT=stream`: the host's picture is off on their device, so the first
//!   guest gets the stream's own controller (its rumble then comes the way a game's does).
//!
//! Play Together is opened and answered with the host API as for a real console (`POST
//! /api/join/open {"addr": "192.0.2.10"}`, `/api/join/invite`, `/api/join/decide`, `GET
//! /api/join?addr=` for what the host sees). A keyframe asked for (a guest joining, or one that
//! fell behind) skips ahead to the recording's next keyframe. `POST /api/ps5/rp/stop` ends the
//! session as a host stopping would; it starts again after [`RESTART_AFTER`] (the guests' "the
//! host is starting again" path).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::body::Bytes;
use ps5upload_core::vpad::{
    self, Ack, Bind, Companion, ConsoleUser, Opened, Output, PadState, StateItem,
};

use crate::join::link::Port;
use crate::rp_replay::{Frame, KIND_AUDIO, KIND_AUDIO_HEADER, KIND_VIDEO, KIND_VIDEO_HEADER};
use crate::rp_stream_api::DevSession;

pub(crate) const ENV_FILE: &str = "PS5UPLOAD_DEV_REPLAY_HOST";
const ENV_CONSOLE: &str = "PS5UPLOAD_DEV_REPLAY_CONSOLE";
const ENV_RUMBLE: &str = "PS5UPLOAD_DEV_REPLAY_RUMBLE_S";
const ENV_SEAT: &str = "PS5UPLOAD_DEV_REPLAY_SEAT";
/// TEST-NET-1 (RFC 5737): never a real console.
pub(crate) const DEFAULT_CONSOLE: &str = "192.0.2.10";
/// After a stop, the session starts again this long after.
const RESTART_AFTER: Duration = Duration::from_secs(5);
/// How long each rumble lasts.
const RUMBLE_FOR: Duration = Duration::from_secs(1);
/// The longest the player sleeps at once (it also looks at keyframe asks and input meanwhile).
const TICK: Duration = Duration::from_millis(4);

// ── The recording ────────────────────────────────────────────────────────────────────────────────

/// A recording, ready to play.
#[derive(Debug)]
pub(crate) struct Recording {
    pub video_header: Vec<u8>,
    pub audio_header: Option<Vec<u8>>,
    /// Pictures and sound in order, times from the first frame.
    pub frames: Vec<Frame>,
    pub hevc: bool,
    /// HEVC Main10 (the console sends it only for HDR).
    pub hdr: bool,
}

/// Reads a `PSRP1` recording: its headers first, then frames, with at least one keyframe.
pub(crate) fn parse(b: &[u8]) -> std::result::Result<Recording, String> {
    let rest = b
        .strip_prefix(b"PSRP1")
        .ok_or_else(|| "not a PSRP1 recording".to_string())?;
    let mut i = 0usize;
    let mut video_header = None;
    let mut audio_header = None;
    let mut frames = Vec::new();
    while i < rest.len() {
        let head = rest
            .get(i..i + 13)
            .ok_or_else(|| format!("record at byte {} is cut short", i + 5))?;
        let kind = head[0];
        let t_us = u64::from_be_bytes(head[1..9].try_into().unwrap_or_default());
        let len = u32::from_be_bytes(head[9..13].try_into().unwrap_or_default()) as usize;
        i += 13;
        let key = if kind == KIND_VIDEO {
            let k = *rest
                .get(i)
                .ok_or_else(|| "a picture's keyframe byte is missing".to_string())?;
            i += 1;
            k != 0
        } else {
            false
        };
        let data = rest
            .get(i..i.checked_add(len).ok_or("length overflows")?)
            .ok_or_else(|| format!("a record of {len} bytes runs past the end"))?;
        i += len;
        match kind {
            KIND_VIDEO_HEADER => video_header = Some(data.to_vec()),
            KIND_AUDIO_HEADER => audio_header = Some(data.to_vec()),
            KIND_VIDEO | KIND_AUDIO => {
                if kind == KIND_AUDIO && data.len() < 3 {
                    return Err("a sound frame without its index".into());
                }
                frames.push(Frame {
                    kind,
                    t_us,
                    key,
                    data: Bytes::copy_from_slice(data),
                });
            }
            k => return Err(format!("unknown record kind {k}")),
        }
    }
    let video_header = video_header.ok_or("no video header")?;
    // Play from the first keyframe; times from there.
    let first = frames
        .iter()
        .position(|f| f.kind == KIND_VIDEO && f.key)
        .ok_or("no keyframe")?;
    frames.drain(..first);
    let t0 = frames[0].t_us;
    for f in &mut frames {
        f.t_us = f.t_us.saturating_sub(t0);
    }
    let hevc = nal_units(&video_header).any(|n| matches!((n[0] >> 1) & 0x3f, 32..=34));
    let hdr = hevc
        && nal_units(&video_header)
            .find(|n| (n[0] >> 1) & 0x3f == 33)
            .and_then(|sps| sps.get(3))
            .is_some_and(|b| b & 0x1f == 2);
    Ok(Recording {
        video_header,
        audio_header,
        frames,
        hevc,
        hdr,
    })
}

/// The NAL units of an Annex-B stream (each without its start code, never empty).
fn nal_units(b: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= b.len() {
        if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let ends: Vec<usize> = starts
        .iter()
        .skip(1)
        .map(|s| {
            let mut e = s - 3;
            while e > 0 && b[e - 1] == 0 {
                e -= 1; // a four-byte start code's leading zero
            }
            e
        })
        .chain(std::iter::once(b.len()))
        .collect();
    starts
        .into_iter()
        .zip(ends)
        .filter(|(s, e)| e > s)
        .map(move |(s, e)| &b[s..e])
}

// ── Playing it ───────────────────────────────────────────────────────────────────────────────────

/// Where the player is: the next frame, and the clock it is played to.
struct Cursor {
    next: usize,
    /// When frame time 0 is (frames are due at `base + t_us`).
    base: Instant,
    /// Laps played: each one moves the sound's indexes on, so a guest never sees old ones again.
    lap: u16,
}

/// The index a lap's sound frame goes out with: the recording's, moved on by whole laps.
fn lap_index(index: u16, lap: u16, span: u16) -> u16 {
    index.wrapping_add(lap.wrapping_mul(span))
}

/// How many sound indexes one lap covers (first to last, inclusive).
fn audio_span(frames: &[Frame]) -> u16 {
    let idx = |f: &Frame| u16::from_be_bytes([f.data[0], f.data[1]]);
    let mut audio = frames.iter().filter(|f| f.kind == KIND_AUDIO);
    match (
        audio.next(),
        frames.iter().rev().find(|f| f.kind == KIND_AUDIO),
    ) {
        (Some(a), Some(b)) => idx(b).wrapping_sub(idx(a)).wrapping_add(1),
        _ => 0,
    }
}

/// The next keyframe at or after `from`, or the first (a new lap) when none is left.
fn next_key(frames: &[Frame], from: usize) -> (usize, bool) {
    match frames
        .iter()
        .skip(from)
        .position(|f| f.kind == KIND_VIDEO && f.key)
    {
        Some(p) => (from + p, false),
        None => (0, true),
    }
}

/// A frame interval for the gap between laps: the recording's first picture gap, else 60 fps.
fn lap_gap(frames: &[Frame]) -> Duration {
    let mut v = frames.iter().filter(|f| f.kind == KIND_VIDEO);
    match (v.next(), v.next()) {
        (Some(a), Some(b)) if b.t_us > a.t_us && b.t_us - a.t_us < 200_000 => {
            Duration::from_micros(b.t_us - a.t_us)
        }
        _ => Duration::from_micros(16_667),
    }
}

fn play(rec: &Recording, mut s: DevSession, rumble_every: Option<Duration>, console: &str) {
    s.video_header(&rec.video_header);
    if let Some(a) = &rec.audio_header {
        s.audio(a, true);
    }
    let span = audio_span(&rec.frames);
    let gap = lap_gap(&rec.frames);
    let mut c = Cursor {
        next: 0,
        base: Instant::now(),
        lap: 0,
    };
    let mut buttons = 0u32;
    let mut audio = Vec::with_capacity(2048);
    let started = Instant::now();
    let mut rumbling = false;
    while !s.ended() {
        let now = Instant::now();
        while let Some(st) = s.input() {
            if st.buttons != buttons {
                crate::log_at!(
                    Info,
                    "together",
                    "replay host {console}: the stream's controller: {}",
                    changes(buttons, st.buttons, &STREAM_NAMES)
                );
                buttons = st.buttons;
            }
        }
        if let Some(every) = rumble_every {
            let on = in_rumble(started, now, every);
            if on != rumbling {
                rumbling = on;
                if on {
                    s.rumble(255, 128);
                } else {
                    s.rumble(0, 0);
                }
            }
        }
        if s.wants_keyframe() {
            let at = rec.frames.get(c.next);
            if !at.is_some_and(|f| f.kind == KIND_VIDEO && f.key) {
                let (k, wrapped) = next_key(&rec.frames, c.next);
                if wrapped {
                    c.lap = c.lap.wrapping_add(1);
                }
                c.next = k;
                c.base = now
                    .checked_sub(Duration::from_micros(rec.frames[k].t_us))
                    .unwrap_or(now);
                crate::log_at!(
                    Debug,
                    "together",
                    "replay host {console}: a keyframe was asked for: on to the one at {} ms",
                    rec.frames[k].t_us / 1000
                );
            }
        }
        let f = &rec.frames[c.next];
        let due = c.base + Duration::from_micros(f.t_us);
        if due > now {
            std::thread::sleep((due - now).min(TICK));
            continue;
        }
        if f.kind == KIND_VIDEO {
            s.picture(&f.data, f.key);
        } else {
            let i = lap_index(u16::from_be_bytes([f.data[0], f.data[1]]), c.lap, span);
            audio.clear();
            audio.extend_from_slice(&i.to_be_bytes());
            audio.extend_from_slice(&f.data[2..]);
            s.audio(&audio, false);
        }
        c.next += 1;
        if c.next == rec.frames.len() {
            c.next = 0;
            c.lap = c.lap.wrapping_add(1);
            c.base = Instant::now() + gap;
        }
    }
}

/// Whether the stand-in console rumbles now: the first [`RUMBLE_FOR`] of every `every`.
fn in_rumble(started: Instant, now: Instant, every: Duration) -> bool {
    let t = now.saturating_duration_since(started).as_millis() as u64;
    let every = every.as_millis().max(1) as u64;
    t % every < RUMBLE_FOR.as_millis() as u64 && t >= every
}

// ── The stand-in helper ──────────────────────────────────────────────────────────────────────────

/// What `vpad.*` would answer for the console that is not there.
pub(crate) struct StandIn {
    console: String,
    started: Instant,
    rumble_every: Option<Duration>,
    /// Each open pad: its token, handle and the buttons last logged.
    pads: Mutex<HashMap<u8, (String, i64, u32)>>,
    next_handle: Mutex<i64>,
}

/// The light bar colours the stand-in cycles through, one per rumble.
const COLOURS: [[u8; 3]; 4] = [[0, 64, 255], [255, 32, 0], [0, 200, 64], [255, 0, 200]];

impl StandIn {
    pub(crate) fn new(console: &str, rumble_every: Option<Duration>) -> Self {
        StandIn {
            console: console.to_string(),
            started: Instant::now(),
            rumble_every,
            pads: Mutex::new(HashMap::new()),
            next_handle: Mutex::new(1),
        }
    }

    fn output_at(&self, now: Instant) -> Output {
        let Some(every) = self.rumble_every else {
            return Output {
                motors: [0, 0],
                color: COLOURS[0],
            };
        };
        let n = (now.saturating_duration_since(self.started).as_millis() / every.as_millis().max(1))
            as usize;
        Output {
            motors: if in_rumble(self.started, now, every) {
                [255, 128]
            } else {
                [0, 0]
            },
            color: COLOURS[n % COLOURS.len()],
        }
    }
}

impl Port for StandIn {
    fn open(
        &self,
        _host: &str,
        slot: u8,
        token: &str,
        bind: Option<Bind>,
    ) -> Result<Option<Opened>> {
        let mut pads = self.pads.lock().unwrap_or_else(|e| e.into_inner());
        let resumed = pads.get(&slot).is_some_and(|(t, _, _)| t == token);
        let handle = if resumed {
            pads[&slot].1
        } else {
            let mut h = self.next_handle.lock().unwrap_or_else(|e| e.into_inner());
            *h += 1;
            *h
        };
        pads.insert(slot, (token.to_string(), handle, 0));
        crate::log_at!(
            Info,
            "together",
            "replay host {}: Player {slot}'s pad {}",
            self.console,
            if resumed { "taken back" } else { "made" }
        );
        Ok(Some(Opened {
            ok: true,
            slot,
            handle,
            user: bind.map(|b| b.user),
            resumed: Some(resumed),
            bound: bind.map(|_| true),
            ..Default::default()
        }))
    }

    fn states(&self, _host: &str, items: &[StateItem]) -> Result<Option<Ack>> {
        let mut pads = self.pads.lock().unwrap_or_else(|e| e.into_inner());
        let mut applied = 0;
        for it in items {
            let Some(p) = pads.get_mut(&it.slot) else {
                continue;
            };
            applied += 1;
            if it.state.buttons != p.2 {
                crate::log_at!(
                    Info,
                    "together",
                    "replay host {}: Player {}: {}",
                    self.console,
                    it.slot,
                    changes(p.2, it.state.buttons, &PAD_NAMES)
                );
                p.2 = it.state.buttons;
            }
        }
        Ok(Some(Ack {
            ok: true,
            applied: Some(applied),
            ..Default::default()
        }))
    }

    fn hb(&self, _host: &str, _slot: u8, _token: &str, _live: bool) -> Result<Option<Ack>> {
        Ok(Some(Ack {
            ok: true,
            neutral: Some(false),
            ..Default::default()
        }))
    }

    fn close(&self, _host: &str, slot: u8, _token: &str) -> Result<Option<Ack>> {
        if self
            .pads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&slot)
            .is_some()
        {
            crate::log_at!(
                Info,
                "together",
                "replay host {}: Player {slot}'s pad removed",
                self.console
            );
        }
        Ok(Some(Ack {
            ok: true,
            ..Default::default()
        }))
    }

    fn output(&self, _host: &str, _slot: u8) -> Result<Option<Output>> {
        Ok(Some(self.output_at(Instant::now())))
    }

    fn status(&self, _host: &str) -> Result<Option<serde_json::Value>> {
        let pads = self.pads.lock().unwrap_or_else(|e| e.into_inner());
        let slots: Vec<u8> = pads.keys().copied().collect();
        Ok(Some(serde_json::json!({"slots": slots, "replay": true})))
    }

    fn companion(&self, _host: &str) -> Result<Option<Companion>> {
        Ok(Some(Companion {
            ok: true,
            companion: "running".into(),
            bind: true,
            err: None,
        }))
    }

    fn users(&self, _host: &str) -> Result<Vec<ConsoleUser>> {
        Ok(vec![ConsoleUser {
            id: 0x1000_0001,
            name: "Replay host".into(),
            foreground: true,
        }])
    }

    fn toast(&self, _host: &str, title: &str, body: &str) -> Result<()> {
        crate::log_at!(
            Info,
            "together",
            "replay host {}: TV notice: {title}: {body}",
            self.console
        );
        Ok(())
    }
}

// ── Button names for the log ─────────────────────────────────────────────────────────────────────

/// libScePad's bits (a helper pad's state).
const PAD_NAMES: [(u32, &str); 17] = [
    (vpad::button::CROSS, "cross"),
    (vpad::button::CIRCLE, "circle"),
    (vpad::button::SQUARE, "square"),
    (vpad::button::TRIANGLE, "triangle"),
    (vpad::button::UP, "up"),
    (vpad::button::DOWN, "down"),
    (vpad::button::LEFT, "left"),
    (vpad::button::RIGHT, "right"),
    (vpad::button::L1, "l1"),
    (vpad::button::R1, "r1"),
    (vpad::button::L2, "l2"),
    (vpad::button::R2, "r2"),
    (vpad::button::L3, "l3"),
    (vpad::button::R3, "r3"),
    (vpad::button::OPTIONS, "options"),
    (vpad::button::TOUCHPAD, "touchpad"),
    (vpad::button::PS, "ps"),
];

/// Remote Play's bits (the stream's own controller).
const STREAM_NAMES: [(u32, &str); 16] = {
    use ps5upload_core::rp_stream::button as b;
    [
        (b::CROSS, "cross"),
        (b::CIRCLE, "circle"),
        (b::SQUARE, "square"),
        (b::TRIANGLE, "triangle"),
        (b::DPAD_UP, "up"),
        (b::DPAD_DOWN, "down"),
        (b::DPAD_LEFT, "left"),
        (b::DPAD_RIGHT, "right"),
        (b::L1, "l1"),
        (b::R1, "r1"),
        (b::L3, "l3"),
        (b::R3, "r3"),
        (b::OPTIONS, "options"),
        (b::SHARE, "create"),
        (b::TOUCHPAD, "touchpad"),
        (b::PS, "ps"),
    ]
};

/// "pressed ps options, released cross" (what changed from `was` to `now`).
fn changes(was: u32, now: u32, names: &[(u32, &str)]) -> String {
    let list = |bits: u32| -> Vec<&str> {
        names
            .iter()
            .filter(|(b, _)| bits & b != 0)
            .map(|(_, n)| *n)
            .collect()
    };
    let down = list(now & !was);
    let up = list(was & !now);
    let mut parts = Vec::new();
    if !down.is_empty() {
        parts.push(format!("pressed {}", down.join(" ")));
    }
    if !up.is_empty() {
        parts.push(format!("released {}", up.join(" ")));
    }
    if parts.is_empty() {
        "sticks or triggers only".into()
    } else {
        parts.join(", ")
    }
}

// ── Start ────────────────────────────────────────────────────────────────────────────────────────

/// The seconds in `PS5UPLOAD_DEV_REPLAY_RUMBLE_S` (default 10; 0: none).
fn rumble_every(v: Option<&str>) -> Option<Duration> {
    let s = v.and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(10);
    (s > 0).then(|| Duration::from_secs(s.max(2)))
}

/// Starts the replayed host when `PS5UPLOAD_DEV_REPLAY_HOST` names a recording. Nothing otherwise.
pub(crate) fn start_from_env() {
    let Some(path) = std::env::var_os(ENV_FILE).filter(|p| !p.is_empty()) else {
        return;
    };
    let console = std::env::var(ENV_CONSOLE)
        .ok()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| DEFAULT_CONSOLE.to_string());
    let rumble = rumble_every(std::env::var(ENV_RUMBLE).ok().as_deref());
    let stream_seat = std::env::var(ENV_SEAT).is_ok_and(|v| v.trim() == "stream");
    let rec = match std::fs::read(&path)
        .map_err(|e| e.to_string())
        .and_then(|b| parse(&b))
    {
        Ok(r) => Arc::new(r),
        Err(e) => {
            crate::log_at!(
                Error,
                "together",
                "replay host: {} can't be played: {e}",
                std::path::Path::new(&path).display()
            );
            return;
        }
    };
    crate::log_at!(
        Warn,
        "together",
        "replay host (developers only): playing {} as the live Remote Play session of {console}: {} {}, {} frames, {:.1} s a lap, sound {}, {}",
        std::path::Path::new(&path).display(),
        if rec.hevc { "HEVC" } else { "H.264" },
        if rec.hdr { "Main10 (HDR)" } else { "SDR" },
        rec.frames.len(),
        rec.frames.last().map_or(0, |f| f.t_us) as f64 / 1e6,
        if rec.audio_header.is_some() { "on" } else { "off" },
        if stream_seat { "picture off (a guest may get the stream's controller)" } else { "picture on" }
    );
    crate::join::link::set_dev_port(&console, Arc::new(StandIn::new(&console, rumble)));
    let spawned = std::thread::Builder::new()
        .name("dev-replay-host".into())
        .spawn(move || loop {
            match DevSession::start(&console, rec.hevc, rec.hdr, stream_seat) {
                Some(s) => {
                    let id = s.session();
                    crate::log_at!(
                        Info,
                        "together",
                        "replay host {console}: session {id} playing"
                    );
                    // The stream's controller rumbles as a game's would (a guest on it hears).
                    play(&rec, s, rumble.filter(|_| stream_seat), &console);
                    crate::log_at!(
                        Info,
                        "together",
                        "replay host {console}: session {id} ended; again in {} s",
                        RESTART_AFTER.as_secs()
                    );
                }
                None => {} // another session runs: wait for it to end
            }
            std::thread::sleep(RESTART_AFTER);
        });
    if let Err(e) = spawned {
        crate::log_at!(Error, "together", "replay host: no thread: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An HEVC IDR picture's NAL (type 19).
    const KEY1: &[u8] = &[0, 0, 0, 1, 0x26, 0x01, 0xa1];

    /// A tiny HEVC header: VPS, SPS (profile `profile`), PPS.
    fn hevc_header(profile: u8) -> Vec<u8> {
        let mut v = vec![0, 0, 0, 1, 0x40, 0x01, 0x0c, 0x01];
        v.extend([0, 0, 0, 1, 0x42, 0x01, 0x01, 0x60 | profile, 0, 0]);
        v.extend([0, 0, 0, 1, 0x44, 0x01, 0xc1]);
        v
    }

    fn rec(header: &[u8]) -> Vec<u8> {
        let f = |kind, t, key, d: &'static [u8]| Frame {
            kind,
            t_us: t,
            key,
            data: Bytes::from_static(d),
        };
        let frames = vec![
            f(KIND_AUDIO, 900, false, &[0, 7, 0xaa]),
            f(KIND_VIDEO, 1_000, false, &[0, 0, 0, 1, 0x02, 0x01, 0xd0]),
            f(KIND_VIDEO, 2_000, true, KEY1),
            f(KIND_AUDIO, 2_500, false, &[0, 8, 0xbb]),
            f(KIND_VIDEO, 18_667, false, &[0, 0, 0, 1, 0x02, 0x01, 0xd1]),
            f(KIND_VIDEO, 35_333, true, &[0, 0, 0, 1, 0x26, 0x01, 0xa2]),
            f(KIND_AUDIO, 36_000, false, &[0, 9, 0xcc]),
        ];
        crate::rp_replay::encode(header, Some(&[2, 16, 0, 0, 0xbb, 0x80]), &frames)
    }

    #[test]
    fn a_saved_replay_reads_back_from_its_first_keyframe() {
        let r = parse(&rec(&hevc_header(1))).unwrap();
        assert!(r.hevc);
        assert!(!r.hdr);
        assert_eq!(
            r.audio_header.as_deref(),
            Some(&[2, 16, 0, 0, 0xbb, 0x80][..])
        );
        let got: Vec<(u8, u64, bool)> = r.frames.iter().map(|f| (f.kind, f.t_us, f.key)).collect();
        assert_eq!(
            got,
            vec![
                (KIND_VIDEO, 0, true),
                (KIND_AUDIO, 500, false),
                (KIND_VIDEO, 16_667, false),
                (KIND_VIDEO, 33_333, true),
                (KIND_AUDIO, 34_000, false),
            ]
        );
        assert_eq!(audio_span(&r.frames), 2);
        assert_eq!(lap_gap(&r.frames), Duration::from_micros(16_667));
    }

    #[test]
    fn main10_is_hdr_and_h264_is_not_hevc() {
        let r = parse(&rec(&hevc_header(2))).unwrap();
        assert!(r.hevc && r.hdr);
        let avc = [0, 0, 0, 1, 0x67, 0x64, 0, 0x28, 0, 0, 0, 1, 0x68, 0xee];
        let r = parse(&rec(&avc)).unwrap();
        assert!(!r.hevc && !r.hdr);
    }

    #[test]
    fn broken_recordings_are_refused() {
        assert!(parse(b"PSRP2").is_err());
        let mut b = rec(&hevc_header(1));
        b.truncate(b.len() - 1);
        assert!(parse(&b).is_err(), "cut short");
        let mut b = crate::rp_replay::encode(&hevc_header(1), None, &[]);
        assert!(parse(&b).is_err(), "no keyframe");
        b.extend([9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(parse(&b).unwrap_err().contains("unknown"));
    }

    #[test]
    fn keyframe_asks_skip_ahead_and_wrap_to_a_new_lap() {
        let r = parse(&rec(&hevc_header(1))).unwrap();
        assert_eq!(next_key(&r.frames, 1), (3, false));
        assert_eq!(next_key(&r.frames, 3), (3, false));
        assert_eq!(next_key(&r.frames, 4), (0, true));
        // A lap's sound indexes follow on from the last one's.
        assert_eq!(lap_index(8, 1, 2), 10);
        assert_eq!(lap_index(u16::MAX, 1, 2), 1);
    }

    #[test]
    fn the_stand_in_rumbles_for_a_second_of_each_period_and_never_before_the_first() {
        let s = StandIn::new("192.0.2.10", Some(Duration::from_secs(10)));
        let at = |ms: u64| s.output_at(s.started + Duration::from_millis(ms));
        assert_eq!(
            at(500).motors,
            [0, 0],
            "not at once: the guest is still joining"
        );
        assert_eq!(at(10_200).motors, [255, 128]);
        assert_eq!(at(11_200).motors, [0, 0]);
        assert_ne!(at(10_200).color, at(20_200).color);
        let quiet = StandIn::new("192.0.2.10", None);
        assert_eq!(quiet.output_at(Instant::now()).motors, [0, 0]);
        assert_eq!(rumble_every(Some("0")), None);
        assert_eq!(rumble_every(None), Some(Duration::from_secs(10)));
    }

    /// The recording reaches a Play Together player exactly as a live session's pictures and
    /// sound do: the codec header, pictures from a keyframe, sound with indexes that carry on
    /// across laps; and a stop ends the player.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_player_gets_the_recording_as_a_live_session_gives_it() {
        use axum::extract::ws::Message;
        let _turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
        let host = "192.0.2.77";
        let r = parse(&rec(&hevc_header(2))).unwrap();
        let s = DevSession::start(host, r.hevc, r.hdr, false).unwrap();
        assert!(
            DevSession::start(host, true, false, false).is_none(),
            "one session at a time"
        );
        assert_eq!(
            crate::rp_stream_api::live_video(host).map(|v| v.0),
            Some("h265_hdr")
        );
        let skipped = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut feed =
            crate::rp_stream_api::add_player(1 << 62 | 77, host, 6, skipped, false, false).unwrap();
        let player = std::thread::spawn(move || play(&r, s, None, "192.0.2.77"));
        let mut kinds = Vec::new();
        let mut sound = Vec::new();
        let until = tokio::time::Instant::now() + Duration::from_secs(2);
        while sound.len() < 4 {
            tokio::select! {
                Some(m) = feed.video.recv() => if let Message::Binary(b) = m {
                    kinds.push((b[0], b[1..].to_vec()));
                },
                Some(m) = feed.other.recv() => if let Message::Binary(b) = m {
                    if b[0] == 4 {
                        sound.push(u16::from_be_bytes([b[1], b[2]]));
                    }
                },
                _ = tokio::time::sleep_until(until) => panic!("only {kinds:?} and {sound:?} came"),
            }
        }
        assert_eq!(kinds[0].0, 0, "the codec header first");
        assert_eq!((kinds[1].0, kinds[1].1.as_slice()), (1, KEY1));
        // One lap holds indexes 8 and 9; the next lap's go on from there.
        assert_eq!(&sound[..4], &[8, 9, 10, 11]);
        crate::rp_stream_api::stop_running();
        player.join().unwrap();
        assert!(
            crate::rp_stream_api::live_video(host).is_none(),
            "the session is gone"
        );
    }

    #[test]
    fn the_stand_in_takes_pads_and_logs_what_changed() {
        let s = StandIn::new("192.0.2.10", None);
        let o = s.open("h", 2, "tok", None).unwrap().unwrap();
        assert!(o.ok && o.resumed == Some(false));
        let again = s.open("h", 2, "tok", None).unwrap().unwrap();
        assert_eq!(again.resumed, Some(true));
        assert_eq!(again.handle, o.handle);
        let item = |buttons| StateItem {
            slot: 2,
            token: "tok".into(),
            seq: 1,
            state: PadState {
                buttons,
                ..PadState::NEUTRAL
            },
        };
        let a = s.states("h", &[item(vpad::button::PS)]).unwrap().unwrap();
        assert_eq!(a.applied, Some(1));
        assert_eq!(
            changes(
                vpad::button::CROSS,
                vpad::button::PS | vpad::button::OPTIONS,
                &PAD_NAMES
            ),
            "pressed options ps, released cross"
        );
        assert!(
            s.status("h").unwrap().is_some(),
            "Play Together opens on it"
        );
        s.close("h", 2, "tok").unwrap();
        let none = s.states("h", &[item(0)]).unwrap().unwrap();
        assert_eq!(none.applied, Some(0));
    }
}
