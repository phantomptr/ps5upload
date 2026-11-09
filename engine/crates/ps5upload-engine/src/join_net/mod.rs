//! Play Together over the internet: a guest's browser reaches this engine peer to peer over WebRTC,
//! with a signalling hub in between only to swap the offer, the answer and ICE candidates.
//!
//! **The invite.** `POST /api/join/invite {internet: true}` opens a room on the hub ([`hub`]) and
//! answers a link `https://ps5upload.phantomptr.com/join/#room=<id>&k=<invite key>&fp=<DTLS
//! fingerprint>&hub=<hub host>`. Everything after `#` stays in the guest's browser. `k` is the LAN
//! invite's key with the same meaning (the host still allows each guest). `fp` is the SHA-256 of
//! this engine's DTLS certificate, upper-case hex with colons ([`fingerprint`]): one certificate per
//! engine run, so the page refuses a peer whose answer carries another one (a hub that tried to
//! stand in the middle).
//!
//! **Signalling** (hub protocol v1, `{"t":"signal","to"|"from":…,"data":…}`), the `data` both ways:
//! * guest → engine `{"type": "offer", "sdp": "…"}`, then any number of `{"type": "candidate",
//!   "candidate": "candidate:…", "sdpMid": "0"}` (`RTCIceCandidate.toJSON()`; an empty candidate,
//!   the end of gathering, is ignored, and so are mDNS `.local` names);
//! * engine → guest `{"type": "answer", "sdp": "…"}` (with every candidate the engine had), later
//!   `{"type": "candidate", "candidate": "candidate:…", "sdpMid": <the offer's mid>,
//!   "sdpMLineIndex": 0}` for an address found after it (a router mapping), or `{"type": "bye",
//!   "why": "full" | "offer"}` when the engine will not take the offer.
//!
//! **The connection** ([`rtc`]): the guest makes the offer with one reliable, ordered data channel
//! labelled `"join"`. ICE has this computer's addresses, what the STUN servers in the settings see
//! (`stun.cloudflare.com:3478`, `stun.l.google.com:19302` unless the host changed them), and the
//! router mapping UPnP or NAT-PMP made ([`nat`]). No TURN: a pair of NATs that cannot meet shows
//! as `"failed"` on the host's panel.
//!
//! **On the channel** every message of `/api/join/ws` (join.rs) goes unchanged: the hello, stages,
//! status, pad frames, the picture and sound. Binary ones are cut into pieces as [`chunk`] says;
//! text ones go whole. The guest session is join.rs's own ([`crate::join::guest_session`]) over
//! [`RtcSocket`]. When the channel's buffer passes [`HIGH_WATER`], the session stops taking
//! pictures, its shallow queue fills and the stream skips this guest to the next keyframe: the
//! slow-guest rule of the LAN socket.
//!
//! **The phone link** (pad_link.rs: a phone as a controller, or a device that watches) has rooms of
//! its own ([`Kind::Pad`]): the link `https://ps5upload.phantomptr.com/pad/#room=…&k=…&fp=…&hub=…`
//! (`&w=1` to watch), a data channel labelled `"pad"`, and on it the messages of `/api/pad/ws`
//! after a first `{"hello": {"t": key, "name", "d", "w"}}` (the LAN socket's query). A room's
//! peers get only its own kind's session: a channel with the other label is ignored.

pub(crate) mod chunk;
pub(crate) mod crypto;
pub(crate) mod hub;
pub(crate) mod local_signal;
pub(crate) mod nat;
pub(crate) mod rtc;
pub(crate) mod stun;

#[cfg(test)]
mod ice_path;
#[cfg(test)]
mod link_sim;
#[cfg(test)]
pub(crate) mod tests;

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::Message;
use tokio::sync::{mpsc, Notify};

/// The guest page that reads an internet invite.
pub(crate) const PAGE: &str = "https://ps5upload.phantomptr.com/join/";
/// The phone page that reads an internet phone link (a controller, or `&w=1` to watch).
pub(crate) const PAD_PAGE: &str = "https://ps5upload.phantomptr.com/pad/";
/// The page that reads a Share link in a browser (share/mod.rs): file downloads only.
pub(crate) const SHARE_PAGE: &str = "https://ps5upload.phantomptr.com/share/";

/// What a room's peers come for, and so which session their channel gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Play Together (join.rs).
    Join,
    /// The phone link (pad_link.rs): a controller or a watcher.
    Pad,
    /// A Share link (share/mod.rs): a friend receiving what was shared, over a "share" channel.
    Share,
}

impl Kind {
    /// The label of the data channel the page opens.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Kind::Join => "join",
            Kind::Pad => "pad",
            Kind::Share => "share",
        }
    }

    /// The website's page its links open.
    pub(crate) fn page(self) -> &'static str {
        match self {
            Kind::Join => PAGE,
            Kind::Pad => PAD_PAGE,
            Kind::Share => SHARE_PAGE,
        }
    }
}
pub(crate) const DEFAULT_HUB: &str = "wss://hub.phantomptr.com/ws";
/// Public STUN servers the engine (and the guest page) ask by default. The hub's own STUN at
/// `hub.phantomptr.com:3478` is used only when the settings name it.
pub(crate) const DEFAULT_STUN: [&str; 2] = ["stun.cloudflare.com:3478", "stun.l.google.com:19302"];
/// Bytes waiting on a guest's channel past which the session stops sending (its pictures then
/// skip to the next keyframe).
pub(crate) const HIGH_WATER: usize = 256 * 1024;
/// A peer that left (or failed) stays on the host's panel this long.
const ENDED_SHOWN: Duration = Duration::from_secs(30);

/// One internet guest's connection, as the host's panel shows it.
pub(crate) struct PeerInfo {
    /// The hub's id for the guest (not secret: the hub made it).
    pub gid: String,
    pub room: String,
    started: Instant,
    st: Mutex<PeerSt>,
    /// Its channel's back-pressure (set once the driver made it): the link numbers read it.
    flow: std::sync::OnceLock<Arc<Flow>>,
}

#[derive(Default)]
struct PeerSt {
    ice: &'static str,
    connected: bool,
    failed: bool,
    ended: Option<Instant>,
    rtt_ms: Option<u32>,
    /// The selected candidate pair (this engine's socket, the guest's address), from the stats.
    pair: Option<(std::net::SocketAddr, std::net::SocketAddr)>,
    why: Option<&'static str>,
    /// What went out to the guest over the last stats second (every byte on the wire), kbit/s.
    sent_kbps: Option<u32>,
    /// `peer_bytes_tx` and its time at the last stats event.
    sent_at: Option<(u64, Instant)>,
    /// The video track (rtc.rs), when the guest's offer had one the engine can send on.
    track: Option<TrackSt>,
}

/// An internet guest's video track: what the offer allows, whether the session uses it, and its
/// numbers (each set by the driver as str0m reports them; nothing per frame is kept).
#[derive(Default, Clone)]
pub(crate) struct TrackSt {
    /// The answer has an H.264 (packetization mode 1) or an H.265 payload type to send with.
    pub h264: bool,
    pub h265: bool,
    /// The session sends its pictures on the track (else on the data channel).
    pub on: bool,
    /// Access units written to the track.
    pub frames: u64,
    /// From str0m's egress stats: RTP packets sent (retransmissions included), NACKs, PLIs and
    /// FIRs received, the loss the guest reported, the RTCP round trip.
    pub packets: u64,
    pub nacks: u64,
    pub plis: u64,
    pub firs: u64,
    pub loss: Option<f32>,
    pub rtcp_rtt_ms: Option<u32>,
    /// The send-side bandwidth estimate (TWCC), kbit/s, and since when it stays under the
    /// stream's bitrate.
    pub bwe_kbps: Option<u32>,
    pub under_since: Option<Instant>,
    /// The stream's bitrate (kbit/s): the console cannot change it mid-session.
    pub stream_kbps: Option<u32>,
    /// When the session put its pictures on the track: the connection check runs from here
    /// ([`CHECK_FOR`]: str0m's estimate probes up to [`rtc`]'s BWE_HEADROOM over the rate), and
    /// whether its result was logged.
    pub on_at: Option<Instant>,
    pub checked: bool,
    /// The offer has an audio section with Opus, and the session's sound goes there (#720:
    /// the browser's jitter buffer and loss concealment, not the page's own player).
    pub opus: bool,
    pub audio_on: bool,
}

/// The connection check at a guest's start: this long of the estimate (with str0m's probing) and
/// the RTCP round trip and loss before the result counts.
pub(crate) const CHECK_FOR: Duration = Duration::from_millis(2500);
/// What a suggestion leaves free over the slowest guest's estimate: it fits at this share of it.
pub(crate) const FIT_SHARE: f64 = 1.0 / 1.2;

/// The estimate under the stream's bitrate this long: the host is told to lower the quality.
pub(crate) const SLOW_FOR: Duration = Duration::from_secs(5);
/// …by this much (an estimate a few percent under the rate still carries it).
const SLOW_UNDER: f64 = 0.9;

impl TrackSt {
    /// A new estimate. Under the line ([`SLOW_UNDER`] of the rate) it counts from then; it
    /// stops counting only once the estimate is back up to the rate itself (hysteresis: one that
    /// hovers about the line does not start over every second).
    pub(crate) fn estimate(&mut self, kbps: u32, now: Instant) {
        self.bwe_kbps = Some(kbps);
        let Some(s) = self.stream_kbps.map(|s| s as f64) else {
            self.under_since = None;
            return;
        };
        let k = kbps as f64;
        if k >= s {
            self.under_since = None;
        } else if k < s * SLOW_UNDER && self.under_since.is_none() {
            self.under_since = Some(now);
        }
    }

    /// The connection check is still running (the track just started, or no estimate yet).
    pub(crate) fn measuring(&self, now: Instant) -> bool {
        self.on
            && (self.bwe_kbps.is_none()
                || self
                    .on_at
                    .is_none_or(|t| now.saturating_duration_since(t) < CHECK_FOR))
    }

    /// How long the estimate has stayed under the line, whole seconds (0 when it is not).
    pub(crate) fn under_s(&self, now: Instant) -> u64 {
        self.under_since
            .map_or(0, |t| now.saturating_duration_since(t).as_secs())
    }

    /// The most this guest carries with room to spare ([`FIT_SHARE`] of the estimate), kbit/s.
    pub(crate) fn fit_kbps(&self) -> Option<u32> {
        self.bwe_kbps.map(|k| (k as f64 * FIT_SHARE).round() as u32)
    }

    /// The estimate stayed under the stream's bitrate for [`SLOW_FOR`].
    pub(crate) fn slow(&self, now: Instant) -> bool {
        self.on
            && self
                .under_since
                .is_some_and(|t| now.saturating_duration_since(t) >= SLOW_FOR)
    }
}

impl PeerInfo {
    fn new(room: &str, gid: &str) -> Arc<PeerInfo> {
        let p = Arc::new(PeerInfo {
            gid: gid.to_string(),
            room: room.to_string(),
            started: Instant::now(),
            st: Mutex::new(PeerSt {
                ice: "new",
                ..PeerSt::default()
            }),
            flow: std::sync::OnceLock::new(),
        });
        let mut g = PEERS.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        g.retain(|p| {
            p.st.lock()
                .map(|s| s.ended.is_none_or(|t| now.duration_since(t) < ENDED_SHOWN))
                .unwrap_or(false)
        });
        g.push(p.clone());
        p
    }

    fn with<R>(&self, f: impl FnOnce(&mut PeerSt) -> R) -> R {
        f(&mut self.st.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub(crate) fn path(&self) -> &'static str {
        self.with(|s| {
            if s.failed {
                "failed"
            } else if s.ended.is_some() {
                "closed"
            } else if s.connected {
                "direct"
            } else {
                "connecting"
            }
        })
    }

    pub(crate) fn rtt_ms(&self) -> Option<u32> {
        self.with(|s| s.rtt_ms)
    }

    /// Which way the connection goes now: `"lan"` (the home network), `"tailscale"` or
    /// `"internet"` (straight through both routers), from the pair ICE selected; None before it
    /// connected.
    pub(crate) fn net_path(&self) -> Option<&'static str> {
        self.with(|s| s.pair)
            .map(|(local, remote)| rtc::path_of(local, remote))
    }

    /// The driver's view of the video track (rtc.rs).
    pub(crate) fn track_with<R>(&self, f: impl FnOnce(&mut TrackSt) -> R) -> Option<R> {
        self.with(|s| s.track.as_mut().map(f))
    }

    /// The offer had a video section the engine can send on (with these codecs).
    pub(crate) fn track_offered(&self, h264: bool, h265: bool) {
        self.with(|s| {
            let t = s.track.get_or_insert_with(TrackSt::default);
            t.h264 = h264;
            t.h265 = h265;
        });
    }

    /// The offer had an audio section with Opus (rtc.rs). Before or after its video section: the
    /// track's state is made by whichever came first.
    pub(crate) fn audio_offered(&self) {
        self.with(|s| s.track.get_or_insert_with(TrackSt::default).opus = true);
    }

    /// The session's sound on the audio track, when the offer had one and the pictures go on the
    /// video track (the two are played together, in sync). True when it goes there.
    pub(crate) fn use_audio(&self) -> bool {
        self.track_with(|t| {
            t.audio_on = t.opus && t.on;
            t.audio_on
        })
        .unwrap_or(false)
    }

    /// The session's choice: its pictures (`hevc` or not) on the track, when it can carry them.
    /// True when they go there.
    pub(crate) fn use_track(&self, hevc: bool, stream_kbps: u32) -> bool {
        self.track_with(|t| {
            t.on = if hevc { t.h265 } else { t.h264 };
            t.stream_kbps = Some(stream_kbps);
            if t.on && t.on_at.is_none() {
                t.on_at = Some(Instant::now());
            }
            t.on
        })
        .unwrap_or(false)
    }

    /// The driver's stats event (once a second): the bytes sent so far, for the rate.
    fn sent(&self, total: u64, now: Instant) {
        self.with(|s| {
            if let Some((was, at)) = s.sent_at {
                let dt = now.saturating_duration_since(at).as_secs_f64();
                if dt > 0.2 {
                    let kbps = total.saturating_sub(was) as f64 * 8.0 / 1000.0 / dt;
                    s.sent_kbps = Some(kbps.round().min(u32::MAX as f64) as u32);
                }
            }
            s.sent_at = Some((total, now));
        });
    }

    /// The connection's numbers, read on demand (the host's panel, together_status, a bug report):
    /// round trip, what went out over the last second, and what waits to go.
    pub(crate) fn link_json(&self) -> serde_json::Value {
        let f = self.flow.get();
        let now = Instant::now();
        let (rtt_ms, sent_kbps, track) = self.with(|s| (s.rtt_ms, s.sent_kbps, s.track.clone()));
        let on = track.as_ref().is_some_and(|t| t.on);
        let path = self.net_path();
        serde_json::json!({
            // How the guest is connected: "lan" (the home network), "tailscale", "internet"
            // (direct through both routers); null while ICE has no pair yet.
            "path": path,
            // Where the picture goes: "track" (RTP) or "channel" (the data channel).
            "video": if on { "track" } else { "channel" },
            // Where the sound goes: "track" (Opus RTP, beside the picture) or "channel".
            "audio": if track.as_ref().is_some_and(|t| t.audio_on) { "track" } else { "channel" },
            // The track's numbers (null on the channel): `frames_sent` (access units written),
            // `rtp_packets` (retransmissions included), `nacks` (each answered with a resend from
            // the send buffer), `plis` / `firs` (keyframe asks from the guest's browser), `loss` (its
            // receiver reports, 0-1), `rtcp_rtt_ms`, `bwe_kbps` (the bandwidth estimate),
            // `stream_kbps`, and `slow`: the estimate stayed under the stream's rate (lower the
            // stream's quality: the console cannot change it mid-session).
            "track": track.filter(|t| t.on).map(|t| serde_json::json!({
                "frames_sent": t.frames,
                "rtp_packets": t.packets,
                "nacks": t.nacks,
                "plis": t.plis,
                "firs": t.firs,
                "loss": t.loss.map(|l| (l as f64 * 1000.0).round() / 1000.0),
                "rtcp_rtt_ms": t.rtcp_rtt_ms,
                "bwe_kbps": t.bwe_kbps,
                "stream_kbps": t.stream_kbps,
                "slow": t.slow(now),
                // The connection check: still running (the first CHECK_FOR of the track), how
                // long the estimate has stayed under the stream's rate (s, with hysteresis), and
                // the most it carries with 20% to spare (kbit/s): what a lower quality for
                // everyone should fit under.
                "measuring": t.measuring(now),
                "under_s": t.under_s(now),
                "fit_kbps": t.fit_kbps(),
            })),
            "rtt_ms": rtt_ms,
            "sent_kbps": sent_kbps,
            // The driver's queue (messages the channel did not take yet) and the data channel's
            // own buffer (SCTP: sent but not acknowledged, or not sent yet).
            "queued_bytes": f.map(|f| f.driver.load(Ordering::Relaxed)),
            "buffered_bytes": f.map(|f| f.channel.load(Ordering::Relaxed)),
            // The most that waited (both together) over the last stats second, and ever.
            "backlog_peak_1s_bytes": f.map(|f| f.peak_1s.load(Ordering::Relaxed)),
            "backlog_peak_bytes": f.map(|f| f.peak.load(Ordering::Relaxed)),
            // Times the session's send waited for the channel (its pictures then skip ahead).
            "send_waits": f.map(|f| f.waits.load(Ordering::Relaxed)),
            "high_water_bytes": HIGH_WATER,
        })
    }

    pub(crate) fn json(&self, now: Instant) -> serde_json::Value {
        let path = self.path();
        let link = self.link_json();
        self.with(|s| {
            serde_json::json!({
                "gid": self.gid,
                "path": path,
                "ice": s.ice,
                "rtt_ms": s.rtt_ms,
                "since_s": now.duration_since(self.started).as_secs(),
                "why": s.why,
                "link": link,
            })
        })
    }
}

/// Every internet peer still shown (ended ones for [`ENDED_SHOWN`]).
static PEERS: Mutex<Vec<Arc<PeerInfo>>> = Mutex::new(Vec::new());

/// The peers of `room`, for the host's panel.
pub(crate) fn peers_json(room: &str) -> Vec<serde_json::Value> {
    let now = Instant::now();
    PEERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|p| p.room == room)
        .filter(|p| p.with(|s| s.ended.is_none_or(|t| now.duration_since(t) < ENDED_SHOWN)))
        .map(|p| p.json(now))
        .collect()
}

/// Back-pressure between a guest session and its peer connection.
#[derive(Default)]
pub(crate) struct Flow {
    /// Bytes the session sent that the driver has not taken yet.
    unwritten: AtomicUsize,
    /// Bytes the driver holds plus what the data channel buffers.
    queued: AtomicUsize,
    closed: AtomicBool,
    notify: Notify,
    // What the link numbers show (PeerInfo::link_json): the two parts of `queued`, its peak over
    // the last stats second and ever, and how often a send had to wait.
    driver: AtomicUsize,
    channel: AtomicUsize,
    peak_1s: AtomicUsize,
    peak_now: AtomicUsize,
    peak: AtomicUsize,
    waits: AtomicU64,
}

impl Flow {
    fn pending(&self) -> usize {
        self.unwritten.load(Ordering::Relaxed) + self.queued.load(Ordering::Relaxed)
    }
    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        self.notify.notify_waiters();
    }
    /// The driver's queue and the channel's buffer now.
    fn set_queued(&self, driver: usize, channel: usize) {
        let n = driver + channel;
        self.driver.store(driver, Ordering::Relaxed);
        self.channel.store(channel, Ordering::Relaxed);
        self.peak_now.fetch_max(n, Ordering::Relaxed);
        self.peak.fetch_max(n, Ordering::Relaxed);
        self.queued.store(n, Ordering::Relaxed);
        if self.pending() <= HIGH_WATER {
            self.notify.notify_waiters();
        }
    }
    /// A stats second ended: its peak is the one shown, the next starts from now.
    fn second(&self) {
        let now = self.queued.load(Ordering::Relaxed);
        self.peak_1s.store(
            self.peak_now.swap(now, Ordering::Relaxed),
            Ordering::Relaxed,
        );
    }
}

/// The guest session's side of an internet guest's data channel.
pub(crate) struct RtcSocket {
    id: u64,
    rx: mpsc::Receiver<Message>,
    up: mpsc::UnboundedSender<rtc::Cmd>,
    flow: Arc<Flow>,
    info: Arc<PeerInfo>,
}

fn msg_len(m: &Message) -> usize {
    match m {
        Message::Text(t) => t.len(),
        Message::Binary(b) => b.len(),
        _ => 8,
    }
}

impl crate::join::GuestSocket for RtcSocket {
    async fn send(&mut self, m: Message) -> Result<(), ()> {
        // Wait while the channel is full: the session then stops taking pictures.
        if self.flow.pending() > HIGH_WATER {
            self.flow.waits.fetch_add(1, Ordering::Relaxed);
        }
        loop {
            if self.flow.closed.load(Ordering::Relaxed) {
                return Err(());
            }
            if self.flow.pending() <= HIGH_WATER {
                break;
            }
            let n = self.flow.notify.notified();
            tokio::pin!(n);
            n.as_mut().enable();
            if self.flow.closed.load(Ordering::Relaxed) || self.flow.pending() <= HIGH_WATER {
                continue;
            }
            let _ = tokio::time::timeout(Duration::from_millis(100), n).await;
        }
        let len = msg_len(&m);
        self.flow.unwritten.fetch_add(len, Ordering::Relaxed);
        self.up.send(rtc::Cmd::Up(self.id, m, len)).map_err(|_| ())
    }

    async fn recv(&mut self) -> Option<Result<Message, ()>> {
        self.rx.recv().await.map(Ok)
    }

    fn net(&self) -> Option<Arc<PeerInfo>> {
        Some(self.info.clone())
    }

    fn video_track(&mut self, hevc: bool, stream_kbps: u32) -> bool {
        let on = self.info.use_track(hevc, stream_kbps);
        if on {
            let _ = self.up.send(rtc::Cmd::Track(self.id, hevc, stream_kbps));
        }
        on
    }

    fn audio_track(&mut self) -> bool {
        let on = self.info.use_audio();
        if on {
            let _ = self.up.send(rtc::Cmd::AudioOn(self.id));
        }
        on
    }

    async fn send_audio(&mut self, m: Message) -> Result<(), ()> {
        if self.flow.closed.load(Ordering::Relaxed) {
            return Err(());
        }
        match m {
            Message::Binary(b) => self.up.send(rtc::Cmd::Audio(self.id, b)).map_err(|_| ()),
            _ => Ok(()),
        }
    }

    async fn send_video(&mut self, m: Message) -> Result<(), ()> {
        // No back-pressure: str0m sends a picture at once and keeps its packets for resends, so
        // there is no backlog to skip (a lost packet costs a resend, not the picture).
        if self.flow.closed.load(Ordering::Relaxed) {
            return Err(());
        }
        match m {
            Message::Binary(b) => self.up.send(rtc::Cmd::Video(self.id, b)).map_err(|_| ()),
            _ => Ok(()),
        }
    }
}

impl Drop for RtcSocket {
    fn drop(&mut self) {
        let _ = self.up.send(rtc::Cmd::Gone(self.id));
    }
}

/// The engine's DTLS certificate fingerprint (one certificate per engine run), as the invite link
/// carries it: SHA-256, upper-case hex with colons.
pub(crate) fn fingerprint() -> Option<String> {
    rtc::cert().map(|c| c.1.clone())
}

/// The hub's host (and port) as the link names it, from its `wss://host[:port]/path` URL.
pub(crate) fn hub_host(url: &str) -> Option<String> {
    let uri: axum::http::Uri = url.parse().ok()?;
    if !matches!(uri.scheme_str(), Some("wss" | "ws")) {
        return None;
    }
    uri.authority().map(|a| a.as_str().to_string())
}

/// A hub URL the settings may hold: `wss://`, or `ws://` to this computer (a hub under test).
pub(crate) fn hub_url_ok(url: &str) -> bool {
    let Ok(uri) = url.parse::<axum::http::Uri>() else {
        return false;
    };
    let Some(host) = uri.host() else {
        return false;
    };
    match uri.scheme_str() {
        Some("wss") => true,
        Some("ws") => {
            let h = host.trim_start_matches('[').trim_end_matches(']');
            h == "localhost"
                || h.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        }
        _ => false,
    }
}

/// A room's link (an internet invite, or the phone link's), on the website's page for its kind.
/// `lan`: this engine's own page for the kind on the home network (`http://192.168.1.10:19113/join`),
/// the hint the page offers as "Open on this Wi-Fi" when it cannot reach the hub, and where the
/// app signals without the hub ([`local_signal`]). It is no secret: the page there asks the host,
/// as the link's key does. `ts`: the same page at this computer's Tailscale address (the Tailscale
/// app's), when it has one and it is not `lan` already: an app guest on the same tailnet signals
/// there. Reachable only from inside that tailnet, so no more telling than `lan`.
pub(crate) fn link_for(
    kind: Kind,
    room: &str,
    key: &str,
    fp: &str,
    hub: &str,
    lan: Option<&str>,
    ts: Option<&str>,
) -> String {
    let mut s = format!("{}#room={room}&k={key}&fp={fp}&hub={hub}", kind.page());
    let page = |p: &&str| p.starts_with("http://") || p.starts_with("https://");
    let lan = lan.filter(page);
    if let Some(l) = lan {
        s.push_str("&lan=");
        s.push_str(&pct(l));
    }
    if let Some(t) = ts.filter(page).filter(|t| Some(*t) != lan) {
        s.push_str("&ts=");
        s.push_str(&pct(t));
    }
    s
}

/// `s` with everything but RFC 3986's unreserved characters percent-encoded (a fragment value).
pub(crate) fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `n` random bytes as base64url without padding (16 → 22 characters, 32 → 43).
pub(crate) fn random_b64(n: usize) -> Option<String> {
    use base64::Engine;
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).ok()?;
    Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b))
}
