//! Opt-in hardware check: stream a `.rar` to a REAL PS5 and read it back.
//!
//! Ignored by default. Run with the console's transfer port:
//!
//! ```text
//! PS5_ADDR=192.168.86.100:9113 \
//!   cargo test --release -p ps5upload-tests --test hw_rar_stream \
//!   -- --ignored --nocapture
//! ```
//!
//! The mock server proves the wire format; only a console proves the
//! payload accepts what we send and writes the right bytes to disk.
#![cfg(not(target_os = "android"))]

use ps5upload_core::fs_ops;
use ps5upload_core::transfer::{transfer_rar_streaming, TransferConfig};
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../ps5upload-core/testdata/rar")
        .join(name)
}

#[test]
#[ignore = "needs a real PS5: set PS5_ADDR"]
fn streams_a_rar_to_a_real_console_and_reads_it_back() {
    let Ok(addr) = std::env::var("PS5_ADDR") else {
        eprintln!("PS5_ADDR not set — skipping");
        return;
    };
    // Somewhere obviously ours, cleaned up at the end.
    let dest_root = "/data/ps5upload/streamtest";

    let cfg = TransferConfig::new(&addr);
    let tx = {
        let mut id = [0u8; 16];
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        id[..8].copy_from_slice(&ns.to_le_bytes());
        id
    };

    let res = transfer_rar_streaming(
        &cfg,
        tx,
        dest_root,
        &fixture("crypted.rar"),
        Some("unrar"),
        0,
    )
    .expect("streamed to console");
    println!(
        "committed: {} shards, {} bytes -> {dest_root}",
        res.shards_sent, res.bytes_sent
    );

    // Read it back off the console and compare bytes. `.gitignore` is the
    // fixture's only entry and its contents are known exactly.
    //
    // Filesystem ops live on the MANAGEMENT port (9114); the transfer port
    // (9113) answers `wrong_port` for them. Derive one from the other rather
    // than making the caller pass both.
    let mgmt = {
        let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(&addr);
        format!("{host}:9114")
    };
    let landed = format!("{dest_root}/.gitignore");
    let got = fs_ops::fs_read(&mgmt, &landed, 0, 4096).expect("read back from console");
    assert_eq!(
        String::from_utf8_lossy(&got),
        "target\nCargo.lock\n",
        "console holds different bytes than the archive contained"
    );
    println!(
        "verified on console: {landed} = {:?}",
        String::from_utf8_lossy(&got)
    );

    let _ = fs_ops::fs_delete(&mgmt, &landed);
    let _ = fs_ops::fs_delete(&mgmt, dest_root);
    println!("cleaned up {dest_root}");
}

/// A real game archive, multi-volume and password-protected, with the
/// multi-GB payload files excluded so it stays polite to the console's disk.
///
/// The single-entry fixture proves the wire works; this proves the real
/// thing works — 25 files from a 9-volume set, each verified on the console
/// against the size its archive header declared.
#[test]
#[ignore = "needs a real PS5 and REAL_RAR"]
fn streams_a_real_game_subset_to_a_console() {
    let (Ok(addr), Ok(real)) = (std::env::var("PS5_ADDR"), std::env::var("REAL_RAR")) else {
        eprintln!("PS5_ADDR / REAL_RAR not set — skipping");
        return;
    };
    let pw = std::env::var("REAL_RAR_PW").ok();
    let archive = PathBuf::from(real);
    let dest_root = "/data/ps5upload/streamtest-real";
    let mgmt = {
        let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(&addr);
        format!("{host}:9114")
    };

    // Skip the multi-GB package/movie payloads: this is about correctness,
    // not filling somebody's console.
    let excludes: Vec<String> = vec!["package/**".into(), "movies/**".into()];
    let (total, expected) =
        ps5upload_core::transfer::rar_plan_entries_for_test(&archive, pw.as_deref(), &excludes)
            .expect("plan");
    println!("streaming {} files, {} bytes", expected.len(), total);

    let mut cfg = TransferConfig::new(&addr);
    cfg.excludes = excludes;
    let tx = {
        let mut id = [0u8; 16];
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        id[..8].copy_from_slice(&ns.to_le_bytes());
        id[15] = 0xA5;
        id
    };

    let t0 = std::time::Instant::now();
    let res = transfer_rar_streaming(&cfg, tx, dest_root, &archive, pw.as_deref(), 0)
        .expect("streamed to console");
    println!(
        "committed {} shards, {} bytes in {:?}",
        res.shards_sent,
        res.bytes_sent,
        t0.elapsed()
    );

    // Verify every file landed at the size its header declared.
    let mut bad = Vec::new();
    for (rel, want) in &expected {
        let path = format!("{dest_root}/{rel}");
        match fs_ops::fs_read(&mgmt, &path, want.saturating_sub(1), 8) {
            Ok(tail) if !tail.is_empty() || *want == 0 => {}
            Ok(_) => bad.push(format!("{rel}: empty tail, expected {want} bytes")),
            Err(e) => bad.push(format!("{rel}: {e}")),
        }
    }
    let listing = fs_ops::list_dir(&mgmt, dest_root, Default::default());
    println!("dest listing ok: {}", listing.is_ok());

    let _ = fs_ops::fs_delete(&mgmt, dest_root);
    println!("cleaned up {dest_root}");
    assert!(bad.is_empty(), "problems:\n{}", bad.join("\n"));
    println!("verified {} files on the console", expected.len());
}
