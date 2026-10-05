//! "Skip files the console already has" end to end: the adapter the engine calls
//! (`upload_dir_skip_existing_in`) over a real session to the C receiver on loopback,
//! for a local folder, a remote (NAS) source that reports mtimes, and one that does not
//! (SPEC.md §11.4). Each run is a new job (a new upload), as the engine's resume
//! strategy makes it.
#![cfg(unix)]
mod common;

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1_ctest::CServer;
use common::{dir, SECRET};
use ps5upload_ava1::upload::{upload_dir_skip_existing_in, SkipMode};
use ps5upload_ava1::Pool;
use ps5upload_core::source_fs::{ReadSeek, SourceFs, SourceMeta};
use ps5upload_core::transfer::TransferConfig;

const MTIME: u64 = 1_650_000_000;

/// A paired engine pool and the C receiver it talks to.
struct Rig {
    pool: Pool,
    srv: CServer,
    t: PathBuf,
}

fn rig(tag: &str) -> Rig {
    let t = dir(tag);
    let ava = t.join("ava");
    std::fs::create_dir_all(&ava).unwrap();
    let srv_peers = t.join("srv-peers");
    // The pool's identity is trusted by the receiver, and it knows the receiver's key.
    let me = Identity::load_or_create(&ava.join("identity")).unwrap();
    PeerStore::load(&srv_peers)
        .unwrap()
        .add(me.public(), "engine")
        .unwrap();
    PeerStore::load(&ava.join("peers"))
        .unwrap()
        .add(Identity::from_secret(SECRET).public(), "C receiver")
        .unwrap();
    let srv = CServer::start_data(SECRET, &srv_peers, &t.join("jobs"), 200, 2000, 2000, 0);
    let pool = Pool::new(ava).with_addr(srv.addr());
    Rig { pool, srv, t }
}

struct Run {
    sent: u64,
    resent: u64,
    files_sent: u64,
    skipped_files: u64,
    skipped_bytes: u64,
    /// The engine's progress counter when the upload returned (sent plus skipped).
    progress: u64,
}

fn cfg(fs: Option<Arc<dyn SourceFs>>) -> (TransferConfig, Arc<AtomicU64>) {
    let mut c = TransferConfig::new("127.0.0.1:9120");
    let sent = Arc::new(AtomicU64::new(0));
    c.progress_bytes = Some(sent.clone());
    c.progress_files = Some(Arc::new(AtomicU64::new(0)));
    c.progress_files_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.progress_bytes_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.cancel = Some(Arc::new(AtomicBool::new(false)));
    c.source_fs = fs;
    (c, sent)
}

static JOB: AtomicU64 = AtomicU64::new(1);

/// One upload of `src` to `dest` as a brand-new job.
fn run(r: &Rig, fs: Option<Arc<dyn SourceFs>>, src: &Path, dest: &Path, mode: SkipMode) -> Run {
    let (c, sent) = cfg(fs);
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&JOB.fetch_add(1, Ordering::SeqCst).to_le_bytes());
    id[8..12].copy_from_slice(&std::process::id().to_le_bytes());
    let res = upload_dir_skip_existing_in(&r.pool, &c, id, dest.to_str().unwrap(), src, mode)
        .unwrap_or_else(|e| panic!("upload failed: {e:#}"));
    let body: serde_json::Value = serde_json::from_str(&res.commit_ack_body).unwrap();
    Run {
        sent: res.bytes_sent,
        resent: body["resent"].as_u64().unwrap(),
        files_sent: body["files_sent"].as_u64().unwrap(),
        skipped_files: body["skipped_files"].as_u64().unwrap(),
        skipped_bytes: body["skipped_bytes"].as_u64().unwrap(),
        progress: sent.load(Ordering::Relaxed),
    }
}

fn write(p: &Path, b: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b).unwrap();
}

fn big(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed))
        .collect()
}

#[test]
fn local_source_second_run_skips_everything_and_a_changed_file_is_resent() {
    let r = rig("nas-local");
    let src = r.t.join("src");
    let dest = r.t.join("dest");
    write(&src.join("a.bin"), &big(300_000, 1));
    write(&src.join("d/b.bin"), &big(200_000, 2));
    let first = run(&r, None, &src, &dest, SkipMode::Fast);
    assert!(first.sent >= 500_000, "first run sends the bytes");
    assert_eq!(
        std::fs::read(dest.join("d/b.bin")).unwrap(),
        big(200_000, 2)
    );

    assert_eq!((first.files_sent, first.skipped_files), (2, 0));

    let second = run(&r, None, &src, &dest, SkipMode::Fast);
    assert_eq!(second.sent, 0, "identical tree: nothing re-sent");
    assert_eq!(second.resent, 0);
    // Everything skipped: filesSent 0 and skippedFiles N is the client's "already up to
    // date", and the bar (sent + skipped) reaches the whole 500 000 bytes.
    assert_eq!(
        (
            second.files_sent,
            second.skipped_files,
            second.skipped_bytes
        ),
        (0, 2, 500_000)
    );
    assert_eq!(second.progress, 500_000);

    // A size change is re-sent, and only that file.
    write(&src.join("d/b.bin"), &big(210_000, 3));
    let third = run(&r, None, &src, &dest, SkipMode::Fast);
    assert!(
        third.sent >= 210_000 && third.sent < 300_000,
        "only the changed file: {}",
        third.sent
    );
    assert_eq!(
        std::fs::read(dest.join("d/b.bin")).unwrap(),
        big(210_000, 3)
    );
    assert_eq!(std::fs::read(dest.join("a.bin")).unwrap(), big(300_000, 1));
    drop(r.srv);
}

#[test]
fn local_same_size_new_mtime_is_resent_by_fast_mode() {
    let r = rig("nas-mtime-change");
    let src = r.t.join("src");
    let dest = r.t.join("dest");
    write(&src.join("a.bin"), &big(100_000, 1));
    write(&src.join("b.bin"), &big(100_000, 2));
    run(&r, None, &src, &dest, SkipMode::Fast);
    // Same size, other bytes, a later mtime (an editor saved over it).
    write(&src.join("b.bin"), &big(100_000, 7));
    let f = std::fs::File::options()
        .write(true)
        .open(src.join("b.bin"))
        .unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(120))
        .unwrap();
    let again = run(&r, None, &src, &dest, SkipMode::Fast);
    assert_eq!((again.files_sent, again.skipped_files), (1, 1));
    assert_eq!(std::fs::read(dest.join("b.bin")).unwrap(), big(100_000, 7));
    drop(r.srv);
}

/// A one-level in-memory share; `with_mtime` decides whether the backend can report times.
#[derive(Debug)]
struct Nas {
    files: Mutex<BTreeMap<String, Vec<u8>>>,
    with_mtime: bool,
    /// Every `open` waits this long (a slow share).
    open_delay_ms: u64,
    /// Every opened path, in order.
    opened: Mutex<Vec<String>>,
    /// Paths from here on block in `open` until the gate opens.
    gate_from: Option<String>,
    gate: (Mutex<bool>, std::sync::Condvar),
}

impl Nas {
    fn new(files: BTreeMap<String, Vec<u8>>, with_mtime: bool) -> Nas {
        Nas {
            files: Mutex::new(files),
            with_mtime,
            open_delay_ms: 0,
            opened: Mutex::default(),
            gate_from: None,
            gate: (Mutex::new(true), std::sync::Condvar::new()),
        }
    }

    fn open_gate(&self) {
        *self.gate.0.lock().unwrap() = true;
        self.gate.1.notify_all();
    }
}

impl SourceFs for Nas {
    fn open(&self, p: &Path) -> std::io::Result<Box<dyn ReadSeek>> {
        if self.open_delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(self.open_delay_ms));
        }
        let path = p.to_str().unwrap();
        if self.gate_from.as_deref().is_some_and(|g| path >= g) {
            let mut open = self.gate.0.lock().unwrap();
            while !*open {
                open = self.gate.1.wait(open).unwrap();
            }
        }
        self.opened.lock().unwrap().push(path.to_string());
        let f = self.files.lock().unwrap();
        let b = f
            .get(p.to_str().unwrap())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"))?;
        Ok(Box::new(Cursor::new(b.clone())))
    }
    fn metadata(&self, p: &Path) -> std::io::Result<SourceMeta> {
        match self.files.lock().unwrap().get(p.to_str().unwrap()) {
            Some(b) => Ok(SourceMeta {
                len: b.len() as u64,
                is_dir: false,
                is_file: true,
            }),
            None => Ok(SourceMeta {
                len: 0,
                is_dir: true,
                is_file: false,
            }),
        }
    }
    fn read_dir(&self, _p: &Path) -> std::io::Result<Vec<(PathBuf, bool)>> {
        Ok(self
            .files
            .lock()
            .unwrap()
            .keys()
            .map(|k| (PathBuf::from(k), false))
            .collect())
    }
    fn mtime(&self, p: &Path) -> Option<u64> {
        (self.with_mtime && self.files.lock().unwrap().contains_key(p.to_str().unwrap()))
            .then_some(MTIME)
    }
}

fn nas(with_mtime: bool) -> Arc<Nas> {
    let mut files = BTreeMap::new();
    files.insert("/share/a".to_string(), big(300_000, 5));
    files.insert("/share/b".to_string(), big(200_000, 6));
    Arc::new(Nas::new(files, with_mtime))
}

fn remote_flow(tag: &str, with_mtime: bool) {
    let r = rig(tag);
    let dest = r.t.join("dest");
    let n = nas(with_mtime);
    let fs: Arc<dyn SourceFs> = n.clone();
    let first = run(
        &r,
        Some(fs.clone()),
        Path::new("/share"),
        &dest,
        SkipMode::Fast,
    );
    assert!(first.sent >= 500_000);

    let second = run(
        &r,
        Some(fs.clone()),
        Path::new("/share"),
        &dest,
        SkipMode::Fast,
    );
    assert_eq!(second.sent, 0, "identical share: nothing re-sent");

    // Same size, other bytes. Mtime-less: the verify fallback sees it. Mtime-bearing: the
    // backend reports the same time, so skip-existing cannot (the documented weakness,
    // SPEC.md §11.4); Safe mode catches it either way.
    n.files
        .lock()
        .unwrap()
        .insert("/share/b".into(), big(200_000, 9));
    let third = run(
        &r,
        Some(fs.clone()),
        Path::new("/share"),
        &dest,
        SkipMode::Fast,
    );
    if with_mtime {
        assert_eq!(third.sent, 0, "size+mtime equal means skipped");
        assert_eq!(std::fs::read(dest.join("b")).unwrap(), big(200_000, 6));
    } else {
        assert!(
            third.sent >= 200_000 && third.sent < 300_000,
            "{}",
            third.sent
        );
        assert_eq!(std::fs::read(dest.join("b")).unwrap(), big(200_000, 9));
    }
    let safe = run(
        &r,
        Some(fs.clone()),
        Path::new("/share"),
        &dest,
        SkipMode::Safe,
    );
    assert_eq!(std::fs::read(dest.join("b")).unwrap(), big(200_000, 9));
    if with_mtime {
        assert!(safe.sent >= 200_000 && safe.sent < 300_000, "{}", safe.sent);
    } else {
        assert_eq!(safe.sent, 0, "already current");
    }

    // A size change is re-sent under both.
    n.files
        .lock()
        .unwrap()
        .insert("/share/a".into(), big(310_000, 4));
    let fifth = run(&r, Some(fs), Path::new("/share"), &dest, SkipMode::Fast);
    assert!(
        fifth.sent >= 310_000 && fifth.sent < 400_000,
        "{}",
        fifth.sent
    );
    assert_eq!(std::fs::read(dest.join("a")).unwrap(), big(310_000, 4));
    drop(r.srv);
}

#[test]
fn remote_source_with_mtimes_skips_by_size_and_time() {
    remote_flow("nas-mtime", true);
}

#[test]
fn remote_source_without_mtimes_verifies_and_resends_only_what_changed() {
    remote_flow("nas-nomtime", false);
}

/// `n` 50 000-byte files `f00..` on a share without mtimes.
fn many(n: usize, with_mtime: bool) -> Nas {
    let files = (0..n)
        .map(|i| (format!("/share/f{i:02}"), big(50_000, i as u8)))
        .collect();
    Nas::new(files, with_mtime)
}

fn waited(what: &str, mut ok: impl FnMut() -> bool) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !ok() {
        assert!(
            std::time::Instant::now() < end,
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn hashing_reports_progress_and_cancel_stops_it_promptly() {
    let r = rig("nas-hash");
    let dest = r.t.join("dest");
    let mut nas = many(40, false);
    nas.open_delay_ms = 50; // 2 s to hash the share
    let fs: Arc<dyn SourceFs> = Arc::new(nas);

    // Progress advances while hashing: a watcher sees a value strictly inside (0, total).
    let (mut c, _) = cfg(Some(fs.clone()));
    let verify = Arc::new(AtomicU64::new(0));
    c.progress_verify = Some(verify.clone());
    let cancel = c.cancel.clone().unwrap();
    let seen_mid = AtomicBool::new(false);
    let started = std::time::Instant::now();
    let res = std::thread::scope(|sc| {
        let h = sc.spawn(|| {
            upload_dir_skip_existing_in(
                &r.pool,
                &c,
                [0x51; 16],
                dest.to_str().unwrap(),
                Path::new("/share"),
                SkipMode::Fast,
            )
        });
        // Cancel once some, but not all, of the 2 000 000 bytes are hashed.
        waited("hashing to start", || verify.load(Ordering::Relaxed) > 0);
        let v = verify.load(Ordering::Relaxed);
        seen_mid.store(v > 0 && v < 2_000_000, Ordering::Relaxed);
        cancel.store(true, Ordering::Relaxed);
        h.join().unwrap()
    });
    assert!(
        seen_mid.load(Ordering::Relaxed),
        "progress advanced during hashing"
    );
    let e = res.unwrap_err();
    assert!(e.to_string().contains("transfer_cancelled"), "{e:#}");
    assert!(
        started.elapsed() < std::time::Duration::from_millis(1800),
        "cancel did not wait for the whole hash: {:?}",
        started.elapsed()
    );
    assert!(!dest.exists(), "nothing was sent");
    drop(r.srv);
}

#[test]
fn nas_upload_resume_after_a_drop() {
    let mut r = rig("nas-drop");
    let dest = r.t.join("dest");
    let mut nas = many(40, true);
    nas.gate_from = Some("/share/f20".into());
    *nas.gate.0.lock().unwrap() = false; // f20.. wait until released
    let nas = Arc::new(nas);
    let fs: Arc<dyn SourceFs> = nas.clone();
    let (c, _) = cfg(Some(fs));
    let done = c.progress_files.clone().unwrap();
    let Rig { pool, srv, .. } = &mut r;
    let res = std::thread::scope(|sc| {
        let h = sc.spawn(|| {
            upload_dir_skip_existing_in(
                pool,
                &c,
                [0x52; 16],
                dest.to_str().unwrap(),
                Path::new("/share"),
                SkipMode::Fast,
            )
        });
        // f00..f19 are on the console's journal; then the console restarts (session
        // killed, memory gone, journal kept) while the rest is still being read.
        waited("10 durable files", || done.load(Ordering::Relaxed) >= 10);
        srv.restart_data();
        nas.open_gate();
        h.join().unwrap()
    });
    let res = res.unwrap_or_else(|e| panic!("resume failed: {e:#}"));
    for i in 0..40 {
        assert_eq!(
            std::fs::read(dest.join(format!("f{i:02}"))).unwrap(),
            big(50_000, i as u8),
            "f{i:02}"
        );
    }
    assert_eq!(res.files_sent, 40);
    // Only the missing files were read again: f00..f19 reached the console's journal
    // before the drop and were opened exactly once. (f20.. may be opened twice: the
    // first attempt's readers were released into the dead session.)
    let opened = nas.opened.lock().unwrap().clone();
    for i in 0..20 {
        let name = format!("/share/f{i:02}");
        assert_eq!(
            opened.iter().filter(|p| **p == name).count(),
            1,
            "{name} was read again after it was durable: {opened:?}"
        );
    }
}
