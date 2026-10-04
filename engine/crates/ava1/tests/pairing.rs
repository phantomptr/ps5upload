//! Pairing policy (SPEC.md §5, §8): who may hold an unconfirmed session and for how
//! long, how many connections one address gets, and what a server does about a peers
//! file it cannot read.
mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ava1::gen;
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::server::{Limits, ServerCtx};
use ava1::session::{connect, Session};
use ava1::Ava1Error;
use common::*;
use tokio::net::TcpStream;

/// An unpaired server with its window open for a minute; counts pairing notifications.
async fn open_server(limits: Limits) -> (std::net::SocketAddr, Arc<ServerCtx>, Arc<AtomicUsize>) {
    let shown = Arc::new(AtomicUsize::new(0));
    let n = shown.clone();
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "console",
        PeerStore::in_memory(),
        node_info_rpc("console"),
    )
    .with_timing(fast())
    .with_limits(limits)
    .with_notify(Box::new(move |_| {
        n.fetch_add(1, Ordering::SeqCst);
    }));
    assert!(ctx.open_pairing_if_unpaired(Duration::from_secs(60)));
    let (addr, ctx) = start(ctx).await;
    (addr, ctx, shown)
}

async fn stranger(addr: std::net::SocketAddr) -> Result<Session, Ava1Error> {
    connect(
        &addr.to_string(),
        Arc::new(Identity::generate().unwrap()),
        Arc::new(Mutex::new(PeerStore::in_memory())),
        "phone",
        fast(),
    )
    .await
}

/// A device that knocks and then reads the code off the console's screen (the one place it
/// exists), as a person would.
async fn knocker(addr: std::net::SocketAddr, ctx: &ServerCtx) -> (Session, u32) {
    let id = Arc::new(Identity::generate().unwrap());
    let s = connect(
        &addr.to_string(),
        id.clone(),
        Arc::new(Mutex::new(PeerStore::in_memory())),
        "phone",
        fast(),
    )
    .await
    .unwrap();
    let code = console_code(ctx, &id.public()).await;
    (s, code)
}

fn is_busy<T: std::fmt::Debug>(r: &Result<T, Ava1Error>) -> bool {
    matches!(r, Err(Ava1Error::Refused { code, .. }) if *code == gen::ERR_BUSY)
}

fn is_pairing_closed<T: std::fmt::Debug>(r: &Result<T, Ava1Error>) -> bool {
    matches!(r, Err(Ava1Error::Refused { code, .. }) if *code == gen::ERR_PAIRING_CLOSED)
}

#[tokio::test]
async fn an_unconfirmed_session_ends_when_the_window_closes() {
    let (addr, ctx, _) = open_server(Limits::default()).await;
    let s = stranger(addr).await.unwrap();
    assert!(s.pairing_pending());
    assert_eq!(ctx.sessions(), 1);
    ctx.close_pairing();
    let why = tokio::time::timeout(Duration::from_secs(5), s.closed())
        .await
        .expect("the unconfirmed session was closed");
    assert!(why.contains("not confirmed"), "{why}");
    wait_for(Duration::from_secs(5), || ctx.sessions() == 0)
        .await
        .expect("its slot is free");
}

#[tokio::test]
async fn an_unconfirmed_session_ends_at_the_confirm_deadline() {
    let limits = Limits {
        pair_confirm: Duration::from_millis(400),
        ..Limits::default()
    };
    let (addr, ctx, _) = open_server(limits).await;
    let s = stranger(addr).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), s.closed())
        .await
        .expect("closed at the deadline");
    assert!(ctx.pairing_open(), "the window itself is still open");
    // A paired session is not subject to the deadline.
    let (mut p, code) = knocker(addr, &ctx).await;
    p.confirm_pairing(code).await.unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(!p.is_closed());
    p.node_info().await.unwrap();
}

#[tokio::test]
async fn at_most_two_devices_wait_to_be_confirmed() {
    let (addr, ctx, _) = open_server(Limits::default()).await;
    let a = stranger(addr).await.unwrap();
    let _b = stranger(addr).await.unwrap();
    let c = stranger(addr).await;
    assert!(is_busy(&c), "{c:?}");
    // A place frees when one of them leaves.
    a.close().await;
    wait_for(Duration::from_secs(5), || ctx.sessions() == 1)
        .await
        .unwrap();
    let t = std::time::Instant::now();
    loop {
        match stranger(addr).await {
            Ok(_) => break,
            r => assert!(is_busy(&r) && t.elapsed() < Duration::from_secs(5), "{r:?}"),
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn one_address_holds_at_most_twelve_connections() {
    let (addr, ctx, me, peers) = paired().await;
    let mut held = Vec::new();
    for _ in 0..ava1::server::MAX_CONNS_PER_IP {
        held.push(TcpStream::connect(addr).await.unwrap());
    }
    wait_for(Duration::from_secs(5), || {
        ctx.connections() == ava1::server::MAX_CONNS_PER_IP
    })
    .await
    .expect("twelve accepted");
    let r = connect(&addr.to_string(), me.clone(), peers.clone(), "c", fast()).await;
    assert!(is_busy(&r), "{r:?}");
    held.pop();
    wait_for(Duration::from_secs(5), || {
        ctx.connections() < ava1::server::MAX_CONNS_PER_IP
    })
    .await
    .unwrap();
    connect(&addr.to_string(), me, peers, "c", fast())
        .await
        .unwrap();
}

/// A server whose notifications are recorded, as (name, code): the console's screen.
async fn screen_server(
    limits: Limits,
) -> (
    std::net::SocketAddr,
    Arc<ServerCtx>,
    Arc<Mutex<Vec<(String, u32)>>>,
) {
    let screen = Arc::new(Mutex::new(Vec::new()));
    let s2 = screen.clone();
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "console",
        PeerStore::in_memory(),
        node_info_rpc("console"),
    )
    .with_timing(fast())
    .with_limits(limits)
    .with_notify(Box::new(move |r| {
        s2.lock().unwrap().push((r.peer_name.clone(), r.code));
    }));
    assert!(ctx.open_pairing_if_unpaired(Duration::from_secs(60)));
    let (addr, ctx) = start(ctx).await;
    (addr, ctx, screen)
}

#[tokio::test]
async fn every_session_shows_its_own_code_even_right_after_a_strangers() {
    // A stranger knocked a moment ago from the same address: the user's own attempt must
    // still show its code (a rate limit that hid it would leave them typing a stale one).
    let (addr, ctx, screen) = screen_server(Limits::default()).await;
    let (_stranger, stranger_code) = knocker(addr, &ctx).await;
    let (mut mine, my_code) = knocker(addr, &ctx).await;
    wait_for(Duration::from_secs(5), || screen.lock().unwrap().len() == 2)
        .await
        .expect("both sessions were shown");
    let shown: Vec<u32> = screen.lock().unwrap().iter().map(|(_, c)| *c).collect();
    assert!(
        shown.contains(&stranger_code) && shown.contains(&my_code),
        "{shown:?}"
    );
    mine.confirm_pairing(my_code).await.unwrap();
}

#[tokio::test]
async fn an_identical_request_is_shown_once_and_a_later_one_again() {
    let (addr, _ctx, screen) = screen_server(Limits::default()).await;
    // The same device (same key, same address) knocking in a loop: shown once.
    let me = Arc::new(Identity::generate().unwrap());
    for _ in 0..3 {
        let _ = connect(
            &addr.to_string(),
            me.clone(),
            Arc::new(Mutex::new(PeerStore::in_memory())),
            "phone",
            fast(),
        )
        .await
        .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        screen.lock().unwrap().len(),
        1,
        "an identical repeat is not shown again"
    );

    let limits = Limits {
        notify_every: Duration::from_millis(100),
        ..Limits::default()
    };
    let (addr, _ctx, screen) = screen_server(limits).await;
    let _ = connect(
        &addr.to_string(),
        me.clone(),
        Arc::new(Mutex::new(PeerStore::in_memory())),
        "phone",
        fast(),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    let _ = connect(
        &addr.to_string(),
        me,
        Arc::new(Mutex::new(PeerStore::in_memory())),
        "phone",
        fast(),
    )
    .await
    .unwrap();
    wait_for(Duration::from_secs(5), || screen.lock().unwrap().len() == 2)
        .await
        .expect("a later request is shown again");
}

#[tokio::test]
async fn junk_sessions_that_never_run_the_pake_never_close_the_window() {
    // Three throwaway sessions per round send a confirm with nothing behind it. They guess
    // nothing, so they cost no budget however many there are.
    let limits = Limits {
        welcomes_per_ip: 1000,
        ..Limits::default()
    };
    let (addr, ctx, _) = open_server(limits).await;
    for _ in 0..30 {
        let mut g = rogue(addr).await;
        g.w.send_msg(1, &PairConfirm { mac: [7; 32] })
            .await
            .unwrap();
        assert!(g.next(PairResult::TYPE).await.is_none_or(|f| f
            .decode::<PairResult>()
            .unwrap()
            .accepted
            == 0));
        drop(g);
        wait_for(Duration::from_secs(5), || ctx.sessions() == 0)
            .await
            .unwrap();
    }
    // Malformed and degenerate PAKE openings are free too.
    for body in [
        vec![0u8; 3],
        PairPakeClient { y: [0; 32] }.to_bytes().unwrap(),
    ] {
        let mut g = rogue(addr).await;
        g.w.send(PairPakeClient::TYPE, 1, &body).await.unwrap();
        drop(g);
        wait_for(Duration::from_secs(5), || ctx.sessions() == 0)
            .await
            .unwrap();
    }
    assert_eq!(ctx.pair_failures(), 0);
    assert!(ctx.pairing_open());
    let mut g = rogue(addr).await;
    let code = console_code(&ctx, &g.key).await;
    assert!(g.pake(code).await.1, "the real user still pairs");
}

#[tokio::test]
async fn one_address_cannot_start_more_than_its_share_of_pairing_sessions() {
    let (addr, ctx, _) = open_server(Limits::default()).await;
    let mut ok = 0;
    for _ in 0..8 {
        match stranger(addr).await {
            Ok(s) => {
                ok += 1;
                s.close().await;
                wait_for(Duration::from_secs(5), || ctx.sessions() == 0)
                    .await
                    .unwrap();
            }
            Err(e) => assert!(
                matches!(&e, Ava1Error::Refused { code, .. } if *code == gen::ERR_BUSY),
                "{e:?}"
            ),
        }
    }
    assert_eq!(
        ok,
        ava1::server::WELCOMES_PER_IP,
        "the rest were refused as busy"
    );
    assert!(ctx.pairing_open() && ctx.pair_failures() == 0);
}

#[tokio::test]
async fn a_successful_pairing_closes_the_window() {
    // The automatic window.
    let (addr, ctx, _) = open_server(Limits::default()).await;
    let (mut first, code) = knocker(addr, &ctx).await;
    let waiting = stranger(addr).await.unwrap();
    first.confirm_pairing(code).await.unwrap();
    assert!(!ctx.pairing_open(), "one window, one pairing");
    let r = stranger(addr).await;
    assert!(is_pairing_closed(&r), "{r:?}");
    // The other device that was waiting is let go too.
    tokio::time::timeout(Duration::from_secs(5), waiting.closed())
        .await
        .expect("the other unconfirmed session ends with the window");
    // A window opened by pairing.open closes the same way.
    first.open_pairing(60).await.unwrap();
    assert!(ctx.pairing_open());
    let (mut second, code) = knocker(addr, &ctx).await;
    second.confirm_pairing(code).await.unwrap();
    assert!(!ctx.pairing_open());
    assert!(!first.is_closed());
    first.node_info().await.unwrap();
}

#[tokio::test]
async fn an_unreadable_peers_file_keeps_pairing_closed_and_is_never_overwritten() {
    // A directory where the file should be: it exists, and reading it fails.
    let d = temp_dir("unreadable");
    let path = d.join("peers");
    std::fs::create_dir(&path).unwrap();
    assert!(PeerStore::load(&path).is_err());
    let mut store = PeerStore::load_or_unreadable(&path);
    assert!(store.unreadable().is_some());
    assert!(store.add([1; 32], "x").is_err(), "never written");
    assert!(!store.contains(&[1; 32]));
    assert!(path.is_dir(), "left alone");
    assert!(!d.join("peers.tmp").exists());

    let logged = Arc::new(Mutex::new(Vec::<String>::new()));
    let l2 = logged.clone();
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "console",
        store,
        node_info_rpc("console"),
    )
    .with_timing(fast())
    .with_log(Box::new(move |m| l2.lock().unwrap().push(m.to_string())));
    assert!(
        !ctx.open_pairing_if_unpaired(Duration::from_secs(60)),
        "unknown peers is not no peers"
    );
    assert!(logged.lock().unwrap()[0].contains("could not be read"));
    let (addr, ctx) = start(ctx).await;
    let r = stranger(addr).await;
    assert!(is_pairing_closed(&r), "{r:?}");
    // Even with the window forced open, a pairing that cannot be stored is refused.
    ctx.open_pairing(Duration::from_secs(60));
    let (mut s, code) = knocker(addr, &ctx).await;
    let r = s.confirm_pairing(code).await;
    assert!(matches!(r, Err(Ava1Error::Refused { .. })), "{r:?}");
    assert!(path.is_dir());
    assert!(logged
        .lock()
        .unwrap()
        .iter()
        .any(|m| m.contains("not stored")));
}

#[test]
fn a_missing_peers_file_is_an_empty_store_not_an_unreadable_one() {
    let d = temp_dir("missing");
    let mut store = PeerStore::load_or_unreadable(&d.join("peers"));
    assert!(store.unreadable().is_none());
    store.add([2; 32], "x").unwrap();
    assert!(PeerStore::load(&d.join("peers"))
        .unwrap()
        .contains(&[2; 32]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_devices_confirming_at_the_same_moment_get_one_pairing() {
    // The owner's approval takes a while (a prompt, a slow disk): long enough for a
    // second PairConfirm to arrive while the first is still being decided.
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "console",
        PeerStore::in_memory(),
        node_info_rpc("console"),
    )
    .with_timing(fast())
    .with_approve(Box::new(|_| {
        std::thread::sleep(Duration::from_millis(150));
        true
    }));
    assert!(ctx.open_pairing_if_unpaired(Duration::from_secs(60)));
    let (addr, ctx) = start(ctx).await;
    let ((mut a, ca), (mut b, cb)) = (knocker(addr, &ctx).await, knocker(addr, &ctx).await);
    let (ra, rb) = tokio::join!(a.confirm_pairing(ca), b.confirm_pairing(cb));
    assert_eq!(
        u8::from(ra.is_ok()) + u8::from(rb.is_ok()),
        1,
        "one window, one pairing: {ra:?} {rb:?}"
    );
    assert!(!ctx.pairing_open());
}

// ---- the pairing PAKE (SPEC.md §4.6, §5.5): the code exists only on the console's screen ----

use ava1::conn::{FrameReader, FrameWriter};
use ava1::cpace;
use ava1::gen::{PairConfirm, PairPakeClient, PairPakeServer, PairResult};
use ava1::wire::{FrameMessage, Message};
use common::{RawReader, RawWriter};

struct Rogue {
    r: RawReader,
    w: RawWriter,
    h: [u8; 64],
    key: [u8; 32],
    /// Everything the console sent that is not a ping: all this host ever gets to see.
    seen: Vec<Vec<u8>>,
}

/// A LAN host with a throwaway key: completes Noise, is welcomed with knows_you = 0, and
/// holds the connection. It sees the handshake hash and every frame, and nothing else.
async fn rogue(addr: std::net::SocketAddr) -> Rogue {
    let me = Identity::generate().unwrap();
    let sock = TcpStream::connect(addr).await.unwrap();
    let (rh, wh) = sock.into_split();
    let (mut r, mut w) = (FrameReader::new(rh), FrameWriter::new(wh));
    let est = ava1::handshake::client(&mut r, &mut w, &me, "rogue", |_| false, 0)
        .await
        .unwrap();
    assert!(est.pairing.is_some_and(|p| p.server_must_confirm));
    Rogue {
        r,
        w,
        h: est.keys.hash,
        key: me.public(),
        seen: Vec::new(),
    }
}

impl Rogue {
    async fn next(&mut self, ty: u8) -> Option<ava1::conn::Frame> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match self.r.recv().await {
                    Ok(f) if f.ty == ty => return Some(f),
                    Ok(_) => {}
                    Err(_) => return None,
                }
            }
        })
        .await
        .unwrap_or(None)
    }

    /// The first half only: the PAKE opening with `guess`; the key, when the console answered.
    async fn begin(&mut self, guess: u32) -> Option<[u8; 32]> {
        let g = cpace::generator(&self.h, guess);
        let x = [0x5au8; 32];
        let ya = cpace::public(&x, &g).unwrap();
        self.w.send_msg(1, &PairPakeClient { y: ya }).await.unwrap();
        let f = self.next(PairPakeServer::TYPE).await?;
        let yb: PairPakeServer = f.decode().unwrap();
        cpace::key(&self.h, &x, &yb.y, &ya, &yb.y)
    }

    /// The second half: confirm with `mac`; whether the console accepted.
    async fn finish(&mut self, mac: [u8; 32]) -> bool {
        self.w.send_msg(2, &PairConfirm { mac }).await.unwrap();
        self.next(PairResult::TYPE)
            .await
            .is_some_and(|f| f.decode::<PairResult>().unwrap().accepted != 0)
    }

    /// The whole exchange with `guess` as the code; Some(y_client, mac_client) of what it sent
    /// and whether the console accepted.
    async fn pake(&mut self, guess: u32) -> (Option<([u8; 32], [u8; 32])>, bool) {
        let g = cpace::generator(&self.h, guess);
        let x = [0x5au8; 32];
        let ya = cpace::public(&x, &g).unwrap();
        self.w.send_msg(1, &PairPakeClient { y: ya }).await.unwrap();
        let Some(f) = self.next(PairPakeServer::TYPE).await else {
            return (None, false);
        };
        self.seen.push(f.body.to_vec());
        let yb: PairPakeServer = f.decode().unwrap();
        let k = cpace::key(&self.h, &x, &yb.y, &ya, &yb.y).unwrap();
        let mac = cpace::mac(&k, b"client", &self.h);
        self.w.send_msg(2, &PairConfirm { mac }).await.unwrap();
        let Some(f) = self.next(PairResult::TYPE).await else {
            return (Some((ya, mac)), false);
        };
        self.seen.push(f.body.to_vec());
        (
            Some((ya, mac)),
            f.decode::<PairResult>().unwrap().accepted != 0,
        )
    }
}

#[tokio::test]
async fn a_rogue_that_skips_the_proof_is_refused_and_not_stored() {
    let (addr, ctx, _) = open_server(Limits::default()).await;
    // The old PairConfirm shape, with nothing to prove, and no PAKE before it.
    let mut g = rogue(addr).await;
    g.w.send(PairConfirm::TYPE, 1, &[0, 0]).await.unwrap();
    assert!(g.next(PairResult::TYPE).await.is_none_or(|f| f
        .decode::<PairResult>()
        .unwrap()
        .accepted
        == 0));
    assert!(!ctx.knows(&g.key));
    // A full-size PairConfirm without having run the PAKE.
    let mut g = rogue(addr).await;
    g.w.send_msg(1, &PairConfirm { mac: [7; 32] })
        .await
        .unwrap();
    assert!(g.next(PairResult::TYPE).await.is_none_or(|f| f
        .decode::<PairResult>()
        .unwrap()
        .accepted
        == 0));
    assert!(!ctx.knows(&g.key));
}

#[tokio::test]
async fn a_rogue_that_does_not_know_the_code_fails_and_learns_nothing() {
    let (addr, ctx, _) = open_server(Limits::default()).await;
    let mut g = rogue(addr).await;
    let code = console_code(&ctx, &g.key).await;
    // Everything a host on the wire has: h (it ran the handshake), so it can derive any
    // transcript-bound value. None of it is the code: a derived-from-transcript guess fails.
    let derived = u32::from_le_bytes(g.h[..4].try_into().unwrap()) % 1_000_000;
    let guess = if derived == code {
        (code + 1) % 1_000_000
    } else {
        derived
    };
    let (_, accepted) = g.pake(guess).await;
    assert!(!accepted);
    assert!(!ctx.knows(&g.key));
    // What it received does not contain the code, in either of its encodings.
    let ascii = format!("{code:06}").into_bytes();
    let le = code.to_le_bytes();
    for body in &g.seen {
        assert!(!body.windows(6).any(|w| w == ascii.as_slice()));
        assert!(!body.windows(4).any(|w| w == le.as_slice()) || code == 0);
    }
}

#[tokio::test]
async fn the_right_code_pairs() {
    let (addr, ctx, _) = open_server(Limits::default()).await;
    let mut g = rogue(addr).await;
    let code = console_code(&ctx, &g.key).await;
    let (_, accepted) = g.pake(code).await;
    assert!(accepted);
    assert!(ctx.knows(&g.key));
    assert!(!ctx.pairing_open());
}

#[tokio::test]
async fn the_session_api_pairs_with_the_right_code_and_not_a_wrong_one() {
    let (addr, ctx, _) = open_server(Limits::default()).await;
    let (mut s, code) = knocker(addr, &ctx).await;
    let wrong = (code + 1) % 1_000_000;
    let r = s.confirm_pairing(wrong).await;
    assert!(
        matches!(&r, Err(Ava1Error::Refused { code, .. }) if *code == gen::ERR_PAIRING_CODE),
        "{r:?}"
    );
    assert_eq!(ctx.pair_failures(), 1, "the console counted it");
    let (mut s, code) = knocker(addr, &ctx).await;
    s.confirm_pairing(code).await.unwrap();
    s.node_info().await.unwrap();
}

#[tokio::test]
async fn one_addresses_five_wrong_guesses_spend_its_budget_and_nothing_else() {
    let limits = Limits {
        welcomes_per_ip: 100,
        ..Limits::default()
    };
    let (addr, ctx, _) = open_server(limits).await;
    for i in 0..ava1::server::MAX_PAIR_FAILURES_PER_IP {
        let mut g = rogue(addr).await;
        let code = console_code(&ctx, &g.key).await;
        assert!(!g.pake((code + 1) % 1_000_000).await.1, "guess {i}");
        drop(g);
        wait_for(Duration::from_secs(5), || ctx.sessions() == 0)
            .await
            .expect("the refused session is gone");
    }
    assert_eq!(ctx.pair_failures(), ava1::server::MAX_PAIR_FAILURES_PER_IP);
    assert!(
        ctx.pairing_open(),
        "five guesses from one address do not close the window"
    );
    // That address is out of guesses: even the right code is refused from it ...
    let mut g = rogue(addr).await;
    let code = console_code(&ctx, &g.key).await;
    assert!(!g.pake(code).await.1);
    assert_eq!(
        ctx.pair_failures(),
        ava1::server::MAX_PAIR_FAILURES_PER_IP,
        "and it is free"
    );
    // ... until a paired device (or a restart) reopens the window.
    ctx.open_pairing(Duration::from_secs(60));
    let mut g = rogue(addr).await;
    let code = console_code(&ctx, &g.key).await;
    assert!(g.pake(code).await.1, "reopened: pairs");
}

#[tokio::test]
async fn the_global_cap_on_guesses_closes_the_window() {
    // Per-address budget out of the way: only the global cap acts (its end-to-end check).
    let limits = Limits {
        pair_fails_per_ip: 100,
        pair_fails_total: 5,
        welcomes_per_ip: 100,
        ..Limits::default()
    };
    let (addr, ctx, _) = open_server(limits).await;
    for i in 0..5 {
        assert!(ctx.pairing_open(), "still open before guess {i}");
        let mut g = rogue(addr).await;
        let code = console_code(&ctx, &g.key).await;
        assert!(!g.pake((code + 1) % 1_000_000).await.1);
        drop(g);
        wait_for(Duration::from_secs(5), || ctx.sessions() == 0)
            .await
            .unwrap();
    }
    assert!(!ctx.pairing_open(), "the window closed");
    let r = stranger(addr).await;
    assert!(is_pairing_closed(&r), "{r:?}");
    ctx.open_pairing(Duration::from_secs(60));
    let mut g = rogue(addr).await;
    let code = console_code(&ctx, &g.key).await;
    assert!(g.pake(code).await.1, "reopened: pairs");
}

#[tokio::test]
async fn a_session_gets_one_attempt() {
    let (addr, ctx, _) = open_server(Limits::default()).await;
    let mut g = rogue(addr).await;
    let code = console_code(&ctx, &g.key).await;
    assert!(!g.pake((code + 1) % 1_000_000).await.1);
    // The same session, now with the right code: it ended with the refusal.
    let (sent, accepted) = g.pake(code).await;
    assert!(!accepted && sent.is_some() || sent.is_none());
    assert!(!ctx.knows(&g.key));
}

#[tokio::test]
async fn a_relayed_exchange_fails_on_the_other_handshake() {
    // A man in the middle holds two handshakes (client <-> MITM <-> console), so h differs
    // on the two legs. The user types the console's code into the app; everything the app
    // sends is bound to the first leg's h. Replaying it on the console's leg cannot verify,
    // even though the code is right.
    let (addr, ctx, _) = open_server(Limits::default()).await;
    let mut leg1 = rogue(addr).await; // stands for the app's own session
    let code = console_code(&ctx, &leg1.key).await;
    let (sent, accepted) = leg1.pake(code).await;
    assert!(accepted);
    let (ya, mac) = sent.unwrap();
    ctx.open_pairing(Duration::from_secs(60));
    // The console's leg: another handshake, another h.
    let mut leg2 = rogue(addr).await;
    assert_ne!(leg1.h, leg2.h);
    leg2.w.send_msg(1, &PairPakeClient { y: ya }).await.unwrap();
    assert!(leg2.next(PairPakeServer::TYPE).await.is_some());
    leg2.w.send_msg(2, &PairConfirm { mac }).await.unwrap();
    let accepted = leg2
        .next(PairResult::TYPE)
        .await
        .is_some_and(|f| f.decode::<PairResult>().unwrap().accepted != 0);
    assert!(
        !accepted,
        "the relayed proof does not verify on the other leg"
    );
    assert!(!ctx.knows(&leg2.key));
}

/// A console that does not know the code: it answers the exchange anyway (accepted, with
/// a made-up confirmation). The app must not store it.
async fn fake_console() -> (std::net::SocketAddr, [u8; 32]) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let id = Identity::generate().unwrap();
    let key = id.public();
    tokio::spawn(async move {
        let (sock, _) = l.accept().await.unwrap();
        let (rh, wh) = sock.into_split();
        let (mut r, mut w) = (FrameReader::new(rh), FrameWriter::new(wh));
        let hs1 = r.recv().await.unwrap();
        let _est = ava1::handshake::server(
            &mut r,
            &mut w,
            hs1,
            &id,
            "fake",
            |_| ava1::handshake::Admission::Pairing,
            0,
        )
        .await
        .unwrap();
        while let Ok(f) = r.recv().await {
            if f.ty == PairPakeClient::TYPE {
                let y = cpace::public(&[9u8; 32], &cpace::generator(&[0u8; 64], 1)).unwrap();
                let _ = w.send_msg(f.channel, &PairPakeServer { y }).await;
            } else if f.ty == PairConfirm::TYPE {
                let _ = w
                    .send_msg(
                        f.channel,
                        &PairResult {
                            accepted: 1,
                            mac: [0x11; 32],
                        },
                    )
                    .await;
            }
        }
    });
    (addr, key)
}

#[tokio::test]
async fn a_fake_console_cannot_pass_the_clients_check() {
    let (addr, fake_key) = fake_console().await;
    let peers = Arc::new(Mutex::new(PeerStore::in_memory()));
    let mut s = connect(
        &addr.to_string(),
        Arc::new(Identity::generate().unwrap()),
        peers.clone(),
        "phone",
        fast(),
    )
    .await
    .unwrap();
    assert!(s.pairing_pending());
    let r = s.confirm_pairing(123456).await;
    assert!(
        matches!(&r, Err(Ava1Error::Refused { code, .. }) if *code == gen::ERR_PAIRING_CODE),
        "{r:?}"
    );
    assert!(!peers.lock().unwrap().contains(&fake_key), "never stored");
}

#[tokio::test]
async fn two_sessions_at_the_last_guess_cannot_both_be_evaluated() {
    // 4 of 5 guesses used from this address. Two sessions then run the PAKE (both pass the
    // budget check at its start) and confirm at the same moment: only one guess is left, so
    // only one may be evaluated and counted. The other is refused without looking at its proof.
    let limits = Limits {
        welcomes_per_ip: 100,
        unpaired: 10,
        ..Limits::default()
    };
    let (addr, ctx, _) = open_server(limits).await;
    for _ in 0..4 {
        let mut g = rogue(addr).await;
        let code = console_code(&ctx, &g.key).await;
        assert!(!g.pake((code + 1) % 1_000_000).await.1);
        drop(g);
        wait_for(Duration::from_secs(5), || ctx.sessions() == 0)
            .await
            .unwrap();
    }
    assert_eq!(ctx.pair_failures(), 4);
    let (mut a, mut b) = (rogue(addr).await, rogue(addr).await);
    let (ca, cb) = (
        console_code(&ctx, &a.key).await,
        console_code(&ctx, &b.key).await,
    );
    let (ka, kb) = (
        a.begin((ca + 1) % 1_000_000).await.unwrap(),
        b.begin((cb + 1) % 1_000_000).await.unwrap(),
    );
    let (ma, mb) = (
        cpace::mac(&ka, b"client", &a.h),
        cpace::mac(&kb, b"client", &b.h),
    );
    let (ra, rb) = tokio::join!(a.finish(ma), b.finish(mb));
    assert!(!ra && !rb);
    assert_eq!(
        ctx.pair_failures(),
        5,
        "one guess evaluated, not two (and not 6 of 5)"
    );
    assert!(
        ctx.pairing_open(),
        "five guesses from one address do not close the window"
    );
}

#[tokio::test]
async fn a_flood_of_sessions_shows_the_newest_code_and_drops_the_older_ones() {
    // Burst of 3, then one per 400 ms, over all addresses. Six sessions at once: three show at
    // once; the 4th and 5th wait and are dropped when the 6th overtakes them, which shows
    // its own code as soon as there is credit. The user's (latest) attempt is on the screen.
    let limits = Limits {
        notice_burst: 3,
        notice_refill: Duration::from_millis(400),
        welcomes_per_ip: 100,
        unpaired: 10,
        ..Limits::default()
    };
    let (addr, ctx, screen) = screen_server(limits).await;
    let mut rogues = Vec::new();
    for _ in 0..6 {
        rogues.push(rogue(addr).await);
    }
    let last_code = console_code(&ctx, &rogues[5].key).await;
    wait_for(Duration::from_secs(5), || screen.lock().unwrap().len() >= 3)
        .await
        .expect("the burst shows");
    assert_eq!(screen.lock().unwrap().len(), 3, "only the burst, at once");
    wait_for(Duration::from_secs(5), || screen.lock().unwrap().len() >= 4)
        .await
        .expect("the newest waits for credit and is shown");
    tokio::time::sleep(Duration::from_millis(900)).await;
    let shown = screen.lock().unwrap().clone();
    assert_eq!(shown.len(), 4, "the 4th and 5th were dropped: {shown:?}");
    assert_eq!(shown[3].1, last_code, "the newest session's own code");
}
