//! The live feed (#635): the running Remote Play stream as MPEG-TS for OBS (Media Source, input
//! format `mpegts`) or any ffmpeg-based player, without re-encoding (rp_live_ts.rs).
//!
//! A child module of rp_stream_api.rs. Each reader is one more fan-out of the session's frames,
//! beside the page, Instant Replay and watchers, and never disturbs them:
//!
//! * `GET /api/ps5/rp/live.ts[?addr=<console>]`: MPEG-TS for as long as the reader stays. It
//!   starts at a keyframe (one is asked for at once). No session running (or a different
//!   console than `addr`): 503 `{"error": "no_session"}` straight away, never a wait, so a
//!   player's own retry (OBS: Reconnect Delay) picks the stream up once it runs. The feed ends
//!   when the session does.
//! * `GET /api/ps5/rp/live/status`: readers, codec, resolution/fps/bitrate, seconds since the
//!   last keyframe, frames held back after a loss.
//!
//! Both answer only on this computer (loopback peer, and a localhost Host header against DNS
//! rebinding), like the automation API.
//!
//! The stream thread only hands each reader a reference to the frame (no copy, no muxing) on a
//! bounded queue (READER_QUEUE). A reader that falls behind loses frames up to the next
//! keyframe rather than growing memory or holding the session up; a keyframe is then asked
//! for. While a reader is attached, a keyframe is asked for every KEYFRAME_EVERY (2 s), so OBS
//! can start and recover quickly; with Instant Replay on (5 s) the shorter interval wins. After
//! a lost frame (the assembler gave up on it, or the frame index skipped), the frames after it
//! would show smeared damage until the next keyframe, so a reader gets none of them until that
//! keyframe, which is asked for at once (the stream thread still asks at most every 500 ms).
//! Audio keeps flowing meanwhile.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Query};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::rp_stream::{Codec, StreamConfig};
use tokio::sync::mpsc;

use crate::rp_live_ts::TsMuxer;

/// While a reader is attached, a keyframe at least this often.
pub(crate) const KEYFRAME_EVERY: Duration = Duration::from_secs(2);
/// How long the console takes to answer a keyframe request (measured on a PS5 Pro, FW 13.60:
/// about a second at 1080p60). The periodic request goes out this much before KEYFRAME_EVERY so
/// the keyframes land about KEYFRAME_EVERY apart (asked at 2 s, they came every ~3 s).
const KEYFRAME_LEAD: Duration = Duration::from_millis(1000);
/// No second periodic request while one is in flight (each extra one is another keyframe: a
/// burst of bitrate for nothing).
const KEYFRAME_ASK_GAP: Duration = Duration::from_millis(1500);
/// Frames (video and audio) waiting for one reader: about 2 s of a 60 fps stream with sound.
const READER_QUEUE: usize = 320;
/// Readers at once: more is a mistake (a player retrying in a loop), not a use.
const MAX_READERS: usize = 8;

/// What the stream thread hands a reader.
pub(crate) enum FeedEvent {
    Video {
        au: Bytes,
        index: u16,
        at: Instant,
        key: bool,
        /// The codec parameter sets, on a keyframe.
        header: Option<Bytes>,
    },
    Audio {
        opus: Bytes,
        index: u16,
        at: Instant,
    },
}

/// One reader's counts, for the log and the status.
#[derive(Default)]
pub(crate) struct ReaderStats {
    pub bytes: AtomicU64,
    pub keyframes: AtomicU64,
    pub frames: AtomicU64,
    pub held_after_loss: AtomicU64,
    pub dropped_slow: AtomicU64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Wait {
    /// Joined: nothing until the first keyframe.
    Join,
    /// A frame was lost: the ones after it are damaged until the next keyframe.
    Loss,
    /// Its queue was full: skipped ahead to the next keyframe.
    Slow,
}

struct Reader {
    id: u64,
    tx: mpsc::Sender<FeedEvent>,
    wait: Option<Wait>,
    stats: Arc<ReaderStats>,
    since: Instant,
    peer: String,
    /// The engine's own reader (Go live, rp_golive.rs), not a player on an HTTP connection.
    internal: bool,
}

/// The session's live-feed readers and what they need from the stream.
pub(crate) struct LiveFeed {
    hevc: bool,
    readers: Mutex<Vec<Reader>>,
    next_id: AtomicU64,
    /// The codec parameter sets now (they change with the stream's resolution profile).
    header: Mutex<Option<Bytes>>,
    /// Audio channels from the console's audio header (stereo until it says).
    channels: Mutex<u8>,
    config: Mutex<Option<StreamConfig>>,
    /// The last frame index seen, to tell a skipped frame.
    last_index: Mutex<Option<u16>>,
    /// The stream thread reported lost frames since the last frame.
    lost: std::sync::atomic::AtomicBool,
    last_key: Mutex<Option<Instant>>,
    /// The last periodic keyframe request (see KEYFRAME_ASK_GAP).
    last_ask: Mutex<Option<Instant>>,
    /// Asked for a keyframe (a reader joined, fell behind, or saw a loss).
    want_key: std::sync::atomic::AtomicBool,
    /// The session ended: no new readers (one added after `close` would wait forever).
    closed: std::sync::atomic::AtomicBool,
    held_after_loss: AtomicU64,
    losses: AtomicU64,
}

impl LiveFeed {
    pub(crate) fn new(hevc: bool) -> Self {
        LiveFeed {
            hevc,
            readers: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            header: Mutex::new(None),
            channels: Mutex::new(2),
            config: Mutex::new(None),
            last_index: Mutex::new(None),
            lost: Default::default(),
            last_key: Mutex::new(None),
            last_ask: Mutex::new(None),
            want_key: Default::default(),
            closed: Default::default(),
            held_after_loss: AtomicU64::new(0),
            losses: AtomicU64::new(0),
        }
    }

    pub(crate) fn set_config(&self, cfg: StreamConfig) {
        if let Ok(mut g) = self.config.lock() {
            *g = Some(cfg);
        }
    }

    fn fps(&self) -> u32 {
        self.config
            .lock()
            .ok()
            .and_then(|g| g.map(|c| c.fps))
            .unwrap_or(60)
    }

    pub(crate) fn readers(&self) -> usize {
        self.readers.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// The codec parameter sets arrived (or changed).
    pub(crate) fn video_header(&self, h: &[u8]) {
        if let Ok(mut g) = self.header.lock() {
            *g = Some(Bytes::copy_from_slice(h));
        }
    }

    /// The console's 14-byte audio header.
    pub(crate) fn audio_header(&self, h: &[u8]) {
        if let Some(a) = ps5upload_core::rp_stream::AudioHeader::parse(h) {
            if let Ok(mut g) = self.channels.lock() {
                *g = a.channels;
            }
        }
    }

    /// The stream thread gave up on frames: what follows is damaged until a keyframe.
    pub(crate) fn lost(&self) {
        self.lost.store(true, Ordering::Relaxed);
    }

    /// A new reader: its queue, its counts and its id. A keyframe is asked for at once.
    pub(crate) fn add(
        &self,
        peer: String,
        internal: bool,
    ) -> Option<(u64, mpsc::Receiver<FeedEvent>, Arc<ReaderStats>)> {
        let mut g = self.readers.lock().ok()?;
        if self.closed.load(Ordering::Relaxed) {
            return None;
        }
        // The cap is for outside readers (OBS, scripts): the engine's own (Go live, one at a
        // time) is never shut out by them, which would read as "Remote Play stopped".
        if !internal && g.iter().filter(|r| !r.internal).count() >= MAX_READERS {
            return None;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(READER_QUEUE);
        let stats = Arc::new(ReaderStats::default());
        g.push(Reader {
            id,
            tx,
            wait: Some(Wait::Join),
            stats: stats.clone(),
            since: Instant::now(),
            peer,
            internal,
        });
        self.want_key.store(true, Ordering::Relaxed);
        Some((id, rx, stats))
    }

    /// A reader left (or the session ended): its summary goes to the engine log.
    pub(crate) fn remove(&self, id: u64) {
        let r = self.readers.lock().ok().and_then(|mut g| {
            let i = g.iter().position(|r| r.id == id)?;
            Some(g.remove(i))
        });
        if let Some(r) = r {
            log_reader("disconnected", &r);
        }
    }

    /// A reader that took one frame and left (a frame request): nothing for the log.
    pub(crate) fn remove_quiet(&self, id: u64) {
        if let Ok(mut g) = self.readers.lock() {
            g.retain(|r| r.id != id);
        }
    }

    /// The session ended: every reader's queue closes, and its stream with it.
    pub(crate) fn close(&self) {
        let all = self
            .readers
            .lock()
            .map(|mut g| {
                // Under the readers' lock, so an add can't slip in between.
                self.closed.store(true, Ordering::Relaxed);
                std::mem::take(&mut *g)
            })
            .unwrap_or_default();
        for r in &all {
            log_reader("session ended", r);
        }
    }

    /// One whole frame from the stream thread.
    pub(crate) fn video(&self, au: &[u8], index: u16, key: bool, at: Instant) {
        // A skipped index is a lost frame too (the assembler also reports those).
        let gap = self
            .last_index
            .lock()
            .ok()
            .and_then(|mut g| g.replace(index))
            .is_some_and(|l| index != l.wrapping_add(1));
        let loss = self.lost.swap(false, Ordering::Relaxed) || gap;
        if loss {
            self.losses.fetch_add(1, Ordering::Relaxed);
        }
        if key {
            if let Ok(mut g) = self.last_key.lock() {
                *g = Some(at);
            }
        }
        let Ok(mut g) = self.readers.lock() else {
            return;
        };
        if g.is_empty() {
            return;
        }
        let au = Bytes::copy_from_slice(au);
        let header = if key {
            self.header.lock().ok().and_then(|h| h.clone())
        } else {
            None
        };
        // A keyframe is asked for when a reader starts waiting (a loss, or it fell behind), not
        // on every frame while it waits: a reader that stays stuck (ffmpeg blocked on a dead
        // connection, a half-open OBS socket) would otherwise have the console send a keyframe
        // every 500 ms for everyone. The periodic keyframe (KEYFRAME_EVERY) still comes.
        let mut want = false;
        for r in g.iter_mut() {
            if loss && !key && r.wait.is_none() {
                r.wait = Some(Wait::Loss);
                want = true;
            }
            if let Some(w) = r.wait {
                if !key {
                    if w == Wait::Loss {
                        r.stats.held_after_loss.fetch_add(1, Ordering::Relaxed);
                        self.held_after_loss.fetch_add(1, Ordering::Relaxed);
                    }
                    continue;
                }
                r.wait = None;
            }
            let ev = FeedEvent::Video {
                au: au.clone(),
                index,
                at,
                key,
                header: header.clone(),
            };
            if let Err(mpsc::error::TrySendError::Full(_)) = r.tx.try_send(ev) {
                r.stats.dropped_slow.fetch_add(1, Ordering::Relaxed);
                r.wait = Some(Wait::Slow);
                // Only when it falls behind, not each keyframe it still can't take.
                want |= !key;
            }
        }
        if want {
            self.want_key.store(true, Ordering::Relaxed);
        }
    }

    /// One audio frame from the stream thread: the console's u16 BE frame index, then Opus.
    /// Only readers already playing get it (their stream starts at a video keyframe).
    pub(crate) fn audio(&self, frame: &[u8], at: Instant) {
        if frame.len() < 3 {
            return;
        }
        let Ok(g) = self.readers.lock() else {
            return;
        };
        if g.iter().all(|r| r.wait == Some(Wait::Join)) {
            return;
        }
        let index = u16::from_be_bytes([frame[0], frame[1]]);
        let opus = Bytes::copy_from_slice(&frame[2..]);
        for r in g.iter().filter(|r| r.wait != Some(Wait::Join)) {
            // A full queue loses this 10 ms of sound; the video side handles catching up.
            let _ = r.tx.try_send(FeedEvent::Audio {
                opus: opus.clone(),
                index,
                at,
            });
        }
    }

    /// The stream should ask the console for a keyframe: a reader needs one, or it has been
    /// KEYFRAME_EVERY since the last while someone reads.
    pub(crate) fn wants_keyframe(&self, now: Instant) -> bool {
        let asked = self.want_key.swap(false, Ordering::Relaxed);
        if self.readers() == 0 {
            return false;
        }
        if asked {
            return true;
        }
        let since_key = self.last_key.lock().ok().and_then(|g| *g);
        let due = since_key
            .is_none_or(|t| now.saturating_duration_since(t) + KEYFRAME_LEAD >= KEYFRAME_EVERY);
        if !due {
            return false;
        }
        let Ok(mut ask) = self.last_ask.lock() else {
            return true;
        };
        if ask.is_some_and(|t| now.saturating_duration_since(t) < KEYFRAME_ASK_GAP) {
            return false;
        }
        *ask = Some(now);
        true
    }

    pub(crate) fn status(&self) -> serde_json::Value {
        let cfg = self.config.lock().ok().and_then(|g| *g);
        let readers: Vec<serde_json::Value> = self
            .readers
            .lock()
            .map(|g| {
                g.iter()
                    .map(|r| {
                        serde_json::json!({
                            "id": r.id,
                            "go_live": r.internal,
                            "connected_s": r.since.elapsed().as_secs(),
                            "playing": r.wait != Some(Wait::Join),
                            "bytes": r.stats.bytes.load(Ordering::Relaxed),
                            "frames": r.stats.frames.load(Ordering::Relaxed),
                            "keyframes": r.stats.keyframes.load(Ordering::Relaxed),
                            "held_after_loss": r.stats.held_after_loss.load(Ordering::Relaxed),
                            "dropped_slow": r.stats.dropped_slow.load(Ordering::Relaxed),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let since_key = self
            .last_key
            .lock()
            .ok()
            .and_then(|g| *g)
            .map(|t| (t.elapsed().as_secs_f64() * 10.0).round() / 10.0);
        let go_live = readers.iter().filter(|r| r["go_live"] == true).count();
        serde_json::json!({
            "running": true,
            // Players reading over HTTP (OBS); Go live's own reader is `go_live`.
            "consumers": readers.len() - go_live,
            "go_live": go_live,
            "readers": readers,
            "codec": cfg.map(|c| codec_name(c.codec)).unwrap_or(if self.hevc { "h265" } else { "h264" }),
            "width": cfg.map(|c| c.width),
            "height": cfg.map(|c| c.height),
            "fps": cfg.map(|c| c.fps),
            "bitrate_kbps": cfg.map(|c| c.bitrate_kbps),
            "since_keyframe_s": since_key,
            "frames_held_after_loss": self.held_after_loss.load(Ordering::Relaxed),
            "losses": self.losses.load(Ordering::Relaxed),
            "keyframe_every_s": KEYFRAME_EVERY.as_secs(),
        })
    }
}

fn codec_name(c: Codec) -> &'static str {
    match c {
        Codec::H264 => "h264",
        Codec::H265 => "h265",
        Codec::H265Hdr => "h265_hdr",
    }
}

fn log_reader(what: &str, r: &Reader) {
    let s = &r.stats;
    crate::log_info!(
        "rp_live: reader {} ({}) {what} after {:.0} s: {} bytes, {} frames, {} keyframes, {} held after loss, {} dropped (slow reader)",
        r.id,
        r.peer,
        r.since.elapsed().as_secs_f64(),
        s.bytes.load(Ordering::Relaxed),
        s.frames.load(Ordering::Relaxed),
        s.keyframes.load(Ordering::Relaxed),
        s.held_after_loss.load(Ordering::Relaxed),
        s.dropped_slow.load(Ordering::Relaxed),
    );
}

/// One reader's side: its frames into TS bytes.
pub(crate) struct ReaderMux {
    mux: TsMuxer,
    origin: Instant,
    stats: Arc<ReaderStats>,
    id: u64,
    first_key_logged: bool,
}

impl ReaderMux {
    pub(crate) fn new(feed: &LiveFeed, id: u64, stats: Arc<ReaderStats>) -> Self {
        let channels = feed.channels.lock().map(|g| *g).unwrap_or(2);
        ReaderMux {
            mux: TsMuxer::new(feed.hevc, feed.fps(), channels),
            origin: Instant::now(),
            stats,
            id,
            first_key_logged: false,
        }
    }

    fn us(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.origin).as_micros() as u64
    }

    /// The TS bytes for one event (often none: audio waiting to be put in order).
    pub(crate) fn push(&mut self, ev: FeedEvent) -> Vec<u8> {
        let out = match ev {
            FeedEvent::Video {
                au,
                index,
                at,
                key,
                header,
            } => {
                let t = self.us(at);
                let out = self.mux.video(&au, index, t, key, header.as_deref());
                if !out.is_empty() {
                    self.stats.frames.fetch_add(1, Ordering::Relaxed);
                    if key {
                        self.stats.keyframes.fetch_add(1, Ordering::Relaxed);
                        if !self.first_key_logged {
                            self.first_key_logged = true;
                            crate::log_info!(
                                "rp_live: reader {} playing: first keyframe after {} ms{}",
                                self.id,
                                self.origin.elapsed().as_millis(),
                                if header.is_some() {
                                    ""
                                } else {
                                    " (no codec header yet!)"
                                }
                            );
                        }
                    }
                }
                out
            }
            FeedEvent::Audio { opus, index, at } => {
                let t = self.us(at);
                self.mux.audio(&opus, index, t)
            }
        };
        self.stats
            .bytes
            .fetch_add(out.len() as u64, Ordering::Relaxed);
        out
    }
}

/// Takes the reader off the feed when its stream is dropped (the player hung up).
struct Leave {
    feed: Arc<LiveFeed>,
    id: u64,
}

impl Drop for Leave {
    fn drop(&mut self) {
        self.feed.remove(self.id);
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct LiveQuery {
    /// The console whose session to read (when given, it must be the one running).
    #[serde(default)]
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
}

fn fail(code: StatusCode, error: &str, message: &str) -> Response {
    (
        code,
        Json(serde_json::json!({ "error": error, "message": message })),
    )
        .into_response()
}

/// The running session, if it is the one `addr` names (any, without `addr`).
fn session_for(addr: Option<&str>) -> Option<Arc<super::Live>> {
    let l = super::live()?;
    match addr.map(str::trim).filter(|a| !a.is_empty()) {
        Some(a) if crate::vinput_api::host_key(a) != crate::vinput_api::host_key(&l.host) => None,
        _ => Some(l),
    }
}

/// The running session as a stream for a site sees it (Go live checks it against each site's
/// limits, rp_golive.rs).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub(crate) struct SessionInfo {
    pub host: String,
    /// "h264", "h265" or "h265_hdr".
    pub codec: &'static str,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}

/// The running session's video, if one runs.
pub(crate) fn session_info() -> Option<SessionInfo> {
    let l = super::live()?;
    let cfg = l.feed.config.lock().ok().and_then(|g| *g);
    Some(SessionInfo {
        host: l.host.clone(),
        codec: cfg.map(|c| codec_name(c.codec)).unwrap_or(if l.feed.hevc {
            "h265"
        } else {
            "h264"
        }),
        width: cfg.map_or(1920, |c| c.width),
        height: cfg.map_or(1080, |c| c.height),
        fps: cfg.map_or(60, |c| c.fps),
        bitrate_kbps: cfg.map_or(0, |c| c.bitrate_kbps),
    })
}

/// One of the engine's own readers of the feed (Go live): MPEG-TS bytes, the same as a player
/// on live.ts gets. It leaves the feed when dropped.
pub(crate) struct FeedReader {
    rx: mpsc::Receiver<FeedEvent>,
    mux: ReaderMux,
    _leave: Leave,
    pub stats: Arc<ReaderStats>,
    pub session: SessionInfo,
}

impl FeedReader {
    /// The next TS bytes; None once the session ended.
    pub(crate) async fn next(&mut self) -> Option<Vec<u8>> {
        loop {
            let ev = self.rx.recv().await?;
            let out = self.mux.push(ev);
            if !out.is_empty() {
                return Some(out);
            }
        }
    }
}

/// A reader on the running session's feed for the engine itself. None when no session runs
/// (or the feed has all the readers it takes).
pub(crate) fn open_reader(who: &str) -> Option<FeedReader> {
    let session = session_info()?;
    let live = super::live()?;
    let feed = live.feed.clone();
    let (id, rx, stats) = feed.add(who.to_string(), true)?;
    crate::log_info!("rp_live: reader {id} ({who}) connected to {}", live.host);
    let mux = ReaderMux::new(&feed, id, stats.clone());
    Some(FeedReader {
        rx,
        mux,
        _leave: Leave { feed, id },
        stats,
        session,
    })
}

/// `GET /api/ps5/rp/live.ts`.
pub async fn live_ts(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<LiveQuery>,
) -> Response {
    if !crate::vinput_api::host_is_local(&headers) {
        return crate::vinput_api::host_not_local();
    }
    if !crate::vinput_api::local_allowed(peer.ip()) {
        return fail(
            StatusCode::FORBIDDEN,
            "local_only",
            "The live feed is only for this computer.",
        );
    }
    let Some(live) = session_for(q.addr.as_deref()) else {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_session",
            "No Remote Play session is running. Start Remote Play in PS5Upload, then open this again.",
        );
    };
    let feed = live.feed.clone();
    let Some((id, rx, stats)) = feed.add(peer.to_string(), false) else {
        return fail(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_readers",
            "Too many players are reading the live feed.",
        );
    };
    crate::log_info!(
        "rp_live: reader {id} ({peer}) connected to {} ({} reading)",
        live.host,
        feed.readers()
    );
    let mux = ReaderMux::new(&feed, id, stats);
    let leave = Leave { feed, id };
    let body =
        futures_util::stream::unfold((rx, mux, leave), |(mut rx, mut mux, leave)| async move {
            loop {
                let ev = rx.recv().await?;
                let out = mux.push(ev);
                if !out.is_empty() {
                    return Some((Ok::<_, std::io::Error>(Bytes::from(out)), (rx, mux, leave)));
                }
            }
        });
    (
        [
            (header::CONTENT_TYPE, "video/mp2t"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from_stream(body),
    )
        .into_response()
}

/// `GET /api/ps5/rp/live/status`.
pub async fn status(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<LiveQuery>,
) -> Response {
    if !crate::vinput_api::host_is_local(&headers) {
        return crate::vinput_api::host_not_local();
    }
    if !crate::vinput_api::local_allowed(peer.ip()) {
        return fail(
            StatusCode::FORBIDDEN,
            "local_only",
            "The live feed is only for this computer.",
        );
    }
    let v = match session_for(q.addr.as_deref()) {
        Some(l) => {
            let mut v = l.feed.status();
            v["host"] = serde_json::json!(l.host);
            v
        }
        None => serde_json::json!({"running": false, "consumers": 0}),
    };
    Json(v).into_response()
}

/// A pretend Remote Play session for other modules' tests (Go live): frames go into the feed
/// as the stream thread would put them.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct TestSession {
    live: Arc<super::Live>,
}

/// Some session is installed as the running one.
#[cfg(test)]
pub(crate) fn session_running() -> bool {
    super::LIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some()
}

#[cfg(test)]
impl TestSession {
    /// Installs the session as the running one.
    pub(crate) fn start(host: &str, cfg: StreamConfig) -> Self {
        let live = Arc::new(super::Live::new(
            host.into(),
            cfg.codec.is_hevc(),
            Duration::from_secs(60),
        ));
        live.feed.set_config(cfg);
        *super::LIVE.lock().unwrap() = Some(live.clone());
        TestSession { live }
    }
    pub(crate) fn feed(&self) -> &LiveFeed {
        &self.live.feed
    }
    /// The session ends: readers' streams end and nothing runs.
    pub(crate) fn end(&self) {
        self.live.feed.close();
        let mut g = super::LIVE.lock().unwrap();
        if g.as_ref().is_some_and(|x| Arc::ptr_eq(x, &self.live)) {
            *g = None;
        }
    }
}

/// A test that panics still ends its session: the next test would find it running.
#[cfg(test)]
impl Drop for TestSession {
    fn drop(&mut self) {
        self.end();
    }
}

#[cfg(test)]
#[path = "rp_live_tests.rs"]
pub(crate) mod tests;
