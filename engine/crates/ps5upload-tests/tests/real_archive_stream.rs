//! Opt-in verification against a REAL multi-volume archive.
//!
//! Ignored by default — it needs an archive nobody can ship. Run it with:
//!
//! ```text
//! REAL_RAR='/path/to/game.part01.rar' REAL_RAR_PW='secret' \
//!   cargo test --release -p ps5upload-tests --test real_archive_stream \
//!   -- --ignored --nocapture
//! ```
//!
//! Worth keeping rather than deleting after one use: every shipped `.rar`
//! fixture holds a SINGLE entry, so no CI test can see ordering, volume
//! merging, or split-file handling. This one caught a real bug — the
//! streaming manifest was being built from the sorted preview instead of
//! archive order, which transposed two adjacent files and would have sent
//! shards out of sequence.
//!
//! Use `--release`. A debug build spends its time in the decompressor.
#![cfg(not(target_os = "android"))]
use ps5upload_core::rar_stream::{next_entry, EntryReader};
use ps5upload_core::transfer::{rar_plan_entries_for_test, spawn_rar_worker_for_test};
use std::io::Read;
use std::path::PathBuf;

#[test]
#[ignore = "needs REAL_RAR: a real multi-volume archive"]
fn real_archive_streams_every_entry_at_its_declared_size() {
    let Ok(real) = std::env::var("REAL_RAR") else {
        eprintln!("REAL_RAR not set — skipping");
        return;
    };
    let a = PathBuf::from(real);
    let pw = std::env::var("REAL_RAR_PW").ok();
    // ARCHIVE order — the same thing transfer_rar_streaming builds its
    // manifest from. The sorted preview would not match the stream.
    let (total, expected) = rar_plan_entries_for_test(&a, pw.as_deref(), &[]).expect("plan");
    println!("plan: {} files, {} bytes", expected.len(), total);

    let (rx, worker) = spawn_rar_worker_for_test(&a, pw.as_deref(), vec![]);
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    let mut seen = 0usize;
    let mut streamed = 0u64;
    let mut problems: Vec<String> = Vec::new();
    let t0 = std::time::Instant::now();

    while let Some(name) = next_entry(&rx).expect("next entry") {
        let mut rd = EntryReader::new(&rx);
        let mut got = 0u64;
        loop {
            let n = rd.read(&mut buf).expect("read entry");
            if n == 0 {
                break;
            }
            got += n as u64;
        }
        // Report rather than panic, so one bad entry does not hide the rest.
        match expected.get(seen) {
            None => problems.push(format!("EXTRA entry {seen}: {name} ({got} bytes)")),
            Some((want_name, want_size)) => {
                if &name != want_name {
                    problems.push(format!("NAME at {seen}: worker={name} plan={want_name}"));
                } else if got != *want_size {
                    problems.push(format!("SIZE for {name}: streamed={got} plan={want_size}"));
                }
            }
        }
        streamed += got;
        seen += 1;
        if seen.is_multiple_of(30) {
            println!(
                "  {seen}/{} entries, {streamed} bytes, {:?}",
                expected.len(),
                t0.elapsed()
            );
        }
    }
    drop(rx);
    let _ = worker.join();

    println!(
        "streamed {seen} entries, {streamed} bytes in {:?}",
        t0.elapsed()
    );
    if seen < expected.len() {
        for (n, s) in &expected[seen..] {
            problems.push(format!("MISSING: {n} ({s} bytes)"));
        }
    }
    if streamed != total {
        problems.push(format!("TOTAL: streamed={streamed} plan={total}"));
    }
    assert!(problems.is_empty(), "problems:\n{}", problems.join("\n"));
    println!("OK: all {seen} entries matched name and size");
}
