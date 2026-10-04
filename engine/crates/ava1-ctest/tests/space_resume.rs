//! The free-space check against the real C receiver (design 015/02, review 019 F1/F2):
//!
//! - a resume is admitted when what is LEFT fits, even though the whole upload would not (#365):
//!   files already in place, and the blocks a kept `.ava-part` already holds, are credited;
//! - an over-commit is refused up front, before a byte is sent, not hours in (F1);
//! - two uploads into one drive cannot both be admitted into room for one;
//! - the upload survives the console going to rest mode and coming back, with and without the
//!   engine restarting across the sleep (#353 on AVA1), every byte hash-verified.
//!
//! The room comes from the pool's probe seam: the tests set what the drive "has", the receiver
//! reports what it holds, nothing is mocked in between.
#![cfg(unix)]
mod common;

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1_chaos::{ChaosConfig, ChaosProxy};
use ava1_ctest::CServer;
use common::{dir, SECRET};
use ps5upload_ava1::space::Room;
use ps5upload_ava1::upload::{upload_dir_in, upload_dir_skip_existing_in, SkipMode, UploadFailure};
use ps5upload_ava1::Pool;
use ps5upload_core::transfer::TransferConfig;

const MIB: u64 = 1 << 20;
/// The forty small files of the rest-mode scenario (20_000 + i bytes each, at most).
const SMALL_TOTAL: u64 = 40 * 20_040;

fn cfg() -> TransferConfig {
    let mut c = TransferConfig::new("127.0.0.1:9113");
    c.progress_bytes = Some(Arc::new(AtomicU64::new(0)));
    c.progress_files = Some(Arc::new(AtomicU64::new(0)));
    c.progress_files_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.progress_bytes_finalized = Some(Arc::new(AtomicU64::new(0)));
    c.cancel = Some(Arc::new(AtomicBool::new(false)));
    c
}

fn pair(t: &Path) -> PathBuf {
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
    ava
}

fn serve(t: &Path) -> CServer {
    CServer::start_data(
        SECRET,
        &t.join("srv-peers"),
        &t.join("jobs"),
        200,
        2000,
        2000,
        0,
    )
}

/// A pool whose drive has `room` allocatable bytes (changeable while it runs).
fn pool_with_room(ava: PathBuf, addr: String, volume: &str, room: Arc<AtomicU64>) -> Pool {
    let volume = volume.to_string();
    Pool::new(ava)
        .with_addr(addr)
        .with_room_probe(Arc::new(move |_, _| {
            let r = room.load(Ordering::SeqCst);
            Some(Room {
                volume: volume.clone(),
                dev: None,
                free_bytes: r,
                reserve_bytes: 0,
                allocatable_bytes: r,
            })
        }))
}

fn bytes(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed))
        .collect()
}

fn write(p: &Path, b: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b).unwrap();
}

fn refusal(e: &anyhow::Error) -> Option<&UploadFailure> {
    e.downcast_ref::<UploadFailure>()
        .filter(|f| f.reason == "preflight_insufficient_space")
}

/// Allocated bytes of every file under `root`, each at most its length (what the drive holds).
fn on_disk(root: &Path) -> u64 {
    let mut total = 0;
    let Ok(rd) = std::fs::read_dir(root) else {
        return 0;
    };
    for e in rd.flatten() {
        let m = e.metadata().unwrap();
        if m.is_dir() {
            total += on_disk(&e.path());
        } else {
            total += (m.blocks() * 512).min(m.len());
        }
    }
    total
}

fn job(n: u8) -> [u8; 16] {
    let mut id = [n; 16];
    id[8..12].copy_from_slice(&std::process::id().to_le_bytes());
    id
}

// ---- (1) files in place are credited (#365) ----------------------------------------------

#[test]
fn a_folder_already_on_the_console_is_credited_and_a_fresh_one_is_refused_before_any_byte() {
    let t = dir("space-done");
    let ava = pair(&t);
    let srv = serve(&t);
    let room = Arc::new(AtomicU64::new(u64::MAX / 4));
    let pool = pool_with_room(ava, srv.addr(), "/ctest-done", room.clone());
    let src = t.join("src");
    write(&src.join("a.bin"), &bytes(900_000, 1));
    write(&src.join("d/b.bin"), &bytes(700_000, 2));
    let dest = t.join("dest");
    let d = dest.to_str().unwrap();

    upload_dir_skip_existing_in(&pool, &cfg(), job(1), d, &src, SkipMode::Fast).unwrap();
    assert_eq!(
        std::fs::read(dest.join("a.bin")).unwrap(),
        bytes(900_000, 1)
    );

    // The folder grows by 400 KB; the drive has room for exactly that and no more. The whole
    // folder (2 MB) does not fit, the rest (400 KB) does.
    write(&src.join("c.bin"), &bytes(400_000, 3));
    room.store(400_000, Ordering::SeqCst);
    let c = cfg();
    let sent = c.progress_bytes.clone().unwrap();
    let r = upload_dir_skip_existing_in(&pool, &c, job(2), d, &src, SkipMode::Fast)
        .unwrap_or_else(|e| panic!("a resume that fits was refused: {e:#}"));
    assert!(
        r.bytes_sent >= 400_000 && r.bytes_sent < 1_000_000,
        "{}",
        r.bytes_sent
    );
    assert_eq!(
        std::fs::read(dest.join("c.bin")).unwrap(),
        bytes(400_000, 3)
    );
    assert!(sent.load(Ordering::Relaxed) > 0);

    // The same room refuses a fresh upload of the same folder, up front: nothing is sent and
    // nothing is written, and the message names what is needed.
    let fresh = t.join("fresh");
    let c = cfg();
    let sent = c.progress_bytes.clone().unwrap();
    let e = upload_dir_skip_existing_in(
        &pool,
        &c,
        job(3),
        fresh.to_str().unwrap(),
        &src,
        SkipMode::Fast,
    )
    .expect_err("2 MB cannot go into 400 KB");
    let f = refusal(&e).unwrap_or_else(|| panic!("wrong failure: {e:#}"));
    assert!(
        f.detail.contains("needs 2000000 more bytes"),
        "{}",
        f.detail
    );
    assert!(f.detail.contains("short by 1600000 bytes"), "{}", f.detail);
    assert_eq!(sent.load(Ordering::Relaxed), 0, "refused after sending");
    assert!(!fresh.join("a.bin").exists());
}

// ---- (2) a kept .ava-part is credited, and the refusal leaves the journal alone ----------

#[tokio::test(flavor = "multi_thread")]
async fn a_kept_partial_is_credited_and_a_too_small_drive_leaves_it_for_the_retry() {
    let t = dir("space-part");
    let ava = pair(&t);
    let srv = serve(&t);
    let proxy = ChaosProxy::start(
        srv.addr().parse().unwrap(),
        ChaosConfig {
            bytes_per_sec: Some(512 << 10),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let room = Arc::new(AtomicU64::new(u64::MAX / 4));
    let pool = Arc::new(pool_with_room(
        ava,
        proxy.addr.to_string(),
        "/ctest-part",
        room.clone(),
    ));
    let src = t.join("src");
    let total = 8 * MIB;
    write(&src.join("f0"), &bytes((4 * MIB) as usize, 10));
    write(&src.join("f1"), &bytes((4 * MIB) as usize, 11));
    let dest = t.join("dest");
    let id = job(7);

    // First attempt: cancelled once a couple of MiB are on the wire.
    let c1 = cfg();
    let (flag, sent) = (
        c1.cancel.clone().unwrap(),
        c1.progress_bytes.clone().unwrap(),
    );
    let (p, s, d) = (
        pool.clone(),
        src.clone(),
        dest.to_str().unwrap().to_string(),
    );
    let first = tokio::task::spawn_blocking(move || upload_dir_in(&p, &c1, id, &d, &s));
    let start = std::time::Instant::now();
    while sent.load(Ordering::Relaxed) < 2 * MIB {
        assert!(start.elapsed() < Duration::from_secs(60), "no progress");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    flag.store(true, Ordering::Relaxed);
    tokio::time::timeout(Duration::from_secs(60), first)
        .await
        .expect("the cancelled upload never returned")
        .unwrap()
        .expect_err("cancelled");

    // Let the receiver settle, then measure what the drive holds for this job.
    let staged = PathBuf::from(format!("{}.ava-part", dest.display()));
    let mut held = on_disk(&staged);
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let now = on_disk(&staged);
        if now == held && now > 0 {
            break;
        }
        held = now;
    }
    assert!(held > 0, "nothing was kept: the scenario did not run");
    assert!(held < total, "everything landed: cancelled too late");

    // A drive with less room than the rest needs: refused up front, with the credit named.
    room.store(total - held - 256 * 1024, Ordering::SeqCst);
    let c = cfg();
    let sent2 = c.progress_bytes.clone().unwrap();
    let (p, s, d) = (
        pool.clone(),
        src.clone(),
        dest.to_str().unwrap().to_string(),
    );
    let e = tokio::task::spawn_blocking(move || upload_dir_in(&p, &c, id, &d, &s))
        .await
        .unwrap()
        .expect_err("256 KiB short must be refused");
    let f = refusal(&e).unwrap_or_else(|| panic!("wrong failure: {e:#}"));
    assert!(f.detail.contains("already on the console"), "{}", f.detail);
    assert_eq!(sent2.load(Ordering::Relaxed), 0, "refused after sending");

    // Room for exactly the rest: admitted, finished, byte-identical. Without the credit this
    // drive (less than the 8 MiB whole) could never take the retry.
    room.store(total - held + 256 * 1024, Ordering::SeqCst);
    let (p, s, d) = (
        pool.clone(),
        src.clone(),
        dest.to_str().unwrap().to_string(),
    );
    let c = cfg();
    tokio::time::timeout(
        Duration::from_secs(120),
        tokio::task::spawn_blocking(move || upload_dir_in(&p, &c, id, &d, &s)),
    )
    .await
    .expect("the resume did not finish")
    .unwrap()
    .unwrap_or_else(|e| panic!("a resume that fits was refused: {e:#}"));
    assert!(std::fs::read(dest.join("f0")).unwrap() == bytes((4 * MIB) as usize, 10));
    assert!(std::fs::read(dest.join("f1")).unwrap() == bytes((4 * MIB) as usize, 11));
}

// ---- (3) two uploads, one drive -----------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn two_uploads_into_room_for_one_admit_one_and_refuse_the_other_early() {
    let t = dir("space-two");
    let ava = pair(&t);
    let srv = serve(&t);
    let proxy = ChaosProxy::start(
        srv.addr().parse().unwrap(),
        ChaosConfig {
            bytes_per_sec: Some(512 << 10),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // Room for 8 MiB: either upload fits alone; both together do not. A takes ~16 s at this
    // rate, far longer than B needs to reach the door.
    let room = Arc::new(AtomicU64::new(8 * MIB + 64 * 1024));
    let pool = Arc::new(pool_with_room(
        ava,
        proxy.addr.to_string(),
        "/ctest-two",
        room,
    ));
    let (a, b) = (t.join("srcA"), t.join("srcB"));
    write(&a.join("a.bin"), &bytes((8 * MIB) as usize, 20));
    write(&b.join("b.bin"), &bytes((8 * MIB) as usize, 21));

    let ca = cfg();
    let (sent_a, cancel_a) = (
        ca.progress_bytes.clone().unwrap(),
        ca.cancel.clone().unwrap(),
    );
    let (p, d) = (pool.clone(), t.join("destA").to_str().unwrap().to_string());
    let first = tokio::task::spawn_blocking(move || upload_dir_in(&p, &ca, job(31), &d, &a));
    let start = std::time::Instant::now();
    while sent_a.load(Ordering::Relaxed) == 0 {
        assert!(start.elapsed() < Duration::from_secs(60), "A never started");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // A is mid-flight and has promised the room. B must be turned away at the door.
    let (p, d) = (pool.clone(), t.join("destB").to_str().unwrap().to_string());
    let cb = cfg();
    let sent_b = cb.progress_bytes.clone().unwrap();
    let e = tokio::task::spawn_blocking(move || upload_dir_in(&p, &cb, job(32), &d, &b))
        .await
        .unwrap()
        .expect_err("B would overfill the drive");
    let f = refusal(&e).unwrap_or_else(|| panic!("wrong failure: {e:#}"));
    assert!(
        f.detail.contains("promised to other uploads"),
        "{}",
        f.detail
    );
    assert_eq!(sent_b.load(Ordering::Relaxed), 0);
    assert!(
        !cancel_a.load(Ordering::Relaxed) && !first.is_finished(),
        "A was disturbed by B's refusal"
    );
    tokio::time::timeout(Duration::from_secs(120), first)
        .await
        .expect("A did not finish")
        .unwrap()
        .unwrap_or_else(|e| panic!("A failed: {e:#}"));
}

// ---- (4) rest mode: the console sleeps mid-upload and wakes (#353 on AVA1) ----------------

/// Mid-upload the link dies and the console's payload is gone (rest mode ends the payload);
/// at wake a new payload starts on the same journals, behind the same address. With
/// `restart_engine` the engine process is gone too: the call returns, a new pool (same
/// identity and sender state) resumes the same job. Either way the upload completes with
/// every byte hash-verified and nothing durable is sent twice.
async fn rest_mode(tag: &str, restart_engine: bool) {
    let t = dir(tag);
    let ava = pair(&t);
    let srv = serve(&t);
    let cfg_slow = ChaosConfig {
        bytes_per_sec: Some(512 << 10),
        ..Default::default()
    };
    let proxy = ChaosProxy::start(srv.addr().parse().unwrap(), cfg_slow.clone())
        .await
        .unwrap();
    let front = proxy.addr;
    let room = Arc::new(AtomicU64::new(u64::MAX / 4));
    let src = t.join("src");
    let total = 9 * MIB / 2;
    for i in 0..3u8 {
        write(
            &src.join(format!("big{i}")),
            &bytes((MIB * 3 / 2) as usize, 40 + i),
        );
    }
    for i in 0..40u8 {
        write(
            &src.join(format!("s/small{i}")),
            &bytes(20_000 + usize::from(i), i),
        );
    }
    let want: Vec<(String, Vec<u8>)> = (0..3u8)
        .map(|i| (format!("big{i}"), bytes((MIB * 3 / 2) as usize, 40 + i)))
        .chain((0..40u8).map(|i| (format!("s/small{i}"), bytes(20_000 + usize::from(i), i))))
        .collect();
    let dest = t.join("dest");
    let id = job(50);

    let pool = Arc::new(pool_with_room(
        ava.clone(),
        front.to_string(),
        &format!("/ctest-{tag}"),
        room.clone(),
    ));
    let c = cfg();
    let (flag, sent, durable) = (
        c.cancel.clone().unwrap(),
        c.progress_bytes.clone().unwrap(),
        c.progress_bytes_finalized.clone().unwrap(),
    );
    let (p, s, d) = (
        pool.clone(),
        src.clone(),
        dest.to_str().unwrap().to_string(),
    );
    let run = tokio::task::spawn_blocking(move || upload_dir_in(&p, &c, id, &d, &s));
    let start = std::time::Instant::now();
    while sent.load(Ordering::Relaxed) < 2 * MIB {
        assert!(start.elapsed() < Duration::from_secs(60), "no progress");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // The console goes to rest mode: the link dies, the payload is gone.
    proxy.blackhole(true);
    proxy.kill_all();
    drop(proxy);
    drop(srv);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let (second_flag, second_sent, resumed) = if restart_engine {
        flag.store(true, Ordering::Relaxed);
        let _ = tokio::time::timeout(Duration::from_secs(90), run)
            .await
            .expect("the engine call did not return");
        (None, None, None)
    } else {
        (Some(flag), Some(sent.clone()), Some(run))
    };
    let durable_at_sleep = durable.load(Ordering::Relaxed);

    // Wake: a new payload on the same journals and files, behind the same address.
    let srv = serve(&t);
    let proxy = ChaosProxy::start_on(&front.to_string(), srv.addr().parse().unwrap(), cfg_slow)
        .await
        .expect("the old address is not free again");

    let (finish, counter) = if let Some(run) = resumed {
        (run, second_sent.unwrap())
    } else {
        let pool2 = Arc::new(pool_with_room(
            ava,
            front.to_string(),
            &format!("/ctest-{tag}"),
            room,
        ));
        let c = cfg();
        let counter = c.progress_bytes.clone().unwrap();
        let (s, d) = (src.clone(), dest.to_str().unwrap().to_string());
        (
            tokio::task::spawn_blocking(move || upload_dir_in(&pool2, &c, id, &d, &s)),
            counter,
        )
    };
    let finished = tokio::time::timeout(Duration::from_secs(180), finish)
        .await
        .unwrap_or_else(|_| {
            if let Some(f) = &second_flag {
                f.store(true, Ordering::Relaxed);
            }
            panic!(
                "the resume did not finish: {} sent",
                counter.load(Ordering::Relaxed)
            )
        })
        .unwrap()
        .unwrap_or_else(|e| panic!("the upload did not survive the sleep: {e:#}"));
    drop(proxy);

    for (name, bytes) in &want {
        assert!(
            std::fs::read(dest.join(name)).unwrap() == *bytes,
            "{name} differs after the resume"
        );
    }
    // Resuming never re-sends what was durable when the console went away.
    if restart_engine {
        assert!(
            finished.bytes_sent + durable_at_sleep <= total + SMALL_TOTAL,
            "re-sent durable data: {} sent after {} durable",
            finished.bytes_sent,
            durable_at_sleep
        );
    }
    drop(srv);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upload_survives_the_console_resting_with_the_engine_running() {
    rest_mode("space-rest-live", false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upload_survives_the_console_resting_and_the_engine_restarting() {
    rest_mode("space-rest-restart", true).await;
}
