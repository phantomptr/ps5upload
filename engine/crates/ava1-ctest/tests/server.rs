#![cfg(unix)]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ava1::conn::{FrameReader, FrameWriter};
use ava1::gen;
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::session::{connect, Timing};
use ava1::Ava1Error;
use ava1_chaos::{ChaosConfig, ChaosProxy};
use ava1_ctest::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const SECRET: [u8; 32] = [0x42; 32];

fn fast() -> Timing {
    Timing {
        ping_every: Duration::from_millis(100),
        dead_after: Duration::from_millis(500),
        handshake: Duration::from_millis(500),
        ..Timing::default()
    }
}

/// `fast()` server options with the per-address limit out of the way.
fn roomy(handshake_ms: u32) -> ffi::TestOpts {
    ffi::TestOpts {
        ping_ms: 100,
        dead_ms: 500,
        handshake_ms,
        max_conns_per_ip: 10_000,
        ..Default::default()
    }
}

fn opts(pairing_s: u32) -> ffi::TestOpts {
    ffi::TestOpts {
        pairing_s,
        ping_ms: 100,
        dead_ms: 500,
        handshake_ms: 500,
        ..Default::default()
    }
}

async fn stranger(srv: &CServer) -> Result<ava1::session::Session, Ava1Error> {
    connect(
        &srv.addr(),
        Arc::new(Identity::generate().unwrap()),
        Arc::new(Mutex::new(PeerStore::in_memory())),
        "phone",
        fast(),
    )
    .await
}

fn is_busy<T>(r: &Result<T, Ava1Error>) -> bool {
    matches!(r, Err(Ava1Error::Refused { code, .. }) if *code == gen::ERR_BUSY)
}

fn is_pairing_closed<T>(r: &Result<T, Ava1Error>) -> bool {
    matches!(r, Err(Ava1Error::Refused { code, .. }) if *code == gen::ERR_PAIRING_CLOSED)
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ava1-c-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A client the C server already knows, and that knows the C server.
fn paired_client(peers_file: &std::path::Path) -> (Arc<Identity>, Arc<Mutex<PeerStore>>) {
    let me = Arc::new(Identity::generate().unwrap());
    PeerStore::load(peers_file)
        .unwrap()
        .add(me.public(), "rust client")
        .unwrap();
    let mut mine = PeerStore::in_memory();
    mine.add(Identity::from_secret(SECRET).public(), "C test server")
        .unwrap();
    (me, Arc::new(Mutex::new(mine)))
}

async fn wait_conns(s: &CServer, n: i32) {
    let t = Instant::now();
    while s.conns() > n && t.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        s.conns() <= n,
        "C server still has {} connections",
        s.conns()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rust_client_talks_to_the_c_server() {
    let d = dir("basic");
    let (me, peers) = paired_client(&d.join("peers"));
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 500, 500);
    let s = connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap();
    assert!(!s.pairing_pending());
    assert_eq!(s.peer_name(), "C test server");
    assert_eq!(s.node_info().await.unwrap().name, "C test server");
    assert_eq!(
        s.rpc(999, &[]).await.unwrap().status,
        gen::ERR_UNKNOWN_METHOD
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(!s.is_closed() && s.rtt().is_some(), "heartbeats both ways");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_allows_eight_calls_in_flight_and_answers_the_ninth_busy() {
    // SPEC.md §7.4: 8 in flight per session.
    let d = dir("inflight");
    let (me, peers) = paired_client(&d.join("peers"));
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 500, 500);
    let s = Arc::new(
        connect(&srv.addr(), me, peers, "laptop", fast())
            .await
            .unwrap(),
    );
    let mut held = Vec::new();
    for _ in 0..8 {
        let s2 = s.clone();
        held.push(tokio::spawn(async move { s2.rpc(0x7701, &[]).await }));
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        s.rpc(gen::METHOD_NODE_INFO, &[]).await.unwrap().status,
        gen::ERR_BUSY
    );
    for h in held {
        assert_eq!(h.await.unwrap().unwrap().status, gen::STATUS_OK);
    }
    assert_eq!(
        s.rpc(gen::METHOD_NODE_INFO, &[]).await.unwrap().status,
        gen::STATUS_OK
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_replies_up_to_256_kib_and_never_clips_a_larger_claim() {
    // SPEC.md §7.4: RPC_OUT_MAX is 256 KiB; a handler claiming more is ERR_INTERNAL with a cause.
    let d = dir("replycap");
    let (me, peers) = paired_client(&d.join("peers"));
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 500, 500);
    let s = connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap();
    let ask = |n: u32| n.to_le_bytes().to_vec();
    let r = s.rpc(0x7702, &ask(256 * 1024)).await.unwrap();
    assert_eq!((r.status, r.body.len()), (gen::STATUS_OK, 256 * 1024));
    assert!(r.body.iter().all(|b| *b == 0xAB));
    let r = s.rpc(0x7702, &ask(256 * 1024 + 1)).await.unwrap();
    assert_eq!(r.status, gen::ERR_INTERNAL);
    assert_eq!(r.body, b"reply exceeds the 256 KiB RPC cap");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_refuses_a_request_over_56_kib_and_keeps_the_session() {
    let d = dir("reqcap");
    let (me, peers) = paired_client(&d.join("peers"));
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 500, 500);
    let s = connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap();
    let r = s
        .rpc(gen::METHOD_NODE_INFO, &vec![0u8; 56 * 1024])
        .await
        .unwrap();
    assert_eq!(r.status, gen::STATUS_OK);
    let r = s
        .rpc(gen::METHOD_NODE_INFO, &vec![0u8; 56 * 1024 + 1])
        .await
        .unwrap();
    assert_eq!(r.status, gen::ERR_PROTOCOL);
    assert_eq!(r.body, b"request exceeds the 56 KiB RPC cap");
    assert_eq!(
        s.rpc(gen::METHOD_NODE_INFO, &[]).await.unwrap().status,
        gen::STATUS_OK
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ava1_rpc_text_answers_ok_when_it_fits_and_internal_when_truncated() {
    // The pattern ported management handlers use (SPEC.md §7.3): never `ok` with a clipped body.
    let d = dir("rpctext");
    let (me, peers) = paired_client(&d.join("peers"));
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 500, 500);
    let s = connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap();
    let r = s.rpc(0x7703, b"{\"ok\":true}").await.unwrap();
    assert_eq!(
        (r.status, r.body.as_slice()),
        (gen::STATUS_OK, &b"{\"ok\":true}"[..])
    );
    // 15 bytes plus the NUL fit a 16-byte window; 16 do not.
    let r = s.rpc(0x7703, &[b'a'; 15]).await.unwrap();
    assert_eq!((r.status, r.body.len()), (gen::STATUS_OK, 15));
    let r = s.rpc(0x7703, &[b'a'; 16]).await.unwrap();
    assert_eq!(r.status, gen::ERR_INTERNAL);
    assert_eq!(r.body, b"reply truncated");
}

#[tokio::test(flavor = "multi_thread")]
async fn pairing_with_the_c_server() {
    let d = dir("pair");
    let srv = CServer::start(SECRET, &d.join("peers"), 60, 100, 500, 500);
    let me = Arc::new(Identity::generate().unwrap());
    let peers = Arc::new(Mutex::new(PeerStore::in_memory()));
    let mut s = connect(&srv.addr(), me.clone(), peers.clone(), "laptop", fast())
        .await
        .unwrap();
    // The console shows the code once it has welcomed us; connect() can return first. The
    // code exists only there: the user reads it off the screen and types it.
    let code = shown_code(&srv, 1).await;
    assert_eq!(srv.pair_requests().0, 1);
    assert!(matches!(
        s.rpc(gen::METHOD_NODE_INFO, &[]).await,
        Err(Ava1Error::NotPaired)
    ));
    assert_eq!(
        s.rpc_unchecked_for_test(gen::METHOD_NODE_INFO, &[])
            .await
            .unwrap()
            .status,
        gen::ERR_NOT_PAIRED
    );
    assert!(matches!(s.open_lane().await, Err(Ava1Error::NotPaired)));
    assert!(srv.pairing_open());
    s.confirm_pairing(code).await.unwrap();
    assert!(
        !srv.pairing_open(),
        "a successful pairing closes the window"
    );
    s.node_info().await.unwrap();
    let file = std::fs::read_to_string(d.join("peers")).unwrap();
    assert!(
        file.contains(&ava1::hex::encode(&me.public())) && file.contains(" laptop"),
        "{file}"
    );
    // Rust reads what C wrote.
    assert!(PeerStore::load(&d.join("peers"))
        .unwrap()
        .contains(&me.public()));
}

/// SPEC.md §5: a data-plane frame on a control connection whose pairing is not accepted is a
/// protocol error — a sealed Error(ERR_NOT_PAIRED), then the connection closes — not silence.
#[tokio::test(flavor = "multi_thread")]
async fn a_data_frame_before_pairing_is_answered_not_paired_and_closes() {
    let d = dir("unpaired-data");
    let srv = CServer::start(SECRET, &d.join("peers"), 60, 100, 500, 500);
    let me = Arc::new(Identity::generate().unwrap());
    let peers = Arc::new(Mutex::new(PeerStore::in_memory()));
    let s = connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap();
    assert!(s.pairing_pending(), "not paired yet");
    let job = [0x66; 16];
    let link = s.job(job);
    link.control
        .send(&gen::Resume {
            job_id: job,
            manifest_hash: [0; 32],
        })
        .await
        .unwrap();
    let why = tokio::time::timeout(Duration::from_secs(5), s.closed())
        .await
        .expect("the server closed the connection");
    assert!(
        why.contains(&format!("error {}", gen::ERR_NOT_PAIRED)),
        "sealed ERR_NOT_PAIRED: {why}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_refuses_strangers_when_pairing_is_closed() {
    let d = dir("closed");
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 500, 500);
    let r = connect(
        &srv.addr(),
        Arc::new(Identity::generate().unwrap()),
        Arc::new(Mutex::new(PeerStore::in_memory())),
        "x",
        fast(),
    )
    .await;
    assert!(matches!(r, Err(Ava1Error::Refused { code, .. }) if code == gen::ERR_PAIRING_CLOSED));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_window_stays_shut_once_paired_until_a_peer_opens_it() {
    let d = dir("window");
    let (me, peers) = paired_client(&d.join("peers"));
    // pairing_s = 60, but the peers file is not empty: no automatic window (design review, flaw 2).
    let srv = CServer::start(SECRET, &d.join("peers"), 60, 100, 500, 500);
    let stranger = Arc::new(Identity::generate().unwrap());
    let none = Arc::new(Mutex::new(PeerStore::in_memory()));
    let r = connect(&srv.addr(), stranger.clone(), none.clone(), "phone", fast()).await;
    assert!(
        matches!(r, Err(Ava1Error::Refused { code, .. }) if code == gen::ERR_PAIRING_CLOSED),
        "{r:?}"
    );
    let s = connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap();
    s.open_pairing(60).await.unwrap();
    let p = connect(&srv.addr(), stranger, none, "phone", fast())
        .await
        .unwrap();
    assert!(p.pairing_pending());
}

#[tokio::test(flavor = "multi_thread")]
async fn lanes_on_the_c_server() {
    let d = dir("lanes");
    let (me, peers) = paired_client(&d.join("peers"));
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 500, 500);
    let s = connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap();
    let mut lanes = Vec::new();
    for _ in 0..gen::MAX_LANES {
        lanes.push(s.open_lane().await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(lanes.iter().all(|l| !l.is_closed() && l.rtt().is_some()));
    // Superseding: the same id again replaces the old connection.
    let old = lanes.remove(2);
    let _new = s.reopen_lane_for_test(old.id).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), old.closed())
        .await
        .expect("old lane closed by the C server");
    // Lanes end with their session.
    s.close().await;
    for l in &lanes {
        tokio::time::timeout(Duration::from_secs(2), l.closed())
            .await
            .expect("lane ended with session");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_rejects_forged_and_replayed_frames() {
    let d = dir("forged");
    let (me, peers) = paired_client(&d.join("peers"));
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 500, 500);
    let _s = connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap();
    // A forged Join.
    let (rh, wh) = TcpStream::connect(srv.addr()).await.unwrap().into_split();
    let (mut r, mut w) = (FrameReader::new(rh), FrameWriter::new(wh));
    w.send_msg(
        0,
        &gen::Join {
            session_id: [1; 16],
            lane_id: 1,
            client_nonce: [2; 16],
            tag: [3; 16],
        },
    )
    .await
    .unwrap();
    assert_eq!(
        r.recv().await.unwrap().decode::<gen::Error>().unwrap().code,
        gen::ERR_BAD_JOIN
    );
    // A tagged frame with the wrong key on a fresh control connection: the C server just closes.
    let (rh, wh) = TcpStream::connect(srv.addr()).await.unwrap().into_split();
    let (mut r, mut w) = (FrameReader::new(rh), FrameWriter::new(wh));
    w.set_key([9; 32]);
    w.send_msg(0, &gen::Ping { seq: 1, t_us: 1 }).await.unwrap();
    assert!(r.recv().await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn c_server_drops_a_silent_half_handshake() {
    // Review focus 1.
    let d = dir("slow");
    let (me, peers) = paired_client(&d.join("peers"));
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 500, 300);
    let mut raw = TcpStream::connect(srv.addr()).await.unwrap();
    raw.write_all(b"A1\x01\x00\x00").await.unwrap();
    let t = Instant::now();
    let mut b = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(3), raw.read(&mut b))
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(n, 0);
    assert!(
        t.elapsed() < Duration::from_millis(2500),
        "{:?}",
        t.elapsed()
    );
    connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn c_server_refuses_a_storm_then_recovers() {
    // Review focus 2.
    let d = dir("storm");
    let (me, peers) = paired_client(&d.join("peers"));
    // One address may hold only 12 connections by default; lift that so the global
    // limit is what refuses.
    let srv = CServer::start_with(SECRET, &d.join("peers"), roomy(2000));
    let mut held = Vec::new();
    for _ in 0..64 {
        held.push(TcpStream::connect(srv.addr()).await.unwrap());
    }
    let t = Instant::now();
    while srv.conns() < 64 && t.elapsed() < Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let r = connect(&srv.addr(), me.clone(), peers.clone(), "laptop", fast()).await;
    assert!(
        matches!(r, Err(Ava1Error::Refused { code, .. }) if code == gen::ERR_BUSY),
        "{r:?}"
    );
    drop(held);
    wait_conns(&srv, 0).await;
    connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn c_server_drops_a_blackholed_session() {
    // Review focus 3.
    let d = dir("blackhole");
    let (me, peers) = paired_client(&d.join("peers"));
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 500, 500);
    let proxy = ChaosProxy::start(srv.addr().parse().unwrap(), ChaosConfig::default())
        .await
        .unwrap();
    let s = connect(&proxy.addr.to_string(), me, peers, "laptop", fast())
        .await
        .unwrap();
    let _lane = s.open_lane().await.unwrap();
    assert_eq!(srv.conns(), 2);
    proxy.blackhole(true);
    let t = Instant::now();
    wait_conns(&srv, 0).await;
    assert!(
        t.elapsed() < Duration::from_millis(2500),
        "{:?}",
        t.elapsed()
    );
    let why = tokio::time::timeout(Duration::from_secs(2), s.closed())
        .await
        .unwrap();
    assert!(why.contains("stopped answering"), "{why}");
}

#[test]
fn c_store_rejects_bad_identity_and_skips_bad_peers() {
    // Review focus 4.
    let d = dir("store");
    let id = d.join("identity");
    let a = c_identity_load_or_create(&id).unwrap();
    assert_eq!(c_identity_load_or_create(&id).unwrap(), a);
    assert_eq!(
        Identity::load_or_create(&id).unwrap().public(),
        a,
        "Rust reads C's identity file"
    );
    std::fs::write(&id, [7u8; 33]).unwrap();
    assert!(c_identity_load_or_create(&id).is_err());
    assert_eq!(std::fs::read(&id).unwrap(), vec![7u8; 33], "never replaced");

    let peers = d.join("peers");
    let good = "cd".repeat(32);
    std::fs::write(
        &peers,
        format!("junk\n{good} 1700000000 Phat\nxx\n{good}9 1 y\n\n"),
    )
    .unwrap();
    assert_eq!(c_peers_load(&peers, &[0xcd; 32]), (1, true));
    assert_eq!(c_peers_load(&d.join("missing"), &[0; 32]), (0, false));
}

#[tokio::test(flavor = "multi_thread")]
async fn c_server_drops_a_trickling_handshake() {
    // The handshake deadline is absolute: a client that keeps feeding bytes is still cut.
    let d = dir("trickle");
    let (me, peers) = paired_client(&d.join("peers"));
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 500, 500);
    let mut raw = TcpStream::connect(srv.addr()).await.unwrap();
    let t = Instant::now();
    let mut header = [0u8; 16];
    header[..3].copy_from_slice(b"A1\x01");
    for b in header {
        if raw.write_all(&[b]).await.is_err() {
            break; // already closed by the server
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        if t.elapsed() > Duration::from_millis(1200) {
            break;
        }
    }
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(2), raw.read(&mut buf))
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(n, 0, "the server closed the trickling connection");
    assert!(
        t.elapsed() < Duration::from_millis(2500),
        "{:?}",
        t.elapsed()
    );
    connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn c_server_refuses_a_17th_session_before_the_handshake() {
    let d = dir("sess17");
    let mut mine = PeerStore::in_memory();
    mine.add(Identity::from_secret(SECRET).public(), "C test server")
        .unwrap();
    let mine = Arc::new(Mutex::new(mine));
    let mut store = PeerStore::load(&d.join("peers")).unwrap();
    let ids: Vec<Arc<Identity>> = (0..17)
        .map(|_| Arc::new(Identity::generate().unwrap()))
        .collect();
    for (i, id) in ids.iter().enumerate() {
        store.add(id.public(), &format!("c{i}")).unwrap();
    }
    let srv = CServer::start_with(SECRET, &d.join("peers"), roomy(2000));
    let mut sessions = Vec::new();
    for id in &ids[..16] {
        sessions.push(
            connect(&srv.addr(), id.clone(), mine.clone(), "c", fast())
                .await
                .unwrap(),
        );
    }
    // connect() reads the first frame expecting Hs2: Refused means the server sent the
    // error instead of Hs2, i.e. it refused before any Noise work.
    let r = connect(&srv.addr(), ids[16].clone(), mine.clone(), "c", fast()).await;
    assert!(
        matches!(r, Err(Ava1Error::Refused { code, .. }) if code == gen::ERR_BUSY),
        "{r:?}"
    );
    // A freed slot is usable again.
    sessions.pop().unwrap().close().await;
    let t = Instant::now();
    loop {
        match connect(&srv.addr(), ids[16].clone(), mine.clone(), "c", fast()).await {
            Ok(_) => break,
            Err(e) => assert!(t.elapsed() < Duration::from_secs(3), "{e:?}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_never_reuses_a_lane_key() {
    let d = dir("rejoin");
    let (me, peers) = paired_client(&d.join("peers"));
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 500, 500);
    let s = connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap();
    async fn ack(addr: &str, j: &gen::Join) -> [u8; 16] {
        let (rh, wh) = TcpStream::connect(addr).await.unwrap().into_split();
        let (mut r, mut w) = (FrameReader::new(rh), FrameWriter::new(wh));
        w.send_msg(0, j).await.unwrap();
        r.recv()
            .await
            .unwrap()
            .decode::<gen::JoinAck>()
            .unwrap()
            .server_nonce
    }
    let j = s.join_frame_for_test(4, [0x61; 16]);
    let sn1 = ack(&srv.addr(), &j).await;
    for i in 0..70u8 {
        ack(&srv.addr(), &s.join_frame_for_test(4, [i; 16])).await;
        // One lane connection at a time, so the per-address limit never applies.
        wait_conns(&srv, 1).await;
    }
    // Past the 64-entry window the replay is acked again, but with fresh keys.
    let sn2 = ack(&srv.addr(), &j).await;
    assert_ne!(sn1, sn2);
    assert_ne!(
        s.lane_keys_for_test(4, &j.client_nonce, &sn1),
        s.lane_keys_for_test(4, &j.client_nonce, &sn2)
    );
}

type RawW = FrameWriter<tokio::net::tcp::OwnedWriteHalf>;
type RawR = FrameReader<tokio::net::tcp::OwnedReadHalf>;

/// A paired client driven by hand: handshake only, small receive buffer, no heartbeats.
async fn raw_session(addr: &str, me: &Identity) -> (RawR, RawW) {
    let sock = tokio::net::TcpSocket::new_v4().unwrap();
    sock.set_recv_buffer_size(4096).unwrap();
    let stream = sock.connect(addr.parse().unwrap()).await.unwrap();
    let (rh, wh) = stream.into_split();
    let (mut r, mut w) = (FrameReader::new(rh), FrameWriter::new(wh));
    let est = ava1::handshake::client(&mut r, &mut w, me, "raw", |_| true, 0)
        .await
        .unwrap();
    assert!(est.pairing.is_none());
    (r, w)
}

fn half_frame_header() -> Vec<u8> {
    ava1::frame::Header {
        ty: gen::RpcRequest::TYPE,
        flags: ava1::frame::FLAG_SEALED,
        channel: 2,
        body_len: 60_000,
    }
    .encode()
    .to_vec()
}

use ava1::wire::FrameMessage;

#[tokio::test(flavor = "multi_thread")]
async fn a_max_size_frame_slower_than_dead_after_keeps_the_c_session() {
    let d = dir("bigframe");
    let (me, peers) = paired_client(&d.join("peers"));
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 200, 1000, 3000);
    let cfg = ChaosConfig {
        bytes_per_sec: Some(16 * 1024),
        ..ChaosConfig::default()
    };
    let proxy = ChaosProxy::start(srv.addr().parse().unwrap(), cfg)
        .await
        .unwrap();
    let t = Timing {
        ping_every: Duration::from_millis(200),
        dead_after: Duration::from_millis(1000),
        handshake: Duration::from_secs(3),
        ..Timing::default()
    };
    let s = connect(&proxy.addr.to_string(), me, peers, "laptop", t)
        .await
        .unwrap();
    let start = Instant::now();
    let r = s
        .rpc(gen::METHOD_NODE_INFO, &vec![0x5a; 56 * 1024])
        .await
        .unwrap();
    assert_eq!(r.status, gen::STATUS_OK);
    assert!(start.elapsed() > t.dead_after * 3, "{:?}", start.elapsed());
    // The C server kept pinging while it read the frame, so the client stayed alive too.
    assert!(!s.is_closed(), "client side");
    assert_eq!(srv.conns(), 1, "server side");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_drops_a_peer_that_stops_mid_frame() {
    let d = dir("midframe");
    let (me, _peers) = paired_client(&d.join("peers"));
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 500, 500);
    let (_r, w) = raw_session(&srv.addr(), &me).await;
    let mut stream = w.into_inner();
    let mut raw = half_frame_header();
    raw.extend_from_slice(&[0u8; 1000]);
    stream.write_all(&raw).await.unwrap();
    let t = Instant::now();
    wait_conns(&srv, 0).await;
    assert!(
        t.elapsed() < Duration::from_millis(2000),
        "{:?}",
        t.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_drops_a_dripping_frame_by_the_rate_floor() {
    let d = dir("drip");
    let (me, _peers) = paired_client(&d.join("peers"));
    let srv = CServer::start_with(
        SECRET,
        &d.join("peers"),
        ffi::TestOpts {
            ping_ms: 100,
            dead_ms: 500,
            handshake_ms: 500,
            min_frame_rate: 64 * 1024,
            ..Default::default()
        },
    );
    let (_r, w) = raw_session(&srv.addr(), &me).await;
    let mut stream = w.into_inner();
    stream.write_all(&half_frame_header()).await.unwrap();
    let drip = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if stream.write_all(&[0]).await.is_err() {
                return;
            }
        }
    });
    let t = Instant::now();
    while srv.conns() > 0 && t.elapsed() < Duration::from_secs(8) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(srv.conns(), 0, "the rate floor ends a dripping frame");
    assert!(
        t.elapsed() > Duration::from_millis(900),
        "{:?}",
        t.elapsed()
    );
    drip.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_drops_a_peer_that_never_reads_its_replies() {
    let d = dir("noread");
    let (me, peers) = paired_client(&d.join("peers"));
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 500, 500);
    let (_r, mut w) = raw_session(&srv.addr(), &me).await; // _r is never read
    let flood = tokio::spawn(async move {
        let q = gen::RpcRequest {
            method: gen::METHOD_NODE_INFO,
            body: Vec::new(),
        };
        let mut id = 1u32;
        while w.send_msg(id, &q).await.is_ok() {
            id = id.wrapping_add(1);
        }
    });
    let t = Instant::now();
    while srv.conns() > 0 && t.elapsed() < Duration::from_secs(10) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        srv.conns(),
        0,
        "the C server drops a peer that stopped reading"
    );
    flood.abort();
    connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap()
        .node_info()
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_ends_unconfirmed_sessions_with_the_window_or_the_deadline() {
    let d = dir("unconfirmed");
    // Deadline: the window stays open, the session does not.
    let srv = CServer::start_with(
        SECRET,
        &d.join("peers"),
        ffi::TestOpts {
            pair_confirm_ms: 400,
            ..opts(60)
        },
    );
    let s = stranger(&srv).await.unwrap();
    let why = tokio::time::timeout(Duration::from_secs(5), s.closed())
        .await
        .expect("closed at the confirm deadline");
    assert!(why.contains("not confirmed"), "{why}");
    assert!(srv.pairing_open());
    wait_conns(&srv, 0).await;
    drop(srv);
    // Window: closing it (here by letting 1 s run out) ends the waiting session.
    let srv = CServer::start_with(SECRET, &d.join("peers"), opts(1));
    let s = stranger(&srv).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), s.closed())
        .await
        .expect("closed with the window");
    assert!(!srv.pairing_open());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_caps_unconfirmed_sessions_and_connections_per_address() {
    let d = dir("caps");
    let srv = CServer::start_with(SECRET, &d.join("peers"), opts(60));
    let a = stranger(&srv).await.unwrap();
    let _b = stranger(&srv).await.unwrap();
    let c = stranger(&srv).await;
    assert!(is_busy(&c), "a third unconfirmed device: {:?}", c.err());
    assert_eq!(
        srv.pair_requests().0,
        2,
        "every welcomed session shows its own code"
    );
    a.close().await;
    wait_conns(&srv, 1).await;
    stranger(&srv).await.unwrap();
    drop(srv);

    let (me, peers) = paired_client(&d.join("peers2"));
    let srv = CServer::start_with(
        SECRET,
        &d.join("peers2"),
        ffi::TestOpts {
            handshake_ms: 3000,
            ..opts(0)
        },
    );
    let mut held = Vec::new();
    for _ in 0..12 {
        held.push(TcpStream::connect(srv.addr()).await.unwrap());
    }
    let t = Instant::now();
    while srv.conns() < 12 && t.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let r = connect(&srv.addr(), me.clone(), peers.clone(), "laptop", fast()).await;
    assert!(
        is_busy(&r),
        "a 13th connection from one address: {:?}",
        r.err()
    );
    held.pop();
    wait_conns(&srv, 11).await;
    connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_with_an_unreadable_peers_file_pairs_no_one_and_leaves_it_alone() {
    let d = dir("unreadable");
    let path = d.join("peers");
    std::fs::create_dir(&path).unwrap(); // exists, but reading it fails
    let srv = CServer::start_with(SECRET, &path, opts(60));
    assert!(srv.logs() >= 1, "the failure is logged");
    assert!(
        !srv.pairing_open(),
        "no automatic window: unknown is not unpaired"
    );
    let r = stranger(&srv).await;
    assert!(is_pairing_closed(&r), "{:?}", r.err());
    // Even with the window forced open, nothing is accepted or written.
    srv.open_pairing(60);
    let mut s = stranger(&srv).await.unwrap();
    let code = shown_code(&srv, 1).await;
    assert!(s.confirm_pairing(code).await.is_err());
    assert!(path.is_dir(), "left alone");
    assert!(!d.join("peers.tmp").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_reports_a_pairing_it_cannot_store() {
    let d = dir("nostore");
    // The peers file's directory does not exist: loading finds no file (an empty store,
    // so the window opens), but the pairing cannot be written.
    let srv = CServer::start_with(SECRET, &d.join("gone").join("peers"), opts(60));
    let mut s = stranger(&srv).await.unwrap();
    let code = shown_code(&srv, 1).await;
    assert!(s.confirm_pairing(code).await.is_err());
    assert!(srv.logs() >= 1, "ava1_peers_save's failure is logged");
    // Nothing half-stored in memory either: the same device is still a stranger.
    assert!(srv.pairing_open());
}

/// Sends `j` on a fresh connection; returns the JoinAck's server nonce.
async fn c_join_ack(addr: &str, j: &gen::Join) -> [u8; 16] {
    let (rh, wh) = TcpStream::connect(addr).await.unwrap().into_split();
    let (mut r, mut w) = (FrameReader::new(rh), FrameWriter::new(wh));
    w.send_msg(0, j).await.unwrap();
    r.recv()
        .await
        .unwrap()
        .decode::<gen::JoinAck>()
        .unwrap()
        .server_nonce
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replayed_join_does_not_take_over_a_live_c_lane() {
    let d = dir("replay-takeover");
    let (me, peers) = paired_client(&d.join("peers"));
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 500, 500);
    let s = connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap();
    // A Join captured on the network, kept until the server has forgotten its nonce.
    let captured = s.join_frame_for_test(1, [0x77; 16]);
    c_join_ack(&srv.addr(), &captured).await;
    for i in 0..70u8 {
        c_join_ack(&srv.addr(), &s.join_frame_for_test(3, [i; 16])).await;
        wait_conns(&srv, 1).await;
    }
    let live = s.open_lane().await.unwrap();
    assert_eq!(live.id, 1);
    let (rh, wh) = TcpStream::connect(srv.addr()).await.unwrap().into_split();
    let (mut r, mut w) = (FrameReader::new(rh), FrameWriter::new(wh));
    w.send_msg(0, &captured).await.unwrap();
    r.recv().await.unwrap().decode::<gen::JoinAck>().unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!live.is_closed(), "a replayed Join ended the live lane");
    w.set_key([0x13; 32]);
    w.send_msg(0, &gen::Ping { seq: 1, t_us: 1 }).await.unwrap();
    assert!(r.recv().await.is_err(), "the replayer is disconnected");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        !live.is_closed(),
        "a forged first frame ended the live lane"
    );
    // A genuine re-join still takes the lane over at once.
    let again = s.reopen_lane_for_test(1).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), live.closed())
        .await
        .expect("the genuine re-join superseded the old connection");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!again.is_closed());
}

/// S1: message 3 carrying `ci_bytes` as its ClientInfo. Returns the sealed frame the C
/// server answers with (a decoded Error) and whether it showed a pairing request.
async fn c_server_with_client_info(ci_bytes: Vec<u8>) -> (Option<gen::Error>, u32) {
    let d = dir("s1-clientinfo");
    let srv = CServer::start_with(SECRET, &d.join("peers"), opts(60));
    let me = Identity::generate().unwrap();
    let stream = TcpStream::connect(srv.addr()).await.unwrap();
    let (rh, wh) = stream.into_split();
    let (mut r, mut w) = (FrameReader::new(rh), FrameWriter::new(wh));
    let mut hs = ava1::keys::Handshake::initiator(&me).unwrap();
    let hello = gen::HelloInfo {
        version_min: gen::PROTOCOL_VERSION,
        version_max: gen::PROTOCOL_VERSION,
        caps: 0,
    };
    use ava1::wire::Message;
    let noise = hs.write(&hello.to_bytes().unwrap()).unwrap();
    w.send_msg(0, &gen::Hs1 { noise }).await.unwrap();
    let m2: gen::Hs2 = r.recv().await.unwrap().decode().unwrap();
    let si = gen::ServerInfo::decode(&hs.read(&m2.noise).unwrap()).unwrap();
    assert_ne!(si.pair_commit, [0; 32], "the C server commits in message 2");
    let noise = hs.write(&ci_bytes).unwrap();
    w.send_msg(0, &gen::Hs3 { noise }).await.unwrap();
    let k = hs.finish();
    r.set_key(ava1::keys::control_key(&k.s2c));
    let seen = match tokio::time::timeout(Duration::from_secs(2), r.recv()).await {
        Ok(Ok(f)) if f.ty == gen::Error::TYPE => f.decode::<gen::Error>().ok(),
        _ => None,
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    (seen, srv.pair_requests().0)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_refuses_a_client_without_its_pairing_nonce() {
    use ava1::wire::Message;
    let with = gen::ClientInfo {
        nonce_c: [1; 16],
        name: Some("x".into()),
    }
    .to_bytes()
    .unwrap();
    // Empty, and the old name-only layout: neither carries nonce_c.
    for bytes in [Vec::new(), with[16..].to_vec()] {
        let (seen, shown) = c_server_with_client_info(bytes).await;
        assert_eq!(seen.expect("a sealed refusal").code, gen::ERR_PROTOCOL);
        assert_eq!(shown, 0, "no pairing request for a refused client");
    }
    // The same path with the nonce succeeds: the refusal above is the missing field.
    let (seen, shown) = c_server_with_client_info(with).await;
    assert!(
        seen.as_ref().is_none_or(|e| e.code != gen::ERR_PROTOCOL),
        "{seen:?}"
    );
    assert_eq!(shown, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_notifies_only_once_welcome_is_sent() {
    let d = dir("notify-after-welcome");
    let srv = CServer::start_with(SECRET, &d.join("peers"), opts(60));
    // A stranger that finishes the Noise handshake and resets the connection at once: the
    // server's Welcome cannot be delivered, so nobody is there to pair with.
    {
        let me = Identity::generate().unwrap();
        let stream = TcpStream::connect(srv.addr()).await.unwrap();
        stream.set_zero_linger().unwrap();
        let (rh, wh) = stream.into_split();
        let (mut r, mut w) = (FrameReader::new(rh), FrameWriter::new(wh));
        let mut hs = ava1::keys::Handshake::initiator(&me).unwrap();
        let hello = gen::HelloInfo {
            version_min: gen::PROTOCOL_VERSION,
            version_max: gen::PROTOCOL_VERSION,
            caps: 0,
        };
        use ava1::wire::Message;
        let noise = hs.write(&hello.to_bytes().unwrap()).unwrap();
        w.send_msg(0, &gen::Hs1 { noise }).await.unwrap();
        let m2: gen::Hs2 = r.recv().await.unwrap().decode().unwrap();
        hs.read(&m2.noise).unwrap();
        let ci = gen::ClientInfo {
            nonce_c: [7; 16],
            name: Some("ghost".into()),
        };
        let noise = hs.write(&ci.to_bytes().unwrap()).unwrap();
        w.send_msg(0, &gen::Hs3 { noise }).await.unwrap();
        // Dropping both halves with a zero linger sends RST.
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        srv.pair_requests().0,
        0,
        "nobody was welcomed, nothing shown"
    );
    // A real device asking a moment later is shown: the ghost spent no notification.
    let _p = stranger(&srv).await.unwrap();
    let t = Instant::now();
    while srv.pair_requests().0 == 0 && t.elapsed() < Duration::from_secs(1) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(srv.pair_requests().0, 1, "the real request was not shown");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_whose_link_died_reconnects_to_the_c_server_with_all_its_lanes() {
    let d = dir("reconnect");
    let (me, peers) = paired_client(&d.join("peers"));
    // dead_after 5 s: the old connections would linger that long.
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 5000, 500);
    let t = Timing {
        dead_after: Duration::from_secs(5),
        ..fast()
    };
    let proxy = ChaosProxy::start(srv.addr().parse().unwrap(), ChaosConfig::default())
        .await
        .unwrap();
    let old = connect(
        &proxy.addr.to_string(),
        me.clone(),
        peers.clone(),
        "laptop",
        t,
    )
    .await
    .unwrap();
    let mut old_lanes = Vec::new();
    for _ in 0..gen::MAX_LANES {
        old_lanes.push(old.open_lane().await.unwrap());
    }
    assert_eq!(srv.conns(), 9);
    proxy.blackhole(true);
    let new = connect(&srv.addr(), me, peers, "laptop", t).await.unwrap();
    let mut lanes = Vec::new();
    for i in 0..gen::MAX_LANES {
        match new.open_lane().await {
            Ok(l) => lanes.push(l),
            Err(e) => panic!("lane {} of the new session: {e:?}", i + 1),
        }
    }
    new.node_info().await.unwrap();
    // The old connections end right away, not after dead_after.
    wait_conns(&srv, 9).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!new.is_closed() && lanes.iter().all(|l| !l.is_closed()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_c_reader_kept_from_reading_past_dead_after_keeps_a_live_session() {
    // The reader thread can spend dead_after and more outside recv() — here waiting for
    // the peers-file lock while another session's pairing is being written — while its
    // client keeps sending. Those bytes are waiting in the socket: the session is alive.
    let d = dir("away");
    let (me, _peers) = paired_client(&d.join("peers"));
    let srv = CServer::start(SECRET, &d.join("peers"), 0, 100, 500, 500);
    srv.open_pairing(60);
    // The pairing write goes to peers.tmp first. As a FIFO, opening it blocks until
    // someone reads: the pairing below holds the lock for as long as the test likes.
    let fifo = d.join("peers.tmp");
    assert!(std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let mut a = stranger(&srv).await.unwrap();
    let code = shown_code(&srv, 1).await;
    let a_confirm = tokio::spawn(async move {
        let _ = a.confirm_pairing(code).await;
        a
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    // A paired device confirms as well (harmless: already paired), so its reader waits
    // for the same lock. It keeps pinging all the while.
    let (mut r, mut w) = raw_session(&srv.addr(), &me).await;
    w.send_msg(1, &gen::PairConfirm { mac: [0; 32] })
        .await
        .unwrap();
    let release = {
        let fifo = fifo.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1000));
            let mut f = std::fs::File::open(&fifo).unwrap();
            let mut sink = Vec::new();
            std::io::Read::read_to_end(&mut f, &mut sink).unwrap();
        })
    };
    for seq in 0..15 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        w.send_msg(0, &gen::Ping { seq, t_us: 1 }).await.unwrap();
    }
    release.join().unwrap();
    let q = gen::RpcRequest {
        method: gen::METHOD_NODE_INFO,
        body: Vec::new(),
    };
    w.send_msg(2, &q).await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let f = r.recv().await?;
            if f.ty == gen::RpcResponse::TYPE {
                return Ok::<_, Ava1Error>(f.decode::<gen::RpcResponse>()?.status);
            }
        }
    })
    .await
    .expect("an answer");
    assert_eq!(
        got.unwrap(),
        gen::STATUS_OK,
        "the session was dropped though its client never went quiet"
    );
    drop(a_confirm);
}

// ---- Launch tokens (SPEC.md §5.2) ----

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// The C server as ava1_glue.c starts it from a stamped slot: the launcher's key in its
/// peers file, and the slot's key (state 1) or key and token (state 2) in its config.
/// The pairing window is open, as on a fresh console.
fn launched_c_server(d: &std::path::Path, launcher: [u8; 32], token: Option<[u8; 16]>) -> CServer {
    PeerStore::load(&d.join("peers"))
        .unwrap()
        .add(launcher, "launcher")
        .unwrap();
    CServer::start_with(
        SECRET,
        &d.join("peers"),
        ffi::TestOpts {
            launch: if token.is_some() { 2 } else { 1 },
            launch_key: launcher,
            launch_token: token.unwrap_or_default(),
            ..opts(60)
        },
    )
}

fn launch_client(tokens: ava1::launch::LaunchTokens) -> (Arc<Identity>, Arc<Mutex<PeerStore>>) {
    (
        Arc::new(Identity::generate().unwrap()),
        Arc::new(Mutex::new(
            PeerStore::in_memory().with_launch_tokens(tokens),
        )),
    )
}

fn c_server_key() -> [u8; 32] {
    Identity::from_secret(SECRET).public()
}

#[test]
fn the_test_options_mirror_the_c_struct() {
    assert_eq!(
        unsafe { ffi::ava1_test_sizeof_opts() },
        std::mem::size_of::<ffi::TestOpts>()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_helper_launched_with_a_token_pairs_with_no_code() {
    let d = dir("launch-ok");
    let tokens = ava1::launch::LaunchTokens::in_memory();
    let token = tokens.issue().unwrap();
    let (me, peers) = launch_client(tokens);
    let srv = launched_c_server(&d, me.public(), Some(token));
    let s = connect(&srv.addr(), me, peers.clone(), "laptop", fast())
        .await
        .unwrap();
    assert!(!s.pairing_pending(), "no prompt");
    assert!(peers.lock().unwrap().contains(&c_server_key()));
    assert_eq!(s.node_info().await.unwrap().name, "C test server");
    s.open_lane().await.unwrap();
    assert_eq!(
        unsafe { ffi::ava1_test_pair_requests() },
        0,
        "nothing shown on the console"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_helper_with_a_token_we_did_not_issue_means_pairing() {
    let d = dir("launch-wrong");
    let tokens = ava1::launch::LaunchTokens::in_memory();
    tokens.issue().unwrap();
    let (me, peers) = launch_client(tokens);
    let srv = launched_c_server(&d, me.public(), Some([0x99; 16]));
    let s = connect(&srv.addr(), me, peers.clone(), "laptop", fast())
        .await
        .unwrap();
    assert!(s.pairing_pending());
    assert!(!peers.lock().unwrap().contains(&c_server_key()));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_helper_with_an_expired_token_means_pairing() {
    let d = dir("launch-expired");
    let tokens = ava1::launch::LaunchTokens::in_memory();
    let token = [0x42; 16];
    tokens
        .record(token, now_unix() - ava1::launch::TOKEN_TTL_S - 60)
        .unwrap();
    let (me, peers) = launch_client(tokens);
    let srv = launched_c_server(&d, me.public(), Some(token));
    let s = connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap();
    assert!(s.pairing_pending());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_helper_stamped_with_a_key_only_pairs_as_before() {
    let d = dir("launch-keyonly");
    let tokens = ava1::launch::LaunchTokens::in_memory();
    tokens.issue().unwrap();
    let (me, peers) = launch_client(tokens);
    let srv = launched_c_server(&d, me.public(), None);
    let mut s = connect(&srv.addr(), me, peers, "laptop", fast())
        .await
        .unwrap();
    assert!(s.pairing_pending());
    s.confirm_trusted().unwrap();
    s.node_info().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn another_client_of_the_c_helper_gets_no_proof() {
    let d = dir("launch-other");
    let tokens = ava1::launch::LaunchTokens::in_memory();
    let token = tokens.issue().unwrap();
    let launcher = Identity::generate().unwrap().public();
    let srv = launched_c_server(&d, launcher, Some(token));
    // The launcher is a peer, so the automatic window stayed shut: open one, as a paired
    // device's pairing.open would.
    srv.open_pairing(60);
    // Holds the token, but is not the launcher's key: no proof is ever sent to it.
    let (me, peers) = launch_client(tokens);
    let s = connect(&srv.addr(), me, peers.clone(), "phone", fast())
        .await
        .unwrap();
    assert!(s.pairing_pending());
    assert!(!peers.lock().unwrap().contains(&c_server_key()));
}

#[tokio::test(flavor = "multi_thread")]
async fn each_c_handshake_proves_afresh_and_an_old_proof_does_not_replay() {
    let d = dir("launch-replay");
    let me = Identity::generate().unwrap();
    let token = [0x6c; 16];
    let srv = launched_c_server(&d, me.public(), Some(token));
    let mut seen = Vec::new();
    for _ in 0..2 {
        let stream = TcpStream::connect(srv.addr()).await.unwrap();
        let (rh, wh) = stream.into_split();
        let (mut r, mut w) = (FrameReader::new(rh), FrameWriter::new(wh));
        let got = std::sync::Mutex::new(None);
        let est = ava1::handshake::client_launched(
            &mut r,
            &mut w,
            &me,
            "raw",
            None,
            |_| false,
            |h, p| {
                *got.lock().unwrap() = Some((*h, *p));
                ava1::launch::proof(&token, h) == *p
            },
            0,
        )
        .await
        .unwrap();
        assert!(est.launched && est.pairing.is_none());
        seen.push(got.into_inner().unwrap().expect("a proof was sent"));
    }
    let ((h1, p1), (h2, p2)) = (seen[0], seen[1]);
    assert_ne!(h1, h2);
    assert_ne!(p1, p2);
    assert_ne!(
        ava1::launch::proof(&token, &h2),
        p1,
        "the first proof fails on the second handshake"
    );
}

/// The code the C console shows (its notification: the one place it exists), once its
/// `n`th pairing notification has fired.
async fn shown_code(srv: &CServer, n: u32) -> u32 {
    let t = Instant::now();
    while srv.pair_requests().0 < n && t.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(srv.pair_requests().0 >= n, "the console showed no code");
    srv.pair_requests().1
}

// ---- the pairing PAKE (SPEC.md §4.6, §5.5) against the C console ----

use ava1::conn::Frame;
use ava1::cpace;
use ava1::wire::Message;

struct Rogue {
    r: RawR,
    w: RawW,
    h: [u8; 64],
    key: [u8; 32],
    seen: Vec<Vec<u8>>,
}

/// A LAN host with a throwaway key, welcomed with knows_you = 0.
async fn rogue(addr: &str) -> Rogue {
    let me = Identity::generate().unwrap();
    let (rh, wh) = TcpStream::connect(addr).await.unwrap().into_split();
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
    async fn next(&mut self, ty: u8) -> Option<Frame> {
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

    /// The whole exchange with `guess` as the code: what it sent, and whether it was accepted.
    async fn pake(&mut self, guess: u32) -> (Option<([u8; 32], [u8; 32])>, bool) {
        let g = cpace::generator(&self.h, guess);
        let x = [0x5au8; 32];
        let ya = cpace::public(&x, &g).unwrap();
        self.w
            .send_msg(1, &gen::PairPakeClient { y: ya })
            .await
            .unwrap();
        let Some(f) = self.next(gen::PairPakeServer::TYPE).await else {
            return (None, false);
        };
        self.seen.push(f.body.to_vec());
        let yb: gen::PairPakeServer = f.decode().unwrap();
        let k = cpace::key(&self.h, &x, &yb.y, &ya, &yb.y).unwrap();
        let mac = cpace::mac(&k, b"client", &self.h);
        self.w.send_msg(2, &gen::PairConfirm { mac }).await.unwrap();
        let Some(f) = self.next(gen::PairResult::TYPE).await else {
            return (Some((ya, mac)), false);
        };
        self.seen.push(f.body.to_vec());
        (
            Some((ya, mac)),
            f.decode::<gen::PairResult>().unwrap().accepted != 0,
        )
    }

    async fn refused(&mut self) -> bool {
        self.next(gen::PairResult::TYPE)
            .await
            .is_none_or(|f| f.decode::<gen::PairResult>().unwrap().accepted == 0)
    }
}

fn stored(d: &std::path::Path, key: &[u8; 32]) -> bool {
    std::fs::read_to_string(d.join("peers"))
        .map(|t| t.contains(&ava1::hex::encode(key)))
        .unwrap_or(false)
}

fn wrong(c: u32) -> u32 {
    (c + 1) % 1_000_000
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rogue_that_skips_the_proof_is_refused_by_the_c_server() {
    let d = dir("pk-noproof");
    let srv = CServer::start(SECRET, &d.join("peers"), 60, 100, 500, 500);
    let mut g = rogue(&srv.addr()).await;
    g.w.send(5, 1, &[0, 0]).await.unwrap(); // the old PairConfirm, nothing to prove
    assert!(g.refused().await);
    assert!(!stored(&d, &g.key));
    assert!(srv.pairing_open(), "one failure leaves the window open");
    let mut g = rogue(&srv.addr()).await;
    g.w.send_msg(1, &gen::PairConfirm { mac: [7; 32] })
        .await
        .unwrap();
    assert!(g.refused().await, "a confirm with no PAKE before it");
    assert!(!stored(&d, &g.key));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rogue_that_does_not_know_the_code_fails_and_learns_nothing_from_the_c_server() {
    let d = dir("pk-wrong");
    let srv = CServer::start(SECRET, &d.join("peers"), 60, 100, 500, 500);
    let mut g = rogue(&srv.addr()).await;
    let code = shown_code(&srv, 1).await;
    // All it has: h. A transcript-bound guess is not the code.
    let derived = u32::from_le_bytes(g.h[..4].try_into().unwrap()) % 1_000_000;
    let guess = if derived == code {
        wrong(code)
    } else {
        derived
    };
    assert!(!g.pake(guess).await.1);
    assert!(!stored(&d, &g.key));
    assert!(srv.logs() >= 1, "each failure is logged");
    let ascii = format!("{code:06}").into_bytes();
    let le = code.to_le_bytes();
    for body in &g.seen {
        assert!(!body.windows(6).any(|w| w == ascii.as_slice()));
        assert!(!body.windows(4).any(|w| w == le.as_slice()) || code == 0);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_pairs_the_right_code() {
    let d = dir("pk-right");
    let srv = CServer::start(SECRET, &d.join("peers"), 60, 100, 500, 500);
    let mut g = rogue(&srv.addr()).await;
    let code = shown_code(&srv, 1).await;
    assert!(g.pake(code).await.1);
    assert!(stored(&d, &g.key));
    assert!(!srv.pairing_open());
}

#[tokio::test(flavor = "multi_thread")]
async fn one_addresses_five_wrong_guesses_spend_its_budget_on_the_c_server() {
    let d = dir("pk-budget");
    let srv = CServer::start_with(
        SECRET,
        &d.join("peers"),
        ffi::TestOpts {
            max_welcomes_per_ip: 100,
            ..opts(60)
        },
    );
    for i in 0..5u32 {
        let mut g = rogue(&srv.addr()).await;
        let code = shown_code(&srv, i + 1).await;
        assert!(!g.pake(wrong(code)).await.1, "guess {i}");
        drop(g);
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert_eq!(srv.pair_guesses(), 5);
    assert!(
        srv.pairing_open(),
        "five guesses from one address do not close the window"
    );
    // Out of guesses, even the right code is refused, and refusing it is free.
    let mut g = rogue(&srv.addr()).await;
    let code = shown_code(&srv, 6).await;
    assert!(!g.pake(code).await.1);
    assert_eq!(srv.pair_guesses(), 5);
    // Reopened (a paired device's pairing.open), the budget starts again.
    srv.open_pairing(60);
    let mut g = rogue(&srv.addr()).await;
    let code = shown_code(&srv, 7).await;
    assert!(g.pake(code).await.1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_global_cap_on_guesses_closes_the_c_servers_window() {
    let d = dir("pk-global");
    let srv = CServer::start_with(
        SECRET,
        &d.join("peers"),
        ffi::TestOpts {
            max_pair_fails_per_ip: 100,
            max_pair_fails_total: 5,
            max_welcomes_per_ip: 100,
            ..opts(60)
        },
    );
    for i in 0..5u32 {
        assert!(srv.pairing_open(), "still open before guess {i}");
        let mut g = rogue(&srv.addr()).await;
        let code = shown_code(&srv, i + 1).await;
        assert!(!g.pake(wrong(code)).await.1);
        drop(g);
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert!(!srv.pairing_open(), "the window closed");
    let r = stranger(&srv).await;
    assert!(is_pairing_closed(&r), "{:?}", r.err());
    srv.open_pairing(60);
    let mut g = rogue(&srv.addr()).await;
    let code = shown_code(&srv, 6).await;
    assert!(g.pake(code).await.1);
}

#[tokio::test(flavor = "multi_thread")]
async fn junk_sessions_never_close_the_c_servers_window() {
    let d = dir("pk-junk");
    let srv = CServer::start_with(
        SECRET,
        &d.join("peers"),
        ffi::TestOpts {
            max_welcomes_per_ip: 1000,
            ..opts(60)
        },
    );
    for _ in 0..30 {
        let mut g = rogue(&srv.addr()).await;
        g.w.send_msg(1, &gen::PairConfirm { mac: [7; 32] })
            .await
            .unwrap();
        assert!(g.refused().await);
        drop(g);
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    for body in [
        vec![0u8; 3],
        gen::PairPakeClient { y: [0; 32] }.to_bytes().unwrap(),
    ] {
        let mut g = rogue(&srv.addr()).await;
        g.w.send(0x0d, 1, &body).await.unwrap();
        drop(g);
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    assert_eq!(srv.pair_guesses(), 0);
    assert!(srv.pairing_open());
    let mut g = rogue(&srv.addr()).await;
    let n = srv.pair_requests().0;
    let code = shown_code(&srv, n).await;
    assert!(g.pake(code).await.1, "the real user still pairs");
}

#[tokio::test(flavor = "multi_thread")]
async fn one_address_cannot_start_more_than_its_share_of_pairing_sessions_on_the_c_server() {
    let d = dir("pk-rate");
    let srv = CServer::start_with(SECRET, &d.join("peers"), opts(60));
    let mut ok = 0;
    for _ in 0..8 {
        match stranger(&srv).await {
            Ok(s) => {
                ok += 1;
                s.close().await;
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            Err(e) => assert!(
                matches!(&e, Ava1Error::Refused { code, .. } if *code == gen::ERR_BUSY),
                "{e:?}"
            ),
        }
    }
    assert_eq!(ok, 6);
    assert!(srv.pairing_open() && srv.pair_guesses() == 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_console_shows_the_users_code_right_after_a_strangers() {
    let d = dir("pk-shown");
    let srv = CServer::start_with(SECRET, &d.join("peers"), opts(60));
    let _stranger = rogue(&srv.addr()).await;
    let first = shown_code(&srv, 1).await;
    // The user's own attempt, moments later from the same address: its code is shown too.
    let mut mine = rogue(&srv.addr()).await;
    let second = shown_code(&srv, 2).await;
    assert_ne!(first, second, "a new session, a new code");
    assert!(mine.pake(second).await.1);
    // An identical repeat (same address, same key) is not shown again.
    let d2 = dir("pk-shown2");
    drop(srv);
    let srv = CServer::start_with(SECRET, &d2.join("peers"), opts(60));
    let me = Arc::new(Identity::generate().unwrap());
    for _ in 0..3 {
        let _ = connect(
            &srv.addr(),
            me.clone(),
            Arc::new(Mutex::new(PeerStore::in_memory())),
            "phone",
            fast(),
        )
        .await
        .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(srv.pair_requests().0, 1);
}

/// The pairing budgets with several addresses at once (the loopback tests only have one).
#[test]
fn the_c_pairing_budget_is_per_address_under_a_global_cap() {
    let mut buf = vec![0u8; unsafe { ffi::ava1_test_sizeof_pairlimit() }];
    let p = buf.as_mut_ptr();
    unsafe {
        ffi::ava1_pl_init(p, 0, 0, 0, 0); // the defaults: 5, 20, 6 per 10 s
        let mut n = 0u32;
        for _ in 0..5 {
            assert_eq!(ffi::ava1_pl_guess_allowed(p, 1, 0), 1);
            assert_eq!(ffi::ava1_pl_guess_failed(p, 1, 0, &mut n), 0);
        }
        assert_eq!(n, 5);
        assert_eq!(
            ffi::ava1_pl_guess_allowed(p, 1, 0),
            0,
            "its own budget is spent"
        );
        assert_eq!(
            ffi::ava1_pl_guess_allowed(p, 2, 0),
            1,
            "another address is untouched"
        );
        // The global cap: 20 in all, so three more addresses (15) and one more (5).
        let mut closed = 0;
        for ip in 2..=4u32 {
            for _ in 0..5 {
                closed = ffi::ava1_pl_guess_failed(p, ip, 0, &mut n);
            }
        }
        assert_eq!(closed, 1, "20 guesses from 4 addresses spend the cap");
        assert_eq!(
            ffi::ava1_pl_guess_allowed(p, 99, 0),
            0,
            "nobody guesses after the cap"
        );
        ffi::ava1_pl_reset(p);
        assert_eq!(
            ffi::ava1_pl_guess_allowed(p, 1, 0),
            1,
            "a new window starts over"
        );
        // New sessions: 6 per address per 10 s.
        for _ in 0..6 {
            assert_eq!(ffi::ava1_pl_welcome_allowed(p, 7, 1000), 1);
        }
        assert_eq!(ffi::ava1_pl_welcome_allowed(p, 7, 1000), 0);
        assert_eq!(ffi::ava1_pl_welcome_allowed(p, 8, 1000), 1, "its own share");
        assert_eq!(
            ffi::ava1_pl_welcome_allowed(p, 7, 11_000),
            1,
            "and it refills"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_c_session_gets_one_attempt() {
    let d = dir("pk-once");
    let srv = CServer::start(SECRET, &d.join("peers"), 60, 100, 500, 500);
    let mut g = rogue(&srv.addr()).await;
    let code = shown_code(&srv, 1).await;
    assert!(!g.pake(wrong(code)).await.1);
    assert!(!g.pake(code).await.1, "the session ended with the refusal");
    assert!(!stored(&d, &g.key));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relayed_exchange_fails_on_the_c_consoles_handshake() {
    // client <-> MITM <-> console: two handshakes, two h. What the app sent is bound to
    // its leg's h, so replaying it to the console cannot verify even with the right code.
    let d = dir("pk-relay");
    let srv = CServer::start(SECRET, &d.join("peers"), 60, 100, 500, 500);
    let mut leg1 = rogue(&srv.addr()).await;
    let code = shown_code(&srv, 1).await;
    let (sent, accepted) = leg1.pake(code).await;
    assert!(accepted);
    let (ya, mac) = sent.unwrap();
    srv.open_pairing(60);
    let mut leg2 = rogue(&srv.addr()).await;
    assert_ne!(leg1.h, leg2.h);
    leg2.w
        .send_msg(1, &gen::PairPakeClient { y: ya })
        .await
        .unwrap();
    assert!(leg2.next(gen::PairPakeServer::TYPE).await.is_some());
    leg2.w.send_msg(2, &gen::PairConfirm { mac }).await.unwrap();
    assert!(leg2.refused().await);
    assert!(!stored(&d, &leg2.key));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_session_api_pairs_with_the_c_server_only_with_the_shown_code() {
    let d = dir("pk-api");
    let srv = CServer::start(SECRET, &d.join("peers"), 60, 100, 500, 500);
    let mut s = stranger(&srv).await.unwrap();
    let code = shown_code(&srv, 1).await;
    let r = s.confirm_pairing(wrong(code)).await;
    assert!(
        matches!(&r, Err(Ava1Error::Refused { code, .. }) if *code == gen::ERR_PAIRING_CODE),
        "{r:?}"
    );
    assert!(srv.pairing_open());
    let mut s = stranger(&srv).await.unwrap();
    let code = shown_code(&srv, 2).await;
    s.confirm_pairing(code).await.unwrap();
    s.node_info().await.unwrap();
}
