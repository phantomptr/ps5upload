// SPDX-License-Identifier: AGPL-3.0-only
// Ported from chiaki-ng: Copyright (C) the Chiaki and chiaki-ng authors. Rust port for
// ps5upload. Licensed under the GNU AGPL v3.0 (LICENSES/AGPL-3.0.txt).
//! Takion: Remote Play's SCTP-like transport over UDP 9296.
//!
//! Ported from chiaki-ng (AGPL-3.0, `lib/src/takion.c`). Every datagram's low
//! nibble is its kind: control (an SCTP-style chunk: INIT, COOKIE, DATA,
//! DATA_ACK), video, audio, congestion report, controller feedback. Until the
//! stream's key exchange completes nothing is authenticated; afterwards each
//! packet carries a 4-byte GMAC and a key position, and audio/video payloads
//! are encrypted with the console's GKCrypt.
//!
//! This is the spike version: one thread, blocking reads with a timeout, data
//! messages delivered in order, video delivered as it arrives (the frame
//! assembler orders units itself).

use std::collections::BTreeMap;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use super::crypto::{GkCrypt, KeyState, BLOCK, GMAC_SIZE};

const A_RWND: u32 = 0x19000;
const STREAMS: u16 = 0x64;
const HEADER: usize = 0x10;
const COOKIE: usize = 0x20;

pub const PACKET_CONTROL: u8 = 0;
pub const PACKET_VIDEO: u8 = 2;
pub const PACKET_AUDIO: u8 = 3;
pub const PACKET_CONGESTION: u8 = 5;

const CHUNK_DATA: u8 = 0;
const CHUNK_INIT: u8 = 1;
const CHUNK_INIT_ACK: u8 = 2;
const CHUNK_DATA_ACK: u8 = 3;
const CHUNK_COOKIE: u8 = 0xa;
const CHUNK_COOKIE_ACK: u8 = 0xb;

pub const DATA_TYPE_PROTOBUF: u8 = 0;
pub const DATA_TYPE_RUMBLE: u8 = 7;
pub const DATA_TYPE_PAD_INFO: u8 = 9;
/// DualSense adaptive triggers (chiaki `CHIAKI_TAKION_MESSAGE_DATA_TYPE_TRIGGER_EFFECTS`).
pub const DATA_TYPE_TRIGGER_EFFECTS: u8 = 11;

fn mac_offset(kind: u8) -> Option<usize> {
    match kind {
        PACKET_CONTROL => Some(5),
        PACKET_VIDEO | PACKET_AUDIO => Some(0xa),
        PACKET_CONGESTION => Some(7),
        _ => None,
    }
}

fn key_pos_offset(kind: u8) -> Option<usize> {
    match kind {
        PACKET_CONTROL => Some(9),
        PACKET_VIDEO | PACKET_AUDIO => Some(0xe),
        PACKET_CONGESTION => Some(0xb),
        _ => None,
    }
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}
fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn seq32_lt(a: u32, b: u32) -> bool {
    a != b && (a.wrapping_sub(b) as i32) < 0
}

/// One video or audio packet, payload decrypted.
#[derive(Debug, Clone)]
pub struct AvPacket {
    pub is_video: bool,
    pub packet_index: u16,
    pub frame_index: u16,
    pub unit_index: u16,
    pub units_in_frame_total: u16,
    pub units_in_frame_fec: u16,
    pub codec: u8,
    pub adaptive_stream_index: u8,
    /// An audio packet that carries the DualSense's haptics (v12 flag byte 0x02), not sound.
    pub is_haptics: bool,
    pub key_pos: u64,
    pub data: Vec<u8>,
}

pub enum Event {
    /// A data message from the console, in order: its data type (0 protobuf
    /// `TakionMessage`, 7 rumble, 9 pad info, 11 trigger effects) and body.
    Data(u8, Vec<u8>),
    Av(AvPacket),
}

pub struct Takion {
    sock: UdpSocket,
    tag_local: u32,
    tag_remote: u32,
    seq_local: u32,
    key_pos_local: u64,
    pub gk_local: Option<GkCrypt>,
    pub gk_remote: Option<GkCrypt>,
    key_state: KeyState,
    /// Next DATA sequence number we owe the application, and early arrivals.
    seq_remote_next: u32,
    data_early: BTreeMap<u32, (u8, Vec<u8>)>,
    /// AV packets that arrived before the console's crypt was ready.
    postponed: Vec<Vec<u8>>,
    pub av_received: u64,
    pub av_bad_mac: u64,
    pub bytes_received: u64,
    /// Test knob: drop this share (0-100) of received video packets
    /// (`PS5UPLOAD_RP_DROP_PCT`), to exercise FEC and loss recovery.
    drop_pct: u32,
    rng: u64,
    pub av_dropped_on_purpose: u64,
    closed: bool,
    /// Our DATA chunks the console has not acknowledged, resent until it does.
    send_buffer: SendBuffer,
    /// DATA chunks sent again, and given up on after `SendBuffer::TRIES_MAX` sends.
    pub data_resent: u64,
    pub data_given_up: u64,
    /// Sends and receives that failed for a reason that passes (an ICMP error, a full buffer).
    pub io_soft_errors: u64,
    /// When the console last sent anything.
    pub last_rx: Instant,
    /// Test knob (`PS5UPLOAD_RP_DROP_OUT_DATA`): `N` loses the first copy of our DATA chunk
    /// number N after the crypt is up, as the network might; `Nall` loses every copy of it,
    /// which is what any lost chunk was before resends.
    drop_out_data: Option<(u64, bool)>,
    data_sent: u64,
    /// The round trip to the console and back, smoothed: our DATA chunk (a heartbeat each
    /// second) out, its DATA_ACK in. None until the first sample.
    rtt: Option<Duration>,
}

/// Our DATA chunks waiting for the console's DATA_ACK (chiaki-ng `takionsendbuffer.c`).
///
/// Takion delivers DATA in order, so one lost chunk holds back every later one: without a
/// resend, a single lost heartbeat starves the console of all of them and it ends the stream a
/// little later. chiaki-ng keeps 16, resends each 200 ms until acknowledged, up to 10 times.
#[derive(Default)]
struct SendBuffer {
    packets: Vec<Unacked>,
}

struct Unacked {
    seq: u32,
    pkt: Vec<u8>,
    sent: Instant,
    tries: u32,
}

impl SendBuffer {
    const SIZE: usize = 16;
    const RESEND_AFTER: Duration = Duration::from_millis(200);
    const TRIES_MAX: u32 = 10;

    /// Keep `pkt` (sent once at `now`) until acknowledged. False when full: it is not kept,
    /// as chiaki-ng does.
    fn push(&mut self, seq: u32, pkt: Vec<u8>, now: Instant) -> bool {
        if self.packets.len() >= Self::SIZE {
            return false;
        }
        self.packets.push(Unacked {
            seq,
            pkt,
            sent: now,
            tries: 1,
        });
        true
    }

    /// The console has every chunk up to `cumulative`: forget them. Returns how many, and the
    /// round trip of the chunk the ack names when it was sent only once (Karn's rule: an ack
    /// for a resent chunk could answer either copy). Since a chunk is resent after 200 ms, a
    /// sample is always shorter than that.
    fn ack(&mut self, cumulative: u32, now: Instant) -> (usize, Option<Duration>) {
        let rtt = self
            .packets
            .iter()
            .find(|p| p.seq == cumulative && p.tries == 1)
            .map(|p| now.saturating_duration_since(p.sent));
        let before = self.packets.len();
        self.packets
            .retain(|p| p.seq != cumulative && !seq32_lt(p.seq, cumulative));
        (before - self.packets.len(), rtt)
    }

    /// The packets to send again now, and how many were given up on.
    fn due(&mut self, now: Instant) -> (Vec<Vec<u8>>, u64) {
        let mut out = Vec::new();
        let mut given_up = 0;
        self.packets.retain_mut(|p| {
            if now.duration_since(p.sent) < Self::RESEND_AFTER {
                return true;
            }
            if p.tries >= Self::TRIES_MAX {
                given_up += 1;
                return false;
            }
            p.tries += 1;
            p.sent = now;
            out.push(p.pkt.clone());
            true
        });
        (out, given_up)
    }
}

/// A new round-trip sample folded into the last value: a quarter of the way, so one late ack
/// moves it little (the page's own round trip is smoothed the same way).
fn smooth_rtt(prev: Option<Duration>, sample: Duration) -> Duration {
    match prev {
        None => sample,
        Some(p) => (p * 3 + sample) / 4,
    }
}

/// `PS5UPLOAD_RP_DROP_OUT_DATA`: `N` or `Nall`.
fn drop_knob(v: &str) -> Option<(u64, bool)> {
    let v = v.trim();
    match v.strip_suffix("all") {
        Some(n) => n.parse().ok().map(|n| (n, true)),
        None => v.parse().ok().map(|n| (n, false)),
    }
}

/// A send or receive error that does not end the stream. Windows reports an ICMP "port
/// unreachable" from an earlier send as WSAECONNRESET on the next receive of a UDP socket, and
/// a Wi-Fi blip as "host/network unreachable" or no buffer space; the stream carries on, and
/// the silence watchdog in `run_stream` ends it if the console really is gone.
fn soft_io_error(e: &std::io::Error) -> bool {
    use std::io::ErrorKind as K;
    matches!(
        e.kind(),
        K::WouldBlock
            | K::TimedOut
            | K::Interrupted
            | K::ConnectionReset
            | K::ConnectionRefused
            | K::HostUnreachable
            | K::NetworkUnreachable
            | K::NetworkDown
    ) || matches!(e.raw_os_error(), Some(code) if is_no_buffer_space(code))
}

/// ENOBUFS (55 on macOS/BSD, 105 on Linux) or WSAENOBUFS (10055).
fn is_no_buffer_space(code: i32) -> bool {
    #[cfg(unix)]
    {
        code == libc::ENOBUFS
    }
    #[cfg(windows)]
    {
        code == 10055
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = code;
        false
    }
}

impl Takion {
    /// Connect and run the INIT / COOKIE handshake.
    pub fn connect(addr: SocketAddr) -> Result<Self> {
        let bind: SocketAddr = if addr.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        }
        .parse()
        .unwrap();
        let sock = UdpSocket::bind(bind).context("binding the stream socket")?;
        let granted = set_recv_buffer(&sock, RECV_BUFFER);
        crate::core_log!(
            "rp_stream: UDP receive buffer {} KiB asked, {} granted",
            RECV_BUFFER >> 10,
            granted.map_or("none reported".to_string(), |g| format!("{} KiB", g >> 10))
        );
        sock.connect(addr).context("connecting the stream socket")?;
        let mut tag = [0u8; 4];
        getrandom::fill(&mut tag).map_err(|e| anyhow!("random: {e}"))?;
        let tag_local = u32::from_le_bytes(tag) | 1;
        let mut t = Self {
            sock,
            tag_local,
            tag_remote: 0,
            seq_local: tag_local,
            key_pos_local: 0,
            gk_local: None,
            gk_remote: None,
            key_state: KeyState::default(),
            seq_remote_next: 0,
            data_early: BTreeMap::new(),
            postponed: Vec::new(),
            av_received: 0,
            av_bad_mac: 0,
            bytes_received: 0,
            drop_pct: std::env::var("PS5UPLOAD_RP_DROP_PCT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
                .min(100),
            rng: tag_local as u64 | 1,
            av_dropped_on_purpose: 0,
            closed: false,
            send_buffer: SendBuffer::default(),
            data_resent: 0,
            data_given_up: 0,
            io_soft_errors: 0,
            last_rx: Instant::now(),
            drop_out_data: std::env::var("PS5UPLOAD_RP_DROP_OUT_DATA")
                .ok()
                .and_then(|v| drop_knob(&v)),
            data_sent: 0,
            rtt: None,
        };
        t.handshake()?;
        Ok(t)
    }

    fn header(
        &self,
        tag: u32,
        key_pos: u64,
        chunk: u8,
        flags: u8,
        data_len: usize,
    ) -> [u8; HEADER] {
        let mut h = [0u8; HEADER];
        h[0..4].copy_from_slice(&tag.to_be_bytes());
        // 4..8: GMAC, filled in at send.
        h[8..12].copy_from_slice(&(key_pos as u32).to_be_bytes());
        h[0xc] = chunk;
        h[0xd] = flags;
        h[0xe..0x10].copy_from_slice(&((data_len + 4) as u16).to_be_bytes());
        h
    }

    fn recv_raw(&self, buf: &mut [u8], timeout: Duration) -> Result<Option<usize>> {
        self.sock
            .set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        match self.sock.recv(buf) {
            Ok(n) => Ok(Some(n)),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            // Gone by the next receive: the stream carries on.
            Err(e) if soft_io_error(&e) => Ok(None),
            Err(e) => Err(anyhow!("receiving from the console: {e}")),
        }
    }

    fn handshake(&mut self) -> Result<()> {
        // INIT ->
        let mut init = vec![PACKET_CONTROL];
        init.extend_from_slice(&self.header(0, 0, CHUNK_INIT, 0, 0x10));
        init.extend_from_slice(&self.tag_local.to_be_bytes());
        init.extend_from_slice(&A_RWND.to_be_bytes());
        init.extend_from_slice(&STREAMS.to_be_bytes());
        init.extend_from_slice(&STREAMS.to_be_bytes());
        init.extend_from_slice(&self.seq_local.to_be_bytes());

        let mut buf = [0u8; 1500];
        let mut cookie = None;
        for _ in 0..3 {
            self.sock.send(&init)?;
            if let Some(n) = self.recv_raw(&mut buf, Duration::from_secs(5))? {
                let pkt = &buf[..n];
                if n < 1 + HEADER + 0x10 + COOKIE || pkt[0] != PACKET_CONTROL {
                    bail!("expected INIT_ACK, got {n} bytes of kind {:#x}", pkt[0]);
                }
                let (chunk, payload) = self.parse_control(pkt)?;
                if chunk != CHUNK_INIT_ACK {
                    bail!("expected INIT_ACK, got chunk {chunk:#x}");
                }
                self.tag_remote = be32(&payload[0..4]);
                if self.tag_remote == 0 {
                    bail!("the console's INIT_ACK carried tag 0");
                }
                let mut c = [0u8; COOKIE];
                c.copy_from_slice(&payload[0x10..0x10 + COOKIE]);
                cookie = Some(c);
                break;
            }
        }
        let cookie = cookie
            .ok_or_else(|| anyhow!("the console did not answer the stream INIT (UDP 9296)"))?;
        // chiaki uses the console's tag as its initial DATA sequence number.
        self.seq_remote_next = self.tag_remote;

        // COOKIE ->
        let mut msg = vec![PACKET_CONTROL];
        msg.extend_from_slice(&self.header(self.tag_remote, 0, CHUNK_COOKIE, 0, COOKIE));
        msg.extend_from_slice(&cookie);
        for _ in 0..3 {
            self.sock.send(&msg)?;
            let deadline = Instant::now() + Duration::from_secs(5);
            while let Some(n) =
                self.recv_raw(&mut buf, deadline.saturating_duration_since(Instant::now()))?
            {
                let pkt = &buf[..n];
                if pkt[0] != PACKET_CONTROL {
                    continue;
                }
                match self.parse_control(pkt) {
                    Ok((CHUNK_COOKIE_ACK, _)) => return Ok(()),
                    Ok((CHUNK_INIT_ACK, _)) => continue, // a resent INIT_ACK
                    Ok((c, _)) => bail!("expected COOKIE_ACK, got chunk {c:#x}"),
                    Err(e) => return Err(e),
                }
            }
        }
        bail!("the console did not acknowledge the stream cookie")
    }

    /// Chunk kind and payload of a control packet (tag checked).
    fn parse_control<'a>(&mut self, pkt: &'a [u8]) -> Result<(u8, &'a [u8])> {
        let m = &pkt[1..];
        if m.len() < HEADER {
            bail!("control message too short");
        }
        if be32(&m[0..4]) != self.tag_local {
            bail!("control message for another session (tag mismatch)");
        }
        let low = be32(&m[8..12]);
        self.key_state.request_pos(low, true);
        let size = be16(&m[0xe..0x10]) as usize;
        if m.len() != size + 0xc {
            bail!(
                "control message size mismatch ({} vs {})",
                m.len(),
                size + 0xc
            );
        }
        Ok((m[0xc], &m[HEADER..]))
    }

    fn advance_key_pos(&mut self, mut len: usize) -> u64 {
        if self.gk_local.is_none() {
            return 0;
        }
        // chiaki rounds this way (adds len % 16, not up to a multiple). Kept as is.
        len += len % BLOCK;
        let cur = self.key_pos_local;
        self.key_pos_local += len as u64;
        cur
    }

    /// Send one datagram. A passing error (see [`soft_io_error`]) is counted, not returned.
    fn send_raw(&mut self, pkt: &[u8]) -> Result<()> {
        match self.sock.send(pkt) {
            Ok(_) => Ok(()),
            Err(e) if soft_io_error(&e) => {
                self.io_soft_errors += 1;
                Ok(())
            }
            Err(e) => Err(anyhow!("sending to the console: {e}")),
        }
    }

    /// MAC `pkt` in place (if crypt is up) and send it.
    fn send_maced(&mut self, pkt: &mut [u8], key_pos: u64) -> Result<()> {
        self.seal(pkt, key_pos);
        self.send_raw(pkt)
    }

    /// MAC `pkt` in place, if the crypt is up.
    fn seal(&mut self, pkt: &mut [u8], key_pos: u64) {
        let kind = pkt[0] & 0xf;
        let mo = mac_offset(kind).unwrap();
        let ko = key_pos_offset(kind).unwrap();
        pkt[mo..mo + GMAC_SIZE].fill(0);
        if let Some(gk) = self.gk_local.as_mut() {
            let saved: [u8; 4] = pkt[ko..ko + 4].try_into().unwrap();
            if kind == PACKET_CONTROL || kind == PACKET_CONGESTION {
                pkt[ko..ko + 4].fill(0);
            }
            let mac = gk.gmac(key_pos, pkt);
            pkt[mo..mo + GMAC_SIZE].copy_from_slice(&mac);
            pkt[ko..ko + 4].copy_from_slice(&saved);
        }
    }

    /// Send one DATA chunk. `first` chunks carry a data-type byte; `flags` 1
    /// marks the last chunk of a message.
    pub fn send_data(&mut self, flags: u8, channel: u16, data: &[u8], first: bool) -> Result<()> {
        let key_pos = self.advance_key_pos(data.len());
        let head = if first { 9 } else { 8 };
        let mut pkt = vec![PACKET_CONTROL];
        let tag_remote = self.tag_remote;
        pkt.extend_from_slice(&self.header(
            tag_remote,
            key_pos,
            CHUNK_DATA,
            flags,
            head + data.len(),
        ));
        let seq = self.seq_local;
        self.seq_local = self.seq_local.wrapping_add(1);
        pkt.extend_from_slice(&seq.to_be_bytes());
        pkt.extend_from_slice(&channel.to_be_bytes());
        pkt.extend_from_slice(&[0, 0]);
        if first {
            pkt.push(0);
        }
        pkt.extend_from_slice(data);
        self.seal(&mut pkt, key_pos);
        // Lost on purpose (the test knob): the resend has to bring it.
        let mut skip = None;
        if self.gk_local.is_some() {
            self.data_sent += 1;
            skip = self
                .drop_out_data
                .filter(|(n, _)| *n == self.data_sent)
                .map(|(_, all)| all);
        }
        if skip.is_none() {
            self.send_raw(&pkt)?;
        }
        if skip != Some(true) {
            self.send_buffer.push(seq, pkt, Instant::now());
        }
        Ok(())
    }

    /// Send again what the console has not acknowledged in time (best effort).
    fn resend_due(&mut self) {
        let (due, given_up) = self.send_buffer.due(Instant::now());
        self.data_given_up += given_up;
        for pkt in due {
            self.data_resent += 1;
            let _ = self.send_raw(&pkt);
        }
    }

    /// A protobuf message, split across DATA chunks the way chiaki splits BIG.
    pub fn send_message(&mut self, channel: u16, msg: &[u8]) -> Result<()> {
        const MTU: usize = 1454 - 50;
        if msg.len() + 26 <= MTU {
            return self.send_data(1, channel, msg, true);
        }
        let mut pos = 0;
        let mut first = true;
        loop {
            let room = if first { MTU - 26 } else { MTU - 25 };
            let rest = msg.len() - pos;
            if rest <= room {
                return self.send_data(1, channel, &msg[pos..], first);
            }
            self.send_data(0, channel, &msg[pos..pos + room], first)?;
            pos += room;
            first = false;
        }
    }

    fn send_data_ack(&mut self, seq: u32) -> Result<()> {
        let key_pos = self.advance_key_pos(1 + HEADER + 0xc);
        let mut pkt = vec![PACKET_CONTROL];
        let tag_remote = self.tag_remote;
        pkt.extend_from_slice(&self.header(tag_remote, key_pos, CHUNK_DATA_ACK, 0, 0xc));
        pkt.extend_from_slice(&seq.to_be_bytes());
        pkt.extend_from_slice(&A_RWND.to_be_bytes());
        pkt.extend_from_slice(&[0, 0, 0, 0]);
        self.send_maced(&mut pkt, key_pos)
    }

    /// The round trip to the console and back (DATA out, DATA_ACK in), smoothed; None before
    /// the console acknowledged one of our chunks the first time it was sent. It includes how
    /// long the console takes to acknowledge, which we cannot see apart.
    pub fn rtt(&self) -> Option<Duration> {
        self.rtt
    }

    /// The periodic "received n, lost m" report (chiaki sends it every
    /// 200 ms; the counts come from the frame and audio assemblers).
    pub fn send_congestion(&mut self, received: u16, lost: u16) -> Result<()> {
        if self.gk_local.is_none() {
            return Ok(());
        }
        let key_pos = self.advance_key_pos(0xf);
        let mut pkt = [0u8; 0xf];
        pkt[0] = PACKET_CONGESTION;
        pkt[3..5].copy_from_slice(&received.to_be_bytes());
        pkt[5..7].copy_from_slice(&lost.to_be_bytes());
        pkt[0xb..0xf].copy_from_slice(&(key_pos as u32).to_be_bytes());
        self.send_maced(&mut pkt, key_pos)
    }

    /// A controller feedback packet: `kind` 1 = history, 6 = state. The payload
    /// is encrypted, then the whole packet MACed (chiaki
    /// `takion_send_feedback_packet`).
    pub fn send_feedback(&mut self, kind: u8, seq: u16, count: u8, payload: &[u8]) -> Result<()> {
        let key_pos = self.advance_key_pos(payload.len() + BLOCK);
        let Some(gk) = self.gk_local.as_mut() else {
            bail!("feedback before the stream crypt is up");
        };
        let mut pkt = Vec::with_capacity(0xc + payload.len());
        pkt.push(kind);
        pkt.extend_from_slice(&seq.to_be_bytes());
        pkt.push(count);
        pkt.extend_from_slice(&(key_pos as u32).to_be_bytes());
        pkt.extend_from_slice(&[0, 0, 0, 0]);
        let mut body = payload.to_vec();
        gk.apply(key_pos + BLOCK as u64, &mut body);
        pkt.extend_from_slice(&body);
        let mac = gk.gmac(key_pos, &pkt);
        pkt[8..12].copy_from_slice(&mac);
        self.send_raw(&pkt)
    }

    /// A microphone packet built by [`super::mic::AudioSender`]: sealed with
    /// our crypt and sent (chiaki `chiaki_takion_send_mic_packet`).
    pub fn send_mic(&mut self, pkt: &mut [u8], ps5: bool) -> Result<()> {
        let key_pos = self.advance_key_pos(super::mic::key_pos_len(pkt, ps5));
        let Some(gk) = self.gk_local.as_mut() else {
            bail!("microphone audio before the stream crypt is up");
        };
        super::mic::seal(gk, key_pos, pkt, ps5);
        self.send_raw(pkt)
    }

    /// Check a received packet's MAC (once the console's crypt is known).
    fn verify(&mut self, pkt: &mut [u8]) -> bool {
        let kind = pkt[0] & 0xf;
        let (Some(mo), Some(ko)) = (mac_offset(kind), key_pos_offset(kind)) else {
            return true;
        };
        let Some(gk) = self.gk_remote.as_mut() else {
            return true;
        };
        if pkt.len() < mo + GMAC_SIZE || pkt.len() < ko + 4 {
            return false;
        }
        let key_pos = self.key_state.request_pos(be32(&pkt[ko..ko + 4]), false);
        let got: [u8; 4] = pkt[mo..mo + GMAC_SIZE].try_into().unwrap();
        pkt[mo..mo + GMAC_SIZE].fill(0);
        let saved: [u8; 4] = pkt[ko..ko + 4].try_into().unwrap();
        if kind == PACKET_CONTROL || kind == PACKET_CONGESTION {
            pkt[ko..ko + 4].fill(0);
        }
        let want = gk.gmac(key_pos, pkt);
        pkt[ko..ko + 4].copy_from_slice(&saved);
        pkt[mo..mo + GMAC_SIZE].copy_from_slice(&got);
        if want != got {
            return false;
        }
        self.key_state.commit(key_pos);
        true
    }

    /// Wait up to `timeout` for the next event. Sends DATA acks as a side effect.
    pub fn poll(&mut self, timeout: Duration) -> Result<Option<Event>> {
        self.resend_due();
        // Packets parked before the crypt was up go first.
        if self.gk_remote.is_some() && !self.postponed.is_empty() {
            let mut pkt = self.postponed.remove(0);
            if let Some(ev) = self.handle(&mut pkt)? {
                return Ok(Some(ev));
            }
        }
        if let Some(d) = self.pop_data()? {
            return Ok(Some(Event::Data(d.0, d.1)));
        }
        let mut buf = vec![0u8; 2048];
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(n) = self.recv_raw(&mut buf, left)? else {
                return Ok(None);
            };
            self.bytes_received += n as u64;
            self.last_rx = Instant::now();
            let mut pkt = buf[..n].to_vec();
            if let Some(ev) = self.handle(&mut pkt)? {
                return Ok(Some(ev));
            }
            if let Some(d) = self.pop_data()? {
                return Ok(Some(Event::Data(d.0, d.1)));
            }
            if left.is_zero() {
                return Ok(None);
            }
        }
    }

    fn pop_data(&mut self) -> Result<Option<(u8, Vec<u8>)>> {
        let next = self.seq_remote_next;
        if let Some(d) = self.data_early.remove(&next) {
            self.seq_remote_next = next.wrapping_add(1);
            self.send_data_ack(next)?;
            return Ok(Some(d));
        }
        Ok(None)
    }

    fn handle(&mut self, pkt: &mut Vec<u8>) -> Result<Option<Event>> {
        if pkt.is_empty() {
            return Ok(None);
        }
        let kind = pkt[0] & 0xf;
        if kind == PACKET_VIDEO && self.drop_pct > 0 && self.gk_remote.is_some() {
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 7;
            self.rng ^= self.rng << 17;
            if (self.rng % 100) < self.drop_pct as u64 {
                self.av_dropped_on_purpose += 1;
                return Ok(None);
            }
        }
        if (kind == PACKET_VIDEO || kind == PACKET_AUDIO) && self.gk_remote.is_none() {
            if self.postponed.len() < 256 {
                self.postponed.push(std::mem::take(pkt));
            }
            return Ok(None);
        }
        if !self.verify(pkt) {
            if kind == PACKET_VIDEO || kind == PACKET_AUDIO {
                self.av_bad_mac += 1;
            }
            return Ok(None);
        }
        match kind {
            PACKET_CONTROL => {
                let Ok((chunk, payload)) = self.parse_control(pkt) else {
                    return Ok(None);
                };
                // The console has our DATA up to this sequence number.
                if chunk == CHUNK_DATA_ACK && payload.len() >= 4 {
                    let (_, rtt) = self.send_buffer.ack(be32(&payload[0..4]), Instant::now());
                    if let Some(sample) = rtt {
                        self.rtt = Some(smooth_rtt(self.rtt, sample));
                    }
                    return Ok(None);
                }
                if chunk == CHUNK_DATA && payload.len() >= 9 {
                    let seq = be32(&payload[0..4]);
                    let data_type = payload[8];
                    if seq32_lt(seq, self.seq_remote_next) {
                        // A resend of something already delivered: ack again.
                        self.send_data_ack(self.seq_remote_next.wrapping_sub(1))?;
                        return Ok(None);
                    }
                    self.data_early
                        .insert(seq, (data_type, payload[9..].to_vec()));
                }
                Ok(None)
            }
            PACKET_VIDEO | PACKET_AUDIO => {
                let Some(mut av) = parse_av_v12(pkt, &mut self.key_state) else {
                    return Ok(None);
                };
                if let Some(gk) = self.gk_remote.as_ref() {
                    gk.apply(av.key_pos + BLOCK as u64, &mut av.data);
                }
                self.av_received += 1;
                Ok(Some(Event::Av(av)))
            }
            _ => Ok(None),
        }
    }

    /// End the stream the way chiaki does: a DISCONNECT on channel 1. Runs at
    /// most once; dropping a `Takion` calls it, so every exit path says goodbye.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let _ = self.send_message(1, &super::proto::disconnect("Client Disconnecting"));
    }
}

impl Drop for Takion {
    fn drop(&mut self) {
        self.close();
    }
}

/// Parse a v12 (PS5) AV packet header; the data stays encrypted.
fn parse_av_v12(buf: &[u8], key_state: &mut KeyState) -> Option<AvPacket> {
    let is_video = buf[0] & 0xf == PACKET_VIDEO;
    let nalu_info = (buf[0] >> 4) & 1 != 0;
    let av = &buf[1..];
    let header = if is_video { 0x17 } else { 0x13 };
    if av.len() < header + 1 {
        return None;
    }
    let packet_index = be16(&av[0..2]);
    let frame_index = be16(&av[2..4]);
    let dword2 = be32(&av[4..8]);
    let (unit_index, units_in_frame_total, units_in_frame_fec) = if is_video {
        (
            ((dword2 >> 0x15) & 0x7ff) as u16,
            (((dword2 >> 0xa) & 0x7ff) + 1) as u16,
            (dword2 & 0x3ff) as u16,
        )
    } else {
        (
            ((dword2 >> 0x18) & 0xff) as u16,
            (((dword2 >> 0x10) & 0xff) + 1) as u16,
            (dword2 & 0xffff) as u16,
        )
    };
    let codec = av[8];
    let key_pos = key_state.request_pos(be32(&av[0xd..0x11]), true);
    let mut p = 0x11;
    let mut adaptive_stream_index = 0;
    if is_video {
        adaptive_stream_index = av[p + 2] >> 5;
        p += 3;
    } else {
        p += 1;
    }
    if nalu_info {
        p += 3;
    }
    // v12 audio: a flag byte, 0x02 for the DualSense's haptics (chiaki av_packet_parse).
    let mut is_haptics = false;
    if !is_video {
        is_haptics = av.get(p) == Some(&0x02);
        p += 1;
    }
    if p > av.len() {
        return None;
    }
    Some(AvPacket {
        is_video,
        packet_index,
        frame_index,
        unit_index,
        units_in_frame_total,
        units_in_frame_fec,
        codec,
        adaptive_stream_index,
        is_haptics,
        key_pos,
        data: av[p..].to_vec(),
    })
}

/// The receive buffer asked for: a 4K keyframe is a burst of hundreds of packets.
const RECV_BUFFER: usize = 4 << 20;
/// Below this a refused size is not halved further: the OS default is kept.
const RECV_BUFFER_MIN: usize = 256 << 10;

/// A big receive buffer: a keyframe arrives as a burst of hundreds of packets, and the defaults
/// are small (64 KiB on Windows, ~200 KiB on Linux, ~768 KiB on macOS). Best effort on every OS:
/// Linux caps it silently at net.core.rmem_max, and macOS refuses a size above
/// kern.ipc.maxsockbuf outright, so a refused size is halved and asked again. Returns the size
/// the OS reports (Linux reports twice what it keeps for its own bookkeeping), never an error.
fn set_recv_buffer(sock: &UdpSocket, bytes: usize) -> Option<usize> {
    let s = socket2::SockRef::from(sock);
    for size in halvings(bytes, RECV_BUFFER_MIN) {
        if s.set_recv_buffer_size(size).is_ok() {
            break;
        }
    }
    s.recv_buffer_size().ok()
}

/// `from`, then half of it, and so on while at least `min`.
fn halvings(from: usize, min: usize) -> impl Iterator<Item = usize> {
    std::iter::successors(Some(from), |&n| Some(n / 2)).take_while(move |&n| n >= min.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_receive_buffer_is_asked_again_at_half() {
        assert_eq!(
            halvings(4 << 20, 256 << 10).collect::<Vec<_>>(),
            [4 << 20, 2 << 20, 1 << 20, 512 << 10, 256 << 10]
        );
        assert_eq!(halvings(100, 256).count(), 0);
    }

    #[test]
    fn the_receive_buffer_is_raised_on_this_os() {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let before = socket2::SockRef::from(&sock).recv_buffer_size().unwrap();
        let granted = set_recv_buffer(&sock, RECV_BUFFER).expect("a size is reported");
        // Whatever the OS caps it at, never smaller than it was, and never an error.
        assert!(granted >= before, "{granted} < {before}");
    }

    fn buffer_with(seqs: &[u32], now: Instant) -> SendBuffer {
        let mut b = SendBuffer::default();
        for &s in seqs {
            assert!(b.push(s, vec![s as u8], now));
        }
        b
    }

    #[test]
    fn an_unacknowledged_chunk_is_sent_again_after_200_ms() {
        let t = Instant::now();
        let mut b = buffer_with(&[7], t);
        assert!(b.due(t + Duration::from_millis(100)).0.is_empty());
        let (again, given_up) = b.due(t + Duration::from_millis(201));
        assert_eq!(again, vec![vec![7u8]]);
        assert_eq!(given_up, 0);
        // The clock restarts at the resend.
        assert!(b.due(t + Duration::from_millis(300)).0.is_empty());
    }

    #[test]
    fn a_cumulative_ack_forgets_everything_up_to_it_across_the_wrap() {
        let t = Instant::now();
        let mut b = buffer_with(&[u32::MAX - 1, u32::MAX, 0, 1], t);
        assert_eq!(b.ack(0, t).0, 3);
        let (left, _) = b.due(t + Duration::from_secs(1));
        assert_eq!(left, vec![vec![1u8]]);
        // An old ack changes nothing.
        assert_eq!(b.ack(u32::MAX - 5, t).0, 0);
    }

    #[test]
    fn an_ack_times_the_round_trip_of_a_chunk_sent_once() {
        let t = Instant::now();
        let mut b = buffer_with(&[5, 6], t);
        // 6 acknowledged 12 ms after it went: one sample, from the chunk the ack names.
        assert_eq!(
            b.ack(6, t + Duration::from_millis(12)),
            (2, Some(Duration::from_millis(12)))
        );
        // A resent chunk gives none: the ack could answer either copy.
        let mut b = buffer_with(&[7], t);
        b.due(t + Duration::from_millis(201));
        assert_eq!(b.ack(7, t + Duration::from_millis(210)), (1, None));
        // An ack for something no longer held gives none.
        assert_eq!(b.ack(7, t + Duration::from_millis(220)), (0, None));
    }

    #[test]
    fn the_round_trip_moves_a_quarter_of_the_way() {
        let ms = Duration::from_millis;
        assert_eq!(smooth_rtt(None, ms(8)), ms(8));
        assert_eq!(smooth_rtt(Some(ms(8)), ms(40)), ms(16));
    }

    #[test]
    fn a_chunk_is_given_up_on_after_ten_sends() {
        let t = Instant::now();
        let mut b = buffer_with(&[1], t);
        let mut sent = 1;
        let mut now = t;
        loop {
            now += Duration::from_millis(250);
            let (again, given_up) = b.due(now);
            sent += again.len();
            if given_up == 1 {
                break;
            }
        }
        assert_eq!(sent, SendBuffer::TRIES_MAX as usize);
        assert!(b.packets.is_empty());
    }

    #[test]
    fn a_full_buffer_keeps_no_more() {
        let t = Instant::now();
        let seqs: Vec<u32> = (0..SendBuffer::SIZE as u32).collect();
        let mut b = buffer_with(&seqs, t);
        assert!(!b.push(99, vec![], t));
        assert_eq!(b.ack(0, t).0, 1);
        assert!(b.push(99, vec![], t));
    }

    #[test]
    fn the_drop_knob_reads_one_copy_or_all() {
        assert_eq!(drop_knob("40"), Some((40, false)));
        assert_eq!(drop_knob("40all"), Some((40, true)));
        assert_eq!(drop_knob("x"), None);
    }

    #[test]
    fn passing_network_errors_do_not_end_the_stream() {
        use std::io::{Error, ErrorKind};
        for k in [
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionRefused,
            ErrorKind::HostUnreachable,
            ErrorKind::NetworkUnreachable,
            ErrorKind::WouldBlock,
        ] {
            assert!(soft_io_error(&Error::from(k)), "{k:?}");
        }
        #[cfg(unix)]
        assert!(soft_io_error(&Error::from_raw_os_error(libc::ENOBUFS)));
        assert!(!soft_io_error(&Error::from(ErrorKind::PermissionDenied)));
        assert!(!soft_io_error(&Error::from(ErrorKind::InvalidInput)));
    }

    /// Until the console acknowledges our DATA, the chunk is sent again, byte for byte.
    #[test]
    fn a_lost_data_chunk_reaches_the_console_on_the_resend() {
        let console = UdpSocket::bind("127.0.0.1:0").unwrap();
        console
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let ours = UdpSocket::bind("127.0.0.1:0").unwrap();
        ours.connect(console.local_addr().unwrap()).unwrap();
        let mut t = Takion {
            sock: ours,
            tag_local: 1,
            tag_remote: 2,
            seq_local: 100,
            key_pos_local: 0,
            gk_local: None,
            gk_remote: None,
            key_state: KeyState::default(),
            seq_remote_next: 0,
            data_early: BTreeMap::new(),
            postponed: Vec::new(),
            av_received: 0,
            av_bad_mac: 0,
            bytes_received: 0,
            drop_pct: 0,
            rng: 1,
            av_dropped_on_purpose: 0,
            closed: true,
            send_buffer: SendBuffer::default(),
            data_resent: 0,
            data_given_up: 0,
            io_soft_errors: 0,
            last_rx: Instant::now(),
            drop_out_data: None,
            data_sent: 0,
            rtt: None,
        };
        t.send_message(1, b"heartbeat").unwrap();
        let mut buf = [0u8; 256];
        let n = console.recv(&mut buf).unwrap();
        let first = buf[..n].to_vec();
        // No ack: the poll after 200 ms sends it again.
        std::thread::sleep(Duration::from_millis(220));
        let _ = t.poll(Duration::from_millis(1));
        assert_eq!(t.data_resent, 1);
        let n = console.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], &first[..]);
        // Acknowledged: never again.
        assert_eq!(t.send_buffer.ack(100, Instant::now()).0, 1);
        std::thread::sleep(Duration::from_millis(220));
        let _ = t.poll(Duration::from_millis(1));
        assert_eq!(t.data_resent, 1);
    }

    /// A v12 audio packet: the flag byte after the header says haptics (0x02) or sound.
    #[test]
    fn a_v12_audio_packet_says_whether_it_is_haptics() {
        let pkt = |flag: u8| {
            let mut b = vec![PACKET_AUDIO];
            let mut av = vec![0u8; 0x12];
            av[0..2].copy_from_slice(&7u16.to_be_bytes()); // packet index
            av[2..4].copy_from_slice(&40u16.to_be_bytes()); // frame index
            av[8] = 5; // codec
            av.push(flag);
            av.extend_from_slice(&[0xaa; 120]);
            b.extend_from_slice(&av);
            b
        };
        let mut ks = KeyState::default();
        let h = parse_av_v12(&pkt(0x02), &mut ks).unwrap();
        assert!(h.is_haptics && !h.is_video);
        assert_eq!((h.frame_index, h.codec, h.data.len()), (40, 5, 120));
        let a = parse_av_v12(&pkt(0x00), &mut ks).unwrap();
        assert!(!a.is_haptics);
        assert_eq!(a.data, vec![0xaa; 120]);
    }
}
