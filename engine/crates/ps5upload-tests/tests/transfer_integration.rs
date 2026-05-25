//! Integration tests for the ps5upload transfer pipeline.
//!
//! Each test spins up a mock FTX2 server on loopback, performs a transfer
//! via `ps5upload_core::transfer`, and asserts the correct data landed.
//!
//! The `ci_throughput_gate_*` tests serve as hardware-free regression gates:
//! they measure loopback throughput and fail if it drops below a conservative
//! floor, catching algorithmic regressions without requiring a live PS5.

mod mock_server;
use mock_server::MockServer;

use ps5upload_core::transfer::{transfer_dir, transfer_file, transfer_file_path, TransferConfig};

fn random_tx_id() -> [u8; 16] {
    let mut id = [0u8; 16];
    for (i, b) in id.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(17).wrapping_add(0xAB);
    }
    id
}

// ─── Single-file transfer ─────────────────────────────────────────────────────

#[test]
fn transfer_file_small() {
    let srv = MockServer::start();
    let cfg = TransferConfig::new(&srv.addr);
    let tx_id = random_tx_id();
    let data = b"hello from ps5upload integration test";

    let result = transfer_file(&cfg, tx_id, "/data/test.txt", data).unwrap();

    assert_eq!(result.shards_sent, 1);
    assert_eq!(result.bytes_sent, data.len() as u64);

    let st = srv.state.lock().unwrap();
    let applied = st
        .applied
        .get("/data/test.txt")
        .expect("file should be applied");
    assert_eq!(applied.as_slice(), data.as_ref());
}

#[test]
fn transfer_file_empty_data() {
    let srv = MockServer::start();
    let cfg = TransferConfig::new(&srv.addr);
    let tx_id = random_tx_id();

    // Empty file: 0 bytes still needs ONE empty shard so the payload's
    // direct writer creates the temp file that COMMIT renames into place.
    // (Sending zero shards made commit fail with `direct_rename_failed` on
    // real hardware — the temp file was never created.)
    let result = transfer_file(&cfg, tx_id, "/data/empty.bin", b"").unwrap();
    assert_eq!(result.bytes_sent, 0);
    assert_eq!(
        result.shards_sent, 1,
        "empty file must still send one shard"
    );
    // The mock assembles non-packed shards under dest_root → the empty file
    // is materialised (present, zero-length), not absent.
    let st = srv.state.lock().unwrap();
    assert_eq!(
        st.applied.get("/data/empty.bin").map(|v| v.as_slice()),
        Some(b"".as_ref()),
        "empty file must be created on the payload"
    );
}

#[test]
fn transfer_file_multi_shard() {
    let srv = MockServer::start();
    // Use a tiny shard size so we get multiple shards from a small file.
    let cfg = TransferConfig {
        shard_size: 16,
        ..TransferConfig::new(&srv.addr)
    };
    let tx_id = random_tx_id();
    let data: Vec<u8> = (0u8..=255).collect(); // 256 bytes → 16 shards of 16

    let result = transfer_file(&cfg, tx_id, "/data/multi.bin", &data).unwrap();

    assert_eq!(result.shards_sent, 16);
    assert_eq!(result.bytes_sent, 256);

    let st = srv.state.lock().unwrap();
    let applied = st.applied.get("/data/multi.bin").expect("file applied");
    assert_eq!(*applied, data);
}

#[test]
fn transfer_file_path_streams_from_disk() {
    let tmp = tempdir();
    let src = tmp.path().join("streamed.bin");
    let data: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    std::fs::write(&src, &data).unwrap();

    let srv = MockServer::start();
    let cfg = TransferConfig {
        shard_size: 1024,
        ..TransferConfig::new(&srv.addr)
    };
    let tx_id = random_tx_id();

    let result = transfer_file_path(&cfg, tx_id, "/data/streamed.bin", &src).unwrap();

    assert_eq!(result.shards_sent, 4);
    assert_eq!(result.bytes_sent, data.len() as u64);
    let st = srv.state.lock().unwrap();
    assert_eq!(
        st.applied.get("/data/streamed.bin").unwrap().as_slice(),
        data.as_slice()
    );
}

// ─── Directory transfer ───────────────────────────────────────────────────────

#[test]
fn transfer_dir_basic() {
    // Create a temp directory with a few files.
    let tmp = tempdir();
    std::fs::write(tmp.path().join("a.txt"), b"file-a").unwrap();
    std::fs::write(tmp.path().join("b.txt"), b"file-b contents here").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub/c.txt"), b"nested-c").unwrap();

    let srv = MockServer::start();
    let cfg = TransferConfig::new(&srv.addr);
    let tx_id = random_tx_id();

    let result = transfer_dir(&cfg, tx_id, "/data/dest", tmp.path()).unwrap();

    // 3 small files → default config packs them into a single shard.
    // Verify: tx committed, per-file applied data matches source.
    let st = srv.state.lock().unwrap();
    let tx_hex = &result.tx_id_hex;
    let tx = st.txs.get(tx_hex).expect("tx should exist");
    assert_eq!(tx.state, "committed");
    assert_eq!(
        tx.shards_received, result.shards_sent,
        "all sent shards should be acked"
    );
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

#[test]
fn transfer_dir_multi_shard_per_file() {
    let tmp = tempdir();
    // 100-byte file with shard_size=30 → 4 shards (30+30+30+10).
    // Disable packing to keep the multi-shard-per-file semantics under test.
    let data: Vec<u8> = (0u8..100).collect();
    std::fs::write(tmp.path().join("big.bin"), &data).unwrap();

    let srv = MockServer::start();
    let cfg = TransferConfig {
        shard_size: 30,
        pack_size: 0,
        ..TransferConfig::new(&srv.addr)
    };
    let tx_id = random_tx_id();

    let result = transfer_dir(&cfg, tx_id, "/data/d", tmp.path()).unwrap();

    assert_eq!(result.shards_sent, 4); // ceil(100/30) = 4
    assert_eq!(result.bytes_sent, 100);
}

#[test]
fn transfer_dir_single_small_file_is_packed_kind2() {
    // Regression for the `packed_unsupported` report (Astro Bot, many tiny
    // files): a multi-file (kind==2) folder transfer can legitimately narrow
    // to EXACTLY ONE small file — e.g. a resume/reconcile that finds a single
    // remaining file. The engine still PACKS that lone small file (packing
    // keys off file SIZE, not file count), so it sends a kind==2 BEGIN_TX with
    // file_count==1 followed by a PACKED STREAM_SHARD. The payload must accept
    // that packed shard. The C payload previously gated packed-shard
    // acceptance on `file_count > 1` and rejected it as `packed_unsupported`;
    // the fix routes on the transaction's multi_file flag (kind==2) instead.
    //
    // This test pins the ENGINE half (a 1-file folder really does emit a
    // kind==2 packed transfer and round-trips byte-exact through the mock).
    // The payload-side acceptance fix lives in payload/src/runtime.c and is
    // validated on hardware.
    let tmp = tempdir();
    let data: Vec<u8> = (0..777u32).map(|j| (j & 0xff) as u8).collect();
    std::fs::write(tmp.path().join("solo.bin"), &data).unwrap();

    let srv = MockServer::start();
    let cfg = TransferConfig {
        pack_size: 8 * 1024, // > file size → the lone small file is packed
        ..TransferConfig::new(&srv.addr)
    };
    let tx_id = random_tx_id();

    let result = transfer_dir(&cfg, tx_id, "/data/solo_dir", tmp.path()).unwrap();
    assert_eq!(result.shards_sent, 1, "one small file = one packed shard");
    assert_eq!(result.bytes_sent, 777);

    let st = srv.state.lock().unwrap();
    assert_eq!(st.txs.get(&result.tx_id_hex).unwrap().state, "committed");
    assert_eq!(
        st.applied
            .get("/data/solo_dir/solo.bin")
            .map(|v| v.as_slice()),
        Some(data.as_slice()),
        "lone packed file should round-trip byte-exact",
    );
}

#[test]
fn transfer_dir_packed_small_files() {
    // Explicit packing test: 20 × 512-byte files, pack_size=8 KiB so they all
    // fit in ONE packed shard. Verifies the packing happy path: one STREAM_SHARD
    // carries all records, mock's packed parser applies each to its path.
    let tmp = tempdir();
    for i in 0..20u32 {
        let name = format!("f{i:03}.bin");
        let payload: Vec<u8> = (0..512u32).map(|j| ((i * 31 + j) & 0xff) as u8).collect();
        std::fs::write(tmp.path().join(&name), &payload).unwrap();
    }

    let srv = MockServer::start();
    let cfg = TransferConfig {
        pack_size: 8 * 1024,
        ..TransferConfig::new(&srv.addr)
    };
    let tx_id = random_tx_id();

    let result = transfer_dir(&cfg, tx_id, "/data/p", tmp.path()).unwrap();

    // 20 files × 512 B = 10 KiB data plus 20 × (8+6)=280 B of record prefix + paths.
    // Fits in a single 8 KiB pack target? No — 10 KiB > 8 KiB. Expect 2 packs.
    assert!(
        result.shards_sent >= 1 && result.shards_sent <= 20,
        "expected 1..20 shards, got {}",
        result.shards_sent
    );
    assert_eq!(result.bytes_sent, 20 * 512);

    let st = srv.state.lock().unwrap();
    assert_eq!(st.txs.get(&result.tx_id_hex).unwrap().state, "committed");
    // Regression guard: the mock's COMMIT_TX used to unconditionally
    // overwrite `applied[dest_root]` with an empty byte vec when the
    // spool was empty (packed transfers don't use the spool). That
    // silently corrupted any test that checked the root key. The
    // commit path now skips that write when spool is empty, so the
    // root key should be absent entirely.
    assert!(
        !st.applied.contains_key("/data/p"),
        "dest_root key should not exist for a pack-only transfer; found empty-bytes pollution",
    );
    for i in 0..20u32 {
        let name = format!("f{i:03}.bin");
        let key = format!("/data/p/{name}");
        let expected: Vec<u8> = (0..512u32).map(|j| ((i * 31 + j) & 0xff) as u8).collect();
        assert_eq!(
            st.applied.get(&key).map(|v| v.as_slice()),
            Some(expected.as_slice()),
            "file {name} applied data mismatch"
        );
    }
}

#[test]
fn transfer_dir_adversarial_packed_roundtrip() {
    // A deliberately nasty game-dump-shaped folder, exercised end to end and
    // checked BYTE-EXACT per file. This is the regression guard for the
    // "ps5upload corrupts a game folder" class: it would catch a path/data
    // desync in the packer, a record-framing off-by-one, a path-separator or
    // UTF-8 encoding bug, or 0/1-byte mishandling — all of which produce a
    // file that's the wrong size OR the right size with wrong content/path.
    //
    // Every file is < pack_file_max so it takes the packed path, and the
    // pack target is tiny so the records span MANY packed shards (the
    // multi-shard packing path, where a framing bug would surface).
    let tmp = tempdir();
    // (relative path, size). Mix of: nested dirs, spaces, non-ASCII, leading
    // dots, 0-byte, 1-byte, and a size that forces a pack-boundary split.
    let files: Vec<(&str, usize)> = vec![
        ("eboot.bin", 1500),
        ("sce_sys/param.json", 800),
        ("sce_sys/icon0.png", 0),                  // 0-byte file
        ("sce_sys/about/right.sprx", 1),           // 1-byte file
        ("Image0/deep/nested/dir/data.dat", 2049), // spans the pack target
        ("name with spaces.bin", 600),
        ("unicodé_名前.bin", 700), // non-ASCII path
        ("dots...in.name", 333),
    ];
    // Deterministic, file-specific byte pattern so a swap between two files
    // (same size) is still caught.
    let mk = |seed: usize, n: usize| -> Vec<u8> {
        (0..n)
            .map(|j| ((seed * 131 + j * 7) & 0xff) as u8)
            .collect()
    };
    for (i, (rel, sz)) in files.iter().enumerate() {
        let p = tmp.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, mk(i + 1, *sz)).unwrap();
    }

    let srv = MockServer::start();
    let cfg = TransferConfig {
        pack_size: 2048, // tiny target → records span multiple packed shards
        ..TransferConfig::new(&srv.addr)
    };
    let tx_id = random_tx_id();
    let result = transfer_dir(&cfg, tx_id, "/data/game", tmp.path()).unwrap();

    let expected_total: u64 = files.iter().map(|(_, s)| *s as u64).sum();
    assert_eq!(result.bytes_sent, expected_total, "total data bytes");

    let st = srv.state.lock().unwrap();
    assert_eq!(st.txs.get(&result.tx_id_hex).unwrap().state, "committed");
    for (i, (rel, sz)) in files.iter().enumerate() {
        // PS5 dest paths are always forward-slash (join_ps5_path normalizes
        // the host separator); the mock keys `applied` by the path the
        // engine embedded in each packed record, so a wrong path here means
        // the file would land in the wrong place on a real console.
        let key = format!("/data/game/{}", rel.replace('\\', "/"));
        let expected = mk(i + 1, *sz);
        assert_eq!(
            st.applied.get(&key).map(|v| v.as_slice()),
            Some(expected.as_slice()),
            "file {rel} mismatch (wrong path or wrong/corrupt content)"
        );
    }
    // No stray dest_root pollution and no extra files invented.
    assert_eq!(
        st.applied.len(),
        files.len(),
        "exactly the source files should have landed, no more, no less"
    );
}

// ─── Protocol correctness ─────────────────────────────────────────────────────

#[test]
fn hello_round_trip() {
    use ftx2_proto::FrameType;
    use ps5upload_core::connection::Connection;

    let srv = MockServer::start();
    let mut c = Connection::connect(&srv.addr).unwrap();
    c.send_frame(FrameType::Hello, b"{}").unwrap();
    let (hdr, body) = c.recv_frame().unwrap();
    assert_eq!(hdr.frame_type().unwrap(), FrameType::HelloAck);
    assert!(!body.is_empty());
}

#[test]
fn abort_tx_marks_aborted() {
    use ftx2_proto::{FrameType, TxMeta};
    use ps5upload_core::connection::Connection;

    let srv = MockServer::start();
    let tx_id = random_tx_id();
    let tx_hex: String = tx_id.iter().map(|b| format!("{b:02x}")).collect();

    // BEGIN_TX
    {
        let extra = r#"{"dest_root":"/tmp/x","total_shards":1,"total_bytes":10,"file_count":1}"#
            .to_string();
        let mut body = TxMeta {
            tx_id,
            kind: 1,
            flags: 0,
        }
        .encode()
        .to_vec();
        body.extend_from_slice(extra.as_bytes());
        let mut c = Connection::connect(&srv.addr).unwrap();
        c.send_frame(FrameType::BeginTx, &body).unwrap();
        let (hdr, _) = c.recv_frame().unwrap();
        assert_eq!(hdr.frame_type().unwrap(), FrameType::BeginTxAck);
    }

    // ABORT_TX
    {
        let body = TxMeta {
            tx_id,
            kind: 0,
            flags: 0,
        }
        .encode();
        let mut c = Connection::connect(&srv.addr).unwrap();
        c.send_frame(FrameType::AbortTx, &body).unwrap();
        let (hdr, _) = c.recv_frame().unwrap();
        assert_eq!(hdr.frame_type().unwrap(), FrameType::AbortTxAck);
    }

    let st = srv.state.lock().unwrap();
    assert_eq!(st.txs[&tx_hex].state, "aborted");
}

/// `ps5upload_core::transfer::abort_transaction` (the public helper
/// that wraps a single ABORT_TX/ABORT_TX_ACK round-trip) marks the
/// matching tx as aborted on the server side. Companion to
/// `abort_tx_marks_aborted` above, which exercises the raw frame
/// dance; this one proves the high-level API works.
///
/// Wiring this to a UI "Cancel" button requires verifying on PS5
/// hardware that the abort actually preempts a long-running shard
/// write (mock has no shard-write blocking semantics). See the
/// doc-comment on `abort_transaction` for the constraint.
#[test]
fn abort_transaction_helper_marks_aborted() {
    use ftx2_proto::{FrameType, TxMeta};
    use ps5upload_core::connection::Connection;
    use ps5upload_core::transfer::abort_transaction;

    let srv = MockServer::start();
    let tx_id = random_tx_id();
    let tx_hex: String = tx_id.iter().map(|b| format!("{b:02x}")).collect();

    // BEGIN_TX so the tx exists on the server.
    {
        let extra = r#"{"dest_root":"/tmp/x","total_shards":1,"total_bytes":10,"file_count":1}"#
            .to_string();
        let mut body = TxMeta {
            tx_id,
            kind: 1,
            flags: 0,
        }
        .encode()
        .to_vec();
        body.extend_from_slice(extra.as_bytes());
        let mut c = Connection::connect(&srv.addr).unwrap();
        c.send_frame(FrameType::BeginTx, &body).unwrap();
        let (hdr, _) = c.recv_frame().unwrap();
        assert_eq!(hdr.frame_type().unwrap(), FrameType::BeginTxAck);
    }

    let res = abort_transaction(&srv.addr, tx_id).expect("abort_transaction failed");
    assert!(
        std::str::from_utf8(&res.ack_body)
            .unwrap_or("")
            .contains("{"),
        "ACK body should look like a JSON object",
    );

    let st = srv.state.lock().unwrap();
    assert_eq!(st.txs[&tx_hex].state, "aborted");
}

/// 2.12.0 added a state guard on the payload-side ABORT_TX handler
/// (`payload/src/runtime.c` near the runtime_acquire_tx_entry call)
/// that refuses to re-finalize a terminal tx — without it, a
/// replayed ABORT_TX from a confused client could overwrite a
/// "committed" record's state with "aborted" in the journal. Mock
/// server mirrors that guard; this test exercises the wire shape.
///
/// The guard only refuses on terminal states (`aborted` / `committed`).
/// State `interrupted` (network drop mid-tx) remains abortable — that's
/// the canonical "user clicks Cancel on a dangling resumable tx" path.
#[test]
fn double_abort_returns_tx_already_terminal() {
    use ftx2_proto::{FrameType, TxMeta};
    use ps5upload_core::connection::Connection;
    use ps5upload_core::transfer::abort_transaction;

    let srv = MockServer::start();
    let tx_id = random_tx_id();

    // BEGIN_TX → tx is "active". Keep the connection ALIVE through
    // the first abort so the mock's per-connection drop handler
    // doesn't mark the tx as "interrupted" (which is a legitimately-
    // abortable state — see `interrupted_tx_is_still_abortable` for
    // that flow).
    let extra =
        r#"{"dest_root":"/tmp/x","total_shards":1,"total_bytes":10,"file_count":1}"#.to_string();
    let mut body = TxMeta {
        tx_id,
        kind: 1,
        flags: 0,
    }
    .encode()
    .to_vec();
    body.extend_from_slice(extra.as_bytes());
    let mut c = Connection::connect(&srv.addr).unwrap();
    c.send_frame(FrameType::BeginTx, &body).unwrap();
    let (hdr, _) = c.recv_frame().unwrap();
    assert_eq!(hdr.frame_type().unwrap(), FrameType::BeginTxAck);

    // First ABORT_TX on a fresh conn — tx becomes "aborted".
    abort_transaction(&srv.addr, tx_id).expect("first abort should succeed");
    // Second ABORT_TX should be refused by the guard.
    let err = abort_transaction(&srv.addr, tx_id).expect_err("double-abort should surface as Err");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("tx_already_terminal"),
        "expected tx_already_terminal, got: {msg}",
    );
    drop(c);
}

/// State `interrupted` (set when the BEGIN_TX connection dropped
/// before COMMIT/ABORT) must remain abortable — the user clicks
/// Cancel on a dangling resumable tx and we want the abort to
/// succeed, not get refused as "already terminal".
///
/// The mock's connection-drop handler marks tx "interrupted" after
/// the per-connection thread joins; that's async vs our test thread,
/// so we don't try to observe the intermediate state directly.
/// What matters is: regardless of whether abort wins the race vs the
/// drop handler, the abort must succeed (active → abort = OK;
/// interrupted → abort = OK; only aborted/committed are refused).
#[test]
fn interrupted_tx_is_still_abortable() {
    use ftx2_proto::{FrameType, TxMeta};
    use ps5upload_core::connection::Connection;
    use ps5upload_core::transfer::abort_transaction;

    let srv = MockServer::start();
    let tx_id = random_tx_id();

    {
        let extra = r#"{"dest_root":"/tmp/x","total_shards":1,"total_bytes":10,"file_count":1}"#
            .to_string();
        let mut body = TxMeta {
            tx_id,
            kind: 1,
            flags: 0,
        }
        .encode()
        .to_vec();
        body.extend_from_slice(extra.as_bytes());
        let mut c = Connection::connect(&srv.addr).unwrap();
        c.send_frame(FrameType::BeginTx, &body).unwrap();
        let (hdr, _) = c.recv_frame().unwrap();
        assert_eq!(hdr.frame_type().unwrap(), FrameType::BeginTxAck);
    }
    // Give the server's per-connection thread a beat to detect the
    // close so it transitions the tx to "interrupted". 50ms is more
    // than enough on localhost.
    std::thread::sleep(std::time::Duration::from_millis(50));
    // Abort should succeed — interrupted is NOT terminal.
    abort_transaction(&srv.addr, tx_id).expect("abort on an interrupted tx should succeed");
}

#[test]
fn blake3_mismatch_returns_error() {
    use ftx2_proto::{FrameType, ShardHeader, TxMeta};
    use ps5upload_core::connection::Connection;

    let srv = MockServer::start();
    let tx_id = random_tx_id();

    // BEGIN_TX
    {
        let extra = r#"{"dest_root":"/tmp/x","total_shards":1,"total_bytes":4,"file_count":1}"#;
        let mut body = TxMeta {
            tx_id,
            kind: 1,
            flags: 0,
        }
        .encode()
        .to_vec();
        body.extend_from_slice(extra.as_bytes());
        let mut c = Connection::connect(&srv.addr).unwrap();
        c.send_frame(FrameType::BeginTx, &body).unwrap();
        c.recv_frame().unwrap();
    }

    // STREAM_SHARD with wrong (non-zero) digest
    {
        let shard_hdr = ShardHeader {
            tx_id,
            shard_seq: 1,
            shard_digest: [0xFFu8; 32], // deliberately wrong
            record_count: 1,
            flags: 0,
        };
        let data = b"test";
        let mut c = Connection::connect(&srv.addr).unwrap();
        c.send_frame_split(FrameType::StreamShard, &shard_hdr.encode(), data)
            .unwrap();
        let (hdr, body) = c.recv_frame().unwrap();
        assert_eq!(hdr.frame_type().unwrap(), FrameType::Error);
        assert!(String::from_utf8_lossy(&body).contains("digest_mismatch"));
    }
}

// ─── Tempdir helper ──────────────────────────────────────────────────────────

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir() -> TempDir {
    // Include thread id + nanosecond timestamp so parallel test runs
    // don't collide on the same `ps5upload_test_<ts>` directory — with
    // enough tests in flight the nanos-only version races in practice.
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tid = std::thread::current().id();
    let path = std::env::temp_dir().join(format!("ps5upload_test_{ts}_{tid:?}_{seq}"));
    std::fs::create_dir_all(&path).unwrap();
    TempDir(path)
}

// ─── CI throughput gates ─────────────────────────────────────────────────────
//
// These run entirely on loopback against the mock server — no PS5 required.
// They catch algorithmic regressions (e.g. unnecessary copies, serialisation
// bugs) before they reach hardware.
//
// Floors are profile-aware because BLAKE3 runs ~10× faster in release
// (SIMD + inlined). `cargo test` defaults to debug, so a single 50 MiB/s
// floor would fail locally and in CI despite release throughput clearing
// 400+ MiB/s. Live PS5 hardware runs ~6 MiB/s (network-bound), so even
// the debug floor gives ~3× headroom over the real transport.

#[cfg(debug_assertions)]
const CI_THROUGHPUT_FLOOR_MIB_PER_SEC: f64 = 20.0;
#[cfg(not(debug_assertions))]
const CI_THROUGHPUT_FLOOR_MIB_PER_SEC: f64 = 200.0;

fn assert_throughput(label: &str, bytes: usize, elapsed: std::time::Duration) {
    let mib_per_sec = (bytes as f64) / (1024.0 * 1024.0) / elapsed.as_secs_f64();
    println!(
        "[gate] {label}: {:.1} MiB/s  ({} MiB in {:.3}s)",
        mib_per_sec,
        bytes / (1024 * 1024),
        elapsed.as_secs_f64()
    );
    assert!(
        mib_per_sec >= CI_THROUGHPUT_FLOOR_MIB_PER_SEC,
        "{label}: throughput {mib_per_sec:.1} MiB/s < floor {CI_THROUGHPUT_FLOOR_MIB_PER_SEC} MiB/s"
    );
}

/// Single 32 MiB file through the mock server.
#[test]
fn ci_throughput_gate_single_file_32mib() {
    let srv = MockServer::start();
    let cfg = TransferConfig::new(&srv.addr);
    let tx_id = random_tx_id();

    const SIZE: usize = 32 * 1024 * 1024;
    let data = vec![0x5Au8; SIZE];

    let t0 = std::time::Instant::now();
    let result = transfer_file(&cfg, tx_id, "/data/gate_32m.bin", &data).unwrap();
    let elapsed = t0.elapsed();

    assert_eq!(result.bytes_sent, SIZE as u64);
    assert_throughput("single-file 32 MiB", SIZE, elapsed);
}

/// Directory: 16 × 2 MiB files (32 MiB total).
#[test]
fn ci_throughput_gate_dir_16x2mib() {
    let srv = MockServer::start();
    let cfg = TransferConfig::new(&srv.addr);
    let tx_id = random_tx_id();
    let td = tempdir();

    const FILE_SIZE: usize = 2 * 1024 * 1024;
    const FILE_COUNT: usize = 16;
    const TOTAL: usize = FILE_SIZE * FILE_COUNT;

    for i in 0..FILE_COUNT {
        let path = td.path().join(format!("f{i:03}.bin"));
        std::fs::write(&path, vec![0xA5u8; FILE_SIZE]).unwrap();
    }

    let t0 = std::time::Instant::now();
    let result = transfer_dir(&cfg, tx_id, "/data/gate_dir", td.path()).unwrap();
    let elapsed = t0.elapsed();

    assert_eq!(result.bytes_sent, TOTAL as u64);
    assert_throughput("dir 16×2 MiB", TOTAL, elapsed);
}

// ─── Excludes integration ─────────────────────────────────────────────────────

/// Roadmap #8 end-to-end: when a folder has OS detritus (.DS_Store,
/// Thumbs.db) and editor backups (*.esbak), cfg.with_default_excludes()
/// causes transfer_dir to skip them before the manifest is built. This
/// is the regression guard that proves "excludes wired into transfer_dir"
/// isn't just a library function nobody calls.
#[test]
fn transfer_dir_respects_default_excludes() {
    let tmp = tempdir();
    // Payload: one real file, one .DS_Store, one .esbak, one nested .git
    std::fs::write(tmp.path().join("eboot.bin"), vec![0u8; 1024]).unwrap();
    std::fs::write(tmp.path().join(".DS_Store"), b"junk").unwrap();
    std::fs::write(tmp.path().join("map.esbak"), b"editor backup").unwrap();
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    std::fs::write(tmp.path().join(".git/HEAD"), b"ref: refs/heads/main").unwrap();
    std::fs::write(tmp.path().join("Thumbs.db"), b"windows junk").unwrap();

    let srv = MockServer::start();
    let cfg = TransferConfig::new(&srv.addr).with_default_excludes();
    let tx_id = random_tx_id();

    let result = transfer_dir(&cfg, tx_id, "/data/excluded", tmp.path()).unwrap();

    // Only eboot.bin should have landed.
    assert_eq!(result.bytes_sent, 1024, "only eboot.bin bytes transferred");

    let st = srv.state.lock().unwrap();
    assert!(
        st.applied.contains_key("/data/excluded/eboot.bin"),
        "eboot should be applied"
    );
    assert!(
        !st.applied.contains_key("/data/excluded/.DS_Store"),
        ".DS_Store should be excluded"
    );
    assert!(
        !st.applied.contains_key("/data/excluded/map.esbak"),
        "*.esbak should be excluded"
    );
    assert!(
        !st.applied.contains_key("/data/excluded/.git/HEAD"),
        ".git/** should be excluded"
    );
    assert!(
        !st.applied.contains_key("/data/excluded/Thumbs.db"),
        "Thumbs.db should be excluded"
    );
}

#[test]
fn transfer_dir_without_excludes_includes_everything() {
    // Default config (empty excludes) preserves legacy behavior — junk
    // files still ship through. Proves the filter is opt-in.
    let tmp = tempdir();
    std::fs::write(tmp.path().join("a.bin"), b"real").unwrap();
    std::fs::write(tmp.path().join(".DS_Store"), b"junk").unwrap();

    let srv = MockServer::start();
    let cfg = TransferConfig::new(&srv.addr); // no .with_default_excludes()
    let tx_id = random_tx_id();
    transfer_dir(&cfg, tx_id, "/data/all", tmp.path()).unwrap();

    let st = srv.state.lock().unwrap();
    assert!(st.applied.contains_key("/data/all/a.bin"));
    assert!(
        st.applied.contains_key("/data/all/.DS_Store"),
        "default behavior: include everything"
    );
}

// ─── Resume tests ─────────────────────────────────────────────────────────────

use ps5upload_core::transfer::{is_retryable_transfer_error, transfer_file_resumable};

/// Full roadmap #5 round-trip: the mock drops the connection after
/// `drop_after_shards` shards, the client reconnects with TX_FLAG_RESUME,
/// the server reports `last_acked_shard`, the client skips past it, and
/// the final committed bytes on the server match the source exactly.
#[test]
fn transfer_file_resumes_after_mid_stream_drop() {
    let srv = MockServer::start();
    let cfg = TransferConfig {
        shard_size: 1024, // tiny shards → many chunks → easy drop point
        ..TransferConfig::new(&srv.addr)
    };
    let tx_id = random_tx_id();

    // 10 shards × 1 KiB = 10 KiB payload, drop after 3 acked.
    let mut data = Vec::with_capacity(10 * 1024);
    for i in 0..10u8 {
        data.extend(std::iter::repeat_n(i, 1024));
    }

    // Arm the drop: after 3 ACKs, the mock returns from the inner
    // handler, triggering the connection to close and the tx to be
    // marked "interrupted."
    srv.state.lock().unwrap().drop_after_shards = Some(3);

    // Call the resumable wrapper with max_retries=3. It should drop,
    // retry with RESUME flag, send the remaining 7 shards, and commit.
    // Important: clear the drop trigger after the first interrupt so
    // the retry doesn't hit the same trap and loop forever.
    let srv_state = srv.state.clone();
    let watcher = std::thread::spawn(move || {
        // Poll until the tx reaches "interrupted" (meaning the drop
        // fired and the connection closed), then clear the trap so the
        // retry can finish. This mimics a real scenario where the drop
        // is transient.
        loop {
            let st = srv_state.lock().unwrap();
            let is_interrupted = st.txs.values().any(|tx| tx.state == "interrupted");
            if is_interrupted {
                drop(st);
                srv_state.lock().unwrap().drop_after_shards = None;
                return;
            }
            drop(st);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });

    let result = transfer_file_resumable(&cfg, tx_id, "/data/resume.bin", &data, 3, 0)
        .expect("resumable transfer must succeed after reconnect");
    watcher.join().unwrap();

    // bytes_sent reflects the full plan size (10 × 1 KiB) regardless of
    // the split-across-attempts detail. shards_sent, by contrast, is
    // what the SUCCESSFUL (second) attempt transmitted — shards 4-10,
    // since the mock reported last_acked_shard=3 on BeginTxAck after
    // the reconnect.
    assert_eq!(result.bytes_sent, data.len() as u64);
    assert_eq!(
        result.shards_sent, 7,
        "second attempt sent shards 4..=10 (3 already on server)"
    );

    // The final bytes on the mock "disk" match the source.
    let st = srv.state.lock().unwrap();
    let applied = st.applied.get("/data/resume.bin").expect("file committed");
    assert_eq!(applied.len(), data.len(), "length match");
    assert_eq!(applied.as_slice(), data.as_slice(), "byte-identical");

    // Tx is committed on the server side, not left "interrupted."
    let tx = st.txs.values().next().expect("one tx present");
    assert_eq!(tx.state, "committed");
    assert_eq!(
        tx.shards_received, 10,
        "mock saw all 10 shards across both attempts"
    );
}

/// Protocol contract check: RESUME flag on a fresh tx_id is a no-op —
/// the payload/mock still accepts it and reports last_acked_shard=0.
#[test]
fn transfer_with_resume_flag_on_fresh_txid_is_noop() {
    let srv = MockServer::start();
    let cfg = TransferConfig {
        shard_size: 512,
        ..TransferConfig::new(&srv.addr)
    };
    let tx_id = random_tx_id();
    let data = vec![0xC3u8; 2048];

    // max_retries=0 means "no retry" — first (and only) attempt still
    // uses flags=0 (fresh BeginTx). This test just exercises the code
    // path to prove transfer_file_resumable behaves identically to
    // transfer_file when nothing fails.
    let result = transfer_file_resumable(&cfg, tx_id, "/data/fresh.bin", &data, 0, 0).unwrap();
    assert_eq!(result.bytes_sent, data.len() as u64);
    let st = srv.state.lock().unwrap();
    assert_eq!(
        st.applied.get("/data/fresh.bin").unwrap().as_slice(),
        data.as_slice()
    );
}

/// Single-file user-initiated resume: caller supplies the same tx_id
/// as a prior (interrupted) attempt, with `initial_flags=TX_FLAG_RESUME`.
/// The mock has 3 shards already journalled from the prior run; the
/// new attempt must skip those and send only shards 4..=10 — that's
/// the protocol contract we wire up for the user-clicked Retry/Resume
/// flow on a failed single-file upload (e.g. WiFi-drop on a 64 GiB
/// image). Mirrors the directory uploader's resume contract.
#[test]
fn transfer_file_resumable_with_initial_resume_flag_skips_acked_shards() {
    use ps5upload_core::transfer::TX_FLAG_RESUME;
    let srv = MockServer::start();
    // Plant a pre-existing interrupted tx with last_acked_shard=3 so a
    // resume attempt sees the mock report "you're already at 3 — send
    // shards 4-10." (Spool isn't populated because this test cares about
    // the resume *contract* — which shards the sender sends — not about
    // mock-side reassembly of pre-existing partial bytes.)
    let tx_id = random_tx_id();
    srv.plant_interrupted_tx(tx_id, "/data/userresume.bin", 10, 3);
    let cfg = TransferConfig {
        shard_size: 1024,
        ..TransferConfig::new(&srv.addr)
    };
    let data: Vec<u8> = (0u32..10_240).map(|i| (i & 0xFF) as u8).collect();

    // initial_flags=TX_FLAG_RESUME — the very first BeginTx asks the
    // mock to adopt the existing entry. max_retries=0 means we expect
    // success on the first attempt; no need to drop the connection.
    let result = transfer_file_resumable(
        &cfg,
        tx_id,
        "/data/userresume.bin",
        &data,
        0,
        TX_FLAG_RESUME,
    )
    .expect("resumable transfer with supplied tx_id must succeed");

    // The contract assertions: bytes_sent reflects the full plan size
    // (regardless of resume split), shards_sent reflects only what
    // this attempt actually transmitted.
    assert_eq!(result.bytes_sent, data.len() as u64);
    assert_eq!(
        result.shards_sent, 7,
        "first (and only) attempt sent shards 4..=10; shards 1-3 already on mock"
    );
    // Mock should record 3 (planted) + 7 (new) = 10 received shards
    // total, and the tx state should be committed (not interrupted).
    let st = srv.state.lock().unwrap();
    let tx_hex = result.tx_id_hex.clone();
    let tx = st.txs.get(&tx_hex).expect("tx present after commit");
    assert_eq!(
        tx.shards_received, 10,
        "all 10 shards received across attempts"
    );
    assert_eq!(tx.state, "committed");
}

/// Defensive: if the payload (bug or stale state) reports
/// `last_acked_shard > total_shards`, the engine must REFUSE to
/// commit, not blindly send zero shards and finalize the tx. That
/// would produce a "ghost commit" — an upload finalised against a
/// destination that has only a partial prior attempt's bytes.
#[test]
fn transfer_file_refuses_ghost_commit_on_bogus_last_acked() {
    use ps5upload_core::transfer::TX_FLAG_RESUME;
    let srv = MockServer::start();
    // Plant a tx where shards_received is impossibly high (15 > 10).
    let tx_id = random_tx_id();
    srv.plant_interrupted_tx(tx_id, "/data/ghost.bin", 10, 15);
    let cfg = TransferConfig {
        shard_size: 1024,
        ..TransferConfig::new(&srv.addr)
    };
    let data: Vec<u8> = vec![0u8; 10_240]; // would fit in 10 shards

    let r = transfer_file_resumable(&cfg, tx_id, "/data/ghost.bin", &data, 0, TX_FLAG_RESUME);
    assert!(r.is_err(), "engine must refuse bogus last_acked_shard");
    let err_str = r.unwrap_err().to_string();
    assert!(
        err_str.contains("last_acked_shard") && err_str.contains("total_shards"),
        "error should explain the discrepancy: {err_str}"
    );
}

/// Same ghost-commit guard as `transfer_file`, but for the directory
/// path — which previously LACKED the bound check (the guard lived only
/// on the single-file paths). A bogus `last_acked_shard > total_shards`
/// must make the dir transfer refuse to commit, not skip every shard and
/// finalize a send that transmitted nothing. Covers the shared
/// `guard_last_acked` helper on a multi-file path.
#[test]
fn transfer_dir_refuses_ghost_commit_on_bogus_last_acked() {
    use ps5upload_core::transfer::{transfer_dir_resumable, TX_FLAG_RESUME};
    let tmp = tempdir();
    // A few multi-shard files so the real plan has a small, concrete
    // total_shards that the planted bogus cursor clearly exceeds.
    for i in 0..6 {
        std::fs::write(tmp.path().join(format!("f{i}.bin")), vec![0u8; 4096]).unwrap();
    }
    let srv = MockServer::start();
    let tx_id = random_tx_id();
    // shards_received = 9999 is impossibly high for this plan, so the
    // mock reports last_acked_shard=9999 and the guard must fire.
    srv.plant_interrupted_tx(tx_id, "/data/ghostdir", 1, 9999);
    let cfg = TransferConfig {
        shard_size: 1024,
        ..TransferConfig::new(&srv.addr)
    };

    let r = transfer_dir_resumable(&cfg, tx_id, "/data/ghostdir", tmp.path(), 0, TX_FLAG_RESUME);
    assert!(
        r.is_err(),
        "dir transfer must refuse bogus last_acked_shard"
    );
    let err = r.unwrap_err().to_string();
    assert!(
        err.contains("last_acked_shard") && err.contains("total_shards"),
        "error should explain the discrepancy: {err}"
    );
}

/// `is_retryable_transfer_error` correctly classifies io::ErrorKind
/// variants. Purely a predicate unit test — no mock server needed.
#[test]
fn is_retryable_transfer_error_kinds() {
    fn wrap(kind: std::io::ErrorKind) -> anyhow::Error {
        anyhow::Error::new(std::io::Error::new(kind, "test"))
    }
    assert!(is_retryable_transfer_error(&wrap(
        std::io::ErrorKind::ConnectionReset
    )));
    assert!(is_retryable_transfer_error(&wrap(
        std::io::ErrorKind::BrokenPipe
    )));
    assert!(is_retryable_transfer_error(&wrap(
        std::io::ErrorKind::TimedOut
    )));
    assert!(is_retryable_transfer_error(&wrap(
        std::io::ErrorKind::UnexpectedEof
    )));
    assert!(is_retryable_transfer_error(&wrap(
        std::io::ErrorKind::ConnectionAborted
    )));

    // Non-retryable kinds: surface the error directly, no resume dance.
    assert!(!is_retryable_transfer_error(&wrap(
        std::io::ErrorKind::InvalidData
    )));
    assert!(!is_retryable_transfer_error(&wrap(
        std::io::ErrorKind::PermissionDenied
    )));

    // Non-io errors (e.g. protocol decode failures) also not retryable.
    assert!(!is_retryable_transfer_error(&anyhow::anyhow!(
        "direct_tx_corrupt"
    )));
}

/// Cross-session resume: the client persists the tx_id from a prior
/// (interrupted) upload and re-supplies it on the next attempt. The
/// resumable wrapper's `initial_flags = TX_FLAG_RESUME` on attempt 0
/// signals "adopt existing entry" instead of falling into the mock's
/// restart-in-place branch. Without the flag, attempt 0 would overwrite
/// the interrupted MockTx and last_acked_shard would come back as 0 —
/// the whole thing would re-upload from scratch.
#[test]
fn transfer_file_list_initial_flags_resume_adopts_existing() {
    use ps5upload_core::transfer::{transfer_file_list_resumable, FileListEntry, TX_FLAG_RESUME};

    let srv = MockServer::start();
    let tx_id = random_tx_id();
    let hex: String = tx_id.iter().map(|b| format!("{b:02x}")).collect();

    // Seed the mock with an interrupted tx entry carrying partial
    // shard state, simulating a prior upload that dropped mid-stream.
    {
        let mut st = srv.state.lock().unwrap();
        st.txs.insert(
            hex.clone(),
            mock_server::MockTx {
                state: "interrupted".to_string(),
                shards_received: 2,
                bytes_received: 2048,
                dest_root: "/data/resume".to_string(),
                total_shards: 0,
                spool: std::collections::HashMap::new(),
            },
        );
    }

    // Create two tiny files that together produce multiple shards
    // (shard_size=512 forces the splitter). We want to verify the
    // client skips the first two shards per the mock's
    // last_acked_shard = 2.
    let tmp = tempdir();
    let f1 = tmp.path().join("a.bin");
    let f2 = tmp.path().join("b.bin");
    std::fs::write(&f1, vec![0xAAu8; 1024]).unwrap();
    std::fs::write(&f2, vec![0xBBu8; 1024]).unwrap();

    let cfg = TransferConfig {
        shard_size: 512,
        pack_size: 0, // force one shard per file chunk
        pack_file_max: 0,
        ..TransferConfig::new(&srv.addr)
    };
    let entries = vec![
        FileListEntry {
            src: f1.to_string_lossy().into_owned(),
            dest: "/data/resume/a.bin".to_string(),
        },
        FileListEntry {
            src: f2.to_string_lossy().into_owned(),
            dest: "/data/resume/b.bin".to_string(),
        },
    ];

    // Call with max_retries=0 (no retries) and initial_flags=RESUME so
    // the single attempt uses TX_FLAG_RESUME, mimicking the engine's
    // cross-session flow on a fresh reconcile request where the client
    // supplied a tx_id from persistence.
    let result =
        transfer_file_list_resumable(&cfg, tx_id, "/data/resume", &entries, 0, TX_FLAG_RESUME)
            .expect("resumable call must adopt existing interrupted entry");

    // With last_acked_shard=2, the client skipped shards 1-2 and sent
    // shards 3+. Four 512-byte shards total across two 1024-byte files,
    // so shards_sent should be 2 (not 4).
    assert_eq!(
        result.shards_sent, 2,
        "should skip the 2 already-acked shards reported by the mock"
    );

    // Mock reports commit OK; tx state ends up "committed".
    let st = srv.state.lock().unwrap();
    let tx = st.txs.get(&hex).expect("tx entry present");
    assert_eq!(tx.state, "committed");
}
