//! The client side of a session (SPEC.md §6–§8).
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::AbortHandle;

use crate::conn::{Frame, FrameReader, FrameWriter};
use crate::cpace;
use crate::gen::{
    self, Bye, Join, JoinAck, PairConfirm, PairPakeClient, PairPakeServer, PairResult, RpcRequest,
    RpcResponse,
};
use crate::handshake::{self, Established};
use crate::keys::{self, Identity, SessionKeys};
use crate::link::{drive, Link, Outbox, DELIVER_DEPTH};
use crate::peers::PeerStore;
use crate::router::{is_data_type, BoxFut, ConnTx, JobId, JobLink, LaneOpener, Router};
use crate::wire::{FrameMessage, Message};
use crate::Ava1Error;

/// The longest an RPC waits for its reply when the caller names no bound.
pub const RPC_TIMEOUT: Duration = Duration::from_secs(60);

/// `disk.calibrate` writes and fsyncs up to 20,000 files five times over.
const CALIBRATE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub ping_every: Duration,
    /// No byte received (or taken by the peer) for this long: the connection is dead.
    pub dead_after: Duration,
    /// The whole handshake, from accept/connect to Welcome.
    pub handshake: Duration,
    /// Bytes/s one frame must at least move at, after a `dead_after` grace (SPEC.md §6).
    pub min_frame_rate: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            ping_every: Duration::from_secs(2),
            dead_after: Duration::from_secs(12),
            handshake: Duration::from_secs(10),
            min_frame_rate: crate::link::MIN_FRAME_RATE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcReply {
    pub status: u16,
    pub body: Vec<u8>,
}

type Pending = Arc<Mutex<HashMap<u32, oneshot::Sender<Frame>>>>;

pub struct Session {
    pub(crate) addr: SocketAddr,
    pub(crate) timing: Timing,
    pub(crate) est: Established,
    peers: Arc<Mutex<PeerStore>>,
    outbox: Outbox,
    link: Link,
    pending: Pending,
    next_req: AtomicU32,
    router: Arc<Router>,
    joiner: Arc<Joiner>,
    dispatcher: AbortHandle,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("addr", &self.addr)
            .field("peer_name", &self.est.peer_name)
            .field("closed", &self.link.is_closed())
            .finish_non_exhaustive()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Jobs hear about the session's end before the dispatcher goes away.
        self.router.close("the session was closed");
        self.dispatcher.abort();
    }
}

async fn within<T>(
    limit: Duration,
    f: impl std::future::Future<Output = Result<T, Ava1Error>>,
) -> Result<T, Ava1Error> {
    tokio::time::timeout(limit, f)
        .await
        .map_err(|_| Ava1Error::Timeout)?
}

/// Removes a pending request on every exit, including the caller dropping the future.
struct PendingEntry<'a> {
    pending: &'a Pending,
    id: u32,
}

impl Drop for PendingEntry<'_> {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.id);
    }
}

pub async fn connect(
    addr: &str,
    me: Arc<Identity>,
    peers: Arc<Mutex<PeerStore>>,
    my_name: &str,
    timing: Timing,
) -> Result<Session, Ava1Error> {
    connect_expecting(addr, None, me, peers, my_name, timing).await
}

/// `connect`, but only to the device whose key is `expected_key` (when `Some`). Another
/// device at the same address — a second console that took over the IP — is refused
/// with `WrongPeer` before it learns who we are.
pub async fn connect_expecting(
    addr: &str,
    expected_key: Option<[u8; 32]>,
    me: Arc<Identity>,
    peers: Arc<Mutex<PeerStore>>,
    my_name: &str,
    timing: Timing,
) -> Result<Session, Ava1Error> {
    // One deadline for the TCP connect and the whole handshake.
    let deadline = tokio::time::Instant::now() + timing.handshake;
    let stream = tokio::time::timeout_at(deadline, TcpStream::connect(addr))
        .await
        .map_err(|_| Ava1Error::Timeout)??;
    stream.set_nodelay(true)?;
    let peer_addr = stream.peer_addr()?;
    let (rh, wh) = stream.into_split();
    let (mut r, mut w) = (FrameReader::new(rh), FrameWriter::new(wh));
    let est = tokio::time::timeout_at(
        deadline,
        handshake::client_launched(
            &mut r,
            &mut w,
            &me,
            my_name,
            expected_key,
            |k| peers.lock().unwrap().contains(k),
            |h, proof| peers.lock().unwrap().launched_by_us(h, proof),
            gen::CAP_DATA_PLANE,
        ),
    )
    .await
    .map_err(|_| Ava1Error::Timeout)??;
    if est.launched {
        // The helper we launched (SPEC.md §5.2): remember it like a confirmed pairing.
        // A key that cannot be stored still leaves this session paired — the proof
        // holds — and the token pairs the next session too until it expires.
        let _ = peers.lock().unwrap().add(est.peer_key, &est.peer_name);
    }
    // Replies up to `RPC_REPLY_MAX` arrive on this connection (SPEC.md §7.4).
    r.set_max_body((crate::frame::RPC_REPLY_MAX + crate::frame::RPC_FRAME_SLACK) as u32);
    let (tx, mut rx) = mpsc::channel(DELIVER_DEPTH);
    let (link, outbox) = drive(r, w, timing, tx);
    let router = Arc::new(Router::default());
    let joiner = Arc::new(Joiner {
        addr: peer_addr,
        timing,
        keys: est.keys.clone(),
        session_id: est.session_id,
        paired: AtomicBool::new(est.pairing.is_none()),
        lanes_live: Arc::default(),
        router: router.clone(),
        held: Mutex::default(),
    });
    let pending: Pending = Arc::default();
    let p2 = pending.clone();
    let r2 = router.clone();
    let dispatcher = tokio::spawn(async move {
        while let Some(f) = rx.recv().await {
            if f.ty == RpcResponse::TYPE || f.ty == PairResult::TYPE || f.ty == PairPakeServer::TYPE
            {
                let waiter = p2.lock().unwrap().remove(&f.channel);
                if let Some(tx) = waiter {
                    let _ = tx.send(f);
                }
            } else if is_data_type(f.ty) {
                let _ = r2.route_control(f).await; // a late frame for a finished job is dropped
            }
        }
        p2.lock().unwrap().clear();
        r2.close("the session ended");
    })
    .abort_handle();
    Ok(Session {
        addr: peer_addr,
        timing,
        est,
        peers,
        outbox,
        link,
        pending,
        next_req: AtomicU32::new(1),
        router,
        joiner,
        dispatcher,
    })
}

impl Session {
    pub fn peer_key(&self) -> [u8; 32] {
        self.est.peer_key
    }

    pub fn peer_name(&self) -> &str {
        &self.est.peer_name
    }

    /// True while the devices are not yet paired and a person must type the code the console
    /// shows. The code itself is not known to this side: it exists only on the console's
    /// screen (SPEC.md §5.5).
    pub fn pairing_pending(&self) -> bool {
        self.est.pairing.is_some()
    }

    pub fn rtt(&self) -> Option<Duration> {
        self.link.rtt()
    }

    pub fn is_closed(&self) -> bool {
        self.link.is_closed()
    }

    /// Waits until the session ends; returns why.
    pub async fn closed(&self) -> String {
        self.link.closed().await
    }

    async fn request<M: FrameMessage + Send + Sync + 'static>(
        &self,
        m: M,
    ) -> Result<Frame, Ava1Error> {
        let id = self.next_req.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        // Removes the entry on every exit, including the caller dropping this future.
        let _entry = PendingEntry {
            pending: &self.pending,
            id,
        };
        // Queued whole or not at all: dropping this future never tears a frame.
        self.outbox.send(id, &m).await?;
        let mut rx = rx;
        tokio::select! {
            f = &mut rx => f.map_err(|_| Ava1Error::Lost(self.link.reason())),
            why = self.link.closed() => {
                // The reply may have arrived in the same instant as the close (a server
                // that answers and hangs up): the dispatcher drains the frames already
                // delivered, then drops our sender. Give it that moment before giving up.
                let late = tokio::time::timeout(Duration::from_millis(250), rx).await;
                match late {
                    Ok(Ok(f)) => Ok(f),
                    _ => Err(Ava1Error::Lost(why)),
                }
            }
        }
    }

    /// Calls `method`. Refused locally (`NotPaired`) until the pairing is confirmed:
    /// before that the other device is unverified, and nothing but PairConfirm is sent
    /// to it.
    pub async fn rpc(&self, method: u16, body: &[u8]) -> Result<RpcReply, Ava1Error> {
        if self.est.pairing.is_some() {
            return Err(Ava1Error::NotPaired);
        }
        self.rpc_unchecked(method, body).await
    }

    /// `rpc` without the pairing check, to observe the server's own refusal. Tests only.
    #[doc(hidden)]
    pub async fn rpc_unchecked_for_test(
        &self,
        method: u16,
        body: &[u8],
    ) -> Result<RpcReply, Ava1Error> {
        self.rpc_unchecked(method, body).await
    }

    async fn rpc_unchecked(&self, method: u16, body: &[u8]) -> Result<RpcReply, Ava1Error> {
        self.rpc_unchecked_within(method, body, RPC_TIMEOUT).await
    }

    /// `rpc` that gives up after `within` (`Ava1Error::Timeout`): a live link whose peer never
    /// answers must not park the caller for ever. The request is withdrawn on the way out.
    pub async fn rpc_within(
        &self,
        method: u16,
        body: &[u8],
        within: Duration,
    ) -> Result<RpcReply, Ava1Error> {
        if self.est.pairing.is_some() {
            return Err(Ava1Error::NotPaired);
        }
        self.rpc_unchecked_within(method, body, within).await
    }

    async fn rpc_unchecked_within(
        &self,
        method: u16,
        body: &[u8],
        within: Duration,
    ) -> Result<RpcReply, Ava1Error> {
        let f = tokio::time::timeout(
            within,
            self.request(RpcRequest {
                method,
                body: body.to_vec(),
            }),
        )
        .await
        .map_err(|_| Ava1Error::Timeout)??;
        let r: RpcResponse = f.decode()?;
        Ok(RpcReply {
            status: r.status,
            body: r.body,
        })
    }

    pub async fn node_info(&self) -> Result<gen::NodeInfo, Ava1Error> {
        let r = self.rpc(gen::METHOD_NODE_INFO, &[]).await?;
        if r.status != gen::STATUS_OK {
            return Err(Ava1Error::Refused {
                code: r.status,
                message: "node.info failed".into(),
            });
        }
        Ok(gen::NodeInfo::decode(&r.body)?)
    }

    /// Measures file creation and sync cost at 1, 2, 4, 8 and 16 workers.
    pub async fn calibrate(
        &self,
        dir: &str,
        files: u32,
        size: u32,
    ) -> Result<Vec<gen::CalPoint>, Ava1Error> {
        let body = gen::DiskCalibrate {
            dir: dir.into(),
            files,
            size,
        }
        .to_bytes()?;
        // Five disk points over up to 20,000 files: ~90 s on a console's internal drive, far
        // past the default bound.
        let reply = self
            .rpc_within(gen::METHOD_DISK_CALIBRATE, &body, CALIBRATE_TIMEOUT)
            .await?;
        if reply.status != gen::STATUS_OK {
            return Err(Ava1Error::Refused {
                code: reply.status,
                message: match String::from_utf8(reply.body) {
                    Ok(m) if !m.is_empty() => m,
                    _ => "disk.calibrate failed".into(),
                },
            });
        }
        Ok(gen::DiskCalibrateResult::decode(&reply.body)?.points)
    }

    /// Asks the other device (which must already trust us) to accept new pairings for
    /// `seconds` — the app's "Pair another device".
    pub async fn open_pairing(&self, seconds: u16) -> Result<(), Ava1Error> {
        let body = gen::PairingOpen { seconds }.to_bytes()?;
        let r = self.rpc(gen::METHOD_PAIRING_OPEN, &body).await?;
        if r.status != gen::STATUS_OK {
            return Err(Ava1Error::Refused {
                code: r.status,
                message: "pairing.open refused".into(),
            });
        }
        Ok(())
    }

    /// Passkey entry (SPEC.md §5.5): `typed` is the code the user read off the console's
    /// screen. It is the password of a PAKE (CPace) on this session: both sides derive a
    /// generator from the handshake hash and the code, exchange public values, and prove
    /// they derived the same key. The console stores this device only if our proof verifies
    /// (so a wrong code, which this side cannot detect, is refused by the console), and this
    /// side stores the console only if the console's proof verifies (so a fake console that
    /// does not know the code is refused here). A refusal ends the session: the next try
    /// is a new handshake with a new code on the console. A server that already trusts us
    /// (the launch path) needs no code: use `confirm_trusted`.
    pub async fn confirm_pairing(&mut self, typed: u32) -> Result<(), Ava1Error> {
        let Some(p) = self.est.pairing else {
            return Ok(());
        };
        if p.server_must_confirm {
            let wrong = |why: &str| Ava1Error::Refused {
                code: gen::ERR_PAIRING_CODE,
                message: why.into(),
            };
            if typed >= 1_000_000 {
                return Err(wrong("the code is six digits"));
            }
            let h = self.est.keys.hash;
            let g = cpace::generator(&h, typed);
            // Wiped on every path out, including a failed request.
            let x = zeroize::Zeroizing::new(keys::random_bytes::<32>()?);
            let ya = cpace::public(&x, &g).ok_or_else(|| wrong("degenerate pairing value"))?;
            let yb: PairPakeServer = self.request(PairPakeClient { y: ya }).await?.decode()?;
            let k = cpace::key(&h, &x, &yb.y, &ya, &yb.y)
                .map(zeroize::Zeroizing::new)
                .ok_or_else(|| wrong("degenerate pairing value"))?;
            drop(x);
            let mac = cpace::mac(&k, b"client", &h);
            let r: PairResult = self.request(PairConfirm { mac }).await?.decode()?;
            if r.accepted == 0 {
                return Err(wrong("the console did not accept that code"));
            }
            if !cpace::ct_eq32(&r.mac, &cpace::mac(&k, b"server", &h)) {
                return Err(wrong("the console could not prove it knows the code"));
            }
        }
        self.store_peer()
    }

    /// Records a console that already trusts us and showed no code (we launched it, or its
    /// trust slot holds our key). Refused if the console wants a code.
    pub fn confirm_trusted(&mut self) -> Result<(), Ava1Error> {
        match self.est.pairing {
            None => Ok(()),
            Some(p) if p.server_must_confirm => Err(Ava1Error::NotPaired),
            Some(_) => self.store_peer(),
        }
    }

    fn store_peer(&mut self) -> Result<(), Ava1Error> {
        self.peers
            .lock()
            .unwrap()
            .add(self.est.peer_key, &self.est.peer_name)?;
        self.est.pairing = None;
        self.joiner.paired.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Says goodbye and ends the session. Bounded: a peer that has stopped reading
    /// cannot hold it up for more than about a second.
    pub async fn close(self) {
        let limit = self.timing.dead_after.min(Duration::from_secs(1));
        let _ = tokio::time::timeout(limit, self.outbox.send(0, &Bye { reason: 0 })).await;
        self.outbox.flush(limit).await;
    }
}

pub struct Lane {
    pub id: u16,
    link: Link,
    _outbox: Outbox,
    _slot: Option<LaneSlot>,
}

/// Holds a lane id in `lanes_live`; frees it on every exit, including a dropped future.
struct LaneSlot {
    live: Arc<Mutex<[bool; 9]>>,
    id: u16,
}

impl Drop for LaneSlot {
    fn drop(&mut self) {
        self.live.lock().unwrap()[self.id as usize] = false;
    }
}

impl Lane {
    pub async fn closed(&self) -> String {
        self.link.closed().await
    }

    pub fn is_closed(&self) -> bool {
        self.link.is_closed()
    }

    pub fn rtt(&self) -> Option<Duration> {
        self.link.rtt()
    }
}

/// Everything needed to join lanes, shared with jobs (SPEC.md §9, §12).
/// What a lane asks the kernel for on each socket buffer (review 003 §1): a thread that
/// is not in `recv` for 10-20 ms (the console opens a frame) must not close the sender's
/// TCP window, and on Wi-Fi the per-lane window is today's ceiling.
pub(crate) const LANE_SOCKBUF: u32 = 4 << 20;

/// Asks for `LANE_SOCKBUF` each way; returns the effective sizes. A refused request is
/// not fatal: the kernel's default buffers still work.
pub(crate) fn tune_lane_socket(sock: &tokio::net::TcpSocket) -> (Option<u32>, Option<u32>) {
    let _ = sock.set_recv_buffer_size(LANE_SOCKBUF);
    let _ = sock.set_send_buffer_size(LANE_SOCKBUF);
    (sock.recv_buffer_size().ok(), sock.send_buffer_size().ok())
}

/// Connects a lane socket with 4 MiB send and receive buffers set before the connect (so
/// the window scale is negotiated for them). Takes what the kernel gives; the effective
/// sizes are logged once per process.
pub(crate) async fn connect_lane_socket(addr: SocketAddr) -> std::io::Result<TcpStream> {
    use tokio::net::TcpSocket;
    let sock = if addr.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    let (rcv, snd) = tune_lane_socket(&sock);
    static LOGGED: std::sync::Once = std::sync::Once::new();
    LOGGED.call_once(|| {
        use std::io::Write;
        // writeln!, not eprintln!: a dead parent's closed stderr must not panic us.
        let _ = writeln!(
            std::io::stderr(),
            "[ava1] lane socket buffers: asked {} each, kernel gave rcv {:?} snd {:?}",
            LANE_SOCKBUF,
            rcv,
            snd
        );
    });
    sock.connect(addr).await
}

pub(crate) struct Joiner {
    addr: SocketAddr,
    timing: Timing,
    keys: SessionKeys,
    session_id: [u8; 16],
    paired: AtomicBool,
    lanes_live: Arc<Mutex<[bool; 9]>>,
    router: Arc<Router>,
    held: Mutex<HashMap<u16, Lane>>,
}

impl Joiner {
    async fn open_lane_at(&self, addr: SocketAddr) -> Result<Lane, Ava1Error> {
        if !self.paired.load(Ordering::SeqCst) {
            return Err(Ava1Error::NotPaired);
        }
        let id = {
            let mut live = self.lanes_live.lock().unwrap();
            let Some(i) = (1..=gen::MAX_LANES as usize).find(|i| !live[*i]) else {
                return Err(Ava1Error::Refused {
                    code: gen::ERR_BUSY,
                    message: "all 8 lanes are open".into(),
                });
            };
            live[i] = true;
            i as u16
        };
        let slot = LaneSlot {
            live: self.lanes_live.clone(),
            id,
        };
        let (link, outbox) = self.join(id, addr).await?;
        Ok(Lane {
            id,
            link,
            _outbox: outbox,
            _slot: Some(slot),
        })
    }

    async fn join(&self, id: u16, addr: SocketAddr) -> Result<(Link, Outbox), Ava1Error> {
        let t = self.timing.handshake;
        let stream = within(t, async { Ok(connect_lane_socket(addr).await?) }).await?;
        stream.set_nodelay(true)?;
        let (rh, wh) = stream.into_split();
        let (mut r, mut w) = (FrameReader::new(rh), FrameWriter::new(wh));
        let client_nonce: [u8; 16] = keys::random_bytes()?;
        let (k, sid) = (&self.keys, self.session_id);
        let tag = keys::join_tag(&k.c2s, &sid, id, &client_nonce);
        w.send_msg(
            0,
            &Join {
                session_id: sid,
                lane_id: id,
                client_nonce,
                tag,
            },
        )
        .await?;
        let f = within(t, r.recv()).await?;
        if f.ty == gen::Error::TYPE {
            return Err(handshake::refused(&f));
        }
        let ack: JoinAck = f.decode()?;
        let want = keys::join_ack_tag(&k.s2c, &sid, id, &client_nonce, &ack.server_nonce);
        if ack.lane_id != id || !keys::ct_eq16(&ack.tag, &want) {
            return Err(Ava1Error::BadTag);
        }
        w.set_key(keys::lane_key(&k.c2s, id, &client_nonce, &ack.server_nonce));
        r.set_key(keys::lane_key(&k.s2c, id, &client_nonce, &ack.server_nonce));
        // Lane frames may be as large as the frame cap (16 MiB), not the control cap.
        r.set_max_body(crate::frame::MAX_BODY);
        let (tx, mut rx) = mpsc::channel::<Frame>(DELIVER_DEPTH);
        let (link, outbox) = drive(r, w, self.timing, tx);
        // The server keeps an older connection of this lane until this one has shown it
        // holds the lane key (SPEC.md §9): a sealed Ping, now, makes the takeover prompt.
        let _ = outbox.try_ping(0);
        let gen_no = self.router.lane_up(id, outbox.clone());
        let router = self.router.clone();
        // Ends when the lane's reader ends (close, death or drop): then the lane is down.
        tokio::spawn(async move {
            while let Some(f) = rx.recv().await {
                if is_data_type(f.ty) {
                    router.route_lane(id, f).await;
                }
            }
            router.lane_down(id, gen_no);
        });
        Ok((link, outbox))
    }
}

impl LaneOpener for Joiner {
    fn open(&self) -> BoxFut<'_, Result<u16, Ava1Error>> {
        Box::pin(async move {
            let lane = self.open_lane_at(self.addr).await?;
            let id = lane.id;
            self.held.lock().unwrap().insert(id, lane);
            Ok(id)
        })
    }

    fn close(&self, id: u16) {
        // Dropping the Lane aborts its reader; the routing task then reports it down.
        self.held.lock().unwrap().remove(&id);
    }
}

impl Session {
    /// Opens a data lane on the lowest free id (1..=8).
    pub async fn open_lane(&self) -> Result<Lane, Ava1Error> {
        self.joiner.open_lane_at(self.addr).await
    }

    /// `open_lane` against another address. Tests only.
    #[doc(hidden)]
    pub async fn open_lane_at(&self, addr: SocketAddr) -> Result<Lane, Ava1Error> {
        self.joiner.open_lane_at(addr).await
    }

    /// Joins `id` again while it is still open here — what a reconnect after a
    /// dropped lane looks like to the server. Tests only.
    #[doc(hidden)]
    pub async fn reopen_lane_for_test(&self, id: u16) -> Result<Lane, Ava1Error> {
        let (link, outbox) = self.joiner.join(id, self.addr).await?;
        Ok(Lane {
            id,
            link,
            _outbox: outbox,
            _slot: None,
        })
    }

    /// The inbox and handles a job uses on this session.
    pub fn job(&self, job_id: JobId) -> JobLink {
        JobLink::new(
            job_id,
            self.router.clone(),
            ConnTx::new(self.outbox.clone()),
            Some(self.joiner.clone() as Arc<dyn LaneOpener>),
        )
    }

    pub fn peer_caps(&self) -> u64 {
        self.est.peer_caps
    }

    /// True when the node advertised `CAP_MGMT`: it serves the management methods (4 and up),
    /// so the engine routes management through AVA1 without probing for ERR_UNKNOWN_METHOD.
    pub fn has_mgmt(&self) -> bool {
        self.est.peer_caps & gen::CAP_MGMT != 0
    }

    /// True when the other device has not accepted us yet: a person must compare codes.
    /// False when it already trusts us (paired before, or it was launched by us).
    pub fn needs_user_pairing(&self) -> bool {
        self.est.pairing.is_some_and(|p| p.server_must_confirm)
    }
}

impl Session {
    /// The Join a client would send for `lane_id` with `client_nonce`. Tests only.
    #[doc(hidden)]
    pub fn join_frame_for_test(&self, lane_id: u16, client_nonce: [u8; 16]) -> Join {
        let sid = self.est.session_id;
        let tag = keys::join_tag(&self.est.keys.c2s, &sid, lane_id, &client_nonce);
        Join {
            session_id: sid,
            lane_id,
            client_nonce,
            tag,
        }
    }

    /// The (c2s, s2c) keys a lane joined with these nonces uses. Tests only.
    #[doc(hidden)]
    pub fn lane_keys_for_test(
        &self,
        lane_id: u16,
        client_nonce: &[u8; 16],
        server_nonce: &[u8; 16],
    ) -> ([u8; 32], [u8; 32]) {
        let k = &self.est.keys;
        (
            keys::lane_key(&k.c2s, lane_id, client_nonce, server_nonce),
            keys::lane_key(&k.s2c, lane_id, client_nonce, server_nonce),
        )
    }
}

#[cfg(test)]
mod sockbuf_tests {
    use super::*;

    #[test]
    fn liveness_defaults_are_a_twelve_second_verdict_on_a_two_second_ping() {
        // SPEC.md section 6; the payload's default (payload/src/ava1_glue.c) matches.
        let t = Timing::default();
        assert_eq!(t.ping_every, Duration::from_secs(2));
        assert_eq!(t.dead_after, Duration::from_secs(12));
    }

    #[tokio::test]
    async fn a_lane_socket_gets_big_buffers_and_still_connects() {
        let sock = tokio::net::TcpSocket::new_v4().unwrap();
        let (rcv, snd) = tune_lane_socket(&sock);
        // The kernel may cap or double the request (Linux CI caps the send side at
        // wmem_max = 208 KiB, reported doubled; the PS5 gives 512 KiB), so assert only that
        // tuning lifted both well above a stock default (16-87 KiB), not the full 4 MiB ask.
        assert!(rcv.unwrap() >= 128 << 10, "rcv {rcv:?}");
        assert!(snd.unwrap() >= 128 << 10, "snd {snd:?}");
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let s = connect_lane_socket(l.local_addr().unwrap()).await.unwrap();
        assert!(s.peer_addr().is_ok());
    }
}
