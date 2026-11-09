//! A small in-process RTMP ingest for the tests: it takes a publisher the way YouTube or
//! nginx-rtmp does (handshake, connect, createStream, publish, then media), records what it got
//! and can refuse, ping, re-chunk small, or hang up. Over plain TCP or TLS (a self-signed
//! certificate for "localhost", trusted by the test's own client only).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;

use super::amf::{self, Amf};
use super::rtmp::{self, ChunkReader, Message};

#[derive(Clone, Debug)]
pub(crate) struct Opts {
    /// `_error` to connect.
    pub reject_connect: bool,
    /// An error status to publish (with the stream name in its description, as some servers
    /// write it: the client must not repeat that).
    pub reject_publish: bool,
    /// The server's own chunk size (small: the client reassembles).
    pub chunk: usize,
    /// A ping after publish; the client must answer it.
    pub ping: bool,
    /// Window acknowledgement size the server asks for.
    pub window: u32,
    /// Hang up after this many media messages.
    pub close_after: Option<usize>,
    /// Take the TCP connection and never answer the handshake.
    pub silent: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            reject_connect: false,
            reject_publish: false,
            chunk: 60,
            ping: true,
            window: 5000,
            close_after: None,
            silent: false,
        }
    }
}

/// What one publisher sent.
#[derive(Clone, Debug, Default)]
pub(crate) struct Got {
    pub app: String,
    pub tc_url: String,
    pub flash_ver: String,
    pub stream: String,
    pub publish_type: String,
    pub published: bool,
    /// Audio, video and data messages, in order.
    pub media: Vec<Message>,
    pub acks: u32,
    pub pongs: u32,
    pub client_chunk: usize,
    pub unpublished: bool,
}

pub(crate) struct Server {
    pub port: u16,
    pub got: Arc<Mutex<Vec<Got>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    /// The publishers so far (the last one still filling in).
    pub(crate) fn got(&self) -> Vec<Got> {
        self.got.lock().unwrap().clone()
    }

    /// Until `f` holds for what was got, or `limit`.
    pub(crate) async fn wait(&self, limit: Duration, f: impl Fn(&[Got]) -> bool) -> bool {
        let end = tokio::time::Instant::now() + limit;
        while tokio::time::Instant::now() < end {
            if f(&self.got()) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        f(&self.got())
    }
}

/// A self-signed certificate for "localhost": the acceptor and the roots that trust it.
pub(crate) fn tls_pair() -> (tokio_rustls::TlsAcceptor, rustls::RootCertStore) {
    use rcgen::{CertificateParams, KeyPair};
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    let cert = params.self_signed(&key).unwrap();
    let der = cert.der().clone();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(der.clone()).unwrap();
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![der],
        rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
    )
    .unwrap();
    (tokio_rustls::TlsAcceptor::from(Arc::new(cfg)), roots)
}

/// Listens on 127.0.0.1 (a free port) and takes publishers one after another.
pub(crate) async fn start(opts: Opts, tls: Option<tokio_rustls::TlsAcceptor>) -> Server {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = got.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = l.accept().await else {
                return;
            };
            let g = g.clone();
            let opts = opts.clone();
            let tls = tls.clone();
            tokio::spawn(async move {
                let i = {
                    let mut v = g.lock().unwrap();
                    v.push(Got::default());
                    v.len() - 1
                };
                match tls {
                    Some(t) => {
                        if let Ok(s) = t.accept(tcp).await {
                            serve(s, &opts, &g, i).await;
                        }
                    }
                    None => serve(tcp, &opts, &g, i).await,
                }
            });
        }
    });
    Server { port, got, task }
}

fn send(out: &mut Vec<u8>, opts: &Opts, csid: u8, typ: u8, msid: u32, body: &[u8]) {
    rtmp::chunk_message(out, csid, 0, typ, msid, body, opts.chunk);
}

fn status(level: &str, code: &str, description: &str) -> Vec<u8> {
    amf::encode(&[
        Amf::str("onStatus"),
        Amf::Number(0.0),
        Amf::Null,
        Amf::obj(&[
            ("level", Amf::str(level)),
            ("code", Amf::str(code)),
            ("description", Amf::str(description)),
        ]),
    ])
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut s: S,
    opts: &Opts,
    got: &Mutex<Vec<Got>>,
    i: usize,
) {
    let edit = |f: &mut dyn FnMut(&mut Got)| {
        if let Some(g) = got.lock().unwrap().get_mut(i) {
            f(g);
        }
    };
    if opts.silent {
        let mut b = [0u8; 4096];
        while matches!(s.read(&mut b).await, Ok(n) if n > 0) {}
        return;
    }
    let mut c0c1 = vec![0u8; 1537];
    if s.read_exact(&mut c0c1).await.is_err() || c0c1[0] != 3 {
        return;
    }
    let mut s012 = vec![3u8];
    s012.extend_from_slice(&[0x5a; 1536]);
    s012.extend_from_slice(&c0c1[1..]);
    if s.write_all(&s012).await.is_err() {
        return;
    }
    let mut c2 = vec![0u8; 1536];
    if s.read_exact(&mut c2).await.is_err() {
        return;
    }
    let mut rd = ChunkReader::default();
    let mut buf = vec![0u8; 8192];
    let mut media = 0usize;
    loop {
        let n = match s.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        rd.push(&buf[..n]);
        let mut out = Vec::new();
        loop {
            let m = match rd.next() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(_) => return,
            };
            match m.typ {
                rtmp::MSG_SET_CHUNK_SIZE => edit(&mut |g| g.client_chunk = rd_size(&m)),
                rtmp::MSG_ACK => edit(&mut |g| g.acks += 1),
                rtmp::MSG_USER_CONTROL if m.body.get(..2) == Some(&[0, 7]) => {
                    edit(&mut |g| g.pongs += 1)
                }
                8 | 9 | 18 => {
                    let m2 = m.clone();
                    edit(&mut |g| g.media.push(m2.clone()));
                    media += 1;
                    if opts.close_after.is_some_and(|c| media >= c) {
                        return;
                    }
                }
                rtmp::MSG_COMMAND => {
                    let v = amf::decode(&m.body);
                    let name = v.first().and_then(Amf::as_str).unwrap_or("").to_string();
                    let txn = v.get(1).cloned().unwrap_or(Amf::Number(0.0));
                    match name.as_str() {
                        "connect" => {
                            let o = v.get(2).cloned().unwrap_or(Amf::Null);
                            let field =
                                |k: &str| o.get(k).and_then(Amf::as_str).unwrap_or("").to_string();
                            let (app, tc, fv) = (field("app"), field("tcUrl"), field("flashVer"));
                            edit(&mut |g| {
                                g.app = app.clone();
                                g.tc_url = tc.clone();
                                g.flash_ver = fv.clone();
                            });
                            send(
                                &mut out,
                                opts,
                                2,
                                rtmp::MSG_WINDOW_ACK,
                                0,
                                &opts.window.to_be_bytes(),
                            );
                            let mut pb = opts.window.to_be_bytes().to_vec();
                            pb.push(2);
                            send(&mut out, opts, 2, rtmp::MSG_PEER_BANDWIDTH, 0, &pb);
                            send(
                                &mut out,
                                opts,
                                2,
                                rtmp::MSG_SET_CHUNK_SIZE,
                                0,
                                &(opts.chunk as u32).to_be_bytes(),
                            );
                            let (verb, level, code) = if opts.reject_connect {
                                ("_error", "error", "NetConnection.Connect.Rejected")
                            } else {
                                ("_result", "status", "NetConnection.Connect.Success")
                            };
                            let body = amf::encode(&[
                                Amf::str(verb),
                                txn,
                                Amf::obj(&[("fmsVer", Amf::str("FMS/3,0,1,123"))]),
                                Amf::obj(&[
                                    ("level", Amf::str(level)),
                                    ("code", Amf::str(code)),
                                    ("description", Amf::str("Connection succeeded.")),
                                ]),
                            ]);
                            send(&mut out, opts, 3, rtmp::MSG_COMMAND, 0, &body);
                            // onBWDone, as Wowza-style servers send right after.
                            let bw =
                                amf::encode(&[Amf::str("onBWDone"), Amf::Number(0.0), Amf::Null]);
                            send(&mut out, opts, 3, rtmp::MSG_COMMAND, 0, &bw);
                        }
                        // Some ingests refuse releaseStream: the client must not care.
                        "releaseStream" => {
                            let body =
                                amf::encode(&[Amf::str("_error"), txn, Amf::Null, Amf::Null]);
                            send(&mut out, opts, 3, rtmp::MSG_COMMAND, 0, &body);
                        }
                        "FCPublish" => {
                            let body = amf::encode(&[
                                Amf::str("onFCPublish"),
                                Amf::Number(0.0),
                                Amf::Null,
                            ]);
                            send(&mut out, opts, 3, rtmp::MSG_COMMAND, 0, &body);
                        }
                        "createStream" => {
                            let body = amf::encode(&[
                                Amf::str("_result"),
                                txn,
                                Amf::Null,
                                Amf::Number(1.0),
                            ]);
                            send(&mut out, opts, 3, rtmp::MSG_COMMAND, 0, &body);
                        }
                        "publish" => {
                            let stream = v.get(3).and_then(Amf::as_str).unwrap_or("").to_string();
                            let ty = v.get(4).and_then(Amf::as_str).unwrap_or("").to_string();
                            let msg = if opts.reject_publish {
                                status(
                                    "error",
                                    "NetStream.Publish.BadName",
                                    &format!("Stream {stream} is already publishing"),
                                )
                            } else {
                                status("status", "NetStream.Publish.Start", "Start publishing")
                            };
                            edit(&mut |g| {
                                g.stream = stream.clone();
                                g.publish_type = ty.clone();
                                g.published = !opts.reject_publish;
                            });
                            send(&mut out, opts, 5, rtmp::MSG_COMMAND, m.msid, &msg);
                            if opts.ping && !opts.reject_publish {
                                send(
                                    &mut out,
                                    opts,
                                    2,
                                    rtmp::MSG_USER_CONTROL,
                                    0,
                                    &[0, 6, 0, 0, 0x12, 0x34],
                                );
                            }
                        }
                        "FCUnpublish" | "deleteStream" => edit(&mut |g| g.unpublished = true),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if !out.is_empty() && s.write_all(&out).await.is_err() {
            return;
        }
    }
}

fn rd_size(m: &Message) -> usize {
    m.body
        .get(..4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
        .unwrap_or(0)
}
