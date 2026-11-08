//! A measured internet guest: the host's engine and a guest, two str0m peers, joined by a
//! simulated internet link (delay, jitter, loss, a bandwidth cap with its queue), on a virtual
//! clock. Deterministic (seeded), and fast: 20 s of 1080p60 takes a few seconds.
//!
//! The host side is the engine's own pipeline for an internet guest, step for step: the console's
//! frames (60 a second, sizes like a Remote Play stream at its bitrate, a keyframe only when asked)
//! go through the player's shallow picture queue (rp_stream_api `add_player`, skip to the next
//! keyframe when it is full), the guest session's sends (held while [`super::HIGH_WATER`] bytes
//! wait), the driver's queue and the data channel ([`super::rtc::SCTP_BUFFER`]), cut into pieces
//! as [`super::chunk`] does. Keyframe asks go through the same gates (rp_stream_api `lag_ask`, the
//! stream thread's 500 ms gate). The guest side is the page's (pad/watchPlayer.ts `WatchPlayer`):
//! it waits for a keyframe, drops to the next one when its decode queue is past the backlog limit,
//! asks for one at most once a second, and draws each picture as it is decoded.
//!
//! What it measures, on the guest's side: the pictures' arrival jitter, stalls (no new picture for
//! over 100 ms), pictures skipped (by the host, by the guest), the frame rate shown and the
//! latency from the console to the screen.
//!
//! `cargo test -p ps5upload-engine --release link_sim -- --ignored --nocapture` prints the table.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use str0m::bwe::{Bitrate, BweKind};
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::channel::ChannelId;
use str0m::format::Codec;
use str0m::media::{Direction, Frequency, KeyframeRequestKind, MediaKind, MediaTime, Mid, Pt};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};

use super::chunk::{self, Piece, Reassembler};

/// The link between host and guest, both ways alike.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LinkCfg {
    pub rtt_ms: u32,
    /// Extra one-way delay, uniform in 0..jitter (packets stay in order, as on Wi-Fi).
    pub jitter_ms: u32,
    /// Packets lost at random, each way.
    pub loss: f64,
    /// The narrowest hop (the host's upload, say), kbit/s; 0: none.
    pub cap_kbps: u32,
    /// Its queue, in ms at the cap; a packet that finds it full is dropped.
    pub queue_ms: u32,
}

/// The console's stream.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StreamCfg {
    pub kbps: u32,
    pub fps: u32,
    /// A keyframe's size over an average picture's.
    pub key_ratio: f64,
}

impl StreamCfg {
    /// The Play screen's default: 1080p60 at 15 Mbit/s (rp_stream_api `begin`).
    pub(crate) const DEFAULT: StreamCfg = StreamCfg {
        kbps: 15_000,
        fps: 60,
        key_ratio: 6.0,
    };
}

/// The engine's and page's choices this measures.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Policy {
    /// Pictures a player's queue holds before it skips to the next keyframe.
    pub player_queue: usize,
    /// Bytes waiting on the channel past which the session stops sending.
    pub high_water: usize,
    pub sctp_buffer: usize,
    /// rp_stream_api: keyframe asks for guests, at least this far apart (all together, and each).
    pub key_gap: Duration,
    /// …and for a guest that keeps falling behind.
    pub lagging_key_gap: Duration,
    pub lag_skips: u32,
    pub lag_forgiven: Duration,
    /// The stream thread's IDR gate (500 ms). `idr_ask_kept`: an ask inside it waits for the gate
    /// (false: it is lost, as before 12.0.1).
    pub idr_gate: Duration,
    pub idr_ask_kept: bool,
    /// The page's decode-queue limit before it drops to the next keyframe (watchPlayer
    /// BACKLOG_LIMIT.player).
    pub guest_backlog: usize,
    /// The picture as an RTP video track (str0m's media API: NACK and retransmission, a PLI asks
    /// for a keyframe), the data channel keeping sound, status and pads; false: the picture on the
    /// data channel with everything else, as the engine sends it.
    pub rtp_video: bool,
    /// The track as the engine sets it up (rtc.rs `with_track`: H.264/H.265 only, its send
    /// buffer, the bandwidth estimate aimed at the stream's bitrate, the codec header in front of a
    /// keyframe); false: str0m's defaults (the prototype in #762's table).
    pub rtp_engine: bool,
    /// …with its bandwidth estimate (and so str0m's pacer); false: measured without.
    pub rtp_bwe: bool,
}

impl Policy {
    /// The engine and page as they were (12.0.0).
    pub(crate) fn before() -> Policy {
        Policy {
            player_queue: 6,
            high_water: 256 * 1024,
            sctp_buffer: 1024 * 1024,
            key_gap: Duration::from_secs(2),
            lagging_key_gap: Duration::from_secs(5),
            lag_skips: 3,
            lag_forgiven: Duration::from_secs(10),
            idr_gate: Duration::from_millis(500),
            idr_ask_kept: false,
            guest_backlog: 4,
            rtp_video: false,
            rtp_engine: false,
            rtp_bwe: true,
        }
    }

    /// The engine and page as they are now.
    pub(crate) fn now() -> Policy {
        Policy {
            player_queue: crate::join::PLAYER_QUEUE,
            high_water: super::HIGH_WATER,
            sctp_buffer: super::rtc::SCTP_BUFFER,
            key_gap: crate::rp_stream_api::GUEST_KEY_GAP,
            lagging_key_gap: crate::rp_stream_api::INTERNET_LAGGING_KEY_GAP,
            lag_skips: crate::rp_stream_api::LAG_SKIPS,
            lag_forgiven: crate::rp_stream_api::LAG_FORGIVEN,
            idr_gate: Duration::from_millis(500),
            idr_ask_kept: true,
            guest_backlog: 4,
            rtp_video: false,
            rtp_engine: false,
            rtp_bwe: true,
        }
    }
}

/// What the guest saw.
#[derive(Clone, Debug, Default)]
pub(crate) struct Report {
    /// Seconds measured (after the first 2).
    pub secs: f64,
    /// Pictures drawn per second.
    pub fps: f64,
    /// Gaps of over 100 ms between pictures drawn, their total and the longest (ms).
    pub stalls: u32,
    pub stall_ms: f64,
    pub worst_stall_ms: f64,
    /// Standard deviation of the time between pictures arriving whole (ms).
    pub jitter_ms: f64,
    /// Console to screen (ms).
    pub lat_p50: f64,
    pub lat_p95: f64,
    pub lat_p99: f64,
    pub host_skips: u64,
    pub guest_drops: u64,
    pub keyframes: u64,
    /// What reached the guest's channel (kbit/s, all data).
    pub goodput_kbps: f64,
    /// The host's backlog at its largest (driver + channel, bytes).
    pub max_backlog: usize,
    /// The track: NACKs and PLIs the host got, the bandwidth estimate's median and lowest
    /// (kbit/s, past the first estimates; 0 without one).
    pub nacks: u64,
    pub plis: u64,
    pub bwe_p50: u32,
    pub bwe_min: u32,
}

impl Report {
    /// The runs' average (the counts too, rounded).
    pub(crate) fn mean(v: &[Report]) -> Report {
        let n = v.len().max(1) as f64;
        let f = |g: fn(&Report) -> f64| v.iter().map(g).sum::<f64>() / n;
        let u = |g: fn(&Report) -> f64| (v.iter().map(g).sum::<f64>() / n).round();
        Report {
            secs: f(|r| r.secs),
            fps: f(|r| r.fps),
            stalls: u(|r| r.stalls as f64) as u32,
            stall_ms: f(|r| r.stall_ms),
            worst_stall_ms: v.iter().map(|r| r.worst_stall_ms).fold(0.0, f64::max),
            jitter_ms: f(|r| r.jitter_ms),
            lat_p50: f(|r| r.lat_p50),
            lat_p95: f(|r| r.lat_p95),
            lat_p99: f(|r| r.lat_p99),
            host_skips: u(|r| r.host_skips as f64) as u64,
            guest_drops: u(|r| r.guest_drops as f64) as u64,
            keyframes: u(|r| r.keyframes as f64) as u64,
            goodput_kbps: f(|r| r.goodput_kbps),
            max_backlog: v.iter().map(|r| r.max_backlog).max().unwrap_or(0),
            nacks: u(|r| r.nacks as f64) as u64,
            plis: u(|r| r.plis as f64) as u64,
            bwe_p50: u(|r| r.bwe_p50 as f64) as u32,
            bwe_min: v.iter().map(|r| r.bwe_min).min().unwrap_or(0),
        }
    }

    pub(crate) fn row(&self) -> String {
        format!(
            "{:3.0}s {:5.1} fps | {:3} stalls {:6.0} ms (worst {:5.0}) | jitter {:5.1} ms | lat p50 {:5.0} p95 {:5.0} p99 {:5.0} | skips host {:4} guest {:4} | keys {:3} | {:6.0} kbps | backlog max {:4} KiB | nacks {:5} plis {:3} | bwe p50 {:6} min {:6}",
            self.secs,
            self.fps,
            self.stalls,
            self.stall_ms,
            self.worst_stall_ms,
            self.jitter_ms,
            self.lat_p50,
            self.lat_p95,
            self.lat_p99,
            self.host_skips,
            self.guest_drops,
            self.keyframes,
            self.goodput_kbps,
            self.max_backlog / 1024,
            self.nacks,
            self.plis,
            self.bwe_p50,
            self.bwe_min
        )
    }
}

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// One direction of the link.
struct Way {
    /// When the narrow hop is free again.
    free_at: Instant,
    /// The last packet's arrival (no reordering).
    last_at: Instant,
}

/// A packet on the link: (arrival, seq, to_host, bytes).
type InFlight = (Instant, u64, bool, Vec<u8>);

struct Link {
    cfg: LinkCfg,
    rng: Rng,
    ways: [Way; 2],
    /// (arrival, seq) → (to_host, bytes)
    q: BinaryHeap<Reverse<InFlight>>,
    seq: u64,
}

impl Link {
    fn send(&mut self, now: Instant, to_host: bool, data: Vec<u8>) {
        let cfg = self.cfg;
        let w = &mut self.ways[to_host as usize];
        let mut depart = now;
        if cfg.cap_kbps > 0 {
            let start = w.free_at.max(now);
            let queued = start.saturating_duration_since(now);
            if queued > Duration::from_millis(cfg.queue_ms as u64) {
                return; // the narrow hop's queue is full
            }
            let tx = Duration::from_secs_f64(
                (data.len() + 28) as f64 * 8.0 / (cfg.cap_kbps as f64 * 1000.0),
            );
            w.free_at = start + tx;
            depart = w.free_at;
        }
        if self.rng.f() < cfg.loss {
            return;
        }
        let jitter = Duration::from_secs_f64(self.rng.f() * cfg.jitter_ms as f64 / 1000.0);
        let at = (depart + Duration::from_micros(cfg.rtt_ms as u64 * 500) + jitter).max(w.last_at);
        w.last_at = at;
        self.seq += 1;
        self.q.push(Reverse((at, self.seq, to_host, data)));
    }

    fn next_at(&self) -> Option<Instant> {
        self.q.peek().map(|Reverse((t, ..))| *t)
    }
}

const HOST: &str = "10.0.0.1:5000";
const GUEST: &str = "10.0.0.2:6000";
const KIND_VIDEO: u8 = 1;
const KIND_AUDIO: u8 = 4;
/// Our stamp in each picture: kind, frame number, its time (µs from the start), key.
const STAMP: usize = 1 + 4 + 8 + 1;

/// The host's side of one guest.
struct Host {
    rtc: Rtc,
    cid: Option<ChannelId>,
    /// The video track (RTP), and its H.264 payload type.
    mid: Option<Mid>,
    pt: Option<Pt>,
    /// The player's picture queue (rp_stream_api's, then join.rs `play` takes from it).
    vq: VecDeque<Vec<u8>>,
    /// Sound and status (the "other" queue, sent first).
    oq: VecDeque<Vec<u8>>,
    /// The driver's queue: (channel, binary, piece).
    out: VecDeque<(ChannelId, bool, Vec<u8>)>,
    out_bytes: usize,
    need_key: bool,
    lag_skips: u32,
    last_skip: Option<Instant>,
    key_asked: Option<Instant>,
    /// rp_stream_api `guest_key_asked` (one guest here, so the same as `key_asked` but kept apart).
    any_key_asked: Option<Instant>,
    /// The engine's per-guest ask gate (join.rs KeyAsk).
    guest_ask: Option<Instant>,
    want_key: bool,
    last_idr: Option<Instant>,
    key_due: Option<Instant>,
    skips: u64,
    keyframes: u64,
    max_backlog: usize,
    /// The stream's bitrate (the estimate is aimed at it), and what the track's stats said: the
    /// bandwidth estimates (kbit/s), NACKs and PLIs received.
    kbps: u32,
    bwe: Vec<u32>,
    nacks: u64,
    plis: u64,
}

/// The guest's page.
struct Guest {
    rtc: Rtc,
    cid: ChannelId,
    mid: Option<Mid>,
    reasm: Reassembler,
    saw_key: bool,
    last_ask: Option<Instant>,
    /// Decode queue: (gen µs, key); decoded one after another.
    dq: VecDeque<u64>,
    busy_until: Instant,
    drops: u64,
    arrivals: Vec<Instant>,
    shown: Vec<(Instant, u64)>,
    bytes: u64,
    asks: VecDeque<Instant>,
    /// Policy::guest_backlog.
    backlog: usize,
}

/// A decode takes this long on the guest (a phone's hardware decoder, one at a time).
const DECODE: Duration = Duration::from_millis(4);

pub(crate) fn run(link: LinkCfg, stream: StreamCfg, pol: Policy, secs: u64, seed: u64) -> Report {
    let t0 = Instant::now();
    let host_addr: SocketAddr = HOST.parse().unwrap();
    let guest_addr: SocketAddr = GUEST.parse().unwrap();

    // The guest's jitter buffer: a browser holds a frame by time (until its retransmission is
    // due), not by count; str0m's default 30 packets would give up on a 1080p keyframe.
    let mut g = Rtc::builder()
        .set_crypto_provider(super::crypto::provider())
        .set_reordering_size_video(4000)
        .build(t0);
    g.add_local_candidate(Candidate::host(guest_addr, "udp").unwrap());
    let mut api = g.sdp_api();
    let cid = api.add_channel("join".into());
    let gmid = pol
        .rtp_video
        .then(|| api.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None));
    let (offer, pending) = api.apply().unwrap();
    let (cert, _) = super::rtc::cert().unwrap();
    let mut hb = Rtc::builder()
        .set_crypto_provider(super::crypto::provider())
        .set_dtls_cert(cert.clone())
        .set_stats_interval(Some(Duration::from_secs(1)))
        .set_sctp_max_buffered_amount(pol.sctp_buffer);
    if pol.rtp_engine {
        hb = super::rtc::with_track(hb);
        if !pol.rtp_bwe {
            hb = hb.enable_bwe(None);
        }
    }
    let mut h = hb.build(t0);
    h.add_local_candidate(Candidate::host(host_addr, "udp").unwrap());
    let answer = h
        .sdp_api()
        .accept_offer(SdpOffer::from_sdp_string(&offer.to_sdp_string()).unwrap())
        .unwrap();
    g.sdp_api()
        .accept_answer(
            pending,
            SdpAnswer::from_sdp_string(&answer.to_sdp_string()).unwrap(),
        )
        .unwrap();

    let mut net = Link {
        cfg: link,
        rng: Rng(seed | 1),
        ways: [
            Way {
                free_at: t0,
                last_at: t0,
            },
            Way {
                free_at: t0,
                last_at: t0,
            },
        ],
        q: BinaryHeap::new(),
        seq: 0,
    };
    let mut sizes = Rng(seed.wrapping_mul(31) | 1);
    let mut host = Host {
        rtc: h,
        cid: None,
        mid: None,
        pt: None,
        vq: VecDeque::new(),
        oq: VecDeque::new(),
        out: VecDeque::new(),
        out_bytes: 0,
        need_key: true,
        lag_skips: 0,
        last_skip: None,
        key_asked: None,
        any_key_asked: None,
        guest_ask: None,
        want_key: true,
        last_idr: None,
        key_due: None,
        skips: 0,
        keyframes: 0,
        max_backlog: 0,
        kbps: stream.kbps,
        bwe: Vec::new(),
        nacks: 0,
        plis: 0,
    };
    let mut guest = Guest {
        rtc: g,
        cid,
        mid: gmid,
        reasm: Reassembler::new(64 << 20),
        saw_key: false,
        last_ask: None,
        dq: VecDeque::new(),
        busy_until: t0,
        drops: 0,
        arrivals: Vec::new(),
        shown: Vec::new(),
        bytes: 0,
        asks: VecDeque::new(),
        backlog: pol.guest_backlog,
    };

    let period = Duration::from_secs_f64(1.0 / stream.fps as f64);
    let mean = stream.kbps as f64 * 1000.0 / 8.0 / stream.fps as f64;
    let mut now = t0;
    let mut started: Option<Instant> = None;
    let mut next_frame = t0;
    let mut next_audio = t0;
    let mut next_status = t0;
    let mut frame_no = 0u32;
    let mut end = t0 + Duration::from_secs(30);
    let mut spins = 0u32;
    let trace = std::env::var_os("LINK_SIM_TRACE").is_some();
    let mut next_trace = t0;
    loop {
        // Everything each side has to do now.
        let host_to = drain_host(&mut host, &mut net, now, &pol);
        let guest_to = drain_guest(&mut guest, &mut net, now);
        if started.is_none() && host.cid.is_some() && (!pol.rtp_video || host.pt.is_some()) {
            started = Some(now);
            next_frame = now;
            next_audio = now;
            next_status = now;
            next_trace = now;
            end = now + Duration::from_secs(secs);
        }
        if now >= end {
            break;
        }
        // The next thing to happen.
        let mut next = end;
        for t in [
            host_to,
            guest_to,
            net.next_at(),
            started.map(|_| next_frame),
            started.map(|_| next_audio),
            started.map(|_| next_status),
            (!guest.dq.is_empty()).then_some(guest.busy_until),
            host.key_due,
        ]
        .into_iter()
        .flatten()
        {
            next = next.min(t);
        }
        if next <= now {
            spins += 1;
            if spins > 64 {
                next = now + Duration::from_micros(50);
                spins = 0;
            } else {
                next = now;
            }
        } else {
            spins = 0;
        }
        now = next;
        while net.next_at().is_some_and(|t| t <= now) {
            let Reverse((_, _, to_host, data)) = net.q.pop().unwrap();
            let (rtc, src, dst) = if to_host {
                (&mut host.rtc, guest_addr, host_addr)
            } else {
                (&mut guest.rtc, host_addr, guest_addr)
            };
            if let Ok(contents) = data.as_slice().try_into() {
                let _ = rtc.handle_input(Input::Receive(
                    now,
                    Receive {
                        proto: Protocol::Udp,
                        source: src,
                        destination: dst,
                        contents,
                    },
                ));
            }
            // str0m wants its output taken after every input.
            if to_host {
                drain_host(&mut host, &mut net, now, &pol);
            } else {
                drain_guest(&mut guest, &mut net, now);
            }
        }
        let _ = host.rtc.handle_input(Input::Timeout(now));
        let _ = guest.rtc.handle_input(Input::Timeout(now));
        // The guest's decoder.
        while !guest.dq.is_empty() && guest.busy_until <= now {
            let gen = guest.dq.pop_front().unwrap();
            guest.shown.push((guest.busy_until, gen));
            if !guest.dq.is_empty() {
                guest.busy_until += DECODE;
            }
        }
        let Some(t_start) = started else { continue };
        if trace && now >= next_trace {
            next_trace += Duration::from_millis(250);
            let b = backlog(&mut host);
            eprintln!(
                "t={:6.2}s backlog {:7} vq {} rx {:9} shown {} skips {} drops {}",
                now.duration_since(t_start).as_secs_f64(),
                b,
                host.vq.len(),
                guest.bytes,
                guest.shown.len(),
                host.skips,
                guest.drops
            );
        }
        // The console's stream thread: an asked-for keyframe (its 500 ms gate).
        if host.want_key && host.key_due.is_none() {
            let open = host
                .last_idr
                .is_none_or(|t| now.saturating_duration_since(t) > pol.idr_gate);
            if open {
                host.last_idr = Some(now);
                host.key_due = Some(now + Duration::from_millis(30));
                host.want_key = false;
            } else if !pol.idr_ask_kept {
                host.want_key = false; // swapped out and lost
            }
        }
        while next_frame <= now {
            let key = host.key_due.is_some_and(|t| t <= next_frame);
            if key {
                host.key_due = None;
                host.keyframes += 1;
            }
            let size = if key {
                mean * stream.key_ratio
            } else {
                mean * (0.6 + 0.8 * sizes.f())
            } as usize;
            let mut m = vec![0u8; size.max(STAMP)];
            m[0] = KIND_VIDEO;
            m[1..5].copy_from_slice(&frame_no.to_le_bytes());
            let gen = next_frame.duration_since(t_start).as_micros() as u64;
            m[5..13].copy_from_slice(&gen.to_le_bytes());
            m[13] = key as u8;
            frame_no += 1;
            if pol.rtp_video {
                to_track(&mut host, &m, key, gen, now);
            } else {
                to_player(&mut host, &pol, m, key, now);
            }
            next_frame += period;
        }
        while next_audio <= now {
            let mut m = vec![0u8; 120];
            m[0] = KIND_AUDIO;
            host.oq.push_back(m);
            next_audio += Duration::from_millis(10);
        }
        while next_status <= now {
            // The once-a-second status (text) and ping.
            host.oq.push_back(vec![b'{'; 300]);
            next_status += Duration::from_secs(1);
        }
    }
    report(&host, &guest, started.unwrap_or(t0), now, period)
}

/// rp_stream_api `to_spectators` for one player.
fn to_player(h: &mut Host, pol: &Policy, m: Vec<u8>, key: bool, now: Instant) {
    if h.need_key && !key {
        lag_ask(h, pol, now);
        return;
    }
    h.need_key = false;
    if h.vq.len() >= pol.player_queue {
        h.need_key = true;
        if h.last_skip
            .is_none_or(|t| now.saturating_duration_since(t) > pol.lag_forgiven)
        {
            h.lag_skips = 0;
        }
        h.lag_skips += 1;
        h.last_skip = Some(now);
        h.skips += 1;
        lag_ask(h, pol, now);
        return;
    }
    h.vq.push_back(m);
}

fn lag_ask(h: &mut Host, pol: &Policy, now: Instant) {
    let own = if h.lag_skips >= pol.lag_skips {
        pol.lagging_key_gap
    } else {
        pol.key_gap
    };
    let ago = |t: Option<Instant>, gap: Duration| {
        t.is_none_or(|t| now.saturating_duration_since(t) >= gap)
    };
    if ago(h.key_asked, own) && ago(h.any_key_asked, pol.key_gap) {
        h.key_asked = Some(now);
        h.any_key_asked = Some(now);
        h.want_key = true;
    }
}

fn backlog(h: &mut Host) -> usize {
    let mut b = h.out_bytes;
    for c in [h.cid].into_iter().flatten() {
        if let Some(mut ch) = h.rtc.channel(c) {
            b += ch.buffered_amount();
        }
    }
    b
}

/// join.rs `play` (sound first, a send held past the high water) and the driver's `flush`.
fn pump(h: &mut Host, pol: &Policy) {
    let Some(cid) = h.cid else { return };
    loop {
        flush(h);
        let b = backlog(h);
        h.max_backlog = h.max_backlog.max(b);
        if b > pol.high_water {
            return;
        }
        let (m, vid) = if let Some(m) = h.oq.pop_front() {
            (m, false)
        } else if let Some(m) = h.vq.pop_front() {
            (m, true)
        } else {
            return;
        };
        let _ = vid;
        let ch = cid;
        if m[0] == b'{' {
            h.out_bytes += m.len();
            h.out.push_back((ch, false, m));
        } else {
            for p in chunk::split(&m) {
                h.out_bytes += p.len();
                h.out.push_back((ch, true, p));
            }
        }
    }
}

fn flush(h: &mut Host) {
    while let Some((c, bin, data)) = h.out.front() {
        let Some(mut ch) = h.rtc.channel(*c) else {
            return;
        };
        match ch.write(*bin, data) {
            Ok(true) => {
                h.out_bytes -= data.len();
                h.out.pop_front();
            }
            _ => return,
        }
    }
}

fn drain_host(h: &mut Host, net: &mut Link, now: Instant, pol: &Policy) -> Option<Instant> {
    let mut to = None;
    for _ in 0..2 {
        pump(h, pol);
        loop {
            match h.rtc.poll_output() {
                Ok(Output::Timeout(t)) => {
                    to = Some(t);
                    break;
                }
                Err(_) => return None,
                Ok(Output::Transmit(t)) => net.send(now, false, t.contents.to_vec()),
                Ok(Output::Event(e)) => match e {
                    Event::ChannelOpen(id, label) => {
                        if label == "join" {
                            h.cid = Some(id);
                        }
                    }
                    Event::MediaAdded(m) if pol.rtp_engine => {
                        // rtc.rs: the payload type for the stream's profile, the estimate aimed
                        // at its bitrate.
                        h.mid = Some(m.mid);
                        h.pt = super::rtc::TrackPts::of(&mut h.rtc, m.mid).h264_for(Some(0x64));
                        let k = h.kbps as u64;
                        h.rtc.bwe().reset(Bitrate::kbps(k));
                        h.rtc
                            .bwe()
                            .set_bitrate(Bitrate::kbps(0), Bitrate::kbps(k * 5 / 4));
                    }
                    Event::MediaAdded(m) => {
                        h.mid = Some(m.mid);
                        h.pt = h.rtc.writer(m.mid).and_then(|w| {
                            w.payload_params()
                                .find(|p| p.spec().codec == Codec::H264)
                                .map(|p| p.pt())
                        });
                    }
                    Event::EgressBitrateEstimate(BweKind::Twcc { estimate, .. }) => {
                        h.bwe.push((estimate.as_f64() / 1000.0) as u32);
                    }
                    Event::MediaEgressStats(s) => {
                        h.nacks = s.nacks;
                        h.plis = s.plis;
                        if std::env::var_os("LINK_SIM_TRACE").is_some() {
                            eprintln!(
                                "egress: packets {} nacks {} plis {} rtt {:?} loss {:?}",
                                s.packets, s.nacks, s.plis, s.rtt, s.loss
                            );
                        }
                    }
                    // The guest's PLI: the same gates as its keyframe ask.
                    Event::KeyframeRequest(_) => {
                        if h.guest_ask.is_none_or(|t| {
                            now.saturating_duration_since(t) >= Duration::from_secs(1)
                        }) {
                            h.guest_ask = Some(now);
                            lag_ask(h, pol, now);
                        }
                    }
                    // The page's {"keyframe": true}: join.rs KeyAsk, then lag_ask.
                    Event::ChannelData(d)
                        if !d.binary
                            && d.data.windows(8).any(|w| w == b"keyframe")
                            && h.guest_ask.is_none_or(|t| {
                                now.saturating_duration_since(t) >= Duration::from_secs(1)
                            }) =>
                    {
                        h.guest_ask = Some(now);
                        lag_ask(h, pol, now);
                    }
                    _ => {}
                },
            }
        }
    }
    to
}

fn drain_guest(g: &mut Guest, net: &mut Link, now: Instant) -> Option<Instant> {
    let mut to;
    loop {
        match g.rtc.poll_output() {
            Ok(Output::Timeout(t)) => {
                to = Some(t);
                break;
            }
            Err(_) => return None,
            Ok(Output::Transmit(t)) => net.send(now, true, t.contents.to_vec()),
            Ok(Output::Event(Event::ChannelData(d))) => {
                g.bytes += d.data.len() as u64;
                if !d.binary {
                    continue;
                }
                if d.id != g.cid {
                    continue;
                }
                let r = &mut g.reasm;
                if let Ok(Piece::Message(m)) = r.push(&d.data) {
                    if m.first() == Some(&KIND_VIDEO) && m.len() >= STAMP {
                        on_picture(g, &m, now);
                    }
                }
            }
            Ok(Output::Event(Event::MediaData(d))) => {
                g.bytes += d.data.len() as u64;
                on_track(g, &d.data, d.contiguous, now);
            }
            Ok(Output::Event(_)) => {}
        }
    }
    while let Some(_t) = g.asks.pop_front() {
        match g.mid {
            // The browser's PLI.
            Some(mid) => {
                if let Some(mut w) = g.rtc.writer(mid) {
                    let _ = w.request_keyframe(None, KeyframeRequestKind::Pli);
                }
            }
            None => {
                if let Some(mut ch) = g.rtc.channel(g.cid) {
                    let _ = ch.write(false, br#"{"keyframe":true}"#);
                }
            }
        }
        to = Some(now); // to go out
    }
    to
}

/// A picture as H.264 the track carries: an Annex B access unit of one NAL (IDR or not) whose
/// payload starts with our stamp, its bytes as nibbles (never 0, so never a start code).
fn annex_b(m: &[u8], key: bool) -> Vec<u8> {
    let mut v = Vec::with_capacity(m.len() + STAMP + 5);
    v.extend_from_slice(&[0, 0, 0, 1, if key { 0x65 } else { 0x41 }]);
    for b in &m[..STAMP] {
        v.push(0x40 | (b >> 4));
        v.push(0x40 | (b & 0x0f));
    }
    v.resize(m.len().max(v.len()), 0xab);
    v
}

/// The stamp back from a track's access unit.
fn stamp_of(au: &[u8]) -> Option<Vec<u8>> {
    let i = au.windows(3).position(|w| w == [0, 0, 1])? + 3;
    let body = au.get(i + 1..i + 1 + 2 * STAMP)?;
    Some(
        body.chunks(2)
            .map(|c| ((c[0] & 0x0f) << 4) | (c[1] & 0x0f))
            .collect(),
    )
}

/// The console's picture straight onto the track (RTP has no backlog to skip: str0m sends it at
/// once, keeps it for retransmission, and a lost piece costs that frame only on the receiver).
fn to_track(h: &mut Host, m: &[u8], key: bool, gen_us: u64, now: Instant) {
    if h.need_key && !key {
        return;
    }
    h.need_key = false;
    let (Some(mid), Some(pt)) = (h.mid, h.pt) else {
        return;
    };
    let au = annex_b(m, key);
    if let Some(w) = h.rtc.writer(mid) {
        let t = MediaTime::new(gen_us * 90 / 1000, Frequency::NINETY_KHZ);
        let _ = w.write(pt, now, t, au);
    }
}

/// The browser's video element, roughly: a picture after a gap it could not repair waits for a
/// keyframe (and asks with a PLI); the rest decodes in order.
fn on_track(g: &mut Guest, au: &[u8], contiguous: bool, now: Instant) {
    let Some(st) = stamp_of(au) else { return };
    if st.len() < STAMP || st[0] != KIND_VIDEO {
        return;
    }
    g.arrivals.push(now);
    let gen = u64::from_le_bytes(st[5..13].try_into().unwrap());
    let key = st[13] == 1;
    if std::env::var_os("LINK_SIM_TRACE").is_some() && (key || !contiguous) {
        eprintln!(
            "track: frame {} key {key} contiguous {contiguous} at {:?}",
            u32::from_le_bytes(st[1..5].try_into().unwrap()),
            now
        );
    }
    if !contiguous && !key {
        g.saw_key = false;
    }
    if !g.saw_key && !key {
        g.drops += 1;
        if g.last_ask
            .is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_secs(1))
        {
            g.last_ask = Some(now);
            g.asks.push_back(now);
        }
        return;
    }
    g.saw_key = true;
    if g.dq.is_empty() {
        g.busy_until = now + DECODE;
    }
    g.dq.push_back(gen);
}

/// watchPlayer `onVideo`.
fn on_picture(g: &mut Guest, m: &[u8], now: Instant) {
    g.arrivals.push(now);
    let gen = u64::from_le_bytes(m[5..13].try_into().unwrap());
    let key = m[13] == 1;
    let ask = |g: &mut Guest| {
        if g.last_ask
            .is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_secs(1))
        {
            g.last_ask = Some(now);
            g.asks.push_back(now);
        }
    };
    if !g.saw_key && !key {
        g.drops += 1;
        return ask(g);
    }
    if !key && g.dq.len() > g.backlog {
        g.saw_key = false;
        g.drops += 1;
        return ask(g);
    }
    g.saw_key = true;
    if g.dq.is_empty() {
        g.busy_until = now + DECODE;
    }
    g.dq.push_back(gen);
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn report(h: &Host, g: &Guest, start: Instant, end: Instant, period: Duration) -> Report {
    // The first 2 s are the start (the first keyframe): not counted.
    let from = start + Duration::from_secs(2);
    let secs = end.saturating_duration_since(from).as_secs_f64().max(0.001);
    let shown: Vec<(Instant, u64)> = g
        .shown
        .iter()
        .copied()
        .filter(|(t, _)| *t >= from)
        .collect();
    let mut r = Report {
        secs,
        fps: shown.len() as f64 / secs,
        host_skips: h.skips,
        guest_drops: g.drops,
        keyframes: h.keyframes,
        goodput_kbps: g.bytes as f64 * 8.0
            / 1000.0
            / end
                .saturating_duration_since(start)
                .as_secs_f64()
                .max(0.001),
        max_backlog: h.max_backlog,
        nacks: h.nacks,
        plis: h.plis,
        ..Report::default()
    };
    // The estimates past the first few (it starts at the stream's bitrate and probes).
    let mut est: Vec<f64> = h
        .bwe
        .iter()
        .skip(h.bwe.len() / 15)
        .map(|&k| k as f64)
        .collect();
    if !est.is_empty() {
        r.bwe_min = est.iter().copied().fold(f64::INFINITY, f64::min) as u32;
        r.bwe_p50 = pct(&mut est, 0.5) as u32;
    }
    // Stalls: no new picture for over 100 ms (the end of the run counts too).
    let mut last = from;
    for (t, _) in shown.iter().chain(std::iter::once(&(end, 0))) {
        let gap = t.saturating_duration_since(last).as_secs_f64() * 1000.0;
        if gap > 100.0 {
            r.stalls += 1;
            r.stall_ms += gap;
            r.worst_stall_ms = r.worst_stall_ms.max(gap);
        }
        last = *t;
    }
    let arr: Vec<f64> = g
        .arrivals
        .iter()
        .filter(|t| **t >= from)
        .collect::<Vec<_>>()
        .windows(2)
        .map(|w| w[1].saturating_duration_since(*w[0]).as_secs_f64() * 1000.0)
        .collect();
    if !arr.is_empty() {
        let p = period.as_secs_f64() * 1000.0;
        r.jitter_ms = (arr.iter().map(|d| (d - p).powi(2)).sum::<f64>() / arr.len() as f64).sqrt();
    }
    let mut lat: Vec<f64> = shown
        .iter()
        .map(|(t, gen)| {
            (t.saturating_duration_since(start).as_micros() as f64 - *gen as f64) / 1000.0
        })
        .collect();
    r.lat_p50 = pct(&mut lat, 0.5);
    r.lat_p95 = pct(&mut lat, 0.95);
    r.lat_p99 = pct(&mut lat, 0.99);
    r
}

/// The links the table runs: (name, link).
pub(crate) fn links() -> Vec<(&'static str, LinkCfg)> {
    let l = |rtt_ms, jitter_ms, loss, cap_kbps| LinkCfg {
        rtt_ms,
        jitter_ms,
        loss,
        cap_kbps,
        queue_ms: 60,
    };
    vec![
        ("30ms clean", l(30, 2, 0.0, 0)),
        ("30ms 0.5%", l(30, 5, 0.005, 0)),
        ("30ms 1%", l(30, 5, 0.01, 0)),
        ("30ms 2%", l(30, 5, 0.02, 0)),
        ("80ms 0.5%", l(80, 10, 0.005, 0)),
        ("80ms 1%", l(80, 10, 0.01, 0)),
        ("80ms 2%", l(80, 10, 0.02, 0)),
        ("30ms 25M up", l(30, 5, 0.0, 25_000)),
        ("30ms 12M up", l(30, 5, 0.0, 12_000)),
        ("30ms 0.5% 25M", l(30, 5, 0.005, 25_000)),
    ]
}

#[test]
fn a_clean_link_shows_every_picture() {
    let link = LinkCfg {
        rtt_ms: 30,
        jitter_ms: 2,
        loss: 0.0,
        cap_kbps: 0,
        queue_ms: 60,
    };
    let r = run(link, StreamCfg::DEFAULT, Policy::now(), 5, 7);
    assert!(r.fps > 55.0, "{}", r.row());
    assert_eq!(r.stalls, 0, "{}", r.row());
    assert_eq!(r.host_skips, 0, "{}", r.row());
}

/// The table: every link, the engine before and now. Run with `--release --ignored --nocapture`.
#[test]
#[ignore]
fn link_sim_table() {
    let secs = std::env::var("LINK_SIM_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let kbps: Vec<u32> = std::env::var("LINK_SIM_KBPS")
        .ok()
        .map(|s| s.split(',').filter_map(|k| k.parse().ok()).collect())
        .unwrap_or_else(|| vec![StreamCfg::DEFAULT.kbps]);
    let only = std::env::var("LINK_SIM_POLICY").ok();
    let seeds: u64 = std::env::var("LINK_SIM_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    let deep = Policy {
        player_queue: 60,
        high_water: 4 << 20,
        sctp_buffer: 4 << 20,
        ..Policy::now()
    };
    for (name, pol) in [
        ("before", Policy::before()),
        ("now", Policy::now()),
        ("never-skip", deep),
        (
            // The page's decode-queue limit doubled: measured no better (kept for the record).
            "guest-backlog-8",
            Policy {
                guest_backlog: 8,
                ..Policy::now()
            },
        ),
        (
            // #762's prototype: str0m's defaults.
            "rtp",
            Policy {
                rtp_video: true,
                ..Policy::now()
            },
        ),
        (
            // The engine's track (rtc.rs with_track).
            "track",
            Policy {
                rtp_video: true,
                rtp_engine: true,
                ..Policy::now()
            },
        ),
        (
            // …without the bandwidth estimate and its pacer (measured, not shipped).
            "track-nobwe",
            Policy {
                rtp_video: true,
                rtp_engine: true,
                rtp_bwe: false,
                ..Policy::now()
            },
        ),
    ] {
        if only
            .as_deref()
            .is_some_and(|o| !o.split(',').any(|o| o == name))
        {
            continue;
        }
        for &k in &kbps {
            println!("== {name} at {k} kbit/s: {pol:?}");
            let stream = StreamCfg {
                kbps: k,
                ..StreamCfg::DEFAULT
            };
            let link_only = std::env::var("LINK_SIM_LINK").ok();
            for (lname, link) in links() {
                if link_only.as_deref().is_some_and(|l| l != lname) {
                    continue;
                }
                // LINK_SIM_SEEDS runs: one loss pattern can hit a keyframe or miss it.
                let runs: Vec<Report> = (0..seeds)
                    .map(|i| run(link, stream, pol, secs, 42 + i * 1000))
                    .collect();
                println!("{lname:>14} | {}", Report::mean(&runs).row());
            }
        }
    }
}
