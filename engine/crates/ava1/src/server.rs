//! The server side: accept loop, control connections, pairing, RPC (SPEC.md §6–§8).
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};

use crate::conn::{Frame, FrameReader, FrameWriter};
use crate::cpace;
use crate::gen::{
    self, Hs1, Join, JoinAck, PairConfirm, PairPakeClient, PairPakeServer, PairResult, Ping, Pong,
    RpcRequest, RpcResponse,
};
use crate::handshake::{self, refuse, Admission};
use crate::keys::{self, Identity, SessionKeys};
use crate::launch::LaunchSecret;
use crate::link::{drive, Full, Outbox, DELIVER_DEPTH};
use crate::peers::PeerStore;
use crate::router::{is_data_type, job_of, ConnTx, JobHost, JobLink, Router};
use crate::session::{RpcReply, Timing};
use crate::wire::{FrameMessage, Message};
use crate::Ava1Error;

pub const MAX_CONNS: usize = 64;
pub const MAX_SESSIONS: usize = 16;
/// Jobs one session may have open at once; a `JobOpen` or `Resume` past it is answered
/// `ERR_BUSY` (review 006 #4). The console's job table is the same size.
pub const MAX_JOBS_PER_SESSION: usize = 32;
/// Calls in flight per session; more are answered `ERR_BUSY`.
pub const RPC_WORKERS: usize = 8;
pub use crate::frame::{RPC_REPLY_MAX, RPC_REQUEST_MAX};
/// The longest window `pairing.open` may ask for.
pub const MAX_PAIRING_WINDOW_S: u16 = 600;
/// Wrong guesses at the pairing code (SPEC.md §4.6) one source address may make per window.
pub const MAX_PAIR_FAILURES_PER_IP: u32 = 5;
/// Wrong guesses from everyone together, after which the window closes: it must be reopened
/// from a paired device (or by restarting the node). 20 guesses is about 2e-5 of the space.
pub const MAX_PAIR_FAILURES: u32 = 20;
/// New pairing sessions one source address may start per `WELCOME_WINDOW`.
pub const WELCOMES_PER_IP: u32 = 6;
pub const WELCOME_WINDOW: Duration = Duration::from_secs(10);
/// Connections one source address may hold (a session is 1 control + up to 8 lanes).
pub const MAX_CONNS_PER_IP: usize = 12;
/// Sessions that were welcomed during a pairing window but have not confirmed yet.
pub const MAX_UNPAIRED: usize = 2;
/// How long such a session may wait for its PairConfirm.
pub const PAIR_CONFIRM_DEADLINE: Duration = Duration::from_secs(60);
/// An identical pairing request (same address, same key) is shown at most once per this long;
/// every other session shows its own code.
pub const NOTIFY_EVERY: Duration = Duration::from_secs(10);

/// The server's admission limits (SPEC.md §8). Tests lower or raise them.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub conns_per_ip: usize,
    pub unpaired: usize,
    pub pair_confirm: Duration,
    pub notify_every: Duration,
    pub pair_fails_per_ip: u32,
    pub pair_fails_total: u32,
    pub welcomes_per_ip: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            conns_per_ip: MAX_CONNS_PER_IP,
            unpaired: MAX_UNPAIRED,
            pair_confirm: PAIR_CONFIRM_DEADLINE,
            notify_every: NOTIFY_EVERY,
            pair_fails_per_ip: MAX_PAIR_FAILURES_PER_IP,
            pair_fails_total: MAX_PAIR_FAILURES,
            welcomes_per_ip: WELCOMES_PER_IP,
        }
    }
}

/// What limits pairing attempts (SPEC.md §4.6): real guesses (the PAKE ran and the proof was
/// wrong) per source address and in all, and the rate of new pairing sessions per address.
/// A session that never completes the PAKE guessed nothing and is bounded only by the rate and
/// by `Limits::unpaired`.
#[derive(Debug, Default)]
struct PairBudget {
    total: u32,
    per_ip: HashMap<IpAddr, u32>,
    welcomes: HashMap<IpAddr, (Instant, u32)>,
}

impl PairBudget {
    fn reset(&mut self) {
        self.total = 0;
        self.per_ip.clear();
    }

    /// May `ip` still guess?
    fn guess_allowed(&self, ip: IpAddr, l: &Limits) -> bool {
        self.total < l.pair_fails_total
            && self.per_ip.get(&ip).copied().unwrap_or(0) < l.pair_fails_per_ip
    }

    /// A wrong guess from `ip`: (its count, whether the window must now close).
    fn guess_failed(&mut self, ip: IpAddr, l: &Limits) -> (u32, bool) {
        self.total += 1;
        let n = self.per_ip.entry(ip).or_insert(0);
        *n += 1;
        (*n, self.total >= l.pair_fails_total)
    }

    /// A new pairing session from `ip`: false when it already had its share this window.
    fn welcome_allowed(&mut self, ip: IpAddr, now: Instant, l: &Limits) -> bool {
        if self.welcomes.len() > 256 {
            self.welcomes
                .retain(|_, (t, _)| now.duration_since(*t) < WELCOME_WINDOW);
        }
        let e = self.welcomes.entry(ip).or_insert((now, 0));
        if now.duration_since(e.0) >= WELCOME_WINDOW {
            *e = (now, 0);
        }
        if e.1 >= l.welcomes_per_ip {
            return false;
        }
        e.1 += 1;
        true
    }
}

pub struct PairRequest {
    pub ip: IpAddr,
    pub peer_key: [u8; 32],
    pub peer_name: String,
    pub code: u32,
}

pub type RpcHandler = Box<dyn Fn(u16, &[u8]) -> RpcReply + Send + Sync>;
pub type PairHook = Box<dyn Fn(&PairRequest) -> bool + Send + Sync>;
pub type NotifyHook = Box<dyn Fn(&PairRequest) + Send + Sync>;
pub type LogHook = Box<dyn Fn(&str) + Send + Sync>;

pub(crate) struct SessionEntry {
    pub(crate) keys: SessionKeys,
    peer_key: [u8; 32],
    pub(crate) paired: AtomicBool,
    /// The six-digit code this session's console shows (random, never sent anywhere).
    code: u32,
    /// The PAKE key once the client's public value arrived (SPEC.md §5.5), taken by the confirm.
    pake: Mutex<Option<zeroize::Zeroizing<[u8; 32]>>>,
    pake_started: AtomicBool,
    pub(crate) router: Arc<Router>,
    /// Per lane id, how many connections have taken it over. A lane connection ends when
    /// its number is no longer the current one.
    lane_gen: watch::Sender<[u32; 9]>,
    pub(crate) nonces: Mutex<VecDeque<[u8; 16]>>,
    /// True once the session is over (its control connection ended, or the same device
    /// connected again): the control connection and every lane end at once.
    ended: watch::Sender<bool>,
    /// The session's connections, so their per-address counts can be given back the
    /// moment the session is superseded, before their tasks have wound down.
    conns: Mutex<Vec<Weak<ConnSlot>>>,
    /// Held while the session is welcomed but unconfirmed.
    unpaired: Mutex<Option<UnpairedSlot>>,
}

impl SessionEntry {
    fn adopt(&self, slot: &Arc<ConnSlot>) {
        let mut conns = self.conns.lock().unwrap();
        conns.retain(|c| c.strong_count() > 0);
        conns.push(Arc::downgrade(slot));
    }

    /// Ends the session now: wakes its connections' tasks and frees what it held.
    fn end(&self) {
        self.ended.send_replace(true);
        for c in self.conns.lock().unwrap().drain(..) {
            if let Some(c) = c.upgrade() {
                c.release_ip();
            }
        }
        self.unpaired.lock().unwrap().take();
    }
}

/// Resolves once the session is over.
async fn over(ended: &mut watch::Receiver<bool>) {
    let _ = ended.wait_for(|e| *e).await;
}

/// Resolves once `lane` has been taken over by a connection newer than `gen_no`.
async fn taken_over(gens: &mut watch::Receiver<[u32; 9]>, lane: usize, gen_no: u32) {
    let _ = gens.wait_for(|g| g[lane] != gen_no).await;
}

pub struct ServerCtx {
    identity: Identity,
    name: String,
    /// The key in this node's trust slot (SPEC.md §5.1): known without pairing.
    launcher: Option<[u8; 32]>,
    /// The slot's launch token, if it carried one (SPEC.md §5.2).
    launch: Option<LaunchSecret>,
    timing: Timing,
    limits: Limits,
    peers: Mutex<PeerStore>,
    pairing_until: Mutex<Option<Instant>>,
    notify: NotifyHook,
    /// Pairing requests shown lately, by (address, key): an identical repeat is not shown again.
    shown: Mutex<HashMap<(IpAddr, [u8; 32]), Instant>>,
    log: LogHook,
    /// An extra veto on top of the code check (never a substitute for it).
    approve: Option<PairHook>,
    pair_budget: Mutex<PairBudget>,
    rpc: RpcHandler,
    /// Hosts data-plane jobs (SPEC.md §11); its presence advertises CAP_DATA_PLANE.
    jobs: Option<Arc<dyn JobHost>>,
    /// Serves the management methods (SPEC.md §7.3); advertises CAP_MGMT.
    mgmt: bool,
    pub(crate) sessions: Mutex<HashMap<[u8; 16], Arc<SessionEntry>>>,
    conns: AtomicUsize,
    per_ip: Mutex<HashMap<IpAddr, usize>>,
    session_slots: AtomicUsize,
    unpaired: AtomicUsize,
}

impl ServerCtx {
    pub fn new(identity: Identity, name: &str, peers: PeerStore, rpc: RpcHandler) -> Self {
        Self {
            identity,
            name: name.to_string(),
            launcher: None,
            launch: None,
            timing: Timing::default(),
            limits: Limits::default(),
            peers: Mutex::new(peers),
            pairing_until: Mutex::new(None),
            notify: Box::new(|_| {}),
            shown: Mutex::default(),
            log: Box::new(|_| {}),
            approve: None,
            pair_budget: Mutex::default(),
            rpc,
            jobs: None,
            mgmt: false,
            sessions: Mutex::default(),
            conns: AtomicUsize::new(0),
            per_ip: Mutex::default(),
            session_slots: AtomicUsize::new(0),
            unpaired: AtomicUsize::new(0),
        }
    }

    /// Trusts `key` without pairing, as a payload trusts the key stamped into its trust
    /// slot (SPEC.md §5.1).
    pub fn with_launcher(mut self, key: [u8; 32]) -> Self {
        self.launcher = Some(key);
        self
    }

    /// `with_launcher`, for a slot that also carried a launch `token`: the launcher's
    /// Welcome then proves the token (SPEC.md §5.2).
    pub fn with_launch(mut self, key: [u8; 32], token: [u8; 16]) -> Self {
        self.launcher = Some(key);
        self.launch = Some(LaunchSecret { key, token });
        self
    }

    /// The launcher (the key from the trust slot) is known without the peers file: this
    /// server was handed that key by the code that launched it. The C payload keeps the
    /// same trust in its peers file instead (SPEC.md §5.1), so a peers write that fails
    /// leaves its launcher refused while this one still admits it — they differ only in
    /// that case, and only this harness's favour.
    fn is_known(&self, key: &[u8; 32]) -> bool {
        self.launcher.as_ref() == Some(key) || self.peers.lock().unwrap().contains(key)
    }

    pub fn with_limits(mut self, l: Limits) -> Self {
        self.limits = l;
        self
    }

    /// Where the server reports what an operator should know (a peers file it could not
    /// read, a pairing it could not store). Default: nowhere.
    pub fn with_log(mut self, f: LogHook) -> Self {
        self.log = f;
        self
    }

    pub fn with_timing(mut self, t: Timing) -> Self {
        self.timing = t;
        self
    }

    /// Serves the management methods through the rpc handler and advertises CAP_MGMT.
    pub fn with_mgmt(mut self) -> Self {
        self.mgmt = true;
        self
    }

    /// Hosts data-plane jobs on this server and advertises CAP_DATA_PLANE.
    pub fn with_jobs(mut self, host: Arc<dyn JobHost>) -> Self {
        self.jobs = Some(host);
        self
    }

    /// Called when an unknown device starts pairing (show `code` to the user).
    pub fn with_notify(mut self, f: NotifyHook) -> Self {
        self.notify = f;
        self
    }

    /// An additional owner veto on a PairConfirm. The typed code is always checked first;
    /// with no hook the code alone decides.
    pub fn with_approve(mut self, f: PairHook) -> Self {
        self.approve = Some(f);
        self
    }

    pub fn open_pairing(&self, d: Duration) {
        self.pair_budget.lock().unwrap().reset();
        *self.pairing_until.lock().unwrap() = Some(Instant::now() + d);
    }

    /// The automatic window: only a node with no paired peer opens one by itself
    /// (SPEC.md §5 item 6). Returns whether it opened. A peers file that exists but could
    /// not be read is not "no paired peer": the window stays shut, and it is logged.
    pub fn open_pairing_if_unpaired(&self, d: Duration) -> bool {
        let unpaired = {
            let peers = self.peers.lock().unwrap();
            if let Some(why) = peers.unreadable() {
                (self.log)(&format!(
                    "ava1: the peers file could not be read ({why}); pairing stays closed and the file is left alone"
                ));
                return false;
            }
            peers.list().is_empty()
        };
        if unpaired {
            self.open_pairing(d);
        }
        unpaired
    }

    pub fn close_pairing(&self) {
        *self.pairing_until.lock().unwrap() = None;
    }

    pub fn pairing_open(&self) -> bool {
        self.pairing_until
            .lock()
            .unwrap()
            .is_some_and(|t| Instant::now() < t)
    }

    /// Wrong guesses at the code since the window was last opened.
    pub fn pair_failures(&self) -> u32 {
        self.pair_budget.lock().unwrap().total
    }

    /// The code the console shows for the session of `peer` (tests: the console's screen).
    #[doc(hidden)]
    pub fn code_for_test(&self, peer: &[u8; 32]) -> Option<u32> {
        self.sessions
            .lock()
            .unwrap()
            .values()
            .find(|e| &e.peer_key == peer)
            .map(|e| e.code)
    }

    /// Whether `key` is a paired device.
    pub fn knows(&self, key: &[u8; 32]) -> bool {
        self.peers.lock().unwrap().contains(key)
    }

    pub fn connections(&self) -> usize {
        self.conns.load(Ordering::SeqCst)
    }

    pub fn sessions(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }

    /// One session per device (SPEC.md §8): a device that completes a new handshake
    /// replaces whatever session it had. A client reconnecting after its link died must
    /// not be refused because of its own dead connections, which linger until
    /// `dead_after`: they are ended here and their per-address counts given back at
    /// once.
    fn supersede(&self, peer_key: &[u8; 32]) {
        let old: Vec<Arc<SessionEntry>> = {
            let mut map = self.sessions.lock().unwrap();
            let ids: Vec<[u8; 16]> = map
                .iter()
                .filter(|(_, e)| &e.peer_key == peer_key)
                .map(|(id, _)| *id)
                .collect();
            ids.iter().filter_map(|id| map.remove(id)).collect()
        };
        for e in old {
            e.end();
        }
    }

    /// A wrong guess at the code: the PAKE ran and the client's proof did not verify. Logged
    /// per source address; its budget shrinks, and when everyone's guesses together reach the
    /// cap the window closes until a paired device (or a restart) reopens it. That address's
    /// next knock shows a new code at once.
    fn guess_failed(&self, req: &PairRequest) {
        let (n, close, total) = {
            let mut b = self.pair_budget.lock().unwrap();
            let (n, close) = b.guess_failed(req.ip, &self.limits);
            (n, close, b.total)
        };
        (self.log)(&format!(
            "ava1: pairing refused: wrong code from {} at {} ({n} of {} from this address, {total} of {} in all)",
            req.peer_name, req.ip, self.limits.pair_fails_per_ip, self.limits.pair_fails_total
        ));
        self.shown
            .lock()
            .unwrap()
            .retain(|(ip, _), _| *ip != req.ip);
        if close {
            self.close_pairing();
            (self.log)("ava1: too many wrong pairing codes: the window is closed");
        }
    }

    /// Decides a PairConfirm (SPEC.md §5.5). `proof` is `Some(ok)` when the PAKE ran and the
    /// client's confirmation was checked (`ok`: it verified, i.e. it knew the code the console
    /// shows), `None` when there was nothing to check (no PAKE before it, or it did not
    /// decode): refused, but not a guess, so not counted. A wrong proof is a counted guess.
    /// One window, one pairing: the window check, the store and the closing of the window
    /// happen under one lock, so two devices confirming at the same moment cannot both get
    /// in. An owner hook (which may wait on a person) is asked outside the lock, and the
    /// window checked again after it.
    fn accept_pairing(&self, req: &PairRequest, proof: Option<bool>) -> bool {
        if !self.pairing_open() {
            return false;
        }
        match proof {
            Some(true) => {}
            Some(false) => {
                self.guess_failed(req);
                return false;
            }
            None => return false,
        }
        if self.approve.as_ref().is_some_and(|a| !a(req)) {
            return false;
        }
        let mut until = self.pairing_until.lock().unwrap();
        if !until.is_some_and(|t| Instant::now() < t) {
            return false;
        }
        let stored = self.peers.lock().unwrap().add(req.peer_key, &req.peer_name);
        match stored {
            Ok(()) => {
                // Whoever else is waiting must ask again.
                *until = None;
                true
            }
            Err(e) => {
                (self.log)(&format!("ava1: pairing not stored: {e}"));
                false
            }
        }
    }

    /// Shows a pairing request: every session shows its own code (the user must always see the
    /// code of their attempt), except an identical repeat (same address and key) within
    /// `notify_every`. A flood is bounded by `Limits::unpaired` and the per-address rate of
    /// new pairing sessions, not by hiding codes.
    fn notify_session(&self, req: &PairRequest) {
        {
            let mut shown = self.shown.lock().unwrap();
            let now = Instant::now();
            if shown.len() > 64 {
                shown.retain(|_, t| now.duration_since(*t) < self.limits.notify_every);
            }
            let key = (req.ip, req.peer_key);
            if shown
                .get(&key)
                .is_some_and(|t| now.duration_since(*t) < self.limits.notify_every)
            {
                return;
            }
            shown.insert(key, now);
        }
        (self.notify)(req);
    }
}

/// One connection's place in the global and per-address counts.
struct ConnSlot {
    ctx: Arc<ServerCtx>,
    ip: IpAddr,
    /// Still counted against its address. Cleared early when its session is superseded.
    ip_held: AtomicBool,
}

impl ConnSlot {
    /// `Err` says which limit was hit.
    fn take(ctx: &Arc<ServerCtx>, ip: IpAddr) -> Result<Self, &'static str> {
        if ctx.conns.fetch_add(1, Ordering::SeqCst) >= MAX_CONNS {
            ctx.conns.fetch_sub(1, Ordering::SeqCst);
            return Err("too many connections");
        }
        let mut per_ip = ctx.per_ip.lock().unwrap();
        let n = per_ip.entry(ip).or_insert(0);
        if *n >= ctx.limits.conns_per_ip {
            if *n == 0 {
                per_ip.remove(&ip);
            }
            drop(per_ip);
            ctx.conns.fetch_sub(1, Ordering::SeqCst);
            return Err("too many connections from this address");
        }
        *n += 1;
        drop(per_ip);
        Ok(Self {
            ctx: ctx.clone(),
            ip,
            ip_held: AtomicBool::new(true),
        })
    }

    /// Gives back this connection's place in its address's count (once).
    fn release_ip(&self) {
        if !self.ip_held.swap(false, Ordering::SeqCst) {
            return;
        }
        let mut per_ip = self.ctx.per_ip.lock().unwrap();
        if let Some(n) = per_ip.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                per_ip.remove(&self.ip);
            }
        }
    }
}

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.release_ip();
        self.ctx.conns.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A welcomed but not yet confirmed session's place among `Limits::unpaired`.
struct UnpairedSlot(Arc<ServerCtx>);

impl UnpairedSlot {
    fn take(ctx: &Arc<ServerCtx>) -> Option<Self> {
        if ctx.unpaired.fetch_add(1, Ordering::SeqCst) >= ctx.limits.unpaired {
            ctx.unpaired.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(ctx.clone()))
    }
}

impl Drop for UnpairedSlot {
    fn drop(&mut self) {
        self.0.unpaired.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Accepts forever. Never exits on an accept error (a transient errno must not take
/// the server down); refuses connections past `MAX_CONNS`, or past
/// `Limits::conns_per_ip` from one address, with `ERR_BUSY`.
pub async fn serve(listener: TcpListener, ctx: Arc<ServerCtx>) {
    loop {
        let (s, from) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let slot = match ConnSlot::take(&ctx, from.ip()) {
            Ok(slot) => slot,
            Err(why) => {
                tokio::spawn(async move {
                    let (_, wh) = s.into_split();
                    let mut w = FrameWriter::new(wh);
                    let deadline = tokio::time::Instant::now() + FAREWELL;
                    refuse_by(deadline, &mut w, gen::ERR_BUSY, why).await;
                });
                continue;
            }
        };
        let (ctx, slot) = (ctx.clone(), Arc::new(slot));
        tokio::spawn(async move {
            let _ = handle(s, &ctx, &slot).await;
            drop(slot);
        });
    }
}

struct SessionSlot(Arc<ServerCtx>);

impl SessionSlot {
    fn take(ctx: &Arc<ServerCtx>) -> Option<Self> {
        if ctx.session_slots.fetch_add(1, Ordering::SeqCst) >= MAX_SESSIONS {
            ctx.session_slots.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(ctx.clone()))
    }
}

impl Drop for SessionSlot {
    fn drop(&mut self) {
        self.0.session_slots.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn handle(s: TcpStream, ctx: &Arc<ServerCtx>, slot: &Arc<ConnSlot>) -> Result<(), Ava1Error> {
    s.set_nodelay(true)?;
    let (rh, wh) = s.into_split();
    let (mut r, mut w) = (FrameReader::new(rh), FrameWriter::new(wh));
    // One deadline for the whole handshake, from accept to Welcome (or JoinAck): a peer
    // that trickles bytes cannot stretch it, and nothing before it can wait longer.
    let deadline = tokio::time::Instant::now() + ctx.timing.handshake;
    let first = tokio::time::timeout_at(deadline, r.recv())
        .await
        .map_err(|_| Ava1Error::Timeout)??;
    match first.ty {
        Hs1::TYPE => control(r, w, first, ctx, slot, deadline).await,
        Join::TYPE => lane(r, w, first, ctx, slot, deadline).await,
        t => {
            refuse_by(deadline, &mut w, gen::ERR_PROTOCOL, "expected Hs1 or Join").await;
            Err(Ava1Error::Unexpected(t))
        }
    }
}

async fn control(
    mut r: FrameReader<OwnedReadHalf>,
    mut w: FrameWriter<OwnedWriteHalf>,
    first: Frame,
    ctx: &Arc<ServerCtx>,
    slot: &Arc<ConnSlot>,
    deadline: tokio::time::Instant,
) -> Result<(), Ava1Error> {
    // Reserve atomically before the handshake; released on every exit path by the guard.
    let Some(_slot) = SessionSlot::take(ctx) else {
        refuse_by(deadline, &mut w, gen::ERR_BUSY, "too many sessions").await;
        return Err(Ava1Error::Refused {
            code: gen::ERR_BUSY,
            message: "too many sessions".into(),
        });
    };
    let slot_ip = slot.ip;
    // Held while the session is welcomed but unconfirmed; dropped when it pairs or ends.
    let mut unpaired_slot: Option<UnpairedSlot> = None;
    let est = tokio::time::timeout_at(
        deadline,
        handshake::server_launched(
            &mut r,
            &mut w,
            first,
            &ctx.identity,
            &ctx.name,
            ctx.launch.as_ref(),
            |k| {
                // Message 3 has just proved the client holds `k`. Any session that key still
                // has is replaced, before the limits below (and before Welcome, so the
                // client's lanes find the old connections' counts already given back).
                if ctx.is_known(k) {
                    ctx.supersede(k);
                    Admission::Known
                } else if !ctx.pairing_open() {
                    Admission::Refuse(
                        gen::ERR_PAIRING_CLOSED,
                        "this device is not paired and pairing is closed",
                    )
                } else {
                    ctx.supersede(k);
                    match UnpairedSlot::take(ctx) {
                        Some(slot) => {
                            if ctx.pair_budget.lock().unwrap().welcome_allowed(
                                slot_ip,
                                Instant::now(),
                                &ctx.limits,
                            ) {
                                unpaired_slot = Some(slot);
                                Admission::Pairing
                            } else {
                                Admission::Refuse(
                                    gen::ERR_BUSY,
                                    "too many pairing attempts from this address",
                                )
                            }
                        }
                        None => Admission::Refuse(gen::ERR_BUSY, "too many devices are pairing"),
                    }
                }
            },
            (if ctx.jobs.is_some() {
                gen::CAP_DATA_PLANE
            } else {
                0
            }) | (if ctx.mgmt { gen::CAP_MGMT } else { 0 }),
        ),
    )
    .await
    .map_err(|_| Ava1Error::Timeout)??;
    let welcomed = Instant::now();
    let req = PairRequest {
        ip: slot_ip,
        peer_key: est.peer_key,
        peer_name: est.peer_name.clone(),
        // A random code for this session, from the CSPRNG; shown on the console's screen
        // only, never derived from the transcript and never sent (SPEC.md §5.5).
        code: if est.pairing.is_some() {
            cpace::random_code()?
        } else {
            0
        },
    };
    let entry = Arc::new(SessionEntry {
        keys: est.keys.clone(),
        peer_key: est.peer_key,
        paired: AtomicBool::new(est.pairing.is_none()),
        code: req.code,
        pake: Mutex::new(None),
        pake_started: AtomicBool::new(false),
        router: Arc::new(Router::default()),
        lane_gen: watch::Sender::new([0; 9]),
        nonces: Mutex::new(VecDeque::new()),
        ended: watch::Sender::new(false),
        conns: Mutex::default(),
        unpaired: Mutex::new(unpaired_slot),
    });
    entry.adopt(slot);
    let mut ended = entry.ended.subscribe();
    ctx.sessions
        .lock()
        .unwrap()
        .insert(est.session_id, entry.clone());
    if est.pairing.is_some() {
        ctx.notify_session(&req);
    }
    let (tx, mut rx) = mpsc::channel(DELIVER_DEPTH);
    let (link, outbox) = drive(r, w, ctx.timing, tx);
    let rpc_slots = Arc::new(tokio::sync::Semaphore::new(RPC_WORKERS));
    // Replies from this loop are only ever queued, never awaited: a peer that sends
    // requests but stops reading fills the queue and is disconnected (`reply`), and the
    // writer task ends the link once it has taken nothing for `dead_after`.
    let reply = |channel: u32, status: u16| -> bool {
        let r = RpcResponse {
            status,
            body: Vec::new(),
        };
        outbox.try_send(channel, &r).is_ok()
    };
    let mut check = tokio::time::interval(ctx.timing.ping_every);
    loop {
        let f = tokio::select! {
            f = rx.recv() => match f {
                Some(f) => f,
                None => break,
            },
            // The same device connected again: this session is the old one.
            _ = over(&mut ended) => break,
            _ = check.tick() => {
                // An unconfirmed session is only useful while it can still be confirmed:
                // it ends with the pairing window, or after the confirm deadline.
                if !entry.paired.load(Ordering::SeqCst)
                    && (!ctx.pairing_open() || welcomed.elapsed() > ctx.limits.pair_confirm)
                {
                    refuse_on(&outbox, gen::ERR_PAIRING_CLOSED, "pairing was not confirmed in time").await;
                    break;
                }
                continue;
            }
        };
        match f.ty {
            RpcRequest::TYPE => {
                let Ok(q) = f.decode::<RpcRequest>() else {
                    refuse_on(&outbox, gen::ERR_PROTOCOL, "bad RpcRequest").await;
                    break;
                };
                let channel = f.channel;
                if !entry.paired.load(Ordering::SeqCst) {
                    if !reply(channel, gen::ERR_NOT_PAIRED) {
                        break;
                    }
                    continue;
                }
                if q.body.len() > RPC_REQUEST_MAX {
                    let r = RpcResponse {
                        status: gen::ERR_PROTOCOL,
                        body: b"request exceeds the 56 KiB RPC cap".to_vec(),
                    };
                    if outbox.try_send(channel, &r).is_err() {
                        break;
                    }
                    continue;
                }
                if q.method == gen::METHOD_PAIRING_OPEN {
                    let status = match gen::PairingOpen::decode(&q.body) {
                        Ok(o) => {
                            ctx.open_pairing(Duration::from_secs(u64::from(
                                o.seconds.min(MAX_PAIRING_WINDOW_S),
                            )));
                            gen::STATUS_OK
                        }
                        Err(_) => gen::ERR_PROTOCOL,
                    };
                    if !reply(channel, status) {
                        break;
                    }
                    continue;
                }
                // Calls run on workers; the reader (and so liveness) never waits for one.
                let Ok(permit) = rpc_slots.clone().try_acquire_owned() else {
                    if !reply(channel, gen::ERR_BUSY) {
                        break;
                    }
                    continue;
                };
                let (ctx, outbox) = (ctx.clone(), outbox.clone());
                tokio::spawn(async move {
                    let reply = tokio::task::spawn_blocking(move || (ctx.rpc)(q.method, &q.body))
                        .await
                        .unwrap_or(RpcReply {
                            status: gen::ERR_INTERNAL,
                            body: Vec::new(),
                        });
                    let reply = if reply.body.len() > RPC_REPLY_MAX {
                        RpcReply {
                            status: gen::ERR_INTERNAL,
                            body: b"reply exceeds the 256 KiB RPC cap".to_vec(),
                        }
                    } else {
                        reply
                    };
                    // Waits for room (bounded: a stuck peer ends the link, which fails this).
                    let _ = outbox
                        .send(
                            channel,
                            &RpcResponse {
                                status: reply.status,
                                body: reply.body,
                            },
                        )
                        .await;
                    drop(permit);
                });
            }
            PairPakeClient::TYPE => {
                // The first half of the pairing (SPEC.md §5.5): our public value, computed
                // from the code only this console's screen shows. Once per session. Nothing
                // here reveals anything about the code, so nothing here counts as a guess.
                let h = &entry.keys.hash;
                if entry.paired.load(Ordering::SeqCst)
                    || entry.pake_started.swap(true, Ordering::SeqCst)
                {
                    break;
                }
                if !ctx.pairing_open() {
                    refuse_on(&outbox, gen::ERR_PAIRING_CLOSED, "pairing is closed").await;
                    break;
                }
                if !ctx
                    .pair_budget
                    .lock()
                    .unwrap()
                    .guess_allowed(req.ip, &ctx.limits)
                {
                    (ctx.log)(&format!(
                        "ava1: pairing refused: {} has used its wrong-code budget",
                        req.ip
                    ));
                    refuse_on(
                        &outbox,
                        gen::ERR_PAIRING_CLOSED,
                        "too many wrong codes from this address",
                    )
                    .await;
                    break;
                }
                let Ok(m) = f.decode::<PairPakeClient>() else {
                    outbox.flush(FAREWELL).await;
                    break;
                };
                let (Ok(x), g) = (
                    keys::random_bytes::<32>().map(zeroize::Zeroizing::new),
                    cpace::generator(h, req.code),
                ) else {
                    break;
                };
                let k = cpace::public(&x, &g).and_then(|yb| {
                    cpace::key(h, &x, &m.y, &m.y, &yb).map(|k| (yb, zeroize::Zeroizing::new(k)))
                });
                drop(x);
                let Some((yb, k)) = k else {
                    outbox.flush(FAREWELL).await;
                    break;
                };
                *entry.pake.lock().unwrap() = Some(k);
                if outbox
                    .try_send(f.channel, &PairPakeServer { y: yb })
                    .is_err()
                {
                    break;
                }
            }
            PairConfirm::TYPE => {
                let already = entry.paired.load(Ordering::SeqCst);
                let h = &entry.keys.hash;
                // One attempt per session: whatever the outcome, a refusal ends it below. The
                // key is taken out of the entry (and wiped when dropped, on every path).
                let k = if already {
                    None
                } else {
                    entry.pake.lock().unwrap().take()
                };
                // Some(ok): the PAKE ran, so this is a real guess. None: nothing was guessed.
                let proof = match (&k, f.decode::<PairConfirm>()) {
                    (Some(k), Ok(c)) => Some(cpace::ct_eq32(&c.mac, &cpace::mac(k, b"client", h))),
                    _ => None,
                };
                let accepted = already || ctx.accept_pairing(&req, proof);
                entry.paired.store(accepted, Ordering::SeqCst);
                if accepted && !already {
                    entry.unpaired.lock().unwrap().take();
                }
                // The console proves it knew the code too: the client stores nothing without it.
                let mac = match (accepted && !already, &k) {
                    (true, Some(k)) => cpace::mac(k, b"server", h),
                    _ => [0; 32],
                };
                drop(k);
                let result = PairResult {
                    accepted: u8::from(accepted),
                    mac,
                };
                if outbox.try_send(f.channel, &result).is_err() || !accepted {
                    outbox.flush(FAREWELL).await;
                    break;
                }
            }
            t if is_data_type(t) => {
                // A JobOpen for a job id that is still registered (the sender cancelled it a
                // moment ago and is resuming it on this session) must not be routed to the old
                // job: its receiver is draining what the old lanes buffered, ends on the
                // JobCancel queued before this frame, and drops whatever is queued behind it,
                // so the open would never be answered. BUSY now; the sender retries.
                if f.ty == gen::JobOpen::TYPE {
                    if let Some(job) = job_of(&f).filter(|j| entry.router.has_job(j)) {
                        let busy = gen::JobOpenAck {
                            job_id: job,
                            status: gen::ERR_BUSY,
                            credit: 0,
                            staged: 0,
                            workers: 0,
                            message: Some("the job's previous run is still closing".into()),
                        };
                        if outbox.try_send(f.channel, &busy).is_err() {
                            break;
                        }
                        continue;
                    }
                }
                // A known job's frames were delivered by the router; what comes back is
                // for no job at all.
                if let Some(f) = entry.router.route_control(f).await {
                    let opens = f.ty == gen::JobOpen::TYPE || f.ty == gen::Resume::TYPE;
                    // Admission: every open job holds a task and an inbox, so a paired peer
                    // cannot open them without bound.
                    if opens && entry.router.job_count() >= MAX_JOBS_PER_SESSION {
                        if let Some(job) = job_of(&f) {
                            let refused = if f.ty == gen::JobOpen::TYPE {
                                outbox.try_send(
                                    f.channel,
                                    &gen::JobOpenAck {
                                        job_id: job,
                                        status: gen::ERR_BUSY,
                                        credit: 0,
                                        staged: 0,
                                        workers: 0,
                                        message: Some("too many jobs are open".into()),
                                    },
                                )
                            } else {
                                outbox.try_send(
                                    f.channel,
                                    &gen::JobMap {
                                        job_id: job,
                                        status: gen::ERR_BUSY,
                                        last: 1,
                                        done: vec![],
                                        partial: vec![],
                                        message: Some("too many jobs are open".into()),
                                    },
                                )
                            };
                            if refused.is_err() {
                                break;
                            }
                        }
                        continue;
                    }
                    if let (Some(host), true, Some(job), true) = (
                        &ctx.jobs,
                        opens,
                        job_of(&f),
                        entry.paired.load(Ordering::SeqCst),
                    ) {
                        let link = JobLink::new(
                            job,
                            entry.router.clone(),
                            ConnTx::new(outbox.clone()),
                            None,
                        );
                        host.accept(link, f, est.peer_key);
                    }
                    // Anything else for an unknown job is a late frame: dropped.
                }
            }
            _ if f.ignorable() => {}
            _ => {
                refuse_on(&outbox, gen::ERR_PROTOCOL, "unexpected frame").await;
                break;
            }
        }
    }
    ctx.sessions.lock().unwrap().remove(&est.session_id);
    // Jobs hear about the session's end; lanes end with the session, at once.
    entry.router.close("the session ended");
    entry.end();
    drop(link);
    Ok(())
}

/// The longest a connection's last words (an Error, a PairResult) may take to leave.
const FAREWELL: Duration = Duration::from_secs(1);

/// Writes `Error{code, message}` on a connection not yet handed to a link, giving up at
/// the handshake deadline.
async fn refuse_by(
    deadline: tokio::time::Instant,
    w: &mut FrameWriter<OwnedWriteHalf>,
    code: u16,
    message: &str,
) {
    let _ = tokio::time::timeout_at(deadline, refuse(w, code, message)).await;
}

/// Queues `Error{code, message}` and gives it up to `FAREWELL` to go out.
async fn refuse_on(outbox: &Outbox, code: u16, message: &str) {
    let e = gen::Error {
        code,
        message: message.into(),
    };
    if outbox.try_send(0, &e) != Err(Full::Closed) {
        outbox.flush(FAREWELL).await;
    }
}

/// The nonces a session remembers, so a captured Join cannot be replayed.
const JOIN_NONCES: usize = 64;

async fn lane(
    mut r: FrameReader<OwnedReadHalf>,
    mut w: FrameWriter<OwnedWriteHalf>,
    first: Frame,
    ctx: &Arc<ServerCtx>,
    slot: &Arc<ConnSlot>,
    deadline: tokio::time::Instant,
) -> Result<(), Ava1Error> {
    let j: Join = first.decode()?;
    let entry = ctx.sessions.lock().unwrap().get(&j.session_id).cloned();
    let refused = |code: u16| Ava1Error::Refused {
        code,
        message: "join refused".into(),
    };
    let Some(entry) = entry else {
        refuse_by(deadline, &mut w, gen::ERR_BAD_JOIN, "unknown session").await;
        return Err(refused(gen::ERR_BAD_JOIN));
    };
    let want = keys::join_tag(&entry.keys.c2s, &j.session_id, j.lane_id, &j.client_nonce);
    let lane_ok = (1..=gen::MAX_LANES as u16).contains(&j.lane_id);
    if !lane_ok || !keys::ct_eq16(&want, &j.tag) {
        refuse_by(deadline, &mut w, gen::ERR_BAD_JOIN, "join refused").await;
        return Err(refused(gen::ERR_BAD_JOIN));
    }
    let fresh = {
        let mut n = entry.nonces.lock().unwrap();
        let fresh = !n.contains(&j.client_nonce);
        if fresh {
            if n.len() >= JOIN_NONCES {
                n.pop_front();
            }
            n.push_back(j.client_nonce);
        }
        fresh
    };
    if !fresh {
        refuse_by(deadline, &mut w, gen::ERR_BAD_JOIN, "join replayed").await;
        return Err(refused(gen::ERR_BAD_JOIN));
    }
    if !entry.paired.load(Ordering::SeqCst) {
        refuse_by(deadline, &mut w, gen::ERR_NOT_PAIRED, "pair first").await;
        return Err(Ava1Error::NotPaired);
    }
    entry.adopt(slot);
    let lane = j.lane_id as usize;
    // A fresh server nonce per join: even a replayed Join (one older than the nonce
    // window) gets keys never used before, so no (key, counter) pair repeats.
    let server_nonce: [u8; 16] = keys::random_bytes()?;
    let (cn, sn) = (j.client_nonce, server_nonce);
    let tag = keys::join_ack_tag(&entry.keys.s2c, &j.session_id, j.lane_id, &cn, &sn);
    let ack = JoinAck {
        lane_id: j.lane_id,
        server_nonce,
        tag,
    };
    tokio::time::timeout_at(deadline, w.send_msg(0, &ack))
        .await
        .map_err(|_| Ava1Error::Timeout)??;
    r.set_key(keys::lane_key(&entry.keys.c2s, j.lane_id, &cn, &sn));
    w.set_key(keys::lane_key(&entry.keys.s2c, j.lane_id, &cn, &sn));
    // A Join can be captured and sent again by someone who does not hold the session
    // keys. So this connection takes the lane over — ending an older connection of the
    // same lane id — only once its first sealed frame has opened under the new lane
    // key. Until then the older connection is left alone, and its bodies stay capped at
    // the control size: a forged Join must not buy a 16 MiB buffer per attempt.
    let proof = tokio::time::timeout_at(deadline, r.recv())
        .await
        .map_err(|_| Ava1Error::Timeout)??;
    // Only now, with the lane key proven, may frames be as large as the frame cap.
    r.set_max_body(crate::frame::MAX_BODY);
    let mut gen_no = 0;
    entry.lane_gen.send_modify(|g| {
        g[lane] += 1;
        gen_no = g[lane];
    });
    let (mut gens, mut ended) = (entry.lane_gen.subscribe(), entry.ended.subscribe());
    let (tx, mut rx) = mpsc::channel(DELIVER_DEPTH);
    let (link, outbox) = drive(r, w, ctx.timing, tx);
    let lane_gen = entry.router.lane_up(j.lane_id, outbox.clone());
    // The proving frame is a frame like any other (a client sends a Ping).
    let mut first = Some(proof);
    loop {
        let f = match first.take() {
            Some(f) => f,
            None => tokio::select! {
                f = rx.recv() => match f {
                    Some(f) => f,
                    None => break,
                },
                _ = taken_over(&mut gens, lane, gen_no) => break,
                _ = over(&mut ended) => break,
            },
        };
        match f.ty {
            Ping::TYPE => {
                let Ok(p) = f.decode::<Ping>() else {
                    refuse_on(&outbox, gen::ERR_PROTOCOL, "malformed Ping").await;
                    break;
                };
                let pong = Pong {
                    seq: p.seq,
                    t_us: p.t_us,
                };
                if outbox.try_send(0, &pong) == Err(Full::Closed) {
                    break;
                }
            }
            Pong::TYPE => {}
            gen::Bye::TYPE | gen::Error::TYPE => break,
            t if is_data_type(t) => entry.router.route_lane(j.lane_id, f).await,
            _ if f.ignorable() => {}
            _ => {
                refuse_on(&outbox, gen::ERR_PROTOCOL, "unexpected frame on a lane").await;
                break;
            }
        }
    }
    entry.router.lane_down(j.lane_id, lane_gen);
    drop(link);
    Ok(())
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, n])
    }

    #[test]
    fn one_addresses_guesses_do_not_lock_out_another() {
        let l = Limits::default();
        let mut b = PairBudget::default();
        for _ in 0..l.pair_fails_per_ip {
            assert!(b.guess_allowed(ip(1), &l));
            let (_, close) = b.guess_failed(ip(1), &l);
            assert!(!close);
        }
        assert!(!b.guess_allowed(ip(1), &l), "its own budget is spent");
        assert!(b.guess_allowed(ip(2), &l), "another address is untouched");
        b.reset();
        assert!(b.guess_allowed(ip(1), &l), "a new window starts over");
    }

    #[test]
    fn the_global_cap_holds_across_addresses() {
        let l = Limits::default();
        let mut b = PairBudget::default();
        let mut closed_at = None;
        'all: for n in 1..=250u8 {
            for _ in 0..l.pair_fails_per_ip {
                assert!(closed_at.is_none());
                if !b.guess_allowed(ip(n), &l) {
                    break;
                }
                let (_, close) = b.guess_failed(ip(n), &l);
                if close {
                    closed_at = Some(b.total);
                    break 'all;
                }
            }
        }
        assert_eq!(
            closed_at,
            Some(l.pair_fails_total),
            "20 guesses in all, from 4 addresses"
        );
        assert!(!b.guess_allowed(ip(99), &l), "nobody guesses after the cap");
    }

    #[test]
    fn new_pairing_sessions_are_rated_per_address() {
        let l = Limits::default();
        let mut b = PairBudget::default();
        let t0 = Instant::now();
        for _ in 0..l.welcomes_per_ip {
            assert!(b.welcome_allowed(ip(1), t0, &l));
        }
        assert!(!b.welcome_allowed(ip(1), t0, &l));
        assert!(
            b.welcome_allowed(ip(2), t0, &l),
            "another address has its own share"
        );
        assert!(
            b.welcome_allowed(ip(1), t0 + WELCOME_WINDOW, &l),
            "and it refills"
        );
    }
}
