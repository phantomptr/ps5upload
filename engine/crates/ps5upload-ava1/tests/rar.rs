//! RAR uploads over AVA1 (Task 12). There is no `rar` tool in the repo's toolchain
//! (the format's compressor is proprietary), so the multi-entry archives here are
//! written by a small RAR5 *stored* writer (`rar5`): real RAR5 containers that UnRAR
//! reads, with method 0 (no compression). That exercises everything the source owns
//! (headers, entry order, solid/non-solid skipping, CRC checks, typed failures, resume).
//! Compressed decoding and passwords are covered by the repo's checked-in fixtures
//! under `ps5upload-core/testdata/rar` (`crypted.rar`: content-encrypted, password
//! `unrar`; `comment-hpw-password.rar`: header-encrypted, password `password`).
#![cfg(not(target_os = "android"))]
mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ava1::keys::Identity;
use ava1::seq::{EntrySink, Keep, Restart, SeqSource};
use ava1_chaos::{ChaosConfig, ChaosProxy};
use common::*;
use ps5upload_ava1::rar_source::{RarOpenError, RarReason, RarSource};
use ps5upload_ava1::upload::{self, UploadFailure};
use ps5upload_ava1::Pool;
use ps5upload_core::transfer::TransferConfig;

// ---- a RAR5 stored-archive writer ---------------------------------------------------

fn vint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn crc32(b: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(b);
    c.sum()
}

/// One block: CRC32 of everything after it, then `size` and `body`.
fn block(body: &[u8]) -> Vec<u8> {
    let mut hdr = Vec::new();
    vint(body.len() as u64, &mut hdr);
    hdr.extend_from_slice(body);
    let mut out = crc32(&hdr).to_le_bytes().to_vec();
    out.extend_from_slice(&hdr);
    out
}

pub enum Ent<'a> {
    File(&'a str, &'a [u8]),
    /// A file whose header says "unpacked size unknown" (flag 0x8).
    Unknown(&'a str, &'a [u8]),
    Dir(&'a str),
    /// A file with a Unix-seconds modification time in its header.
    Dated(&'a str, &'a [u8], u32),
}

fn rar5(entries: &[Ent], solid: bool) -> Vec<u8> {
    let mut out = b"Rar!\x1a\x07\x01\x00".to_vec();
    // Main header: type 1, flags 0, archive flags (0x4 = solid).
    let mut main = Vec::new();
    vint(1, &mut main);
    vint(0, &mut main);
    vint(if solid { 4 } else { 0 }, &mut main);
    out.extend(block(&main));
    for (i, e) in entries.iter().enumerate() {
        let (name, data, is_dir, unknown) = match e {
            Ent::File(n, d) | Ent::Dated(n, d, _) => (*n, *d, false, false),
            Ent::Unknown(n, d) => (*n, *d, false, true),
            Ent::Dir(n) => (*n, &[][..], true, false),
        };
        let mut h = Vec::new();
        vint(2, &mut h); // file header
        vint(if is_dir { 0 } else { 2 }, &mut h); // header flags: data area follows
        if !is_dir {
            vint(data.len() as u64, &mut h); // data size
        }
        // file flags: 1 = directory, 4 = data CRC32 present
        vint(
            if is_dir {
                1
            } else if unknown {
                4 | 8
            } else if matches!(e, Ent::Dated(..)) {
                4 | 2
            } else {
                4
            },
            &mut h,
        );
        vint(if unknown { 0 } else { data.len() as u64 }, &mut h); // unpacked size
        vint(0o644, &mut h); // attributes
        if let Ent::Dated(_, _, t) = e {
            h.extend_from_slice(&t.to_le_bytes()); // mtime (Unix seconds)
        }
        if !is_dir {
            h.extend_from_slice(&crc32(data).to_le_bytes());
        }
        // compression info: version 0, solid bit 0x40 on all but the first, method 0
        vint(if solid && i > 0 { 0x40 } else { 0 }, &mut h);
        vint(1, &mut h); // host OS: unix
        vint(name.len() as u64, &mut h);
        h.extend_from_slice(name.as_bytes());
        out.extend(block(&h));
        out.extend_from_slice(data);
    }
    // End of archive header.
    let mut end = Vec::new();
    vint(5, &mut end);
    vint(0, &mut end);
    vint(0, &mut end);
    out.extend(block(&end));
    out
}

fn write(path: &Path, b: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b).unwrap();
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ps5upload-core/testdata/rar")
        .join(name)
}

struct Files(Vec<(String, Vec<u8>)>);

fn sample(n: usize) -> Files {
    Files(
        (0..n)
            .map(|i| {
                let size = match i {
                    0 => 0,
                    1 => 3 * 1024 * 1024 + 17, // a large file (several groups)
                    _ => 500 + i * 37,
                };
                (format!("d{}/f{i}.bin", i % 3), pattern(i as u8, 0, size))
            })
            .collect(),
    )
}

fn archive_of(f: &Files, solid: bool, dirs: &[&str]) -> Vec<u8> {
    let mut ents: Vec<Ent> = dirs.iter().map(|d| Ent::Dir(d)).collect();
    ents.extend(f.0.iter().map(|(n, b)| Ent::File(n, b)));
    rar5(&ents, solid)
}

fn cfg() -> TransferConfig {
    TransferConfig::new("127.0.0.1")
}

async fn setup(tag: &str) -> (PathBuf, PathBuf, Pool) {
    let d = temp(tag);
    let ava = d.join("engine");
    let key = Identity::load_or_create(&ava.join("identity"))
        .unwrap()
        .public();
    std::fs::create_dir_all(d.join("host/share")).unwrap();
    let addr = host(&d.join("host"), key).await;
    (d, ava.clone(), Pool::new(ava).with_addr(addr))
}

async fn upload_rar(
    pool: Pool,
    archive: PathBuf,
    pw: Option<&'static str>,
    id: u8,
) -> anyhow::Result<ps5upload_core::transfer::TransferResult> {
    tokio::time::timeout(
        Duration::from_secs(90),
        tokio::task::spawn_blocking(move || {
            upload::upload_rar_in(&pool, &cfg(), [id; 16], "dst", &archive, pw)
        }),
    )
    .await
    .expect("rar upload timed out")
    .unwrap()
}

fn assert_landed(d: &Path, f: &Files) {
    for (n, b) in &f.0 {
        assert_eq!(
            &std::fs::read(d.join("host/share/dst").join(n)).unwrap(),
            b,
            "{n}"
        );
    }
}

fn reason(e: &anyhow::Error) -> String {
    e.downcast_ref::<UploadFailure>()
        .unwrap_or_else(|| panic!("not a typed UploadFailure: {e:#}"))
        .reason
        .clone()
}

// ---- uploads --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_nonsolid_rar_uploads_and_verifies() {
    let (d, _, pool) = setup("rar-nonsolid").await;
    let f = sample(40);
    write(&d.join("a.rar"), &archive_of(&f, false, &["empty_dir"]));
    let r = upload_rar(pool, d.join("a.rar"), None, 1).await.unwrap();
    assert_eq!(r.files_sent, 40);
    assert_landed(&d, &f);
    assert!(d.join("host/share/dst/empty_dir").is_dir());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_solid_rar_uploads_and_verifies() {
    let (d, _, pool) = setup("rar-solid").await;
    let f = sample(40);
    write(&d.join("a.rar"), &archive_of(&f, true, &[]));
    let r = upload_rar(pool, d.join("a.rar"), None, 2).await.unwrap();
    assert_eq!(r.files_sent, 40);
    assert_landed(&d, &f);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rar_upload_resumes_after_the_connection_drops() {
    let d = temp("rar-drop");
    let ava = d.join("engine");
    let key = Identity::load_or_create(&ava.join("identity"))
        .unwrap()
        .public();
    std::fs::create_dir_all(d.join("host/share")).unwrap();
    let addr = host(&d.join("host"), key).await;
    let f = Files(
        (0..12)
            .map(|i| (format!("big/f{i}"), pattern(i as u8, 0, 2 << 20)))
            .collect(),
    );
    write(&d.join("a.rar"), &archive_of(&f, false, &[]));
    let proxy = Arc::new(
        ChaosProxy::start(
            addr.parse().unwrap(),
            ChaosConfig {
                bytes_per_sec: Some(4 << 20),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    let pool = Pool::new(ava).with_addr(proxy.addr.to_string());
    let c = cfg();
    let _ = &c;
    // Kill every connection once some of the archive has reached the console.
    let share = d.join("host/share/dst");
    let p2 = proxy.clone();
    let killer = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !(share.exists()
            || std::fs::read_dir(d_jobs(&share)).is_ok_and(|mut r| r.next().is_some()))
        {
            assert!(std::time::Instant::now() < deadline, "never started");
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(1500));
        p2.kill_all();
    });
    let r = upload_rar(pool, d.join("a.rar"), None, 3).await.unwrap();
    killer.join().unwrap();
    assert_eq!(r.files_sent, 12);
    assert_landed(&d, &f);
}

fn d_jobs(share: &Path) -> PathBuf {
    share.parent().unwrap().parent().unwrap().join("jobs")
}

// ---- the source's pass ----------------------------------------------------------------

#[derive(Default)]
struct Rec {
    cur: Option<String>,
    got: Vec<(String, Vec<u8>)>,
}

impl EntrySink for Rec {
    fn begin(&mut self, path: &str) -> std::io::Result<()> {
        self.cur = Some(path.to_owned());
        self.got.push((path.to_owned(), Vec::new()));
        Ok(())
    }
    fn data(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.got.last_mut().unwrap().1.extend_from_slice(bytes);
        Ok(())
    }
    fn end(&mut self) -> std::io::Result<()> {
        self.cur = None;
        Ok(())
    }
}

fn manifest_id(m: &ava1::manifest::Manifest, path: &str) -> u32 {
    m.entries.iter().position(|e| e.path == path).unwrap() as u32
}

#[test]
fn rar_nonsolid_resume_skips_done_entries() {
    let d = temp("rar-resume-ns");
    let f = sample(10);
    write(&d.join("a.rar"), &archive_of(&f, false, &[]));
    let (m, src) = RarSource::open(&d.join("a.rar"), None, &[]).unwrap();
    assert!(!src.is_solid());
    // The console holds every entry but the last two (archive order).
    let missing: Vec<String> = f.0[8..].iter().map(|(n, _)| n.clone()).collect();
    let restart = missing
        .iter()
        .map(|p| src.restart_for(manifest_id(&m, p)))
        .min()
        .unwrap();
    assert_eq!(restart, Restart(8), "non-solid: the entry itself");
    let mut asked: Vec<String> = Vec::new();
    let mut want = |p: &str, _s: u64| {
        asked.push(p.to_owned());
        Keep::All
    };
    let mut rec = Rec::default();
    src.pass(restart, &mut want, &mut rec, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        asked, missing,
        "earlier entries were seeked past, not decoded"
    );
    assert_eq!(rec.got, f.0[8..].to_vec());
}

#[test]
fn rar_solid_resume_still_correct() {
    let d = temp("rar-resume-solid");
    let f = sample(10);
    write(&d.join("a.rar"), &archive_of(&f, true, &[]));
    let (m, src) = RarSource::open(&d.join("a.rar"), None, &[]).unwrap();
    assert!(src.is_solid());
    // A solid archive restarts at the beginning whatever the first missing entry is.
    assert_eq!(src.restart_for(manifest_id(&m, &f.0[7].0)), Restart::START);
    let done: Vec<&str> = f.0[..7].iter().map(|(n, _)| n.as_str()).collect();
    let mut want = |p: &str, _s: u64| {
        if done.contains(&p) {
            Keep::Skip
        } else {
            Keep::All
        }
    };
    let mut rec = Rec::default();
    src.pass(Restart::START, &mut want, &mut rec, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(rec.got, f.0[7..].to_vec());
}

#[test]
fn rar_reordered_listing_only_matters_when_entries_are_skipped_by_ordinal() {
    let d = temp("rar-reordered");
    let f = sample(6);
    write(&d.join("a.rar"), &archive_of(&f, false, &[]));
    let (_m, src) = RarSource::open(&d.join("a.rar"), None, &[]).unwrap();
    // The listing the manifest was built from disagrees with the extraction order.
    let mut order: Vec<String> = f.0.iter().map(|(n, _)| n.clone()).collect();
    order.swap(2, 3);
    let src = src.with_listing_order_for_test(order);
    // A fresh pass binds by path: nothing is skipped by ordinal, so it uploads whole.
    let mut want = |_: &str, _: u64| Keep::All;
    let mut rec = Rec::default();
    src.pass(Restart(0), &mut want, &mut rec, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(rec.got, f.0.to_vec());
    // A resume from ordinal 3 trusts positions: refused, typed.
    let mut rec = Rec::default();
    let e = src
        .pass(Restart(3), &mut want, &mut rec, &AtomicBool::new(false))
        .unwrap_err();
    let f = ps5upload_ava1::rar_source::rar_failure(&e).expect("typed");
    assert_eq!(f.reason, RarReason::Reordered);
    assert_eq!(f.reason.as_str(), "ava1_rar_reordered");
    assert!(
        rec.got.is_empty(),
        "nothing was delivered from a wrong position"
    );
}

#[test]
fn rar_cancel_ends_a_long_skip_promptly() {
    let d = temp("rar-cancel");
    // A solid archive: skipping the first (48 MiB) entry means decoding it, the case
    // that used to be unstoppable. The cancel is raised as the skip starts.
    let big = pattern(9, 0, 48 << 20);
    let ents = [Ent::File("big", &big), Ent::File("after", b"tail")];
    write(&d.join("a.rar"), &rar5(&ents, true));
    let (_m, src) = RarSource::open(&d.join("a.rar"), None, &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let asked = std::sync::atomic::AtomicU32::new(0);
    let mut want = |_: &str, _: u64| {
        asked.fetch_add(1, Ordering::Relaxed);
        cancel.store(true, Ordering::Relaxed);
        Keep::Skip
    };
    let mut rec = Rec::default();
    let t = std::time::Instant::now();
    let e = src
        .pass(Restart::START, &mut want, &mut rec, &cancel)
        .unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::Interrupted, "{e}");
    assert_eq!(asked.load(Ordering::Relaxed), 1, "the walk went no further");
    assert!(rec.got.is_empty());
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
}

#[test]
fn a_sink_error_stops_a_pass_and_is_returned() {
    struct Bad;
    impl EntrySink for Bad {
        fn begin(&mut self, _: &str) -> std::io::Result<()> {
            Ok(())
        }
        fn data(&mut self, _: &[u8]) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "lane gone",
            ))
        }
        fn end(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let d = temp("rar-sinkerr");
    write(&d.join("a.rar"), &archive_of(&sample(4), false, &[]));
    let (_m, src) = RarSource::open(&d.join("a.rar"), None, &[]).unwrap();
    let mut want = |_: &str, s: u64| if s > 0 { Keep::All } else { Keep::Skip };
    let e = src
        .pass(Restart::START, &mut want, &mut Bad, &AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe);
}

// ---- typed failures -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn rar_password_wrong_is_a_clear_error() {
    let (_d, _, pool) = setup("rar-pw-wrong").await;
    // content-encrypted: names list without a password, the wrong one fails on data
    let e = upload_rar(pool, fixture("crypted.rar"), Some("not-it"), 4)
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "ava1_rar_password_wrong");
    assert!(!format!("{e:#}").contains("not-it"), "the password leaked");
    // header-encrypted: fails at planning, before any connection
    let d = temp("rar-pw-wrong-h");
    let pool = Pool::new(d.join("ava")).with_addr("127.0.0.1:1");
    let e = upload::upload_rar_in(
        &pool,
        &cfg(),
        [5; 16],
        "dst",
        &fixture("comment-hpw-password.rar"),
        Some("nope"),
    )
    .unwrap_err();
    assert_eq!(reason(&e), "ava1_rar_password_wrong");
    assert_eq!(pool.attempts(), 0);
    // the right password uploads
    let (d, _, pool) = setup("rar-pw-right").await;
    let r = upload_rar(pool, fixture("crypted.rar"), Some("unrar"), 6)
        .await
        .unwrap();
    assert!(r.files_sent >= 1);
    assert_eq!(
        std::fs::read(d.join("host/share/dst/.gitignore")).unwrap(),
        b"target\nCargo.lock\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rar_password_lost_after_restart_asks_again() {
    // A resumed job after an engine restart has no password: a typed reason the UI
    // turns into a prompt, for content- and header-encrypted archives alike.
    let (_d, _, pool) = setup("rar-pw-lost").await;
    let e = upload_rar(pool, fixture("crypted.rar"), None, 7)
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "ava1_rar_password_required");
    let d = temp("rar-pw-lost-h");
    let pool = Pool::new(d.join("ava")).with_addr("127.0.0.1:1");
    let e = upload::upload_rar_in(
        &pool,
        &cfg(),
        [8; 16],
        "dst",
        &fixture("comment-hpw-password.rar"),
        None,
    )
    .unwrap_err();
    assert_eq!(reason(&e), "ava1_rar_password_required");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_corrupt_rar_fails_typed() {
    let f = sample(6);
    let good = archive_of(&f, false, &[]);
    // A flipped byte inside entry data (the stored CRC no longer matches)
    let (d, _, pool) = setup("rar-corrupt").await;
    let mut bad = good.clone();
    let at = bad
        .windows(40)
        .position(|w| w == &f.0[3].1[..40])
        .expect("entry data in the archive");
    bad[at + 20] ^= 0xff;
    write(&d.join("bad.rar"), &bad);
    let e = upload_rar(pool, d.join("bad.rar"), None, 9)
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "ava1_rar_corrupt");
    // A truncated archive
    let (d, _, pool) = setup("rar-trunc").await;
    write(&d.join("t.rar"), &good[..good.len() / 2]);
    let e = upload_rar(pool, d.join("t.rar"), None, 10)
        .await
        .unwrap_err();
    let r = reason(&e);
    assert!(
        r == "ava1_rar_corrupt" || r == "ava1_rar_failed",
        "truncated archive reason {r}: {e:#}"
    );
}

#[test]
fn an_unsafe_entry_path_fails_before_connecting() {
    let d = temp("rar-unsafe");
    write(
        &d.join("a.rar"),
        &rar5(&[Ent::File("../evil", b"x")], false),
    );
    let pool = Pool::new(d.join("ava")).with_addr("127.0.0.1:1");
    let e =
        upload::upload_rar_in(&pool, &cfg(), [11; 16], "dst", &d.join("a.rar"), None).unwrap_err();
    assert!(e.downcast_ref::<UploadFailure>().is_some(), "{e:#}");
    assert_eq!(pool.attempts(), 0);
}

#[test]
fn a_duplicate_entry_is_unsupported() {
    let d = temp("rar-dup");
    write(
        &d.join("a.rar"),
        &rar5(&[Ent::File("a", b"1"), Ent::File("a", b"2")], false),
    );
    match RarSource::open(&d.join("a.rar"), None, &[]) {
        Err(RarOpenError::Unsupported(m)) => assert!(m.contains("more than once"), "{m}"),
        other => panic!("{:?}", other.err()),
    }
}

#[test]
fn rar_cancel_ends_a_long_excluded_solid_skip_promptly() {
    // An excluded entry in a solid archive is still decoded (to stay aligned); that
    // decode must poll the stop flag like any other skip.
    struct Stopper(std::sync::Arc<AtomicBool>);
    impl EntrySink for Stopper {
        fn begin(&mut self, _: &str) -> std::io::Result<()> {
            Ok(())
        }
        fn data(&mut self, _: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn end(&mut self) -> std::io::Result<()> {
            // Stop 1 ms into the excluded entry's decode (which follows).
            let c = self.0.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1));
                c.store(true, Ordering::Relaxed);
            });
            Ok(())
        }
    }
    let d = temp("rar-cancel-excl");
    let big = vec![7u8; 256 << 20];
    let ents = [
        Ent::File("a-first", b"head"),
        Ent::File("skipme/big", &big),
        Ent::File("z-last", b"tail"),
    ];
    write(&d.join("a.rar"), &rar5(&ents, true));
    let excl = vec!["skipme".to_string()];
    let (_m, src) = RarSource::open(&d.join("a.rar"), None, &excl).unwrap();
    let run = |stop_early: bool| {
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let mut want = |_: &str, _: u64| Keep::All;
        let t = std::time::Instant::now();
        let r = if stop_early {
            src.pass(
                Restart::START,
                &mut want,
                &mut Stopper(cancel.clone()),
                &cancel,
            )
        } else {
            src.pass(Restart::START, &mut want, &mut Rec::default(), &cancel)
        };
        (r, t.elapsed())
    };
    let (full, t_full) = run(false);
    full.unwrap();
    let (stopped, t_stop) = run(true);
    assert_eq!(stopped.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
    assert!(
        t_stop < t_full / 2,
        "stopping took {t_stop:?}, a full decode {t_full:?}: the excluded decode did not poll"
    );
}

#[test]
fn an_entry_with_unknown_unpacked_size_is_measured_and_uploaded() {
    let d = temp("rar-unknown-size");
    let body = pattern(5, 0, 70_000);
    let ents = [
        Ent::File("a", b"known"),
        Ent::Unknown("b/unknown", &body),
        Ent::File("empty", b""),
    ];
    write(&d.join("a.rar"), &rar5(&ents, false));
    let (m, src) = RarSource::open(&d.join("a.rar"), None, &[]).unwrap();
    let e = m.entries.iter().find(|e| e.path == "b/unknown").unwrap();
    assert_eq!(e.size, 70_000, "the size was measured, not trusted");
    let mut want = |_: &str, _: u64| Keep::All;
    let mut rec = Rec::default();
    src.pass(Restart::START, &mut want, &mut rec, &AtomicBool::new(false))
        .unwrap();
    let got: std::collections::HashMap<_, _> = rec.got.into_iter().collect();
    assert_eq!(got["b/unknown"], body);
    assert_eq!(got["a"], b"known");
    assert!(got["empty"].is_empty());
}

#[test]
fn duplicate_detection_ignores_case() {
    let d = temp("rar-dup-case");
    write(
        &d.join("a.rar"),
        &rar5(
            &[Ent::File("Data/File", b"1"), Ent::File("data/file", b"2")],
            false,
        ),
    );
    match RarSource::open(&d.join("a.rar"), None, &[]) {
        Err(RarOpenError::Unsupported(m)) => assert!(m.contains("differ only in case"), "{m}"),
        other => panic!("{:?}", other.err()),
    }
}

#[test]
fn a_cancelled_job_does_not_open_the_archive() {
    let d = temp("rar-precancel");
    // The path does not exist: opening would be an error, not Interrupted.
    let (_m, src) = {
        write(&d.join("a.rar"), &archive_of(&sample(3), false, &[]));
        RarSource::open(&d.join("a.rar"), None, &[]).unwrap()
    };
    std::fs::remove_file(d.join("a.rar")).unwrap();
    let mut want = |_: &str, _: u64| Keep::All;
    let e = src
        .pass(
            Restart::START,
            &mut want,
            &mut Rec::default(),
            &AtomicBool::new(true),
        )
        .unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::Interrupted, "{e}");
}

#[test]
fn an_entrys_own_mtime_is_carried_into_the_manifest() {
    let d = temp("rar-mtime");
    let ents = [
        Ent::Dated("dated", b"hello", 1_709_209_840),
        Ent::File("undated", b"x"),
    ];
    write(&d.join("a.rar"), &rar5(&ents, false));
    let (m, _src) = RarSource::open(&d.join("a.rar"), None, &[]).unwrap();
    let t = |n: &str| m.entries.iter().find(|e| e.path == n).unwrap().mtime;
    // UnRAR rounds to 2 s in the host's zone; an even second round-trips exactly.
    if cfg!(unix) {
        assert_eq!(t("dated"), 1_709_209_840);
    }
    assert_eq!(t("undated"), 0);
}

#[test]
fn duplicates_and_case_clashes_are_terminal_not_a_fallback() {
    let d = temp("rar-terminal");
    let pool = Pool::new(d.join("ava")).with_addr("127.0.0.1:1");
    for (name, ents, what) in [
        (
            "dup",
            vec![Ent::File("a", b"1"), Ent::File("a", b"2")],
            "more than once",
        ),
        (
            "case",
            vec![Ent::File("A", b"1"), Ent::File("a", b"2")],
            "differ only in case",
        ),
        (
            "filedir",
            vec![Ent::File("x", b"1"), Ent::File("x/y", b"2")],
            "both a file and a directory",
        ),
    ] {
        write(&d.join(format!("{name}.rar")), &rar5(&ents, false));
        let e = upload::upload_rar_in(
            &pool,
            &cfg(),
            [12; 16],
            "dst",
            &d.join(format!("{name}.rar")),
            None,
        )
        .unwrap_err();
        assert!(
            e.downcast_ref::<upload::RarUnsupported>().is_none(),
            "{name}"
        );
        let f = e.downcast_ref::<UploadFailure>().expect(name);
        assert_eq!(f.reason, "ava1_rar_unsupported", "{name}");
        assert!(f.detail.contains(what), "{name}: {}", f.detail);
    }
    assert_eq!(pool.attempts(), 0);
}

// ---- R6 (#370): only the packages of a multi-package RAR -------------------------------

fn pkg_like(seed: u8, size: usize) -> Vec<u8> {
    let mut v = pattern(seed, 0, size);
    v[..4].copy_from_slice(b"\x7FCNT");
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn an_allow_list_unpacks_only_the_packages_from_folders() {
    for solid in [false, true] {
        let (d, _, pool) = setup(if solid { "rar-pkgs-solid" } else { "rar-pkgs" }).await;
        let base = pkg_like(1, 300_000);
        let patch = pkg_like(2, 120_000);
        let upper = pkg_like(3, 5_000);
        let readme = pattern(9, 0, 700);
        let nfo = pattern(8, 0, 90);
        let arch = rar5(
            &[
                Ent::Dir("Game"),
                Ent::Dir("Game/Update"),
                Ent::Dir("docs"),
                Ent::File("Game/readme.txt", &readme),
                Ent::File("Game/Base.pkg", &base),
                Ent::File("Game/Update/Patch.pkg", &patch),
                Ent::File("docs/info.nfo", &nfo),
                Ent::File("Loose.PKG", &upper),
            ],
            solid,
        );
        write(&d.join("m.rar"), &arch);
        let mut c = cfg();
        c.excludes = vec!["!*.pkg".into()];
        let archive = d.join("m.rar");
        let r = tokio::time::timeout(
            Duration::from_secs(90),
            tokio::task::spawn_blocking(move || {
                upload::upload_rar_in(&pool, &c, [5; 16], "dst", &archive, None)
            }),
        )
        .await
        .expect("timed out")
        .unwrap()
        .unwrap();
        assert_eq!(r.files_sent, 3, "solid={solid}");
        let root = d.join("host/share/dst");
        assert_eq!(std::fs::read(root.join("Game/Base.pkg")).unwrap(), base);
        assert_eq!(
            std::fs::read(root.join("Game/Update/Patch.pkg")).unwrap(),
            patch
        );
        assert_eq!(std::fs::read(root.join("Loose.PKG")).unwrap(), upper);
        // Nothing else came across, and a folder that held no package was not created.
        assert!(!root.join("Game/readme.txt").exists());
        assert!(!root.join("docs").exists(), "no empty shell for docs/");
    }
}

#[test]
fn the_package_listing_finds_nested_packages_and_ignores_the_rest() {
    let d = temp("rar-pkg-list");
    let patch = pkg_like(2, 100);
    let arch = rar5(
        &[
            Ent::File("a/readme.txt", b"hi"),
            Ent::File("a/b/One.pkg", &patch),
            Ent::File("Two.pkg", &patch),
        ],
        false,
    );
    write(&d.join("l.rar"), &arch);
    let layout =
        ps5upload_core::transfer::rar_layout(&d.join("l.rar"), None, &["!*.pkg".to_string()])
            .unwrap();
    let mut names: Vec<_> = layout.files.iter().map(|(n, _)| n.clone()).collect();
    names.sort();
    assert_eq!(names, ["Two.pkg", "a/b/One.pkg"]);
}
