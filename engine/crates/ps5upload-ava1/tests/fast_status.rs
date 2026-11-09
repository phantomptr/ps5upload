//! How fast a status check answers when the pooled session went quiet under it: a console
//! that went into rest mode (its open connection silent, its port refusing), one whose open
//! connection died while the console itself is fine, and one coming back after rest. Against
//! the Rust AVA1 server on 127.0.0.1 behind a proxy that can freeze the connections it carries.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ava1::host::FolderHost;
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::server::{self, ServerCtx};
use ava1::session::RpcReply;
use ava1::wire::Message;
use ps5upload_ava1::gen;
use ps5upload_ava1::mgmt::AvaTransport;
use ps5upload_ava1::Pool;
use ps5upload_core::mgmt::{m, MgmtTransport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The status route's per-call budget (`ps5_status` in the engine).
const ROUTE_TIMEOUT: Duration = ps5upload_ava1::mgmt::STATUS_TIMEOUT;

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("p5a-fast-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A proxy in front of the console that can freeze every connection open now (they stay open
/// and take bytes, but nothing comes back: a console whose helper stopped mid-session) and
/// stop or start listening (a port that refuses, as in rest mode).
struct Proxy {
    addr: String,
    upstream: String,
    next: Arc<AtomicU64>,
    frozen_below: Arc<AtomicU64>,
    listener: Option<tokio::task::JoinHandle<()>>,
}

impl Proxy {
    async fn start(upstream: String) -> Proxy {
        let mut p = Proxy {
            addr: "127.0.0.1:0".into(),
            upstream,
            next: Arc::default(),
            frozen_below: Arc::default(),
            listener: None,
        };
        p.listen().await;
        p
    }

    async fn listen(&mut self) {
        let l = loop {
            match tokio::net::TcpListener::bind(&self.addr).await {
                Ok(l) => break l,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        };
        self.addr = l.local_addr().unwrap().to_string();
        let (up, next, frozen) = (
            self.upstream.clone(),
            self.next.clone(),
            self.frozen_below.clone(),
        );
        self.listener = Some(tokio::spawn(async move {
            while let Ok((down, _)) = l.accept().await {
                let me = next.fetch_add(1, Ordering::SeqCst);
                let (up, frozen) = (up.clone(), frozen.clone());
                tokio::spawn(async move {
                    let Ok(upc) = tokio::net::TcpStream::connect(&up).await else {
                        return;
                    };
                    let (dr, dw) = down.into_split();
                    let (ur, uw) = upc.into_split();
                    tokio::spawn(pump(dr, uw, me, frozen.clone()));
                    tokio::spawn(pump(ur, dw, me, frozen));
                });
            }
        }));
    }

    fn freeze_open_connections(&self) {
        self.frozen_below
            .store(self.next.load(Ordering::SeqCst), Ordering::SeqCst);
    }

    async fn stop_listening(&mut self) {
        if let Some(t) = self.listener.take() {
            t.abort();
            let _ = t.await;
        }
    }
}

async fn pump(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    me: u64,
    frozen_below: Arc<AtomicU64>,
) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match from.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        // A frozen connection swallows what it is sent and says nothing, never closing.
        if me < frozen_below.load(Ordering::SeqCst) {
            continue;
        }
        if to.write_all(&buf[..n]).await.is_err() {
            return;
        }
    }
}

/// A console answering `node.status`, behind a proxy, and a transport whose pool dials the
/// proxy. `power` is the pool's DDP answer (`Some(620)`: rest mode).
async fn console(
    tag: &str,
    power: Option<u16>,
) -> (Arc<AvaTransport>, &'static Pool, Proxy, String) {
    console_slow(tag, power, Arc::default()).await
}

/// [`console`] whose `node.status` takes 3 s while `slow` is set (a helper busy with a scan).
async fn console_slow(
    tag: &str,
    power: Option<u16>,
    slow: Arc<AtomicBool>,
) -> (Arc<AvaTransport>, &'static Pool, Proxy, String) {
    let base = temp(tag);
    let ava = base.join("ava");
    std::fs::create_dir_all(&ava).unwrap();
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    let mut peers = PeerStore::in_memory();
    peers.add(me.public(), "engine").unwrap();
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "host",
        peers,
        Box::new(move |method, _| {
            assert_eq!(method, gen::METHOD_NODE_STATUS);
            if slow.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_secs(3));
            }
            RpcReply {
                status: gen::STATUS_OK,
                body: gen::NodeStatus {
                    version: "9.9.9".into(),
                    ..Default::default()
                }
                .to_bytes()
                .unwrap(),
            }
        }),
    )
    .with_jobs(Arc::new(FolderHost {
        root: base.join("share"),
        jobs_dir: base.join("jobs"),
    }))
    .with_mgmt();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(server::serve(l, Arc::new(ctx)));
    let proxy = Proxy::start(addr).await;
    let pool: &'static Pool = Box::leak(Box::new(
        Pool::new(ava)
            .with_addr(proxy.addr.clone())
            .with_power_probe(move |_| power),
    ));
    let t = AvaTransport::with_pool(pool);
    (Arc::new(t), pool, proxy, format!("{tag}-console"))
}

async fn status(t: &Arc<AvaTransport>, c: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let (t, c) = (t.clone(), c.to_string());
    tokio::task::spawn_blocking(move || t.call(&c, m::NODE_STATUS, "STATUS", b"", ROUTE_TIMEOUT))
        .await
        .unwrap()
}

/// Calls status every `every` (as a client polls) until it answers, up to `limit`; how long
/// that took from `t0`, and how long the first call took to answer at all.
async fn until_ok(
    t: &Arc<AvaTransport>,
    c: &str,
    every: Duration,
    limit: Duration,
) -> (Duration, Duration) {
    let t0 = Instant::now();
    let mut first = None;
    loop {
        let r = status(t, c).await;
        first.get_or_insert(t0.elapsed());
        if r.is_ok() {
            return (t0.elapsed(), first.unwrap());
        }
        assert!(t0.elapsed() < limit, "no good status within {limit:?}");
        tokio::time::sleep(every).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_console_gone_quiet_under_its_session_is_reported_in_seconds() {
    let (t, _p, mut proxy, c) = console("rest", Some(620)).await;
    assert!(status(&t, &c).await.unwrap().is_some());
    // The console goes into rest mode: its open connection says nothing more, its port refuses.
    proxy.freeze_open_connections();
    proxy.stop_listening().await;
    let t0 = Instant::now();
    let e = status(&t, &c).await.unwrap_err();
    let first = t0.elapsed();
    eprintln!("rest: first status answered in {first:?}: {e:#}");
    assert!(
        first < ROUTE_TIMEOUT + Duration::from_secs(1),
        "the stale session held the check for {first:?}"
    );
    let timing = ps5upload_ava1::timings::last(&c).unwrap();
    assert!(timing.stale_session_dropped, "{timing:?}");
    assert_eq!(timing.tcp_ok, Some(false));
    // The next check opens a session, finds the port refusing and DDP says rest mode.
    let t1 = Instant::now();
    let e = status(&t, &c).await.unwrap_err();
    eprintln!("rest: next status in {:?}: {e:#}", t1.elapsed());
    assert!(e.to_string().contains("console_rest_mode"), "{e:#}");
    assert!(t1.elapsed() < Duration::from_secs(4), "{:?}", t1.elapsed());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dead_session_to_a_console_that_is_fine_is_replaced_quickly() {
    let (t, _p, proxy, c) = console("dead", Some(200)).await;
    assert!(status(&t, &c).await.unwrap().is_some());
    // The open connection dies silently (the console dropped it while this computer slept);
    // new connections are served.
    proxy.freeze_open_connections();
    let (good, first) = until_ok(&t, &c, Duration::from_secs(1), Duration::from_secs(30)).await;
    eprintln!("dead session: first answer {first:?}, first good status {good:?}");
    assert!(
        good < Duration::from_secs(9),
        "a dead session held the console back for {good:?}"
    );
    let timing = ps5upload_ava1::timings::last(&c).unwrap();
    assert!(timing.ok, "{timing:?}");
    assert_eq!(timing.version.as_deref(), Some("9.9.9"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_console_back_from_rest_is_seen_before_its_remembered_verdict_runs_out() {
    let (t, pool, mut proxy, c) = console("wake", Some(620)).await;
    proxy.stop_listening().await;
    let e = status(&t, &c).await.unwrap_err();
    assert!(e.to_string().contains("console_rest_mode"), "{e:#}");
    assert_eq!(
        pool.recent_power(&c),
        Some(ps5upload_core::ddp::Power::Rest)
    );
    // It wakes: the port answers again while the rest verdict is still remembered (12 s).
    proxy.listen().await;
    let (good, _) = until_ok(&t, &c, Duration::from_millis(500), Duration::from_secs(30)).await;
    eprintln!("wake: first good status {good:?} after the port answered again");
    assert!(
        good < Duration::from_secs(4),
        "the remembered rest verdict held the console back for {good:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_healthy_console_is_checked_on_its_pooled_session() {
    let (t, pool, _proxy, c) = console("warm", Some(200)).await;
    assert!(status(&t, &c).await.unwrap().is_some());
    let before = pool.attempts();
    for _ in 0..5 {
        status(&t, &c).await.unwrap().unwrap();
    }
    assert_eq!(
        pool.attempts(),
        before,
        "no new session for a healthy console"
    );
    let timing = ps5upload_ava1::timings::last(&c).unwrap();
    assert_eq!(timing.reused_session, Some(true));
    assert!(!timing.stale_session_dropped && !timing.cached_verdict);
    assert!(timing.status_ms.is_some() && timing.handshake_ms.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_helper_is_slow_not_gone_and_keeps_its_session() {
    let slow = Arc::new(AtomicBool::new(false));
    let (t, pool, _proxy, c) = console_slow("busy", Some(200), slow.clone()).await;
    assert!(status(&t, &c).await.unwrap().is_some());
    let before = pool.attempts();
    slow.store(true, Ordering::SeqCst);
    let t0 = Instant::now();
    let e = status(&t, &c).await.unwrap_err();
    assert!(e.to_string().contains("status_slow"), "{e:#}");
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
    let timing = ps5upload_ava1::timings::last(&c).unwrap();
    assert!(!timing.stale_session_dropped, "{timing:?}");
    assert_eq!(timing.tcp_ok, Some(true));
    assert_eq!(timing.reason.as_deref(), Some("status_slow"));
    slow.store(false, Ordering::SeqCst);
    // Let the slow reply drain, then the same session answers again.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    status(&t, &c).await.unwrap().unwrap();
    assert_eq!(
        pool.attempts(),
        before,
        "the busy console's session was kept"
    );
}
