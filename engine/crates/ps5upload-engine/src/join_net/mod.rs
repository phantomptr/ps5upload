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

pub(crate) mod chunk;
pub(crate) mod crypto;
pub(crate) mod hub;
pub(crate) mod nat;
pub(crate) mod rtc;
pub(crate) mod stun;

#[cfg(test)]
pub(crate) mod tests;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::Message;
use tokio::sync::{mpsc, Notify};

/// The guest page that reads an internet invite.
pub(crate) const PAGE: &str = "https://ps5upload.phantomptr.com/join/";
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
}

#[derive(Default)]
struct PeerSt {
    ice: &'static str,
    connected: bool,
    failed: bool,
    ended: Option<Instant>,
    rtt_ms: Option<u32>,
    why: Option<&'static str>,
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

    pub(crate) fn json(&self, now: Instant) -> serde_json::Value {
        let path = self.path();
        self.with(|s| {
            serde_json::json!({
                "gid": self.gid,
                "path": path,
                "ice": s.ice,
                "rtt_ms": s.rtt_ms,
                "since_s": now.duration_since(self.started).as_secs(),
                "why": s.why,
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
}

impl Flow {
    fn pending(&self) -> usize {
        self.unwritten.load(Ordering::Relaxed) + self.queued.load(Ordering::Relaxed)
    }
    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        self.notify.notify_waiters();
    }
    fn set_queued(&self, n: usize) {
        self.queued.store(n, Ordering::Relaxed);
        if self.pending() <= HIGH_WATER {
            self.notify.notify_waiters();
        }
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

/// The guest's link for an internet invite.
pub(crate) fn invite_link(room: &str, key: &str, fp: &str, hub: &str) -> String {
    format!("{PAGE}#room={room}&k={key}&fp={fp}&hub={hub}")
}

/// `n` random bytes as base64url without padding (16 → 22 characters, 32 → 43).
pub(crate) fn random_b64(n: usize) -> Option<String> {
    use base64::Engine;
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).ok()?;
    Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b))
}
