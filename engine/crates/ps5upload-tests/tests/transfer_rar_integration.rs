//! Integration tests for the `.rar` transfer path.
//!
//! The load-bearing test here is equivalence: what lands on the console must
//! not depend on whether the bytes were staged to disk first or streamed.

#![cfg(not(target_os = "android"))]

mod mock_server;
use mock_server::MockServer;

use ps5upload_core::transfer::{transfer_rar_resumable, transfer_rar_streaming, TransferConfig};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn tx_id(seed: u8) -> [u8; 16] {
    let mut id = [0u8; 16];
    for (i, b) in id.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(31).wrapping_add(seed);
    }
    id
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../ps5upload-core/testdata/rar")
        .join(name)
}

type Landed = BTreeMap<String, Vec<u8>>;

fn landed(srv: &MockServer) -> Landed {
    srv.state
        .lock()
        .unwrap()
        .applied
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[test]
fn streamed_output_is_byte_identical_to_staged() {
    // The gate for the whole change: whatever the console ends up with must
    // not depend on which path produced it.
    let staged = {
        let srv = MockServer::start();
        let cfg = TransferConfig {
            shard_size: 4096,
            ..TransferConfig::new(&srv.addr)
        };
        transfer_rar_resumable(
            &cfg,
            tx_id(0x5C),
            "/data/g",
            &fixture("crypted.rar"),
            Some("unrar"),
            0,
            0,
        )
        .expect("staged");
        landed(&srv)
    };
    let streamed = {
        let srv = MockServer::start();
        let cfg = TransferConfig {
            shard_size: 4096,
            ..TransferConfig::new(&srv.addr)
        };
        transfer_rar_streaming(
            &cfg,
            tx_id(0x5C),
            "/data/g",
            &fixture("crypted.rar"),
            Some("unrar"),
            0,
        )
        .expect("streamed");
        landed(&srv)
    };

    assert!(
        !staged.is_empty(),
        "fixture produced nothing — the test would prove nothing"
    );
    assert_eq!(
        streamed, staged,
        "streaming landed different bytes than staging"
    );
}

#[test]
fn a_small_shard_size_still_reassembles_correctly() {
    // Forces many shards per file, so an off-by-one at a chunk/shard boundary
    // shows up as wrong bytes instead of passing by luck.
    let srv = MockServer::start();
    let cfg = TransferConfig {
        shard_size: 7,
        ..TransferConfig::new(&srv.addr)
    };
    transfer_rar_streaming(
        &cfg,
        tx_id(0x21),
        "/data/g",
        &fixture("crypted.rar"),
        Some("unrar"),
        0,
    )
    .expect("streamed");
    let got = landed(&srv);
    assert!(!got.is_empty());
    assert!(got.values().all(|v| !v.is_empty()));
}

#[test]
fn a_wrong_password_fails_the_same_way_staging_did() {
    // `crypted.rar` is CONTENT-encrypted: the names list fine with any
    // password, and only the data is protected. UnRAR cannot tell a wrong
    // password from corruption there, so it reports a CRC error rather than
    // a password code — and the staged path did exactly the same.
    //
    // Do not "fix" this into rar_password_wrong: a genuinely corrupt archive
    // produces the same CRC error, and mislabelling that as a bad password
    // sends people hunting for a password that was never the problem.
    let staged_msg = {
        let srv = MockServer::start();
        let cfg = TransferConfig::new(&srv.addr);
        format!(
            "{:#}",
            transfer_rar_resumable(
                &cfg,
                tx_id(0x33),
                "/data/g",
                &fixture("crypted.rar"),
                Some("nope"),
                0,
                0,
            )
            .unwrap_err()
        )
    };
    let streamed_msg = {
        let srv = MockServer::start();
        let cfg = TransferConfig::new(&srv.addr);
        format!(
            "{:#}",
            transfer_rar_streaming(
                &cfg,
                tx_id(0x33),
                "/data/g",
                &fixture("crypted.rar"),
                Some("nope"),
                0,
            )
            .unwrap_err()
        )
    };
    assert_eq!(
        streamed_msg, staged_msg,
        "streaming reported a different failure than staging did"
    );
}

#[test]
fn a_missing_password_still_reports_a_password_error() {
    let srv = MockServer::start();
    let cfg = TransferConfig::new(&srv.addr);
    let err = transfer_rar_streaming(
        &cfg,
        tx_id(0x44),
        "/data/g",
        &fixture("crypted.rar"),
        None,
        0,
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("rar_password"), "got {msg}");
}

#[test]
fn resuming_skips_shards_the_console_already_has() {
    // Streaming cannot seek backwards, so a resume re-decompresses from the
    // start and simply does not re-send what is already committed. This is
    // the rule the 7z path uses, and it is only sound because shard numbers
    // ascend in send order — see rar_plan_entries.
    use ps5upload_core::transfer::TX_FLAG_RESUME;

    // A tiny shard size turns the fixture's single entry into several
    // shards, so "skipped some but not all" is actually observable.
    let fresh = {
        let srv = MockServer::start();
        let cfg = TransferConfig {
            shard_size: 4,
            ..TransferConfig::new(&srv.addr)
        };
        transfer_rar_streaming(
            &cfg,
            tx_id(0x77),
            "/data/g",
            &fixture("crypted.rar"),
            Some("unrar"),
            0,
        )
        .expect("fresh")
        .shards_sent
    };
    assert!(fresh > 2, "need several shards for this to mean anything");

    let srv = MockServer::start();
    let cfg = TransferConfig {
        shard_size: 4,
        ..TransferConfig::new(&srv.addr)
    };
    // Pretend the console already acked the first two shards.
    srv.plant_interrupted_tx(tx_id(0x77), "/data/g", fresh, 2);
    let resumed = transfer_rar_streaming(
        &cfg,
        tx_id(0x77),
        "/data/g",
        &fixture("crypted.rar"),
        Some("unrar"),
        TX_FLAG_RESUME,
    )
    .expect("resumed")
    .shards_sent;

    assert_eq!(
        resumed,
        fresh - 2,
        "a resume should send every shard except the two already acked"
    );
}
