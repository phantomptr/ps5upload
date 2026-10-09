//! perf-apply (review 003 §4, §8): the receiver's periodic stats line is opt-in like the sender's
//! stage timers (`PS5UPLOAD_AVA1_TIMING`, or the flag file on the console); the end-of-job
//! summary is not.
#![cfg(unix)]
use ava1::gen::ENTRY_FILE;
use ava1::manifest::{Entry, Manifest};
use ava1::verify::GROUP;
use ava1_ctest::*;

fn manifest() -> Manifest {
    Manifest {
        entries: vec![Entry {
            kind: ENTRY_FILE,
            mode: 0o640,
            size: 3 * GROUP + 5,
            mtime: 1_600_000_000,
            path: "big.bin".into(),
            root: None,
        }],
    }
}

fn run_one(tag: &str) -> (bool, String) {
    let t = TempDir::new(format!("ava1-timing-{tag}-{}", std::process::id()));
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &manifest(), 0);
    let on = job.timing_on();
    let d: Vec<u8> = (0..3 * GROUP as usize + 5).map(|i| i as u8).collect();
    for o in (0..d.len()).step_by(GROUP as usize) {
        job.chunk(0, o as u64, &d[o..(o + GROUP as usize).min(d.len())]);
    }
    job.root(0, *blake3::hash(&d).as_bytes());
    assert_eq!(job.wait(15_000), 0, "{}", job.events());
    (on, job.summary())
}

#[test]
fn the_periodic_line_is_opt_in_and_the_end_of_job_summary_is_not() {
    // Its own test binary, so setting the environment cannot race another test.
    std::env::remove_var("PS5UPLOAD_AVA1_TIMING");
    let (on, summary) = run_one("off");
    assert!(!on, "the periodic stats line must be off by default");
    for want in [
        "scan",
        "data",
        "dirs",
        "journal",
        "commit",
        "preallocate",
        "%",
    ] {
        assert!(summary.contains(want), "{want:?} missing from {summary:?}");
    }
    std::env::set_var("PS5UPLOAD_AVA1_TIMING", "1");
    let (on, _) = run_one("on");
    assert!(on, "PS5UPLOAD_AVA1_TIMING turns it on");
    std::env::remove_var("PS5UPLOAD_AVA1_TIMING");
}
