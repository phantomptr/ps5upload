//! Opt-in checks against a REAL console (all `#[ignore]`d). They replace the live tests of
//! `transfer_integration.rs`, `hw_rar_stream.rs` and `real_archive_stream.rs`. Set
//! `REAL_PS5_ADDR=<host>` (a bare host; there is one port now) and run with `-- --ignored
//! --nocapture`. The first run of an engine on a new console needs it paired: launch the helper
//! from the app (a launched helper pairs with no code) or pair from the Connection screen; this
//! process uses the same identity directory as the app (`PS5UPLOAD_DATA_DIR`, else
//! `~/.ps5upload`), so it must NOT run while the engine is connected to the same console (one
//! session per identity evicts the other).
//!
//! No test here runs in CI: they need a console, a path and, for the archive ones, an archive
//! nobody can ship.
#![cfg(not(target_os = "android"))]

mod ava1_common;
use ava1_common::*;

use std::path::PathBuf;

use ps5upload_ava1::upload;
use ps5upload_core::fs_ops;
use ps5upload_core::transfer::TransferConfig;

fn console_addr() -> Option<String> {
    match std::env::var("REAL_PS5_ADDR") {
        Ok(a) if !a.trim().is_empty() => {
            // The management seam is the engine's own transport over the process pool.
            ps5upload_ava1::mgmt::install();
            Some(a)
        }
        _ => {
            eprintln!("REAL_PS5_ADDR unset: skipping");
            None
        }
    }
}

fn env(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(v) => Some(v),
        Err(_) => {
            eprintln!("{name} unset: skipping");
            None
        }
    }
}

fn tag() -> String {
    job_id(0x5A)
        .iter()
        .take(2)
        .chain(std::process::id().to_le_bytes().iter().take(2))
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../ps5upload-core/testdata/rar")
        .join(name)
}

/// Ports `live_ps5_hash_compare_file`: hash a file on the console (a `job.run` hash) and compare
/// with the local file's BLAKE3. A mismatch means the upload corrupted the file; a match means
/// the bytes are exact and any "game will not play" issue is elsewhere (permissions, a missing
/// file). Needs `PS5_REMOTE_PATH` and `LOCAL_FILE_PATH`.
#[test]
#[ignore = "requires REAL_PS5_ADDR + PS5_REMOTE_PATH + LOCAL_FILE_PATH"]
fn live_ps5_hash_compare_file() {
    let (Some(addr), Some(remote), Some(local)) = (
        console_addr(),
        env("PS5_REMOTE_PATH"),
        env("LOCAL_FILE_PATH"),
    ) else {
        return;
    };
    eprintln!("hashing remote {remote} (PS5 {addr}) and local {local}");
    let local_bytes = std::fs::read(&local).expect("read local file");
    let local_hex: String = blake3::hash(&local_bytes)
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let remote_result = fs_ops::fs_hash(&addr, &remote).expect("hash on the console");
    assert_eq!(
        remote_result.size,
        local_bytes.len() as u64,
        "size mismatch: the remote file is truncated or extended"
    );
    assert_eq!(
        remote_result.hash, local_hex,
        "hash mismatch: the file CONTENT is corrupted (sizes match, bytes differ)"
    );
    eprintln!("{remote} matches local byte-exact");
}

/// Ports `live_ps5_chmod_recursive`: recursively chmod a path (default 0777). A recursive chmod
/// of a 46k-file folder runs as a console job (progress, cancel), so no 10-minute socket deadline
/// is needed. Needs `PS5_REMOTE_PATH`; `PS5_CHMOD_MODE` defaults to 0777.
#[test]
#[ignore = "requires REAL_PS5_ADDR + PS5_REMOTE_PATH"]
fn live_ps5_chmod_recursive() {
    let (Some(addr), Some(remote)) = (console_addr(), env("PS5_REMOTE_PATH")) else {
        return;
    };
    let mode = std::env::var("PS5_CHMOD_MODE").unwrap_or_else(|_| "0777".to_string());
    eprintln!("chmod -R {mode} {remote} (on PS5 {addr})");
    let t0 = std::time::Instant::now();
    fs_ops::fs_chmod_with_timeout(
        &addr,
        &remote,
        &mode,
        true,
        Some(std::time::Duration::from_secs(600)),
    )
    .expect("recursive chmod");
    eprintln!(
        "chmod -R {mode} {remote} OK in {:.2}s",
        t0.elapsed().as_secs_f64()
    );
}

/// Ports `live_ps5_syslog_tail_smoke`: the console's kernel log tail round-trips and has content.
#[test]
#[ignore = "requires REAL_PS5_ADDR"]
fn live_ps5_syslog_tail_smoke() {
    let Some(addr) = console_addr() else { return };
    let log = ps5upload_core::hw::syslog_tail(&addr).expect("syslog_tail");
    eprintln!(
        "syslog_tail returned {} bytes / {} lines",
        log.len(),
        log.lines().count()
    );
    assert!(
        !log.is_empty(),
        "syslog should not be empty on a running PS5"
    );
    for line in log.lines().rev().take(6).collect::<Vec<_>>().iter().rev() {
        eprintln!("  | {line}");
    }
}

/// Ports `live_ps5_folder_upload_smoke`: the realistic game-dump folder to
/// `/data/ps5upload/tests/smoke_<tag>`, success and reasonable throughput.
#[test]
#[ignore = "requires REAL_PS5_ADDR pointing at a live console"]
fn live_ps5_folder_upload_smoke() {
    let Some(addr) = console_addr() else { return };
    let t = tempdir();
    let expected = build_game_folder(t.path());
    let total: u64 = expected.iter().map(|(_, b)| b.len() as u64).sum();
    let cfg = TransferConfig::new(&addr).with_default_excludes();
    let dest_root = format!("/data/ps5upload/tests/smoke_{}", tag());
    let t0 = std::time::Instant::now();
    let r = upload::upload_dir(&cfg, job_id(0x5B), &dest_root, t.path())
        .unwrap_or_else(|e| panic!("live PS5 folder upload failed: {e:#}"));
    let elapsed = t0.elapsed();
    assert_eq!(r.bytes_sent, total, "bytes_sent should match the plan");
    eprintln!(
        "live PS5 folder upload: {total} bytes / {} files in {elapsed:?} ({:.1} MB/s) -> {dest_root}",
        expected.len(),
        (total as f64 / 1_000_000.0) / elapsed.as_secs_f64().max(0.001)
    );
}

/// Ports `live_ps5_folder_upload_perf`: a REAL folder on disk (`REAL_PS5_SRC_DIR`) to
/// `/data/ps5upload/tests/perf_<tag>`, timed.
#[test]
#[ignore = "requires REAL_PS5_ADDR and REAL_PS5_SRC_DIR=/path/to/folder"]
fn live_ps5_folder_upload_perf() {
    let (Some(addr), Some(src)) = (console_addr(), env("REAL_PS5_SRC_DIR")) else {
        return;
    };
    let src = PathBuf::from(src);
    assert!(src.is_dir(), "REAL_PS5_SRC_DIR is not a directory: {src:?}");
    let cfg = TransferConfig::new(&addr).with_default_excludes();
    let dest_root = format!("/data/ps5upload/tests/perf_{}", tag());
    eprintln!("uploading {src:?} -> PS5 {dest_root}");
    let t0 = std::time::Instant::now();
    let r = upload::upload_dir(&cfg, job_id(0x5C), &dest_root, &src)
        .unwrap_or_else(|e| panic!("live PS5 perf upload failed: {e:#}"));
    let elapsed = t0.elapsed();
    let mib = r.bytes_sent as f64 / (1024.0 * 1024.0);
    eprintln!(
        "{mib:.1} MiB in {:.2}s = {:.1} MiB/s -> {dest_root}",
        elapsed.as_secs_f64(),
        mib / elapsed.as_secs_f64().max(0.001)
    );
}

/// Ports `streams_a_rar_to_a_real_console_and_reads_it_back` (`hw_rar_stream.rs`): the fixture
/// archive streams to the console and reads back byte for byte; cleaned up after.
#[test]
#[ignore = "requires REAL_PS5_ADDR"]
fn live_ps5_streams_a_rar_and_reads_it_back() {
    let Some(addr) = console_addr() else { return };
    let dest_root = "/data/ps5upload/streamtest";
    let cfg = TransferConfig::new(&addr);
    let r = upload::upload_rar(
        &cfg,
        job_id(0x5D),
        dest_root,
        &fixture("crypted.rar"),
        Some("unrar"),
    )
    .expect("streamed to the console");
    eprintln!("committed: {} bytes -> {dest_root}", r.bytes_sent);
    let landed_path = format!("{dest_root}/.gitignore");
    let got = fs_ops::fs_read(&addr, &landed_path, 0, 4096).expect("read back from the console");
    assert_eq!(
        String::from_utf8_lossy(&got),
        "target\nCargo.lock\n",
        "the console holds different bytes than the archive contained"
    );
    let _ = fs_ops::fs_delete(&addr, &landed_path);
    let _ = fs_ops::fs_delete(&addr, dest_root);
}

/// Ports `streams_a_real_game_subset_to_a_console` (`hw_rar_stream.rs`): a real multi-volume,
/// password-protected game archive (`REAL_RAR`, `REAL_RAR_PW`) with the multi-GB payloads
/// excluded; every file must land at the size its archive header declared.
#[test]
#[ignore = "requires REAL_PS5_ADDR and REAL_RAR"]
fn live_ps5_streams_a_real_game_subset() {
    let (Some(addr), Some(real)) = (console_addr(), env("REAL_RAR")) else {
        return;
    };
    let pw = std::env::var("REAL_RAR_PW").ok();
    let archive = PathBuf::from(real);
    let dest_root = "/data/ps5upload/streamtest-real";
    let excludes: Vec<String> = vec!["package/**".into(), "movies/**".into()];
    let (manifest, _src) =
        ps5upload_ava1::rar_source::RarSource::open(&archive, pw.as_deref(), &excludes)
            .unwrap_or_else(|_| panic!("plan the archive"));
    let expected: Vec<(String, u64)> = manifest
        .entries
        .iter()
        .filter(|e| e.kind == ava1::gen::ENTRY_FILE)
        .map(|e| (e.path.clone(), e.size))
        .collect();
    eprintln!("streaming {} files", expected.len());
    let mut cfg = TransferConfig::new(&addr);
    cfg.excludes = excludes;
    let t0 = std::time::Instant::now();
    let r = upload::upload_rar(&cfg, job_id(0x5E), dest_root, &archive, pw.as_deref())
        .expect("streamed to the console");
    eprintln!("committed {} bytes in {:?}", r.bytes_sent, t0.elapsed());
    let mut bad = Vec::new();
    for (rel, want) in &expected {
        let path = format!("{dest_root}/{rel}");
        match fs_ops::fs_stat(&addr, &path) {
            Ok(st) if st.size == *want => {}
            Ok(st) => bad.push(format!("{rel}: {} bytes, expected {want}", st.size)),
            Err(e) => bad.push(format!("{rel}: {e}")),
        }
    }
    let _ = fs_ops::fs_delete(&addr, dest_root);
    assert!(bad.is_empty(), "problems:\n{}", bad.join("\n"));
    eprintln!("verified {} files on the console", expected.len());
}

/// Ports `real_archive_stream.rs`: a real multi-volume archive (`REAL_RAR`, `REAL_RAR_PW`)
/// uploads to a loopback console with every entry at its declared size, in archive order. That
/// catches ordering, volume merging and split-file handling no one-entry fixture can. No console
/// is needed (the receiver is the Rust host); use `--release`.
#[test]
#[ignore = "needs REAL_RAR: a real multi-volume archive"]
fn real_archive_uploads_every_entry_at_its_declared_size() {
    let Some(real) = env("REAL_RAR") else { return };
    let pw = std::env::var("REAL_RAR_PW").ok();
    let archive = PathBuf::from(real);
    let (manifest, _src) =
        ps5upload_ava1::rar_source::RarSource::open(&archive, pw.as_deref(), &[])
            .unwrap_or_else(|_| panic!("plan the archive"));
    let expected: Vec<(String, u64)> = manifest
        .entries
        .iter()
        .filter(|e| e.kind == ava1::gen::ENTRY_FILE)
        .map(|e| (e.path.clone(), e.size))
        .collect();
    let total: u64 = expected.iter().map(|(_, s)| s).sum();
    println!("plan: {} files, {total} bytes", expected.len());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let c = rt.block_on(console());
    let (pool, path) = (c.pool.clone(), archive.clone());
    let r = upload::upload_rar_in(&pool, &cfg(), job_id(0x5F), "dst", &path, pw.as_deref())
        .expect("upload");
    assert_eq!(r.bytes_sent, total);
    let got = landed(&c.share.join("dst"));
    let mut problems = Vec::new();
    for (name, size) in &expected {
        match got.get(name) {
            None => problems.push(format!("MISSING: {name} ({size} bytes)")),
            Some(b) if b.len() as u64 != *size => {
                problems.push(format!("SIZE for {name}: landed={} plan={size}", b.len()))
            }
            Some(_) => {}
        }
    }
    assert!(problems.is_empty(), "problems:\n{}", problems.join("\n"));
    println!("OK: all {} entries matched name and size", expected.len());
}
