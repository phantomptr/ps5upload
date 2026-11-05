//! AVA1 integration tests for the zip-archive upload path. They replace
//! `transfer_zip_integration.rs`: each builds a `.zip` fixture, uploads it through the Rust AVA1
//! job host and asserts the files land already extracted with byte-identical content (a zip
//! upload is equivalent to uploading the extracted folder). The inspect and plan-preview tests
//! never touched the console; they are carried over unchanged.

mod ava1_common;
use ava1_common::*;

use std::io::Write;
use std::path::PathBuf;

use ps5upload_ava1::upload;
use ps5upload_core::transfer::{inspect_zip, zip_plan_preview};

/// A temp `.zip` that deletes itself on drop.
struct TmpZip(PathBuf);
impl Drop for TmpZip {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Builds a zip from `(name, bytes)` entries; `deflate` toggles DEFLATE vs Stored.
fn build_zip(entries: &[(&str, &[u8])], deflate: bool) -> TmpZip {
    use std::sync::atomic::{AtomicU64, Ordering};
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "ps5u_ava1_ziptest_{}_{}.zip",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let mut zw = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let method = if deflate {
        CompressionMethod::Deflated
    } else {
        CompressionMethod::Stored
    };
    let opts = SimpleFileOptions::default().compression_method(method);
    for (name, bytes) in entries {
        zw.start_file(*name, opts).unwrap();
        zw.write_all(bytes).unwrap();
    }
    zw.finish().unwrap();
    TmpZip(path)
}

/// Deterministic pseudo-random payload so compression does real work and chunk boundaries are
/// content-sensitive.
fn noise(len: usize, salt: u8) -> Vec<u8> {
    (0..len)
        .map(|i| {
            ((i as u32)
                .wrapping_mul(2654435761)
                .wrapping_add(salt as u32)
                & 0xff) as u8
        })
        .collect()
}

async fn put_zip(
    c: &Console,
    id: u8,
    dest_root: &str,
    zip: &TmpZip,
    config: ps5upload_core::transfer::TransferConfig,
) -> anyhow::Result<ps5upload_core::transfer::TransferResult> {
    let (pool, dest, path) = (c.pool.clone(), dest_root.to_string(), zip.0.clone());
    run(300, move || {
        upload::upload_zip_in(&pool, &config, job_id(id), &dest, &path)
    })
    .await
}

/// Ports `transfer_zip_basic_lands_extracted`.
#[tokio::test(flavor = "multi_thread")]
async fn upload_zip_basic_lands_extracted() {
    let zip = build_zip(
        &[
            ("a.txt", b"file-a"),
            ("b.txt", b"file-b contents here"),
            ("sub/c.txt", b"nested-c"),
        ],
        true,
    );
    let c = console().await;
    put_zip(&c, 1, "data/dest", &zip, cfg()).await.unwrap();
    let got = landed(&c.share.join("data/dest"));
    assert_eq!(got.len(), 3);
    assert_eq!(got["a.txt"], b"file-a");
    assert_eq!(got["b.txt"], b"file-b contents here");
    assert_eq!(got["sub/c.txt"], b"nested-c");
}

/// Ports `transfer_zip_large_entry_multi_shard_stream`: one entry far larger than a chunk is
/// inflated as it streams and lands byte for byte.
#[tokio::test(flavor = "multi_thread")]
async fn upload_zip_large_entry_streams_byte_exact() {
    let data = noise(3 * 1024 * 1024 + 17, 7);
    let zip = build_zip(&[("big.bin", &data)], true);
    let c = console().await;
    let r = put_zip(&c, 2, "data/g", &zip, cfg()).await.unwrap();
    assert_eq!(r.bytes_sent, data.len() as u64);
    assert!(std::fs::read(c.share.join("data/g/big.bin")).unwrap() == data);
}

/// Ports `transfer_zip_large_entry_with_tiny_ram_threshold`. The RAM-vs-temp inflate knob is
/// gone (an entry is inflated once, as a stream, whatever its size), so the scenario is the same
/// large entry, which lands byte for byte through the streaming reader alone.
#[tokio::test(flavor = "multi_thread")]
async fn upload_zip_large_entry_needs_no_staging() {
    let data = noise(2 * 1024 * 1024, 19);
    let zip = build_zip(&[("huge.bin", &data)], true);
    let c = console().await;
    put_zip(&c, 3, "data/spill", &zip, cfg()).await.unwrap();
    assert!(std::fs::read(c.share.join("data/spill/huge.bin")).unwrap() == data);
}

/// Ports `transfer_zip_resumes_after_mid_stream_drop`: an upload interrupted mid-entry resumes
/// under the same job id and the file is byte-identical.
#[tokio::test(flavor = "multi_thread")]
async fn upload_zip_resumes_after_an_interruption() {
    let (c, proxy) = slow_console().await;
    let data = pattern(42, 16 * 1024 * 1024);
    let zip = build_zip(&[("game.bin", &data)], false);
    let (pool, path) = (c.pool.clone(), zip.0.clone());
    let e = cancel_midway(&proxy, &cfg(), move |config| {
        upload::upload_zip_in(&pool, &config, job_id(4), "data/r", &path)
    })
    .await;
    assert!(format!("{e:#}").contains("cancel"), "{e:#}");
    put_zip(&c, 4, "data/r", &zip, cfg())
        .await
        .expect("the resumed upload commits");
    assert!(std::fs::read(c.share.join("data/r/game.bin")).unwrap() == data);
}

/// Ports `transfer_zip_packs_small_and_splits_big`: many small entries and one big one in the
/// same archive all land.
#[tokio::test(flavor = "multi_thread")]
async fn upload_zip_small_files_and_a_big_one() {
    let big = noise(1024 * 1024, 3);
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..12u32 {
        entries.push((format!("small/f{i:02}.bin"), noise(300, i as u8)));
    }
    entries.push(("big.bin".to_string(), big.clone()));
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    let zip = build_zip(&refs, true);
    let c = console().await;
    let r = put_zip(&c, 5, "data/mix", &zip, cfg()).await.unwrap();
    assert_eq!(r.bytes_sent, (12 * 300) + big.len() as u64);
    let got = landed(&c.share.join("data/mix"));
    assert_eq!(got.len(), 13);
    for i in 0..12u32 {
        assert_eq!(got[&format!("small/f{i:02}.bin")], noise(300, i as u8));
    }
    assert!(got["big.bin"] == big);
}

/// Ports `transfer_zip_honors_excludes`.
#[tokio::test(flavor = "multi_thread")]
async fn upload_zip_honors_excludes() {
    let zip = build_zip(
        &[
            ("keep.txt", b"keep me"),
            (".DS_Store", b"junk"),
            ("nested/.DS_Store", b"junk2"),
        ],
        true,
    );
    let c = console().await;
    let mut config = cfg();
    config.excludes = vec![".DS_Store".to_string()];
    put_zip(&c, 6, "data/x", &zip, config).await.unwrap();
    let got = landed(&c.share.join("data/x"));
    assert_eq!(got.keys().collect::<Vec<_>>(), vec!["keep.txt"]);
}

/// Ports `transfer_zip_collapses_colliding_paths_last_wins`. Two central-directory names that
/// sanitize to the SAME destination (`x/./y.txt` is `x/y.txt`) cannot both land. The old path
/// collapsed them last-wins; AVA1's manifest refuses a duplicate path outright (one job is one
/// manifest of unique paths), so the archive is refused as unusable and nothing is sent.
#[tokio::test(flavor = "multi_thread")]
async fn upload_zip_with_colliding_paths_is_refused_whole() {
    let zip = build_zip(&[("x/y.txt", b"AAAA"), ("x/./y.txt", b"BBBBBBBB")], true);
    let c = console().await;
    let e = put_zip(&c, 7, "data/dup", &zip, cfg()).await.unwrap_err();
    assert!(
        e.downcast_ref::<upload::ZipUnsupported>().is_some(),
        "expected the archive refused as unusable, got {e:#}"
    );
    assert!(
        landed(&c.share.join("data/dup")).is_empty(),
        "nothing from a refused archive lands"
    );
}

/// The plan and the source must agree: an archive the upload refuses is refused at planning, not
/// previewed as fine and failed after Start.
#[test]
fn zip_plan_preview_refuses_colliding_paths_as_the_upload_does() {
    let zip = build_zip(&[("x/y.txt", b"AAAA"), ("x/./y.txt", b"BBBBBBBB")], true);
    let e = zip_plan_preview(&zip.0, &[]).unwrap_err();
    assert!(
        format!("{e:#}").contains("two zip entries name the same file"),
        "{e:#}"
    );
    // Both excluded: neither is sent, so nothing collides (the source skips them first too).
    let (total, files) = zip_plan_preview(&zip.0, &["y.txt".to_string()]).unwrap();
    assert_eq!((total, files.len()), (0, 0));
}

/// Ports `transfer_zip_rejects_path_traversal` (zip-slip).
#[tokio::test(flavor = "multi_thread")]
async fn upload_zip_rejects_path_traversal() {
    let zip = build_zip(&[("../evil.txt", b"pwned"), ("ok.txt", b"fine")], false);
    let c = console().await;
    let e = put_zip(&c, 8, "data/safe", &zip, cfg()).await.unwrap_err();
    assert!(
        e.downcast_ref::<upload::ZipUnsupported>().is_some()
            || format!("{e:#}").contains("unsafe")
            || format!("{e:#}").contains("invalid"),
        "expected a zip-slip rejection, got: {e:#}"
    );
    assert!(!c.share.join("data/evil.txt").exists());
    assert!(!c.share.join("evil.txt").exists());
    assert!(
        landed(&c.share).is_empty(),
        "nothing lands from a hostile archive"
    );
}

// ─── Inspect + preview (no console needed; protocol-neutral, carried over) ────

#[test]
fn inspect_zip_reports_sizes_and_game_meta() {
    let param = br#"{"titleId":"PPSA01234","contentId":"EP0000-PPSA01234_00-TESTGAME00000000","applicationCategoryType":0,"localizedParameters":{"defaultLanguage":"en-US","en-US":{"titleName":"Test Game"}}}"#;
    let eboot = noise(10_000, 1);
    let zip = build_zip(
        &[
            ("MyGame/eboot.bin", &eboot),
            ("MyGame/sce_sys/param.json", param),
        ],
        true,
    );
    let inspect = inspect_zip(&zip.0).unwrap();
    assert_eq!(inspect.file_count, 2);
    assert_eq!(
        inspect.total_uncompressed,
        eboot.len() as u64 + param.len() as u64
    );
    assert!(inspect.compressed_size > 0);
    assert_eq!(inspect.title.as_deref(), Some("Test Game"));
    assert_eq!(inspect.title_id.as_deref(), Some("PPSA01234"));
    assert_eq!(inspect.application_category_type, Some(0));
    assert_eq!(inspect.game_root.as_deref(), Some("MyGame"));
    assert_eq!(
        inspect.entry_names.as_deref(),
        Some(
            &[
                "MyGame/eboot.bin".to_string(),
                "MyGame/sce_sys/param.json".to_string()
            ][..]
        )
    );
    assert!(inspect.entry_names_complete);
}

/// #645: the inspect lists a non-game archive's files so the client can
/// offer to send it as it is (a jailbreak update, payloads).
#[test]
fn inspect_zip_lists_entry_names() {
    let zip = build_zip(
        &[("y2jb/payload.js", b"js"), ("y2jb/main.elf", b"elf")],
        true,
    );
    let inspect = inspect_zip(&zip.0).unwrap();
    assert_eq!(inspect.game_root, None);
    assert_eq!(
        inspect.entry_names,
        Some(vec![
            "y2jb/payload.js".to_string(),
            "y2jb/main.elf".to_string()
        ])
    );
    assert!(inspect.entry_names_complete);
}

#[test]
fn zip_plan_preview_sorted_total_and_excludes() {
    let zip = build_zip(
        &[
            ("z.bin", b"zzz"),
            ("a.bin", b"aaaa"),
            (".DS_Store", b"junk"),
        ],
        true,
    );
    let (total, files) = zip_plan_preview(&zip.0, &[".DS_Store".to_string()]).unwrap();
    assert_eq!(total, 3 + 4);
    let names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(names, vec!["a.bin", "z.bin"]); // sorted, junk excluded
}

#[test]
fn inspect_zip_on_garbage_errors() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let p = std::env::temp_dir().join(format!(
        "ps5u_ava1_notazip_{}_{}.zip",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&p, b"this is definitely not a zip file").unwrap();
    let r = inspect_zip(&p);
    let _ = std::fs::remove_file(&p);
    assert!(r.is_err(), "garbage input must fail to inspect");
}
