mod common;

use std::io::Write;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use ava1::gen;
use ava1::keys::Identity;
use ava1::send::Progress;
use ava1_chaos::{ChaosConfig, ChaosProxy};
use common::*;
use ps5upload_ava1::relay::ps5_to_ps5_between;
use ps5upload_ava1::upload;
use ps5upload_ava1::zip_source::ZipSource;
use ps5upload_ava1::Pool;
use ps5upload_core::transfer::TransferConfig;

/// The multi-hundred-MiB relays run one at a time: concurrently, a debug build's
/// crypto starves the others past their time bounds (a CPU problem, not a relay one).
static HEAVY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test(flavor = "multi_thread")]
async fn a_tree_relays_between_two_hosts() {
    let _heavy = HEAVY.lock().await;
    let d = temp("tree");
    let ava = d.join("engine");
    let key = Identity::load_or_create(&ava.join("identity"))
        .unwrap()
        .public();
    let a = d.join("a");
    let b = d.join("b");
    std::fs::create_dir_all(a.join("share/src/nested")).unwrap();
    std::fs::create_dir_all(b.join("share")).unwrap();
    for i in 0..40u64 {
        let n = if i < 2 { 40 << 20 } else { 1000 + i };
        write_pattern(&a.join(format!("share/src/nested/f{i}")), i as u8, n);
    }
    std::fs::write(a.join("share/src/nested/empty"), []).unwrap();
    let (addr_a, addr_b) = (host(&a, key).await, host(&b, key).await);
    let (pool_a, pool_b) = (
        Pool::new(ava.clone()).with_addr(addr_a),
        Pool::new(ava).with_addr(addr_b),
    );
    let report = tokio::time::timeout(
        Duration::from_secs(90),
        tokio::task::spawn_blocking(move || {
            ps5_to_ps5_between(
                &pool_a,
                "a",
                "src",
                &pool_b,
                "b",
                "dst",
                [11; 16],
                Arc::new(Progress::default()),
                Arc::new(AtomicBool::new(false)),
            )
        }),
    )
    .await
    .expect("relay timed out")
    .unwrap()
    .unwrap();
    assert_eq!(report.status, gen::STATUS_OK);
    assert_eq!(report.files, 41);
    for i in 0..40u64 {
        let n = if i < 2 { 40 << 20 } else { 1000 + i };
        assert_pattern(&b.join(format!("share/dst/nested/f{i}")), i as u8, n);
    }
    assert!(std::fs::read(b.join("share/dst/nested/empty"))
        .unwrap()
        .is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_partly_durable_file_resumes_without_hanging() {
    let _heavy = HEAVY.lock().await;
    let d = temp("resume");
    let ava = d.join("engine");
    let key = Identity::load_or_create(&ava.join("identity"))
        .unwrap()
        .public();
    let a = d.join("a");
    let b = d.join("b");
    std::fs::create_dir_all(a.join("share/src")).unwrap();
    std::fs::create_dir_all(b.join("share")).unwrap();
    const SIZE: u64 = 160 << 20;
    write_pattern(&a.join("share/src/big"), 5, SIZE);
    let (addr_a, addr_b) = (host(&a, key).await, host(&b, key).await);
    let progress = Arc::new(Progress::default());
    let cancel = Arc::new(AtomicBool::new(false));
    let (first_progress, first_cancel) = (progress.clone(), cancel.clone());
    let (first_ava, first_a, first_b) = (ava.clone(), addr_a.clone(), addr_b.clone());
    let first = tokio::task::spawn_blocking(move || {
        let pa = Pool::new(first_ava.clone()).with_addr(first_a);
        let pb = Pool::new(first_ava).with_addr(first_b);
        ps5_to_ps5_between(
            &pa,
            "a",
            "src",
            &pb,
            "b",
            "dst",
            [15; 16],
            first_progress,
            first_cancel,
        )
    });
    tokio::time::timeout(Duration::from_secs(40), async {
        while progress.bytes_durable.load(Ordering::Relaxed) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first attempt made no durable progress");
    assert!(
        progress.bytes_durable.load(Ordering::Relaxed) < SIZE,
        "first attempt finished before it could be interrupted"
    );
    cancel.store(true, Ordering::Relaxed);
    let _ = tokio::time::timeout(Duration::from_secs(40), first)
        .await
        .expect("cancelled attempt hung")
        .unwrap();
    let durable_before = progress.bytes_durable.load(Ordering::Relaxed);
    let second_progress = Arc::new(Progress::default());
    let sp = second_progress.clone();
    let second = tokio::time::timeout(
        Duration::from_secs(90),
        tokio::task::spawn_blocking(move || {
            let pa = Pool::new(ava.clone()).with_addr(addr_a);
            let pb = Pool::new(ava).with_addr(addr_b);
            ps5_to_ps5_between(
                &pa,
                "a",
                "src",
                &pb,
                "b",
                "dst",
                [15; 16],
                sp,
                Arc::new(AtomicBool::new(false)),
            )
        }),
    )
    .await
    .expect("resumed attempt hung")
    .unwrap()
    .unwrap();
    assert_eq!(second.status, gen::STATUS_OK);
    assert_eq!(second.bytes, SIZE);
    assert!(
        second_progress.resent_bytes.load(Ordering::Relaxed) <= 64 << 20,
        "resent {} bytes",
        second_progress.resent_bytes.load(Ordering::Relaxed)
    );
    // The resume must have skipped what was already durable, not re-sent it all.
    assert!(
        second_progress.bytes_sent.load(Ordering::Relaxed) <= SIZE - durable_before + (64 << 20),
        "sent {} after {durable_before} durable",
        second_progress.bytes_sent.load(Ordering::Relaxed)
    );
    assert_pattern(&b.join("share/dst/big"), 5, SIZE);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_destination_connection_killed_midway_resumes() {
    let _heavy = HEAVY.lock().await;
    let d = temp("kill-destination");
    let ava = d.join("engine");
    let key = Identity::load_or_create(&ava.join("identity"))
        .unwrap()
        .public();
    let a = d.join("a");
    let b = d.join("b");
    std::fs::create_dir_all(a.join("share/src")).unwrap();
    std::fs::create_dir_all(b.join("share")).unwrap();
    const SIZE: u64 = 48 << 20;
    write_pattern(&a.join("share/src/big"), 9, SIZE);
    let (addr_a, addr_b) = (host(&a, key).await, host(&b, key).await);
    let proxy = ChaosProxy::start(
        addr_b.parse().unwrap(),
        ChaosConfig {
            bytes_per_sec: Some(4 << 20),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let pa = Arc::new(Pool::new(ava.clone()).with_addr(addr_a));
    let pb = Arc::new(Pool::new(ava).with_addr(proxy.addr.to_string()));
    let progress = Arc::new(Progress::default());
    let (pa2, pb2, progress2) = (pa.clone(), pb.clone(), progress.clone());
    let run = tokio::task::spawn_blocking(move || {
        ps5_to_ps5_between(
            &pa2,
            "a",
            "src",
            &pb2,
            "b",
            "dst",
            [16; 16],
            progress2,
            Arc::new(AtomicBool::new(false)),
        )
    });
    tokio::time::timeout(Duration::from_secs(40), async {
        while progress.bytes_durable.load(Ordering::Relaxed) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("relay made no durable progress");
    assert!(progress.bytes_durable.load(Ordering::Relaxed) < SIZE);
    proxy.kill_all();
    let report = tokio::time::timeout(Duration::from_secs(90), run)
        .await
        .expect("killed destination did not resume")
        .unwrap()
        .unwrap();
    assert_eq!(report.status, gen::STATUS_OK);
    assert_eq!(report.bytes, SIZE);
    assert!(pb.attempts() >= 2, "destination was never reconnected");
    assert!(progress.resent_bytes.load(Ordering::Relaxed) <= 64 << 20);
    assert_pattern(&b.join("share/dst/big"), 9, SIZE);
}

/// The destination payload restarts mid-relay but keeps its journal: the relay
/// resumes from B's durable map, sends little twice, and the bytes are right.
#[tokio::test(flavor = "multi_thread")]
async fn a_destination_restart_that_keeps_its_journal_resumes() {
    let _heavy = HEAVY.lock().await;
    let d = temp("restart-destination");
    let ava = d.join("engine");
    let key = Identity::load_or_create(&ava.join("identity"))
        .unwrap()
        .public();
    let a = d.join("a");
    let b = d.join("b");
    std::fs::create_dir_all(a.join("share/src")).unwrap();
    std::fs::create_dir_all(b.join("share")).unwrap();
    const SIZE: u64 = 96 << 20;
    write_pattern(&a.join("share/src/big"), 21, SIZE);
    write_pattern(&a.join("share/src/tail"), 22, 70_000);
    let addr_a = host(&a, key).await;
    let mut hb = Restartable::start(&b, key).await;
    let proxy = ChaosProxy::start(
        hb.addr.parse().unwrap(),
        ChaosConfig {
            bytes_per_sec: Some(12 << 20),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let pa = Arc::new(Pool::new(ava.clone()).with_addr(addr_a));
    let pb = Arc::new(Pool::new(ava).with_addr(proxy.addr.to_string()));
    let progress = Arc::new(Progress::default());
    let (pa2, pb2, progress2) = (pa.clone(), pb.clone(), progress.clone());
    let run = tokio::task::spawn_blocking(move || {
        ps5_to_ps5_between(
            &pa2,
            "a",
            "src",
            &pb2,
            "b",
            "dst",
            [17; 16],
            progress2,
            Arc::new(AtomicBool::new(false)),
        )
    });
    tokio::time::timeout(Duration::from_secs(40), async {
        while progress.bytes_durable.load(Ordering::Relaxed) < (16 << 20) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("relay made no durable progress");
    assert!(progress.bytes_durable.load(Ordering::Relaxed) < SIZE);
    hb.restart().await;
    proxy.kill_all();
    let report = tokio::time::timeout(Duration::from_secs(120), run)
        .await
        .expect("restarted destination did not resume")
        .unwrap()
        .unwrap();
    assert_eq!(report.status, gen::STATUS_OK);
    assert!(pb.attempts() >= 2, "destination was never reconnected");
    assert!(
        progress.resent_bytes.load(Ordering::Relaxed) <= 64 << 20,
        "resent {}",
        progress.resent_bytes.load(Ordering::Relaxed)
    );
    assert_pattern(&b.join("share/dst/big"), 21, SIZE);
    assert_pattern(&b.join("share/dst/tail"), 22, 70_000);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_zip_uploads_and_matches_its_contents() {
    let d = temp("upload-zip");
    let ava = d.join("engine");
    let key = Identity::load_or_create(&ava.join("identity"))
        .unwrap()
        .public();
    let host_root = d.join("host");
    std::fs::create_dir_all(host_root.join("share")).unwrap();
    let addr = host(&host_root, key).await;
    let path = d.join("input.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    for i in 0..200 {
        let method = if i == 199 {
            zip::CompressionMethod::Stored
        } else {
            zip::CompressionMethod::Deflated
        };
        let name = format!("nested/f{i}");
        zip.start_file(
            &name,
            zip::write::SimpleFileOptions::default().compression_method(method),
        )
        .unwrap();
        let n = if i < 2 { 3 << 20 } else { i + 100 };
        zip.write_all(&vec![i as u8; n]).unwrap();
    }
    zip.finish().unwrap();
    let pool = Pool::new(ava).with_addr(addr);
    let result = tokio::time::timeout(
        Duration::from_secs(90),
        tokio::task::spawn_blocking(move || {
            upload::upload_zip_in(
                &pool,
                &TransferConfig::new("127.0.0.1"),
                [14; 16],
                "dst",
                &path,
            )
        }),
    )
    .await
    .expect("zip upload timed out")
    .unwrap()
    .unwrap();
    assert_eq!(result.files_sent, 200);
    for i in 0..200 {
        let n = if i < 2 { 3 << 20 } else { i + 100 };
        assert_eq!(
            std::fs::read(host_root.join(format!("share/dst/nested/f{i}"))).unwrap(),
            vec![i as u8; n]
        );
    }
}

#[test]
fn traversal_zip_fails_before_connecting() {
    let d = temp("traversal");
    let path = d.join("bad.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    zip.start_file("../evil", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"bad").unwrap();
    zip.finish().unwrap();
    let pool = Pool::new(d.join("ava")).with_addr("127.0.0.1:1");
    let err = upload::upload_zip_in(
        &pool,
        &TransferConfig::new("127.0.0.1:1"),
        [12; 16],
        "dst",
        &path,
    )
    .unwrap_err();
    assert!(err.to_string().contains("../evil"), "{err:#}");
    // The engine maps this type to `zip_unsupported`.
    assert!(err.downcast_ref::<upload::ZipUnsupported>().is_some());
}

/// Overwrites the CRC-32 field of every central-directory record.
fn break_central_crcs(path: &std::path::Path) {
    let mut b = std::fs::read(path).unwrap();
    let mut i = 0;
    while i + 20 < b.len() {
        if b[i..i + 4] == [0x50, 0x4b, 0x01, 0x02] {
            for k in 0..4 {
                b[i + 16 + k] ^= 0xa5;
            }
        }
        i += 1;
    }
    std::fs::write(path, b).unwrap();
}

fn upload_zip_blocking(
    d: &std::path::Path,
    ava: std::path::PathBuf,
    addr: String,
    path: std::path::PathBuf,
    id: u8,
) -> anyhow::Result<ps5upload_core::transfer::TransferResult> {
    let _ = d;
    let pool = Pool::new(ava).with_addr(addr);
    upload::upload_zip_in(
        &pool,
        &TransferConfig::new("127.0.0.1"),
        [id; 16],
        "dst",
        &path,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_zip_with_a_wrong_crc_fails_typed() {
    for method in [
        zip::CompressionMethod::Deflated,
        zip::CompressionMethod::Stored,
    ] {
        let d = temp("zip-bad-crc");
        let ava = d.join("engine");
        let key = Identity::load_or_create(&ava.join("identity"))
            .unwrap()
            .public();
        let host_root = d.join("host");
        std::fs::create_dir_all(host_root.join("share")).unwrap();
        let addr = host(&host_root, key).await;
        let path = d.join("bad.zip");
        zip_with(&path, &[("f", method, 3, 1 << 20)]);
        break_central_crcs(&path);
        let (d2, ava2) = (d.clone(), ava.clone());
        let err = tokio::time::timeout(
            Duration::from_secs(60),
            tokio::task::spawn_blocking(move || upload_zip_blocking(&d2, ava2, addr, path, 31)),
        )
        .await
        .expect("timed out")
        .unwrap()
        .unwrap_err();
        let f = err
            .downcast_ref::<upload::UploadFailure>()
            .unwrap_or_else(|| panic!("not typed: {err:#}"));
        assert_eq!(f.reason, "ava1_zip_corrupt");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_zip_with_a_flipped_data_byte_fails_typed() {
    let d = temp("zip-flip");
    let ava = d.join("engine");
    let key = Identity::load_or_create(&ava.join("identity"))
        .unwrap()
        .public();
    let host_root = d.join("host");
    std::fs::create_dir_all(host_root.join("share")).unwrap();
    let addr = host(&host_root, key).await;
    let path = d.join("flip.zip");
    zip_with(
        &path,
        &[("f", zip::CompressionMethod::Deflated, 5, 1 << 20)],
    );
    let mut b = std::fs::read(&path).unwrap();
    let mid = b.len() / 3;
    b[mid] ^= 0x40;
    std::fs::write(&path, b).unwrap();
    let (d2, ava2) = (d.clone(), ava.clone());
    let err = tokio::time::timeout(
        Duration::from_secs(60),
        tokio::task::spawn_blocking(move || upload_zip_blocking(&d2, ava2, addr, path, 32)),
    )
    .await
    .expect("timed out")
    .unwrap()
    .unwrap_err();
    let f = err.downcast_ref::<upload::UploadFailure>().expect("typed");
    assert_eq!(f.reason, "ava1_zip_corrupt");
}

#[test]
fn an_entry_read_in_pieces_is_verified_once_and_correct() {
    use ava1::source::ReadAt;
    let d = temp("zip-pieces");
    let path = d.join("p.zip");
    const SIZE: u64 = 3 << 20;
    zip_with(
        &path,
        &[
            ("d", zip::CompressionMethod::Deflated, 7, SIZE),
            ("s", zip::CompressionMethod::Stored, 8, SIZE),
        ],
    );
    let (_, source) = ZipSource::open(&path, &[]).unwrap();
    for (name, seed) in [("d", 7u8), ("s", 8u8)] {
        let mut r = source.open_entry(name).unwrap();
        let mut off = 0u64;
        let mut buf = vec![0u8; 300_000];
        while off < SIZE {
            let n = ava1::source::read_full_at(&mut r, off, &mut buf).unwrap();
            assert!(
                buf[..n] == pattern(seed, off, n)[..],
                "{name} differs at {off}"
            );
            off += n as u64;
        }
        // One backwards seek, then the whole entry again: still correct, still ok.
        let mut small = [0u8; 64];
        r.read_at(1000, &mut small).unwrap();
        assert_eq!(&small[..], &pattern(seed, 1000, 64)[..]);
        let mut off = 0u64;
        while off < SIZE {
            let n = ava1::source::read_full_at(&mut r, off, &mut buf).unwrap();
            off += n as u64;
        }
        if name == "d" {
            assert_eq!(r.restarts(), 3);
        }
    }
    break_central_crcs(&path);
    let (_, source) = ZipSource::open(&path, &[]).unwrap();
    let mut r = source.open_entry("d").unwrap();
    let mut buf = vec![0u8; 4096];
    assert_eq!(r.read_at(0, &mut buf).unwrap(), 4096);
    // Reaching the end of the entry is what verifies it.
    let e = r.read_at(SIZE - 100, &mut buf).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    assert!(ps5upload_ava1::zip_source::is_zip_corrupt(&e));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_zip_entry_over_256_mib_uploads_over_ava1() {
    let _heavy = HEAVY.lock().await;
    let d = temp("zip-big");
    let ava = d.join("engine");
    let key = Identity::load_or_create(&ava.join("identity"))
        .unwrap()
        .public();
    let host_root = d.join("host");
    std::fs::create_dir_all(host_root.join("share")).unwrap();
    let addr = host(&host_root, key).await;
    let path = d.join("big.zip");
    const SIZE: u64 = (256 << 20) + (3 << 20) + 17;
    {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file(
            "big",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .large_file(true),
        )
        .unwrap();
        let chunk = vec![0x5au8; 1 << 20];
        let mut off = 0;
        while off < SIZE {
            let n = (SIZE - off).min(chunk.len() as u64) as usize;
            zip.write_all(&chunk[..n]).unwrap();
            off += n as u64;
        }
        zip.finish().unwrap();
    }
    assert!(std::fs::metadata(&path).unwrap().len() < 8 << 20);
    let (d2, ava2) = (d.clone(), ava.clone());
    let result = tokio::time::timeout(
        Duration::from_secs(600),
        tokio::task::spawn_blocking(move || upload_zip_blocking(&d2, ava2, addr, path, 33)),
    )
    .await
    .expect("big zip upload timed out")
    .unwrap()
    .unwrap();
    assert_eq!(result.files_sent, 1);
    let out = host_root.join("share/dst/big");
    assert_eq!(std::fs::metadata(&out).unwrap().len(), SIZE);
    let mut f = std::fs::File::open(&out).unwrap();
    let mut buf = vec![0u8; 1 << 20];
    let mut seen = 0u64;
    while seen < SIZE {
        use std::io::Read;
        let n = f.read(&mut buf).unwrap();
        assert!(n > 0);
        assert!(buf[..n].iter().all(|&b| b == 0x5a), "bad byte near {seen}");
        seen += n as u64;
    }
}

#[test]
fn zip_source_reads_nested_and_stored_entries() {
    let d = temp("zip");
    let path = d.join("input.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    zip.start_file(
        "nested/one",
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated),
    )
    .unwrap();
    zip.write_all(b"hello world").unwrap();
    zip.start_file(
        "two",
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )
    .unwrap();
    zip.write_all(b"stored").unwrap();
    zip.finish().unwrap();
    let (m, source) = ZipSource::open(&path, &[]).unwrap();
    assert_eq!(m.files(), 2);
    let mut r = ava1::source::Source::open(&source, "nested/one").unwrap();
    let mut buf = [0u8; 5];
    assert_eq!(r.read_at(6, &mut buf).unwrap(), 5);
    assert_eq!(&buf, b"world");
}

fn zip_with(path: &std::path::Path, entries: &[(&str, zip::CompressionMethod, u8, u64)]) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, method, seed, size) in entries {
        zip.start_file(
            *name,
            zip::write::SimpleFileOptions::default().compression_method(*method),
        )
        .unwrap();
        let mut off = 0;
        while off < *size {
            let n = (*size - off).min(1 << 20) as usize;
            zip.write_all(&pattern(*seed, off, n)).unwrap();
            off += n as u64;
        }
    }
    zip.finish().unwrap();
}

#[test]
fn a_large_deflated_entry_inflates_once() {
    use ava1::source::ReadAt;
    let d = temp("inflate-once");
    let path = d.join("big.zip");
    const SIZE: u64 = 32 << 20;
    zip_with(&path, &[("big", zip::CompressionMethod::Deflated, 6, SIZE)]);
    let (_, source) = ZipSource::open(&path, &[]).unwrap();
    let mut r = source.open_entry("big").unwrap();
    let started = std::time::Instant::now();
    let mut off = 0u64;
    let mut buf = vec![0u8; 1 << 20];
    while off < SIZE {
        let n = ava1::source::read_full_at(&mut r, off, &mut buf).unwrap();
        assert_eq!(n, buf.len());
        assert!(buf == pattern(6, off, n), "bytes differ at {off}");
        off += n as u64;
    }
    assert_eq!(
        r.restarts(),
        1,
        "front-to-back must not restart the inflater"
    );
    assert!(started.elapsed() < Duration::from_secs(30));
    // Going backwards restarts exactly once more; a stored read never does.
    let mut small = [0u8; 100];
    r.read_at(10, &mut small).unwrap();
    assert_eq!(&small[..], &pattern(6, 10, 100)[..]);
    assert_eq!(r.restarts(), 2);
}

#[test]
fn a_stored_entry_reads_at_any_offset() {
    use ava1::source::ReadAt;
    let d = temp("stored-random");
    let path = d.join("s.zip");
    zip_with(
        &path,
        &[
            ("a", zip::CompressionMethod::Stored, 1, 100_000),
            ("b", zip::CompressionMethod::Stored, 2, 100_000),
        ],
    );
    let (_, source) = ZipSource::open(&path, &[]).unwrap();
    let mut r = source.open_entry("b").unwrap();
    let mut buf = [0u8; 50];
    assert_eq!(r.read_at(99_000, &mut buf).unwrap(), 50);
    assert_eq!(&buf[..], &pattern(2, 99_000, 50)[..]);
    assert_eq!(r.read_at(5, &mut buf).unwrap(), 50);
    assert_eq!(&buf[..], &pattern(2, 5, 50)[..]);
    assert_eq!(r.read_at(100_000, &mut buf).unwrap(), 0);
}

#[test]
fn an_archive_the_source_cannot_read_is_rejected_at_open() {
    let d = temp("unsupported");
    let path = d.join("bzip.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    zip.start_file(
        "x",
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )
    .unwrap();
    zip.write_all(b"data").unwrap();
    zip.finish().unwrap();
    // Patch the central directory's method field (offset 10 of the record) to bzip2, which is not built in.
    let mut bytes = std::fs::read(&path).unwrap();
    let cd = bytes
        .windows(4)
        .rposition(|w| w == [0x50, 0x4b, 0x01, 0x02])
        .unwrap();
    bytes[cd + 10] = 12;
    std::fs::write(&path, bytes).unwrap();
    let err = ZipSource::open(&path, &[]).err().expect("must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err}");
}

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

#[tokio::test(flavor = "multi_thread")]
async fn a_relay_to_a_console_that_refuses_connections_is_terminal() {
    let d = temp("relay-refused");
    let ava = d.join("engine");
    let key = Identity::load_or_create(&ava.join("identity"))
        .unwrap()
        .public();
    let a = d.join("a");
    std::fs::create_dir_all(a.join("share/src")).unwrap();
    let addr_a = host(&a, key).await;
    let (pa, pb) = (
        Pool::new(ava.clone()).with_addr(addr_a),
        Pool::new(ava).with_addr("127.0.0.1:1"),
    );
    let (reason, took) = tokio::task::spawn_blocking(move || relay_failure(pa, pb))
        .await
        .unwrap();
    assert_eq!(reason, "ava1_unreachable");
    assert!(took < Duration::from_secs(20), "{took:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relay_without_an_identity_fails_at_once() {
    let d = temp("relay-noid");
    std::fs::create_dir_all(d.join("ava/identity")).unwrap();
    let (pa, pb) = (
        Pool::new(d.join("ava")).with_addr("127.0.0.1:1"),
        Pool::new(d.join("ava")).with_addr("127.0.0.1:1"),
    );
    let (reason, took) = tokio::task::spawn_blocking(move || relay_failure(pa, pb))
        .await
        .unwrap();
    assert_eq!(reason, "ava1_no_identity");
    assert!(took < Duration::from_secs(3), "{took:?}");
}

#[test]
fn a_failed_read_restarts_the_inflater_instead_of_continuing_stale() {
    let d = temp("zip-retry");
    let path = d.join("r.zip");
    const SIZE: u64 = 2 << 20;
    zip_with(&path, &[("e", zip::CompressionMethod::Deflated, 4, SIZE)]);
    let (_, source) = ZipSource::open(&path, &[]).unwrap();
    let good = std::fs::read(&path).unwrap();
    // The archive is cut short under the open source: the read hits EOF mid-stream.
    std::fs::write(&path, &good[..good.len() / 2]).unwrap();
    let mut r = source.open_entry("e").unwrap();
    let mut buf = vec![0u8; 1 << 20];
    let mut off = 0u64;
    let failed_at = loop {
        match ava1::source::read_full_at(&mut r, off, &mut buf) {
            Ok(n) => {
                assert!(buf[..n] == pattern(4, off, n)[..], "bytes differ at {off}");
                off += n as u64;
            }
            Err(_) => break off,
        }
        assert!(off < SIZE, "the truncated archive never failed");
    };
    assert_eq!(r.restarts(), 1);
    // The file is whole again; the retry at the same offset must be right.
    std::fs::write(&path, &good).unwrap();
    let n = ava1::source::read_full_at(&mut r, failed_at, &mut buf).unwrap();
    assert!(
        buf[..n] == pattern(4, failed_at, n)[..],
        "stale bytes after a failure"
    );
    assert_eq!(r.restarts(), 2, "a failed read must restart the inflater");
}

#[test]
fn a_zip_entrys_own_mtime_is_carried_into_the_manifest() {
    let d = temp("zip-mtime");
    let path = d.join("t.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let when = zip::DateTime::from_date_and_time(2024, 2, 29, 12, 30, 40).unwrap();
    zip.start_file(
        "dated",
        zip::write::SimpleFileOptions::default().last_modified_time(when),
    )
    .unwrap();
    zip.write_all(b"x").unwrap();
    zip.finish().unwrap();
    let (m, _src) = ps5upload_ava1::zip_source::ZipSource::open(&path, &[]).unwrap();
    assert_eq!(m.entries[0].mtime, 1_709_209_840);
}
