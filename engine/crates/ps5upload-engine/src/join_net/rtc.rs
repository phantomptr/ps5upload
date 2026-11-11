//! The engine's one WebRTC endpoint: a UDP socket on each of this computer's IPv4 addresses (one
//! port number for all when it can), shared by every internet guest's peer connection (str0m tells
//! which packet is whose). One task drives them all; the guest sessions talk to it through
//! [`Cmd`].

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::ws::Message;
use str0m::bwe::{Bitrate, BweKind};
use str0m::change::SdpOffer;
use str0m::channel::ChannelId;
use str0m::config::DtlsCert;
use str0m::format::Codec;
use str0m::media::{Direction, Frequency, KeyframeRequestKind, MediaKind, MediaTime, Mid, Pt};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use super::chunk::{self, Piece, Reassembler};
use super::{nat, stun, Flow, Kind, PeerInfo, RtcSocket};

/// Peers being set up or connected at once, every room together.
const MAX_PEERS: usize = 32;
/// …and per room.
const MAX_ROOM_PEERS: usize = 8;
/// ICE and DTLS must be through this soon after the offer.
const CONNECT_WAIT: Duration = Duration::from_secs(30);
/// A connection that lost its path for this long has failed.
const DISCONNECTED_WAIT: Duration = Duration::from_secs(10);
/// A session that ended has this long to get its last messages (the bye) out.
const CLOSE_DRAIN: Duration = Duration::from_secs(2);
/// How long STUN servers have to answer.
const STUN_WAIT: Duration = Duration::from_secs(2);
/// What the data channel's SCTP may hold (str0m's default is 128 KiB, below [`super::HIGH_WATER`]).
pub(crate) const SCTP_BUFFER: usize = 1024 * 1024;
/// Messages queued from the channel to the session before the peer is dropped as a flood.
const TO_SESSION: usize = 256;
/// The video track's send buffer, in packets: what str0m keeps to answer NACKs (and to probe
/// with). About 2 s at 15 Mbit/s (1,200-byte packets), as a browser's jitter buffer waits for a
/// resend at most that long.
pub(crate) const SEND_BUFFER_VIDEO: usize = 3000;
/// The bandwidth estimate starts here (the Play screen's default bitrate) until the session says
/// its own: started lower, the pacer would hold the pictures back while it climbs.
const BWE_START_KBPS: u64 = 15_000;
/// The estimate looks this far above the stream's bitrate (to tell whether the link carries it).
const BWE_HEADROOM: f64 = 1.25;

/// A guest whose link carries less than the stream (#720: about 2 Mbit/s for a 15 Mbit/s stream).
/// str0m's pacer sends at its estimate, and a picture written to the track waits in its queue
/// until the pacer gets to it (up to its 2 s drain): the guest's picture fell seconds behind, and
/// a written picture can't be taken back. So the track keeps its own short queue in front of
/// str0m's ([`Catchup`]) and writes a picture only when str0m's queue is down to
/// [`TRACK_FEED`]: a picture that waited there longer than this is dropped, with every one after
/// it up to the next keyframe, and a keyframe that comes takes the place of every picture still
/// waiting. That guest jumps to a fresh picture; the host's stream, and every other guest, are not
/// touched (the keyframe is asked for through the session's gates).
pub(crate) const TRACK_LAG_MAX: Duration = Duration::from_millis(400);
/// A picture goes to str0m once its queue would send what it holds within this.
const TRACK_FEED: Duration = Duration::from_millis(20);
/// The keyframe is asked for once a picture waited this long (so that it comes before one has to
/// be dropped), and while pictures are dropped, again this often (the session's gates hold the
/// asks to theirs).
const TRACK_ASK_AT: Duration = Duration::from_millis(200);
const TRACK_ASK_EVERY: Duration = Duration::from_millis(500);
/// str0m's queue's wait is reckoned at the rate it was seen to drain at (before that, the
/// estimate), never below this.
const TRACK_FLOOR_KBPS: u32 = 500;
/// str0m's pacer sends at its estimate times this (pacer/control.rs PACING_FACTOR).
const PACING_FACTOR: f64 = 1.1;

/// A peer hands its socket at most this many datagrams in a row (two of the pacer's 63 kB
/// bursts) before the other peers get their turn…
const TURN_DATAGRAMS: usize = 128;
/// …and has its due timeouts taken at once at most this many times in a row with no datagram to
/// show for it: after that the runtime's timer has it (a peer that says "now" over and over
/// must not spin the driver).
const TURN_IDLE: usize = 16;
/// Packets in and commands the driver takes in a turn it does not wait in.
const TURN_TAKES: usize = 16;
/// The driver lets the runtime's other tasks run after this many turns without a wait.
const YIELD_EVERY: u32 = 32;

/// The estimate is raised back to the stream's rate when it fell under this share of it on a
/// clean link ([`KeepUp`]).
const KEEP_UP_UNDER: f64 = 0.9;
/// …a clean link: under this loss (the guest's reports), at most this many NACKs in the second,
/// and a round trip at most this much over the lowest seen.
const KEEP_UP_LOSS: f32 = 0.02;
const KEEP_UP_NACKS: u64 = 2;
const KEEP_UP_RTT_RISE_MS: u32 = 30;
/// …after the link stayed clean this many seconds in a row; a raise followed within
/// [`KEEP_UP_WATCH`] by loss, NACKs or a rising round trip failed (the link is really that slow):
/// the next one waits [`KEEP_UP_EVERY`] doubled per failure, and after [`KEEP_UP_TRIES`] failures
/// this guest's estimate is left alone.
const KEEP_UP_CLEAN_S: u32 = 3;
const KEEP_UP_WATCH: Duration = Duration::from_secs(5);
const KEEP_UP_EVERY: Duration = Duration::from_secs(2);
const KEEP_UP_TRIES: u32 = 3;

/// The estimate kept at the stream's rate on a clean link (#720). Over a light picture (a menu:
/// a fraction of the rate) the congestion control only sees that fraction acknowledged, and any
/// blip of delay takes the estimate down to about it: on a wired home network, 1-2 Mbit/s for a
/// 10 Mbit/s stream. A browser's encoder follows the estimate; the console cannot, so when the
/// game moves again str0m's pacer holds its pictures to a little over that estimate while it
/// climbs back (about 8% a second), and the guest falls seconds behind. A link that is really
/// too slow shows it as loss or a rising round trip, and is left to the estimate and
/// [`Catchup`]. Shared with the link harness (link_sim.rs).
#[derive(Default, Debug)]
pub(crate) struct KeepUp {
    min_rtt_ms: Option<u32>,
    nacks: Option<u64>,
    /// Seconds in a row the link was clean.
    clean_s: u32,
    /// The last raise, and whether the link stayed clean since; raises that failed.
    last: Option<Instant>,
    watching: bool,
    failed: u32,
    /// Times the estimate was raised.
    pub raised: u64,
}

impl KeepUp {
    /// The last second's look found the link clean (no loss, at most a NACK or two, the round
    /// trip at its usual).
    pub(crate) fn clean_now(&self) -> bool {
        self.clean_s > 0
    }
}

impl KeepUp {
    /// Once a second: the rate to raise the estimate to, or None. `nacks` is str0m's running
    /// count, `loss` the guest's last report, `rtt_ms` the round trip now.
    pub(crate) fn second(
        &mut self,
        estimate_kbps: Option<u32>,
        stream_kbps: Option<u32>,
        loss: Option<f32>,
        nacks: u64,
        rtt_ms: Option<u32>,
        now: Instant,
    ) -> Option<u32> {
        let new_nacks = nacks.saturating_sub(self.nacks.unwrap_or(nacks));
        self.nacks = Some(nacks);
        if let Some(r) = rtt_ms {
            self.min_rtt_ms = Some(self.min_rtt_ms.map_or(r, |m| m.min(r)));
        }
        let clean = loss.unwrap_or(0.0) < KEEP_UP_LOSS
            && new_nacks <= KEEP_UP_NACKS
            && match (rtt_ms, self.min_rtt_ms) {
                (Some(r), Some(m)) => r <= m + KEEP_UP_RTT_RISE_MS,
                _ => false,
            };
        let since = self.last.map(|t| now.saturating_duration_since(t));
        if self.watching {
            if !clean {
                self.watching = false;
                self.failed += 1;
            } else if since.is_some_and(|d| d >= KEEP_UP_WATCH) {
                self.watching = false;
            }
        }
        self.clean_s = if clean { self.clean_s + 1 } else { 0 };
        let (est, stream) = (estimate_kbps?, stream_kbps?);
        if stream == 0 || (est as f64) >= stream as f64 * KEEP_UP_UNDER {
            return None;
        }
        let wait = KEEP_UP_EVERY * 2u32.pow(self.failed.min(5));
        if self.failed >= KEEP_UP_TRIES
            || self.watching
            || self.clean_s < KEEP_UP_CLEAN_S
            || since.is_some_and(|d| d < wait)
        {
            return None;
        }
        self.last = Some(now);
        self.watching = true;
        self.raised += 1;
        Some(stream)
    }
}

/// A picture waiting for the track.
#[derive(Debug, Clone)]
pub(crate) struct TrackPic {
    pub key: bool,
    /// When it came, its RTP time, and the access unit as the track sends it.
    pub at: Instant,
    pub rtp: MediaTime,
    pub data: Vec<u8>,
}

/// What [`Catchup`] did, for the counts, the log and the keyframe ask.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Caught {
    /// Pictures written to str0m.
    pub written: u64,
    /// Ask the console for a keyframe now.
    pub ask: bool,
    /// The guest just fell behind: pictures are dropped up to the next keyframe.
    pub fell: bool,
}

/// The track's catch-up ([`TRACK_LAG_MAX`]). Shared with the link harness (link_sim.rs).
#[derive(Default, Debug)]
pub(crate) struct Catchup {
    waiting: VecDeque<TrackPic>,
    /// Pictures are dropped up to the next keyframe; `fell`: that just began (not reported yet).
    need_key: bool,
    fell: bool,
    asked: Option<Instant>,
    /// The bandwidth estimate (kbit/s), and the rate str0m's queue was seen to drain at (bytes/s):
    /// the estimate is a smoothed one, and the pacer can send far under it for a while.
    estimate_kbps: Option<u32>,
    drained: Option<f64>,
    /// str0m's last look at its queue: when, the bytes it held, and the bytes written since.
    seen: Option<(Instant, usize)>,
    since: usize,
    /// Pictures dropped (left waiting too long, or passed over by a newer keyframe), and the
    /// times pictures were dropped.
    pub dropped: u64,
    pub jumps: u64,
    /// The longest a picture waited here before it went to str0m or was dropped, and the most
    /// str0m's queue held (bytes): for the numbers and a bug report.
    pub waited_max: Duration,
    pub queue_peak: usize,
}

impl Catchup {
    /// The track carries a stream of `kbps`: the estimate is aimed at it (it starts there and
    /// probes up to [`BWE_HEADROOM`] over it).
    pub(crate) fn aim(&mut self, rtc: &mut Rtc, kbps: u32) {
        self.estimate(kbps);
        rtc.bwe().reset(Bitrate::kbps(kbps as u64));
        rtc.bwe().set_bitrate(
            Bitrate::kbps(0),
            Bitrate::kbps((kbps as f64 * BWE_HEADROOM) as u64),
        );
    }

    pub(crate) fn estimate(&mut self, kbps: u32) {
        self.estimate_kbps = Some(kbps);
    }

    /// Pictures are being dropped up to the next keyframe.
    #[cfg(test)]
    pub(crate) fn behind(&self) -> bool {
        self.need_key
    }

    /// The rate str0m's queue was last seen to drain at (kbit/s); None: not seen yet.
    pub(crate) fn drain_kbps(&self) -> Option<u32> {
        self.drained.map(|d| (d / 125.0).round() as u32)
    }

    /// Pictures waiting, and how long the oldest has.
    pub(crate) fn waiting(&self, now: Instant) -> (usize, Duration) {
        let oldest = self
            .waiting
            .front()
            .map_or(Duration::ZERO, |p| now.saturating_duration_since(p.at));
        (self.waiting.len(), oldest)
    }

    /// A new picture: a keyframe takes the place of every picture still waiting; one that is not
    /// waits its turn, or is dropped while the guest waits for a keyframe.
    pub(crate) fn push(&mut self, pic: TrackPic) {
        if pic.key {
            if !self.waiting.is_empty() {
                self.dropped += self.waiting.len() as u64;
                self.jumps += 1;
                self.waiting.clear();
            }
            self.need_key = false;
        } else {
            // One waiting past TRACK_LAG_MAX already (str0m's queue does not drain) is dropped
            // now, not on its turn: the queue stays short however long str0m stalls.
            let first = self.waiting.iter().position(|p| !p.key);
            if let Some(i) = first
                .filter(|&i| pic.at.saturating_duration_since(self.waiting[i].at) > TRACK_LAG_MAX)
            {
                self.drop_from(i);
            }
            if self.need_key {
                self.dropped += 1;
                return;
            }
        }
        self.waiting.push_back(pic);
    }

    /// The pictures waiting from `i` on are dropped: the guest waits for a keyframe.
    fn drop_from(&mut self, i: usize) {
        self.waited_max = self.waited_max.max(TRACK_LAG_MAX);
        self.dropped += (self.waiting.len() - i) as u64;
        self.waiting.truncate(i);
        self.need_key = true;
        self.fell = true;
        self.jumps += 1;
    }

    /// How long str0m's queue takes to send `bytes`.
    fn drain(&self, bytes: usize) -> Duration {
        let est = self.estimate_kbps.unwrap_or(BWE_START_KBPS as u32) as f64 * PACING_FACTOR;
        let rate = self
            .drained
            .unwrap_or(est * 125.0)
            .max(TRACK_FLOOR_KBPS as f64 * 125.0);
        Duration::from_secs_f64(bytes as f64 / rate)
    }

    /// str0m's view of its queue ([`track_queue`]): its bytes, its oldest packet's wait, and when
    /// it looked. A new look tells the rate the queue drains at (while it never ran dry); the
    /// bytes written since the look are added to it.
    ///
    /// The rate it reads is a rough one (track_bench.rs, #720): str0m looks with every packet
    /// and its pacer sends a burst and then waits out its debt, so two looks 2 ms apart or more
    /// are often the ones either side of a wait. On a 15 Mbit/s home-network track it read
    /// 22-47 Mbit/s where the pacer sent 24-39; on a link capped at 12 Mbit/s, 0.9-2.9 while
    /// the estimate was 0.3-0.4. Neither cost a picture there, and links that carry less than
    /// the stream were tuned with it as it is (link_sim.rs): it is left alone.
    fn queue(&mut self, q: (usize, Duration, Option<Instant>)) -> (usize, Duration) {
        let (bytes, oldest, at) = q;
        if let Some(at) = at {
            match self.seen {
                Some((t, _)) if t == at => {}
                prev => {
                    if let Some((t, was)) = prev {
                        let dt = at.saturating_duration_since(t).as_secs_f64();
                        let out = (was + self.since).saturating_sub(bytes);
                        if bytes > 0 && out > 0 && dt >= 0.002 {
                            let r = out as f64 / dt;
                            self.drained = Some(self.drained.map_or(r, |d| d * 0.7 + r * 0.3));
                        }
                    }
                    self.seen = Some((at, bytes));
                    self.since = 0;
                }
            }
        }
        (bytes + self.since, oldest)
    }

    /// The next picture for str0m, if its queue (`queued` bytes, the oldest waiting `oldest`) is
    /// short enough. A picture that waited past [`TRACK_LAG_MAX`] is dropped with the rest up to
    /// the next keyframe.
    fn next(&mut self, queued: usize, oldest: Duration, now: Instant) -> Option<TrackPic> {
        if self.drain(queued).max(oldest) > TRACK_FEED {
            return None;
        }
        let pic = self.waiting.front()?;
        if !pic.key && now.saturating_duration_since(pic.at) > TRACK_LAG_MAX {
            self.drop_from(0);
            return None;
        }
        self.waited_max = self.waited_max.max(now.saturating_duration_since(pic.at));
        self.waiting.pop_front()
    }

    /// Writes what str0m's queue takes now to the track `mid` of `rtc` (payload type `pt`), and
    /// says whether to ask for a keyframe. Called with each new picture and as str0m's queue
    /// drains (each time the driver polls the peer).
    pub(crate) fn pump(&mut self, rtc: &mut Rtc, mid: Mid, pt: Pt, now: Instant) -> Caught {
        let mut c = Caught::default();
        loop {
            let (queued, oldest) = self.queue(track_queue(rtc, mid, now));
            self.queue_peak = self.queue_peak.max(queued);
            let Some(pic) = self.next(queued, oldest, now) else {
                break;
            };
            let size = pic.data.len();
            let Some(w) = rtc.writer(mid) else { break };
            if w.write(pt, now, pic.rtp, pic.data).is_ok() {
                self.since += size;
                c.written += 1;
            }
        }
        c.fell = std::mem::take(&mut self.fell);
        // The keyframe: asked for once a picture waited long, or while pictures are dropped.
        let (_, waited) = self.waiting(now);
        let want = self.need_key || waited >= TRACK_ASK_AT;
        if want
            && self
                .asked
                .is_none_or(|t| now.saturating_duration_since(t) >= TRACK_ASK_EVERY)
        {
            self.asked = Some(now);
            c.ask = true;
        }
        c
    }
}

/// The video track's send queue in str0m (pacer and resends): its bytes, how long the oldest has
/// waited, and when str0m looked (at its last timeout).
pub(crate) fn track_queue(
    rtc: &mut Rtc,
    mid: Mid,
    now: Instant,
) -> (usize, Duration, Option<Instant>) {
    rtc.direct_api()
        .stream_tx_by_mid(mid, None)
        .and_then(|s| s.queue_info())
        .map(|q| {
            let oldest = q
                .first_unsent()
                .map_or(Duration::ZERO, |t| now.saturating_duration_since(t));
            (q.byte_size(), oldest, Some(q.created_at()))
        })
        .unwrap_or_default()
}

/// How often a connected pair's binding is checked (str0m's default is 3 s), and how many tries a
/// check gets. In the path sim (ice_path.rs) a dead path shows after about 15 s instead of about
/// 21; fewer tries change nothing there (str0m's own pair timeouts rule the rest), so they stay
/// generous for a lossy link's first checks. The guest's page notices sooner on its own (no
/// message for 3.5 s) and comes back with its resume token.
pub(crate) const ICE_MAX_RTO: Duration = Duration::from_millis(1000);
pub(crate) const ICE_RETRANSMITS: usize = 6;

/// ICE timing for a game ([`ICE_MAX_RTO`], [`ICE_RETRANSMITS`]). Shared with the path sims.
pub(crate) fn game_ice(b: &mut RtcConfig) {
    b.set_max_stun_rto(ICE_MAX_RTO);
    b.set_max_stun_retransmits(ICE_RETRANSMITS);
}

/// An internet guest's peer connection with the video track on offer: H.264 (H.265 when the offer
/// has it) with str0m's NACK, RTX, PLI/FIR and transport-cc defaults, a send buffer for the
/// resends, and the bandwidth estimate. Shared with the link harness (link_sim.rs).
pub(crate) fn with_track(b: RtcConfig) -> RtcConfig {
    b.clear_codecs()
        .enable_h264(true)
        .enable_h265(true)
        .enable_opus(true, false)
        .set_send_buffer_video(SEND_BUFFER_VIDEO)
        .enable_bwe(Some(Bitrate::kbps(BWE_START_KBPS)))
}

/// The payload types a video section may be sent with, best first per codec: H.264 with
/// packetization mode 1 (FU-A, what a 1080p picture needs) with its profile-level-id, and H.265.
#[derive(Default, Debug, Clone)]
pub(crate) struct TrackPts {
    pub h264: Vec<(Pt, u32)>,
    pub h265: Option<Pt>,
}

impl TrackPts {
    pub(crate) fn of(rtc: &mut Rtc, mid: Mid) -> TrackPts {
        let mut t = TrackPts::default();
        if let Some(w) = rtc.writer(mid) {
            for p in w.payload_params() {
                let spec = p.spec();
                match spec.codec {
                    Codec::H264 if spec.format.packetization_mode == Some(1) => t
                        .h264
                        .push((p.pt(), spec.format.profile_level_id.unwrap_or(0x42e01f))),
                    Codec::H265 if t.h265.is_none() => t.h265 = Some(p.pt()),
                    _ => {}
                }
            }
        }
        t
    }

    /// The H.264 payload type for a stream whose SPS has `profile_idc` (the first one otherwise:
    /// a browser decodes any profile it has, the SDP only names one).
    pub(crate) fn h264_for(&self, profile_idc: Option<u8>) -> Option<Pt> {
        profile_idc
            .and_then(|p| self.h264.iter().find(|(_, plid)| (plid >> 16) as u8 == p))
            .or(self.h264.first())
            .map(|(pt, _)| *pt)
    }
}

/// NAL units of an Annex B access unit: (type, the unit from its header on).
fn nals(au: &[u8], hevc: bool) -> impl Iterator<Item = (u8, &[u8])> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= au.len() {
        if au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let n = starts.len();
    (0..n).filter_map(move |k| {
        let from = starts[k];
        let mut to = if k + 1 < n {
            starts[k + 1] - 3
        } else {
            au.len()
        };
        // A four-byte start code's leading zero belongs to the next one.
        if k + 1 < n && to > from && au[to - 1] == 0 {
            to -= 1;
        }
        let nal = au.get(from..to).filter(|n| !n.is_empty())?;
        let t = if hevc {
            (nal[0] >> 1) & 0x3f
        } else {
            nal[0] & 0x1f
        };
        Some((t, nal))
    })
}

/// The SPS's profile_idc in an H.264 codec header (Annex B parameter sets).
pub(crate) fn h264_profile(header: &[u8]) -> Option<u8> {
    nals(header, false)
        .find(|(t, _)| *t == 7)
        .and_then(|(_, n)| n.get(1).copied())
}

/// Whether an access unit carries its own parameter sets (SPS, or HEVC's VPS/SPS).
fn has_params(au: &[u8], hevc: bool) -> bool {
    nals(au, hevc).any(|(t, _)| if hevc { (32..=33).contains(&t) } else { t == 7 })
}

/// The video track of one peer: where it goes and what the session sent it.
/// A guest's sound as an Opus RTP track (#720): the console's Opus packets go on as they came,
/// so the browser's own jitter buffer, loss concealment and lip sync play them, not the page.
struct Audio {
    mid: Mid,
    pt: Pt,
    /// The session sends its sound here (else on the data channel).
    on: bool,
    /// RTP ticks (48 kHz) per frame, from the sound's header (kind 3): 480 for 10 ms frames.
    ticks: u64,
    /// The console's frame index (u16, wraps) as a running count, for the RTP time.
    last: Option<u16>,
    count: u64,
}

/// Opus's RTP clock.
const OPUS_HZ: u64 = 48_000;

struct Video {
    mid: Mid,
    pts: TrackPts,
    /// The payload type pictures are written with, once the session chose the track.
    pt: Option<Pt>,
    hevc: bool,
    /// The codec header (kind 0): put in front of each keyframe that lacks it, as a decoder that
    /// joins at that keyframe needs (a browser asks for a keyframe until it has the SPS).
    header: Option<Vec<u8>>,
    t0: Instant,
    /// The pictures' way onto the track ([`Catchup`]), its counts as the peer's state last had
    /// them, and when the log last said the guest fell behind.
    catchup: Catchup,
    counted: (u64, u64),
    noted: Option<Instant>,
    /// The estimate kept at the stream's rate on a clean link ([`KeepUp`]), and the padding
    /// floor set for a guest on the home network (kbit/s; 0: none).
    keep_up: KeepUp,
    floor_kbps: u32,
    /// A home-network guest on a clean link: the estimate is held at this (kbit/s; 0: not held),
    /// and the times it was put back there ([`hold_estimate`]).
    pin_kbps: u32,
    pinned: u64,
    /// …while the link has shown it carries that ([`Hold`]).
    hold: Hold,
}

/// Whether a home-network guest's estimate may still be held over the stream's rate
/// ([`hold_estimate`]). A hold let go within [`KEEP_UP_WATCH`] of being taken (loss, NACKs or a
/// rising round trip came with it) failed: the link does not carry the stream, and after
/// [`KEEP_UP_TRIES`] of those the estimate is left to the link for good.
///
/// Held for ever, a link that carries less than the stream is flooded at the held rate until it
/// loses, let go until the estimate has fallen to a few hundred kbit/s and nothing is sent, found
/// clean for that and flooded again. Measured on a home link capped at 12 Mbit/s under a
/// 15 Mbit/s stream (track_bench.rs, 90 s runs): 6-7 pictures a second reach the guest, one in a
/// hundred over 2.1-2.4 s late; given up after three, 13-30 a second (27 in the middle run of
/// five) and 0.6-1.6 s. 12.4.7 never showed it there, its driver sent 10 Mbit/s at most whatever
/// the estimate: 22-24 a second and 0.43 s, which this link still does not always match.
#[derive(Default, Debug)]
struct Hold {
    since: Option<Instant>,
    failed: u32,
}

impl Hold {
    /// The estimate may be held.
    fn open(&self) -> bool {
        self.failed < KEEP_UP_TRIES
    }

    fn taken(&mut self, now: Instant) {
        self.since = Some(now);
    }

    /// The hold was let go (the link is not clean). True when that was one too many: no more
    /// holds for this guest.
    fn let_go(&mut self, now: Instant) -> bool {
        let soon = self
            .since
            .take()
            .is_some_and(|t| now.saturating_duration_since(t) < KEEP_UP_WATCH);
        if soon && self.open() {
            self.failed += 1;
            return !self.open();
        }
        false
    }
}

/// The log says a guest fell behind at most this often (the counts carry the rest).
const BEHIND_NOTE_EVERY: Duration = Duration::from_secs(10);

/// What the driver is asked.
pub(crate) enum Cmd {
    /// A room is open: gather the outside addresses, keep the router mapping. Its kind says which
    /// session its peers' channels get.
    RoomOpen(String, Kind),
    /// A home-network join's own room (join_net/local_signal.rs): offers are taken in it, with no
    /// router mapping or STUN asked for.
    RoomOpenLocal(String, Kind),
    RoomClose(String),
    /// A guest's offer (signal `data.type == "offer"`); `signal` takes what goes back to them.
    Offer {
        room: String,
        gid: String,
        sdp: String,
        signal: mpsc::UnboundedSender<(String, serde_json::Value)>,
    },
    /// A guest's trickled candidate.
    Remote {
        room: String,
        gid: String,
        candidate: String,
    },
    /// The guest left the hub: a peer not connected yet goes (a connected one keeps playing).
    Left {
        room: String,
        gid: String,
    },
    /// A guest session's message (its length as [`Flow`] counted it).
    Up(u64, Message, usize),
    /// The session's pictures (H.265 or not) go on the peer's video track, at this stream bitrate
    /// (kbit/s).
    Track(u64, bool, u32),
    /// One of the session's video messages for the track: `[0][codec header]` or `[1][AU]`.
    Video(u64, Bytes),
    /// The session's sound goes on the peer's audio track from now on.
    AudioOn(u64),
    /// One of the session's sound messages: `[3][header]` or `[4][u16 BE index][Opus packet]`.
    Audio(u64, Bytes),
    /// A guest session ended.
    Gone(u64),
    /// The router mapping, made or lost.
    Mapped(Option<(SocketAddr, SocketAddr, &'static str)>),
}

/// The engine's certificate and its fingerprint (one per engine run).
pub(crate) fn cert() -> Option<&'static (DtlsCert, String)> {
    static C: OnceLock<Option<(DtlsCert, String)>> = OnceLock::new();
    C.get_or_init(|| {
        let c = super::crypto::new_cert()?;
        let fp = super::crypto::fingerprint_of(&c.certificate);
        Some((c, fp))
    })
    .as_ref()
}

/// The running driver.
pub(crate) struct Net {
    pub(crate) cmd: mpsc::UnboundedSender<Cmd>,
}

/// Datagrams a socket keeps for its next try, at most: a quarter second of a 15 Mbit/s track.
/// Past it the oldest is dropped (a resend is asked for, or it was a resend).
const HELD_MAX: usize = 512;
/// A socket that had no room is tried again this soon, and what it holds is dropped after this
/// long (a picture's packet that late is no use, and a socket that stays shut must not be tried
/// for ever).
const HELD_RETRY: Duration = Duration::from_millis(1);
const HELD_FOR: Duration = Duration::from_millis(500);

/// "No buffer space" as this system numbers it: the interface's queue is full. Where a UDP
/// socket has no send buffer of its own to fill (macOS and the BSDs), this is what a burst too
/// many gets, not "would block".
const ENOBUFS: i32 = if cfg!(windows) {
    10055
} else if cfg!(any(target_os = "linux", target_os = "android")) {
    105
} else {
    55
};

/// The socket has no room for a datagram now (its buffer or the interface's queue is full, or
/// the runtime has not seen a new socket writable yet), as against a send that failed.
fn no_room(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::WouldBlock || e.raw_os_error() == Some(ENOBUFS)
}

/// What became of a datagram handed to a socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sent {
    Out,
    /// No room: kept for the next try.
    Held,
    /// The send failed.
    Lost,
}

/// The datagrams a socket had no room for, in order: since when, whose, where to. (They were
/// dropped before, `try_send_to` with its result ignored, and nothing counted them.)
#[derive(Default)]
struct Held(VecDeque<(Instant, Arc<PeerInfo>, SocketAddr, Vec<u8>)>);

impl Held {
    /// A datagram of `who`'s for `to`, through `send` (the socket's non-blocking send): behind
    /// the ones still held, if any. One past [`HELD_MAX`] pushes the oldest out.
    fn send(
        &mut self,
        who: &Arc<PeerInfo>,
        to: SocketAddr,
        data: &[u8],
        now: Instant,
        send: impl FnOnce(&[u8], SocketAddr) -> std::io::Result<usize>,
    ) -> Sent {
        if self.0.is_empty() {
            match send(data, to) {
                Ok(_) => return Sent::Out,
                Err(e) if no_room(&e) => {}
                Err(_) => {
                    who.with(|s| s.udp.lost += 1);
                    return Sent::Lost;
                }
            }
        }
        who.with(|s| s.udp.waited += 1);
        if self.0.len() >= HELD_MAX {
            if let Some((_, old, ..)) = self.0.pop_front() {
                old.with(|s| s.udp.lost += 1);
            }
        }
        self.0.push_back((now, who.clone(), to, data.to_vec()));
        Sent::Held
    }

    /// The held ones again, oldest first, until the socket has no room; one held past
    /// [`HELD_FOR`] is dropped untried. True: none is left.
    fn flush(
        &mut self,
        now: Instant,
        mut send: impl FnMut(&[u8], SocketAddr) -> std::io::Result<usize>,
    ) -> bool {
        while let Some((since, who, to, data)) = self.0.front() {
            if now.saturating_duration_since(*since) > HELD_FOR {
                who.with(|s| s.udp.lost += 1);
            } else {
                match send(data, *to) {
                    Ok(_) => {}
                    Err(e) if no_room(&e) => return false,
                    Err(_) => who.with(|s| s.udp.lost += 1),
                }
            }
            self.0.pop_front();
        }
        true
    }
}

/// One of the driver's sockets: its address, and what it could not take yet.
struct Sock {
    s: Arc<UdpSocket>,
    addr: SocketAddr,
    held: Held,
}

/// The router mapping now: outside address and how it was made.
pub(crate) static NET_MAPPED: Mutex<Option<(SocketAddr, &'static str)>> = Mutex::new(None);

static NET: Mutex<Option<Arc<Net>>> = Mutex::new(None);

/// The driver, started on first use (in a Tokio runtime). None when no socket could be opened.
/// A driver whose task ended (its runtime shut down: each test has its own) is started again.
pub(crate) async fn net() -> Option<Arc<Net>> {
    let alive = |n: &Option<Arc<Net>>| n.clone().filter(|n| !n.cmd.is_closed());
    if let Some(n) = alive(&NET.lock().unwrap_or_else(|e| e.into_inner())) {
        return Some(n);
    }
    let n = start().await?;
    let mut g = NET.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(other) = alive(&g) {
        return Some(other); // another start won; this one's sockets close with its task
    }
    *g = Some(n.clone());
    Some(n)
}

/// The tests' engine for the guest page's Playwright test (a browser does not pair with a loopback
/// candidate): the real interfaces, as a running engine has them.
#[cfg(test)]
pub(crate) static TEST_REAL_IPS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The addresses the sockets go on: every IPv4 interface but loopback and link-local (the tests:
/// loopback only), the home network's first ([`by_reach`]).
pub(crate) fn local_ips() -> Vec<Ipv4Addr> {
    #[cfg(test)]
    if !TEST_REAL_IPS.load(Ordering::Relaxed) {
        return vec![Ipv4Addr::LOCALHOST];
    }
    let mut v: Vec<Ipv4Addr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(a) if !a.ip.is_loopback() && !a.ip.is_link_local() => Some(a.ip),
            _ => None,
        })
        .collect();
    v.sort_by_key(|ip| (reach_rank(IpAddr::V4(*ip)), *ip));
    v.dedup();
    v
}

/// Which network an address is on, best first for a guest: 0 the home network (RFC 1918,
/// link-local, IPv6 ULA and link-local; loopback too), 1 Tailscale (100.64.0.0/10, fd7a:115c:a1e0::/48), 2
/// anything else (a public address).
///
/// ICE ranks this engine's host candidates by the order they are added (str0m gives each its local
/// preference from a counter: is::default_local_preference), so the sockets are kept in this order
/// and their candidates added in it ([`add_host_candidates`]): a guest on the same Wi-Fi as a
/// computer that also runs Tailscale pairs with the home network's address, not the tailnet's
/// (sorted by address, 100.x came before 192.168.x).
pub(crate) fn reach_rank(ip: IpAddr) -> u8 {
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            if a.is_private() || a.is_link_local() || a.is_loopback() {
                0
            } else if o[0] == 100 && (64..128).contains(&o[1]) {
                1
            } else {
                2
            }
        }
        IpAddr::V6(a) => {
            let s = a.segments();
            if s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0 {
                1
            } else if (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80 || a.is_loopback() {
                0
            } else {
                2
            }
        }
    }
}

/// The way a connection goes, from its selected candidate pair (this engine's socket, the guest's
/// address as packets come from it): `"tailscale"` when either end is on the tailnet, `"lan"` when
/// the guest's address is on the home network, else `"internet"` (straight through both routers).
pub(crate) fn path_of(local: SocketAddr, remote: SocketAddr) -> &'static str {
    match (reach_rank(local.ip()), reach_rank(remote.ip())) {
        (1, _) | (_, 1) => "tailscale",
        (_, 0) => "lan",
        _ => "internet",
    }
}

/// This engine's host candidates on `rtc`, the home network's first (their ICE priority follows
/// the order: [`reach_rank`]).
pub(crate) fn add_host_candidates(rtc: &mut Rtc, addrs: impl IntoIterator<Item = SocketAddr>) {
    let mut v: Vec<SocketAddr> = addrs.into_iter().collect();
    v.sort_by_key(|a| reach_rank(a.ip()));
    for a in v {
        if let Ok(c) = Candidate::host(a, "udp") {
            rtc.add_local_candidate(c);
        }
    }
}

async fn start() -> Option<Arc<Net>> {
    let mut socks: Vec<Arc<UdpSocket>> = Vec::new();
    let mut port = 0u16;
    for ip in local_ips() {
        // One port number for every address when it is free (one mapping fits them all).
        let s = match UdpSocket::bind((ip, port)).await {
            Ok(s) => s,
            Err(_) => match UdpSocket::bind((ip, 0)).await {
                Ok(s) => s,
                Err(_) => continue,
            },
        };
        if port == 0 {
            port = s.local_addr().map(|a| a.port()).unwrap_or(0);
        }
        socks.push(Arc::new(s));
    }
    if socks.is_empty() {
        crate::log_at!(
            Info,
            "together",
            "join: no address for internet guests (no IPv4 network)"
        );
        return None;
    }
    let local: Vec<SocketAddr> = socks.iter().filter_map(|s| s.local_addr().ok()).collect();
    crate::log_at!(
        Debug,
        "together",
        "join: internet guests' UDP port {port} on {} address(es)",
        local.len()
    );
    let (cmd, cmd_rx) = mpsc::unbounded_channel();
    let (pkt_tx, pkt_rx) = mpsc::channel(1024);
    for (i, s) in socks.iter().enumerate() {
        let s = s.clone();
        let tx = pkt_tx.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            loop {
                match s.recv_from(&mut buf).await {
                    Ok((n, from)) => {
                        if tx.send((i, from, buf[..n].to_vec())).await.is_err() {
                            return;
                        }
                    }
                    // An ICMP "port unreachable" from an earlier send shows up here on some
                    // systems (Windows): not the socket's end.
                    Err(_) => {
                        if tx.is_closed() {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                }
            }
        });
    }
    let net = Arc::new(Net { cmd: cmd.clone() });
    let d = Driver {
        socks: socks
            .into_iter()
            .zip(local)
            .map(|(s, addr)| Sock {
                s,
                addr,
                held: Held::default(),
            })
            .collect(),
        extra: Vec::new(),
        peers: Vec::new(),
        rooms: HashMap::new(),
        local_rooms: std::collections::HashSet::new(),
        stun_tx: HashMap::new(),
        cmd: cmd.clone(),
        nat_want: None,
    };
    tokio::spawn(d.run(cmd_rx, pkt_rx));
    Some(net)
}

struct Peer {
    id: u64,
    /// Its room's kind: the channel label it may open and the session that gets it.
    kind: Kind,
    info: Arc<PeerInfo>,
    rtc: Rtc,
    /// The offer's data section, for trickled candidates.
    mid: String,
    signal: mpsc::UnboundedSender<(String, serde_json::Value)>,
    cid: Option<ChannelId>,
    to_session: Option<mpsc::Sender<Message>>,
    flow: Arc<Flow>,
    /// Data-channel messages (binary pieces already framed) not yet taken by the channel.
    out: VecDeque<(bool, Vec<u8>)>,
    out_bytes: usize,
    reasm: Reassembler,
    started: Instant,
    disconnected_at: Option<Instant>,
    closing_at: Option<Instant>,
    /// Where its packets last came from (the guest's address, for the connect limit).
    last_src: Option<SocketAddr>,
    /// The video track may be used (a Play Together room, the host's setting on).
    track_ok: bool,
    /// The video track, when the offer had one the engine can send on.
    video: Option<Video>,
    /// The audio track, when the offer had one with Opus.
    audio: Option<Audio>,
}

static NEXT_PEER: AtomicU64 = AtomicU64::new(1);

struct Driver {
    socks: Vec<Sock>,
    /// Outside candidates (STUN's, the router's), added to every new peer.
    extra: Vec<Candidate>,
    peers: Vec<Peer>,
    rooms: HashMap<String, Kind>,
    /// The rooms in `rooms` that are home-network joins' own ([`Cmd::RoomOpenLocal`]): they want
    /// no router mapping.
    local_rooms: std::collections::HashSet<String>,
    /// STUN requests in flight: transaction id → socket index.
    stun_tx: HashMap<[u8; 12], usize>,
    cmd: mpsc::UnboundedSender<Cmd>,
    /// The task keeping the router mapping, while one is wanted.
    nat_want: Option<tokio::sync::watch::Sender<bool>>,
}

fn ice_name(s: IceConnectionState) -> &'static str {
    match s {
        IceConnectionState::New => "new",
        IceConnectionState::Checking => "checking",
        IceConnectionState::Connected => "connected",
        IceConnectionState::Completed => "completed",
        IceConnectionState::Disconnected => "disconnected",
    }
}

/// The `a=mid:` of an offer's first section (the data channel's, in a data-only offer).
fn first_mid(sdp: &str) -> String {
    sdp.lines()
        .find_map(|l| l.trim().strip_prefix("a=mid:"))
        .unwrap_or("0")
        .to_string()
}

fn candidate_json(c: &Candidate, mid: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "candidate",
        "candidate": c.to_sdp_string(),
        "sdpMid": mid,
        "sdpMLineIndex": 0,
    })
}

impl Driver {
    async fn run(
        mut self,
        mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
        mut pkt_rx: mpsc::Receiver<(usize, SocketAddr, Vec<u8>)>,
    ) {
        // Turns in a row without a wait.
        let mut busy = 0u32;
        loop {
            let now = Instant::now();
            let mut next = now + Duration::from_millis(250);
            // What a socket had no room for goes first, ahead of anything new.
            for s in &mut self.socks {
                let sock = &s.s;
                s.held.flush(now, |d, to| sock.try_send_to(d, to));
            }
            for p in &mut self.peers {
                next = next.min(poll(p, &mut self.socks, &self.cmd, now));
            }
            if self.socks.iter().any(|s| !s.held.0.is_empty()) {
                next = next.min(now + HELD_RETRY);
            }
            self.housekeep(now);
            if next <= Instant::now() {
                // A peer has more for its socket now (its share of this turn is out): what came
                // meanwhile is taken without a wait. The runtime's timer is no way to wait for
                // "now": it wakes at the next millisecond, a packet at a time (see `drive`).
                busy += 1;
                if busy.is_multiple_of(YIELD_EVERY) {
                    tokio::task::yield_now().await;
                }
                for _ in 0..TURN_TAKES {
                    match pkt_rx.try_recv() {
                        Ok((i, from, data)) => self.on_packet(i, from, &data),
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => return,
                    }
                }
                for _ in 0..TURN_TAKES {
                    match cmd_rx.try_recv() {
                        Ok(c) => self.on_cmd(c).await,
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => return,
                    }
                }
            } else {
                busy = 0;
                tokio::select! {
                    pkt = pkt_rx.recv() => match pkt {
                        Some((i, from, data)) => self.on_packet(i, from, &data),
                        None => return,
                    },
                    cmd = cmd_rx.recv() => match cmd {
                        Some(c) => self.on_cmd(c).await,
                        None => return,
                    },
                    _ = tokio::time::sleep_until(next.into()) => {}
                }
            }
            let now = Instant::now();
            for p in &mut self.peers {
                if p.rtc.is_alive() {
                    if let Err(e) = p.rtc.handle_input(Input::Timeout(now)) {
                        fail(p, "error", &format!("{e}"));
                    }
                }
            }
        }
    }

    fn on_packet(&mut self, i: usize, from: SocketAddr, data: &[u8]) {
        if let Some(tx) = stun::response_tx(data) {
            if self.stun_tx.remove(&tx).is_some() {
                self.on_stun(i, data);
                return;
            }
        }
        let Some(dest) = self.socks.get(i).map(|s| s.addr) else {
            return;
        };
        let Ok(contents) = data.try_into() else {
            return;
        };
        let input = Input::Receive(
            Instant::now(),
            Receive {
                proto: Protocol::Udp,
                source: from,
                destination: dest,
                contents,
            },
        );
        if let Some(p) = self
            .peers
            .iter_mut()
            .find(|p| p.rtc.is_alive() && p.rtc.accepts(&input))
        {
            p.last_src = Some(from);
            if let Err(e) = p.rtc.handle_input(input) {
                fail(p, "error", &format!("{e}"));
            }
        }
    }

    fn on_stun(&mut self, i: usize, data: &[u8]) {
        let (Some(addr), Some(base)) = (stun::mapped(data), self.socks.get(i).map(|s| s.addr))
        else {
            return;
        };
        if addr == base || !addr.is_ipv4() {
            return; // no NAT in between: the host candidate says it already
        }
        self.add_extra(Candidate::server_reflexive(addr, base, "udp").ok(), "stun");
    }

    /// A new outside candidate: for new peers, and trickled to the ones there are.
    fn add_extra(&mut self, c: Option<Candidate>, how: &str) {
        let Some(c) = c else { return };
        if self.extra.iter().any(|e| e.addr() == c.addr()) {
            return;
        }
        crate::log_at!(
            Debug,
            "together",
            "join: internet candidate from {how}: port {}",
            c.addr().port()
        );
        for p in &mut self.peers {
            if p.closing_at.is_none() && p.rtc.add_local_candidate(c.clone()).is_some() {
                let _ = p
                    .signal
                    .send((p.info.gid.clone(), candidate_json(&c, &p.mid)));
            }
        }
        self.extra.push(c);
    }

    /// Asks the STUN servers in the settings, from every socket.
    async fn gather(&mut self) {
        let servers = crate::join::settings().stun;
        let mut addrs = Vec::new();
        for s in servers.iter().take(4) {
            if cfg!(test) && s.parse::<SocketAddr>().is_err() {
                continue; // the tests never look a name up (nor reach a real server)
            }
            if let Ok(Ok(found)) =
                tokio::time::timeout(STUN_WAIT, tokio::net::lookup_host(s.as_str())).await
            {
                addrs.extend(found.filter(|a| a.is_ipv4()).take(1));
            }
        }
        self.stun_tx.clear();
        for (i, sock) in self.socks.iter().map(|s| &s.s).enumerate() {
            for a in &addrs {
                let mut tx = [0u8; 12];
                if getrandom::fill(&mut tx).is_err() {
                    continue;
                }
                self.stun_tx.insert(tx, i);
                // Awaited, not `try_send_to`: the sockets are new when the first room opens, and a
                // socket the runtime has not yet seen writable refuses a try (WouldBlock), which
                // lost the request, and with it the STUN candidate, whenever the machine was busy.
                let _ = tokio::time::timeout(STUN_WAIT, sock.send_to(&stun::request(tx), *a)).await;
            }
        }
    }

    fn want_mapping(&mut self, want: bool) {
        let want = want && crate::join::settings().upnp;
        match (&self.nat_want, want) {
            (Some(tx), false) => {
                let _ = tx.send(false);
                self.nat_want = None;
            }
            (None, true) => {
                // The address the router sees: the first socket's (the one toward the LAN).
                let Some(local) = self.socks.first().map(|s| s.addr) else {
                    return;
                };
                let (tx, rx) = tokio::sync::watch::channel(true);
                self.nat_want = Some(tx);
                tokio::spawn(keep_mapping(local, rx, self.cmd.clone()));
            }
            _ => {}
        }
    }

    async fn on_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::RoomOpen(r, kind) => {
                self.rooms.insert(r, kind);
                self.want_mapping(true);
                self.gather().await;
            }
            Cmd::RoomOpenLocal(r, kind) => {
                self.rooms.insert(r.clone(), kind);
                self.local_rooms.insert(r);
            }
            Cmd::RoomClose(r) => {
                self.rooms.remove(&r);
                self.local_rooms.remove(&r);
                if self.rooms.len() == self.local_rooms.len() {
                    self.want_mapping(false);
                }
            }
            Cmd::Offer {
                room,
                gid,
                sdp,
                signal,
            } => self.on_offer(room, gid, sdp, signal),
            Cmd::Remote {
                room,
                gid,
                candidate,
            } => {
                let c = candidate.trim();
                let c = c.strip_prefix("a=").unwrap_or(c);
                if c.is_empty() || c.contains(".local ") {
                    return;
                }
                if let (Some(p), Ok(cand)) = (
                    self.peers.iter_mut().find(|p| {
                        p.info.room == room && p.info.gid == gid && p.closing_at.is_none()
                    }),
                    Candidate::from_sdp_string(c),
                ) {
                    if cand.addr().is_ipv4() {
                        p.rtc.add_remote_candidate(cand);
                    }
                }
            }
            Cmd::Left { room, gid } => {
                for p in self
                    .peers
                    .iter_mut()
                    .filter(|p| p.info.room == room && p.info.gid == gid && !p.rtc.is_connected())
                {
                    fail(p, "left", "the guest left before connecting");
                }
            }
            Cmd::Up(id, m, len) => {
                if let Some(p) = self.peers.iter_mut().find(|p| p.id == id) {
                    p.flow.unwritten.fetch_sub(len, Ordering::Relaxed);
                    queue(p, m);
                    flush(p);
                }
            }
            Cmd::Track(id, hevc, kbps) => {
                if let Some(p) = self.peers.iter_mut().find(|p| p.id == id) {
                    track_on(p, hevc, kbps);
                }
            }
            Cmd::Video(id, b) => {
                if let Some(p) = self.peers.iter_mut().find(|p| p.id == id) {
                    to_track(p, &b, Instant::now());
                }
            }
            Cmd::AudioOn(id) => {
                if let Some(a) = self
                    .peers
                    .iter_mut()
                    .find(|p| p.id == id)
                    .and_then(|p| p.audio.as_mut())
                {
                    a.on = true;
                }
            }
            Cmd::Audio(id, b) => {
                if let Some(p) = self.peers.iter_mut().find(|p| p.id == id) {
                    to_audio(p, &b, Instant::now());
                }
            }
            Cmd::Gone(id) => {
                if let Some(p) = self.peers.iter_mut().find(|p| p.id == id) {
                    p.to_session = None;
                    if p.closing_at.is_none() {
                        p.closing_at = Some(Instant::now());
                    }
                }
            }
            Cmd::Mapped(m) => {
                *NET_MAPPED.lock().unwrap_or_else(|e| e.into_inner()) =
                    m.map(|(ext, _, how)| (ext, how));
                if let Some((ext, base, how)) = m {
                    self.add_extra(Candidate::server_reflexive(ext, base, "udp").ok(), how);
                }
            }
        }
    }

    fn on_offer(
        &mut self,
        room: String,
        gid: String,
        sdp: String,
        signal: mpsc::UnboundedSender<(String, serde_json::Value)>,
    ) {
        let bye = |why: &str| {
            let _ = signal.send((gid.clone(), serde_json::json!({"type": "bye", "why": why})));
        };
        let Some(&kind) = self.rooms.get(&room) else {
            return;
        };
        // A new offer from the same guest replaces one that did not connect.
        for p in self.peers.iter_mut().filter(|p| {
            p.info.room == room && p.info.gid == gid && p.cid.is_none() && p.closing_at.is_none()
        }) {
            fail(p, "replaced", "a newer offer");
        }
        let live = |p: &&Peer| p.closing_at.is_none();
        if self.peers.iter().filter(live).count() >= MAX_PEERS
            || self
                .peers
                .iter()
                .filter(live)
                .filter(|p| p.info.room == room)
                .count()
                >= MAX_ROOM_PEERS
        {
            bye("full");
            return;
        }
        let Some((cert, _)) = cert() else {
            bye("offer");
            return;
        };
        let Ok(offer) = SdpOffer::from_sdp_string(&sdp) else {
            bye("offer");
            return;
        };
        let now = Instant::now();
        let mut b = Rtc::builder()
            .set_crypto_provider(super::crypto::provider())
            .set_dtls_cert(cert.clone())
            .set_stats_interval(Some(Duration::from_secs(1)))
            .set_sctp_max_buffered_amount(SCTP_BUFFER);
        game_ice(&mut b);
        // The picture as a video track for a Play Together page that offers one (a pad page's
        // watcher keeps its data channel: pad_link.rs has no track).
        let track = kind == Kind::Join && crate::join::settings().video_track;
        if track {
            b = with_track(b);
        }
        let mut rtc = b.build(now);
        add_host_candidates(&mut rtc, self.socks.iter().map(|s| s.addr));
        for c in &self.extra {
            rtc.add_local_candidate(c.clone());
        }
        let answer = match rtc.sdp_api().accept_offer(offer) {
            Ok(a) => a,
            Err(e) => {
                crate::log_at!(Debug, "together", "join: an internet offer refused: {e}");
                bye("offer");
                return;
            }
        };
        let info = PeerInfo::new(&room, &gid);
        info.with(|s| s.ice = "checking");
        let flow = Arc::new(Flow::default());
        let _ = info.flow.set(flow.clone());
        let _ = signal.send((
            gid.clone(),
            serde_json::json!({"type": "answer", "sdp": answer.to_sdp_string()}),
        ));
        crate::log_at!(Debug, "together", "join: internet guest {gid}: answered");
        self.peers.push(Peer {
            id: NEXT_PEER.fetch_add(1, Ordering::Relaxed),
            kind,
            info,
            rtc,
            mid: first_mid(&sdp),
            signal,
            cid: None,
            to_session: None,
            flow,
            out: VecDeque::new(),
            out_bytes: 0,
            reasm: Reassembler::new(crate::join::GUEST_MSG_MAX),
            started: now,
            disconnected_at: None,
            closing_at: None,
            last_src: None,
            track_ok: track,
            video: None,
            audio: None,
        });
    }

    /// Ends what timed out, drains what is closing, forgets what is gone.
    fn housekeep(&mut self, now: Instant) {
        for p in &mut self.peers {
            if p.cid.is_none()
                && p.closing_at.is_none()
                && now.duration_since(p.started) > CONNECT_WAIT
            {
                let ice = p.info.with(|s| s.ice);
                fail(p, "timeout", &format!("no connection (ICE {ice})"));
            }
            if p.disconnected_at
                .is_some_and(|t| now.duration_since(t) > DISCONNECTED_WAIT)
            {
                p.disconnected_at = None;
                fail(p, "lost", "the path to the guest was lost");
            }
            if let Some(t) = p.closing_at {
                let drained = p.out.is_empty()
                    && p.cid
                        .and_then(|c| p.rtc.channel(c).map(|mut ch| ch.buffered_amount()))
                        .unwrap_or(0)
                        == 0;
                if p.rtc.is_alive()
                    && (drained || now.duration_since(t) > CLOSE_DRAIN)
                    && p.rtc.close().is_err()
                {
                    p.rtc.disconnect();
                }
            }
        }
        self.peers.retain(|p| {
            let keep = p.rtc.is_alive();
            if !keep {
                p.flow.close();
                p.info.with(|s| {
                    s.ended.get_or_insert(now);
                });
            }
            keep
        });
    }
}

/// Ends a peer: the session (if any) hears its channel close, the panel says why.
fn fail(p: &mut Peer, why: &'static str, detail: &str) {
    let connected = p.cid.is_some();
    p.info.with(|s| {
        s.failed = (!connected && why != "replaced") || why == "lost";
        s.why.get_or_insert(why);
        s.ended.get_or_insert_with(Instant::now);
    });
    crate::log_at!(
        Debug,
        "together",
        "join: internet guest {} ended ({why}): {detail}",
        p.info.gid
    );
    p.to_session = None;
    p.flow.close();
    if p.closing_at.is_none() {
        p.closing_at = Some(Instant::now());
    }
    p.rtc.disconnect();
}

/// A session's message onto the peer's outgoing queue, as data-channel messages.
fn queue(p: &mut Peer, m: Message) {
    let push = |p: &mut Peer, binary: bool, b: Vec<u8>| {
        p.out_bytes += b.len();
        p.out.push_back((binary, b));
    };
    match m {
        Message::Text(t) => push(p, false, t.as_bytes().to_vec()),
        Message::Binary(b) => {
            for piece in chunk::split(&b) {
                push(p, true, piece);
            }
        }
        // The WebSocket ping's stand-in: the page answers it with a pong (chunk.rs).
        Message::Ping(b) if b.len() == 8 => {
            let mut v = vec![chunk::PING];
            v.extend_from_slice(&b);
            push(p, true, v);
        }
        Message::Close(_) => {
            p.closing_at.get_or_insert_with(Instant::now);
        }
        _ => {}
    }
}

/// Moves what the channel takes from the queue into it, and tells the session how full it is.
fn flush(p: &mut Peer) {
    let Some(cid) = p.cid else {
        return;
    };
    let mut buffered = 0;
    if let Some(mut ch) = p.rtc.channel(cid) {
        while let Some((binary, data)) = p.out.front() {
            match ch.write(*binary, data) {
                Ok(true) => {
                    p.out_bytes -= data.len();
                    p.out.pop_front();
                }
                Ok(false) => break,
                Err(_) => {
                    p.out.clear();
                    p.out_bytes = 0;
                    break;
                }
            }
        }
        buffered = ch.buffered_amount();
    }
    p.flow.set_queued(p.out_bytes, buffered);
}

/// Drives one peer until it has nothing more to do now: its packets out, its events handled.
/// Returns when it is to be driven next (now or before: it has more to send, and the other peers
/// get their turn first).
fn poll(
    p: &mut Peer,
    socks: &mut [Sock],
    cmd: &mpsc::UnboundedSender<Cmd>,
    now: Instant,
) -> Instant {
    let mut sent = 0;
    let next = drive(p, socks, cmd, now, &mut sent);
    if sent > 0 {
        p.info.with(|s| s.udp.sent += sent as u64);
    }
    next
}

fn drive(
    p: &mut Peer,
    socks: &mut [Sock],
    cmd: &mpsc::UnboundedSender<Cmd>,
    mut now: Instant,
    sent: &mut usize,
) -> Instant {
    let mut idle = 0;
    loop {
        // Pictures waiting for the track go as str0m's queue drains.
        if p.rtc.is_alive()
            && p.video
                .as_ref()
                .is_some_and(|v| v.catchup.waiting(now).0 > 0)
        {
            pump_track(p, now);
        }
        let (before, mut held) = (*sent, false);
        let t = loop {
            if !p.rtc.is_alive() {
                return now + Duration::from_millis(50);
            }
            let out = match p.rtc.poll_output() {
                Ok(o) => o,
                Err(e) => {
                    fail(p, "error", &format!("{e}"));
                    return now;
                }
            };
            match out {
                Output::Timeout(t) => break t,
                Output::Transmit(t) => {
                    *sent += 1;
                    if let Some(s) = socks.iter_mut().find(|s| s.addr == t.source) {
                        let sock = &s.s;
                        let did = s
                            .held
                            .send(&p.info, t.destination, &t.contents, now, |d, to| {
                                sock.try_send_to(d, to)
                            });
                        held |= did == Sent::Held;
                    }
                }
                Output::Event(e) => on_event(p, e, cmd),
            }
        };
        let at = Instant::now();
        // Its socket has no room: no more for it until the next try.
        if held {
            return t.max(at + HELD_RETRY);
        }
        if t > at {
            return t;
        }
        // A timeout already due is taken here and now (as str0m's own run loop does). Its pacer
        // hands over one packet per timeout and asks for the next at once (pacer/leaky.rs
        // `request_immediate_timeout`); waited for in the runtime's timer, each "at once" lasts
        // to the next whole millisecond (Tokio's timers), so the driver sent about 1,000
        // datagrams a second whatever the estimate: a 15 Mbit/s track needs 1,800, and half its
        // pictures waited over 400 ms and were dropped on a link with nothing wrong (#720,
        // track_bench.rs).
        idle = if *sent > before { 0 } else { idle + 1 };
        if idle >= TURN_IDLE {
            return at + Duration::from_millis(1);
        }
        if *sent >= TURN_DATAGRAMS {
            return t;
        }
        if let Err(e) = p.rtc.handle_input(Input::Timeout(at)) {
            fail(p, "error", &format!("{e}"));
            return at;
        }
        now = at;
    }
}

fn on_event(p: &mut Peer, e: Event, cmd: &mpsc::UnboundedSender<Cmd>) {
    match e {
        Event::IceConnectionStateChange(s) => {
            p.info.with(|st| st.ice = ice_name(s));
            p.disconnected_at = match s {
                IceConnectionState::Disconnected => Some(Instant::now()),
                _ => None,
            };
        }
        Event::Connected => {
            p.info.with(|s| s.connected = true);
            crate::log_at!(
                Debug,
                "together",
                "join: internet guest {}: connected in {} ms",
                p.info.gid,
                p.started.elapsed().as_millis()
            );
        }
        Event::ChannelOpen(cid, label) => {
            if label != p.kind.label() || p.cid.is_some() || p.closing_at.is_some() {
                return;
            }
            p.cid = Some(cid);
            if let Some(mut ch) = p.rtc.channel(cid) {
                ch.set_buffered_amount_low_threshold(super::HIGH_WATER / 2);
            }
            let ip = p
                .last_src
                .map(|a| a.ip())
                .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
            if !crate::join::connect_allowed(ip, Instant::now()) {
                queue(
                    p,
                    Message::Text(
                        serde_json::json!({"stage": "refused", "why": "slow_down"})
                            .to_string()
                            .into(),
                    ),
                );
                p.closing_at = Some(Instant::now());
                flush(p);
                return;
            }
            let (tx, rx) = mpsc::channel(TO_SESSION);
            p.to_session = Some(tx);
            let socket = RtcSocket {
                id: p.id,
                rx,
                up: cmd.clone(),
                flow: p.flow.clone(),
                info: p.info.clone(),
            };
            crate::log_at!(
                Debug,
                "together",
                "join: internet guest {}: data channel open",
                p.info.gid
            );
            #[cfg(test)]
            let Some(socket) = track_bench::take_session(socket) else {
                return;
            };
            match p.kind {
                Kind::Join => {
                    tokio::spawn(crate::join::guest_session(socket, ip));
                }
                Kind::Pad => {
                    tokio::spawn(crate::pad_link::rtc_session(
                        socket,
                        ip,
                        p.info.room.clone(),
                    ));
                }
                Kind::Share => {
                    tokio::spawn(crate::share::rtc_session(socket, ip, p.info.room.clone()));
                }
            }
        }
        Event::ChannelData(d) => {
            if Some(d.id) != p.cid {
                return;
            }
            let m = if d.binary {
                match p.reasm.push(&d.data) {
                    Ok(Piece::Partial) => return,
                    Ok(Piece::Message(b)) => Message::Binary(b.into()),
                    Ok(Piece::Pong(b)) => Message::Pong(b.to_vec().into()),
                    Ok(Piece::Ping(b)) => {
                        let mut v = vec![chunk::PONG];
                        v.extend_from_slice(&b);
                        p.out_bytes += v.len();
                        p.out.push_back((true, v));
                        flush(p);
                        return;
                    }
                    Err(why) => {
                        fail(p, "protocol", why);
                        return;
                    }
                }
            } else {
                // Over the LAN socket's limit closes it there; so here.
                if d.data.len() > crate::join::GUEST_MSG_MAX {
                    fail(p, "protocol", "a text message over the limit");
                    return;
                }
                match String::from_utf8(d.data) {
                    Ok(t) => Message::Text(t.into()),
                    Err(_) => {
                        fail(p, "protocol", "text that is not UTF-8");
                        return;
                    }
                }
            };
            if let Some(tx) = &p.to_session {
                if tx.try_send(m).is_err() {
                    fail(p, "flood", "the guest sent faster than the session reads");
                }
            }
        }
        Event::ChannelClose(cid) => {
            if Some(cid) == p.cid {
                p.to_session = None;
                if p.closing_at.is_none() {
                    p.closing_at = Some(Instant::now());
                }
            }
        }
        Event::ChannelBufferedAmountLow(_) => flush(p),
        // The offer's video section: the track, if the engine may send on it with a codec it has.
        Event::MediaAdded(m) => {
            if !p.track_ok || !matches!(m.direction, Direction::SendOnly | Direction::SendRecv) {
                return;
            }
            if m.kind == MediaKind::Audio {
                let opus = p.rtc.writer(m.mid).and_then(|w| {
                    w.payload_params()
                        .find(|x| x.spec().codec == Codec::Opus)
                        .map(|x| x.pt())
                });
                if let (None, Some(pt)) = (p.audio.as_ref(), opus) {
                    p.info.audio_offered();
                    p.audio = Some(Audio {
                        mid: m.mid,
                        pt,
                        on: false,
                        ticks: 960,
                        last: None,
                        count: 0,
                    });
                }
                return;
            }
            if p.video.is_some() || m.kind != MediaKind::Video {
                return;
            }
            let pts = TrackPts::of(&mut p.rtc, m.mid);
            if pts.h264.is_empty() && pts.h265.is_none() {
                return;
            }
            p.info
                .track_offered(!pts.h264.is_empty(), pts.h265.is_some());
            p.video = Some(Video {
                mid: m.mid,
                pts,
                pt: None,
                hevc: false,
                header: None,
                t0: Instant::now(),
                catchup: Catchup::default(),
                counted: (0, 0),
                noted: None,
                keep_up: KeepUp::default(),
                floor_kbps: 0,
                pin_kbps: 0,
                pinned: 0,
                hold: Hold::default(),
            });
        }
        // The guest's browser asks for a keyframe (its decoder lost the picture): the session's
        // own keyframe ask, with its gates (join.rs KeyAsk, then rp_stream_api's guest gaps).
        Event::KeyframeRequest(r) => {
            p.info.track_with(|t| match r.kind {
                KeyframeRequestKind::Pli => t.plis += 1,
                KeyframeRequestKind::Fir => t.firs += 1,
            });
            if let Some(tx) = &p.to_session {
                let _ = tx.try_send(Message::Text(r#"{"keyframe":true}"#.into()));
            }
        }
        Event::MediaEgressStats(s) => {
            // The video track's only. The audio track's come the same way, and whichever came
            // last stood in the video's place (the bench read 3,000 packets, the sound's, after
            // 30 s of a 15 Mbit/s picture): its packet and NACK counts, its loss and round trip,
            // on the host's panel and in what `KeepUp` takes for a clean link.
            if p.video.as_ref().is_none_or(|v| v.mid != s.mid) {
                return;
            }
            p.info.track_with(|t| {
                t.packets = s.packets;
                t.nacks = s.nacks;
                t.loss = s.loss.or(t.loss);
                t.rtcp_rtt_ms = s.rtt.map(|d| d.as_millis() as u32).or(t.rtcp_rtt_ms);
            });
        }
        Event::EgressBitrateEstimate(BweKind::Twcc { estimate, .. }) => {
            let mut kbps = (estimate.as_f64() / 1000.0).round().min(u32::MAX as f64) as u32;
            if let Some(v) = p.video.as_mut() {
                kbps = hold_estimate(&mut p.rtc, v, kbps);
                v.catchup.estimate(kbps);
            }
            let now = Instant::now();
            let check = p.info.track_with(|t| {
                t.estimate(kbps, now);
                // The connection check done: said once, for a bug report.
                if t.checked || t.measuring(now) {
                    return None;
                }
                t.checked = true;
                Some((t.stream_kbps, t.rtcp_rtt_ms, t.loss))
            });
            if let Some(Some((stream, rtt, loss))) = check {
                crate::log_at!(
                    Info,
                    "together",
                    "join: guest {} connection check: about {kbps} kbit/s (stream {}), rtt {} ms, loss {}",
                    p.info.gid,
                    stream.map_or("?".into(), |s| s.to_string()),
                    rtt.map_or("?".into(), |r| r.to_string()),
                    loss.map_or("?".into(), |l| format!("{:.1}%", l * 100.0)),
                );
            }
        }
        Event::PeerStats(s) => {
            // The pair ICE sends on now (this engine's socket, the guest's address): the way the
            // connection goes, for the host's panel.
            if let Some(c) = s.selected_candidate_pair.as_ref() {
                let pair = (c.local.addr, c.remote.addr);
                p.info.with(|st| st.pair = Some(pair));
            }
            if let Some(rtt) = s
                .selected_candidate_pair
                .as_ref()
                .and_then(|c| c.current_round_trip_time)
                .or(s.rtt)
            {
                p.info
                    .with(|st| st.rtt_ms = Some((rtt.as_millis() as u32).max(1)));
            }
            p.info.sent(s.peer_bytes_tx, s.timestamp);
            keep_up(p);
            flush(p);
            p.flow.second();
        }
        _ => {}
    }
}

/// Once a second: the estimate back at the stream's rate when it fell on a clean link
/// ([`KeepUp`]), said in the log at most every [`BEHIND_NOTE_EVERY`].
fn keep_up(p: &mut Peer) {
    let now = Instant::now();
    let rtt = p.info.rtt_ms();
    let Some((est, stream, loss, nacks)) = p.info.track_with(|t| {
        (
            t.on.then_some(t.bwe_kbps).flatten(),
            t.stream_kbps,
            t.loss,
            t.nacks,
        )
    }) else {
        return;
    };
    let lan = p.info.net_path() == Some("lan");
    let Some(v) = p.video.as_mut() else { return };
    // A guest on the home network: str0m pads up to the stream's rate when the picture is light,
    // so the estimate sees the link carry the whole rate and never falls to a still menu's
    // trickle. Free on a home network; an internet guest's data is not padded.
    let floor = match stream {
        Some(k) if lan && est.is_some() => k,
        _ => 0,
    };
    if floor != v.floor_kbps {
        v.floor_kbps = floor;
        if let Some(k) = stream {
            set_rates(&mut p.rtc, floor, k);
        }
        crate::log_at!(
            Info,
            "together",
            "join: guest {} on the home network: the track is padded up to {floor} kbit/s",
            p.info.gid
        );
    }
    let raise = v.keep_up.second(est, stream, loss, nacks, rtt, now);
    // On a clean home network the estimate is held over the stream's rate between these looks
    // too (hold_estimate): str0m paces at its estimate, which a home network's clean link was
    // seen to cap at 6-9 Mbit/s for a 15 Mbit/s stream even padded (#720, 12.4.6), dropping
    // pictures whenever the game moved.
    // Not on a link that has shown it does not carry the stream ([`Hold`]).
    let pin = match stream {
        Some(k) if lan && v.hold.open() && v.keep_up.clean_now() => {
            (k as f64 * BWE_HEADROOM) as u32
        }
        _ => 0,
    };
    if pin != v.pin_kbps {
        if pin > 0 {
            v.hold.taken(now);
        } else if v.hold.let_go(now) {
            crate::log_at!(
                Info,
                "together",
                "join: guest {} estimate left to the link from here: {} holds each ended in loss or delay within {} s (the home network does not carry the stream to this guest)",
                p.info.gid,
                KEEP_UP_TRIES,
                KEEP_UP_WATCH.as_secs(),
            );
        }
        let was = v.pin_kbps;
        crate::log_at!(
            Info,
            "together",
            "join: guest {} estimate {} {} kbit/s (home network, clean link: {}; put back {} times so far)",
            p.info.gid,
            if pin > 0 { "held at" } else { "no longer held at" },
            if pin > 0 { pin } else { was },
            if pin > 0 { "yes" } else { "no" },
            v.pinned,
        );
        v.pin_kbps = pin;
    }
    let Some(to) = raise else {
        return;
    };
    v.catchup.aim(&mut p.rtc, to);
    set_rates(&mut p.rtc, v.floor_kbps, to);
    p.info.track_with(|t| t.estimate(to, now));
    let raised = v.keep_up.raised;
    if raised == 1 || raised % 10 == 0 {
        crate::log_at!(
            Info,
            "together",
            "join: guest {} estimate {} kbit/s on a clean link (rtt {} ms, loss {}): raised to the stream's {to} kbit/s ({raised} times)",
            p.info.gid,
            est.map_or("?".into(), |e| e.to_string()),
            rtt.map_or("?".into(), |r| r.to_string()),
            loss.map_or("?".into(), |l| format!("{:.1}%", l * 100.0)),
        );
    }
}

/// A new estimate on a held link ([`Video::pin_kbps`]): one under the stream's rate is put back to
/// the held rate at once, so str0m's pacer never sends under the stream. Returns the estimate
/// that stands.
fn hold_estimate(rtc: &mut Rtc, v: &mut Video, kbps: u32) -> u32 {
    if v.pin_kbps == 0 || (kbps as f64) * BWE_HEADROOM >= v.pin_kbps as f64 {
        return kbps;
    }
    rtc.bwe().reset(Bitrate::kbps(v.pin_kbps as u64));
    v.pinned += 1;
    v.pin_kbps
}

/// str0m's minimum (padded up to, [`Video::floor_kbps`]) and desired rate for a stream of `kbps`.
fn set_rates(rtc: &mut Rtc, floor_kbps: u32, kbps: u32) {
    rtc.bwe().set_bitrate(
        Bitrate::kbps(floor_kbps as u64),
        Bitrate::kbps((kbps as f64 * BWE_HEADROOM) as u64),
    );
}

/// The session chose the track: the payload type for its codec, and the estimate aimed at its
/// bitrate.
fn track_on(p: &mut Peer, hevc: bool, kbps: u32) {
    let lan = p.info.net_path() == Some("lan");
    let Some(v) = p.video.as_mut() else { return };
    v.hevc = hevc;
    let h264 = v.header.as_deref().and_then(h264_profile);
    v.pt = if hevc {
        v.pts.h265
    } else {
        v.pts.h264_for(h264)
    };
    if kbps > 0 {
        v.catchup.aim(&mut p.rtc, kbps);
        // A home-network guest starts with the estimate held (keep_up lets go if the first
        // seconds are not clean): the first pictures are not held to an estimate still settling.
        if lan {
            if v.hold.open() {
                v.pin_kbps = (kbps as f64 * BWE_HEADROOM) as u32;
                v.hold.taken(Instant::now());
            }
            set_rates(&mut p.rtc, kbps, kbps);
            v.floor_kbps = kbps;
        }
    }
}

/// One of the session's video messages for the track: the codec header is kept, a picture joins
/// the track's queue ([`Catchup`]), which writes it to str0m (it packetizes it, paces it out at
/// its estimate and keeps it for resends) once str0m's queue is short.
fn to_track(p: &mut Peer, b: &[u8], now: Instant) {
    let Some(v) = p.video.as_mut() else { return };
    let (Some(&kind), Some(body)) = (b.first(), b.get(1..)) else {
        return;
    };
    match kind {
        0 => {
            if !v.hevc {
                v.pt = v.pts.h264_for(h264_profile(body)).or(v.pt);
            }
            v.header = Some(body.to_vec());
        }
        1 => {
            if v.pt.is_none() {
                return;
            }
            let us = now.saturating_duration_since(v.t0).as_micros() as u64;
            v.catchup.push(TrackPic {
                key: crate::rp_stream_api::is_keyframe(body, v.hevc),
                at: now,
                rtp: MediaTime::new(us * 90 / 1000, Frequency::NINETY_KHZ),
                data: track_au(body, v.hevc, v.header.as_deref()),
            });
            pump_track(p, now);
        }
        _ => {}
    }
}

/// The track's queue into str0m's as it takes them ([`Catchup::pump`]): the counts, the log line
/// when the guest fell behind, and the keyframe ask.
fn pump_track(p: &mut Peer, now: Instant) {
    let Some(v) = p.video.as_mut() else { return };
    let Some(pt) = v.pt else { return };
    let c = v.catchup.pump(&mut p.rtc, v.mid, pt, now);
    let counts = (v.catchup.dropped, v.catchup.jumps);
    if c.written > 0 || counts != v.counted {
        v.counted = counts;
        let (waited, peak) = (v.catchup.waited_max, v.catchup.queue_peak);
        let drain = v.catchup.drain_kbps();
        p.info.track_with(|t| {
            t.frames += c.written;
            (t.dropped, t.jumps) = counts;
            t.wait_max_ms = waited.as_millis() as u32;
            t.queue_peak = peak;
            t.drain_kbps = drain;
        });
    }
    if c.fell
        && v.noted
            .is_none_or(|t| now.saturating_duration_since(t) >= BEHIND_NOTE_EVERY)
    {
        v.noted = Some(now);
        let (bwe, stream) = p
            .info
            .track_with(|t| (t.bwe_kbps, t.stream_kbps))
            .unwrap_or_default();
        let (dropped, jumps) = counts;
        crate::log_at!(
            Info,
            "together",
            "join: guest {} fell behind on the video track (a picture waited over {} ms; estimate {} kbit/s, stream {}): skipped to the next keyframe ({jumps} times, {dropped} pictures so far)",
            p.info.gid,
            TRACK_LAG_MAX.as_millis(),
            bwe.map_or("?".into(), |k| k.to_string()),
            stream.map_or("?".into(), |s| s.to_string()),
        );
    }
    // The session's own keyframe ask, with its gates (as a PLI from the browser).
    if c.ask {
        if let Some(tx) = &p.to_session {
            let _ = tx.try_send(Message::Text(r#"{"keyframe":true}"#.into()));
        }
    }
}

/// One of the session's sound messages onto the audio track: the header sets the frame length,
/// a frame goes out at its own index's time (a lost one leaves its gap, which the browser
/// conceals).
fn to_audio(p: &mut Peer, b: &[u8], now: Instant) {
    let Some(a) = p.audio.as_mut().filter(|a| a.on) else {
        return;
    };
    match b.first() {
        Some(3) => {
            if let Some(t) = audio_ticks(&b[1..]) {
                a.ticks = t;
            }
        }
        Some(4) if b.len() > 3 => {
            let idx = u16::from_be_bytes([b[1], b[2]]);
            a.count = match a.last {
                None => 0,
                // Forward by the wrapped difference (a late one far behind starts over there).
                Some(l) => a.count.wrapping_add(idx.wrapping_sub(l) as u64),
            };
            a.last = Some(idx);
            let t = MediaTime::new(a.count.wrapping_mul(a.ticks), Frequency::FORTY_EIGHT_KHZ);
            let (mid, pt) = (a.mid, a.pt);
            if let Some(w) = p.rtc.writer(mid) {
                let _ = w.write(pt, now, t, b[3..].to_vec());
            }
        }
        _ => {}
    }
}

/// RTP ticks (48 kHz) per frame from the sound's header: `[channels][?][u32 rate][u32 samples
/// per frame]` (big-endian, pad/watchPlayer.ts reads it the same way).
pub(crate) fn audio_ticks(h: &[u8]) -> Option<u64> {
    let rate = u32::from_be_bytes(h.get(2..6)?.try_into().ok()?) as u64;
    let spf = u32::from_be_bytes(h.get(6..10)?.try_into().ok()?) as u64;
    (rate > 0 && spf > 0).then(|| spf * OPUS_HZ / rate)
}

/// An access unit as the track sends it: a keyframe without its parameter sets gets the codec
/// header in front.
pub(crate) fn track_au(au: &[u8], hevc: bool, header: Option<&[u8]>) -> Vec<u8> {
    match header {
        Some(h) if crate::rp_stream_api::is_keyframe(au, hevc) && !has_params(au, hevc) => {
            [h, au].concat()
        }
        _ => au.to_vec(),
    }
}

/// Keeps the router mapping for `local` while `want` says so: made within the budget, renewed at
/// half its lease, removed at the end.
async fn keep_mapping(
    local: SocketAddr,
    mut want: tokio::sync::watch::Receiver<bool>,
    cmd: mpsc::UnboundedSender<Cmd>,
) {
    let search = nat_search();
    let Some(search) = search else { return };
    let mut m = nat::map(local, &search).await;
    if let Some(mm) = &m {
        let _ = cmd.send(Cmd::Mapped(Some((mm.external, mm.local, mm.how()))));
    }
    loop {
        tokio::select! {
            r = want.changed() => {
                if r.is_err() || !*want.borrow() {
                    break;
                }
            }
            _ = tokio::time::sleep(nat::LEASE / 2) => {
                match &mut m {
                    Some(mm) => {
                        if !nat::renew(mm).await {
                            m = None;
                            let _ = cmd.send(Cmd::Mapped(None));
                        }
                    }
                    None => {
                        m = nat::map(local, &search).await;
                        if let Some(mm) = &m {
                            let _ = cmd.send(Cmd::Mapped(Some((mm.external, mm.local, mm.how()))));
                        }
                    }
                }
            }
        }
    }
    if let Some(mm) = m {
        nat::remove(mm).await;
    }
    let _ = cmd.send(Cmd::Mapped(None));
}

/// Where to look for the router: the LAN's SSDP and NAT-PMP, or (tests) the fake router a test
/// names, never the real one.
fn nat_search() -> Option<nat::Search> {
    #[cfg(test)]
    {
        TEST_NAT.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    #[cfg(not(test))]
    {
        Some(nat::Search::default())
    }
}

#[cfg(test)]
pub(crate) static TEST_NAT: Mutex<Option<nat::Search>> = Mutex::new(None);

#[cfg(test)]
#[path = "track_bench.rs"]
mod track_bench;

#[cfg(test)]
mod track_tests {
    use super::*;

    const HEADER: &[u8] = &[0, 0, 0, 1, 0x67, 0x64, 0, 0x2a, 0xac, 0, 0, 1, 0x68, 0xee];

    #[test]
    fn a_keyframe_gets_the_codec_header_in_front_unless_it_has_its_own() {
        assert_eq!(h264_profile(HEADER), Some(0x64));
        let idr = [0, 0, 1, 0x65, 0x88, 0x11];
        assert_eq!(track_au(&idr, false, Some(HEADER)), [HEADER, &idr].concat());
        // Its own SPS, a picture that is not a keyframe, or no header yet: as it came.
        let with_sps = [HEADER, &idr].concat();
        assert_eq!(track_au(&with_sps, false, Some(HEADER)), with_sps);
        let p = [0, 0, 0, 1, 0x41, 0x9a];
        assert_eq!(track_au(&p, false, Some(HEADER)), p);
        assert_eq!(track_au(&idr, false, None), idr);
        // H.265: an IDR (type 19) without a VPS gets the header too.
        let hevc_idr = [0, 0, 1, 19 << 1, 1, 0x22];
        let hevc_hdr = [0, 0, 1, 32 << 1, 1, 0x0c];
        assert_eq!(
            track_au(&hevc_idr, true, Some(&hevc_hdr)),
            [&hevc_hdr[..], &hevc_idr].concat()
        );
    }

    #[test]
    fn a_picture_that_waited_too_long_is_dropped_up_to_the_next_keyframe() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let pic = |key: bool, at: Instant| TrackPic {
            key,
            at,
            rtp: MediaTime::new(0, Frequency::NINETY_KHZ),
            data: vec![0; if key { 180_000 } else { 30_000 }],
        };
        let mut c = Catchup::default();
        c.estimate(10_000); // 11 Mbit/s paced: 1,375 bytes a ms
                            // str0m's queue short: the picture goes at once.
        c.push(pic(false, ms(0)));
        assert!(c.next(0, Duration::ZERO, ms(0)).is_some());
        // str0m's queue long (73 ms to send): the next ones wait their turn…
        c.push(pic(false, ms(16)));
        c.push(pic(false, ms(33)));
        assert!(c.next(100_000, Duration::ZERO, ms(33)).is_none());
        // …or the oldest packet's wait says so, whatever the estimate.
        assert!(c.next(1_000, Duration::from_millis(50), ms(40)).is_none());
        assert_eq!(c.waiting(ms(216)), (2, Duration::from_millis(200)));
        // Its turn after TRACK_LAG_MAX: dropped, with the rest; the guest waits for a keyframe.
        assert!(c.next(0, Duration::ZERO, ms(500)).is_none());
        assert!(c.fell && c.behind());
        c.push(pic(false, ms(516)));
        assert_eq!(
            c.waiting(ms(516)).0,
            0,
            "dropped while waiting for a keyframe"
        );
        // The keyframe: the guest is back, at a fresh picture.
        c.push(pic(true, ms(533)));
        assert!(!c.behind());
        assert!(c.next(0, Duration::ZERO, ms(533)).is_some_and(|p| p.key));
        assert_eq!((c.jumps, c.dropped), (1, 3));
        // A keyframe takes the place of pictures still waiting (no wait for them to age).
        c.push(pic(false, ms(550)));
        c.push(pic(false, ms(566)));
        c.push(pic(true, ms(583)));
        assert_eq!(c.waiting(ms(583)).0, 1);
        assert_eq!((c.jumps, c.dropped), (2, 5));
        // A keyframe that waited past TRACK_LAG_MAX still goes: it is where the guest catches up.
        assert!(c.next(0, Duration::ZERO, ms(1500)).is_some_and(|p| p.key));
        // str0m stalled: a picture past TRACK_LAG_MAX is dropped as the next one comes, not kept.
        c.push(pic(true, ms(2000)));
        for i in 1..=26 {
            c.push(pic(false, ms(2000 + i * 16)));
        }
        assert_eq!(c.waiting(ms(2416)).0, 27, "the keyframe and 26 pictures");
        assert!(!c.behind());
        c.push(pic(false, ms(2432)));
        assert!(c.behind());
        assert_eq!(c.waiting(ms(2432)).0, 1, "the keyframe, still the way back");
    }

    #[test]
    fn the_queue_is_reckoned_at_the_rate_it_drains_at() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut c = Catchup::default();
        c.estimate(10_000);
        // Before it was seen to drain: at the estimate (1,375 bytes a ms).
        assert_eq!(c.drain(137_500), Duration::from_millis(100));
        // str0m looked: 100 kB queued; 30 kB written since; 10 ms later it holds 30 kB: 100 kB
        // left in 10 ms.
        assert_eq!(
            c.queue((100_000, Duration::ZERO, Some(ms(0)))),
            (100_000, Duration::ZERO)
        );
        c.since += 30_000;
        assert_eq!(
            c.queue((100_000, Duration::ZERO, Some(ms(0)))).0,
            130_000,
            "the same look"
        );
        c.queue((30_000, Duration::ZERO, Some(ms(10))));
        assert_eq!(c.drain(100_000), Duration::from_millis(10));
        // A queue that ran dry tells nothing of the rate.
        c.queue((0, Duration::ZERO, Some(ms(500))));
        assert_eq!(c.drain(100_000), Duration::from_millis(10));
    }

    #[test]
    fn a_hold_that_keeps_ending_in_loss_is_given_up() {
        let t0 = Instant::now();
        let s = |n| t0 + Duration::from_secs(n);
        let mut h = Hold::default();
        assert!(h.open());
        // Held for a minute, then a blip: the link carried the stream, nothing is counted.
        h.taken(s(0));
        assert!(!h.let_go(s(60)));
        // Let go within the watch, three times: this link does not carry it.
        h.taken(s(70));
        assert!(!h.let_go(s(72)));
        h.taken(s(80));
        assert!(!h.let_go(s(83)));
        assert!(h.open());
        h.taken(s(90));
        assert!(h.let_go(s(91)), "the third: given up, and said once");
        assert!(!h.open());
        assert!(!h.let_go(s(92)));
    }

    #[test]
    fn a_datagram_the_socket_has_no_room_for_is_kept_and_goes_in_order() {
        use super::super::UdpSt;
        let who = PeerInfo::new("held-test", "g");
        let t0 = Instant::now();
        let to: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let full = || -> std::io::Result<usize> { Err(std::io::ErrorKind::WouldBlock.into()) };
        let mut h = Held::default();
        let mut out: Vec<Vec<u8>> = Vec::new();
        // Room: straight out.
        let did = h.send(&who, to, b"a", t0, |d, _| {
            out.push(d.to_vec());
            Ok(d.len())
        });
        assert_eq!(did, Sent::Out);
        // No room: kept, and the next one goes behind it without a try of its own.
        assert_eq!(h.send(&who, to, b"b", t0, |_, _| full()), Sent::Held);
        let did = h.send(&who, to, b"c", t0, |_, _| panic!("tried ahead of one held"));
        assert_eq!(did, Sent::Held);
        // Still none (the interface's queue is full, as macOS says it): they stay.
        assert!(!h.flush(t0, |_, _| Err(std::io::Error::from_raw_os_error(ENOBUFS))));
        // Room again: out in the order they came.
        let ms = Duration::from_millis(1);
        assert!(h.flush(t0 + ms, |d, _| {
            out.push(d.to_vec());
            Ok(d.len())
        }));
        assert_eq!(out, [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
        let waited = UdpSt {
            waited: 2,
            ..UdpSt::default()
        };
        assert_eq!(who.udp(), waited, "two waited, none lost");
        // A send that fails is lost, and counted.
        let did = h.send(&who, to, b"d", t0, |_, _| {
            Err(std::io::ErrorKind::PermissionDenied.into())
        });
        assert_eq!(did, Sent::Lost);
        assert_eq!(who.udp().lost, 1);
        // More than it keeps: the oldest go. Held too long: dropped without a try.
        for i in 0..HELD_MAX + 3 {
            h.send(&who, to, &[i as u8], t0, |_, _| full());
        }
        assert_eq!(h.0.len(), HELD_MAX);
        assert_eq!(who.udp().lost, 4);
        assert!(h.flush(t0 + HELD_FOR + ms, |_, _| panic!("tried one held too long")));
        assert_eq!(who.udp().lost, 4 + HELD_MAX as u64);
    }

    #[test]
    fn the_payload_type_follows_the_streams_profile() {
        let t = TrackPts {
            h264: vec![(Pt::from(108), 0x42e01f), (Pt::from(114), 0x64001f)],
            h265: None,
        };
        assert_eq!(t.h264_for(Some(0x64)), Some(Pt::from(114)));
        assert_eq!(
            t.h264_for(Some(0x4d)),
            Some(Pt::from(108)),
            "else the first"
        );
        assert_eq!(t.h264_for(None), Some(Pt::from(108)));
        assert_eq!(TrackPts::default().h264_for(Some(0x64)), None);
    }

    #[test]
    fn the_host_is_told_only_when_the_estimate_stays_under_the_streams_rate() {
        let t0 = Instant::now();
        let mut t = super::super::TrackSt {
            on: true,
            stream_kbps: Some(15_000),
            ..Default::default()
        };
        t.estimate(14_000, t0); // a few percent under: it carries it
        assert!(!t.slow(t0 + Duration::from_secs(9)));
        t.estimate(9_000, t0);
        assert!(!t.slow(t0 + Duration::from_secs(4)));
        t.estimate(8_500, t0 + Duration::from_secs(3));
        assert!(t.slow(t0 + super::super::SLOW_FOR));
        t.estimate(16_000, t0 + Duration::from_secs(6));
        assert!(!t.slow(t0 + Duration::from_secs(20)));
        t.on = false;
        t.estimate(1_000, t0);
        assert!(!t.slow(t0 + Duration::from_secs(20)), "not on the track");
    }

    #[test]
    fn the_sounds_rtp_time_steps_by_its_frame_length_at_48_khz() {
        // 2 channels, 16 bits, 48 kHz, 480 samples a frame (10 ms): 480 ticks.
        assert_eq!(
            super::audio_ticks(&[2, 16, 0, 0, 0xbb, 0x80, 0, 0, 0x01, 0xe0]),
            Some(480)
        );
        // 24 kHz with 240 samples is still 10 ms: 480 ticks of Opus's 48 kHz clock.
        assert_eq!(
            super::audio_ticks(&[2, 16, 0, 0, 0x5d, 0xc0, 0, 0, 0, 0xf0]),
            Some(480)
        );
        assert_eq!(
            super::audio_ticks(&[2, 16, 0, 0, 0xbb, 0x80]),
            None,
            "short"
        );
        assert_eq!(
            super::audio_ticks(&[2, 16, 0, 0, 0, 0, 0, 0, 1, 0xe0]),
            None
        );
    }

    #[test]
    fn the_connection_check_runs_at_the_start_and_counts_slow_seconds_with_hysteresis() {
        let t0 = Instant::now();
        let mut t = super::super::TrackSt {
            on: true,
            stream_kbps: Some(15_000),
            on_at: Some(t0),
            ..Default::default()
        };
        assert!(t.measuring(t0 + Duration::from_secs(5)), "no estimate yet");
        t.estimate(30_000, t0 + Duration::from_secs(1));
        assert!(t.measuring(t0 + Duration::from_secs(1)));
        assert!(!t.measuring(t0 + super::super::CHECK_FOR));
        // 20% to spare over what it carries.
        assert_eq!(t.fit_kbps(), Some(25_000));
        // Under the line (90% of the rate): counted from then…
        t.estimate(9_000, t0 + Duration::from_secs(2));
        assert_eq!(t.under_s(t0 + Duration::from_secs(12)), 10);
        // …through an estimate between the line and the rate…
        t.estimate(14_000, t0 + Duration::from_secs(5));
        assert_eq!(t.under_s(t0 + Duration::from_secs(12)), 10);
        // …until it is back up to the rate.
        t.estimate(15_000, t0 + Duration::from_secs(6));
        assert_eq!(t.under_s(t0 + Duration::from_secs(12)), 0);
        assert_eq!(t.fit_kbps(), Some(12_500));
    }

    /// #720: on a wired home network the estimate fell to 1.5 Mbit/s over a still menu with no
    /// loss at all. After three clean seconds it is raised back to the stream's rate, and again
    /// whenever it falls while the link stays clean.
    #[test]
    fn keep_up_raises_a_fallen_estimate_on_a_clean_link() {
        let t0 = Instant::now();
        let mut k = KeepUp::default();
        let s = |k: &mut KeepUp, i: u64, est: u32| {
            k.second(
                Some(est),
                Some(10_000),
                Some(0.0),
                0,
                Some(2),
                t0 + Duration::from_secs(i),
            )
        };
        assert_eq!(s(&mut k, 0, 9_500), None, "near the rate: left alone");
        assert_eq!(s(&mut k, 1, 1_500), None, "one clean second is not enough");
        assert_eq!(s(&mut k, 2, 1_500), Some(10_000));
        assert_eq!(s(&mut k, 3, 1_400), None, "not again at once");
        assert_eq!(s(&mut k, 8, 1_400), Some(10_000), "it stayed clean: again");
        assert_eq!(k.raised, 2);
    }

    /// A link that is really slow loses packets once the estimate is raised: after three such
    /// raises its estimate is left alone, and a lossy or delayed link is never raised.
    #[test]
    fn keep_up_leaves_a_really_slow_link_alone() {
        let t0 = Instant::now();
        let mut k = KeepUp::default();
        let mut nacks = 0;
        let mut raised = 0;
        for i in 0..120u64 {
            let r = k.second(
                Some(1_000),
                Some(15_000),
                Some(0.0),
                nacks,
                Some(30),
                t0 + Duration::from_secs(i),
            );
            if r.is_some() {
                raised += 1;
                nacks += 40; // the raise floods it
            }
        }
        assert_eq!(raised, KEEP_UP_TRIES);

        let mut lossy = KeepUp::default();
        let mut rising = KeepUp::default();
        for i in 0..30u64 {
            let at = t0 + Duration::from_secs(i);
            assert_eq!(
                lossy.second(Some(1_000), Some(15_000), Some(0.05), 0, Some(30), at),
                None
            );
            let rtt = if i == 0 { 30 } else { 120 };
            assert_eq!(
                rising.second(Some(1_000), Some(15_000), Some(0.0), 0, Some(rtt), at),
                None
            );
        }
    }
}
