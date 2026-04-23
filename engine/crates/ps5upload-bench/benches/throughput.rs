//! End-to-end transfer throughput benchmark.
//!
//! Measures MB/s for the full host-side FTX2 pipeline: BLAKE3 hashing,
//! frame serialization, TCP send, and ACK parsing.  The mock server performs
//! no disk I/O, so results reflect host throughput only — not PS5 storage.
//!
//! To measure against live hardware set PS5_ADDR and run the lab tool instead.
//!
//! Run: cargo bench -p ps5upload-bench --bench throughput

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use ps5upload_bench::start_bench_server;
use ps5upload_core::transfer::{transfer_file, transfer_file_list, FileListEntry, TransferConfig};
use uuid::Uuid;

fn tx_id() -> [u8; 16] {
    *Uuid::new_v4().as_bytes()
}

// ─── Single-file throughput ───────────────────────────────────────────────────

fn bench_transfer_file(c: &mut Criterion) {
    let addr = start_bench_server();
    let mut group = c.benchmark_group("transfer_file");
    group.sample_size(10); // fewer samples — transfers are slow relative to criterion's default

    let sizes: &[(usize, &str)] = &[
        (64 * 1024, "64 KB"),
        (512 * 1024, "512 KB"),
        (4 * 1024 * 1024, "4 MB"),
        (32 * 1024 * 1024, "32 MB"), // exactly one default shard
        (64 * 1024 * 1024, "64 MB"), // two shards
    ];

    for &(size, label) in sizes {
        let data = vec![0xABu8; size];
        let cfg = TransferConfig::new(&addr);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("size", label), &data, |b, data| {
            b.iter(|| transfer_file(&cfg, tx_id(), "/bench/file.bin", data).unwrap());
        });
    }

    group.finish();
}

// ─── Shard-size sensitivity ───────────────────────────────────────────────────
//
// Transfers a fixed 8 MB payload with different shard sizes to show the
// per-shard protocol overhead (connection setup + frame headers).

fn bench_shard_size(c: &mut Criterion) {
    let addr = start_bench_server();
    let mut group = c.benchmark_group("shard_size");
    group.sample_size(10);

    const PAYLOAD: usize = 8 * 1024 * 1024; // 8 MB fixed
    let data = vec![0xCDu8; PAYLOAD];

    let shard_sizes: &[(usize, &str)] = &[
        (64 * 1024, "64 KB shards"),
        (512 * 1024, "512 KB shards"),
        (4 * 1024 * 1024, "4 MB shards"),
        (32 * 1024 * 1024, "32 MB shards"), // single shard
    ];

    group.throughput(Throughput::Bytes(PAYLOAD as u64));
    for &(shard_size, label) in shard_sizes {
        let mut cfg = TransferConfig::new(addr.clone());
        cfg.shard_size = shard_size;
        cfg.max_shard_retries = 3;
        group.bench_with_input(BenchmarkId::new("8MB", label), &cfg, |b, cfg| {
            b.iter(|| transfer_file(cfg, tx_id(), "/bench/shard_test", &data).unwrap());
        });
    }

    group.finish();
}

// ─── Many-small-files throughput ─────────────────────────────────────────────
//
// Measures file-list transfer overhead: N files × K bytes each.
// Shows how per-transaction cost amortises across many files.

fn bench_transfer_file_list(c: &mut Criterion) {
    let addr = start_bench_server();
    let mut group = c.benchmark_group("transfer_file_list");
    group.sample_size(10);

    // Write temp files to disk once; reuse across iterations.
    let tmp_dir = std::env::temp_dir().join("ps5upload_bench_filelist");
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let configs: &[(usize, usize, &str)] = &[
        (10, 64 * 1024, "10 × 64 KB"),
        (100, 64 * 1024, "100 × 64 KB"),
        (10, 1024 * 1024, "10 × 1 MB"),
    ];

    for &(file_count, file_size, label) in configs {
        let data = vec![0xEFu8; file_size];
        let mut entries: Vec<FileListEntry> = Vec::with_capacity(file_count);
        for i in 0..file_count {
            let p = tmp_dir.join(format!("f_{}_{}.bin", file_size, i));
            std::fs::write(&p, &data).unwrap();
            entries.push(FileListEntry {
                src: p.to_string_lossy().into_owned(),
                dest: format!("/bench/{}", p.file_name().unwrap().to_string_lossy()),
            });
        }

        let total_bytes = (file_count * file_size) as u64;
        let cfg = TransferConfig::new(&addr);
        group.throughput(Throughput::Bytes(total_bytes));
        group.bench_with_input(BenchmarkId::new("files", label), &entries, |b, entries| {
            b.iter(|| transfer_file_list(&cfg, tx_id(), "/bench", entries).unwrap());
        });
    }

    // Cleanup
    let _ = std::fs::remove_dir_all(&tmp_dir);
    group.finish();
}

criterion_group!(
    benches,
    bench_transfer_file,
    bench_shard_size,
    bench_transfer_file_list
);
criterion_main!(benches);
