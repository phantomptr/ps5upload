//! Downloads, download-to-zip and console copy/move over AVA1, against Task 17's
//! `FolderHost` (downloads) and a test-local `job.*` RPC fake (copy). Public API only.
//! Every wait is bounded.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ava1::gen::{self, JobCopy, JobRef, Status};
use ava1::host::FolderHost;
use ava1::keys::Identity;
use ava1::manifest::{Entry, Manifest};
use ava1::peers::PeerStore;
use ava1::recv::Sink;
use ava1::server::{self, RpcHandler, ServerCtx};
use ava1::session::RpcReply;
use ava1::wire::{FrameMessage, Message};
use ava1_chaos::{ChaosConfig, ChaosProxy};
use ps5upload_ava1::copy::{
    console_copy_limited, console_copy_with, op_cancel, op_snapshot, record_status,
};
use ps5upload_ava1::download::{self, Counters, ZipCompression, ZipSink};
use ps5upload_ava1::upload::UploadFailure;
use ps5upload_ava1::Pool;
use ps5upload_core::download::DownloadKind;

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("p5a-dl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn within<T>(secs: u64, f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(secs), f)
        .await
        .expect("timed out")
}

fn bytes_for(i: usize, n: usize) -> Vec<u8> {
    let mut b = (i as u8).wrapping_mul(37).wrapping_add(11);
    (0..n)
        .map(|_| {
            b = b.wrapping_mul(31).wrapping_add(7);
            b
        })
        .collect()
}

/// Files `f0..` in a few subdirectories; returns the total bytes.
fn tree(dir: &Path, files: usize, size_fn: impl Fn(usize) -> usize) -> u64 {
    let mut total = 0;
    for i in 0..files {
        let p = dir.join(format!("d{}/f{i}", i % 5));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let n = size_fn(i);
        std::fs::write(p, bytes_for(i, n)).unwrap();
        total += n as u64;
    }
    total
}

/// Every regular file under `root`, by relative path.
fn files_of(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn go(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                go(root, &p, out);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    go(root, root, &mut out);
    out
}

fn node_info() -> RpcHandler {
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

/// A folder host sharing `dir/share`; `bind` reuses an address (a restarted host). The
/// host's identity is kept in `dir/srv-identity`, so a restart is the same console.
async fn serve_host(
    dir: &Path,
    engine_key: [u8; 32],
    bind: &str,
    rpc: RpcHandler,
    jobs: bool,
) -> (tokio::task::JoinHandle<()>, String) {
    let mut peers = PeerStore::in_memory();
    peers.add(engine_key, "engine").unwrap();
    let id = Identity::load_or_create(&dir.join("srv-identity")).unwrap();
    let mut ctx = ServerCtx::new(id, "host", peers, rpc);
    if jobs {
        ctx = ctx.with_jobs(Arc::new(FolderHost {
            root: dir.join("share"),
            jobs_dir: dir.join("hjobs"),
        }));
    }
    let mut tries = 0;
    let l = loop {
        match tokio::net::TcpListener::bind(bind).await {
            Ok(l) => break l,
            Err(e) if tries < 40 => {
                tries += 1;
                let _ = e;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("cannot bind {bind}: {e}"),
        }
    };
    let addr = l.local_addr().unwrap().to_string();
    (tokio::spawn(server::serve(l, Arc::new(ctx))), addr)
}

fn engine_pool(dir: &Path, addr: &str) -> (Pool, [u8; 32]) {
    let ava = dir.join("ava");
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    (Pool::new(ava).with_addr(addr), me.public())
}

/// A pool and a host serving `dir/share` on a fresh port.
async fn host(dir: &Path) -> (Pool, String) {
    let key = Identity::load_or_create(&dir.join("ava").join("identity"))
        .unwrap()
        .public();
    let (_, addr) = serve_host(dir, key, "127.0.0.1:0", node_info(), true).await;
    let (pool, _) = engine_pool(dir, &addr);
    (pool, addr)
}

fn counters() -> Counters {
    Counters {
        bytes: Arc::default(),
        files: Arc::default(),
        files_finalized: Arc::default(),
        bytes_finalized: Arc::default(),
        total: Some(Arc::default()),
    }
}

async fn local(
    pool: Arc<Pool>,
    src: &str,
    kind: DownloadKind,
    dest: &Path,
    c: Counters,
    id: u8,
) -> anyhow::Result<u64> {
    let (src, dest) = (src.to_string(), dest.to_path_buf());
    within(
        90,
        tokio::task::spawn_blocking(move || {
            download::to_local_in(
                &pool, "console", &src, kind, &dest, false, [id; 16], &c, None,
            )
        }),
    )
    .await
    .unwrap()
}

async fn zipped(
    pool: Arc<Pool>,
    src: &str,
    kind: DownloadKind,
    dest: &Path,
    c: Counters,
    id: u8,
) -> anyhow::Result<u64> {
    let (src, dest) = (src.to_string(), dest.to_path_buf());
    within(
        90,
        tokio::task::spawn_blocking(move || {
            download::to_zip_in(
                &pool, "console", &src, kind, &dest, false, [id; 16], &c, None,
            )
        }),
    )
    .await
    .unwrap()
}

fn zip_entries(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut out = BTreeMap::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).unwrap();
        assert!(
            out.insert(f.name().to_string(), buf).is_none(),
            "duplicate entry"
        );
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_downloads_and_matches_the_source() {
    let d = temp("folder");
    let total = tree(&d.join("share/Game"), 300, |i| 1 + (i % 7) * 3000);
    let (pool, _) = host(&d).await;
    let out = d.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let c = counters();
    let n = local(
        Arc::new(pool),
        "Game",
        DownloadKind::Folder,
        &out,
        c.clone(),
        1,
    )
    .await
    .unwrap();
    assert_eq!(n, total);
    assert_eq!(files_of(&out.join("Game")), files_of(&d.join("share/Game")));
    assert_eq!(c.bytes.load(Ordering::Relaxed), total);
    assert_eq!(c.bytes_finalized.load(Ordering::Relaxed), total);
    assert_eq!(c.files.load(Ordering::Relaxed), 300);
    assert_eq!(
        c.total.unwrap().load(Ordering::Relaxed),
        total,
        "the dynamic total comes from the manifest"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_holding_exactly_one_file_is_still_a_folder() {
    let d = temp("onefile");
    std::fs::create_dir_all(d.join("share/Game")).unwrap();
    std::fs::write(d.join("share/Game/only.bin"), b"content").unwrap();
    let (pool, _) = host(&d).await;
    let out = d.join("out");
    std::fs::create_dir_all(&out).unwrap();
    local(
        Arc::new(pool),
        "Game",
        DownloadKind::Folder,
        &out,
        counters(),
        2,
    )
    .await
    .unwrap();
    assert!(out.join("Game").is_dir(), "the folder stays a folder");
    assert_eq!(
        std::fs::read(out.join("Game/only.bin")).unwrap(),
        b"content"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_single_file_downloads_to_its_basename() {
    let d = temp("single");
    std::fs::create_dir_all(d.join("share/dir")).unwrap();
    let data = bytes_for(3, 20 << 20);
    std::fs::write(d.join("share/dir/big.pkg"), &data).unwrap();
    let (pool, _) = host(&d).await;
    let out = d.join("out");
    std::fs::create_dir_all(&out).unwrap();
    local(
        Arc::new(pool),
        "dir/big.pkg",
        DownloadKind::File,
        &out,
        counters(),
        3,
    )
    .await
    .unwrap();
    assert!(std::fs::read(out.join("big.pkg")).unwrap() == data);
    let names: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["big.pkg"], "no part file or extra entry is left");
}

/// Floors measured like the ava1-ctest tiny-download test: the per-file drive flush
/// measured 56 files/s on a Mac; a healthy debug build does several hundred.
const ZIP_FLOOR_FILES_PER_S: f64 = if cfg!(debug_assertions) { 150.0 } else { 400.0 };

#[tokio::test(flavor = "multi_thread")]
async fn two_thousand_tiny_files_zip_fast() {
    let d = temp("zipperf");
    let root = d.join("share/Tiny");
    tree(&root, 2000, |i| 1024 + (i * 977) % (63 * 1024));
    let (pool, _) = host(&d).await;
    let pool = Arc::new(pool);
    let out = d.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let t = std::time::Instant::now();
    zipped(
        pool,
        "Tiny",
        DownloadKind::Folder,
        &out.join("t.zip"),
        counters(),
        0x71,
    )
    .await
    .unwrap();
    let secs = t.elapsed().as_secs_f64();
    let rate = 2000.0 / secs;
    eprintln!("tiny zip: 2000 files in {secs:.2}s = {rate:.0} files/s");
    assert_eq!(zip_entries(&out.join("t.zip")).len(), 2000);
    assert!(
        rate >= ZIP_FLOOR_FILES_PER_S,
        "{rate:.0} files/s < {ZIP_FLOOR_FILES_PER_S}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_downloads_into_a_zip() {
    let d = temp("zip");
    let root = d.join("share/Game");
    tree(&root, 40, |i| (i % 5) * 20_000);
    std::fs::create_dir_all(root.join("empty-dir")).unwrap();
    std::fs::write(d.join("share/foo.pkg"), bytes_for(9, 70_000)).unwrap();
    let (pool, _) = host(&d).await;
    let pool = Arc::new(pool);
    let out = d.join("out");
    std::fs::create_dir_all(&out).unwrap();

    // The tree download creates the empty directory; the zip does not contain it.
    local(
        pool.clone(),
        "Game",
        DownloadKind::Folder,
        &out,
        counters(),
        4,
    )
    .await
    .unwrap();
    assert!(out.join("Game/empty-dir").is_dir());
    zipped(
        pool.clone(),
        "Game",
        DownloadKind::Folder,
        &out.join("g.zip"),
        counters(),
        5,
    )
    .await
    .unwrap();
    let got = zip_entries(&out.join("g.zip"));
    let want: BTreeMap<String, Vec<u8>> = files_of(&root)
        .into_iter()
        .map(|(k, v)| (format!("Game/{k}"), v))
        .collect();
    assert_eq!(got, want, "entries are <basename>/<path>, bytes identical");
    assert!(
        got.keys().all(|k| !k.contains("empty-dir")),
        "an empty directory is not an entry"
    );
    assert!(!out.join("g.zip.ava-part").exists());

    // A single file's entry is exactly its basename, not foo.pkg/foo.pkg.
    zipped(
        pool,
        "foo.pkg",
        DownloadKind::File,
        &out.join("f.zip"),
        counters(),
        6,
    )
    .await
    .unwrap();
    let got = zip_entries(&out.join("f.zip"));
    assert_eq!(got.keys().collect::<Vec<_>>(), ["foo.pkg"]);
    assert!(got["foo.pkg"] == bytes_for(9, 70_000));
}

/// A proxy in front of a host that can be killed and replaced at the same address.
struct Flaky {
    proxy: ChaosProxy,
    host_addr: String,
    host: tokio::task::JoinHandle<()>,
    key: [u8; 32],
}

impl Flaky {
    async fn start(d: &Path) -> (Flaky, Pool) {
        Self::start_with(
            d,
            ChaosConfig {
                bytes_per_sec: Some(2 << 20),
                ..Default::default()
            },
        )
        .await
    }

    async fn start_with(d: &Path, cfg: ChaosConfig) -> (Flaky, Pool) {
        let key = Identity::load_or_create(&d.join("ava").join("identity"))
            .unwrap()
            .public();
        let (host, host_addr) = serve_host(d, key, "127.0.0.1:0", node_info(), true).await;
        let proxy = ChaosProxy::start(host_addr.parse().unwrap(), cfg)
            .await
            .unwrap();
        let (pool, _) = engine_pool(d, &proxy.addr.to_string());
        (
            Flaky {
                proxy,
                host_addr,
                host,
                key,
            },
            pool,
        )
    }

    /// Kills the host and every connection to it, then brings the same console (same
    /// identity, same address, same share) back.
    async fn restart(&mut self, d: &Path) {
        self.host.abort();
        self.proxy.kill_all();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (h, _) = serve_host(d, self.key, &self.host_addr, node_info(), true).await;
        self.host = h;
    }
}

/// Fires `restart` once `bytes` shows durable progress.
async fn restart_midway(mut f: Flaky, d: PathBuf, bytes: Arc<AtomicU64>, at: u64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while bytes.load(Ordering::Relaxed) < at {
        assert!(
            std::time::Instant::now() < deadline,
            "no progress to interrupt"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    f.restart(&d).await;
    // Keep the proxy and host alive until the test drops them.
    std::future::pending::<()>().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_survives_a_host_restart() {
    let d = temp("restart");
    let total = tree(&d.join("share/Game"), 24, |_| 1 << 20);
    let (flaky, pool) = Flaky::start(&d).await;
    let pool = Arc::new(pool);
    let out = d.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let c = counters();
    let killer = tokio::spawn(restart_midway(
        flaky,
        d.clone(),
        c.bytes_finalized.clone(),
        total / 2,
    ));
    let n = local(
        pool.clone(),
        "Game",
        DownloadKind::Folder,
        &out,
        c.clone(),
        7,
    )
    .await
    .unwrap();
    killer.abort();
    assert_eq!(n, total);
    assert_eq!(files_of(&out.join("Game")), files_of(&d.join("share/Game")));
    assert!(pool.attempts() >= 2, "the pool never reconnected");
    // Resumed from the journal, not restarted: the work counter never needed more than
    // the file plus one window.
    assert!(c.bytes.load(Ordering::Relaxed) <= total + (64 << 20));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deflate_zip_download_restarts_a_fresh_archive_after_a_drop() {
    let d = temp("zip-restart");
    let total = tree(&d.join("share/Game"), 24, |_| 1 << 20);
    let (flaky, pool) = Flaky::start(&d).await;
    let pool = Arc::new(pool);
    let out = d.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let c = counters();
    let killer = tokio::spawn(restart_midway(
        flaky,
        d.clone(),
        c.bytes_finalized.clone(),
        total / 2,
    ));
    // The counter must never step backwards, across the restart included.
    let (sample, stop) = (c.bytes.clone(), Arc::new(AtomicBool::new(false)));
    let (stop2, watcher) = (
        stop.clone(),
        std::thread::spawn({
            let stop = stop.clone();
            move || {
                let mut last = 0;
                while !stop.load(Ordering::Relaxed) {
                    let now = sample.load(Ordering::Relaxed);
                    assert!(now >= last, "progress went backwards: {last} -> {now}");
                    last = now;
                    std::thread::sleep(Duration::from_millis(2));
                }
                last
            }
        }),
    );
    let dest = out.join("g.zip");
    let n = {
        let (pool, dest, c) = (pool.clone(), dest.clone(), c.clone());
        within(
            90,
            tokio::task::spawn_blocking(move || {
                download::to_zip_with_in(
                    &pool,
                    "console",
                    "Game",
                    DownloadKind::Folder,
                    &dest,
                    false,
                    ZipCompression::Deflate,
                    [8; 16],
                    &c,
                    None,
                )
            }),
        )
        .await
        .unwrap()
        .unwrap()
    };
    stop2.store(true, Ordering::Relaxed);
    watcher.join().unwrap();
    killer.abort();
    assert_eq!(n, total, "the final attempt's own byte count");
    let want: BTreeMap<String, Vec<u8>> = files_of(&d.join("share/Game"))
        .into_iter()
        .map(|(k, v)| (format!("Game/{k}"), v))
        .collect();
    assert!(
        zip_entries(&dest) == want,
        "the archive holds exactly one run's bytes"
    );
    assert!(pool.attempts() >= 2);
    // Review 019 F3: the work of the discarded attempt is counted, but what is shown never
    // passes the whole archive.
    assert_eq!(
        c.bytes.load(Ordering::Relaxed),
        total,
        "the counter is capped at the archive's own size"
    );
    assert!(!out.join("g.zip.ava-part").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stored_zip_download_resumes_after_a_host_restart() {
    let d = temp("zip-resume");
    let total = tree(&d.join("share/Game"), 24, |_| 1 << 20);
    let (flaky, pool) = Flaky::start(&d).await;
    let pool = Arc::new(pool);
    let out = d.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let c = counters();
    let killer = tokio::spawn(restart_midway(
        flaky,
        d.clone(),
        c.bytes_finalized.clone(),
        total / 2,
    ));
    let dest = out.join("g.zip");
    let n = zipped(
        pool.clone(),
        "Game",
        DownloadKind::Folder,
        &dest,
        c.clone(),
        10,
    )
    .await
    .unwrap();
    killer.abort();
    assert_eq!(n, total);
    assert!(pool.attempts() >= 2, "the pool never reconnected");
    // Resumed, not restarted: a restart adds the discarded attempt to the work counter.
    assert_eq!(c.bytes.load(Ordering::Relaxed), total);
    let want: BTreeMap<String, Vec<u8>> = files_of(&d.join("share/Game"))
        .into_iter()
        .map(|(k, v)| (format!("Game/{k}"), v))
        .collect();
    assert!(zip_entries(&dest) == want);
    assert!(!out.join("g.zip.ava-part").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_download_stops_before_connecting() {
    let d = temp("cancel-dl");
    let (pool, _) = host(&d).await;
    let flag = Arc::new(AtomicBool::new(true));
    let (pool, dest) = (Arc::new(pool), d.join("o.zip"));
    let e = within(
        20,
        tokio::task::spawn_blocking(move || {
            download::to_zip_in(
                &pool,
                "c",
                "x",
                DownloadKind::File,
                &dest,
                false,
                [1; 16],
                &Counters::default(),
                Some(flag),
            )
        }),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(e.to_string(), "transfer_cancelled");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_source_is_a_typed_refusal_not_a_retry() {
    let d = temp("refused");
    let (pool, _) = host(&d).await;
    std::fs::create_dir_all(d.join("out")).unwrap();
    // `..` leaves the shared folder: the host refuses with ERR_PATH.
    let e = local(
        Arc::new(pool),
        "../x",
        DownloadKind::Folder,
        &d.join("out"),
        counters(),
        9,
    )
    .await
    .unwrap_err();
    let f = e.downcast_ref::<UploadFailure>().expect("a typed failure");
    assert_eq!(f.reason, "ava1_not_allowed");
}

fn file_entry(path: &str, size: u64) -> Entry {
    Entry {
        kind: gen::ENTRY_FILE,
        mode: 0o644,
        size,
        mtime: 0,
        path: path.into(),
        root: None,
    }
}

#[test]
fn the_zip_sink_refuses_a_gap_an_overlap_and_a_reorder() {
    let d = temp("zipsink");
    let m = Manifest {
        entries: vec![
            file_entry("a", 100),
            file_entry("b", 50),
            file_entry("e", 0),
        ],
    };
    let s = ZipSink::new(d.join("o.zip"), "Pfx");
    s.prepare(&m).unwrap();
    s.write_at(0, 0, &[1; 60]).unwrap();
    assert!(s.write_at(0, 70, &[1; 10]).is_err(), "a gap is refused");
    assert!(
        s.write_at(0, 50, &[1; 10]).is_err(),
        "an overlap is refused"
    );
    s.write_at(0, 60, &[1; 40]).unwrap();
    assert!(s.write_at(0, 100, &[1]).is_err(), "past the entry's size");
    s.write_at(1, 0, &[2; 50]).unwrap();
    assert!(
        s.write_at(0, 0, &[1; 100]).is_err(),
        "a finished entry cannot restart"
    );
    s.write_whole(2, &[]).unwrap(); // the receiver's up-front empty-file write
    s.finish().unwrap();
    let got = zip_entries(&d.join("o.zip"));
    assert_eq!(
        got.keys().cloned().collect::<Vec<_>>(),
        ["Pfx/a", "Pfx/b", "Pfx/e"]
    );
    assert!(got["Pfx/e"].is_empty());

    // An archive missing a file never reaches its final name.
    let s = ZipSink::new(d.join("short.zip"), "Pfx");
    s.prepare(&m).unwrap();
    s.write_at(0, 0, &[1; 100]).unwrap();
    assert!(s.finish().is_err());
    assert!(!d.join("short.zip").exists());
}

// ---- console copy / move ---------------------------------------------------------

#[derive(Default)]
struct Fake {
    /// Every method received, in order.
    calls: Vec<u16>,
    copy: Option<JobCopy>,
    issued: bool,
    /// While false the job reports running at 1000 of 2000 bytes.
    release: bool,
    /// What a finished job reports: 1 done, 2 failed.
    end_state: u8,
    /// Answer `job.copy` with this status instead of accepting.
    refuse_with: Option<u16>,
    /// Forget the job once (a console restart) after this many status polls.
    forget_after: Option<u32>,
    polls: u32,
    copies: u32,
    /// Release the job once this many copies have been issued (0 = never).
    release_at: u32,
    /// Hold every `job.status` reply for this long (a console that has wedged).
    hang_status: Duration,
}

fn fake_rpc(f: Arc<Mutex<Fake>>) -> RpcHandler {
    let base = node_info();
    Box::new(move |method, body| {
        if method == gen::METHOD_NODE_INFO {
            return base(method, body);
        }
        let hang = f.lock().unwrap().hang_status;
        if method == gen::METHOD_JOB_STATUS && !hang.is_zero() {
            std::thread::sleep(hang);
        }
        let mut f = f.lock().unwrap();
        f.calls.push(method);
        let status = |f: &Fake, id: [u8; 16]| {
            let state = if f.release { f.end_state } else { 0 };
            Status {
                job_id: id,
                bytes_durable: if f.release { 2000 } else { 1000 },
                bytes_received: 1000,
                bytes_total: 2000,
                state: Some(state),
                current: (state == 2).then(|| "disk full".to_string()),
                ..Default::default()
            }
            .to_bytes()
            .unwrap()
        };
        match method {
            gen::METHOD_JOB_COPY => {
                if let Some(s) = f.refuse_with {
                    return RpcReply {
                        status: s,
                        body: vec![],
                    };
                }
                let c = JobCopy::decode(body).unwrap();
                f.copies += 1;
                f.issued = true;
                if f.release_at > 0 && f.copies >= f.release_at {
                    f.release = true;
                }
                let id = c.job_id;
                f.copy = Some(c);
                RpcReply {
                    status: gen::STATUS_OK,
                    body: status(&f, id),
                }
            }
            gen::METHOD_JOB_STATUS => {
                let id = JobRef::decode(body).unwrap().job_id;
                f.polls += 1;
                if f.forget_after.is_some_and(|n| f.polls > n) {
                    f.forget_after = None;
                    f.issued = false;
                }
                if !f.issued {
                    return RpcReply {
                        status: gen::ERR_UNKNOWN_JOB,
                        body: vec![],
                    };
                }
                RpcReply {
                    status: gen::STATUS_OK,
                    body: status(&f, id),
                }
            }
            gen::METHOD_JOB_CANCEL => {
                f.issued = false;
                RpcReply {
                    status: gen::STATUS_OK,
                    body: vec![],
                }
            }
            _ => RpcReply {
                status: gen::ERR_UNKNOWN_METHOD,
                body: vec![],
            },
        }
    })
}

async fn fake_pool(d: &Path, fake: Arc<Mutex<Fake>>) -> Arc<Pool> {
    let key = Identity::load_or_create(&d.join("ava").join("identity"))
        .unwrap()
        .public();
    let (_, addr) = serve_host(d, key, "127.0.0.1:0", fake_rpc(fake), false).await;
    Arc::new(engine_pool(d, &addr).0)
}

/// No console to reach in these tests: the cleanup the engine would run is recorded instead.
struct NoCleanup;
impl ps5upload_ava1::copy::CancelCleanup for NoCleanup {
    fn dest_absent(&self, _: &str, _: &str) -> bool {
        true
    }
    fn clean(&self, _: &str, _: &str, _: bool) {}
}

#[derive(Default)]
struct Recorder(Mutex<Vec<(String, bool)>>);
impl ps5upload_ava1::copy::CancelCleanup for Recorder {
    fn dest_absent(&self, _: &str, _: &str) -> bool {
        true
    }
    fn clean(&self, _: &str, to: &str, absent: bool) {
        self.0.lock().unwrap().push((to.to_string(), absent));
    }
}

async fn run_copy(pool: Arc<Pool>, op: u64, mv: bool, overwrite: bool) -> anyhow::Result<()> {
    within(
        60,
        tokio::task::spawn_blocking(move || {
            console_copy_with(
                &pool, &NoCleanup, "c", "/data/a", "/data/b", op, mv, overwrite,
            )
        }),
    )
    .await
    .unwrap()
}

async fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(
            std::time::Instant::now() < deadline,
            "never happened: {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_console_copy_polls_status_until_done() {
    let d = temp("copy");
    let fake = Arc::new(Mutex::new(Fake {
        end_state: 1,
        ..Default::default()
    }));
    let pool = fake_pool(&d, fake.clone()).await;
    let job = tokio::spawn(run_copy(pool, 7001, false, true));
    wait_until("the snapshot shows progress", || {
        op_snapshot(7001).is_some_and(|s| s.bytes_copied == 1000)
    })
    .await;
    let s = op_snapshot(7001).unwrap();
    assert_eq!(
        (
            s.kind.as_str(),
            s.from.as_str(),
            s.to.as_str(),
            s.total_bytes
        ),
        ("copy", "/data/a", "/data/b", 2000)
    );
    assert!(s.found && !s.cancel_requested);
    fake.lock().unwrap().release = true;
    job.await.unwrap().unwrap();
    assert!(op_snapshot(7001).is_none(), "the registry entry is gone");
    let f = fake.lock().unwrap();
    assert_eq!(f.copies, 1, "issued once");
    let c = f.copy.as_ref().unwrap();
    assert_eq!((c.src.as_str(), c.dest.as_str()), ("/data/a", "/data/b"));
    assert_eq!(
        c.flags,
        gen::JF_OVERWRITE,
        "a copy that may overwrite says so"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_move_without_overwrite_sends_move_and_never_overwrite() {
    let d = temp("move");
    let fake = Arc::new(Mutex::new(Fake {
        end_state: 1,
        release: true,
        ..Default::default()
    }));
    let pool = fake_pool(&d, fake.clone()).await;
    run_copy(pool, 7002, true, false).await.unwrap();
    assert_eq!(
        fake.lock().unwrap().copy.as_ref().unwrap().flags,
        gen::JF_MOVE
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_console_copy_cancel_signals_the_job() {
    let d = temp("copy-cancel");
    let fake = Arc::new(Mutex::new(Fake {
        end_state: 1,
        ..Default::default()
    }));
    let pool = fake_pool(&d, fake.clone()).await;
    let job = tokio::spawn(run_copy(pool, 7003, false, false));
    wait_until("the copy is running", || {
        op_snapshot(7003).is_some_and(|s| s.bytes_copied == 1000)
    })
    .await;
    assert!(op_cancel(7003));
    assert!(op_snapshot(7003).is_some_and(|s| s.cancel_requested) || op_snapshot(7003).is_none());
    let e = job.await.unwrap().unwrap_err();
    assert_eq!(
        e.to_string(),
        "cancelled",
        "the engine maps this exact text to 409"
    );
    assert!(fake.lock().unwrap().calls.contains(&gen::METHOD_JOB_CANCEL));
    assert!(op_snapshot(7003).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_copy_clears_its_leftovers_and_a_finished_one_does_not() {
    let d = temp("copy-cancel-clean");
    let fake = Arc::new(Mutex::new(Fake {
        end_state: 1,
        ..Default::default()
    }));
    let pool = fake_pool(&d, fake.clone()).await;
    let rec = Arc::new(Recorder::default());
    let (p, r) = (pool.clone(), rec.clone());
    let job = tokio::task::spawn_blocking(move || {
        console_copy_with(&p, &*r, "c", "/data/a", "/data/b", 7020, false, true)
    });
    wait_until("the copy is running", || {
        op_snapshot(7020).is_some_and(|s| s.bytes_copied == 1000)
    })
    .await;
    assert!(op_cancel(7020));
    assert_eq!(job.await.unwrap().unwrap_err().to_string(), "cancelled");
    // The job was told to stop, then the destination's own leftovers were cleared, once.
    assert!(fake.lock().unwrap().calls.contains(&gen::METHOD_JOB_CANCEL));
    assert_eq!(*rec.0.lock().unwrap(), [("/data/b".to_string(), true)]);

    // A copy that finishes (or fails) clears nothing.
    let fake = Arc::new(Mutex::new(Fake {
        end_state: 1,
        release: true,
        ..Default::default()
    }));
    let pool = fake_pool(&d, fake).await;
    let (p, r) = (pool.clone(), rec.clone());
    tokio::task::spawn_blocking(move || {
        console_copy_with(&p, &*r, "c", "/data/a", "/data/c", 7021, false, true)
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rec.0.lock().unwrap().len(), 1, "no cleanup after success");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_does_not_wait_on_a_status_call_the_console_never_answers() {
    let d = temp("copy-hang");
    let fake = Arc::new(Mutex::new(Fake {
        end_state: 1,
        ..Default::default()
    }));
    let pool = fake_pool(&d, fake.clone()).await;
    let job = tokio::spawn(run_copy(pool, 7010, false, false));
    wait_until("the copy is running", || {
        op_snapshot(7010).is_some_and(|s| s.bytes_copied == 1000)
    })
    .await;
    fake.lock().unwrap().hang_status = Duration::from_secs(6);
    tokio::time::sleep(Duration::from_millis(600)).await; // the next poll is now hanging
    let t = std::time::Instant::now();
    assert!(op_cancel(7010));
    let e = job.await.unwrap().unwrap_err();
    // The console never confirmed it let go, so this is not a clean cancel (final review #2).
    assert!(
        e.to_string().contains("nothing was deleted"),
        "unconfirmed cancel: {e}"
    );
    assert!(
        t.elapsed() < Duration::from_secs(6),
        "the cancel waited {:?} on a hung call",
        t.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forgotten_job_is_reissued_and_a_failed_one_is_terminal() {
    let d = temp("copy-reissue");
    let fake = Arc::new(Mutex::new(Fake {
        end_state: 1,
        release_at: 2,
        forget_after: Some(0),
        ..Default::default()
    }));
    let pool = fake_pool(&d, fake.clone()).await;
    // The first status poll finds the job gone (a console restart): it is issued again.
    run_copy(pool, 7004, false, false).await.unwrap();
    assert_eq!(fake.lock().unwrap().copies, 2);

    let d = temp("copy-failed");
    let fake = Arc::new(Mutex::new(Fake {
        end_state: 2,
        release: true,
        ..Default::default()
    }));
    let pool = fake_pool(&d, fake.clone()).await;
    let e = run_copy(pool, 7005, false, false).await.unwrap_err();
    let f = e.downcast_ref::<UploadFailure>().unwrap();
    assert_eq!(f.reason, "ava1_copy_failed");
    assert!(f.detail.contains("disk full"), "{}", f.detail);
    assert_eq!(
        fake.lock().unwrap().copies,
        1,
        "a failed copy is not re-run"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_copy_keeps_its_reason_and_a_taken_op_id_is_refused() {
    let d = temp("copy-refused");
    let fake = Arc::new(Mutex::new(Fake {
        refuse_with: Some(gen::ERR_EXISTS),
        ..Default::default()
    }));
    let pool = fake_pool(&d, fake.clone()).await;
    let e = run_copy(pool.clone(), 7006, false, false)
        .await
        .unwrap_err();
    let f = e.downcast_ref::<UploadFailure>().unwrap();
    assert_eq!(f.reason, "ava1_exists");
    assert!(
        f.detail.contains("fs_copy_dest_exists"),
        "the client's prompt token survives: {}",
        f.detail
    );
    assert!(op_snapshot(7006).is_none(), "an error also frees the entry");

    // A second registration of a live op id is refused, not silently shared.
    fake.lock().unwrap().refuse_with = None;
    fake.lock().unwrap().end_state = 1;
    let first = tokio::spawn(run_copy(pool.clone(), 7007, false, false));
    wait_until("the first copy registered", || op_snapshot(7007).is_some()).await;
    let e = run_copy(pool, 7007, false, false).await.unwrap_err();
    assert!(e.to_string().contains("already running"), "{e}");
    op_cancel(7007);
    let _ = first.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn record_status_maps_a_move() {
    let d = temp("copy-maps");
    let fake = Arc::new(Mutex::new(Fake {
        end_state: 1,
        ..Default::default()
    }));
    let pool = fake_pool(&d, fake).await;
    let job = tokio::spawn(run_copy(pool, 7008, true, false));
    wait_until("the move shows progress", || {
        op_snapshot(7008).is_some_and(|s| s.bytes_copied == 1000)
    })
    .await;
    let s = op_snapshot(7008).unwrap();
    assert_eq!((s.kind.as_str(), s.total_bytes), ("move", 2000));
    op_cancel(7008);
    let _ = job.await.unwrap();
    // `record_status` and the lookups never claim an id this module does not own.
    record_status(
        99,
        &Status {
            bytes_durable: 5,
            bytes_total: 9,
            ..Default::default()
        },
    );
    assert!(op_snapshot(99).is_none());
    assert!(!op_cancel(99));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_download_removes_its_staging_and_journal() {
    let d = temp("cancel-mid");
    let total = tree(&d.join("share/Game"), 24, |_| 1 << 20);
    let (_flaky, pool) = Flaky::start(&d).await;
    let pool = Arc::new(pool);
    let out = d.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let c = counters();
    let flag = Arc::new(AtomicBool::new(false));
    let (p2, o2, c2, f2) = (pool.clone(), out.clone(), c.clone(), flag.clone());
    let run = tokio::task::spawn_blocking(move || {
        download::to_local_in(
            &p2,
            "console",
            "Game",
            DownloadKind::Folder,
            &o2,
            false,
            [31; 16],
            &c2,
            Some(f2),
        )
    });
    within(30, async {
        while c.bytes.load(Ordering::Relaxed) < (3 << 20) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(c.bytes.load(Ordering::Relaxed) < total, "finished too soon");
    assert!(out.join("Game.ava-part").exists(), "mid-flight staging");
    flag.store(true, Ordering::Relaxed);
    let e = within(30, run).await.unwrap().unwrap_err();
    assert_eq!(e.to_string(), "transfer_cancelled");
    assert!(!out.join("Game.ava-part").exists(), "staging left behind");
    assert!(!out.join("Game").exists());
    let jobs = d.join("ava/jobs");
    let left = std::fs::read_dir(&jobs).map(|r| r.count()).unwrap_or(0);
    assert_eq!(left, 0, "the journal of a dead job was kept");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_after_a_failed_one_has_no_stale_files() {
    let d = temp("stale");
    tree(&d.join("share/Game"), 6, |_| 70_000);
    let (pool, _) = host(&d).await;
    let pool = Arc::new(pool);
    let out = d.join("out");
    // What a crashed earlier download of a different manifest left behind.
    std::fs::create_dir_all(out.join("Game.ava-part/old")).unwrap();
    std::fs::write(out.join("Game.ava-part/old/stale"), b"stale").unwrap();
    std::fs::write(out.join("Game.ava-part/f0"), b"half a file").unwrap();
    local(
        pool.clone(),
        "Game",
        DownloadKind::Folder,
        &out,
        counters(),
        32,
    )
    .await
    .unwrap();
    assert_eq!(files_of(&out.join("Game")), files_of(&d.join("share/Game")));
    assert!(!out.join("Game.ava-part").exists());
    // A single file: a stale part file is not the start of this download.
    std::fs::write(d.join("share/one.bin"), bytes_for(3, 50_000)).unwrap();
    std::fs::write(out.join("one.bin.ava-part"), vec![9u8; 90_000]).unwrap();
    local(pool, "one.bin", DownloadKind::File, &out, counters(), 33)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(out.join("one.bin")).unwrap(),
        bytes_for(3, 50_000)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn zip_entries_of_several_groups_download_intact() {
    let d = temp("zip-groups");
    let root = d.join("share/Game");
    std::fs::create_dir_all(&root).unwrap();
    // 1 MiB groups: 3 MiB and 2.5 MiB entries take the outboard (multi-group) path.
    std::fs::write(root.join("a_big"), bytes_for(1, 3 << 20)).unwrap();
    std::fs::write(root.join("b_mid"), bytes_for(2, (5 << 20) / 2)).unwrap();
    std::fs::write(root.join("c_small"), bytes_for(3, 4_000)).unwrap();
    let (pool, _) = host(&d).await;
    let out = d.join("out");
    std::fs::create_dir_all(&out).unwrap();
    zipped(
        Arc::new(pool),
        "Game",
        DownloadKind::Folder,
        &out.join("g.zip"),
        counters(),
        34,
    )
    .await
    .unwrap();
    let got = zip_entries(&out.join("g.zip"));
    let want: BTreeMap<String, Vec<u8>> = files_of(&root)
        .into_iter()
        .map(|(k, v)| (format!("Game/{k}"), v))
        .collect();
    assert_eq!(got.len(), 3);
    assert!(got == want, "multi-group zip entries differ");
}

/// A host that answers the first `busy` JobOpens `ERR_BUSY` and serves the rest like `FolderHost`.
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
                                message: Some("too many jobs; try again".into()),
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

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_download_open_is_retried_and_a_console_that_stays_busy_fails_clearly() {
    for (busy, tries, id) in [(2u32, 12u32, 0x31u8), (u32::MAX, 2, 0x32)] {
        let d = temp(&format!("dl-busy-{id}"));
        let src = d.join("share/src");
        std::fs::create_dir_all(&src).unwrap();
        tree(&src, 12, |_| 2048);
        let key = Identity::load_or_create(&d.join("ava").join("identity"))
            .unwrap()
            .public();
        let mut peers = PeerStore::in_memory();
        peers.add(key, "engine").unwrap();
        let h = Arc::new(BusyHost {
            inner: FolderHost {
                root: d.join("share"),
                jobs_dir: d.join("hjobs"),
            },
            busy: busy.into(),
            seen: 0.into(),
        });
        let ctx = ServerCtx::new(Identity::generate().unwrap(), "host", peers, node_info())
            .with_jobs(h.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(server::serve(l, Arc::new(ctx)));
        let pool = Arc::new(
            Pool::new(d.join("ava"))
                .with_addr(addr)
                .with_busy_tries(tries),
        );
        let dest = d.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let r = local(pool, "src", DownloadKind::Folder, &dest, counters(), id).await;
        if busy == 2 {
            r.expect("the download completes after two BUSY answers");
            assert_eq!(h.seen.load(Ordering::SeqCst), 3);
            assert_eq!(files_of(&src), files_of(&dest.join("src")));
        } else {
            let e = r.unwrap_err();
            let f = e
                .downcast_ref::<UploadFailure>()
                .unwrap_or_else(|| panic!("{e:#}"));
            assert_eq!(f.reason, "ava1_busy", "{f:?}");
            assert_eq!(
                h.seen.load(Ordering::SeqCst),
                3,
                "the first try and two retries"
            );
        }
    }
}

/// Hardware run 2026-10-04 (drop60 rejoin livelock), download side: a link that is cut on a
/// short period must still end byte-exact, because every drop is followed by a prompt
/// reconnect, not by the top of the backoff ladder.
#[tokio::test(flavor = "multi_thread")]
async fn periodic_kills_never_strand_a_download() {
    let d = temp("periodic-kill");
    let total = tree(&d.join("share/Game"), 1, |_| 24 << 20);
    let (_flaky, pool) = Flaky::start_with(
        &d,
        ChaosConfig {
            bytes_per_sec: Some(8 << 20),
            kill_every: Some(Duration::from_millis(2500)),
            ..Default::default()
        },
    )
    .await;
    let pool = Arc::new(pool);
    let out = d.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let c = counters();
    let n = within(
        90,
        local(
            pool.clone(),
            "Game",
            DownloadKind::Folder,
            &out,
            c.clone(),
            11,
        ),
    )
    .await
    .unwrap();
    assert_eq!(n, total);
    assert_eq!(files_of(&out.join("Game")), files_of(&d.join("share/Game")));
}

// ---- final review #2, #3, #10: cancel and loss never delete what is not ours ------------

/// A console behind a proxy that can vanish for good (the process dies, the cable is pulled).
async fn vanishing_console(d: &Path, fake: Arc<Mutex<Fake>>) -> (Arc<Pool>, impl FnOnce()) {
    let key = Identity::load_or_create(&d.join("ava").join("identity"))
        .unwrap()
        .public();
    let (host, host_addr) = serve_host(d, key, "127.0.0.1:0", fake_rpc(fake), false).await;
    let proxy = ChaosProxy::start(host_addr.parse().unwrap(), ChaosConfig::default())
        .await
        .unwrap();
    let (pool, _) = engine_pool(d, &proxy.addr.to_string());
    let gone = move || {
        host.abort();
        proxy.kill_all();
        // Keep the proxy bound but dead upstream: new connections are cut at once.
        std::mem::forget(proxy);
    };
    (Arc::new(pool), gone)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_that_never_reaches_the_console_deletes_nothing() {
    let d = temp("copy-cancel-lost");
    let fake = Arc::new(Mutex::new(Fake {
        end_state: 1,
        ..Default::default()
    }));
    let (pool, gone) = vanishing_console(&d, fake.clone()).await;
    let rec = Arc::new(Recorder::default());
    let (p, r) = (pool.clone(), rec.clone());
    let job = tokio::task::spawn_blocking(move || {
        console_copy_with(&p, &*r, "c", "/data/a", "/data/b", 7030, true, true)
    });
    wait_until("the move is running", || {
        op_snapshot(7030).is_some_and(|s| s.bytes_copied == 1000)
    })
    .await;
    gone();
    assert!(op_cancel(7030));
    let e = within(60, job).await.unwrap().unwrap_err();
    let msg = e.to_string();
    assert!(
        msg.contains("may still be finishing") && msg.contains("nothing was deleted"),
        "{msg}"
    );
    assert!(msg.contains("source is untouched"), "a move says so: {msg}");
    assert!(
        rec.0.lock().unwrap().is_empty(),
        "no cleanup after an undelivered cancel"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_while_waiting_on_busy_deletes_nothing_and_stops_nothing() {
    let d = temp("copy-cancel-busy");
    let fake = Arc::new(Mutex::new(Fake {
        refuse_with: Some(gen::ERR_BUSY),
        ..Default::default()
    }));
    let pool = fake_pool(&d, fake.clone()).await;
    let rec = Arc::new(Recorder::default());
    let (p, r) = (pool.clone(), rec.clone());
    let job = tokio::task::spawn_blocking(move || {
        console_copy_with(&p, &*r, "c", "/data/a", "/data/b", 7031, false, false)
    });
    wait_until("the first BUSY answer", || {
        fake.lock().unwrap().calls.contains(&gen::METHOD_JOB_COPY)
    })
    .await;
    assert!(op_cancel(7031));
    let e = within(30, job).await.unwrap().unwrap_err();
    assert_eq!(e.to_string(), "cancelled");
    assert!(
        rec.0.lock().unwrap().is_empty(),
        "another job's staging is not ours to delete"
    );
    assert!(
        !fake.lock().unwrap().calls.contains(&gen::METHOD_JOB_CANCEL),
        "no job of ours existed to stop"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_before_the_copy_registers_sticks_and_touches_nothing() {
    let d = temp("copy-cancel-early");
    let fake = Arc::new(Mutex::new(Fake {
        end_state: 1,
        ..Default::default()
    }));
    let pool = fake_pool(&d, fake.clone()).await;
    // The engine has no entry for the op yet: the cancel is remembered, not dropped.
    assert!(!op_cancel(7032));
    let rec = Arc::new(Recorder::default());
    let (p, r) = (pool.clone(), rec.clone());
    let e = within(
        30,
        tokio::task::spawn_blocking(move || {
            console_copy_with(&p, &*r, "c", "/data/a", "/data/b", 7032, false, true)
        }),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(e.to_string(), "cancelled");
    assert!(rec.0.lock().unwrap().is_empty(), "nothing was ever issued");
    assert!(
        fake.lock().unwrap().calls.is_empty()
            || !fake.lock().unwrap().calls.contains(&gen::METHOD_JOB_COPY),
        "the console was never asked to copy"
    );
    // The pending cancel is consumed: the same id, used again, runs normally.
    fake.lock().unwrap().release = true;
    run_copy(pool, 7032, false, true).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_copy_ends_when_the_console_never_comes_back() {
    let d = temp("copy-vanished");
    let fake = Arc::new(Mutex::new(Fake {
        end_state: 1,
        ..Default::default()
    }));
    let (pool, gone) = vanishing_console(&d, fake.clone()).await;
    let (p, t) = (pool.clone(), std::time::Instant::now());
    let job = tokio::task::spawn_blocking(move || {
        console_copy_limited(
            &p,
            &NoCleanup,
            "c",
            "/data/a",
            "/data/b",
            7033,
            false,
            true,
            Duration::from_secs(3),
        )
    });
    wait_until("the copy is running", || {
        op_snapshot(7033).is_some_and(|s| s.bytes_copied == 1000)
    })
    .await;
    gone();
    let e = within(60, job).await.unwrap().unwrap_err();
    assert!(e.to_string().contains("no copy progress"), "{e}");
    assert!(t.elapsed() < Duration::from_secs(40), "{:?}", t.elapsed());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_confirmed_cancel_cleans_up_only_after_the_console_accepted_this_copy() {
    let d = temp("copy-cancel-confirmed");
    let fake = Arc::new(Mutex::new(Fake {
        end_state: 1,
        ..Default::default()
    }));
    let pool = fake_pool(&d, fake.clone()).await;
    let rec = Arc::new(Recorder::default());
    let (p, r) = (pool.clone(), rec.clone());
    let job = tokio::task::spawn_blocking(move || {
        console_copy_with(&p, &*r, "c", "/data/a", "/data/b", 7034, false, false)
    });
    wait_until("the copy is running", || {
        op_snapshot(7034).is_some_and(|s| s.bytes_copied == 1000)
    })
    .await;
    assert!(fake.lock().unwrap().issued, "the console accepted the copy");
    assert!(op_cancel(7034));
    assert_eq!(
        within(30, job).await.unwrap().unwrap_err().to_string(),
        "cancelled"
    );
    assert_eq!(*rec.0.lock().unwrap(), [("/data/b".to_string(), true)]);
}
