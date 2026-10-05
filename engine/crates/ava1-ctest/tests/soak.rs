//! Host soak (review 009 #2c): loops whole jobs against the real C data layer and demands that
//! its counters return to baseline after every one: the job table empty, nothing unswept, no
//! pack segments left on disk, the process's open descriptors unchanged and its memory bounded.
//!
//! `soak_for_the_configured_minutes` is `#[ignore]`: run it with
//! `AVA1_SOAK_MINUTES=60 cargo test -p ava1-ctest --test soak -- --ignored --test-threads=1`
//! (under `AVA1_CTEST_SANITIZE=1` for the weekly job). `AVA1_SOAK_RSS_MB` bounds the memory
//! growth over the first iteration (default 192; ASan inflates it). The plain test below runs two
//! iterations on every `cargo test` so the harness itself cannot rot.
#![cfg(unix)]
mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1_chaos::{ChaosConfig, ChaosProxy};
use ava1_ctest::{c_job_count, c_reap_far, c_set_read_allowed, c_unswept_global, CServer, LogOpts};
use common::{dir, same_tree, write_tree, SECRET};
use ps5upload_ava1::download::{to_zip_in, Counters};
use ps5upload_ava1::upload::upload_dir_in;
use ps5upload_ava1::Pool;
use ps5upload_core::download::DownloadKind;
use ps5upload_core::transfer::TransferConfig;

fn cfg() -> TransferConfig {
    let mut c = TransferConfig::new("127.0.0.1:9120");
    c.progress_bytes = Some(Arc::new(AtomicU64::new(0)));
    c.progress_files = Some(Arc::new(AtomicU64::new(0)));
    c.progress_files_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.progress_bytes_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.cancel = Some(Arc::new(AtomicBool::new(false)));
    c
}

/// Open descriptors of this process (`/proc/self/fd`, or `/dev/fd` on macOS).
fn open_fds() -> usize {
    let p = if Path::new("/proc/self/fd").exists() {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    std::fs::read_dir(p).map(|d| d.count()).unwrap_or(0)
}

/// Resident set size in MiB; 0 when this platform gives no cheap answer.
fn rss_mb() -> u64 {
    if let Ok(s) = std::fs::read_to_string("/proc/self/statm") {
        let pages: u64 = s
            .split_whitespace()
            .nth(1)
            .and_then(|x| x.parse().ok())
            .unwrap_or(0);
        return pages * 4096 / (1 << 20);
    }
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map_or(0, |kb| kb / 1024)
}

/// Pack segments left in any job directory.
fn pack_files(jobs: &Path) -> usize {
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(jobs) {
        for e in rd.flatten() {
            if let Ok(inner) = std::fs::read_dir(e.path()) {
                n += inner
                    .flatten()
                    .filter(|f| f.file_name().to_string_lossy().starts_with("pack."))
                    .count();
            }
        }
    }
    n
}

struct Env {
    t: PathBuf,
    ava: PathBuf,
    src: PathBuf,
    jobs: PathBuf,
    srv: CServer,
    proxy: ChaosProxy,
    total: u64,
}

impl Env {
    async fn new(tag: &str) -> Env {
        c_set_read_allowed(true);
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
        let jobs = t.join("jobs");
        // durable-by-log ON (the host config leaves it off), swept soon: the pack
        // segments, the unswept counter and the settle path are what the soak is here to watch
        let srv = CServer::start_data_opts(
            SECRET,
            &t.join("srv-peers"),
            &jobs,
            200,
            2000,
            2000,
            0,
            0,
            LogOpts {
                sweep_age_ms: 300,
                ..LogOpts::ON
            },
        );
        let proxy = ChaosProxy::start(
            srv.addr().parse().unwrap(),
            ChaosConfig {
                bytes_per_sec: Some(4 << 20),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // mixed: many small files (the pack log), a few medium, one large
        let src = t.join("src");
        write_tree(&src, 240, |i| match i {
            0 => 3 << 20,
            1..=4 => 200_000 + i,
            _ => i % 3000,
        });
        let total: u64 = walk_bytes(&src);
        Env {
            t,
            ava,
            src,
            jobs,
            srv,
            proxy,
            total,
        }
    }

    /// One whole round: an upload cancelled mid-way and resumed, the result downloaded as a
    /// zip, every session dropped so the jobs park, the clock moved past the park age.
    async fn round(&self, n: u32) -> bool {
        let mut id = [0xa5u8; 16];
        id[..4].copy_from_slice(&n.to_le_bytes());
        let dest = self.t.join(format!("dest{n}"));
        let dest_s = dest.to_str().unwrap().to_string();
        // a fresh pool (and so a fresh session) per round: dropping it parks its jobs
        let pool = Arc::new(Pool::new(self.ava.clone()).with_addr(self.proxy.addr.to_string()));

        // 1. upload, cancelled once a few hundred KiB are on the wire
        let c1 = cfg();
        let (flag, sent) = (
            c1.cancel.clone().unwrap(),
            c1.progress_bytes.clone().unwrap(),
        );
        let (p, s, d) = (pool.clone(), self.src.clone(), dest_s.clone());
        let first = tokio::task::spawn_blocking(move || upload_dir_in(&p, &c1, id, &d, &s));
        let t0 = Instant::now();
        while sent.load(Ordering::Relaxed) < (512 << 10) && !first.is_finished() {
            assert!(
                t0.elapsed() < Duration::from_secs(60),
                "round {n}: no progress"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        flag.store(true, Ordering::Relaxed);
        let r = tokio::time::timeout(Duration::from_secs(60), first)
            .await
            .expect("the cancelled upload never returned")
            .unwrap();
        // (a very fast round may finish before the flag lands: then there is nothing to resume)
        let cancelled = match r {
            Err(e) => {
                assert!(e.to_string().contains("cancel"), "round {n}: {e:#}");
                true
            }
            Ok(_) => false,
        };

        // 2. resume the same job
        let c2 = cfg();
        let (p, s, d) = (pool.clone(), self.src.clone(), dest_s.clone());
        tokio::time::timeout(
            Duration::from_secs(120),
            tokio::task::spawn_blocking(move || upload_dir_in(&p, &c2, id, &d, &s)),
        )
        .await
        .expect("the resume hung")
        .unwrap()
        .unwrap_or_else(|e| panic!("round {n}: resume failed: {e:#}"));
        assert!(same_tree(&self.src, &dest), "round {n}: the upload differs");

        // 3. the uploaded folder as a zip
        let zip = self.t.join(format!("out{n}.zip"));
        let (p, z, src) = (pool.clone(), zip.clone(), dest_s.clone());
        let mut zid = id;
        zid[15] ^= 0xff;
        let bytes = tokio::task::spawn_blocking(move || {
            to_zip_in(
                &p,
                "console",
                &src,
                DownloadKind::Folder,
                &z,
                true,
                zid,
                &Counters::default(),
                None,
            )
        })
        .await
        .unwrap()
        .unwrap_or_else(|e| panic!("round {n}: zip download failed: {e:#}"));
        assert!(
            bytes >= self.total,
            "round {n}: zip holds {bytes} < {}",
            self.total
        );
        assert_eq!(&std::fs::read(&zip).unwrap()[..2], b"PK");
        let _ = std::fs::remove_file(&zip);
        let _ = std::fs::remove_dir_all(&dest);

        // 4. sessions gone: the jobs are parked; reap as if past the park age
        drop(pool);
        let t0 = Instant::now();
        loop {
            c_reap_far();
            if c_job_count() == 0 {
                break;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "round {n}: {} job(s) still in the table",
                c_job_count()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // 5. every counter back to baseline
        let t0 = Instant::now();
        while c_unswept_global() != 0 || pack_files(&self.jobs) != 0 {
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "round {n}: unswept {} bytes, {} pack segment(s) left",
                c_unswept_global(),
                pack_files(&self.jobs)
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        cancelled
    }
}

fn walk_bytes(d: &Path) -> u64 {
    let mut n = 0;
    for e in std::fs::read_dir(d).unwrap().flatten() {
        let m = e.metadata().unwrap();
        n += if m.is_dir() {
            walk_bytes(&e.path())
        } else {
            m.len()
        };
    }
    n
}

/// Runs rounds until `rounds` are done or `deadline` passes (whichever comes first), checking the
/// baselines after each. Round 0 warms the process up and sets the baselines.
async fn soak(tag: &str, rounds: u32, deadline: Option<Duration>) -> u32 {
    let env = Env::new(tag).await;
    let rss_cap: u64 = std::env::var("AVA1_SOAK_RSS_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(192);
    let start = Instant::now();
    let (mut fds0, mut rss0) = (0usize, 0u64);
    let (mut n, mut cancels) = (0u32, 0u32);
    while n < rounds && deadline.is_none_or(|d| start.elapsed() < d) {
        cancels += env.round(n).await as u32;
        // let detached threads (lane joins, the tokio blocking pool) settle before counting
        tokio::time::sleep(Duration::from_millis(300)).await;
        let (fds, rss) = (open_fds(), rss_mb());
        if n == 0 {
            (fds0, rss0) = (fds, rss);
        } else {
            assert_eq!(
                fds, fds0,
                "round {n}: open descriptors drifted ({fds0} -> {fds})"
            );
            assert!(
                rss <= rss0 + rss_cap,
                "round {n}: RSS grew {rss0} -> {rss} MiB (cap +{rss_cap})"
            );
        }
        if n % 20 == 0 {
            eprintln!(
                "soak round {n}: fds {fds} rss {rss} MiB elapsed {:?}",
                start.elapsed()
            );
        }
        n += 1;
    }
    assert!(
        cancels > 0,
        "no round ever cancelled mid-upload: the resume path was not exercised"
    );
    drop(env.srv);
    n
}

#[tokio::test(flavor = "multi_thread")]
async fn a_few_soak_rounds_return_every_counter_to_baseline() {
    assert_eq!(soak("soak-quick", 3, None).await, 3);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "hours-long; set AVA1_SOAK_MINUTES"]
async fn soak_for_the_configured_minutes() {
    let minutes: f64 = std::env::var("AVA1_SOAK_MINUTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.0);
    let n = soak(
        "soak-long",
        u32::MAX,
        Some(Duration::from_secs_f64(minutes * 60.0)),
    )
    .await;
    eprintln!("soak: {n} rounds in {minutes} minute(s), every counter at baseline");
    assert!(n >= 2, "the soak ran only {n} round(s)");
}
