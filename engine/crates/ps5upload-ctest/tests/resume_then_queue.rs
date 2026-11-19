//! #873: an upload that was interrupted and resumed, and the queue's next uploads to the same
//! console. On the C receiver, under the console's small descriptor budget (the `fd_budget`
//! knob: a Mac gives a job thousands of large-file slots, a console about 61) and with the
//! read-back on (the default), through the engine's own upload adapters.
//!
//! Two things are pinned here:
//! - the resumed upload and the fresh ones after it each end soon after their last byte, however
//!   the first one was interrupted (Stop, or a link that died with the console keeping the job);
//! - the engine's count of what the console has never runs ahead of it. A resume right after a
//!   Stop finds frames of the stopped run still arriving: the console writes them, the resumed
//!   run sends the same ranges again, and the console reports both as durable. Counted twice,
//!   the job read "everything sent" (the queue row's "Finalizing on PS5") while tens of MiB were
//!   still on their way, and its result said the console had found bytes that did not match.
#![cfg(unix)]
mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1_chaos::{ChaosConfig, ChaosProxy};
use common::{dir, same_tree, write_tree, SECRET};
use ps5upload_ava1::upload::{upload_dir_in, upload_file_in};
use ps5upload_ava1::Pool;
use ps5upload_core::transfer::{TransferConfig, TransferResult};
use ps5upload_ctest::{CServer, LogOpts, TempDir};

const MIB: u64 = 1 << 20;

/// One run of an upload: its config and the counters the engine's ticker would read.
struct Run {
    cfg: TransferConfig,
    sent: Arc<AtomicU64>,
    durable: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}

fn run() -> Run {
    let mut c = TransferConfig::new("127.0.0.1:9120");
    let sent = Arc::new(AtomicU64::new(0));
    let durable = Arc::new(AtomicU64::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    c.progress_bytes = Some(sent.clone());
    c.progress_files = Some(Arc::new(AtomicU64::new(0)));
    c.progress_files_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.progress_bytes_finalized = Some(durable.clone());
    c.progress_bytes_resumed = Some(Arc::new(AtomicU64::new(0)));
    c.cancel = Some(cancel.clone());
    Run {
        cfg: c,
        sent,
        durable,
        cancel,
    }
}

fn bytes_of(p: &Path) -> u64 {
    if p.is_file() {
        return p.metadata().unwrap().len();
    }
    std::fs::read_dir(p)
        .unwrap()
        .map(|e| bytes_of(&e.unwrap().path()))
        .sum()
}

/// A game-like tree: large files of several chunks between small ones.
fn game(src: &Path, files: usize, seed: usize) {
    write_tree(src, files, |i| {
        if i % 3 == 0 {
            (2 << 20) + ((i + seed) * 7919) % (3 << 20)
        } else {
            4096 + ((i + seed) * 131) % (60 << 10)
        }
    });
}

/// The C receiver as a console (small descriptor budget, durable-by-log, a slow drive), a link
/// to it that can be throttled and cut, and the engine's session pool.
struct Console {
    t: TempDir,
    _srv: CServer,
    proxy: ChaosProxy,
    pool: Arc<Pool>,
}

async fn console(tag: &str, bytes_per_sec: Option<u64>) -> Console {
    let t = dir(tag);
    let ava = t.join("ava");
    std::fs::create_dir_all(&ava).unwrap();
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    PeerStore::load(&t.join("srv-peers"))
        .unwrap()
        .add(me.public(), "engine")
        .unwrap();
    PeerStore::load(&ava.join("peers"))
        .unwrap()
        .add(Identity::from_secret(SECRET).public(), "C receiver")
        .unwrap();
    let srv = CServer::start_data_opts(
        SECRET,
        &t.join("srv-peers"),
        &t.join("jobs"),
        200,
        4000,
        4000,
        3000, // a slow drive: files stay open long enough to pile up
        0,
        LogOpts::ON, // durable-by-log, as on the console
    );
    srv.knob("fd_budget", 120);
    let proxy = ChaosProxy::start(
        srv.addr().parse().unwrap(),
        ChaosConfig {
            bytes_per_sec,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let pool = Arc::new(Pool::new(ava).with_addr(proxy.addr.to_string()));
    Console {
        t,
        _srv: srv,
        proxy,
        pool,
    }
}

fn start(
    pool: &Arc<Pool>,
    cfg: TransferConfig,
    src: &Path,
    dest: &str,
    id: [u8; 16],
) -> tokio::task::JoinHandle<anyhow::Result<TransferResult>> {
    let (p, s, d) = (pool.clone(), src.to_path_buf(), dest.to_string());
    tokio::task::spawn_blocking(move || {
        if s.is_dir() {
            upload_dir_in(&p, &cfg, id, &d, &s)
        } else {
            upload_file_in(&p, &cfg, id, &d, &s)
        }
    })
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Interrupt {
    /// Stop pressed: the engine cancels the job, and the console hears of it.
    Cancel,
    /// The link dies and the engine's job ends without the console hearing of it (the app was
    /// closed, the computer slept): the console keeps the job parked, with its files open.
    Drop,
}

/// Starts `src` and interrupts it once `part` (per mille) of it is acknowledged.
async fn interrupted(c: &Console, src: &Path, dest: &str, id: [u8; 16], part: u64, how: Interrupt) {
    let total = bytes_of(src);
    let r = run();
    let job = start(&c.pool, r.cfg, src, dest, id);
    let t0 = Instant::now();
    while r.sent.load(Ordering::Relaxed) < total * part / 1000 {
        assert!(t0.elapsed() < Duration::from_secs(120), "no progress");
        assert!(
            !job.is_finished(),
            "the upload ended before it was interrupted"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    if how == Interrupt::Drop {
        c.proxy.blackhole(true);
        c.proxy.kill_all();
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    r.cancel.store(true, Ordering::Relaxed);
    let e = tokio::time::timeout(Duration::from_secs(60), job)
        .await
        .expect("the interrupted upload never returned")
        .unwrap()
        .expect_err("interrupted");
    eprintln!(
        "interrupted ({how:?}) with {} of {total} bytes acknowledged, {} durable: {e:#}",
        r.sent.load(Ordering::Relaxed),
        r.durable.load(Ordering::Relaxed)
    );
    if how == Interrupt::Drop {
        c.proxy.blackhole(false);
    }
}

/// How an upload that ran to its end went.
struct Finished {
    /// Seconds between the count reading "every byte is on the console" and the job's end.
    tail: f64,
    /// The highest the count read, and what it read at the end.
    peak: u64,
    last: u64,
    /// Bytes this run put on the wire.
    wire: u64,
    ack: serde_json::Value,
}

async fn finish(c: &Console, src: &Path, dest: &str, id: [u8; 16], what: &str) -> Finished {
    let bound = Duration::from_secs(if cfg!(ava1_ctest_sanitize) { 900 } else { 300 });
    let total = bytes_of(src);
    let r = run();
    let job = start(&c.pool, r.cfg, src, dest, id);
    let t0 = Instant::now();
    let mut all_sent: Option<Instant> = None;
    let mut peak = 0;
    while !job.is_finished() {
        let now = r.sent.load(Ordering::Relaxed);
        peak = peak.max(now);
        if all_sent.is_none() && now >= total {
            all_sent = Some(Instant::now());
        }
        if t0.elapsed() > bound {
            r.cancel.store(true, Ordering::Relaxed); // let the blocked thread end
            panic!(
                "{what}: not finished in {bound:?}: {now} of {total} bytes counted, {} durable, \
                 every byte counted {:?} ago",
                r.durable.load(Ordering::Relaxed),
                all_sent.map(|t| t.elapsed())
            );
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let res = job
        .await
        .unwrap()
        .unwrap_or_else(|e| panic!("{what} failed: {e:#}"));
    let last = r.sent.load(Ordering::Relaxed);
    let f = Finished {
        tail: all_sent.map_or(0.0, |t| t.elapsed().as_secs_f64()),
        peak: peak.max(last),
        last,
        wire: res.bytes_sent,
        ack: serde_json::from_str(&res.commit_ack_body).unwrap(),
    };
    eprintln!(
        "{what}: {:.1} s, {:.1} s after the count read every byte; count peak {} last {} of \
         {total}; {} bytes on the wire: {}",
        t0.elapsed().as_secs_f64(),
        f.tail,
        f.peak,
        f.last,
        f.wire,
        res.commit_ack_body
    );
    f
}

/// How long an upload may go on after its last byte here: these trees commit in well under a
/// second, a wedge never ends.
fn prompt() -> f64 {
    if cfg!(ava1_ctest_sanitize) {
        120.0
    } else {
        30.0
    }
}

/// A game is interrupted part-way and resumed; two more follow it, as the queue sends them.
async fn queue_after(tag: &str, how: Interrupt) {
    let c = console(tag, None).await;
    let srcs: Vec<PathBuf> = (0..3)
        .map(|k| {
            let s = c.t.join(format!("src{k}"));
            game(&s, 150, k * 17);
            s
        })
        .collect();
    let dest = |k: usize| c.t.join(format!("dst{k}")).to_str().unwrap().to_string();
    interrupted(&c, &srcs[0], &dest(0), [0xa1; 16], 400, how).await;
    for (k, (id, what)) in [
        ([0xa1; 16], "the resumed upload"),
        ([0xb2; 16], "the second upload"),
        ([0xc3; 16], "the third upload"),
    ]
    .into_iter()
    .enumerate()
    {
        let total = bytes_of(&srcs[k]);
        let f = finish(&c, &srcs[k], &dest(k), id, what).await;
        assert!(
            f.tail < prompt(),
            "{what} went on {:.1} s after its last byte",
            f.tail
        );
        assert_eq!(f.last, total, "{what}: the count ends at the job's size");
        assert!(f.peak <= total, "{what}: counted {} of {total}", f.peak);
        assert_eq!(f.ack["rechecked_bytes"], 0, "{what}: {}", f.ack);
        assert!(same_tree(&srcs[k], Path::new(&dest(k))), "{what}: the tree");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn after_a_stop_the_resume_and_the_next_uploads_end_soon_after_their_last_byte() {
    queue_after("resume-queue-stop", Interrupt::Cancel).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn after_a_lost_link_the_resume_and_the_next_uploads_end_soon_after_their_last_byte() {
    queue_after("resume-queue-drop", Interrupt::Drop).await;
}

/// One large file (a game image) behind a link slow enough to hold frames in flight: stopped at
/// three fifths and resumed at once.
#[tokio::test(flavor = "multi_thread")]
async fn a_resume_right_after_a_stop_counts_no_byte_twice() {
    let c = console("resume-count", Some(48 * MIB)).await;
    let size = 256 * MIB as usize;
    let src = c.t.join("game.img");
    let body: Vec<u8> = (0..size).map(|i| ((i * 31) ^ (i >> 9)) as u8).collect();
    std::fs::write(&src, &body).unwrap();
    let dest = c.t.join("dst/game.img");
    let dest_s = dest.to_str().unwrap().to_string();
    let total = size as u64;
    interrupted(&c, &src, &dest_s, [0xa1; 16], 600, Interrupt::Cancel).await;
    let f = finish(&c, &src, &dest_s, [0xa1; 16], "the resumed upload").await;
    let resumed = f.ack["resumed_bytes"].as_u64().unwrap();
    assert!(resumed > 0 && resumed < total, "{}", f.ack);
    // What the console held plus what this run sent is the file, once.
    assert_eq!(resumed + f.wire, total, "{}", f.ack);
    // So the count never passes the file's size, and it reads the whole file only once the
    // console has acknowledged its last byte.
    assert!(
        f.peak <= total,
        "the count read {} of {total}: {} bytes were counted twice",
        f.peak,
        f.peak - total
    );
    assert_eq!(f.last, total);
    // Nothing failed a check on the console: no byte was "found not to match".
    assert_eq!(f.ack["rechecked_bytes"], 0, "{}", f.ack);
    assert!(std::fs::read(&dest).unwrap() == body, "the file");
}
