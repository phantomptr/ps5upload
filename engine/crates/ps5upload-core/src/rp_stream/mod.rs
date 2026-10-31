// SPDX-License-Identifier: AGPL-3.0-only
// Ported from chiaki-ng: Copyright (C) the Chiaki and chiaki-ng authors. Rust port for
// ps5upload. Licensed under the GNU AGPL v3.0 (LICENSES/AGPL-3.0.txt).
//! Remote Play streaming (spike): open a session with a paired PS5 and receive
//! its video.
//!
//! Ported from chiaki-ng (AGPL-3.0: `lib/src/session.c`, `ctrl.c`,
//! `streamconnection.c`, `launchspec.c`, `takion.c`, `gkcrypt.c`, `ecdh.c`,
//! `videoreceiver.c`, `frameprocessor.c`), reusing this crate's pairing and
//! control-session pieces (`rp_regist`, `rp_session`).
//!
//! The order, each step waiting on the last:
//! 1. **session-init** (TCP 9295, plain): the console hands out a nonce.
//! 2. **ctrl** (TCP 9295, our auth encrypted with the nonce + RP-Key): the
//!    console answers with a SESSION_ID and then heartbeats, which must be
//!    answered for the whole stream. A background thread does that.
//! 3. **Takion** (UDP 9296): INIT/COOKIE handshake, then BIG (our launch spec
//!    and ECDH key) → BANG (the console's key) → both sides switch on GKCrypt →
//!    STREAMINFO (resolutions + codec headers) → we ack, say a controller is
//!    connected and send our mic's audio header → video and audio flow; we
//!    heartbeat every second and report congestion every 200 ms.
//!
//! Senkusha (the MTU/RTT probe on 9297) is skipped: chiaki falls back to an
//! MTU of 1454 and 1 ms RTT when it fails and streams anyway.

pub mod audio;
pub mod crypto;
pub mod fec;
pub mod feedback;
pub mod guard;
pub mod mic;
pub mod orientation;
pub mod proto;
pub mod takion;
pub mod video;

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use crate::rp_regist::split_http;
use crate::rp_session::{
    ambassador, auth_decrypt, auth_encrypt, b64, bright, make_did, session_init, SessionCreds,
    KEY_SIZE, RP_VERSION, SESSION_PORT,
};

use self::crypto::{Ecdh, GkCrypt, HANDSHAKE_KEY_SIZE};
use self::proto::Message;
use self::takion::{
    Event, Takion, DATA_TYPE_PAD_INFO, DATA_TYPE_PROTOBUF, DATA_TYPE_RUMBLE,
    DATA_TYPE_TRIGGER_EFFECTS,
};

pub use self::audio::{AudioAssembler, AudioHeader, HapticsAssembler};
pub use self::feedback::{button, ControllerState, FeedbackSender};
pub use self::mic::MicInput;
pub use self::proto::Resolution;
pub use self::video::{FrameAssembler, VideoStats};

const STREAM_PORT: u16 = 9296;
const MTU: u32 = 1454;
const RTT_MS: u32 = 1;
/// Takion protocol version a PS5 speaks.
const TAKION_VERSION_PS5: u32 = 12;

const CTRL_SESSION_ID: u16 = 0x0033;
/// The shortest a button press lasts on the console (about three frames).
const MIN_HOLD: Duration = Duration::from_millis(50);
/// Tries at the ctrl connection for one nonce, a second apart.
const CTRL_ATTEMPTS: u32 = 8;
const CTRL_HEARTBEAT_REQ: u16 = 0x00fe;
const CTRL_HEARTBEAT_REP: u16 = 0x01fe;
const CTRL_LOGIN_PIN_REQ: u16 = 0x0004;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    H265,
    /// H.265 Main10 in HDR (PQ, BT.2020): streaming type 3 and `"dynamicRange":"HDR"` in
    /// the launch spec, as chiaki-ng asks for it (ctrl.c, launchspec.c).
    H265Hdr,
}

impl Codec {
    /// The stream is H.265 (SDR or HDR): its access units and headers are HEVC.
    pub fn is_hevc(self) -> bool {
        matches!(self, Codec::H265 | Codec::H265Hdr)
    }

    /// The codec a start message names ("h264", "h265", "h265_hdr"), or None.
    pub fn from_name(name: &str) -> Option<Codec> {
        match name {
            "h264" => Some(Codec::H264),
            "h265" => Some(Codec::H265),
            "h265_hdr" => Some(Codec::H265Hdr),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StreamConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub codec: Codec,
    /// Wake a console found in rest mode before opening the session.
    pub wake: bool,
    /// Set up the microphone (experimental, see mic.rs). Off, the session
    /// sends exactly what it did before the mic existed.
    pub mic: bool,
    /// Say the pad is a DualSense (a real one is attached), so the console sends adaptive
    /// triggers and haptics. Off, the session sends exactly what it did before.
    pub dualsense: bool,
}

impl Default for StreamConfig {
    fn default() -> Self {
        // chiaki's 720p60 preset.
        Self {
            width: 1280,
            height: 720,
            fps: 60,
            bitrate_kbps: 10000,
            codec: Codec::H264,
            wake: true,
            mic: false,
            dualsense: false,
        }
    }
}

/// Milestones, for a caller that wants to time or log them.
#[derive(Debug, Clone)]
pub enum Progress {
    /// The console was in rest mode and is being woken (takes ~20 s).
    Waking,
    Nonce,
    SessionId,
    TakionConnected,
    Bang,
    StreamInfo(Vec<(u32, u32)>),
    FirstFrame(usize),
    /// Once a second while streaming.
    Stats(LiveStats),
}

/// What the stream did in the last second, for a stats overlay.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct LiveStats {
    pub frames: u64,
    pub frames_rebuilt: u64,
    pub frames_lost: u64,
    pub video_bytes: u64,
    pub units: u64,
    pub units_lost: u64,
}

/// Ctrl: our auth, the console's SESSION_ID, then heartbeats until stopped.
struct Ctrl {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    stream: TcpStream,
    /// With the mic or DualSense on, writes go through here: the heartbeat thread and the
    /// stream both send. Off, the heartbeat thread writes alone, as before.
    writer: Option<Arc<Mutex<TcpStream>>>,
    /// Our rpcrypt counter: one per payload we send (the auth headers took
    /// the first five), and the keys it runs on.
    counter_local: u64,
    brt: [u8; 16],
    amb: [u8; 16],
}

impl Ctrl {
    /// A message with a payload, encrypted on our counter (chiaki
    /// `ctrl_message_send`). Used for the microphone (see mic.rs) and the DualSense.
    fn send(&mut self, kind: u16, payload: &[u8]) -> Result<()> {
        let msg = ctrl_message(kind, payload, &self.brt, &self.amb, self.counter_local);
        self.counter_local += 1;
        let Some(writer) = self.writer.as_ref() else {
            bail!("ctrl messages are for a session with the mic or DualSense set up");
        };
        let mut w = writer.lock().map_err(|_| anyhow!("ctrl writer poisoned"))?;
        w.write_all(&msg)?;
        Ok(())
    }
}

impl Ctrl {
    /// The heartbeat thread stopped on its own: the console closed ctrl, or it broke. The
    /// console's heartbeats then go unanswered and the session is over.
    fn ended(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| t.is_finished())
    }
}

impl Drop for Ctrl {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.stream.shutdown(Shutdown::Both);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn resolve(host: &str, port: u16) -> Result<SocketAddr> {
    (host, port)
        .to_socket_addrs()
        .with_context(|| format!("resolving {host}"))?
        .next()
        .ok_or_else(|| anyhow!("{host} did not resolve"))
}

fn ctrl_open(
    host: &str,
    creds: &SessionCreds,
    nonce: &[u8; KEY_SIZE],
    codec: Codec,
    shared_writer: bool,
) -> Result<(Ctrl, String, [u8; 16], [u8; 16])> {
    let amb = ambassador(nonce);
    let brt = bright(nonce, &creds.morning);
    let mut counter = 0u64;
    let mut next = |data: &[u8]| {
        let enc = auth_encrypt(&brt, &amb, counter, data);
        counter += 1;
        b64(&enc)
    };
    let auth = next(&creds.regist_key);
    let did = next(&make_did(&creds.regist_key));
    let ostype = next(b"Win10.0.0\0");
    let bitrate = next(&[0u8; 4]);
    let streaming_type: u32 = match codec {
        Codec::H264 => 1,
        Codec::H265 => 2,
        Codec::H265Hdr => 3,
    };
    let streaming_type = next(&streaming_type.to_le_bytes());
    let counter_local = counter;

    let addr = resolve(host, SESSION_PORT)?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(10))
        .context("connecting for ctrl")?;
    s.set_nodelay(true).ok();
    let req = format!(
        "GET /sie/ps5/rp/sess/ctrl HTTP/1.1\r\n\
         Host: {host}:{SESSION_PORT}\r\n\
         User-Agent: remoteplay Windows\r\n\
         Connection: keep-alive\r\n\
         Content-Length: 0\r\n\
         RP-Auth: {auth}\r\n\
         RP-Version: {RP_VERSION}\r\n\
         RP-Did: {did}\r\n\
         RP-ControllerType: 3\r\n\
         RP-ClientType: 11\r\n\
         RP-OSType: {ostype}\r\n\
         RP-ConPath: 1\r\n\
         RP-StartBitrate: {bitrate}\r\n\
         RP-StreamingType: {streaming_type}\r\n\r\n"
    );
    s.write_all(req.as_bytes())?;
    s.set_read_timeout(Some(Duration::from_secs(15)))?;

    let mut raw = Vec::new();
    let mut buf = [0u8; 2048];
    let header_end = loop {
        let n = s.read(&mut buf).context("reading the ctrl reply")?;
        if n == 0 {
            bail!("the console closed the control connection without answering");
        }
        raw.extend_from_slice(&buf[..n]);
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let reply = split_http(&raw[..header_end])?;
    if reply.code != 200 {
        let reason = reply
            .headers
            .iter()
            .find(|(k, _)| k == "rp-application-reason")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        bail!(
            "the console refused the control session (HTTP {}) {reason}",
            reply.code
        );
    }
    let mut pending = raw[header_end..].to_vec();

    // The console's counter starts at 1 (measured; see rp_session).
    let mut remote_counter = 1u64;
    let deadline = Instant::now() + Duration::from_secs(15);
    s.set_read_timeout(Some(Duration::from_millis(500)))?;
    let session_id = loop {
        if Instant::now() > deadline {
            bail!("the console never sent a session id over ctrl");
        }
        if pending.len() >= 8 {
            let size = u32::from_be_bytes(pending[0..4].try_into().unwrap()) as usize;
            if size > 8192 {
                bail!("ctrl stream desynced");
            }
            if pending.len() >= 8 + size {
                let kind = u16::from_be_bytes([pending[4], pending[5]]);
                let raw_payload = pending[8..8 + size].to_vec();
                pending.drain(..8 + size);
                let mut payload = Vec::new();
                if !raw_payload.is_empty() {
                    payload = auth_decrypt(&brt, &amb, remote_counter, &raw_payload);
                    remote_counter += 1;
                }
                match kind {
                    CTRL_HEARTBEAT_REQ => s.write_all(&ctrl_msg(CTRL_HEARTBEAT_REP))?,
                    CTRL_LOGIN_PIN_REQ => bail!(
                        "the console asks for a login passcode, which streaming cannot supply"
                    ),
                    CTRL_SESSION_ID => {
                        // A length byte, then the id. chiaki takes the console's
                        // id only when it is 24+ alphanumeric characters and
                        // otherwise makes one up, which the console accepts:
                        // FW 13.60 sends 16 characters, so the fallback is the
                        // path that runs there (measured).
                        if session_id_ok(&payload) {
                            break String::from_utf8_lossy(&payload[1..]).into_owned();
                        }
                        break fallback_session_id()?;
                    }
                    _ => {}
                }
                continue;
            }
        }
        match s.read(&mut buf) {
            Ok(0) => bail!("the console closed ctrl before sending a session id"),
            Ok(n) => pending.extend_from_slice(&buf[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(anyhow!("reading ctrl: {e}")),
        }
    };

    // Keep answering heartbeats for the life of the stream.
    let stop = Arc::new(AtomicBool::new(false));
    let mut ts = s.try_clone()?;
    let writer = if shared_writer {
        Some(Arc::new(Mutex::new(s.try_clone()?)))
    } else {
        None
    };
    let tw = writer.clone();
    let stop_t = stop.clone();
    let thread = std::thread::Builder::new()
        .name("rp-ctrl".into())
        .spawn(move || {
            let mut pending = pending;
            let mut buf = [0u8; 2048];
            while !stop_t.load(Ordering::Relaxed) {
                while pending.len() >= 8 {
                    let size = u32::from_be_bytes(pending[0..4].try_into().unwrap()) as usize;
                    // Out of step: nothing after this can be read, heartbeats included.
                    if size > 8192 {
                        return;
                    }
                    if pending.len() < 8 + size {
                        break;
                    }
                    let kind = u16::from_be_bytes([pending[4], pending[5]]);
                    pending.drain(..8 + size);
                    if kind == CTRL_HEARTBEAT_REQ && !heartbeat_reply(&mut ts, tw.as_deref()) {
                        return;
                    }
                }
                match ts.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => pending.extend_from_slice(&buf[..n]),
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(_) => return,
                }
            }
        })?;
    Ok((
        Ctrl {
            stop,
            thread: Some(thread),
            stream: s,
            writer,
            counter_local,
            brt,
            amb,
        },
        session_id,
        brt,
        amb,
    ))
}

/// chiaki's stand-in session id: seconds of uptime, then 48 random bytes in base64.
fn fallback_session_id() -> Result<String> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() % 1_000_000)
        .unwrap_or(0);
    let mut r = [0u8; 48];
    getrandom::fill(&mut r).map_err(|e| anyhow!("random: {e}"))?;
    Ok(format!("{secs}{}", b64(&r)))
}

/// `[len][len alphanumeric bytes]` with at least 24 characters, as chiaki accepts.
fn session_id_ok(p: &[u8]) -> bool {
    p.len() >= 25
        && p[0] as usize == p.len() - 1
        && p[1..].iter().all(|c| c.is_ascii_alphanumeric())
}

/// Answer a heartbeat: straight on the socket as always, or through the
/// shared writer when the mic is set up (it sends ctrl messages too).
fn heartbeat_reply(ts: &mut TcpStream, shared: Option<&Mutex<TcpStream>>) -> bool {
    match shared {
        None => ts.write_all(&ctrl_msg(CTRL_HEARTBEAT_REP)).is_ok(),
        Some(m) => m
            .lock()
            .is_ok_and(|mut w| w.write_all(&ctrl_msg(CTRL_HEARTBEAT_REP)).is_ok()),
    }
}

/// chiaki `CTRL_MESSAGE_TYPE_ENABLE_DUALSENSE_FEATURES`, and the message it sends right after.
const CTRL_ENABLE_DUALSENSE: u16 = 0x13;
const CTRL_DUALSENSE_CONNECT: u16 = 0x11;

/// What a session sends on ctrl right after the session id (chiaki `ctrl_enable_features`):
/// with a DualSense, its features switched on; with the mic set up, the mic muted twice.
/// Without either, nothing, as before both existed.
fn start_ctrl_messages(mic: bool, dualsense: bool) -> Vec<(u16, Vec<u8>)> {
    let mut v = Vec::new();
    if dualsense {
        v.push((CTRL_ENABLE_DUALSENSE, vec![0x00, 0x40, 0x00]));
        v.push((
            CTRL_DUALSENSE_CONNECT,
            vec![
                0xa0, 0xab, 0x51, 0xbd, 0xd1, 0x7e, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00,
                0x00, 0x00,
            ],
        ));
    }
    if mic {
        v.push((mic::CTRL_MIC_TOGGLE, mic::toggle_payload(false).to_vec()));
        v.push((mic::CTRL_MIC_TOGGLE, mic::toggle_payload(false).to_vec()));
    }
    v
}

/// The Takion messages after STREAMINFO, as (channel, protobuf): the ack,
/// the controller (a DualSense or a DualShock 4), and with the mic the uplink
/// audio header (chiaki `stream_connection_enable_microphone`).
fn after_streaminfo_messages(mic: bool, dualsense: bool) -> Vec<(u16, Vec<u8>)> {
    let mut v = vec![
        (9, proto::bare(proto::kind::STREAMINFOACK)),
        (1, proto::controller_connection(dualsense)),
    ];
    if mic {
        v.push((1, proto::stream_info_mic(&mic::stream_info_audio_header())));
    }
    v
}

/// One trigger's effect: its mode and 10 parameter bytes.
pub type TriggerEffect = (u8, [u8; 10]);

/// Trigger effects (data type 11): `[?, left mode, right mode, ?, ?, left 10 bytes, right 10
/// bytes]`, 25 bytes at least (chiaki `stream_connection_takion_data_trigger_effects`).
fn parse_trigger_effects(b: &[u8]) -> Option<(TriggerEffect, TriggerEffect)> {
    if b.len() < 25 {
        return None;
    }
    Some((
        (b[1], b[5..15].try_into().ok()?),
        (b[2], b[15..25].try_into().ok()?),
    ))
}

/// The haptics and trigger strength the console's settings ask for, from pad info: at 20/21
/// in the 0x19-byte form, 12/13 in the 0x11-byte one (chiaki ..._data_pad_info).
fn pad_info_intensity(b: &[u8]) -> Option<(u8, u8)> {
    match b.len() {
        0x19 => Some((b[20], b[21])),
        0x11 => Some((b[12], b[13])),
        _ => None,
    }
}

/// An empty ctrl message: just the 8-byte header.
fn ctrl_msg(kind: u16) -> [u8; 8] {
    let mut h = [0u8; 8];
    h[4..6].copy_from_slice(&kind.to_be_bytes());
    h
}

/// `[size BE32][kind BE16][0, 0]` then the payload, encrypted with the ctrl
/// cipher at `counter` (chiaki `ctrl_message_send`).
fn ctrl_message(
    kind: u16,
    payload: &[u8],
    brt: &[u8; 16],
    amb: &[u8; 16],
    counter: u64,
) -> Vec<u8> {
    let mut m = Vec::with_capacity(8 + payload.len());
    m.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    m.extend_from_slice(&kind.to_be_bytes());
    m.extend_from_slice(&[0, 0]);
    m.extend_from_slice(&auth_encrypt(brt, amb, counter, payload));
    m
}

fn launch_spec(cfg: &StreamConfig, handshake_key: &[u8; HANDSHAKE_KEY_SIZE]) -> String {
    let codec = if cfg.codec.is_hevc() { "hevc" } else { "avc" };
    let range = if cfg.codec == Codec::H265Hdr {
        "HDR"
    } else {
        "SDR"
    };
    format!(
        "{{\"sessionId\":\"sessionId4321\",\"streamResolutions\":[{{\"resolution\":{{\"width\":{w},\"height\":{h}}},\
\"maxFps\":{fps},\"score\":10}}],\"network\":{{\"bwKbpsSent\":{bw},\"bwLoss\":0.001000,\"mtu\":{mtu},\"rtt\":{rtt},\
\"ports\":[53,2053]}},\"slotId\":1,\"appSpecification\":{{\"minFps\":30,\"minBandwidth\":0,\"extTitleId\":\"ps3\",\
\"version\":1,\"timeLimit\":1,\"startTimeout\":100,\"afkTimeout\":100,\"afkTimeoutDisconnect\":100}},\"konan\":{{\
\"ps3AccessToken\":\"accessToken\",\"ps3RefreshToken\":\"refreshToken\"}},\"requestGameSpecification\":{{\
\"model\":\"bravia_tv\",\"platform\":\"android\",\"audioChannels\":\"5.1\",\"language\":\"sp\",\"acceptButton\":\"X\",\
\"connectedControllers\":[\"xinput\",\"ds3\",\"ds4\"],\"yuvCoefficient\":\"bt601\",\"videoEncoderProfile\":\"hw4.1\",\
\"audioEncoderProfile\":\"audio1\",\"adaptiveStreamMode\": \"resize\"}},\"userProfile\":{{\"onlineId\":\"psnId\",\
\"npId\":\"npId\",\"region\":\"US\",\"languagesUsed\":[\"en\",\"jp\"]}},\"videoCodec\":\"{codec}\",\
\"dynamicRange\":\"{range}\",\"handshakeKey\":\"{hk}\"}}",
        w = cfg.width,
        h = cfg.height,
        fps = cfg.fps,
        bw = cfg.bitrate_kbps,
        mtu = MTU,
        rtt = RTT_MS,
        hk = b64(handshake_key),
    )
}

/// Where a stream's output goes and its input comes from.
pub trait StreamSink {
    fn progress(&mut self, _p: Progress) {}
    /// One Annex-B access unit (`header` = codec parameter sets). False stops.
    fn video(&mut self, au: &[u8], header: bool) -> bool;
    /// One audio frame: its frame index (u16 BE) then one Opus packet; or, with
    /// `header`, the 14-byte audio header (channels, bits, rate, frame size).
    /// False stops.
    fn audio(&mut self, _frame: &[u8], _header: bool) -> bool {
        true
    }
    /// The next pad state the user produced, oldest first; None when none is waiting.
    fn input(&mut self) -> Option<ControllerState> {
        None
    }
    /// The console's rumble, 0-255 for the strong (left) and weak (right) motor.
    fn rumble(&mut self, _left: u8, _right: u8) {}
    /// The console's light-bar colour for this pad.
    fn light_bar(&mut self, _rgb: [u8; 3]) {}
    /// DualSense adaptive triggers (sessions with `dualsense` only): each trigger's effect
    /// mode and its 10 parameter bytes, as the pad's output report takes them.
    fn trigger_effects(&mut self, _left: TriggerEffect, _right: TriggerEffect) {}
    /// The console's DualSense effect strength settings, when they change: haptics and
    /// triggers, each 0 off, 1 strong, 2 medium, 3 weak (chiaki `ChiakiDualSenseEffectIntensity`).
    fn dualsense_intensity(&mut self, _haptics: u8, _triggers: u8) {}
    /// One 10 ms haptics frame: 3 kHz stereo s16le PCM (sessions with `dualsense` only).
    fn haptics(&mut self, _pcm: &[u8]) {}
    /// True once the consumer has gone; the stream then ends.
    fn stopped(&mut self) -> bool {
        false
    }
    /// True when the consumer needs a keyframe now (it joined mid-stream, or
    /// dropped frames to keep up); asked of the console at most every 500 ms.
    fn wants_keyframe(&mut self) -> bool {
        false
    }
    /// The page's microphone, oldest first (see mic.rs); None when nothing
    /// is waiting.
    fn mic(&mut self) -> Option<MicInput> {
        None
    }
}

/// Records the console's hold-off when a session ends, however it ends.
struct ReleaseHold {
    host: String,
    clean: std::cell::Cell<bool>,
}

impl Drop for ReleaseHold {
    fn drop(&mut self) {
        guard::hold_off(
            &self.host,
            if self.clean.get() {
                guard::RELEASE_AFTER_CLEAN_STOP
            } else {
                guard::RELEASE_AFTER_DROP
            },
        );
    }
}

/// Puts video packets back in order (chiaki-ng's video reorder queue): a
/// missing packet is waited for up to 16 ms (about a frame), then skipped.
#[derive(Default)]
struct VideoReorder {
    next: Option<u16>,
    waiting_since: Option<Instant>,
    buf: std::collections::HashMap<u16, takion::AvPacket>,
}

impl VideoReorder {
    const TIMEOUT: Duration = Duration::from_millis(16);
    const CAPACITY: usize = 64;

    fn push(&mut self, p: takion::AvPacket) {
        let next = *self
            .next
            .get_or_insert(p.packet_index.wrapping_sub(p.unit_index));
        if (p.packet_index.wrapping_sub(next) as i16) < 0 {
            return; // older than what was already handed on
        }
        self.buf.insert(p.packet_index, p);
    }

    fn pull(&mut self, now: Instant) -> Vec<takion::AvPacket> {
        let mut out = Vec::new();
        let Some(mut next) = self.next else {
            return out;
        };
        loop {
            if let Some(p) = self.buf.remove(&next) {
                out.push(p);
                next = next.wrapping_add(1);
                self.waiting_since = None;
                continue;
            }
            if self.buf.is_empty() {
                break;
            }
            let since = *self.waiting_since.get_or_insert(now);
            if now.duration_since(since) < Self::TIMEOUT && self.buf.len() < Self::CAPACITY {
                break;
            }
            // Give up on the gap: jump to the oldest packet we hold.
            next = *self
                .buf
                .keys()
                .min_by_key(|k| k.wrapping_sub(next))
                .expect("not empty");
            self.waiting_since = None;
        }
        self.next = Some(next);
        out
    }
}

/// What a finished stream run saw.
#[derive(Debug, Default, Clone)]
pub struct StreamReport {
    pub resolutions: Vec<(u32, u32)>,
    pub video: VideoStats,
    pub av_packets: u64,
    pub av_bad_mac: u64,
    pub audio_frames: u64,
    pub av_dropped_on_purpose: u64,
    /// Units the congestion reports covered, lost as seen, and lost as reported.
    pub units_seen: u64,
    pub loss_seen: u64,
    pub loss_reported: u64,
    pub bytes_received: u64,
    pub disconnect_reason: Option<String>,
    /// Our DATA chunks sent again because the console had not acknowledged them, and given up on.
    pub data_resent: u64,
    pub data_given_up: u64,
    /// Sends that failed for a passing reason and were carried on from.
    pub io_soft_errors: u64,
}

/// Nothing at all from the console for this long: it is gone (the network dropped, or it
/// ended the stream without a word). Video and audio packets arrive many times a second even
/// on a still picture, so a healthy stream is never this quiet.
const SILENCE_LIMIT: Duration = Duration::from_secs(15);
/// After ctrl closes, this long for the console's DISCONNECT (and its reason) to arrive.
const CTRL_GONE_GRACE: Duration = Duration::from_secs(3);

/// Open a stream and run it into `sink` (video, audio, status; pad input back)
/// until the sink stops, `max` time passes, or the console disconnects.
pub fn run_stream(
    host: &str,
    creds: &SessionCreds,
    cfg: StreamConfig,
    max: Duration,
    sink: &mut impl StreamSink,
) -> Result<StreamReport> {
    // Readiness without touching the Remote Play slot (see guard.rs).
    guard::prepare(host, creds, cfg.wake, Duration::from_secs(60), &mut || {
        sink.progress(Progress::Waking)
    })?;
    let nonce = match session_init(host, &creds.regist_key) {
        Ok(n) => n,
        Err(e) if format!("{e:#}").contains(guard::REASON_IN_USE) => {
            guard::hold_off(host, guard::IN_USE_BACKOFF);
            return Err(guard::NotReady::Busy(guard::IN_USE_BACKOFF).into());
        }
        Err(e) => return Err(e),
    };
    // From here the console holds a slot for us: whatever happens next, do
    // not ask again until it has had time to let go.
    let _release = ReleaseHold {
        host: host.to_string(),
        clean: std::cell::Cell::new(false),
    };
    sink.progress(Progress::Nonce);
    // Take the nonce once, then retry only the ctrl connection while the console
    // refuses or resets it (rp_session's rule: a second session-init would
    // re-reserve the slot and lock us out). Seen from the engine on the Pro, FW
    // 13.60: "Connection refused" on ctrl right after the nonce, intermittently.
    let mut attempt = 0;
    let opened = loop {
        attempt += 1;
        match ctrl_open(host, creds, &nonce, cfg.codec, cfg.mic || cfg.dualsense) {
            Err(e)
                if attempt < CTRL_ATTEMPTS
                    && (format!("{e:#}").contains("ctrl reply")
                        || format!("{e:#}").contains("connecting for ctrl")) =>
            {
                std::thread::sleep(Duration::from_secs(1));
            }
            r => break r,
        }
    };
    let (mut ctrl, session_id, brt, amb) = match opened {
        Ok(c) => c,
        // The console gave a nonce, then refused or reset ctrl: it is booting
        // (rule 2 in guard.rs) or still letting go of a session. Seen on the
        // Pro awake (DDP 200) 2.5 min after a clean stop; fine again ~5 min
        // later. Same answer as "in use": wait, do not hammer it.
        Err(e)
            if format!("{e:#}").contains("ctrl reply")
                || format!("{e:#}").contains("connecting for ctrl") =>
        {
            guard::hold_off(host, guard::IN_USE_BACKOFF);
            // Keep the console's own words: the user sees "busy", a log the cause.
            return Err(e.context(guard::NotReady::Busy(guard::IN_USE_BACKOFF)));
        }
        Err(e) => return Err(e),
    };
    sink.progress(Progress::SessionId);
    // DualSense features on, the mic muted twice (chiaki `ctrl_enable_features`);
    // like chiaki, a failed send is not fatal.
    for (kind, payload) in start_ctrl_messages(cfg.mic, cfg.dualsense) {
        let _ = ctrl.send(kind, &payload);
    }

    let mut takion = Takion::connect(resolve(host, STREAM_PORT)?)?;
    sink.progress(Progress::TakionConnected);

    let mut handshake_key = [0u8; HANDSHAKE_KEY_SIZE];
    getrandom::fill(&mut handshake_key).map_err(|e| anyhow!("random: {e}"))?;
    let ecdh = Ecdh::new()?;
    let (pub_key, sig) = ecdh.public_key_and_sig(&handshake_key);

    // The launch spec is XORed with the ctrl cipher's keystream at counter 0
    // (chiaki encrypts zeros, then XORs), NUL terminator included.
    let mut spec = launch_spec(&cfg, &handshake_key).into_bytes();
    spec.push(0);
    let ks = auth_encrypt(&brt, &amb, 0, &vec![0u8; spec.len()]);
    for (b, k) in spec.iter_mut().zip(ks) {
        *b ^= k;
    }
    let spec_b64 = b64(&spec);
    let big = proto::big(&proto::Big {
        client_version: TAKION_VERSION_PS5,
        session_key: &session_id,
        launch_spec_b64: &spec_b64,
        ecdh_pub_key: &pub_key,
        ecdh_sig: &sig,
    });
    takion.send_message(1, &big)?;

    let mut report = StreamReport::default();
    let mut early_streaminfo: Option<Vec<u8>> = None;
    let mut have_bang = false;
    let mut stream_info: Option<proto::StreamInfo> = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while stream_info.is_none() {
        if Instant::now() > deadline {
            bail!(if have_bang {
                "the console never sent STREAMINFO"
            } else {
                "the console never answered BIG with BANG"
            });
        }
        if have_bang {
            if let Some(buf) = early_streaminfo.take() {
                if let Message::StreamInfo(si) = proto::decode(&buf)? {
                    stream_info = Some(si);
                    continue;
                }
            }
        }
        let Some(ev) = takion.poll(Duration::from_millis(200))? else {
            continue;
        };
        let Event::Data(DATA_TYPE_PROTOBUF, buf) = ev else {
            continue;
        };
        match proto::decode(&buf)? {
            Message::Bang(b) if !have_bang => {
                if !b.version_accepted {
                    bail!("the console did not accept Takion version {TAKION_VERSION_PS5}");
                }
                if !b.encrypted_key_accepted {
                    bail!("the console did not accept our key");
                }
                let secret = ecdh.derive(&b.ecdh_pub_key, &b.ecdh_sig, &handshake_key)?;
                takion.gk_local = Some(GkCrypt::new(2, &handshake_key, &secret));
                takion.gk_remote = Some(GkCrypt::new(3, &handshake_key, &secret));
                have_bang = true;
                sink.progress(Progress::Bang);
            }
            Message::StreamInfo(si) if have_bang => stream_info = Some(si),
            Message::StreamInfo(_) => early_streaminfo = Some(buf),
            Message::Disconnect(r) => bail!("the console ended the stream: {r}"),
            _ => {}
        }
    }
    let si = stream_info.unwrap();
    report.resolutions = si.resolutions.iter().map(|r| (r.width, r.height)).collect();
    sink.progress(Progress::StreamInfo(report.resolutions.clone()));
    for (channel, msg) in after_streaminfo_messages(cfg.mic, cfg.dualsense) {
        takion.send_message(channel, &msg)?;
    }
    let mut mic_sender = mic::AudioSender::new();
    let (mut mic_connected, mut mic_live) = (false, false);
    if !si.audio_header.is_empty() {
        sink.audio(&si.audio_header, true);
    }
    let mut audio = AudioAssembler::default();
    let mut haptics = HapticsAssembler::default();
    let mut intensity: Option<(u8, u8)> = None;

    let mut assembler = FrameAssembler::new();
    let mut profile: Option<u8> = None;
    let mut first_frame = true;
    let started = Instant::now();
    let mut last_heartbeat = Instant::now();
    let mut feedback = FeedbackSender::new();
    let mut pad_queue: std::collections::VecDeque<ControllerState> = Default::default();
    let mut applied = ControllerState::default();
    let mut last_press = Instant::now();
    let mut reorder = VideoReorder::default();
    let mut last_congestion = Instant::now();
    let mut last_stats = Instant::now();
    let mut stats_prev = VideoStats::default();
    let (mut second_units, mut second_units_lost) = (0u64, 0u64);
    let mut last_idr: Option<Instant> = None;
    let (mut state_seq, mut history_seq) = (0u16, 0u16);
    let mut ctrl_gone: Option<Instant> = None;
    let result = loop {
        if started.elapsed() > max || sink.stopped() {
            break Ok(());
        }
        // Before, a console that went quiet, or closed ctrl, left the page on a frozen
        // picture until the four-hour limit: end the stream and say why.
        if takion.last_rx.elapsed() > SILENCE_LIMIT {
            break Err(anyhow!(
                "the console stopped sending: nothing for {} s (the network dropped, or the console ended the stream)",
                SILENCE_LIMIT.as_secs()
            ));
        }
        if ctrl.ended() {
            let since = *ctrl_gone.get_or_insert_with(Instant::now);
            if since.elapsed() > CTRL_GONE_GRACE {
                break Err(anyhow!(
                    "the console closed the Remote Play control connection"
                ));
            }
        }
        if last_heartbeat.elapsed() >= Duration::from_secs(1) {
            last_heartbeat = Instant::now();
            takion.send_message(1, &proto::bare(proto::kind::HEARTBEAT))?;
        }
        // Every pad state, in order. A press is held for at least MIN_HOLD before
        // its release is applied: the console reads the pad about once a frame,
        // and a tap whose press and release land in one frame never registers
        // (seen: 3 quick taps moved the home screen 1 tile).
        while let Some(pad) = sink.input() {
            pad_queue.push_back(pad);
        }
        while let Some(next) = pad_queue.front().copied() {
            let releases = applied.buttons & !next.buttons != 0
                || (applied.touch.is_some() && next.touch.is_none());
            if releases && last_press.elapsed() < MIN_HOLD {
                break;
            }
            if next.buttons & !applied.buttons != 0
                || (applied.touch.is_none() && next.touch.is_some())
            {
                last_press = Instant::now();
            }
            feedback.set(next);
            applied = next;
            pad_queue.pop_front();
        }
        // The microphone (chiaki StreamSession::ToggleMute and the audio
        // sender): CONNECT once, then unmute/mute; frames only while live.
        while let Some(m) = if cfg.mic { sink.mic() } else { None } {
            match m {
                MicInput::On if !mic_live => {
                    if !mic_connected {
                        mic_connected = true;
                        let _ = ctrl.send(mic::CTRL_MIC_CONNECT, &mic::connect_payload());
                    }
                    let _ = ctrl.send(mic::CTRL_MIC_TOGGLE, &mic::toggle_payload(true));
                    mic_live = true;
                }
                MicInput::Off if mic_live => {
                    let _ = ctrl.send(mic::CTRL_MIC_TOGGLE, &mic::toggle_payload(false));
                    mic_live = false;
                }
                MicInput::Frame(f) if mic_live => {
                    if let Some(mut pkt) = mic_sender.opus_data(&f, true) {
                        // chiaki logs a failed send and carries on.
                        let _ = takion.send_mic(&mut pkt, true);
                    }
                }
                _ => {}
            }
        }
        for pkt in feedback.tick(Instant::now()) {
            match pkt {
                feedback::Packet::State(p) => {
                    takion.send_feedback(6, state_seq, 1, &p)?;
                    state_seq = state_seq.wrapping_add(1);
                }
                feedback::Packet::History(n, p) => {
                    takion.send_feedback(1, history_seq, n, &p)?;
                    history_seq = history_seq.wrapping_add(1);
                }
            }
        }
        // Every 200 ms: what arrived and what did not (chiaki's congestion
        // control). Reported loss is capped at 5%, as chiaki does by default:
        // the console cuts its bitrate on what it is told.
        if last_stats.elapsed() >= Duration::from_secs(1) {
            last_stats = Instant::now();
            let now = assembler.stats;
            sink.progress(Progress::Stats(LiveStats {
                frames: now.frames - stats_prev.frames,
                frames_rebuilt: now.frames_recovered - stats_prev.frames_recovered,
                frames_lost: now.frames_dropped - stats_prev.frames_dropped,
                video_bytes: now.bytes - stats_prev.bytes,
                units: std::mem::take(&mut second_units),
                units_lost: std::mem::take(&mut second_units_lost),
            }));
            stats_prev = now;
        }
        if last_congestion.elapsed() >= Duration::from_millis(200) {
            last_congestion = Instant::now();
            let (vr, vl) = assembler.take_generation();
            let (ar, al) = audio.take_seq();
            let (mut received, mut lost) = (vr + ar, vl + al);
            let total = received + lost;
            if total > 0 && lost * 100 > total * 5 {
                lost = total * 5 / 100;
                received = total - lost;
            }
            report.loss_reported += lost;
            report.loss_seen += vl + al;
            report.units_seen += total;
            second_units += total;
            second_units_lost += vl + al;
            takion.send_congestion(
                received.min(u16::MAX as u64) as u16,
                lost.min(u16::MAX as u64) as u16,
            )?;
        }
        if sink.wants_keyframe()
            && last_idr.is_none_or(|t: Instant| t.elapsed() > Duration::from_millis(500))
        {
            last_idr = Some(Instant::now());
            takion.send_message(2, &proto::bare(proto::kind::IDRREQUEST))?;
        }
        // Video packets in order (late ones waited for briefly) into frames.
        let mut stop = false;
        for p in reorder.pull(Instant::now()) {
            for out in assembler.push(&p) {
                match out {
                    video::Output::Frame(frame) => {
                        if first_frame {
                            first_frame = false;
                            sink.progress(Progress::FirstFrame(frame.len()));
                        }
                        if !sink.video(&frame, false) {
                            stop = true;
                        }
                    }
                    video::Output::Lost { from, to } => {
                        // Tell the console which frames are gone, and ask for a
                        // keyframe so the picture repairs itself quickly.
                        takion.send_message(2, &proto::corrupt_frame(from, to))?;
                        if last_idr
                            .is_none_or(|t: Instant| t.elapsed() > Duration::from_millis(500))
                        {
                            last_idr = Some(Instant::now());
                            takion.send_message(2, &proto::bare(proto::kind::IDRREQUEST))?;
                        }
                    }
                }
            }
        }
        if stop {
            break Ok(());
        }
        // Short waits: input is polled between packets, and video packets
        // arrive every few milliseconds anyway.
        let ev = match takion.poll(Duration::from_millis(4)) {
            Ok(Some(ev)) => ev,
            Ok(None) => continue,
            Err(e) => break Err(e),
        };
        match ev {
            Event::Data(DATA_TYPE_PROTOBUF, buf) => {
                if let Ok(Message::Disconnect(r)) = proto::decode(&buf) {
                    report.disconnect_reason = Some(r);
                    break Ok(());
                }
            }
            // Rumble: [?, left (strong), right (weak)] (chiaki
            // stream_connection_takion_data_rumble).
            Event::Data(DATA_TYPE_RUMBLE, b) if b.len() >= 3 => sink.rumble(b[1], b[2]),
            // Pad info carries the light-bar colour: at 1..4 in the 0x11-byte
            // form, 9..12 in the 0x19-byte one (chiaki ..._data_pad_info).
            Event::Data(DATA_TYPE_PAD_INFO, b) => {
                match b.len() {
                    0x11 => sink.light_bar([b[1], b[2], b[3]]),
                    0x19 => sink.light_bar([b[9], b[10], b[11]]),
                    _ => {}
                }
                // The effect strengths matter only to a DualSense.
                if let Some(i) = pad_info_intensity(&b).filter(|_| cfg.dualsense) {
                    if intensity != Some(i) {
                        intensity = Some(i);
                        sink.dualsense_intensity(i.0, i.1);
                    }
                }
            }
            Event::Data(DATA_TYPE_TRIGGER_EFFECTS, b) => {
                if let Some((l, r)) = parse_trigger_effects(&b) {
                    sink.trigger_effects(l, r);
                }
            }
            Event::Data(..) => {}
            Event::Av(p) if p.is_video => {
                // The codec header goes out before the first frame of each profile.
                if profile != Some(p.adaptive_stream_index) {
                    let Some(r) = si.resolutions.get(p.adaptive_stream_index as usize) else {
                        continue;
                    };
                    profile = Some(p.adaptive_stream_index);
                    if !sink.video(&r.video_header, true) {
                        break Ok(());
                    }
                }
                reorder.push(p);
            }
            Event::Av(p) if p.is_haptics => {
                for frame in haptics.push(&p) {
                    sink.haptics(&frame);
                }
            }
            Event::Av(p) => {
                for (idx, frame) in audio.push(&p) {
                    let mut f = Vec::with_capacity(2 + frame.len());
                    f.extend_from_slice(&idx.to_be_bytes());
                    f.extend_from_slice(&frame);
                    if !sink.audio(&f, false) {
                        break;
                    }
                }
            }
        }
    };
    // Clean stop, in chiaki's order: DISCONNECT over Takion, then close ctrl.
    takion.close();
    drop(ctrl);
    _release.clean.set(true);
    report.video = assembler.stats;
    report.av_packets = takion.av_received;
    report.av_bad_mac = takion.av_bad_mac;
    report.audio_frames = audio.frames;
    report.av_dropped_on_purpose = takion.av_dropped_on_purpose;
    report.bytes_received = takion.bytes_received;
    report.data_resent = takion.data_resent;
    report.data_given_up = takion.data_given_up;
    report.io_soft_errors = takion.io_soft_errors;
    result.map(|_| report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_spec_is_valid_json() {
        let s = launch_spec(&StreamConfig::default(), &[0u8; 16]);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["videoCodec"], "avc");
        assert_eq!(v["network"]["mtu"], 1454);
        assert_eq!(v["streamResolutions"][0]["resolution"]["width"], 1280);
        assert_eq!(v["dynamicRange"], "SDR");
    }

    /// HDR is H.265 with `"dynamicRange":"HDR"` (chiaki-ng launchspec.c); plain H.265 stays SDR.
    #[test]
    fn launch_spec_asks_for_hdr_only_with_the_hdr_codec() {
        let spec = |codec| {
            let cfg = StreamConfig {
                codec,
                ..Default::default()
            };
            serde_json::from_str::<serde_json::Value>(&launch_spec(&cfg, &[0u8; 16])).unwrap()
        };
        let hdr = spec(Codec::H265Hdr);
        assert_eq!(hdr["videoCodec"], "hevc");
        assert_eq!(hdr["dynamicRange"], "HDR");
        let sdr = spec(Codec::H265);
        assert_eq!(sdr["videoCodec"], "hevc");
        assert_eq!(sdr["dynamicRange"], "SDR");
    }

    #[test]
    fn codec_names_and_hevc() {
        assert_eq!(Codec::from_name("h264"), Some(Codec::H264));
        assert_eq!(Codec::from_name("h265"), Some(Codec::H265));
        assert_eq!(Codec::from_name("h265_hdr"), Some(Codec::H265Hdr));
        assert_eq!(Codec::from_name("vp9"), None);
        assert!(Codec::H265Hdr.is_hevc() && Codec::H265.is_hevc() && !Codec::H264.is_hevc());
    }

    /// A session without the mic sends exactly what it did before the mic
    /// existed (bytes recorded from the pre-mic code): nothing extra on ctrl,
    /// STREAMINFOACK on channel 9 and the controller on channel 1, then nothing.
    #[test]
    fn without_the_mic_the_setup_is_unchanged() {
        assert!(start_ctrl_messages(false, false).is_empty());
        assert_eq!(
            after_streaminfo_messages(false, false),
            vec![
                (9, vec![0x08, 0x0e]),
                (
                    1,
                    vec![0x08, 0x15, 0xb2, 0x01, 0x04, 0x10, 0x01, 0x18, 0x02]
                ),
            ]
        );
        // The launch spec does not depend on the mic.
        let on = StreamConfig {
            mic: true,
            ..Default::default()
        };
        assert_eq!(
            launch_spec(&on, &[0u8; 16]),
            launch_spec(&StreamConfig::default(), &[0u8; 16])
        );
        assert!(!StreamConfig::default().mic);
        assert!(!StreamConfig::default().dualsense);
    }

    /// With a DualSense: chiaki's two ctrl messages first, and the controller is type 6.
    #[test]
    fn with_a_dualsense_the_console_is_told_so() {
        let ctrl = start_ctrl_messages(false, true);
        assert_eq!(ctrl[0], (0x13, vec![0x00, 0x40, 0x00]));
        assert_eq!(ctrl[1].0, 0x11);
        assert_eq!(ctrl[1].1.len(), 0x10);
        assert_eq!(&ctrl[1].1[..6], &[0xa0, 0xab, 0x51, 0xbd, 0xd1, 0x7e]);
        assert_eq!(ctrl.len(), 2);
        // With the mic as well: the mic's mutes come after.
        let both = start_ctrl_messages(true, true);
        assert_eq!(&both[..2], &ctrl[..]);
        assert_eq!(&both[2..], &start_ctrl_messages(true, false)[..]);
        let tk = after_streaminfo_messages(false, true);
        assert_eq!(
            tk[1],
            (
                1,
                vec![0x08, 0x15, 0xb2, 0x01, 0x04, 0x10, 0x01, 0x18, 0x06]
            )
        );
    }

    #[test]
    fn trigger_effects_and_intensity_parse_like_chiaki() {
        let mut b = vec![0u8; 25];
        b[1] = 0x21;
        b[2] = 0x26;
        for i in 0..10 {
            b[5 + i] = i as u8;
            b[15 + i] = 0x10 + i as u8;
        }
        let (l, r) = parse_trigger_effects(&b).unwrap();
        assert_eq!(l, (0x21, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]));
        assert_eq!(r.0, 0x26);
        assert_eq!(r.1[9], 0x19);
        assert!(parse_trigger_effects(&b[..24]).is_none());
        let mut p = vec![0u8; 0x19];
        p[20] = 3;
        p[21] = 2;
        assert_eq!(pad_info_intensity(&p), Some((3, 2)));
        let mut p = vec![0u8; 0x11];
        p[12] = 1;
        assert_eq!(pad_info_intensity(&p), Some((1, 0)));
        assert_eq!(pad_info_intensity(&[0u8; 5]), None);
    }

    #[test]
    fn with_the_mic_setup_adds_chiaki_mutes_and_audio_header() {
        let ctrl = start_ctrl_messages(true, false);
        assert_eq!(
            ctrl,
            vec![(0x36, vec![0, 1, 1, 89]), (0x36, vec![0, 1, 1, 89])]
        );
        let tk = after_streaminfo_messages(true, false);
        assert_eq!(&tk[..2], &after_streaminfo_messages(false, false)[..]);
        assert_eq!(tk[2].0, 1);
        assert_eq!(&tk[2].1[..6], &[0x08, 0x0d, 0x7a, 0x10, 0x12, 0x0e]);
    }

    #[test]
    fn ctrl_message_layout_matches_chiaki() {
        let (brt, amb) = ([3u8; 16], [5u8; 16]);
        // ctrl_message_toggle_microphone(ctrl, false), sent as the sixth
        // payload (counter 5, after the five auth headers).
        let m = ctrl_message(0x36, &[0, 1, 1, 89], &brt, &amb, 5);
        assert_eq!(&m[..8], &[0, 0, 0, 4, 0x00, 0x36, 0, 0]);
        assert_eq!(&m[8..], &auth_encrypt(&brt, &amb, 5, &[0, 1, 1, 89])[..]);
        assert_eq!(auth_decrypt(&brt, &amb, 5, &m[8..]), vec![0, 1, 1, 89]);
        // A different counter is a different ciphertext.
        assert_ne!(ctrl_message(0x36, &[0, 1, 1, 89], &brt, &amb, 6), m);
    }
}
