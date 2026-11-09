//! An RTMP(S) publisher: the part of RTMP a broadcaster needs, as OBS and ffmpeg speak it to
//! YouTube, Twitch, Kick, Facebook and nginx-rtmp style servers.
//!
//! * The plain handshake (C0/C1/C2, version 3): what every ingest takes from a publisher.
//! * `connect` (app, tcUrl, flashVer "FMLE/3.0"), `releaseStream`, `FCPublish`, `createStream`,
//!   then `publish(name, "live")` and the wait for `NetStream.Publish.Start`. An `_error` to
//!   connect, or an error status to publish, is the site refusing the stream (almost always the
//!   key).
//! * Chunking out at 4096 bytes (after Set Chunk Size), every message with a full (type 0)
//!   header; chunks in at whatever the server sets, all four header types, extended times.
//! * The server's Window Acknowledgement Size is honoured (acknowledgements go out), pings are
//!   answered. Replies are queued while reading and written before the next message, so
//!   reading stays cancel-safe (the sink selects between its queue and the socket).
//! * rtmps:// is the same over TLS (rustls with the public web roots, as a browser checks a
//!   site), port 443 by default.
//!
//! The URL is split as ffmpeg splits it, so a destination that worked with ffmpeg names the
//! same app and stream here: `rtmp://host[:port]/app/name`, with `app` taking a second path
//! part when there are three or more (`/app/instance/name`); the query stays with the name
//! (Facebook's keys and Twitch's `?bandwidthtest=true`). Nothing here logs or returns the
//! stream name (it is the key): errors carry the server's status code only.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::amf::{self, Amf};
use crate::golive_ffmpeg::Failure;

pub(crate) const MSG_SET_CHUNK_SIZE: u8 = 1;
pub(crate) const MSG_ABORT: u8 = 2;
pub(crate) const MSG_ACK: u8 = 3;
pub(crate) const MSG_USER_CONTROL: u8 = 4;
pub(crate) const MSG_WINDOW_ACK: u8 = 5;
pub(crate) const MSG_PEER_BANDWIDTH: u8 = 6;
pub(crate) const MSG_DATA_AMF3: u8 = 15;
pub(crate) const MSG_COMMAND_AMF3: u8 = 17;
pub(crate) const MSG_DATA: u8 = 18;
pub(crate) const MSG_COMMAND: u8 = 20;

/// Chunk streams: protocol control, commands, then the stream's data, audio and video.
pub(crate) const CS_CONTROL: u8 = 2;
pub(crate) const CS_COMMAND: u8 = 3;
const CS_AUDIO: u8 = 4;
const CS_DATA: u8 = 5;
const CS_VIDEO: u8 = 6;

/// The chunk size this side sends with.
pub(crate) const OUT_CHUNK: usize = 4096;
/// A message from the server bigger than this is not one a publisher is sent.
const MAX_IN_MESSAGE: usize = 4 << 20;
/// Chunk streams the server may use at once.
const MAX_IN_STREAMS: usize = 64;
const HANDSHAKE: usize = 1536;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RtmpUrl {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub app: String,
    /// The stream name: the key, and the query after it. Never log it.
    pub stream: String,
    pub tc_url: String,
}

/// Splits a publish URL as ffmpeg's rtmp protocol does. None for anything but a usable
/// rtmp:// or rtmps:// URL with a stream name.
pub(crate) fn parse_url(url: &str) -> Option<RtmpUrl> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let tls = match scheme.as_str() {
        "rtmp" => false,
        "rtmps" => true,
        _ => return None,
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let authority = authority.rsplit('@').next()?;
    let (host, port) = if let Some(r) = authority.strip_prefix('[') {
        let (h, after) = r.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p.parse::<u16>().ok()?),
            None if after.is_empty() => None,
            None => return None,
        };
        (h.to_string(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p.parse::<u16>().ok()?)),
            None => (authority.to_string(), None),
        }
    };
    if host.is_empty() {
        return None;
    }
    let port = port.unwrap_or(if tls { 443 } else { 1935 });
    let next = path.strip_prefix('/').unwrap_or(path);
    let (app, stream) = match next.find('/') {
        None => ("", next),
        Some(p) => {
            let after = &next[p + 1..];
            let colon = after.find(':');
            match after.find('/') {
                Some(s) if colon.is_none_or(|c| c > s) => (&next[..p + 1 + s], &after[s + 1..]),
                _ => (&next[..p], after),
            }
        }
    };
    if stream.is_empty() {
        return None;
    }
    let host_in_url = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.clone()
    };
    Some(RtmpUrl {
        tls,
        tc_url: format!("{scheme}://{host_in_url}:{port}/{app}"),
        host,
        port,
        app: app.to_string(),
        stream: stream.to_string(),
    })
}

/// Why a publish failed. None of these carry the stream name.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RtmpError {
    /// No connection to the server (name, refused, no route, the handshake never answered).
    Unreachable(String),
    /// The secure connection failed (the certificate, mostly).
    Tls(String),
    /// The server refused the connection or the stream: its status code.
    Rejected(String),
    /// The connection ended or failed after it was up.
    Dropped(String),
    /// The server sent something this client can't follow.
    Protocol(String),
    /// A step took too long (the step).
    Timeout(&'static str),
}

impl RtmpError {
    pub(crate) fn failure(&self) -> Failure {
        match self {
            RtmpError::Unreachable(_) => Failure::Unreachable,
            RtmpError::Tls(_) => Failure::Tls,
            RtmpError::Rejected(_) => Failure::Rejected,
            RtmpError::Dropped(_) => Failure::Dropped,
            RtmpError::Protocol(_) => Failure::Other,
            RtmpError::Timeout(step) if *step == "connect" => Failure::Unreachable,
            RtmpError::Timeout(_) => Failure::Dropped,
        }
    }
}

impl std::fmt::Display for RtmpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RtmpError::Unreachable(e) => write!(f, "can't reach the server: {e}"),
            RtmpError::Tls(e) => write!(f, "secure connection failed: {e}"),
            RtmpError::Rejected(c) => write!(f, "the server refused the stream ({c})"),
            RtmpError::Dropped(e) => write!(f, "the connection dropped: {e}"),
            RtmpError::Protocol(e) => write!(f, "unexpected answer from the server: {e}"),
            RtmpError::Timeout(s) => write!(f, "no answer from the server ({s})"),
        }
    }
}

fn dropped(e: std::io::Error) -> RtmpError {
    RtmpError::Dropped(e.to_string())
}

/// A connection a publisher runs over: TCP, or TLS over TCP.
pub(crate) trait Conn: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Conn for T {}

/// TLS checked against the public web roots.
pub(crate) fn public_tls() -> Option<tokio_rustls::TlsConnector> {
    static TLS: OnceLock<Option<tokio_rustls::TlsConnector>> = OnceLock::new();
    TLS.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        tls_with_roots(roots)
    })
    .clone()
}

pub(crate) fn tls_with_roots(roots: rustls::RootCertStore) -> Option<tokio_rustls::TlsConnector> {
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .ok()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Some(tokio_rustls::TlsConnector::from(Arc::new(config)))
}

/// TCP (and TLS for rtmps://) to the server.
pub(crate) async fn connect(
    url: &RtmpUrl,
    tls: Option<&tokio_rustls::TlsConnector>,
) -> Result<Box<dyn Conn>, RtmpError> {
    let tcp = tokio::net::TcpStream::connect((url.host.as_str(), url.port))
        .await
        .map_err(|e| RtmpError::Unreachable(e.to_string()))?;
    let _ = tcp.set_nodelay(true);
    if !url.tls {
        return Ok(Box::new(tcp));
    }
    let tls = tls.ok_or_else(|| RtmpError::Tls("no TLS configuration".into()))?;
    let name = rustls::pki_types::ServerName::try_from(url.host.clone())
        .map_err(|_| RtmpError::Unreachable("not a valid server name".into()))?;
    match tls.connect(name, tcp).await {
        Ok(s) => Ok(Box::new(s)),
        Err(e) => {
            let is_tls = e
                .get_ref()
                .is_some_and(|i| i.downcast_ref::<rustls::Error>().is_some());
            Err(if is_tls {
                RtmpError::Tls(e.to_string())
            } else {
                RtmpError::Unreachable(e.to_string())
            })
        }
    }
}

/// One message, as chunks with a full header on the first and type 3 headers after.
pub(crate) fn chunk_message(
    out: &mut Vec<u8>,
    csid: u8,
    ts: u32,
    typ: u8,
    msid: u32,
    body: &[u8],
    chunk: usize,
) {
    debug_assert!((2..64).contains(&csid));
    let ext = ts >= 0xff_ffff;
    let ts24 = if ext { 0xff_ffff } else { ts };
    out.push(csid);
    out.extend_from_slice(&ts24.to_be_bytes()[1..]);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    out.push(typ);
    out.extend_from_slice(&msid.to_le_bytes());
    if ext {
        out.extend_from_slice(&ts.to_be_bytes());
    }
    let mut parts = body.chunks(chunk.max(1));
    if let Some(first) = parts.next() {
        out.extend_from_slice(first);
    }
    for p in parts {
        out.push(0xc0 | csid);
        if ext {
            out.extend_from_slice(&ts.to_be_bytes());
        }
        out.extend_from_slice(p);
    }
}

/// A whole message from the other side.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Message {
    pub csid: u32,
    pub typ: u8,
    pub ts: u32,
    pub msid: u32,
    pub body: Vec<u8>,
}

#[derive(Default, Debug)]
struct InStream {
    ts: u32,
    delta: u32,
    len: usize,
    typ: u8,
    msid: u32,
    ext: bool,
    buf: Vec<u8>,
    /// A header (type 0) was seen: types 1-3 have something to go on.
    started: bool,
}

/// Reassembles chunks into messages.
#[derive(Debug)]
pub(crate) struct ChunkReader {
    pub chunk_size: usize,
    streams: HashMap<u32, InStream>,
    pending: Vec<u8>,
}

impl Default for ChunkReader {
    fn default() -> Self {
        ChunkReader {
            chunk_size: 128,
            streams: HashMap::new(),
            pending: Vec::new(),
        }
    }
}

impl ChunkReader {
    pub(crate) fn push(&mut self, data: &[u8]) {
        self.pending.extend_from_slice(data);
    }

    /// The next whole message, None until more data comes. A broken stream is an Err.
    pub(crate) fn next(&mut self) -> Result<Option<Message>, String> {
        loop {
            match self.one_chunk()? {
                Chunk::Incomplete => return Ok(None),
                Chunk::Part => continue,
                Chunk::Done(m) => {
                    if m.typ == MSG_SET_CHUNK_SIZE && m.body.len() >= 4 {
                        let n = u32::from_be_bytes([m.body[0], m.body[1], m.body[2], m.body[3]])
                            & 0x7fff_ffff;
                        if n == 0 {
                            return Err("chunk size 0".into());
                        }
                        self.chunk_size = (n as usize).min(1 << 24);
                    }
                    if m.typ == MSG_ABORT && m.body.len() >= 4 {
                        let cs = u32::from_be_bytes([m.body[0], m.body[1], m.body[2], m.body[3]]);
                        if let Some(s) = self.streams.get_mut(&cs) {
                            s.buf.clear();
                        }
                    }
                    return Ok(Some(m));
                }
            }
        }
    }

    fn one_chunk(&mut self) -> Result<Chunk, String> {
        let b = &self.pending;
        let mut at = 0usize;
        let need = |at: usize, n: usize| b.len() >= at + n;
        if !need(at, 1) {
            return Ok(Chunk::Incomplete);
        }
        let fmt = b[0] >> 6;
        let mut csid = (b[0] & 0x3f) as u32;
        at += 1;
        if csid == 0 {
            if !need(at, 1) {
                return Ok(Chunk::Incomplete);
            }
            csid = b[at] as u32 + 64;
            at += 1;
        } else if csid == 1 {
            if !need(at, 2) {
                return Ok(Chunk::Incomplete);
            }
            csid = b[at] as u32 + (b[at + 1] as u32) * 256 + 64;
            at += 2;
        }
        let hlen = [11usize, 7, 3, 0][fmt as usize];
        if !need(at, hlen) {
            return Ok(Chunk::Incomplete);
        }
        let u24 = |i: usize| u32::from_be_bytes([0, b[i], b[i + 1], b[i + 2]]);
        if !self.streams.contains_key(&csid) && self.streams.len() >= MAX_IN_STREAMS {
            return Err("too many chunk streams".into());
        }
        let prev = self.streams.get(&csid);
        if fmt != 0 && !prev.is_some_and(|s| s.started) {
            return Err(format!("chunk type {fmt} with no header before it"));
        }
        let mid_message = prev.is_some_and(|s| !s.buf.is_empty());
        let (mut ts_field, mut len, mut typ, mut msid) = prev
            .map(|s| (if fmt == 0 { s.ts } else { s.delta }, s.len, s.typ, s.msid))
            .unwrap_or_default();
        if fmt <= 2 {
            ts_field = u24(at);
        }
        if fmt <= 1 {
            len = u24(at + 3) as usize;
            typ = b[at + 6];
        }
        if fmt == 0 {
            msid = u32::from_le_bytes([b[at + 7], b[at + 8], b[at + 9], b[at + 10]]);
        }
        at += hlen;
        let ext = if fmt == 3 {
            prev.is_some_and(|s| s.ext)
        } else {
            ts_field == 0xff_ffff
        };
        let mut ext_ts = None;
        if ext {
            if !need(at, 4) {
                return Ok(Chunk::Incomplete);
            }
            ext_ts = Some(u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]));
            at += 4;
        }
        if len > MAX_IN_MESSAGE {
            return Err(format!("a message of {len} bytes"));
        }
        let have = if fmt == 3 && mid_message {
            prev.map_or(0, |s| s.buf.len())
        } else {
            0
        };
        let take = (len - have.min(len)).min(self.chunk_size);
        if !need(at, take) {
            return Ok(Chunk::Incomplete);
        }
        // The whole chunk is here: take it.
        let data = b[at..at + take].to_vec();
        let consumed = at + take;
        let s = self.streams.entry(csid).or_default();
        let new_message = !(fmt == 3 && mid_message);
        if new_message {
            let t = ext_ts.unwrap_or(ts_field);
            match fmt {
                0 => {
                    s.ts = t;
                    s.delta = 0;
                }
                1 | 2 => {
                    s.delta = t;
                    s.ts = s.ts.wrapping_add(t);
                }
                // A new message with type 3: the last delta again.
                _ => s.ts = s.ts.wrapping_add(s.delta),
            }
            s.buf.clear();
            s.len = len;
            s.typ = typ;
            s.msid = msid;
            if fmt != 3 {
                s.ext = ext;
            }
            s.started = true;
        }
        s.buf.extend_from_slice(&data);
        self.pending.drain(..consumed);
        let s = self.streams.get_mut(&csid).expect("just inserted");
        if s.buf.len() >= s.len {
            let body = std::mem::take(&mut s.buf);
            return Ok(Chunk::Done(Message {
                csid,
                typ: s.typ,
                ts: s.ts,
                msid: s.msid,
                body,
            }));
        }
        Ok(Chunk::Part)
    }
}

enum Chunk {
    Incomplete,
    Part,
    Done(Message),
}

/// A command message's values (AMF0, or AMF3's wrapper around AMF0).
pub(crate) fn command_values(m: &Message) -> Option<Vec<Amf>> {
    match m.typ {
        MSG_COMMAND | MSG_DATA => Some(amf::decode(&m.body)),
        MSG_COMMAND_AMF3 | MSG_DATA_AMF3 => Some(amf::decode(m.body.get(1..)?)),
        _ => None,
    }
}

/// The plain handshake, client side.
async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S) -> Result<(), RtmpError> {
    let mut c0c1 = vec![0u8; 1 + HANDSHAKE];
    c0c1[0] = 3;
    let _ = getrandom::fill(&mut c0c1[9..]);
    let unreachable = |e: std::io::Error| RtmpError::Unreachable(format!("handshake: {e}"));
    s.write_all(&c0c1).await.map_err(unreachable)?;
    s.flush().await.map_err(unreachable)?;
    let mut s0s1 = vec![0u8; 1 + HANDSHAKE];
    s.read_exact(&mut s0s1).await.map_err(unreachable)?;
    if s0s1[0] != 3 {
        return Err(RtmpError::Protocol(format!(
            "handshake version {}",
            s0s1[0]
        )));
    }
    let mut s2 = vec![0u8; HANDSHAKE];
    s.read_exact(&mut s2).await.map_err(unreachable)?;
    s.write_all(&s0s1[1..]).await.map_err(unreachable)?;
    Ok(())
}

/// A published stream on a server.
pub(crate) struct Publisher<S> {
    io: S,
    rd: ChunkReader,
    stream_id: u32,
    stream: String,
    /// Bytes from the server, and the last count acknowledged.
    received: u64,
    acked: u64,
    window: u32,
    /// Replies (acknowledgements, pongs) to write before the next message.
    pending: Vec<u8>,
    inbuf: Vec<u8>,
    out: Vec<u8>,
    pub bytes_out: u64,
}

const TXN_CONNECT: f64 = 1.0;
const TXN_CREATE_STREAM: f64 = 4.0;
const TXN_PUBLISH: f64 = 5.0;

impl<S: AsyncRead + AsyncWrite + Unpin + Send> Publisher<S> {
    /// Handshake, connect, create the stream and publish it. Returns once the server said
    /// `NetStream.Publish.Start`.
    pub(crate) async fn publish(mut io: S, url: &RtmpUrl) -> Result<Self, RtmpError> {
        handshake(&mut io).await?;
        let mut p = Publisher {
            io,
            rd: ChunkReader::default(),
            stream_id: 0,
            stream: url.stream.clone(),
            received: 0,
            acked: 0,
            window: 0,
            pending: Vec::new(),
            inbuf: vec![0u8; 16 * 1024],
            out: Vec::with_capacity(64 * 1024),
            bytes_out: 0,
        };
        p.write_msg(
            CS_CONTROL,
            MSG_SET_CHUNK_SIZE,
            0,
            0,
            &(OUT_CHUNK as u32).to_be_bytes(),
        )
        .await?;
        p.command(&[
            Amf::str("connect"),
            Amf::Number(TXN_CONNECT),
            Amf::obj(&[
                ("app", Amf::str(&url.app)),
                ("type", Amf::str("nonprivate")),
                ("flashVer", Amf::str("FMLE/3.0 (compatible; FMSc/1.0)")),
                ("tcUrl", Amf::str(&url.tc_url)),
            ]),
        ])
        .await?;
        p.wait_result(TXN_CONNECT).await?;
        let name = Amf::str(&p.stream);
        p.command(&[
            Amf::str("releaseStream"),
            Amf::Number(2.0),
            Amf::Null,
            name.clone(),
        ])
        .await?;
        p.command(&[
            Amf::str("FCPublish"),
            Amf::Number(3.0),
            Amf::Null,
            name.clone(),
        ])
        .await?;
        p.command(&[
            Amf::str("createStream"),
            Amf::Number(TXN_CREATE_STREAM),
            Amf::Null,
        ])
        .await?;
        let r = p.wait_result(TXN_CREATE_STREAM).await?;
        let id = r.get(3).and_then(Amf::as_f64).unwrap_or(1.0);
        if !(0.0..=u32::MAX as f64).contains(&id) {
            return Err(RtmpError::Protocol("stream id".into()));
        }
        p.stream_id = id as u32;
        let body = amf::encode(&[
            Amf::str("publish"),
            Amf::Number(TXN_PUBLISH),
            Amf::Null,
            name,
            Amf::str("live"),
        ]);
        p.write_msg(CS_COMMAND, MSG_COMMAND, 0, p.stream_id, &body)
            .await?;
        p.wait_publish_start().await?;
        Ok(p)
    }

    async fn command(&mut self, vals: &[Amf]) -> Result<(), RtmpError> {
        let body = amf::encode(vals);
        self.write_msg(CS_COMMAND, MSG_COMMAND, 0, 0, &body).await
    }

    async fn write_msg(
        &mut self,
        csid: u8,
        typ: u8,
        ts: u32,
        msid: u32,
        body: &[u8],
    ) -> Result<(), RtmpError> {
        self.out.clear();
        self.out.append(&mut self.pending);
        chunk_message(&mut self.out, csid, ts, typ, msid, body, OUT_CHUNK);
        self.io.write_all(&self.out).await.map_err(dropped)?;
        self.io.flush().await.map_err(dropped)?;
        self.bytes_out += self.out.len() as u64;
        Ok(())
    }

    /// Reads once from the server and handles what came (replies are queued). Cancel-safe: the
    /// only wait is the read.
    pub(crate) async fn read_once(&mut self) -> Result<Vec<Message>, RtmpError> {
        let n = self.io.read(&mut self.inbuf).await.map_err(dropped)?;
        if n == 0 {
            return Err(RtmpError::Dropped(
                "the server closed the connection".into(),
            ));
        }
        self.received += n as u64;
        self.rd.push(&self.inbuf[..n]);
        let mut cmds = Vec::new();
        while let Some(m) = self.rd.next().map_err(RtmpError::Protocol)? {
            self.control(&m);
            if matches!(m.typ, MSG_COMMAND | MSG_COMMAND_AMF3) {
                cmds.push(m);
            }
        }
        if self.window > 0 && self.received - self.acked >= self.window as u64 / 2 {
            self.acked = self.received;
            chunk_message(
                &mut self.pending,
                CS_CONTROL,
                0,
                MSG_ACK,
                0,
                &(self.received as u32).to_be_bytes(),
                OUT_CHUNK,
            );
        }
        Ok(cmds)
    }

    fn control(&mut self, m: &Message) {
        let u32_at = |i: usize| {
            m.body
                .get(i..i + 4)
                .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        };
        match m.typ {
            MSG_WINDOW_ACK => self.window = u32_at(0).unwrap_or(0),
            MSG_PEER_BANDWIDTH => {
                // Answered with our own window, as the spec asks.
                if let Some(w) = u32_at(0) {
                    chunk_message(
                        &mut self.pending,
                        CS_CONTROL,
                        0,
                        MSG_WINDOW_ACK,
                        0,
                        &w.to_be_bytes(),
                        OUT_CHUNK,
                    );
                }
            }
            // Ping request (6) → ping response (7) with the same time.
            MSG_USER_CONTROL if m.body.len() >= 6 && m.body[..2] == [0, 6] => {
                let mut b = vec![0, 7];
                b.extend_from_slice(&m.body[2..6]);
                chunk_message(
                    &mut self.pending,
                    CS_CONTROL,
                    0,
                    MSG_USER_CONTROL,
                    0,
                    &b,
                    OUT_CHUNK,
                );
            }
            _ => {}
        }
    }

    /// Commands until the `_result` of transaction `txn`; its `_error` is a refusal.
    async fn wait_result(&mut self, txn: f64) -> Result<Vec<Amf>, RtmpError> {
        loop {
            for m in self.read_once().await? {
                let Some(v) = command_values(&m) else {
                    continue;
                };
                let name = v.first().and_then(Amf::as_str).unwrap_or("");
                let id = v.get(1).and_then(Amf::as_f64);
                if id != Some(txn) {
                    // releaseStream / FCPublish answers (some servers refuse those: harmless),
                    // onBWDone, onFCPublish.
                    continue;
                }
                match name {
                    "_result" => return Ok(v),
                    "_error" => return Err(RtmpError::Rejected(status_code(&v, "_error"))),
                    _ => {}
                }
            }
            self.flush_pending().await?;
        }
    }

    async fn wait_publish_start(&mut self) -> Result<(), RtmpError> {
        loop {
            for m in self.read_once().await? {
                let Some(v) = command_values(&m) else {
                    continue;
                };
                match v.first().and_then(Amf::as_str) {
                    Some("onStatus") => {
                        let info = v.get(3);
                        let code = info
                            .and_then(|i| i.get("code"))
                            .and_then(Amf::as_str)
                            .unwrap_or("");
                        let level = info
                            .and_then(|i| i.get("level"))
                            .and_then(Amf::as_str)
                            .unwrap_or("");
                        if code == "NetStream.Publish.Start" {
                            return Ok(());
                        }
                        if level == "error" || code.contains("Failed") || code.contains("BadName") {
                            return Err(RtmpError::Rejected(status_code(&v, "onStatus")));
                        }
                    }
                    Some("_error") if v.get(1).and_then(Amf::as_f64) == Some(TXN_PUBLISH) => {
                        return Err(RtmpError::Rejected(status_code(&v, "_error")));
                    }
                    _ => {}
                }
            }
            self.flush_pending().await?;
        }
    }

    async fn flush_pending(&mut self) -> Result<(), RtmpError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let p = std::mem::take(&mut self.pending);
        self.io.write_all(&p).await.map_err(dropped)?;
        self.io.flush().await.map_err(dropped)?;
        self.bytes_out += p.len() as u64;
        Ok(())
    }

    /// One FLV tag body as a message on the stream.
    pub(crate) async fn send(&mut self, tag: u8, ts: u32, body: &[u8]) -> Result<(), RtmpError> {
        let csid = match tag {
            super::flv::TAG_AUDIO => CS_AUDIO,
            super::flv::TAG_VIDEO => CS_VIDEO,
            _ => CS_DATA,
        };
        self.write_msg(csid, tag, ts, self.stream_id, body).await
    }

    /// What the server says while streaming: an error status ends the stream.
    pub(crate) fn check_status(cmds: &[Message]) -> Result<(), RtmpError> {
        for m in cmds {
            let Some(v) = command_values(m) else {
                continue;
            };
            if v.first().and_then(Amf::as_str) == Some("onStatus") {
                let level = v.get(3).and_then(|i| i.get("level")).and_then(Amf::as_str);
                if level == Some("error") {
                    return Err(RtmpError::Rejected(status_code(&v, "onStatus")));
                }
            }
        }
        Ok(())
    }

    /// Unpublishes (best effort) and closes the connection.
    pub(crate) async fn close(mut self) {
        let name = Amf::str(&self.stream);
        let _ = self
            .command(&[Amf::str("FCUnpublish"), Amf::Number(6.0), Amf::Null, name])
            .await;
        let _ = self
            .command(&[
                Amf::str("deleteStream"),
                Amf::Number(7.0),
                Amf::Null,
                Amf::Number(self.stream_id as f64),
            ])
            .await;
        let _ = self.io.shutdown().await;
    }
}

/// The status code of an `_error` or `onStatus` (its info object's `code`), never its
/// description (a server may quote the stream name there).
fn status_code(v: &[Amf], fallback: &str) -> String {
    v.get(3)
        .and_then(|i| i.get("code"))
        .and_then(Amf::as_str)
        .filter(|c| c.len() <= 80 && c.chars().all(|c| c.is_ascii_graphic()))
        .unwrap_or(fallback)
        .to_string()
}

#[cfg(test)]
#[path = "rtmp_tests.rs"]
mod tests;
