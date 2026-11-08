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
pub(crate) mod nat;
pub(crate) mod rtc;
pub(crate) mod stun;

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

/// What a room's peers come for, and so which session their channel gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Play Together (join.rs).
    Join,
    /// The phone link (pad_link.rs): a controller or a watcher.
    Pad,
}

impl Kind {
    /// The label of the data channel the page opens.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Kind::Join => "join",
            Kind::Pad => "pad",
        }
    }

    /// The website's page its links open.
    pub(crate) fn page(self) -> &'static str {
        match self {
            Kind::Join => PAGE,
            Kind::Pad => PAD_PAGE,
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
    why: Option<&'static str>,
    /// What went out to the guest over the last stats second (every byte on the wire), kbit/s.
    sent_kbps: Option<u32>,
    /// `peer_bytes_tx` and its time at the last stats event.
    sent_at: Option<(u64, Instant)>,
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
        let (rtt_ms, sent_kbps) = self.with(|s| (s.rtt_ms, s.sent_kbps));
        serde_json::json!({
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
pub(crate) fn link_for(kind: Kind, room: &str, key: &str, fp: &str, hub: &str) -> String {
    format!("{}#room={room}&k={key}&fp={fp}&hub={hub}", kind.page())
}

/// `n` random bytes as base64url without padding (16 → 22 characters, 32 → 43).
pub(crate) fn random_b64(n: usize) -> Option<String> {
    use base64::Engine;
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).ok()?;
    Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b))
}
