//! BLAKE3 shard-digest microbenchmark.
//!
//! Isolates the hashing cost from TCP so we can understand what fraction of
//! transfer time is pure computation vs network overhead.
//!
//! Run: cargo bench -p ps5upload-bench --bench hash

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use ps5upload_core::hash_shard;

fn bench_hash(c: &mut Criterion) {
    let mut group = c.benchmark_group("hash_shard");

    // Sizes spanning typical use: tiny record, page, shard fragment, full shard.
    let sizes: &[(usize, &str)] = &[
        (4 * 1024, "4 KB"),
        (64 * 1024, "64 KB"),
        (1024 * 1024, "1 MB"),
        (8 * 1024 * 1024, "8 MB"),
        (32 * 1024 * 1024, "32 MB"), // default shard size
    ];

    for &(size, label) in sizes {
        let data = vec![0xABu8; size];
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("blake3", label), &data, |b, d| {
            b.iter(|| hash_shard(std::hint::black_box(d)));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_hash);
criterion_main!(benches);
