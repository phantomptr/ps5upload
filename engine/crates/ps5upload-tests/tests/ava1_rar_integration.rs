//! AVA1 integration tests for the `.rar` upload path. They replace `transfer_rar_integration.rs`.
//! The load-bearing one is equivalence: what lands on the console must be exactly what the
//! host-side extractor produces from the same archive (the old test compared two upload paths,
//! staged and streamed; the staged path no longer exists, so the oracle is the extractor).

#![cfg(not(target_os = "android"))]

mod ava1_common;
use ava1_common::*;

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use ps5upload_ava1::upload::{self, UploadFailure};
use ps5upload_core::archive_extract::extract;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../ps5upload-core/testdata/rar")
        .join(name)
}

async fn put_rar(
    c: &Console,
    id: u8,
    fixture_name: &str,
    password: Option<&'static str>,
) -> anyhow::Result<ps5upload_core::transfer::TransferResult> {
    let (pool, path) = (c.pool.clone(), fixture(fixture_name));
    run(120, move || {
        upload::upload_rar_in(&pool, &cfg(), job_id(id), "data/g", &path, password)
    })
    .await
}

fn reason(e: &anyhow::Error) -> String {
    e.downcast_ref::<UploadFailure>()
        .unwrap_or_else(|| panic!("a typed failure, got {e:#}"))
        .reason
        .clone()
}

/// Ports `streamed_output_is_byte_identical_to_staged` (and, with it,
/// `a_small_shard_size_still_reassembles_correctly`: every entry lands whole, none empty).
#[tokio::test(flavor = "multi_thread")]
async fn upload_rar_matches_the_host_extraction_byte_for_byte() {
    let c = console().await;
    put_rar(&c, 1, "crypted.rar", Some("unrar")).await.unwrap();
    let uploaded = landed(&c.share.join("data/g"));
    let out = tempdir();
    let never = AtomicBool::new(false);
    extract(
        &fixture("crypted.rar"),
        out.path(),
        Some("unrar"),
        &mut |_, _| {},
        &never,
    )
    .expect("the oracle extracts");
    let oracle = landed(out.path());
    assert!(
        !uploaded.is_empty(),
        "fixture produced nothing: the test would prove nothing"
    );
    assert!(
        uploaded.values().all(|v| !v.is_empty()),
        "an entry landed empty"
    );
    assert_eq!(
        uploaded, oracle,
        "the upload landed different bytes than the extractor"
    );
}

/// Ports `a_wrong_password_fails_the_same_way_staging_did`. `crypted.rar` is CONTENT-encrypted:
/// names list with any password and only the data is protected, so a wrong password fails when
/// the data is read, typed as a wrong password, and the password never appears in the error.
#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_rar_password_is_a_typed_failure_that_does_not_leak_it() {
    let c = console().await;
    let e = put_rar(&c, 2, "crypted.rar", Some("nope"))
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "ava1_rar_password_wrong");
    assert!(!format!("{e:#}").contains("nope"), "the password leaked");
}

/// Ports `a_missing_password_still_reports_a_password_error`.
#[tokio::test(flavor = "multi_thread")]
async fn a_missing_rar_password_asks_for_one() {
    let c = console().await;
    let e = put_rar(&c, 3, "crypted.rar", None).await.unwrap_err();
    assert_eq!(reason(&e), "ava1_rar_password_required");
}

/// Ports `resuming_skips_shards_the_console_already_has`. A resume re-decodes forward and does
/// not re-send what the console already holds durably; the fixture is a single tiny entry, so
/// the observable here is that re-running a finished job under its id is accepted and lands the
/// same bytes. The mid-transfer resume (killed session, skipped entries, reordered listing) is
/// covered with real multi-entry archives by `ps5upload-ava1/tests/rar.rs`
/// (`a_rar_upload_resumes_after_the_connection_drops`, `rar_nonsolid_resume_skips_done_entries`).
#[tokio::test(flavor = "multi_thread")]
async fn re_running_a_finished_rar_job_lands_the_same_bytes() {
    let c = console().await;
    put_rar(&c, 4, "crypted.rar", Some("unrar")).await.unwrap();
    let first = landed(&c.share.join("data/g"));
    put_rar(&c, 4, "crypted.rar", Some("unrar")).await.unwrap();
    assert_eq!(landed(&c.share.join("data/g")), first);
    assert_eq!(
        first[".gitignore"], b"target\nCargo.lock\n",
        "the fixture's first entry"
    );
}
