//! The engine's side of the signalling hub (protocol v1): it hosts one room per Play Together that
//! has an internet invite (and one for the phone link's internet link), and passes each guest's offer and candidates to [`super::rtc`] and the
//! answers back. The hub sees the room, its secret and the SDP; never the invite key.
//!
//! JSON text frames over `wss://<hub>/ws`, each at most 16 KiB. Host → hub `{"v":1,"t":"host",
//! "room","secret"}` → `{"t":"hosted","room"}` (or `{"t":"error","why"}`); the hub then sends
//! `{"t":"guest","gid"}`, `{"t":"signal","from":gid,"data"}`, `{"t":"left","gid"}`; the host
//! sends `{"v":1,"t":"signal","to":gid,"data"}` and, at the end, `{"v":1,"t":"leave"}`. A WebSocket
//! ping goes every 20 s. A dropped connection is made again with backoff, with the same room and
//! secret (the hub gives the room back). The room closes when the invite does: Play Together
//! closed, or the invite expired with nobody from the internet still in.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message as WsMsg;

use super::rtc::Cmd;
use super::Kind;

/// The largest frame either side sends.
pub(crate) const FRAME_MAX: usize = 16 * 1024;
const PING_EVERY: Duration = Duration::from_secs(20);
/// Nothing from the hub for this long (it answers every ping): the connection is dead.
const QUIET_MAX: Duration = Duration::from_secs(50);
const CONNECT_WAIT: Duration = Duration::from_secs(10);
/// How often the room checks it is still wanted.
const CHECK_EVERY: Duration = Duration::from_secs(if cfg!(test) { 1 } else { 5 });
const BACKOFF_FIRST: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 30 });

pub(crate) trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
pub(crate) type Ws = tokio_tungstenite::WebSocketStream<Box<dyn Io>>;

#[derive(Debug)]
struct St {
    state: &'static str,
    error: Option<String>,
    hosted_once: bool,
    connects: u32,
}

pub(crate) struct Shared {
    id: Mutex<String>,
    secret: String,
    url: String,
    kind: Kind,
    st: Mutex<St>,
    /// When the room was asked for (how long the hub has had to answer).
    started: std::time::Instant,
}

/// The hub not hosting the room this long after it was asked for (or a failed connect): the
/// invite is made for the home network instead ([`Room::offline`]). A hub that answers does so in
/// well under a second; a computer without internet fails its lookup at once or times out.
pub(crate) const OFFLINE_AFTER: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 6 });

/// Whether the room is still wanted (asked every few seconds and before each connect).
pub(crate) type Wanted = Box<dyn Fn() -> bool + Send + Sync>;

impl Shared {
    fn id(&self) -> String {
        self.id.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn set(&self, state: &'static str, error: Option<String>) {
        let mut g = self.st.lock().unwrap_or_else(|e| e.into_inner());
        g.state = state;
        if error.is_some() || state == "hosted" {
            g.error = error;
        }
        if state == "hosted" {
            g.hosted_once = true;
        }
    }
}

/// A room on the hub, open while this lives (Play Together, or the phone link, holds it).
pub(crate) struct Room {
    shared: Arc<Shared>,
    _stop: watch::Sender<()>,
}

impl Room {
    /// Opens a room of `kind` on the hub at `url`, kept while `wanted` says so (and this lives).
    pub(crate) fn start(kind: Kind, wanted: Wanted, url: String) -> Option<Room> {
        let shared = Arc::new(Shared {
            id: Mutex::new(super::random_b64(16)?),
            secret: super::random_b64(32)?,
            url,
            kind,
            st: Mutex::new(St {
                state: "connecting",
                error: None,
                hosted_once: false,
                connects: 0,
            }),
            started: std::time::Instant::now(),
        });
        let (stop, stop_rx) = watch::channel(());
        tokio::spawn(run(shared.clone(), wanted, stop_rx));
        Some(Room {
            shared,
            _stop: stop,
        })
    }

    pub(crate) fn id(&self) -> String {
        self.shared.id()
    }

    pub(crate) fn url(&self) -> &str {
        &self.shared.url
    }

    /// The hub can't be reached: not hosting the room now, and a connect failed or it has had
    /// [`OFFLINE_AFTER`] to. The host's page then shares the home network's link (works on the
    /// Wi-Fi only) until the hub hosts the room again.
    pub(crate) fn offline(&self) -> bool {
        let st = self.shared.st.lock().unwrap_or_else(|e| e.into_inner());
        st.state != "hosted"
            && (st.error.is_some() || self.shared.started.elapsed() >= OFFLINE_AFTER)
    }

    /// The invite's internet half, as the host's page shows it (`key`: the invite's; `lan` and
    /// `ts`: this engine's pages on the home network and on Tailscale the link names, see
    /// [`super::link_for`]).
    pub(crate) fn json(&self, key: &str, lan: Option<&str>, ts: Option<&str>) -> serde_json::Value {
        let offline = self.offline();
        let id = self.id();
        let hub = super::hub_host(&self.shared.url).unwrap_or_default();
        let fp = super::fingerprint();
        let st = self.shared.st.lock().unwrap_or_else(|e| e.into_inner());
        // What the router did for this invite (#822): `state` (rtc::router_state), `via` and
        // `port` (outside) once a port is open, `local_port` the UDP port to forward by hand.
        let mapped = *super::rtc::NET_MAPPED
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let local_port = super::rtc::NET_PORT.load(std::sync::atomic::Ordering::Relaxed);
        let router = serde_json::json!({
            "state": super::rtc::router_state(crate::join::settings().upnp),
            "via": mapped.map(|(_, how)| how),
            "port": mapped.map(|(a, _)| a.port()),
            "local_port": (local_port != 0).then_some(local_port),
        });
        serde_json::json!({
            "url": fp.as_ref().map(|fp| super::link_for(self.shared.kind, &id, key, fp, &hub, lan, ts)),
            "room": id,
            "hub": hub,
            "fp": fp,
            "state": st.state,
            "error": st.error,
            // The hub can't be reached (Room::offline): share the home network's link instead.
            "offline": offline,
            "router": router,
            "peers": super::peers_json(&id),
        })
    }
}

pub(crate) async fn connect(url: &str) -> Result<Ws, String> {
    let uri: axum::http::Uri = url.parse().map_err(|_| "bad hub address".to_string())?;
    let host = uri.host().ok_or("bad hub address")?.to_string();
    let tls = uri.scheme_str() == Some("wss");
    let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
    let config = WebSocketConfig::default()
        .max_message_size(Some(FRAME_MAX))
        .max_frame_size(Some(FRAME_MAX));
    tokio::time::timeout(CONNECT_WAIT, async {
        let tcp = tokio::net::TcpStream::connect((host.trim_matches(['[', ']']), port))
            .await
            .map_err(|e| format!("connect: {e}"))?;
        let _ = tcp.set_nodelay(true);
        let io: Box<dyn Io> = if tls {
            let name = rustls::pki_types::ServerName::try_from(host.clone())
                .map_err(|_| "bad hub name".to_string())?;
            let s = crate::pad_link::relay_tls()
                .ok_or("no TLS")?
                .connect(name, tcp)
                .await
                .map_err(|e| format!("TLS: {e}"))?;
            Box::new(s)
        } else {
            Box::new(tcp)
        };
        let (ws, _) = tokio_tungstenite::client_async_with_config(url, io, Some(config))
            .await
            .map_err(|e| format!("WebSocket: {e}"))?;
        Ok(ws)
    })
    .await
    .map_err(|_| "timed out".to_string())?
}

fn frame(v: serde_json::Value) -> WsMsg {
    WsMsg::Text(v.to_string().into())
}

/// How one connection to the hub ended.
enum End {
    /// The room closes (Play Together or the invite did).
    Stop,
    /// Connect again after the backoff.
    Again,
    /// Connect again at once (a new room id after `room_taken`).
    Now,
}

async fn run(shared: Arc<Shared>, wanted: Wanted, mut stop: watch::Receiver<()>) {
    let Some(net) = super::rtc::net().await else {
        shared.set("error", Some("no network for internet guests".into()));
        return;
    };
    let kind = shared.kind;
    let mut room = shared.id();
    let _ = net.cmd.send(Cmd::RoomOpen(room.clone(), kind));
    let mut backoff = BACKOFF_FIRST;
    loop {
        if !wanted() {
            break;
        }
        shared.st.lock().unwrap_or_else(|e| e.into_inner()).connects += 1;
        let end = match connect(&shared.url).await {
            Ok(ws) => session(ws, &shared, &wanted, &net.cmd, &mut stop, &mut backoff).await,
            Err(e) => {
                crate::log_at!(Debug, "together", "join: hub unreachable: {e}");
                shared.set("reconnecting", Some(e));
                End::Again
            }
        };
        let wait = match end {
            End::Stop => break,
            End::Now => Duration::ZERO,
            End::Again => {
                let w = backoff;
                backoff = (backoff * 2).min(BACKOFF_MAX);
                if shared.st.lock().unwrap_or_else(|e| e.into_inner()).state == "hosted" {
                    shared.set("reconnecting", None);
                }
                w
            }
        };
        let id = shared.id();
        if id != room {
            let _ = net.cmd.send(Cmd::RoomClose(room));
            room = id;
            let _ = net.cmd.send(Cmd::RoomOpen(room.clone(), kind));
        }
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            r = stop.changed() => if r.is_err() { break },
        }
    }
    shared.set("closed", None);
    let _ = net.cmd.send(Cmd::RoomClose(room));
    crate::log_at!(Debug, "together", "join: internet room closed");
}

async fn session(
    mut ws: Ws,
    shared: &Arc<Shared>,
    wanted: &Wanted,
    cmd: &mpsc::UnboundedSender<Cmd>,
    stop: &mut watch::Receiver<()>,
    backoff: &mut Duration,
) -> End {
    let room = shared.id();
    let hello = serde_json::json!({"v": 1, "t": "host", "room": room, "secret": shared.secret});
    if ws.send(frame(hello)).await.is_err() {
        return End::Again;
    }
    let (sig_tx, mut sig_rx) = mpsc::unbounded_channel::<(String, serde_json::Value)>();
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut check = tokio::time::interval(CHECK_EVERY);
    let mut heard = tokio::time::Instant::now();
    loop {
        tokio::select! {
            r = stop.changed() => {
                if r.is_err() {
                    let _ = ws.send(frame(serde_json::json!({"v": 1, "t": "leave"}))).await;
                    let _ = ws.close(None).await;
                    return End::Stop;
                }
            }
            _ = check.tick() => {
                if !wanted() {
                    let _ = ws.send(frame(serde_json::json!({"v": 1, "t": "leave"}))).await;
                    let _ = ws.close(None).await;
                    return End::Stop;
                }
                if heard.elapsed() > QUIET_MAX {
                    shared.set("reconnecting", Some("the hub went quiet".into()));
                    return End::Again;
                }
            }
            _ = ping.tick() => {
                if ws.send(WsMsg::Ping(Vec::new().into())).await.is_err() {
                    return End::Again;
                }
            }
            s = sig_rx.recv() => {
                if let Some((to, data)) = s {
                    let f = frame(serde_json::json!({"v": 1, "t": "signal", "to": to, "data": data}));
                    if ws.send(f).await.is_err() {
                        return End::Again;
                    }
                }
            }
            m = ws.next() => {
                heard = tokio::time::Instant::now();
                let t = match m {
                    Some(Ok(WsMsg::Text(t))) => t,
                    Some(Ok(WsMsg::Close(_))) | None | Some(Err(_)) => {
                        crate::log_at!(Debug, "together", "join: hub connection lost");
                        return End::Again;
                    }
                    Some(Ok(_)) => continue,
                };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(t.as_str()) else {
                    continue;
                };
                let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                match v.get("t").and_then(|x| x.as_str()).unwrap_or("") {
                    "hosted" => {
                        crate::log_at!(Info, "together", "join: internet invite open on the hub");
                        shared.set("hosted", None);
                        *backoff = BACKOFF_FIRST;
                    }
                    "error" => {
                        let why = s("why");
                        crate::log_at!(Debug, "together", "join: the hub says {why}");
                        let hosted_once = shared.st.lock().unwrap_or_else(|e| e.into_inner()).hosted_once;
                        if why == "room_taken" && !hosted_once {
                            // Someone else's room by that name: another name, before anyone
                            // has the link.
                            if let Some(id) = super::random_b64(16) {
                                *shared.id.lock().unwrap_or_else(|e| e.into_inner()) = id;
                                return End::Now;
                            }
                        }
                        shared.set("error", Some(why));
                        if !matches!(shared.st.lock().unwrap_or_else(|e| e.into_inner()).state, "hosted") {
                            return End::Again;
                        }
                    }
                    "guest" => {
                        crate::log_at!(Debug, "together", "join: internet guest {} on the hub", s("gid"));
                    }
                    "signal" => {
                        let gid = s("from");
                        let data = v.get("data").cloned().unwrap_or_default();
                        let field = |k: &str| data.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                        if gid.is_empty() || gid.len() > 64 {
                            continue;
                        }
                        match field("type").as_str() {
                            "offer" => {
                                let _ = cmd.send(Cmd::Offer {
                                    room: room.clone(),
                                    gid,
                                    sdp: field("sdp"),
                                    signal: sig_tx.clone(),
                                });
                            }
                            "candidate" => {
                                let _ = cmd.send(Cmd::Remote {
                                    room: room.clone(),
                                    gid,
                                    candidate: field("candidate"),
                                });
                            }
                            _ => {}
                        }
                    }
                    "left" => {
                        let _ = cmd.send(Cmd::Left { room: room.clone(), gid: s("gid") });
                    }
                    _ => {}
                }
            }
        }
    }
}
