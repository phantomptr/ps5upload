//! Its own binary on purpose: this relay must end within seconds of the destination ending
//! its job, and in the shared relay binary it queued behind the multi-hundred-MiB relays (one
//! lock) and once hung the whole CI job. Here it runs alone, on small files (128 MiB in all:
//! still twice the relay's 64 MiB buffer, so the readers really are parked), and on a plain
//! thread so a hang fails at the bound instead of holding the runtime's drop.
mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ava1::gen;
use ava1::keys::Identity;
use ava1::send::Progress;
use common::*;
use ps5upload_ava1::relay::ps5_to_ps5_between;
use ps5upload_ava1::upload;
use ps5upload_ava1::Pool;

fn relay_failure(pa: Pool, pb: Pool) -> (String, Duration) {
    let started = std::time::Instant::now();
    let e = ps5_to_ps5_between(
        &pa,
        "a",
        "src",
        &pb,
        "b",
        "dst",
        [41; 16],
        Arc::new(Progress::default()),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap_err();
    // A terminal session failure names the console it came from (ConsoleFailure); others
    // are plain UploadFailures.
    let reason = match e.downcast_ref::<upload::ConsoleFailure>() {
        Some(cf) => cf.failure.reason.clone(),
        None => e
            .downcast_ref::<upload::UploadFailure>()
            .unwrap_or_else(|| panic!("not an UploadFailure: {e:#}"))
            .reason
            .clone(),
    };
    (reason, started.elapsed())
}

/// A destination that ends its upload part-way (its job stops, with a reason) while the
/// session stays open: the relay's readers are parked for bytes the destination will never
/// ask for. The relay must report the destination's reason within seconds, not after the
/// 120 s no-progress bound.
struct EndsItsJob {
    inner: ava1::host::FolderHost,
    ended: Arc<AtomicBool>,
}

impl ava1::router::JobHost for EndsItsJob {
    fn accept(&self, link: ava1::router::JobLink, first: ava1::conn::Frame, peer: [u8; 32]) {
        let upload = first
            .decode::<gen::JobOpen>()
            .map(|o| o.kind == gen::JOB_UPLOAD)
            .unwrap_or(false);
        if !upload {
            return self.inner.accept(link, first, peer);
        }
        let open: gen::JobOpen = first.decode().unwrap();
        let progress = Arc::new(Progress::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let o = ava1::recv::RecvOptions {
            credit: 64 << 20,
            flags: open.flags,
            jobs_dir: self.inner.jobs_dir.clone(),
            ordered: open.flags & gen::JF_ORDERED != 0,
            progress: progress.clone(),
            cancel: cancel.clone(),
            progress_deadline: None,
        };
        let sink = Arc::new(ava1::recv::LocalSink::new(
            self.inner.root.join(&open.root),
            open.flags & gen::JF_SINGLE_FILE != 0,
        ));
        let ended = self.ended.clone();
        tokio::spawn(async move {
            let mut link = link;
            let watch = tokio::spawn(async move {
                while progress.bytes_durable.load(Ordering::Relaxed) == 0 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                ended.store(true, Ordering::Relaxed);
                cancel.store(true, Ordering::Relaxed);
            });
            let _ = ava1::recv::receive_job(&mut link, open, sink, o).await;
            watch.abort();
            // The job is over; the session (and this link's connection) stays open.
            tokio::time::sleep(Duration::from_secs(120)).await;
            drop(link);
        });
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_destination_that_ends_its_job_stops_the_relay_within_seconds() {
    let d = temp("dest-ends");
    let ava = d.join("engine");
    let key = Identity::load_or_create(&ava.join("identity"))
        .unwrap()
        .public();
    let a = d.join("a");
    let b = d.join("b");
    std::fs::create_dir_all(a.join("share/src")).unwrap();
    std::fs::create_dir_all(b.join("share")).unwrap();
    for i in 0..4u8 {
        write_pattern(&a.join(format!("share/src/f{i}")), i, 32 << 20);
    }
    let addr_a = host(&a, key).await;
    // The source is read through a capped link (~8 s for the 128 MiB) so the destination's
    // first durable report, which ends its job, always lands mid-transfer: uncapped on a fast
    // runner the whole relay finished before the destination ended anything.
    let slow_a = ava1_chaos::ChaosProxy::start(
        addr_a.parse().unwrap(),
        ava1_chaos::ChaosConfig {
            bytes_per_sec: Some(16 << 20),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let ended = Arc::new(AtomicBool::new(false));
    let mut peers = ava1::peers::PeerStore::in_memory();
    peers.add(key, "engine").unwrap();
    let ctx = ava1::server::ServerCtx::new(Identity::generate().unwrap(), "host", peers, rpc())
        .with_jobs(Arc::new(EndsItsJob {
            inner: ava1::host::FolderHost {
                root: b.join("share"),
                jobs_dir: b.join("jobs"),
            },
            ended: ended.clone(),
        }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr_b = l.local_addr().unwrap().to_string();
    let server = tokio::spawn(ava1::server::serve(l, Arc::new(ctx)));
    let (pa, pb) = (
        Pool::new(ava.clone()).with_addr(slow_a.addr.to_string()),
        Pool::new(ava).with_addr(addr_b),
    );
    // A plain thread, not spawn_blocking: a hung relay must fail this test at the bound, and
    // a runtime waits for its blocking threads on drop — the binary then hung for the whole
    // CI job instead of failing here.
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(relay_failure(pa, pb));
    });
    let (reason, took) = tokio::time::timeout(Duration::from_secs(60), rx)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "the relay hung (destination ended: {})",
                ended.load(Ordering::Relaxed)
            )
        })
        .unwrap();
    server.abort();
    assert!(ended.load(Ordering::Relaxed), "the destination never ended");
    assert!(
        took < Duration::from_secs(15),
        "took {took:?} to notice (reason {reason})"
    );
    assert!(reason.starts_with("ava1_"), "reason {reason}");
}
