//! AVA1 integration tests for the upload paths: single file, folder, file list, excludes, packed
//! small files, cancel and resume, a source that is not local disk, and the loopback throughput
//! gates. They replace `transfer_integration.rs` (the retired protocol's mock-server tests); each
//! test names the old one it ports in its doc comment, and the commit that adds this file carries
//! the whole mapping table.
//!
//! The console is the Rust job host over a temp folder (`ava1_common::console`): the same
//! receiver, journal and apply code the payload's C server is tested against.

mod ava1_common;
use ava1_common::*;

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ps5upload_ava1::upload;
use ps5upload_core::transfer::{FileListEntry, TransferConfig};

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Uploads `src` (a file) to `dest` under the console's share and returns the result.
async fn put_file(
    c: &Console,
    id: u8,
    dest: &str,
    src: &Path,
) -> anyhow::Result<ps5upload_core::transfer::TransferResult> {
    let (pool, dest, src) = (c.pool.clone(), dest.to_string(), src.to_path_buf());
    run(300, move || {
        upload::upload_file_in(&pool, &cfg(), job_id(id), &dest, &src)
    })
    .await
}

async fn put_dir(
    c: &Console,
    id: u8,
    dest_root: &str,
    src: &Path,
    config: TransferConfig,
) -> anyhow::Result<ps5upload_core::transfer::TransferResult> {
    let (pool, dest, src) = (c.pool.clone(), dest_root.to_string(), src.to_path_buf());
    run(120, move || {
        upload::upload_dir_in(&pool, &config, job_id(id), &dest, &src)
    })
    .await
}

// ─── Single-file transfer ──────────────────────────────────────────────────────

/// Ports `transfer_file_small`.
#[tokio::test(flavor = "multi_thread")]
async fn upload_file_small() {
    let c = console().await;
    let t = tempdir();
    let data = b"hello from ps5upload integration test";
    write(&t.path().join("test.txt"), data);
    let r = put_file(&c, 1, "data/test.txt", &t.path().join("test.txt"))
        .await
        .unwrap();
    assert_eq!(r.bytes_sent, data.len() as u64);
    assert_eq!(std::fs::read(c.share.join("data/test.txt")).unwrap(), data);
}

/// Ports `transfer_file_empty_data`: a zero-byte file still lands (present, zero length).
#[tokio::test(flavor = "multi_thread")]
async fn upload_file_empty_lands_present() {
    let c = console().await;
    let t = tempdir();
    write(&t.path().join("empty.bin"), b"");
    let r = put_file(&c, 2, "data/empty.bin", &t.path().join("empty.bin"))
        .await
        .unwrap();
    assert_eq!(r.bytes_sent, 0);
    let got = std::fs::read(c.share.join("data/empty.bin")).expect("the empty file exists");
    assert!(got.is_empty());
}

/// Ports `transfer_file_multi_shard` and `transfer_file_path_streams_from_disk`: a file larger
/// than one chunk, streamed from disk, lands byte for byte.
#[tokio::test(flavor = "multi_thread")]
async fn upload_file_spanning_many_chunks_is_byte_exact() {
    let c = console().await;
    let t = tempdir();
    let data = pattern(7, 5 * 1024 * 1024 + 123);
    write(&t.path().join("multi.bin"), &data);
    let r = put_file(&c, 3, "data/multi.bin", &t.path().join("multi.bin"))
        .await
        .unwrap();
    assert_eq!(r.bytes_sent, data.len() as u64);
    assert!(std::fs::read(c.share.join("data/multi.bin")).unwrap() == data);
}

/// Ports `transfer_with_resume_flag_on_fresh_txid_is_noop`: a job id the console has never seen
/// uploads everything, exactly as a plain upload does.
#[tokio::test(flavor = "multi_thread")]
async fn upload_with_a_job_id_the_console_never_saw_sends_everything() {
    let c = console().await;
    let t = tempdir();
    let data = vec![0xC3u8; 200_000];
    write(&t.path().join("fresh.bin"), &data);
    let r = put_file(&c, 0xF0, "data/fresh.bin", &t.path().join("fresh.bin"))
        .await
        .unwrap();
    assert_eq!(r.bytes_sent, data.len() as u64);
    assert_eq!(std::fs::read(c.share.join("data/fresh.bin")).unwrap(), data);
}

// ─── Folder transfer ───────────────────────────────────────────────────────────

/// Ports `transfer_dir_basic`.
#[tokio::test(flavor = "multi_thread")]
async fn upload_dir_basic() {
    let c = console().await;
    let t = tempdir();
    write(&t.path().join("a.txt"), b"file-a");
    write(&t.path().join("b.txt"), b"file-b contents here");
    write(&t.path().join("sub/c.txt"), b"nested-c");
    let r = put_dir(&c, 4, "data/dest", t.path(), cfg()).await.unwrap();
    assert_eq!(r.bytes_sent, 6 + 20 + 8);
    let got = landed(&c.share.join("data/dest"));
    assert_eq!(got.len(), 3);
    assert_eq!(got["a.txt"], b"file-a");
    assert_eq!(got["b.txt"], b"file-b contents here");
    assert_eq!(got["sub/c.txt"], b"nested-c");
}

/// Ports `transfer_dir_multi_shard_per_file`: one file much larger than a chunk.
#[tokio::test(flavor = "multi_thread")]
async fn upload_dir_with_one_large_file() {
    let c = console().await;
    let t = tempdir();
    let data = pattern(3, 3 * 1024 * 1024 + 1);
    write(&t.path().join("big.bin"), &data);
    let r = put_dir(&c, 5, "data/d", t.path(), cfg()).await.unwrap();
    assert_eq!(r.bytes_sent, data.len() as u64);
    assert!(std::fs::read(c.share.join("data/d/big.bin")).unwrap() == data);
}

/// Ports `transfer_dir_single_small_file_is_packed_kind2`: a folder that narrows to one small
/// file still uploads as a folder (not as a single-file job) and round-trips byte-exact.
#[tokio::test(flavor = "multi_thread")]
async fn upload_dir_with_one_small_file() {
    let c = console().await;
    let t = tempdir();
    let data: Vec<u8> = (0..777u32).map(|j| (j & 0xff) as u8).collect();
    write(&t.path().join("solo.bin"), &data);
    let r = put_dir(&c, 6, "data/solo_dir", t.path(), cfg())
        .await
        .unwrap();
    assert_eq!(r.bytes_sent, 777);
    assert_eq!(
        std::fs::read(c.share.join("data/solo_dir/solo.bin")).unwrap(),
        data
    );
    assert_eq!(landed(&c.share.join("data/solo_dir")).len(), 1);
}

/// Ports `transfer_dir_packed_small_files`: many tiny files all arrive, each at its own path, and
/// nothing else appears (no pollution of the destination root).
#[tokio::test(flavor = "multi_thread")]
async fn upload_dir_of_many_small_files() {
    let c = console().await;
    let t = tempdir();
    for i in 0..20u32 {
        let payload: Vec<u8> = (0..512u32).map(|j| ((i * 31 + j) & 0xff) as u8).collect();
        write(&t.path().join(format!("f{i:03}.bin")), &payload);
    }
    let r = put_dir(&c, 7, "data/p", t.path(), cfg()).await.unwrap();
    assert_eq!(r.bytes_sent, 20 * 512);
    let got = landed(&c.share.join("data/p"));
    assert_eq!(got.len(), 20, "exactly the source files, no more");
    for i in 0..20u32 {
        let want: Vec<u8> = (0..512u32).map(|j| ((i * 31 + j) & 0xff) as u8).collect();
        assert_eq!(got[&format!("f{i:03}.bin")], want, "file {i}");
    }
}

/// The names and sizes that make a game dump hard: nested dirs, spaces, non-ASCII, leading dots,
/// zero- and one-byte files, sizes that straddle a chunk boundary.
fn adversarial_files() -> Vec<(&'static str, usize)> {
    vec![
        ("eboot.bin", 1500),
        ("sce_sys/param.json", 800),
        ("sce_sys/icon0.png", 0),
        ("sce_sys/about/right.sprx", 1),
        ("Image0/deep/nested/dir/data.dat", 2049),
        ("name with spaces.bin", 600),
        ("unicodé_名前.bin", 700),
        ("dots...in.name", 333),
    ]
}

/// Ports `transfer_dir_adversarial_packed_roundtrip`: every file byte-exact at the right path,
/// nothing invented.
#[tokio::test(flavor = "multi_thread")]
async fn upload_dir_adversarial_names_round_trip() {
    let c = console().await;
    let t = tempdir();
    let files = adversarial_files();
    for (i, (rel, sz)) in files.iter().enumerate() {
        write(&t.path().join(rel), &pattern(i + 1, *sz));
    }
    let r = put_dir(&c, 8, "data/game", t.path(), cfg()).await.unwrap();
    assert_eq!(
        r.bytes_sent,
        files.iter().map(|(_, s)| *s as u64).sum::<u64>()
    );
    let got = landed(&c.share.join("data/game"));
    assert_eq!(got.len(), files.len(), "no more, no less");
    for (i, (rel, sz)) in files.iter().enumerate() {
        assert!(
            got[*rel] == pattern(i + 1, *sz),
            "{rel}: wrong path or wrong/corrupt content"
        );
    }
}

/// Ports `transfer_dir_respects_default_excludes`.
#[tokio::test(flavor = "multi_thread")]
async fn upload_dir_respects_default_excludes() {
    let c = console().await;
    let t = tempdir();
    write(&t.path().join("eboot.bin"), &[0u8; 1024]);
    write(&t.path().join(".DS_Store"), b"junk");
    write(&t.path().join("map.esbak"), b"editor backup");
    write(&t.path().join(".git/HEAD"), b"ref: refs/heads/main");
    write(&t.path().join("Thumbs.db"), b"windows junk");
    let r = put_dir(
        &c,
        9,
        "data/excluded",
        t.path(),
        cfg().with_default_excludes(),
    )
    .await
    .unwrap();
    assert_eq!(r.bytes_sent, 1024, "only eboot.bin bytes transferred");
    let got = landed(&c.share.join("data/excluded"));
    assert_eq!(got.keys().collect::<Vec<_>>(), vec!["eboot.bin"]);
}

/// Ports `transfer_dir_without_excludes_includes_everything`: the filter is opt-in.
#[tokio::test(flavor = "multi_thread")]
async fn upload_dir_without_excludes_includes_everything() {
    let c = console().await;
    let t = tempdir();
    write(&t.path().join("a.bin"), b"real");
    write(&t.path().join(".DS_Store"), b"junk");
    put_dir(&c, 10, "data/all", t.path(), cfg()).await.unwrap();
    let got = landed(&c.share.join("data/all"));
    assert!(got.contains_key("a.bin"));
    assert!(got.contains_key(".DS_Store"), "default: include everything");
}

fn assert_folder_landed(root: &Path, expected: &[(String, Vec<u8>)], ctx: &str) {
    let got = landed(root);
    for (rel, bytes) in expected {
        assert!(
            got.get(rel).map(|v| v == bytes).unwrap_or(false),
            "{ctx}: {rel} wrong path or wrong/corrupt content"
        );
    }
    assert_eq!(
        got.len(),
        expected.len(),
        "{ctx}: exactly the real files should land: junk leaked or files invented"
    );
}

/// Ports `transfer_dir_byte_exact_100x`: the same realistic folder, a hundred times, lands
/// identical bytes at identical paths every time, junk filtered, nothing invented.
#[tokio::test(flavor = "multi_thread")]
async fn upload_dir_byte_exact_100x() {
    let _turn = heavy().await;
    let t = tempdir();
    let expected = build_game_folder(t.path());
    let total: u64 = expected.iter().map(|(_, b)| b.len() as u64).sum();
    let c = console().await;
    for iter in 0..100u8 {
        let root = format!("data/game{iter}");
        let r = put_dir(&c, iter, &root, t.path(), cfg().with_default_excludes())
            .await
            .unwrap_or_else(|e| panic!("iter {iter}: upload failed: {e:#}"));
        assert_eq!(r.bytes_sent, total, "iter {iter}: total bytes");
        assert_folder_landed(&c.share.join(&root), &expected, &format!("iter {iter}"));
    }
}

/// Ports `transfer_dir_progress_files_counter_climbs_per_file`: the per-file counter ends at the
/// source-file count (one per file however the bytes were grouped).
#[tokio::test(flavor = "multi_thread")]
async fn upload_dir_progress_files_counter_counts_every_file() {
    let t = tempdir();
    let expected = build_game_folder(t.path());
    let c = console().await;
    let files = Arc::new(AtomicU64::new(0));
    let mut config = cfg().with_default_excludes();
    config.progress_files = Some(Arc::clone(&files));
    let r = put_dir(&c, 11, "data/game", t.path(), config)
        .await
        .unwrap();
    assert_eq!(
        r.bytes_sent,
        expected.iter().map(|(_, b)| b.len() as u64).sum::<u64>()
    );
    assert_eq!(files.load(Ordering::Relaxed), expected.len() as u64);
}

/// Ports `transfer_dir_from_a_source_fs_arrives_byte_for_byte`: a folder served from somewhere
/// other than local disk (a saved server, in the engine) uploads byte for byte.
#[tokio::test(flavor = "multi_thread")]
async fn upload_dir_from_a_source_fs_arrives_byte_for_byte() {
    use ps5upload_core::source_fs::{ReadSeek, SourceFs, SourceMeta};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[derive(Debug)]
    struct Mem(BTreeMap<PathBuf, Vec<u8>>);
    impl SourceFs for Mem {
        fn open(&self, p: &Path) -> std::io::Result<Box<dyn ReadSeek>> {
            let b = self.0.get(p).cloned().ok_or(std::io::ErrorKind::NotFound)?;
            Ok(Box::new(std::io::Cursor::new(b)))
        }
        fn metadata(&self, p: &Path) -> std::io::Result<SourceMeta> {
            if let Some(b) = self.0.get(p) {
                return Ok(SourceMeta {
                    len: b.len() as u64,
                    is_dir: false,
                    is_file: true,
                });
            }
            if self.0.keys().any(|k| k.starts_with(p)) {
                return Ok(SourceMeta {
                    len: 0,
                    is_dir: true,
                    is_file: false,
                });
            }
            Err(std::io::ErrorKind::NotFound.into())
        }
        fn read_dir(&self, p: &Path) -> std::io::Result<Vec<(PathBuf, bool)>> {
            let mut out: Vec<(PathBuf, bool)> = Vec::new();
            for k in self.0.keys() {
                if let Ok(rest) = k.strip_prefix(p) {
                    let mut c = rest.components();
                    let first = p.join(c.next().unwrap());
                    let is_dir = c.next().is_some();
                    if !out.iter().any(|(q, _)| *q == first) {
                        out.push((first, is_dir));
                    }
                }
            }
            Ok(out)
        }
    }

    let root = PathBuf::from("/nas/games/Game");
    let big: Vec<u8> = (0..(3 * 1024 * 1024 + 123))
        .map(|i| (i % 241) as u8)
        .collect();
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("eboot.bin", big),
        (
            "sce_sys/param.json",
            b"{\"titleId\":\"PPSA01234\"}".to_vec(),
        ),
        ("sce_sys/icon0.png", vec![7u8; 5000]),
    ];
    let mem = Mem(files
        .iter()
        .map(|(p, b)| (root.join(p), b.clone()))
        .collect());
    let c = console().await;
    let mut config = cfg();
    config.source_fs = Some(Arc::new(mem));
    put_dir(&c, 12, "data/dest", &root, config).await.unwrap();
    let got = landed(&c.share.join("data/dest"));
    assert_eq!(got.len(), files.len());
    for (rel, bytes) in &files {
        assert!(got.get(*rel).map(|g| g == bytes).unwrap_or(false), "{rel}");
    }
}

// ─── File list ─────────────────────────────────────────────────────────────────

/// Ports the transfer_file_list scenarios (the engine's `/api/transfer/file-list`): an explicit
/// list lands under its root, each at its destination.
#[tokio::test(flavor = "multi_thread")]
async fn upload_file_list_lands_each_file_at_its_destination() {
    let c = console().await;
    let t = tempdir();
    write(&t.path().join("a.bin"), &[0xAAu8; 1024]);
    write(&t.path().join("b.bin"), &[0xBBu8; 2048]);
    let entries = vec![
        FileListEntry {
            src: t.path().join("a.bin").to_string_lossy().into_owned(),
            dest: "a.bin".into(),
        },
        FileListEntry {
            src: t.path().join("b.bin").to_string_lossy().into_owned(),
            dest: "sub/b.bin".into(),
        },
    ];
    let pool = c.pool.clone();
    let r = run(60, move || {
        upload::upload_list_in(&pool, &cfg(), job_id(13), "data/resume", &entries)
    })
    .await
    .unwrap();
    assert_eq!(r.bytes_sent, 1024 + 2048);
    let got = landed(&c.share.join("data/resume"));
    assert_eq!(got["a.bin"], vec![0xAAu8; 1024]);
    assert_eq!(got["sub/b.bin"], vec![0xBBu8; 2048]);
    assert_eq!(got.len(), 2);
}

// ─── File lists with destinations outside the upload root ─────────────────────

fn entry(src: &Path, dest: &str) -> FileListEntry {
    FileListEntry {
        src: src.to_string_lossy().into_owned(),
        dest: dest.into(),
    }
}

/// A file list whose destinations sit under the upload root AND elsewhere (absolute paths) used
/// to work over the retired protocol. One AVA1 job is one manifest under one root, so the list
/// becomes one job per destination directory, run in sequence: everything lands, the jobs are
/// grouped by directory, and the progress counters aggregate across them.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_list_with_destinations_outside_the_root_becomes_one_job_per_directory() {
    let a = abs_console(false).await;
    let t = tempdir();
    let (f1, f2, f3, f4) = (
        t.path().join("1"),
        t.path().join("2"),
        t.path().join("3"),
        t.path().join("4"),
    );
    write(&f1, &pattern(1, 3000));
    write(&f2, &pattern(2, 5000));
    write(&f3, &pattern(3, 7000));
    write(&f4, &pattern(4, 11_000));
    let entries = vec![
        entry(&f1, "in/a.bin"),
        entry(&f2, "/data/other/b.bin"),
        entry(&f3, "/data/other/c.bin"),
        entry(&f4, "/data/third/deep/d.bin"),
    ];
    let (bytes, files) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
    let mut config = cfg();
    config.progress_bytes = Some(bytes.clone());
    config.progress_files = Some(files.clone());
    let pool = a.console.pool.clone();
    let r = run(60, move || {
        upload::upload_list_in(&pool, &config, job_id(50), "/data/games", &entries)
    })
    .await
    .expect("the list uploads");
    let share = &a.console.share;
    assert_eq!(
        std::fs::read(share.join("data/games/in/a.bin")).unwrap(),
        pattern(1, 3000)
    );
    assert_eq!(
        std::fs::read(share.join("data/other/b.bin")).unwrap(),
        pattern(2, 5000)
    );
    assert_eq!(
        std::fs::read(share.join("data/other/c.bin")).unwrap(),
        pattern(3, 7000)
    );
    assert_eq!(
        std::fs::read(share.join("data/third/deep/d.bin")).unwrap(),
        pattern(4, 11_000)
    );
    let mut roots = a.roots.lock().unwrap().clone();
    roots.sort();
    roots.dedup();
    assert_eq!(
        roots,
        vec!["/data/games", "/data/other", "/data/third/deep"]
    );
    assert_eq!(
        r.bytes_sent,
        3000 + 5000 + 7000 + 11_000,
        "bytes aggregate across the jobs"
    );
    assert_eq!(
        bytes.load(Ordering::Relaxed),
        26_000,
        "the progress counter aggregates too"
    );
    assert_eq!(files.load(Ordering::Relaxed), 4);
    let ack: serde_json::Value = serde_json::from_str(&r.commit_ack_body).unwrap();
    assert_eq!(ack["files"], 4);
    assert_eq!(ack["jobs"], 3);
}

/// A list that is entirely inside the root is still one job (nothing changes for the common case).
#[tokio::test(flavor = "multi_thread")]
async fn a_file_list_inside_the_root_stays_one_job() {
    let a = abs_console(false).await;
    let t = tempdir();
    write(&t.path().join("1"), b"one");
    write(&t.path().join("2"), b"two");
    let entries = vec![
        entry(&t.path().join("1"), "x/1"),
        entry(&t.path().join("2"), "/data/games/x/2"),
    ];
    let pool = a.console.pool.clone();
    run(60, move || {
        upload::upload_list_in(&pool, &cfg(), job_id(51), "/data/games", &entries)
    })
    .await
    .unwrap();
    assert_eq!(a.roots.lock().unwrap().len(), 1);
    assert_eq!(landed(&a.console.share.join("data/games")).len(), 2);
}

/// Cancel stops every job of the list: a cancel during the first job means the later
/// directories are never opened.
#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_split_file_list_stops_all_of_it() {
    let a = abs_console(true).await;
    let t = tempdir();
    write(&t.path().join("big"), &pattern(9, 16 * 1024 * 1024));
    write(&t.path().join("small"), b"later");
    let entries = vec![
        entry(&t.path().join("big"), "big.bin"),
        entry(&t.path().join("small"), "/data/later/small.bin"),
    ];
    let pool = a.console.pool.clone();
    let proxy = a.proxy.clone().unwrap();
    let e = cancel_midway(&proxy, &cfg(), move |config| {
        upload::upload_list_in(&pool, &config, job_id(52), "/data/games", &entries)
    })
    .await;
    assert!(format!("{e:#}").contains("cancel"), "{e:#}");
    assert!(
        !a.roots.lock().unwrap().iter().any(|r| r == "/data/later"),
        "the second directory's job must never start"
    );
    assert!(!a.console.share.join("data/later").exists());
}

/// A failure reports the first failing path, with the console's own reason kept.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_directory_reports_its_first_path() {
    let a = abs_console(false).await;
    let t = tempdir();
    write(&t.path().join("ok"), b"fine");
    write(&t.path().join("bad1"), b"x");
    write(&t.path().join("bad2"), b"y");
    let entries = vec![
        entry(&t.path().join("ok"), "ok.bin"),
        entry(&t.path().join("bad1"), "/forbidden/a/one.bin"),
        entry(&t.path().join("bad2"), "/forbidden/a/two.bin"),
    ];
    let pool = a.console.pool.clone();
    let e = run(60, move || {
        upload::upload_list_in(&pool, &cfg(), job_id(53), "/data/games", &entries)
    })
    .await
    .unwrap_err();
    let msg = format!("{e:#}");
    assert!(
        msg.contains("/forbidden/a/one.bin"),
        "names the first failing path: {msg}"
    );
    assert!(!msg.contains("two.bin"), "only the first: {msg}");
    assert!(
        a.console.share.join("data/games/ok.bin").exists(),
        "the jobs before the failure stay done"
    );
}

// ─── Cancel and resume ─────────────────────────────────────────────────────────

/// Ports `abort_tx_marks_aborted` and `abort_transaction_helper_marks_aborted`: cancelling stops
/// the upload with `transfer_cancelled` and nothing half-written is left under the final name.
#[tokio::test(flavor = "multi_thread")]
async fn cancelling_an_upload_stops_it_and_leaves_no_partial_file() {
    let (c, proxy) = slow_console().await;
    let t = tempdir();
    write(&t.path().join("big.bin"), &pattern(5, 16 * 1024 * 1024));
    let (pool, src) = (c.pool.clone(), t.path().join("big.bin"));
    let e = cancel_midway(&proxy, &cfg(), move |config| {
        upload::upload_file_in(&pool, &config, job_id(20), "data/big.bin", &src)
    })
    .await;
    assert!(
        format!("{e:#}").contains("cancel"),
        "expected a cancel, got {e:#}"
    );
    assert!(
        !c.share.join("data/big.bin").exists(),
        "a cancelled single-file upload must not leave the file under its final name"
    );
}

/// Ports `transfer_file_resumable_with_initial_resume_flag_skips_acked_shards` and
/// `interrupted_tx_is_still_abortable`: after a cancelled (interrupted) attempt the same job id
/// resumes, the console keeps what was durable, and the result is byte-exact.
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_upload_resumes_under_the_same_job_id() {
    let (c, proxy) = slow_console().await;
    let t = tempdir();
    let data = pattern(6, 16 * 1024 * 1024);
    write(&t.path().join("big.bin"), &data);
    let (pool, src) = (c.pool.clone(), t.path().join("big.bin"));
    let e = cancel_midway(&proxy, &cfg(), move |config| {
        upload::upload_file_in(&pool, &config, job_id(21), "data/big.bin", &src)
    })
    .await;
    assert!(format!("{e:#}").contains("cancel"), "{e:#}");
    let r = put_file(&c, 21, "data/big.bin", &t.path().join("big.bin"))
        .await
        .expect("the resumed upload commits");
    // The resumed attempt sends only what the console did not already hold durably.
    assert!(r.bytes_sent <= data.len() as u64);
    assert!(std::fs::read(c.share.join("data/big.bin")).unwrap() == data);
}

/// Ports `transfer_file_resumes_after_mid_stream_drop`: the connection is killed once while the
/// upload runs; the adapter reconnects with the same job id and the file is byte-identical.
#[tokio::test(flavor = "multi_thread")]
async fn upload_file_resumes_after_a_mid_stream_drop() {
    let (c, proxy) = slow_console().await;
    let t = tempdir();
    let data = pattern(8, 16 * 1024 * 1024);
    write(&t.path().join("resume.bin"), &data);
    let finalized = Arc::new(AtomicU64::new(0));
    let mut config = cfg();
    config.progress_bytes_finalized = Some(finalized.clone());
    let killer = {
        let (proxy, finalized) = (proxy.clone(), finalized.clone());
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            while finalized.load(Ordering::Relaxed) == 0 {
                assert!(std::time::Instant::now() < deadline, "no durable progress");
                std::thread::sleep(Duration::from_millis(2));
            }
            proxy.kill_all();
            // The drop happened mid-transfer, which is all the cap was for: the resume runs
            // at full speed.
            proxy.set_bytes_per_sec(None);
        })
    };
    let (pool, src) = (c.pool.clone(), t.path().join("resume.bin"));
    let r = run(300, move || {
        upload::upload_file_in(&pool, &config, job_id(22), "data/resume.bin", &src)
    })
    .await
    .expect("the upload survives a dropped connection");
    killer.join().unwrap();
    // `bytes_sent` counts what crossed the wire, including what the drop made the sender repeat.
    assert!(r.bytes_sent >= data.len() as u64);
    assert!(std::fs::read(c.share.join("data/resume.bin")).unwrap() == data);
    assert!(proxy.kills() >= 1, "the connection was dropped once");
}

/// Ports `transfer_file_list_initial_flags_resume_adopts_existing`: a file-list upload cancelled
/// midway resumes under the same job id and lands every file.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_list_upload_resumes_after_an_interruption() {
    let (c, proxy) = slow_console().await;
    let t = tempdir();
    let a = pattern(1, 12 * 1024 * 1024);
    let b = pattern(2, 12 * 1024 * 1024);
    write(&t.path().join("a.bin"), &a);
    write(&t.path().join("b.bin"), &b);
    let entries = vec![
        FileListEntry {
            src: t.path().join("a.bin").to_string_lossy().into_owned(),
            dest: "a.bin".into(),
        },
        FileListEntry {
            src: t.path().join("b.bin").to_string_lossy().into_owned(),
            dest: "b.bin".into(),
        },
    ];
    let (pool, e2) = (c.pool.clone(), entries.clone());
    let e = cancel_midway(&proxy, &cfg(), move |config| {
        upload::upload_list_in(&pool, &config, job_id(23), "data/resume", &e2)
    })
    .await;
    assert!(format!("{e:#}").contains("cancel"), "{e:#}");
    let pool = c.pool.clone();
    run(300, move || {
        upload::upload_list_in(&pool, &cfg(), job_id(23), "data/resume", &entries)
    })
    .await
    .expect("the resumed list commits");
    let got = landed(&c.share.join("data/resume"));
    assert!(got["a.bin"] == a && got["b.bin"] == b);
    assert_eq!(got.len(), 2);
}

/// Ports `transfer_file_refuses_ghost_commit_on_bogus_last_acked` and the folder variant: the
/// old guard stopped an engine from committing against a stale cursor. The AVA1 counterpart is a
/// job id reused after the source changed: the console must never splice the old bytes with the
/// new, so the upload either restarts and lands the NEW content exactly, or refuses.
#[tokio::test(flavor = "multi_thread")]
async fn a_reused_job_id_with_changed_bytes_never_splices() {
    let (c, proxy) = slow_console().await;
    let t = tempdir();
    let old = pattern(30, 16 * 1024 * 1024);
    let new = pattern(31, 16 * 1024 * 1024);
    let src = t.path().join("g.bin");
    write(&src, &old);
    let (pool, s2) = (c.pool.clone(), src.clone());
    let e = cancel_midway(&proxy, &cfg(), move |config| {
        upload::upload_file_in(&pool, &config, job_id(24), "data/g.bin", &s2)
    })
    .await;
    assert!(format!("{e:#}").contains("cancel"), "{e:#}");
    write(&src, &new);
    match put_file(&c, 24, "data/g.bin", &src).await {
        Ok(_) => assert!(
            std::fs::read(c.share.join("data/g.bin")).unwrap() == new,
            "a committed file must be the NEW content, never a splice"
        ),
        Err(_) => assert!(
            !c.share.join("data/g.bin").exists(),
            "a refused upload must not leave a committed file"
        ),
    }
}

// ─── Console session ───────────────────────────────────────────────────────────

/// Ports `hello_round_trip`: a session to the console opens and answers its identity
/// (`node.info`) over the control stream.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_opens_and_the_console_answers_node_info() {
    use ava1::wire::Message;
    let c = console().await;
    let s = c.pool.session(&c.addr).await.expect("a paired session");
    let reply = s.rpc(ava1::gen::METHOD_NODE_INFO, &[]).await.unwrap();
    assert_eq!(reply.status, ava1::gen::STATUS_OK);
    let info = ava1::gen::NodeInfo::decode(&reply.body).unwrap();
    assert!(!info.version.is_empty());
}

// ─── CI throughput gates ───────────────────────────────────────────────────────
//
// Loopback, no PS5: they catch algorithmic regressions (extra copies, serialisation bugs)
// before they reach hardware. The debug floor is generous (overhead dominated, varies with
// concurrent CPU load); the release floor is real.

#[cfg(debug_assertions)]
const FLOOR_MIB_PER_SEC: f64 = 10.0;
#[cfg(not(debug_assertions))]
const FLOOR_MIB_PER_SEC: f64 = 200.0;

fn assert_throughput(label: &str, bytes: usize, elapsed: Duration) {
    let rate = (bytes as f64) / (1024.0 * 1024.0) / elapsed.as_secs_f64();
    println!(
        "[gate] {label}: {rate:.1} MiB/s ({} MiB in {:.3}s)",
        bytes / (1024 * 1024),
        elapsed.as_secs_f64()
    );
    assert!(
        rate >= FLOOR_MIB_PER_SEC,
        "{label}: {rate:.1} MiB/s < floor {FLOOR_MIB_PER_SEC} MiB/s"
    );
}

/// Ports `ci_throughput_gate_single_file_32mib`.
#[tokio::test(flavor = "multi_thread")]
async fn ci_throughput_gate_single_file_32mib() {
    const SIZE: usize = 32 * 1024 * 1024;
    let _turn = heavy().await;
    let c = console().await;
    let t = tempdir();
    write(&t.path().join("gate.bin"), &vec![0x5Au8; SIZE]);
    let t0 = std::time::Instant::now();
    let r = put_file(&c, 40, "data/gate_32m.bin", &t.path().join("gate.bin"))
        .await
        .unwrap();
    let elapsed = t0.elapsed();
    assert_eq!(r.bytes_sent, SIZE as u64);
    assert_throughput("single-file 32 MiB", SIZE, elapsed);
}

/// Ports `ci_throughput_gate_dir_16x2mib`.
#[tokio::test(flavor = "multi_thread")]
async fn ci_throughput_gate_dir_16x2mib() {
    const FILE: usize = 2 * 1024 * 1024;
    const COUNT: usize = 16;
    let _turn = heavy().await;
    let c = console().await;
    let t = tempdir();
    for i in 0..COUNT {
        write(&t.path().join(format!("f{i:03}.bin")), &vec![0xA5u8; FILE]);
    }
    let t0 = std::time::Instant::now();
    let r = put_dir(&c, 41, "data/gate_dir", t.path(), cfg())
        .await
        .unwrap();
    let elapsed = t0.elapsed();
    assert_eq!(r.bytes_sent, (FILE * COUNT) as u64);
    assert_throughput("dir 16x2 MiB", FILE * COUNT, elapsed);
}
