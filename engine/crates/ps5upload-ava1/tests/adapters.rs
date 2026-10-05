//! The upload adapters end to end, against Task 17's `FolderHost` server on
//! 127.0.0.1. Built with public API only (an integration test of this crate cannot
//! see `ava1`'s own `tests/common`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ava1::gen;
use ava1::host::FolderHost;
use ava1::keys::Identity;
use ava1::manifest;
use ava1::peers::PeerStore;
use ava1::server::{self, ServerCtx};
use ava1::session::RpcReply;
use ava1::source::LocalSource;
use ava1::wire::{FrameMessage, Message};
use ava1_chaos::{ChaosConfig, ChaosProxy};
use ps5upload_ava1::console;
use ps5upload_ava1::upload;
use ps5upload_ava1::{block_on, Pool, PostCommitError, PostCommitKind};
use ps5upload_core::transfer::{FileListEntry, TransferConfig};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("p5a-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// Writes `files` files of deterministic content, `size_fn(i)` bytes each, dotted
/// through a few directories. Returns the total bytes written.
fn tree(dir: &Path, files: usize, size_fn: impl Fn(usize) -> usize) -> u64 {
    let mut total = 0u64;
    for i in 0..files {
        let p = dir.join(format!("x{}/f{i}", i % 4));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let n = size_fn(i);
        let mut data = Vec::with_capacity(n);
        let mut b = (i as u8).wrapping_mul(37).wrapping_add(11);
        while data.len() < n {
            data.push(b);
            b = b.wrapping_mul(37).wrapping_add(11);
        }
        std::fs::write(p, &data).unwrap();
        total += n as u64;
    }
    total
}

/// Both trees hold the same files, byte for byte.
fn same_tree(a: &Path, b: &Path) {
    let wa = manifest::walk(&LocalSource::new(a.to_path_buf()), &|_| false).unwrap();
    let wb = manifest::walk(&LocalSource::new(b.to_path_buf()), &|_| false).unwrap();
    let fa: Vec<_> = wa
        .entries
        .iter()
        .filter(|e| e.kind == gen::ENTRY_FILE)
        .collect();
    let fb: Vec<_> = wb
        .entries
        .iter()
        .filter(|e| e.kind == gen::ENTRY_FILE)
        .collect();
    assert_eq!(fa.len(), fb.len(), "file counts differ");
    for (ea, eb) in fa.iter().zip(&fb) {
        assert_eq!(ea.path, eb.path, "file sets differ");
        assert_eq!(ea.size, eb.size, "{}: size differs", ea.path);
        let ba = std::fs::read(a.join(&ea.path)).unwrap();
        let bb = std::fs::read(b.join(&eb.path)).unwrap();
        assert!(ba == bb, "{}: bytes differ", ea.path);
    }
}

/// Every wait in this file is bounded (project rule): a missing signal fails the
/// test, not the round.
async fn within<T>(secs: u64, f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(secs), f)
        .await
        .expect("timed out")
}

/// A folder host on 127.0.0.1. The server knows the engine's key (as a stamped
/// payload would); the pool does not know the server's — which is exactly the
/// `pairing_code().is_some() && !needs_user_pairing()` branch the pool walks (the
/// console already trusts us). Deliberate: it exercises the self-confirm path, and
/// the pool's peers file grows a line for it, so a second `session()` call for the
/// same console reuses the map entry instead of reconnecting.
async fn host(dir: &Path, host_jobs: bool) -> (String, Pool) {
    let ava = dir.join("ava");
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    let mut peers = PeerStore::in_memory();
    peers.add(me.public(), "engine").unwrap();
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "host",
        peers,
        node_info_rpc(),
    );
    let ctx = if host_jobs {
        ctx.with_jobs(Arc::new(FolderHost {
            root: dir.join("share"),
            jobs_dir: dir.join("hjobs"),
        }))
    } else {
        ctx
    };
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    (addr.clone(), Pool::new(ava).with_addr(addr))
}

/// Only METHOD_NODE_INFO gets a real answer; everything else is `ERR_UNKNOWN_METHOD`
/// (the named constant, not a literal — C8).
fn node_info_rpc() -> ava1::server::RpcHandler {
    Box::new(|method, _| {
        if method == gen::METHOD_NODE_INFO {
            let info = gen::NodeInfo {
                version: "test".into(),
                platform: "rust".into(),
                name: "host".into(),
                firmware: None,
            };
            RpcReply {
                status: gen::STATUS_OK,
                body: info.to_bytes().unwrap(),
            }
        } else {
            RpcReply {
                status: gen::ERR_UNKNOWN_METHOD,
                body: Vec::new(),
            }
        }
    })
}

fn cfg() -> TransferConfig {
    let mut c = TransferConfig::new("127.0.0.1");
    c.progress_bytes = Some(Arc::new(AtomicU64::new(0)));
    c.progress_files = Some(Arc::new(AtomicU64::new(0)));
    c.progress_files_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.progress_bytes_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.cancel = Some(Arc::new(AtomicBool::new(false)));
    c
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_dir_lands_and_reports_progress() {
    let d = temp_dir("dir");
    std::fs::create_dir_all(&d).unwrap();
    let src = d.join("src");
    let total = tree(&src, 200, |i| i * 100);
    let (_addr, pool) = host(&d, true).await;
    let c = cfg();
    let (bytes, files_done) = (
        c.progress_bytes.clone().unwrap(),
        c.progress_files_finalized.clone().unwrap(),
    );
    let src2 = src.clone();
    let r = within(
        60,
        tokio::task::spawn_blocking(move || upload::upload_dir_in(&pool, &c, [1; 16], "in", &src2)),
    )
    .await
    .unwrap()
    .unwrap();
    let ack: serde_json::Value = serde_json::from_str(&r.commit_ack_body).unwrap();
    assert_eq!(ack["protocol"], "ava1");
    assert_eq!(ack["files"], 200);
    same_tree(&src, &d.join("share/in"));
    assert_eq!(
        bytes.load(Ordering::Relaxed),
        total,
        "progress_bytes reached the tree's total (the bridge ran, A3)"
    );
    assert_eq!(
        files_done.load(Ordering::Relaxed),
        200,
        "progress_files_finalized reached 200 (C12: durable files)"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_list_maps_relative_destinations_under_the_root() {
    let d = temp_dir("list");
    std::fs::create_dir_all(d.join("a")).unwrap();
    std::fs::write(d.join("a/1"), b"one").unwrap();
    std::fs::write(d.join("a/2"), b"two").unwrap();
    let (_addr, pool) = host(&d, true).await;
    let entries = vec![
        FileListEntry {
            src: d.join("a/1").to_string_lossy().into_owned(),
            dest: "x/1".into(),
        },
        FileListEntry {
            src: d.join("a/2").to_string_lossy().into_owned(),
            dest: "y/2".into(),
        },
    ];
    let c = cfg();
    let r = within(
        60,
        tokio::task::spawn_blocking(move || {
            upload::upload_list_in(&pool, &c, [2; 16], "list", &entries)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(r.files_sent, 2);
    assert_eq!(std::fs::read(d.join("share/list/x/1")).unwrap(), b"one");
    assert_eq!(std::fs::read(d.join("share/list/y/2")).unwrap(), b"two");
    // The rest of the tree is untouched: only the two mapped files exist.
    let files: Vec<String> = manifest::walk(&LocalSource::new(d.join("share").clone()), &|_| false)
        .unwrap()
        .entries
        .into_iter()
        .filter(|e| e.kind == gen::ENTRY_FILE)
        .map(|e| e.path)
        .collect();
    assert_eq!(files, vec!["list/x/1".to_string(), "list/y/2".to_string()]);

    // A destination that escapes (`..`) fails locally: anyhow, no console round trip (a fresh
    // pool never connects). An absolute destination elsewhere is NOT an error: it is a job of
    // its own (see `ps5upload-tests/tests/ava1_transfer_integration.rs`).
    let bad = vec![FileListEntry {
        src: d.join("a/1").to_string_lossy().into_owned(),
        dest: "../elsewhere/3".into(),
    }];
    let c2 = cfg();
    let p2 = Pool::new(d.join("ava")).with_addr("127.0.0.1:1");
    let err = upload::upload_list_in(&p2, &c2, [3; 16], "list", &bad).unwrap_err();
    assert!(!format!("{err:#}").is_empty());
    assert_eq!(p2.attempts(), 0, "the refusal is local: nothing connected");
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_file_writes_the_destination_path_itself() {
    let d = temp_dir("single");
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::write(d.join("src/single.bin"), vec![0x5a; 500_000]).unwrap();
    let (_addr, pool) = host(&d, true).await;
    let c = cfg();
    let src = d.join("src/single.bin");
    let r = within(
        60,
        tokio::task::spawn_blocking(move || {
            upload::upload_file_in(&pool, &c, [4; 16], "single.bin", &src)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    // C11: `dest` is the full destination path — the file lands AT the path, not in a
    // directory named after it.
    let landed = d.join("share/single.bin");
    assert!(landed.is_file(), "the destination path itself is the file");
    assert!(
        !landed.join("single.bin").exists(),
        "the destination was treated as a directory"
    );
    assert_eq!(std::fs::read(&landed).unwrap(), vec![0x5a; 500_000]);
    assert_eq!(r.files_sent, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn post_commit_failure_is_not_a_resend() {
    let d = temp_dir("post");
    let src = d.join("src");
    let total = tree(&src, 200, |_| 128 * 1024);
    let (_addr, pool) = host(&d, true).await;
    let c = cfg();
    let sent = c.progress_bytes.clone().unwrap();
    let share = d.join("share/out");
    // Create the destination (non-empty, so even a race with the receiver's
    // exists-check cannot silently rename over it) once data is flowing: the receiver
    // staged the landing (share/out did not exist at open), so the final move
    // refuses with ERR_EXISTS — a post-commit failure, not a transport one.
    let progress = c.progress_bytes.clone().unwrap();
    let maker = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while progress.load(Ordering::Relaxed) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the transfer never started"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::create_dir_all(&share).unwrap();
        std::fs::write(share.join("taken"), b"x").unwrap();
    });
    let e = within(
        60,
        tokio::task::spawn_blocking(move || upload::upload_dir_in(&pool, &c, [5; 16], "out", &src)),
    )
    .await
    .unwrap()
    .unwrap_err();
    maker.join().unwrap();
    let pe = e
        .downcast_ref::<PostCommitError>()
        .expect("the refusal is a PostCommitError");
    assert_eq!(pe.kind, PostCommitKind::Exists);
    assert_eq!(pe.kind.as_str(), "ava1_commit_exists");
    assert!(
        !ps5upload_core::transfer::is_retryable_transfer_error(&e),
        "a post-commit failure must not be retried"
    );
    // No byte went twice: the loop did not resume (a post-commit failure is final),
    // so Received bytes never exceed the source by more than one bundle (64 KiB).
    assert!(
        sent.load(Ordering::Relaxed) <= total + 64 * 1024,
        "{} > {}: bytes were sent more than once",
        sent.load(Ordering::Relaxed),
        total + 64 * 1024
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_console_that_wants_a_user_code_is_not_paired() {
    let d = temp_dir("pair");
    std::fs::create_dir_all(&d).unwrap();
    // A second server whose peer store does NOT know the engine's key and whose
    // pairing window is closed (the default: it only opens after open_pairing).
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "stranger",
        PeerStore::in_memory(),
        node_info_rpc(),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    let pool = Pool::new(d.join("ava")).with_addr(addr.clone());
    assert!(
        pool.session(&addr).await.is_err(),
        "the console refused the stranger"
    );
    // The readiness check says what the person has to do, and a refused pairing is not a
    // transfer's to settle: `not_paired`, the pairing dialog's trigger.
    let (pool, addr) = (Arc::new(pool), addr.clone());
    let failure = tokio::task::spawn_blocking(move || {
        console::require_in(&pool, &addr, ava1::gen::CAP_DATA_PLANE).unwrap_err()
    })
    .await
    .unwrap();
    assert_eq!(failure.reason, "not_paired");
    assert_eq!(
        failure.detail,
        "This PS5 has not accepted this app yet. Pair it from the Connection screen."
    );
}

/// A client polls many endpoints: ten rapid calls to an unpaired console must make ONE handshake
/// (each one can show a pairing code on the console and counts against its per-IP connection cap),
/// not ten. With the memory switched off the same ten calls dial ten times.
#[tokio::test(flavor = "multi_thread")]
async fn rapid_calls_to_an_unpaired_console_make_one_handshake() {
    let d = temp_dir("pair-cache");
    std::fs::create_dir_all(&d).unwrap();
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "stranger",
        PeerStore::in_memory(),
        node_info_rpc(),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    let cached = Arc::new(Pool::new(d.join("ava")).with_addr(addr.clone()));
    let uncached = Arc::new(
        Pool::new(d.join("ava2"))
            .with_addr(addr.clone())
            .with_refusal_ttl(Duration::ZERO),
    );
    let (c2, u2, a2) = (cached.clone(), uncached.clone(), addr.clone());
    tokio::task::spawn_blocking(move || {
        for _ in 0..10 {
            let f = console::require_in(&c2, &a2, ava1::gen::CAP_DATA_PLANE).unwrap_err();
            assert_eq!(f.reason, "not_paired");
            console::require_in(&u2, &a2, ava1::gen::CAP_DATA_PLANE).unwrap_err();
        }
    })
    .await
    .unwrap();
    assert_eq!(cached.attempts(), 1, "ten calls, one handshake");
    assert_eq!(
        uncached.attempts(),
        10,
        "without the memory every call dials"
    );
}

/// An unreachable console is remembered too, so a poll does not block on it every call, and a
/// pairing (or any session that works) forgets the answer at once.
#[test]
fn an_unreachable_console_is_remembered_until_cleared() {
    let d = temp_dir("down-cache");
    let p = Pool::new(d.join("ava")).with_addr("127.0.0.1:1");
    for _ in 0..10 {
        let f = console::require_in(&p, "c", ava1::gen::CAP_DATA_PLANE).unwrap_err();
        assert_eq!(f.reason, "helper_not_ava1");
    }
    assert_eq!(p.attempts(), 1);
    p.clear_refusal("c");
    console::require_in(&p, "c", ava1::gen::CAP_DATA_PLANE).unwrap_err();
    assert_eq!(p.attempts(), 2, "cleared: the next call tries again");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_session_resumes_the_same_job() {
    let d = temp_dir("chaos");
    let src = d.join("src");
    let total = tree(&src, 16, |_| 1 << 20); // 16 MiB: long enough to kill mid-transfer
    let (addr, pool) = host(&d, true).await;
    // One deterministic kill instead of the brief's periodic killer (measured: a
    // periodic kill plus the backoff ladder lost every cycle to the 5 s wait, and a
    // tree small enough to finish between kills flaked). The cap keeps the transfer
    // slow enough that the 200 ms progress tick observes durable bytes mid-transfer;
    // the killer then drops every connection once, so the resume loop must reconnect
    // with the same job id and the console's journal must resume a partially-durable
    // job.
    let proxy = ChaosProxy::start(
        addr.parse().unwrap(),
        ChaosConfig {
            bytes_per_sec: Some(2 << 20),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let proxy = Arc::new(proxy);
    let pool = Arc::new(pool.with_addr(proxy.addr.to_string()));
    let c = cfg();
    let progress = c.progress_bytes.clone().unwrap();
    let finalized = c.progress_bytes_finalized.clone().unwrap();
    let proxy2 = proxy.clone();
    let finalized2 = finalized.clone();
    let killer = std::thread::spawn(move || {
        // Wait for durable progress (and not completion): the kill lands on a job
        // whose journal already holds bytes.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while finalized2.load(Ordering::Relaxed) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the transfer never made anything durable"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            progress.load(Ordering::Relaxed) > 0,
            "no bytes were acked either"
        );
        proxy2.kill_all();
    });
    let (pool2, src2) = (pool.clone(), src.clone());
    let r = within(
        60,
        tokio::task::spawn_blocking(move || {
            upload::upload_dir_in(&pool2, &c, [6; 16], "in", &src2)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    killer.join().unwrap();
    same_tree(&src, &d.join("share/in"));
    assert_eq!(
        finalized.load(Ordering::Relaxed),
        total,
        "the whole tree is durable after the resume"
    );
    let attempts = pool.attempts();
    let ack: serde_json::Value = serde_json::from_str(&r.commit_ack_body).unwrap();
    let resent = ack["resent"].as_u64().unwrap_or(0);
    // The observed numbers, for the flake record (--nocapture).
    println!("killed-session resume: attempts = {attempts}, resent = {resent}");
    // A whole-session death never sets `resent`: the new session sends the missing
    // ranges as fresh frames (resent counts in-session requeues, not cross-session
    // ones), so the reconnect — attempts >= 2 with durable bytes surviving in the
    // log — is the evidence the brief asks for.
    assert!(
        attempts >= 2,
        "the session was killed but the pool never reconnected (attempts = {attempts})"
    );
    assert!(
        resent > 0 || attempts >= 2,
        "a reconnect is otherwise evidenced (resent = {resent}, attempts = {attempts})"
    );
    drop(proxy);
}

#[test]
fn block_on_works_outside_any_runtime() {
    // The lab's CLI path: a plain thread with no runtime — the private fallback
    // runtime runs the future.
    let out = block_on(async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        41
    });
    assert_eq!(out, 41);
}

#[tokio::test(flavor = "multi_thread")]
async fn block_on_works_from_a_blocking_thread_and_a_multithread_worker() {
    // From a spawn_blocking thread (the engine's pattern, C15): the handle's block_on.
    let out = within(
        30,
        tokio::task::spawn_blocking(|| {
            block_on(async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                42
            })
        }),
    )
    .await
    .expect("block_on on a blocking thread");
    assert_eq!(out, 42);
    // From a multi-thread worker in sync context (block_in_place).
    let out = tokio::task::block_in_place(|| block_on(async { 43 }));
    assert_eq!(out, 43);
}

async fn failure_of(pool: Pool, src: PathBuf) -> (String, Duration) {
    let started = std::time::Instant::now();
    let e = within(
        60,
        tokio::task::spawn_blocking(move || {
            upload::upload_dir_in(&pool, &cfg(), [21; 16], "out", &src)
        }),
    )
    .await
    .unwrap()
    .unwrap_err();
    let f = e
        .downcast_ref::<upload::UploadFailure>()
        .unwrap_or_else(|| panic!("not an UploadFailure: {e:#}"));
    (f.reason.clone(), started.elapsed())
}

#[tokio::test(flavor = "multi_thread")]
async fn three_refused_connections_end_an_upload_as_unreachable() {
    let d = temp_dir("refused");
    let src = d.join("src");
    tree(&src, 2, |_| 1000);
    let pool = Pool::new(d.join("ava")).with_addr("127.0.0.1:1");
    let (reason, took) = failure_of(pool, src).await;
    assert_eq!(reason, "ava1_unreachable");
    assert!(took < Duration::from_secs(20), "{took:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn no_identity_ends_an_upload_without_a_connection_attempt() {
    let d = temp_dir("noid");
    let src = d.join("src");
    tree(&src, 2, |_| 1000);
    std::fs::create_dir_all(d.join("ava/identity")).unwrap();
    let pool = Pool::new(d.join("ava")).with_addr("127.0.0.1:1");
    assert!(!pool.has_identity());
    let (reason, took) = failure_of(pool, src).await;
    assert_eq!(reason, "ava1_no_identity");
    assert!(took < Duration::from_secs(3), "{took:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_console_that_wants_a_user_code_ends_an_upload_as_not_paired() {
    let d = temp_dir("notpaired");
    let src = d.join("src");
    tree(&src, 2, |_| 1000);
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "stranger",
        PeerStore::in_memory(),
        node_info_rpc(),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    let pool = Pool::new(d.join("ava")).with_addr(addr);
    let (reason, took) = failure_of(pool, src).await;
    assert_eq!(reason, "ava1_not_paired");
    assert!(took < Duration::from_secs(20), "{took:?}");
}

/// A host that answers the first `busy` JobOpens `ERR_BUSY` (a console whose recovery pass holds the job id)
/// and serves the rest like `FolderHost`.
struct BusyHost {
    inner: FolderHost,
    busy: std::sync::atomic::AtomicU32,
    seen: std::sync::atomic::AtomicU32,
}

impl ava1::router::JobHost for BusyHost {
    fn accept(&self, link: ava1::router::JobLink, first: ava1::conn::Frame, peer: [u8; 32]) {
        use std::sync::atomic::Ordering::SeqCst;
        if first.ty == gen::JobOpen::TYPE {
            self.seen.fetch_add(1, SeqCst);
            let left = self.busy.load(SeqCst);
            if left > 0 {
                self.busy.store(left - 1, SeqCst);
                if let Ok(open) = first.decode::<gen::JobOpen>() {
                    tokio::spawn(async move {
                        let _ = link
                            .control
                            .send(&gen::JobOpenAck {
                                job_id: open.job_id,
                                status: gen::ERR_BUSY,
                                credit: 0,
                                staged: 0,
                                workers: 0,
                                message: Some(
                                    "the console is finishing this job's files; try again".into(),
                                ),
                            })
                            .await;
                    });
                    return;
                }
            }
        }
        self.inner.accept(link, first, peer)
    }
}

async fn busy_host(dir: &Path, busy: u32) -> (Arc<BusyHost>, Pool) {
    let ava = dir.join("ava");
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    let mut peers = PeerStore::in_memory();
    peers.add(me.public(), "engine").unwrap();
    let h = Arc::new(BusyHost {
        inner: FolderHost {
            root: dir.join("share"),
            jobs_dir: dir.join("hjobs"),
        },
        busy: busy.into(),
        seen: 0.into(),
    });
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "host",
        peers,
        node_info_rpc(),
    )
    .with_jobs(h.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    (h, Pool::new(ava).with_addr(addr))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_job_open_is_retried_until_the_console_accepts() {
    // the console answers BUSY twice (recovery holds the job), then OK: the upload completes
    let d = temp_dir("busy-then-ok");
    let src = d.join("src");
    tree(&src, 40, |_| 4096);
    let (h, pool) = busy_host(&d, 2).await;
    let c = cfg();
    let r = within(
        60,
        tokio::task::spawn_blocking(move || {
            upload::upload_dir_in(&pool, &c, [0x21; 16], "out", &src)
        }),
    )
    .await
    .unwrap();
    r.expect("the upload completes after the BUSY answers");
    assert_eq!(
        h.seen.load(Ordering::SeqCst),
        3,
        "two BUSY answers, then the real open"
    );
    same_tree(&d.join("src"), &d.join("share/out"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_console_that_stays_busy_fails_the_upload_after_the_bound_with_a_clear_reason() {
    let d = temp_dir("busy-forever");
    let src = d.join("src");
    tree(&src, 4, |_| 1024);
    let (h, pool) = busy_host(&d, u32::MAX).await;
    let pool = pool.with_busy_tries(3);
    let c = cfg();
    let e = within(
        60,
        tokio::task::spawn_blocking(move || {
            upload::upload_dir_in(&pool, &c, [0x22; 16], "out", &src)
        }),
    )
    .await
    .unwrap()
    .unwrap_err();
    let f = e
        .downcast_ref::<upload::UploadFailure>()
        .unwrap_or_else(|| panic!("not a classified failure: {e:#}"));
    assert_eq!(f.reason, "ava1_busy", "{f:?}");
    assert!(f.detail.contains("busy"), "{}", f.detail);
    assert_eq!(
        h.seen.load(Ordering::SeqCst),
        4,
        "the first try and three retries, then it gave up"
    );
}

/// A host that never answers the first `mute` JobOpens (the open is swallowed, as an old job's inbox
/// swallowed it), then serves normally.
struct MuteHost {
    inner: FolderHost,
    mute: std::sync::atomic::AtomicU32,
    seen: std::sync::atomic::AtomicU32,
}

impl ava1::router::JobHost for MuteHost {
    fn accept(&self, link: ava1::router::JobLink, first: ava1::conn::Frame, peer: [u8; 32]) {
        use std::sync::atomic::Ordering::SeqCst;
        if first.ty == gen::JobOpen::TYPE {
            self.seen.fetch_add(1, SeqCst);
            let left = self.mute.load(SeqCst);
            if left > 0 {
                self.mute.store(left - 1, SeqCst);
                drop(link);
                return;
            }
        }
        self.inner.accept(link, first, peer)
    }
}

async fn mute_host(dir: &Path, mute: u32) -> (Arc<MuteHost>, Pool) {
    let ava = dir.join("ava");
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    let mut peers = PeerStore::in_memory();
    peers.add(me.public(), "engine").unwrap();
    let h = Arc::new(MuteHost {
        inner: FolderHost {
            root: dir.join("share"),
            jobs_dir: dir.join("hjobs"),
        },
        mute: mute.into(),
        seen: 0.into(),
    });
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "host",
        peers,
        node_info_rpc(),
    )
    .with_jobs(h.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    (
        h,
        Pool::new(ava)
            .with_addr(addr)
            .with_open_ack_timeout(Duration::from_millis(400)),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_open_nobody_answers_is_retried_after_the_ack_timeout() {
    let d = temp_dir("mute-then-ok");
    let src = d.join("src");
    tree(&src, 10, |_| 2048);
    let (h, pool) = mute_host(&d, 2).await;
    let c = cfg();
    within(
        60,
        tokio::task::spawn_blocking(move || {
            upload::upload_dir_in(&pool, &c, [0x31; 16], "out", &src)
        }),
    )
    .await
    .unwrap()
    .expect("the upload completes once the open is answered");
    assert_eq!(
        h.seen.load(Ordering::SeqCst),
        3,
        "two lost opens, then the real one"
    );
    same_tree(&d.join("src"), &d.join("share/out"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_console_that_never_answers_the_open_fails_with_a_typed_reason() {
    let d = temp_dir("mute-forever");
    let src = d.join("src");
    tree(&src, 4, |_| 1024);
    let (h, pool) = mute_host(&d, u32::MAX).await;
    let pool = pool.with_busy_tries(2);
    let c = cfg();
    let e = within(
        60,
        tokio::task::spawn_blocking(move || {
            upload::upload_dir_in(&pool, &c, [0x32; 16], "out", &src)
        }),
    )
    .await
    .unwrap()
    .unwrap_err();
    let f = e
        .downcast_ref::<upload::UploadFailure>()
        .unwrap_or_else(|| panic!("not a classified failure: {e:#}"));
    assert_eq!(f.reason, "ava1_open_timeout", "{f:?}");
    assert_eq!(
        h.seen.load(Ordering::SeqCst),
        3,
        "the first try and two retries"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_ends_the_wait_for_an_unanswered_open() {
    let d = temp_dir("mute-cancel");
    let src = d.join("src");
    tree(&src, 4, |_| 1024);
    let (_h, pool) = mute_host(&d, u32::MAX).await;
    let pool = pool.with_open_ack_timeout(Duration::from_secs(30));
    let c = cfg();
    let cancel = c.cancel.clone().unwrap();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(600));
        cancel.store(true, Ordering::Relaxed);
    });
    let t = std::time::Instant::now();
    let e = within(
        60,
        tokio::task::spawn_blocking(move || {
            upload::upload_dir_in(&pool, &c, [0x33; 16], "out", &src)
        }),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(e.to_string().contains("cancel"), "{e:#}");
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
}

/// A host whose first job never ends and never reads: its link stays registered on the session, the way
/// a cancelled job still draining its lanes is.
struct HoldHost {
    held: std::sync::Mutex<Vec<ava1::router::JobLink>>,
    seen: std::sync::atomic::AtomicU32,
}

impl ava1::router::JobHost for HoldHost {
    fn accept(&self, link: ava1::router::JobLink, _first: ava1::conn::Frame, _peer: [u8; 32]) {
        self.seen.fetch_add(1, Ordering::SeqCst);
        self.held.lock().unwrap().push(link);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_open_for_a_job_still_registered_is_answered_busy_not_swallowed() {
    // The job id is still registered on the session (its old run is closing). The new JobOpen must be
    // answered BUSY by the server at once: routed to the old job it would be dropped, unanswered.
    let d = temp_dir("hold-busy");
    let src = d.join("src");
    tree(&src, 2, |_| 512);
    let ava = d.join("ava");
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    let mut peers = PeerStore::in_memory();
    peers.add(me.public(), "engine").unwrap();
    let h = Arc::new(HoldHost {
        held: Default::default(),
        seen: 0.into(),
    });
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "host",
        peers,
        node_info_rpc(),
    )
    .with_jobs(h.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    let pool = Pool::new(ava)
        .with_addr(addr)
        .with_busy_tries(2)
        .with_open_ack_timeout(Duration::from_secs(1));
    let c = cfg();
    let t = std::time::Instant::now();
    let e = within(
        60,
        tokio::task::spawn_blocking(move || {
            upload::upload_dir_in(&pool, &c, [0x34; 16], "out", &src)
        }),
    )
    .await
    .unwrap()
    .unwrap_err();
    let f = e
        .downcast_ref::<upload::UploadFailure>()
        .unwrap_or_else(|| panic!("not a classified failure: {e:#}"));
    assert_eq!(f.reason, "ava1_busy", "{f:?}");
    assert!(t.elapsed() < Duration::from_secs(15), "{:?}", t.elapsed());
    assert_eq!(
        h.seen.load(Ordering::SeqCst),
        1,
        "only the first open reached the host"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_ends_the_busy_wait() {
    let d = temp_dir("busy-cancel");
    let src = d.join("src");
    tree(&src, 4, |_| 1024);
    let (_h, pool) = busy_host(&d, u32::MAX).await;
    let c = cfg();
    let cancel = c.cancel.clone().unwrap();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(600));
        cancel.store(true, Ordering::Relaxed);
    });
    let t = std::time::Instant::now();
    let e = within(
        60,
        tokio::task::spawn_blocking(move || {
            upload::upload_dir_in(&pool, &c, [0x23; 16], "out", &src)
        }),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(e.to_string().contains("cancel"), "{e:#}");
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
}

/// Hardware run 2026-10-04 (drop60): a proxy killing every connection on a short period must
/// not strand the job. Whatever happens to the link, the console coming back means the job
/// continues from its durable state and ends byte-exact.
#[tokio::test(flavor = "multi_thread")]
async fn periodic_kills_never_strand_an_upload() {
    let d = temp_dir("periodic-kill");
    let src = d.join("src");
    let total = tree(&src, 1, |_| 32 << 20);
    let (addr, pool) = host(&d, true).await;
    let proxy = ChaosProxy::start(
        addr.parse().unwrap(),
        ChaosConfig {
            bytes_per_sec: Some(8 << 20),
            kill_every: Some(Duration::from_millis(2000)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let pool = Arc::new(pool.with_addr(proxy.addr.to_string()));
    let c = cfg();
    let finalized = c.progress_bytes_finalized.clone().unwrap();
    let (pool2, src2) = (pool.clone(), src.clone());
    // ~6 s natively. Coverage instrumentation (CARGO_LLVM_COV) slows every handshake against
    // the same 2 s kill period, so each attempt lands less: give it room there.
    let bound = if std::env::var_os("CARGO_LLVM_COV").is_some() {
        240
    } else {
        60
    };
    within(
        bound,
        tokio::task::spawn_blocking(move || {
            upload::upload_dir_in(&pool2, &c, [9; 16], "in", &src2)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    same_tree(&src, &d.join("share/in"));
    assert_eq!(finalized.load(Ordering::Relaxed), total);
    println!("periodic kills: attempts = {}", pool.attempts());
    drop(proxy);
}
