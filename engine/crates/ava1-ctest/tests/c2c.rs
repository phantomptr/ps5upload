#![cfg(unix)]
//! Console to console (SPEC.md §18): the receiving console hands out a ticket
//! (c2c.allow), the sending console dials it with that ticket and pushes the job
//! (c2c.send). Console A is the C server in this process; console B is another C server
//! in a child process (`c2c_peer`), since the C server is a process-wide singleton. The
//! test itself plays the engine, paired with both.
mod common;

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ava1::gen::{self, C2cAllow, C2cSend, C2cTicket, JobRef, Status};
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::session::{connect, Session};
use ava1::wire::Message;
use ava1_ctest::CServer;
use common::*;

const B_SECRET: [u8; 32] = [0x43; 32];

/// Console B: a C server in its own process, killed when dropped.
struct Peer {
    child: Child,
    port: u16,
}

impl Peer {
    fn start(peers: &Path, jobs: &Path, fsync_delay_us: u32) -> Peer {
        let mut child = Command::new(env!("CARGO_BIN_EXE_c2c_peer"))
            .arg(B_SECRET[0].to_string())
            .arg(peers)
            .arg(jobs)
            .arg(fsync_delay_us.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port = line.trim().strip_prefix("PORT ").unwrap().parse().unwrap();
        Peer { child, port }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The engine: one identity, paired with both consoles.
fn engine(a_peers: &Path, b_peers: &Path) -> (Arc<Identity>, Arc<Mutex<PeerStore>>) {
    let me = Arc::new(Identity::generate().unwrap());
    for p in [a_peers, b_peers] {
        PeerStore::load(p)
            .unwrap()
            .add(me.public(), "engine")
            .unwrap();
    }
    let mut mine = PeerStore::in_memory();
    mine.add(Identity::from_secret(SECRET).public(), "A")
        .unwrap();
    mine.add(Identity::from_secret(B_SECRET).public(), "B")
        .unwrap();
    (me, Arc::new(Mutex::new(mine)))
}

async fn allow(b: &Session, job: [u8; 16], root: &Path) -> [u8; 16] {
    let body = C2cAllow {
        job_id: job,
        key: Identity::from_secret(SECRET).public(),
        root: root.to_str().unwrap().into(),
    };
    let r = b
        .rpc(gen::METHOD_C2C_ALLOW, &body.to_bytes().unwrap())
        .await
        .unwrap();
    assert_eq!(r.status, gen::STATUS_OK);
    C2cTicket::decode(&r.body).unwrap().token
}

/// c2c.send on A: (status, the error text when refused).
async fn send(
    a: &Session,
    job: [u8; 16],
    port: u16,
    key: [u8; 32],
    token: [u8; 16],
    src: &Path,
    dest: &Path,
) -> (u16, String) {
    let body = C2cSend {
        job_id: job,
        host: "127.0.0.1".into(),
        port,
        key,
        token,
        src: src.to_str().unwrap().into(),
        dest: dest.to_str().unwrap().into(),
        flags: 0,
    };
    let r = a
        .rpc(gen::METHOD_C2C_SEND, &body.to_bytes().unwrap())
        .await
        .unwrap();
    let text = if r.status == gen::STATUS_OK {
        String::new()
    } else {
        String::from_utf8_lossy(&r.body).into_owned()
    };
    (r.status, text)
}

async fn status(s: &Session, job: [u8; 16]) -> Option<Status> {
    let r = s
        .rpc(
            gen::METHOD_JOB_STATUS,
            &JobRef { job_id: job }.to_bytes().unwrap(),
        )
        .await
        .unwrap();
    (r.status == gen::STATUS_OK).then(|| Status::decode(&r.body).unwrap())
}

async fn wait_end(s: &Session, job: [u8; 16]) -> Status {
    for _ in 0..1200 {
        if let Some(st) = status(s, job).await {
            if st.state.unwrap_or(0) != 0 {
                return st;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the send did not end");
}

struct Pair {
    _srv: CServer,
    peer: Peer,
    a: Session,
    b: Session,
}

async fn pair(d: &Path, b_fsync_delay_us: u32) -> Pair {
    ava1_ctest::c_set_read_allowed(true);
    let (me, mine) = engine(&d.join("a-peers"), &d.join("b-peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("a-peers"),
        &d.join("a-jobs"),
        200,
        2000,
        2000,
        0,
    );
    let peer = Peer::start(&d.join("b-peers"), &d.join("b-jobs"), b_fsync_delay_us);
    let a = connect(&srv.addr(), me.clone(), mine.clone(), "engine", calm())
        .await
        .unwrap();
    let b = connect(
        &format!("127.0.0.1:{}", peer.port),
        me,
        mine,
        "engine",
        calm(),
    )
    .await
    .unwrap();
    Pair {
        _srv: srv,
        peer,
        a,
        b,
    }
}

fn b_key() -> [u8; 32] {
    Identity::from_secret(B_SECRET).public()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_goes_straight_from_one_console_to_the_other() {
    let d = dir("c2c-folder");
    let src = d.join("a/game");
    write_tree(&src, 300, |i| {
        if i % 50 == 0 {
            (3 << 20) + i
        } else {
            1000 + i
        }
    });
    let dest = d.join("b/game");
    let p = pair(&d, 0).await;
    let job = [0xC1; 16];
    let token = allow(&p.b, job, &dest).await;
    let (st, why) = send(&p.a, job, p.peer.port, b_key(), token, &src, &dest).await;
    assert_eq!(st, gen::STATUS_OK, "{why}");
    let end = wait_end(&p.a, job).await;
    assert_eq!(end.state, Some(1), "{:?}", end.current);
    assert!(same_tree(&src, &dest));
    // The job on B is the engine's own: it can read it (and resume it through itself).
    let on_b = wait_end(&p.b, job).await;
    assert_eq!(on_b.state, Some(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_single_file_goes_straight_across() {
    let d = dir("c2c-file");
    std::fs::create_dir_all(d.join("a")).unwrap();
    let src = d.join("a/app.ffpfsc");
    std::fs::write(&src, vec![7u8; (5 << 20) + 3]).unwrap();
    std::fs::create_dir_all(d.join("b")).unwrap();
    let dest = d.join("b/app.ffpfsc");
    let p = pair(&d, 0).await;
    let job = [0xC2; 16];
    let token = allow(&p.b, job, &dest).await;
    let (st, why) = send(&p.a, job, p.peer.port, b_key(), token, &src, &dest).await;
    assert_eq!(st, gen::STATUS_OK, "{why}");
    assert_eq!(wait_end(&p.a, job).await.state, Some(1));
    assert_eq!(std::fs::read(&src).unwrap(), std::fs::read(&dest).unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_token_or_key_is_refused_before_anything_is_sent() {
    let d = dir("c2c-refused");
    let src = d.join("a/game");
    write_tree(&src, 3, |_| 100);
    let dest = d.join("b/game");
    let p = pair(&d, 0).await;
    let job = [0xC3; 16];
    let token = allow(&p.b, job, &dest).await;
    let (st, why) = send(&p.a, job, p.peer.port, b_key(), [9; 16], &src, &dest).await;
    assert_eq!(st, gen::ERR_IO);
    assert!(why.contains("refused"), "{why}");
    let (st, why) = send(&p.a, job, p.peer.port, [5; 32], token, &src, &dest).await;
    assert_eq!(st, gen::ERR_IO);
    assert!(why.contains("not the one expected"), "{why}");
    assert!(!dest.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ticket_covers_its_one_destination_only() {
    let d = dir("c2c-root");
    let src = d.join("a/game");
    write_tree(&src, 3, |_| 100);
    let p = pair(&d, 0).await;
    let job = [0xC4; 16];
    let token = allow(&p.b, job, &d.join("b/allowed")).await;
    let elsewhere = d.join("b/elsewhere");
    let (st, why) = send(&p.a, job, p.peer.port, b_key(), token, &src, &elsewhere).await;
    assert_eq!(st, gen::STATUS_OK, "{why}");
    let end = wait_end(&p.a, job).await;
    assert_eq!(end.state, Some(2));
    assert!(!elsewhere.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn nobody_listening_fails_at_once_with_the_reason() {
    let d = dir("c2c-unreachable");
    let src = d.join("a/game");
    write_tree(&src, 1, |_| 10);
    let p = pair(&d, 0).await;
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let (st, why) = send(
        &p.a,
        [0xC5; 16],
        port,
        b_key(),
        [1; 16],
        &src,
        &d.join("b/x"),
    )
    .await;
    assert_eq!(st, gen::ERR_IO);
    assert!(why.contains("cannot reach"), "{why}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_send_resumes_with_the_same_job() {
    let d = dir("c2c-resume");
    let src = d.join("a/game");
    write_tree(&src, 400, |i| 2000 + i);
    let dest = d.join("b/game");
    // B syncs slowly, so the cancel lands mid-way.
    let p = pair(&d, 3000).await;
    let job = [0xC6; 16];
    let token = allow(&p.b, job, &dest).await;
    let (st, why) = send(&p.a, job, p.peer.port, b_key(), token, &src, &dest).await;
    assert_eq!(st, gen::STATUS_OK, "{why}");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let r =
        p.a.rpc(
            gen::METHOD_JOB_CANCEL,
            &JobRef { job_id: job }.to_bytes().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status, gen::STATUS_OK);
    tokio::time::sleep(Duration::from_millis(600)).await;
    // Again, the same job: B picks up what it already holds.
    let token = allow(&p.b, job, &dest).await;
    let (st, why) = send(&p.a, job, p.peer.port, b_key(), token, &src, &dest).await;
    assert_eq!(st, gen::STATUS_OK, "{why}");
    let end = wait_end(&p.a, job).await;
    assert_eq!(end.state, Some(1), "{:?}", end.current);
    assert!(same_tree(&src, &dest));
}

/// The engine's pool directories for A and B, paired with each console (before the consoles
/// start: they read their peers file once).
fn pair_engine(d: &Path) {
    for (tag, console_peers, key) in [
        (
            "ava-a",
            d.join("a-peers"),
            Identity::from_secret(SECRET).public(),
        ),
        ("ava-b", d.join("b-peers"), b_key()),
    ] {
        let ava = d.join(tag);
        std::fs::create_dir_all(&ava).unwrap();
        let me = Identity::load_or_create(&ava.join("identity")).unwrap();
        PeerStore::load(&console_peers)
            .unwrap()
            .add(me.public(), "engine")
            .unwrap();
        PeerStore::load(&ava.join("peers"))
            .unwrap()
            .add(key, "console")
            .unwrap();
    }
}

/// Engine pools for A and B, as the app's engine holds them.
fn pools(d: &Path, b_port: u16, a_addr: &str) -> (ps5upload_ava1::Pool, ps5upload_ava1::Pool) {
    (
        ps5upload_ava1::Pool::new(d.join("ava-a")).with_addr(a_addr),
        ps5upload_ava1::Pool::new(d.join("ava-b")).with_addr(format!("127.0.0.1:{b_port}")),
    )
}

fn routed(
    d: &Path,
    pa: &ps5upload_ava1::Pool,
    pb: &ps5upload_ava1::Pool,
    src: &Path,
    dest: &Path,
    job: [u8; 16],
) -> (ps5upload_ava1::c2c::Route, Vec<ps5upload_ava1::c2c::Route>) {
    let _ = d;
    let seen = Mutex::new(Vec::new());
    let (_, route) = ps5upload_ava1::c2c::ps5_to_ps5_routed_between(
        pa,
        "192.0.2.1",
        src.to_str().unwrap(),
        pb,
        "192.0.2.2",
        dest.to_str().unwrap(),
        job,
        Arc::default(),
        Arc::default(),
        |r| seen.lock().unwrap().push(r.clone()),
    )
    .unwrap();
    (route, seen.into_inner().unwrap())
}

#[test]
fn the_engine_sends_directly_when_the_consoles_reach_each_other() {
    ava1_ctest::c_set_read_allowed(true);
    let d = dir("c2c-engine-direct");
    let src = d.join("a/game");
    write_tree(&src, 120, |i| if i == 0 { 9 << 20 } else { 500 + i });
    let dest = d.join("b/game");
    pair_engine(&d);
    let srv = CServer::start_data(
        SECRET,
        &d.join("a-peers"),
        &d.join("a-jobs"),
        200,
        2000,
        2000,
        0,
    );
    let peer = Peer::start(&d.join("b-peers"), &d.join("b-jobs"), 0);
    let (pa, pb) = pools(&d, peer.port, &srv.addr());
    let (route, seen) = routed(&d, &pa, &pb, &src, &dest, [0xD1; 16]);
    assert_eq!(route, ps5upload_ava1::c2c::Route::Direct);
    assert_eq!(seen, vec![ps5upload_ava1::c2c::Route::Direct]);
    assert!(same_tree(&src, &dest));
}

#[test]
fn the_engine_carries_it_itself_when_the_consoles_cannot_connect_and_says_why() {
    ava1_ctest::c_set_read_allowed(true);
    let d = dir("c2c-engine-fallback");
    let src = d.join("a/game");
    write_tree(&src, 60, |i| 700 + i);
    let dest = d.join("b/game");
    pair_engine(&d);
    let srv = CServer::start_data(
        SECRET,
        &d.join("a-peers"),
        &d.join("a-jobs"),
        200,
        2000,
        2000,
        0,
    );
    let peer = Peer::start(&d.join("b-peers"), &d.join("b-jobs"), 0);
    let (pa, pb) = pools(&d, peer.port, &srv.addr());
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    ps5upload_ava1::c2c::set_dial_port_for_tests(port);
    let (route, _) = routed(&d, &pa, &pb, &src, &dest, [0xD2; 16]);
    ps5upload_ava1::c2c::set_dial_port_for_tests(0);
    match route {
        ps5upload_ava1::c2c::Route::Relay { reason } => {
            assert!(reason.contains("could not connect"), "{reason}")
        }
        r => panic!("{r:?}"),
    }
    assert!(same_tree(&src, &dest));
}

#[tokio::test(flavor = "multi_thread")]
async fn two_sends_to_the_same_console_run_side_by_side() {
    let d = dir("c2c-two");
    let (src1, src2) = (d.join("a/one"), d.join("a/two"));
    write_tree(&src1, 300, |i| 3000 + i);
    write_tree(&src2, 300, |i| 4000 + i);
    let (dest1, dest2) = (d.join("b/one"), d.join("b/two"));
    // B syncs slowly, so the two sends overlap.
    let p = pair(&d, 2000).await;
    let (j1, j2) = ([0xC7; 16], [0xC8; 16]);
    let t1 = allow(&p.b, j1, &dest1).await;
    let t2 = allow(&p.b, j2, &dest2).await;
    let (st, why) = send(&p.a, j1, p.peer.port, b_key(), t1, &src1, &dest1).await;
    assert_eq!(st, gen::STATUS_OK, "{why}");
    let (st, why) = send(&p.a, j2, p.peer.port, b_key(), t2, &src2, &dest2).await;
    assert_eq!(st, gen::STATUS_OK, "{why}");
    let (e1, e2) = (wait_end(&p.a, j1).await, wait_end(&p.a, j2).await);
    assert_eq!(e1.state, Some(1), "{:?}", e1.current);
    assert_eq!(e2.state, Some(1), "{:?}", e2.current);
    assert!(same_tree(&src1, &dest1) && same_tree(&src2, &dest2));
}
