//! Integration tests for the zip-archive transfer path (Strategy C).
//!
//! Each test builds a `.zip` fixture, transfers it through the mock FTX2
//! server via `ps5upload_core::transfer::transfer_zip`, and asserts the files
//! land already extracted with byte-identical content — i.e. a zip upload is
//! equivalent to uploading the extracted folder. The on-the-wire frames are
//! identical to a directory transfer, so the mock server needs no zip
//! awareness; these tests exercise the host-side inflate-to-cache + shard
//! planning, not a new protocol.

mod mock_server;
use mock_server::MockServer;

use ps5upload_core::transfer::{
    inspect_zip, transfer_zip, transfer_zip_with_opts, zip_plan_preview, TransferConfig,
};
use std::io::Write;
use std::path::PathBuf;

fn random_tx_id() -> [u8; 16] {
    let mut id = [0u8; 16];
    for (i, b) in id.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(17).wrapping_add(0xAB);
    }
    id
}

/// A temp `.zip` that deletes itself on drop.
struct TmpZip(PathBuf);
impl Drop for TmpZip {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Build a zip at a unique temp path from `(name, bytes)` entries. `deflate`
/// toggles DEFLATE vs Stored so we cover both — the materialiser inflates
/// either the same way.
fn build_zip(entries: &[(&str, &[u8])], deflate: bool) -> TmpZip {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("ps5upload_ziptest_{ts}_{seq}.zip"));

    let f = std::fs::File::create(&path).unwrap();
    let mut zw = zip::ZipWriter::new(f);
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

/// Deterministic pseudo-random payload so compression actually does work and
/// multi-shard boundaries are content-sensitive.
fn pattern(len: usize, salt: u8) -> Vec<u8> {
    (0..len)
        .map(|i| {
            ((i as u32)
                .wrapping_mul(2654435761)
                .wrapping_add(salt as u32)
                & 0xff) as u8
        })
        .collect()
}

// ─── Happy path ────────────────────────────────────────────────────────────

#[test]
fn transfer_zip_basic_lands_extracted() {
    let zip = build_zip(
        &[
            ("a.txt", b"file-a"),
            ("b.txt", b"file-b contents here"),
            ("sub/c.txt", b"nested-c"),
        ],
        true,
    );

    let srv = MockServer::start();
    let cfg = TransferConfig::new(&srv.addr);
    let result = transfer_zip(&cfg, random_tx_id(), "/data/dest", &zip.0).unwrap();

    let st = srv.state.lock().unwrap();
    assert_eq!(st.txs.get(&result.tx_id_hex).unwrap().state, "committed");
    assert_eq!(
        st.applied.get("/data/dest/a.txt").map(|v| v.as_slice()),
        Some(b"file-a".as_ref())
    );
    assert_eq!(
        st.applied.get("/data/dest/b.txt").map(|v| v.as_slice()),
        Some(b"file-b contents here".as_ref())
    );
    assert_eq!(
        st.applied.get("/data/dest/sub/c.txt").map(|v| v.as_slice()),
        Some(b"nested-c".as_ref())
    );
}

// ─── Large entry split across many shards (RAM cache) ────────────────────────

#[test]
fn transfer_zip_large_entry_multi_shard_ram_cache() {
    let data = pattern(100_000, 7);
    let zip = build_zip(&[("big.bin", &data)], true);

    let srv = MockServer::start();
    // Tiny shard, no packing → one zip entry yields many NonPacked shards, all
    // served from the single in-RAM inflated copy.
    let cfg = TransferConfig {
        shard_size: 4096,
        pack_size: 0,
        ..TransferConfig::new(&srv.addr)
    };
    let result = transfer_zip(&cfg, random_tx_id(), "/data/g", &zip.0).unwrap();

    assert_eq!(result.shards_sent, data.len().div_ceil(4096) as u64);
    // The mock maps each manifest file's non-packed shards (in seq order) to
    // its real dest path — so the single entry "big.bin" lands at
    // dest_root/big.bin. A byte-exact match here proves the per-entry cache
    // served every shard's range correctly and in order across the inflated
    // copy.
    let st = srv.state.lock().unwrap();
    assert_eq!(
        st.applied.get("/data/g/big.bin").map(|v| v.as_slice()),
        Some(data.as_slice())
    );
}

// ─── Large entry forced to spill to a temp file (Tmp cache) ──────────────────

#[test]
fn transfer_zip_large_entry_temp_spill() {
    let data = pattern(80_000, 19);
    let zip = build_zip(&[("huge.bin", &data)], true);

    let srv = MockServer::start();
    let cfg = TransferConfig {
        shard_size: 8192,
        pack_size: 0,
        ..TransferConfig::new(&srv.addr)
    };
    // ram_threshold = 64 bytes → the 80 KB entry exceeds it and inflates to a
    // temp file; ranges are served via seek+read from that file.
    let result =
        transfer_zip_with_opts(&cfg, random_tx_id(), "/data/spill", &zip.0, 64, 0).unwrap();

    assert_eq!(result.bytes_sent, data.len() as u64);
    // Single non-packed entry "huge.bin" → mapped to dest_root/huge.bin by the
    // mock. A byte-exact match proves the temp-file cache served every range
    // right.
    let st = srv.state.lock().unwrap();
    assert_eq!(
        st.applied.get("/data/spill/huge.bin").map(|v| v.as_slice()),
        Some(data.as_slice())
    );
}

// ─── Packed small files mixed with a big one ─────────────────────────────────

#[test]
fn transfer_zip_packs_small_and_splits_big() {
    let big = pattern(50_000, 3);
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..12u32 {
        entries.push((format!("small/f{i:02}.bin"), pattern(300, i as u8)));
    }
    entries.push(("big.bin".to_string(), big.clone()));
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    let zip = build_zip(&refs, true);

    let srv = MockServer::start();
    let cfg = TransferConfig {
        shard_size: 8192,
        pack_size: 8 * 1024,
        ..TransferConfig::new(&srv.addr)
    };
    let result = transfer_zip(&cfg, random_tx_id(), "/data/mix", &zip.0).unwrap();
    // Plan total = every entry's uncompressed size.
    assert_eq!(result.bytes_sent, (12 * 300) + big.len() as u64);

    let st = srv.state.lock().unwrap();
    assert_eq!(st.txs.get(&result.tx_id_hex).unwrap().state, "committed");
    for i in 0..12u32 {
        let key = format!("/data/mix/small/f{i:02}.bin");
        assert_eq!(
            st.applied.get(&key).map(|v| v.as_slice()),
            Some(pattern(300, i as u8).as_slice()),
            "small file {i} mismatch"
        );
    }
    assert_eq!(
        st.applied.get("/data/mix/big.bin").map(|v| v.as_slice()),
        Some(big.as_slice())
    );
}

// ─── Excludes ────────────────────────────────────────────────────────────────

#[test]
fn transfer_zip_honors_excludes() {
    let zip = build_zip(
        &[
            ("keep.txt", b"keep me"),
            (".DS_Store", b"junk"),
            ("nested/.DS_Store", b"junk2"),
        ],
        true,
    );

    let srv = MockServer::start();
    let mut cfg = TransferConfig::new(&srv.addr);
    cfg.excludes = vec![".DS_Store".to_string()];
    transfer_zip(&cfg, random_tx_id(), "/data/x", &zip.0).unwrap();

    let st = srv.state.lock().unwrap();
    assert!(st.applied.contains_key("/data/x/keep.txt"));
    assert!(!st.applied.contains_key("/data/x/.DS_Store"));
    assert!(!st.applied.contains_key("/data/x/nested/.DS_Store"));
}

// ─── Duplicate / colliding entry paths ───────────────────────────────────────

#[test]
fn transfer_zip_collapses_colliding_paths_last_wins() {
    // Two distinct central-directory names that sanitize to the SAME dest
    // path (`x/./y.txt` collapses to `x/y.txt`). The planner must send only
    // one copy — the last (the byte stream that would win on disk) — not
    // both, so file_count/bytes match reality.
    let zip = build_zip(&[("x/y.txt", b"AAAA"), ("x/./y.txt", b"BBBBBBBB")], true);
    let srv = MockServer::start();
    let cfg = TransferConfig::new(&srv.addr);
    let result = transfer_zip(&cfg, random_tx_id(), "/data/dup", &zip.0).unwrap();

    // Only the surviving copy's bytes are sent (8, not 4+8=12).
    assert_eq!(result.bytes_sent, 8);
    let st = srv.state.lock().unwrap();
    assert_eq!(
        st.applied.get("/data/dup/x/y.txt").map(|v| v.as_slice()),
        Some(b"BBBBBBBB".as_ref())
    );
}

// ─── Security: zip-slip ──────────────────────────────────────────────────────

#[test]
fn transfer_zip_rejects_path_traversal() {
    let zip = build_zip(&[("../evil.txt", b"pwned"), ("ok.txt", b"fine")], false);
    let srv = MockServer::start();
    let cfg = TransferConfig::new(&srv.addr);
    let err = transfer_zip(&cfg, random_tx_id(), "/data/safe", &zip.0).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("unsafe") || msg.contains("invalid"),
        "expected a zip-slip rejection, got: {msg}"
    );
}

// ─── Inspect + preview (no PS5 needed) ───────────────────────────────────────

#[test]
fn inspect_zip_reports_sizes_and_game_meta() {
    let param = br#"{"titleId":"PPSA01234","contentId":"EP0000-PPSA01234_00-TESTGAME00000000","applicationCategoryType":0,"localizedParameters":{"defaultLanguage":"en-US","en-US":{"titleName":"Test Game"}}}"#;
    let eboot = pattern(10_000, 1);
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
        "ps5upload_notazip_{}_{}.zip",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&p, b"this is definitely not a zip file").unwrap();
    let r = inspect_zip(&p);
    let _ = std::fs::remove_file(&p);
    assert!(r.is_err(), "garbage input must fail to inspect");
}
