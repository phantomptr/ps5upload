//! Per-frame round-trip latency benchmark.
//!
//! Measures the overhead of establishing a connection and completing one
//! FTX2 frame exchange.  This drives connection-per-frame protocol decisions
//! and shows where latency headroom exists.
//!
//! Run: cargo bench -p ps5upload-bench --bench latency

use criterion::{criterion_group, criterion_main, Criterion};
use ftx2_proto::{FrameType, TxMeta};
use ps5upload_bench::start_bench_server;
use ps5upload_core::connection::Connection;
use uuid::Uuid;

fn tx_id() -> [u8; 16] {
    *Uuid::new_v4().as_bytes()
}

fn bench_hello(c: &mut Criterion) {
    let addr = start_bench_server();
    c.bench_function("latency/hello", |b| {
        b.iter(|| {
            let mut conn = Connection::connect(&addr).unwrap();
            conn.send_frame(FrameType::Hello, b"{}").unwrap();
            conn.recv_frame().unwrap();
        });
    });
}

fn bench_status(c: &mut Criterion) {
    let addr = start_bench_server();
    c.bench_function("latency/status", |b| {
        b.iter(|| {
            let mut conn = Connection::connect(&addr).unwrap();
            conn.send_frame(FrameType::Status, b"").unwrap();
            conn.recv_frame().unwrap();
        });
    });
}

fn bench_begin_tx(c: &mut Criterion) {
    let addr = start_bench_server();
    c.bench_function("latency/begin_tx", |b| {
        b.iter(|| {
            let tx_id = tx_id();
            let meta = TxMeta {
                tx_id,
                kind: 1,
                flags: 0,
            };
            let extra =
                br#"{"dest_root":"/bench","total_shards":1,"total_bytes":1,"file_count":1}"#;
            let mut body = meta.encode().to_vec();
            body.extend_from_slice(extra);
            let mut conn = Connection::connect(&addr).unwrap();
            conn.send_frame(FrameType::BeginTx, &body).unwrap();
            conn.recv_frame().unwrap();
        });
    });
}

fn bench_commit_tx(c: &mut Criterion) {
    let addr = start_bench_server();
    c.bench_function("latency/commit_tx", |b| {
        b.iter(|| {
            let tx_id = tx_id();
            let meta = TxMeta {
                tx_id,
                kind: 0,
                flags: 0,
            };
            let mut conn = Connection::connect(&addr).unwrap();
            conn.send_frame(FrameType::CommitTx, &meta.encode())
                .unwrap();
            conn.recv_frame().unwrap();
        });
    });
}

criterion_group!(
    benches,
    bench_hello,
    bench_status,
    bench_begin_tx,
    bench_commit_tx
);
criterion_main!(benches);
