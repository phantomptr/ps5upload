//! Durable-by-log for small files (review 003 §3.2, SPEC.md §15.7), console receiver: the pack log, the
//! two-fsync batch, the sweep, recovery after every crash point, the unswept cap and the settling report.
#![cfg(unix)]
use std::path::PathBuf;

use ava1::gen::{self, ENTRY_DIR, ENTRY_FILE};
use ava1::journal::{job_dir, Journal, State};
use ava1::manifest::{Entry, Manifest};
use ava1_ctest::*;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ava1-logsmall-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn small(n: usize, extra_unsent: bool) -> Manifest {
    let mut entries = vec![Entry {
        kind: ENTRY_DIR,
        mode: 0o755,
        size: 0,
        mtime: 0,
        path: "d".into(),
        root: None,
    }];
    for i in 0..n {
        entries.push(Entry {
            kind: ENTRY_FILE,
            mode: 0o640,
            size: 4,
            mtime: 1_600_000_000,
            path: format!("d/{i}"),
            root: None,
        });
    }
    if extra_unsent {
        // a file nobody sends: the job stays open, so nothing ends it and the sweep is only age-driven
        entries.push(Entry {
            kind: ENTRY_FILE,
            mode: 0o640,
            size: 4,
            mtime: 1_600_000_000,
            path: "never".into(),
            root: None,
        });
    }
    Manifest { entries }
}

fn body(i: usize) -> Vec<u8> {
    format!("{i:04}").into_bytes()
}

fn send(job: &CApplyJob, i: usize) {
    job.record(i as u32 + 1, &body(i), *blake3::hash(&body(i)).as_bytes());
}

fn slow_sweep() -> LogOpts {
    LogOpts {
        sweep_age_ms: 600_000,
        ..LogOpts::ON
    }
}

fn settled(job: &CApplyJob) {
    let t0 = std::time::Instant::now();
    while job.unswept() != 0 || job.segments() != 0 {
        assert!(
            t0.elapsed().as_secs() < 20,
            "never settled: unswept {} segments {}\n{}",
            job.unswept(),
            job.segments(),
            job.events()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn ids_done(ev: &str) -> Vec<u32> {
    let line = ev.lines().find(|l| l.starts_with("map status=0")).unwrap();
    let runs = line
        .split("done=")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap();
    let mut out = vec![];
    for r in runs.split(',').filter(|r| !r.is_empty()) {
        let (a, b) = r.split_once('+').unwrap();
        let (a, b): (u32, u32) = (a.parse().unwrap(), b.parse().unwrap());
        out.extend(a..a + b);
    }
    out
}

#[test]
fn a_logged_batch_costs_two_fsyncs_not_one_per_file() {
    // Test 1 of the design: pack fsync + journal fsync, whatever the number of files. The sweep is
    // held off (age 10 min) and the job kept open so only the batch is counted.
    let n = 300;
    let mut costs = vec![];
    for opts in [slow_sweep(), LogOpts::OFF] {
        let t = tmp("twofsync");
        let root = t.join("dest");
        std::fs::create_dir_all(&root).unwrap();
        let job = CApplyJob::begin_opts(&t.join("jobs"), &root, 0, &small(n, true), 0, 0, opts);
        job.hold_batches(true);
        for i in 0..n {
            send(&job, i);
        }
        job.wait_pending(n as u32, 10_000);
        let c0 = job.fsync_calls();
        job.hold_batches(false);
        job.wait_event("durable", 10_000);
        costs.push(job.fsync_calls() - c0);
        if opts.mode == 1 {
            assert_eq!(job.unswept() as usize, n, "every file waits for the sweep");
            for i in 0..n {
                assert_eq!(std::fs::read(root.join(format!("d/{i}"))).unwrap(), body(i));
            }
        }
    }
    assert_eq!(costs[0], 2, "the logged batch: pack + journal");
    assert!(
        costs[1] >= n as u32,
        "the per-file path syncs every file: {}",
        costs[1]
    );
}

#[test]
fn two_thousand_files_land_settle_and_replay_to_the_same_state() {
    let t = tmp("settle");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let n = 2000;
    let job = CApplyJob::begin_opts(
        &t.join("jobs"),
        &root,
        0,
        &small(n, false),
        0,
        0,
        LogOpts::ON,
    );
    for i in 0..n {
        send(&job, i);
    }
    assert_eq!(job.wait(30_000), 0, "{}", job.events());
    settled(&job); // the sweep deletes every segment
    for i in (0..n).step_by(97) {
        assert_eq!(std::fs::read(root.join(format!("d/{i}"))).unwrap(), body(i));
    }
    drop(job);
    let (_, recs) = Journal::open(&job_dir(&t.join("jobs"), &[7; 16])).unwrap();
    let mut st = State::default();
    for r in &recs {
        st.apply(r);
    }
    assert_eq!(st.done.len(), n);
    assert!(st.unswept.is_empty() && st.packs.is_empty(), "{st:?}");
    assert_eq!(st.finished, Some(0));
}

#[test]
fn jobdone_reports_settling_for_a_merge_and_not_for_a_staged_tree() {
    // merge into an existing folder: JobDone goes out with the log durable, files settle behind it
    let t = tmp("settling");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let job = CApplyJob::begin_opts(
        &t.join("jobs"),
        &root,
        0,
        &small(40, false),
        0,
        0,
        slow_sweep(),
    );
    for i in 0..40 {
        send(&job, i);
    }
    assert_eq!(job.wait(15_000), 0, "{}", job.events());
    assert!(
        job.events().contains("settling\ndone 0"),
        "{}",
        job.events()
    );
    settled(&job); // a finished job sweeps everything at once, whatever the age
    drop(job);
    // a staged tree settles before its rename: the tail waits, JobDone carries no flag
    let t = tmp("settling-staged");
    let job = CApplyJob::begin_opts(
        &t.join("jobs"),
        &t.join("fresh"),
        0,
        &small(40, false),
        0,
        0,
        slow_sweep(),
    );
    for i in 0..40 {
        send(&job, i);
    }
    assert_eq!(job.wait(15_000), 0, "{}", job.events());
    assert!(!job.events().contains("settling"), "{}", job.events());
    assert_eq!(job.unswept(), 0);
    assert_eq!(std::fs::read(t.join("fresh/d/7")).unwrap(), body(7));
}

fn pack(t: &std::path::Path, n: u32) -> PathBuf {
    job_dir(&t.join("jobs"), &[7; 16]).join(format!("pack.{n}"))
}

/// Crashes the console at `crash_at` with a full batch journaled/synced as the point implies, runs
/// `damage`, restarts, and returns the resumed job after resending what its map does not list.
fn crash_and_resume(
    tag: &str,
    crash_at: i32,
    n: usize,
    damage: impl Fn(&std::path::Path),
) -> (CRecv, PathBuf, Vec<u32>) {
    let t = tmp(tag);
    std::fs::create_dir_all(t.join("dest")).unwrap(); // merge: the files are in place at dest/d
    let m = small(n, false);
    let r = CRecv::open_opts(
        &t.join("jobs"),
        &t.join("dest"),
        0,
        gen::POLICY_REPLACE,
        crash_at,
        LogOpts::ON,
    );
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.hold_batches(true);
    for i in 0..n {
        send(&r, i);
    }
    r.wait_pending(n as u32, 5000);
    r.hold_batches(false);
    r.wait_stopped(10_000);
    damage(&t);
    let r = r.restart(0); // helper restart: recovery runs at start, again on JobOpen
    r.manifest(&m);
    let ev = r.wait_event("map status=0", 5000);
    let done = ids_done(&ev);
    for i in 0..n {
        if !done.contains(&(i as u32 + 1)) {
            send(&r, i);
        }
    }
    (r, t, done)
}

fn all_there(t: &std::path::Path, n: usize) {
    for i in 0..n {
        assert_eq!(
            std::fs::read(t.join(format!("dest/d/{i}"))).unwrap(),
            body(i),
            "file {i}"
        );
    }
}

#[test]
fn a_crash_after_the_pack_fsync_before_the_journal_resends_the_batch() {
    // crash point 1: the log is synced, the journal has nothing: no file is done, all are resent
    let (r, t, done) = crash_and_resume("c-before-journal", 1, 30, |_| {});
    assert!(done.is_empty(), "{done:?}");
    assert_eq!(r.wait(15_000), 0, "{}", r.events());
    all_there(&t, 30);
}

#[test]
fn files_lost_with_the_page_cache_come_back_from_the_log() {
    // crash point 2: the journal names the batch, JobDone/Durable never went out; half the files never
    // reached the disk (as after a power cut): recovery rewrites them from the pack, nothing is resent
    let (r, t, done) = crash_and_resume("c-lost-files", 2, 40, |t| {
        for i in 0..20 {
            std::fs::remove_file(t.join(format!("dest/d/{i}"))).unwrap();
        }
        std::fs::write(t.join("dest/d/30"), b"junk").unwrap(); // a wrong-size leftover
    });
    assert_eq!(done.len(), 40, "{done:?}");
    assert_eq!(r.wait(15_000), 0, "{}", r.events());
    all_there(&t, 40);
    settled(&r);
}

#[test]
fn a_torn_log_tail_resends_only_the_files_whose_record_is_unreadable() {
    let (r, t, done) = crash_and_resume("c-torn", 2, 40, |t| {
        let p = pack(t, 0);
        let len = std::fs::metadata(&p).unwrap().len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&p)
            .unwrap()
            .set_len(len - 5)
            .unwrap(); // the last record is cut mid-way
                       // The workers append records in whatever order they win the pack lock, so the cut
                       // record is not necessarily file 39's. Every file is lost with the page cache: the
                       // intact records rebuild theirs from the log, and only the torn record's file is resent.
        for i in 0..40 {
            std::fs::remove_file(t.join(format!("dest/d/{i}"))).unwrap();
        }
    });
    assert_eq!(
        done.len(),
        39,
        "only the torn record's file is resent: {done:?}"
    );
    assert_eq!(r.wait(15_000), 0, "{}", r.events());
    all_there(&t, 40);
}

#[test]
fn a_missing_segment_resends_its_files_and_nothing_else_breaks() {
    let (r, t, done) = crash_and_resume("c-nosegment", 2, 25, |t| {
        std::fs::remove_file(pack(t, 0)).unwrap();
    });
    assert!(done.is_empty(), "{done:?}");
    assert_eq!(r.wait(15_000), 0, "{}", r.events());
    all_there(&t, 25);
}

#[test]
fn a_crash_after_the_sweep_is_journaled_before_the_segment_goes_leaves_nothing_behind() {
    // crash point 10: JnlSweep is durable, the pack file still exists; recovery finds nothing unswept
    // and removes the stray segment
    let (r, t, done) = crash_and_resume("c-after-sweep", 10, 30, |_| {});
    assert!(!done.is_empty(), "{done:?}");
    assert_eq!(r.wait(15_000), 0, "{}", r.events());
    all_there(&t, 30);
    settled(&r);
}

#[test]
fn the_unswept_cap_makes_workers_sweep_and_never_loses_a_file() {
    // Test 3: a tiny cap (128 KiB) and tiny segments (64 KiB) for 1.6 MiB of records: workers sweep
    // instead of writing past the cap, segments roll and are deleted, every byte lands
    let t = tmp("cap");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let n = 400;
    let opts = LogOpts {
        mode: 1,
        pack_segment: 64 << 10,
        unswept_max: 128 << 10,
        sweep_age_ms: 600_000,
        ..LogOpts::ON
    };
    let job = CApplyJob::begin_opts(&t.join("jobs"), &root, 0, &small(n, false), 0, 0, opts);
    let mut most = 0;
    for i in 0..n {
        send(&job, i);
        most = most.max(job.segments());
    }
    let start = std::time::Instant::now();
    assert_eq!(job.wait(60_000), 0, "{}", job.events());
    assert!(start.elapsed().as_secs() < 30);
    assert!(most >= 1, "the log was used");
    settled(&job);
    for i in 0..n {
        assert_eq!(std::fs::read(root.join(format!("d/{i}"))).unwrap(), body(i));
    }
}

#[test]
fn a_compacted_journal_still_names_the_unswept_files_so_a_restart_recovers_them() {
    // The snapshot carries `unswept` and `segments` (they cannot be extensions of records, so they are
    // byte streams): compaction between the batch and the sweep must not lose the way back to the log.
    let t = tmp("compact-unswept");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let n = 30;
    let m = small(n, true);
    let r = CRecv::open_opts(
        &t.join("jobs"),
        &t.join("dest"),
        0,
        gen::POLICY_REPLACE,
        0,
        slow_sweep(),
    );
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.hold_batches(true);
    for i in 0..n {
        send(&r, i);
    }
    r.wait_pending(n as u32, 5000);
    r.hold_batches(false);
    r.wait_event("durable", 10_000);
    assert_eq!(r.compact_now(), 0);
    for i in 0..n {
        std::fs::remove_file(t.join(format!("dest/d/{i}"))).unwrap(); // the page cache is lost
    }
    let r = r.restart(0); // no sweep ran: a helper restart finds the snapshot's unswept list
    r.manifest(&m);
    let ev = r.wait_event("map status=0", 5000);
    assert_eq!(ids_done(&ev).len(), n, "{ev}");
    all_there(&t, n);
}

// ---- review 007 #5: the runtime off-switch (/data/ps5upload/debug/ava1-log-small-off) ----

/// A job begun with the flag file present (and the engine ON) takes the per-file path: every file is
/// fsynced, nothing waits for a sweep and no pack segment exists.
fn flagged_job(t: &std::path::Path, tag: &str, n: usize) -> CApplyJob {
    let root = t.join(tag);
    std::fs::create_dir_all(&root).unwrap();
    let job = CApplyJob::begin_opts(
        &t.join(format!("jobs-{tag}")),
        &root,
        0,
        &small(n, true),
        0,
        0,
        slow_sweep(),
    );
    job.hold_batches(true);
    for i in 0..n {
        send(&job, i);
    }
    job.wait_pending(n as u32, 10_000);
    job
}

#[test]
fn the_flag_file_turns_durable_by_log_off_for_a_job_opened_while_it_exists() {
    let t = tmp("flag-on");
    let flag = t.join("ava1-log-small-off");
    c_set_log_small_flag(Some(&flag));
    let n = 50;
    // flag absent: logged (two fsyncs per batch, every file unswept until the sweep)
    {
        let job = flagged_job(&t, "a", n);
        let c0 = job.fsync_calls();
        job.hold_batches(false);
        job.wait_event("durable", 10_000);
        assert_eq!(job.fsync_calls() - c0, 2, "logged batch");
        assert_eq!(job.unswept() as usize, n);
    }
    // flag present: the per-file path, no log
    std::fs::write(&flag, b"").unwrap();
    {
        let job = flagged_job(&t, "b", n);
        let c0 = job.fsync_calls();
        job.hold_batches(false);
        job.wait_event("durable", 10_000);
        assert!(job.fsync_calls() - c0 >= n as u32, "per-file fsyncs");
        assert_eq!(job.unswept(), 0, "nothing waits for a sweep");
        assert_eq!(job.segments(), 0, "no pack segment was created");
        for i in 0..n {
            assert_eq!(std::fs::read(t.join(format!("b/d/{i}"))).unwrap(), body(i));
        }
    }
    // flag removed again: the next job is logged (toggled between jobs, no restart)
    std::fs::remove_file(&flag).unwrap();
    {
        let job = flagged_job(&t, "c", n);
        job.hold_batches(false);
        job.wait_event("durable", 10_000);
        assert_eq!(job.unswept() as usize, n, "logged again");
    }
    c_set_log_small_flag(None);
}

#[test]
fn a_job_never_switches_paths_when_the_flag_appears_mid_job() {
    let t = tmp("flag-mid");
    let flag = t.join("ava1-log-small-off");
    c_set_log_small_flag(Some(&flag));
    let n = 40;
    let job = flagged_job(&t, "a", n); // opened without the flag: logged
    std::fs::write(&flag, b"").unwrap(); // an operator flips it while the job runs
    let c0 = job.fsync_calls();
    job.hold_batches(false);
    job.wait_event("durable", 10_000);
    assert_eq!(job.fsync_calls() - c0, 2, "the job stayed logged");
    assert_eq!(job.unswept() as usize, n);
    drop(job);
    c_set_log_small_flag(None);
}

#[test]
fn recovery_runs_for_a_logged_job_even_when_the_flag_is_set() {
    // The flag only decides how NEW jobs write. A crashed logged job's journal names pack records that
    // must still be recovered (files lost with the page cache come back from the log).
    let t = tmp("flag-recover");
    let flag = t.join("ava1-log-small-off");
    c_set_log_small_flag(Some(&flag));
    let n = 40;
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let m = small(n, false);
    let r = CRecv::open_opts(
        &t.join("jobs"),
        &t.join("dest"),
        0,
        gen::POLICY_REPLACE,
        2,
        LogOpts::ON,
    );
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.hold_batches(true);
    for i in 0..n {
        send(&r, i);
    }
    r.wait_pending(n as u32, 5000);
    r.hold_batches(false);
    r.wait_stopped(10_000);
    for i in 0..20 {
        std::fs::remove_file(t.join(format!("dest/d/{i}"))).unwrap();
    }
    std::fs::write(&flag, b"").unwrap(); // the operator turns the log off, then the helper restarts
    let r = r.restart(0);
    r.manifest(&m);
    let ev = r.wait_event("map status=0", 5000);
    assert_eq!(ids_done(&ev).len(), n, "{ev}");
    assert_eq!(r.wait(15_000), 0, "{}", r.events());
    all_there(&t, n);
    drop(r);
    c_set_log_small_flag(None);
}

/// Review 007 #8: the sweep's first pass checks content, not only size. A file whose size is right
/// but whose bytes are not (zero-filled blocks after a power cut, a damaged page) is re-made from
/// its log record before it is fsynced and its record released.
#[test]
fn the_sweep_remakes_a_file_with_the_right_size_and_the_wrong_bytes() {
    let t = tmp("sweep-content");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let n = 10;
    let job = CApplyJob::begin_opts(
        &t.join("jobs"),
        &root,
        0,
        &small(n, false),
        0,
        0,
        slow_sweep(), // nothing sweeps until the job ends
    );
    job.hold_batches(true);
    for i in 0..n {
        send(&job, i);
    }
    job.wait_pending(n as u32, 10_000);
    job.hold_batches(false);
    job.wait_event("durable", 10_000);
    assert_eq!(job.unswept() as usize, n);
    std::fs::write(root.join("d/3"), b"zzzz").unwrap(); // same size (4), wrong content
    std::fs::remove_file(root.join("d/7")).unwrap(); // and a lost one: the live sweep re-makes it from its own record
    std::fs::write(root.join("d/9"), b"zz").unwrap(); // and a short one
    assert_eq!(job.wait(15_000), 0, "{}", job.events());
    settled(&job);
    for i in 0..n {
        assert_eq!(
            std::fs::read(root.join(format!("d/{i}"))).unwrap(),
            body(i),
            "file {i}"
        );
    }
}
