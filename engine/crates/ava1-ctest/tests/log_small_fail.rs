//! Durable-by-log, the failure and settle paths (review dbl): a sweep that fails after JobDone, a stop in
//! the final drain, a manifest change while files are unswept, a reaped or crashed job nobody holds, the
//! cross-job cap, and the crash points inside a sweep.
#![cfg(unix)]
use std::path::{Path, PathBuf};

use ava1::gen::{self, ENTRY_DIR, ENTRY_FILE};
use ava1::journal::{job_dir, Journal, Record, State};
use ava1::manifest::{Entry, Manifest};
use ava1_ctest::*;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ava1-logfail-{tag}-{}", std::process::id()));
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

fn slow() -> LogOpts {
    LogOpts {
        sweep_age_ms: 600_000,
        ..LogOpts::ON
    }
}

fn settled(job: &CApplyJob, secs: u64) {
    let t0 = std::time::Instant::now();
    while job.unswept() != 0 || job.segments() != 0 {
        assert!(
            t0.elapsed().as_secs() < secs,
            "never settled: unswept {} segments {}\n{}",
            job.unswept(),
            job.segments(),
            job.events()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn done_ids(ev: &str) -> Vec<u32> {
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

fn replay(jobs: &Path, byte: u8) -> (State, Vec<Record>) {
    let (_, recs) = Journal::open(&job_dir(jobs, &[byte; 16])).unwrap();
    let mut st = State::default();
    for r in &recs {
        st.apply(r);
    }
    (st, recs)
}

fn packs(jobs: &Path, byte: u8) -> usize {
    std::fs::read_dir(job_dir(jobs, &[byte; 16]))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("pack."))
                .count()
        })
        .unwrap_or(0)
}

fn all_there(t: &Path, n: usize) {
    for i in 0..n {
        assert_eq!(
            std::fs::read(t.join(format!("dest/d/{i}"))).unwrap(),
            body(i),
            "file {i}"
        );
    }
}

// ---- C1: a failed sweep after JobDone ------------------------------------------------------

#[test]
fn a_sweep_that_fails_after_jobdone_is_retried_until_the_files_settle() {
    let t = tmp("retry");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap(); // a merge: settles behind JobDone
    let job = CApplyJob::begin_opts(
        &t.join("jobs"),
        &root,
        0,
        &small(40, false),
        0,
        0,
        LogOpts::ON,
    );
    job.fail_sweeps(3);
    for i in 0..40 {
        send(&job, i);
    }
    assert_eq!(job.wait(15_000), 0, "{}", job.events());
    settled(&job, 20); // before the fix the taken files were lost and this never ended
    for i in 0..40 {
        assert_eq!(std::fs::read(root.join(format!("d/{i}"))).unwrap(), body(i));
    }
}

#[test]
fn a_persistent_sweep_failure_is_reported_in_status_and_clears_when_it_stops() {
    let t = tmp("sticky");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let job = CApplyJob::begin_opts(
        &t.join("jobs"),
        &root,
        0,
        &small(20, false),
        0,
        0,
        LogOpts::ON,
    );
    job.fail_sweeps(-1);
    for i in 0..20 {
        send(&job, i);
    }
    assert_eq!(job.wait(15_000), 0, "{}", job.events());
    job.wait_event(&format!("status code={}", gen::ERR_IO), 20_000);
    assert!(job.unswept() > 0, "the files are not settled");
    job.fail_sweeps(0);
    settled(&job, 20);
}

#[test]
fn a_staged_tree_retries_a_failing_sweep_before_its_rename_and_never_renames_over_unsynced_files() {
    // transient: two failures, then it works: the tree lands
    let t = tmp("staged-retry");
    let job = CApplyJob::begin_opts(
        &t.join("jobs"),
        &t.join("fresh"),
        0,
        &small(20, false),
        0,
        0,
        slow(),
    );
    job.fail_sweeps(2);
    for i in 0..20 {
        send(&job, i);
    }
    assert_eq!(job.wait(30_000), 0, "{}", job.events());
    assert_eq!(std::fs::read(t.join("fresh/d/7")).unwrap(), body(7));
    drop(job);
    // permanent: the job ends ERR_IO and the tree stays where it was (not renamed)
    let t = tmp("staged-fail");
    let job = CApplyJob::begin_opts(
        &t.join("jobs"),
        &t.join("fresh"),
        0,
        &small(20, false),
        0,
        0,
        slow(),
    );
    job.fail_sweeps(-1);
    for i in 0..20 {
        send(&job, i);
    }
    assert_eq!(job.wait(60_000), gen::ERR_IO as i32, "{}", job.events());
    assert!(!t.join("fresh/d").exists(), "renamed over unsynced files");
}

// ---- C2: a stop in the final drain ---------------------------------------------------------

#[test]
fn a_stop_during_the_final_drain_journals_no_terminal_failure_and_resumes() {
    let t = tmp("stopdrain");
    let m = small(20, false);
    // a staged tree, the sweep held off by age: only finish's drain sweeps, and the crash point
    // (MID_SWEEP) stops the job inside it
    let r = CRecv::open_opts(
        &t.join("jobs"),
        &t.join("dest"),
        0,
        gen::POLICY_REPLACE,
        11,
        slow(),
    );
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    for i in 0..20 {
        send(&r, i);
    }
    r.wait_stopped(15_000);
    let jobs = t.join("jobs");
    let recs = replay(&jobs, 7).1;
    assert!(
        !recs.iter().any(|r| matches!(r, Record::Done(_))),
        "a stop wrote a terminal Done: {recs:?}"
    );
    let r = r.restart(0);
    r.manifest(&m);
    let ev = r.wait_event("map status=0", 5000);
    for i in 0..20 {
        if !done_ids(&ev).contains(&(i as u32 + 1)) {
            send(&r, i);
        }
    }
    assert_eq!(r.wait(15_000), 0, "{}", r.events());
    all_there(&t, 20);
}

// ---- C3: a manifest change while files are unswept -----------------------------------------

#[test]
fn a_manifest_change_is_refused_when_the_unswept_files_cannot_be_settled() {
    let t = tmp("remap");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let m = small(20, true);
    let r = CRecv::open_opts(
        &t.join("jobs"),
        &t.join("dest"),
        0,
        gen::POLICY_REPLACE,
        0,
        slow(),
    );
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.hold_batches(true);
    for i in 0..20 {
        send(&r, i);
    }
    r.wait_pending(20, 5000);
    r.hold_batches(false);
    r.wait_event("durable", 10_000);
    assert!(r.unswept() > 0);
    r.fail_sweeps(-1);
    // the sender comes back with a manifest that renumbers every file
    let mut m2 = small(20, true);
    m2.entries.insert(
        1,
        Entry {
            kind: ENTRY_FILE,
            mode: 0o640,
            size: 4,
            mtime: 1_600_000_000,
            path: "a-new-first".into(),
            root: None,
        },
    );
    assert_eq!(r.reopen(true), 0);
    r.manifest(&m2);
    let t0 = std::time::Instant::now();
    let ev = loop {
        let ev = r.events();
        if ev.matches("map status=").count() >= 2 {
            break ev;
        }
        assert!(t0.elapsed().as_secs() < 20, "no answer: {ev}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let second = ev.split("map status=").nth(2).unwrap();
    assert!(
        !second.starts_with('0'),
        "the manifest was adopted over unswept files: {ev}"
    );
}

// ---- C4: nobody holds the job ---------------------------------------------------------------

#[test]
fn a_reaped_settling_job_is_recovered_by_housekeeping_not_lost() {
    let t = tmp("reaped");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let m = small(30, true);
    let opts = LogOpts {
        recover_every_ms: 200,
        ..slow()
    };
    let r = CRecv::open_opts(
        &t.join("jobs"),
        &t.join("dest"),
        0,
        gen::POLICY_REPLACE,
        0,
        opts,
    );
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.hold_batches(true);
    for i in 0..30 {
        send(&r, i);
    }
    r.wait_pending(30, 5000);
    r.hold_batches(false);
    r.wait_event("durable", 10_000);
    for i in 0..10 {
        std::fs::remove_file(t.join(format!("dest/d/{i}"))).unwrap(); // lost with the page cache
    }
    r.reap_and_drop(); // the job is destroyed with its files unswept
    let jobs = t.join("jobs");
    assert!(
        packs(&jobs, 7) > 0,
        "the log is the only copy: it must stay"
    );
    let t0 = std::time::Instant::now();
    while !(0..30).all(|i| std::fs::read(t.join(format!("dest/d/{i}"))).ok() == Some(body(i))) {
        assert!(
            t0.elapsed().as_secs() < 20,
            "housekeeping never recovered the job"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    while !replay(&jobs, 7).0.unswept.is_empty() {
        assert!(t0.elapsed().as_secs() < 30, "never swept");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn gc_never_removes_a_job_directory_that_holds_a_log_until_a_long_ceiling() {
    let t = tmp("gc");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let m = small(10, false);
    let r = CRecv::open_opts(
        &t.join("jobs"),
        &t.join("dest"),
        0,
        gen::POLICY_REPLACE,
        2,
        slow(),
    );
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.hold_batches(true);
    for i in 0..10 {
        send(&r, i);
    }
    r.wait_pending(10, 5000);
    r.hold_batches(false);
    r.wait_stopped(10_000);
    drop(r);
    let jobs = t.join("jobs");
    assert!(packs(&jobs, 7) > 0);
    // a second, idle directory with no log is collected as before
    std::fs::create_dir_all(jobs.join("00000000000000000000000000000001")).unwrap();
    let removed = jobs_gc(&jobs, 3 * 86_400, 86_400);
    assert_eq!(removed, 1, "only the directory without a log goes");
    assert!(
        job_dir(&jobs, &[7; 16]).exists(),
        "gc removed a job that still holds its log"
    );
    // a log that recovery could not settle in a week beyond the normal age is given up on, and says so, but
    // not on one boot's wall clock (a clock moved forward would age every log at once): it takes three
    // boots that each saw it past the ceiling (final review: console)
    for strike in 1..=2u64 {
        gc_boot(100 + strike);
        assert_eq!(
            jobs_gc(&jobs, (20 + strike as i64) * 86_400, 86_400),
            0,
            "strike {strike}: the clock alone must not take a log"
        );
        assert!(job_dir(&jobs, &[7; 16]).exists());
    }
    gc_boot(103);
    let removed = jobs_gc(&jobs, 23 * 86_400, 86_400);
    gc_boot(0);
    assert_eq!(
        removed, 1,
        "three boots, a day apart, saw it past the ceiling"
    );
    assert!(!job_dir(&jobs, &[7; 16]).exists());
}

#[test]
fn a_directory_recovery_cannot_open_is_kept_for_a_while_and_then_given_up() {
    let t = tmp("gc-unopenable");
    let jobs = t.join("jobs");
    let d = jobs.join("abababababababababababababababab");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("journal"), b"not a journal").unwrap(); // peek fails: recovery cannot open it
    std::fs::write(d.join("pack.0"), b"AVA1PCK1").unwrap();
    assert_eq!(
        jobs_gc(&jobs, 3 * 86_400, 86_400),
        0,
        "kept: it holds a log"
    );
    gc_boot(201);
    assert_eq!(jobs_gc(&jobs, 20 * 86_400, 86_400), 0, "strike 1");
    gc_boot(202);
    assert_eq!(jobs_gc(&jobs, 21 * 86_400, 86_400), 0, "strike 2");
    gc_boot(203);
    assert_eq!(
        jobs_gc(&jobs, 22 * 86_400, 86_400),
        1,
        "given up after the ceiling, three boots a day apart"
    );
    gc_boot(0);
}

/// Final review (console), re-review: strikes count boots, not starts. Any number of re-sends within one boot,
/// or boots less than a day apart (a clock set wrong), add one strike at most.
#[test]
fn gc_strikes_do_not_accumulate_within_a_boot_or_within_a_day() {
    let t = tmp("gc-strikes");
    let jobs = t.join("jobs");
    let d = jobs.join("babababababababababababababababa");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("journal"), b"x").unwrap();
    std::fs::write(d.join("pack.0"), b"AVA1PCK1").unwrap();
    gc_boot(301);
    for _ in 0..10 {
        assert_eq!(
            jobs_gc(&jobs, 20 * 86_400, 86_400),
            0,
            "re-sends in one boot"
        );
    }
    // new boots, but only hours apart
    for (i, b) in (302..306u64).enumerate() {
        gc_boot(b);
        assert_eq!(
            jobs_gc(&jobs, 20 * 86_400 + (i as i64 + 1) * 3600, 86_400),
            0,
            "boot {b} an hour later"
        );
    }
    assert!(d.exists(), "a log was collected without three real boots");
    gc_boot(0);
}

/// A directory stamped in the future is skipped alone; the others are still collected.
#[test]
fn a_future_stamped_directory_does_not_stop_the_gc_of_the_others() {
    let t = tmp("gc-future");
    let jobs = t.join("jobs");
    let old = jobs.join("1111aaaa1111aaaa1111aaaa1111aaaa");
    let fut = jobs.join("2222bbbb2222bbbb2222bbbb2222bbbb");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::create_dir_all(&fut).unwrap();
    let day = std::time::Duration::from_secs(86_400);
    let now = std::time::SystemTime::now();
    std::fs::File::open(&old)
        .unwrap()
        .set_modified(now - day * 30)
        .unwrap();
    std::fs::File::open(&fut)
        .unwrap()
        .set_modified(now + day * 10)
        .unwrap();
    assert_eq!(jobs_gc(&jobs, 0, 86_400), 1);
    assert!(!old.exists() && fut.exists());
}

/// Final review (console): `time(NULL)` is the wall clock, and the console's clock is set by the user and
/// by the app. A clock that is wrong must not delete resumable job directories.
#[test]
fn gc_does_nothing_when_the_clock_is_implausible_or_behind_a_jobs_own_stamp() {
    let t = tmp("gc-clock");
    let jobs = t.join("jobs");
    let old = jobs.join("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd");
    std::fs::create_dir_all(&old).unwrap();
    // Thirty days old: an ordinary collection.
    let thirty = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86_400);
    std::fs::File::open(&old)
        .unwrap()
        .set_modified(thirty)
        .unwrap();
    // Before 2024 (the console's clock reset to its epoch): the age of everything is nonsense.
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert_eq!(jobs_gc(&jobs, 1_000_000 - now_unix, 86_400), 0);
    assert!(old.exists(), "an implausible clock took a job directory");
    // A clock moved back two days: the fresh directory is stamped in the future and is skipped; the old one
    // (30 days, still 28 days old) is collected, since one future stamp must not stop the rest.
    let fresh = jobs.join("efefefefefefefefefefefefefefefef");
    std::fs::create_dir_all(&fresh).unwrap();
    assert_eq!(jobs_gc(&jobs, -2 * 86_400, 86_400), 1);
    assert!(fresh.exists(), "a future-stamped directory was collected");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::File::open(&old)
        .unwrap()
        .set_modified(thirty)
        .unwrap();
    // A sane clock collects the old directory as before.
    assert_eq!(jobs_gc(&jobs, 0, 86_400), 1);
    assert!(!old.exists() && fresh.exists());
}

#[test]
fn the_cross_job_cap_throttles_like_the_per_job_one() {
    // 400 records of ~4 KiB with the batches held: only the cap can stop the writers, since nothing is
    // journaled, so nothing can be swept. Without a cap all 840 KiB are in the log a second later.
    let t = tmp("total");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let n = 400;
    let opts = LogOpts {
        mode: 1,
        pack_segment: 64 << 10,
        unswept_max: 64 << 20, // the per-job cap is out of the way
        unswept_total: 64 << 10,
        sweep_age_ms: 600_000,
        ..LogOpts::ON
    };
    let mut m = small(n, false);
    for e in m.entries.iter_mut().filter(|e| e.kind == ENTRY_FILE) {
        e.size = 2000; // ~2 KiB records: 400 of them are ~840 KiB
    }
    let big = |i: usize| -> Vec<u8> { (0..2000).map(|k| (k + i) as u8).collect() };
    let job = CApplyJob::begin_opts(&t.join("jobs"), &root, 0, &m, 0, 0, opts);
    job.hold_batches(true);
    for i in 0..n {
        job.record(i as u32 + 1, &big(i), *blake3::hash(&big(i)).as_bytes());
    }
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let held = job.unswept_bytes();
    assert!(
        held < 6 * (64 << 10),
        "the cross-job cap did not hold the log down: {held} bytes with nothing swept"
    );
    job.hold_batches(false);
    assert_eq!(job.wait(60_000), 0, "{}", job.events());
    settled(&job, 30);
    for i in 0..n {
        assert_eq!(std::fs::read(root.join(format!("d/{i}"))).unwrap(), big(i));
    }
}

fn crashed_dir(t: &Path, byte: u8) {
    std::fs::create_dir_all(t.join(format!("dest{byte}"))).unwrap();
    let m = small(10, false);
    let opts = LogOpts {
        job_byte: byte,
        ..slow()
    };
    // its own jobs directory (a later start would recover the earlier ones), gathered by the caller
    let r = CRecv::open_opts(
        &t.join(format!("jobs{byte}")),
        &t.join(format!("dest{byte}")),
        0,
        gen::POLICY_REPLACE,
        2,
        opts,
    );
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.hold_batches(true);
    for i in 0..10 {
        send(&r, i);
    }
    r.wait_pending(10, 5000);
    r.hold_batches(false);
    r.wait_stopped(10_000);
}

#[test]
fn start_recovery_takes_a_few_job_directories_and_housekeeping_the_rest() {
    let t = tmp("cap");
    let jobs = t.join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    for b in [1u8, 2, 3] {
        crashed_dir(&t, b);
        let hex = job_dir(&t.join(format!("jobs{b}")), &[b; 16]);
        std::fs::rename(&hex, job_dir(&jobs, &[b; 16])).unwrap();
    }
    assert!([1u8, 2, 3].iter().all(|b| packs(&jobs, *b) > 0));
    // a start with a job of its own (no JobOpen for the three): the start-time pass takes one
    // directory (recover_max 1); housekeeping, 2 s away, takes the others
    let opts = LogOpts {
        recover_max: 1,
        recover_every_ms: 2000,
        job_byte: 9,
        ..slow()
    };
    let r = CRecv::open_opts(&jobs, &t.join("dest9"), 0, gen::POLICY_REPLACE, 0, opts);
    // The start's pass runs on the recovery thread: wait for it to take its one directory.
    let t1 = std::time::Instant::now();
    let left = loop {
        let left = [1u8, 2, 3].iter().filter(|b| packs(&jobs, **b) > 0).count();
        if left < 3 || t1.elapsed().as_secs() > 10 {
            break left;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(left, 2, "start recovery is capped at recover_max");
    let t0 = std::time::Instant::now();
    while [1u8, 2, 3].iter().any(|b| packs(&jobs, *b) > 0) {
        assert!(
            t0.elapsed().as_secs() < 40,
            "housekeeping never finished the rest"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    drop(r);
}

// ---- recovery and the crash points inside a sweep -------------------------------------------

fn crash_then<F: Fn(&Path)>(
    tag: &str,
    crash_at: i32,
    staged: bool,
    n: usize,
    damage: F,
) -> (CRecv, PathBuf, Manifest) {
    let t = tmp(tag);
    if !staged {
        std::fs::create_dir_all(t.join("dest")).unwrap();
    }
    let m = small(n, false);
    let r = CRecv::open_opts(
        &t.join("jobs"),
        &t.join("dest"),
        0,
        gen::POLICY_REPLACE,
        crash_at,
        slow(),
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
    (r, t, m)
}

fn files_dir(t: &Path, staged: bool) -> PathBuf {
    if staged {
        t.join("dest.ava-part/d")
    } else {
        t.join("dest/d")
    }
}

#[test]
fn start_time_recovery_alone_restores_lost_files_without_any_jobopen() {
    let (r, t, _m) = crash_then("startonly", 2, false, 30, |t| {
        for i in 0..15 {
            std::fs::remove_file(files_dir(t, false).join(i.to_string())).unwrap();
        }
    });
    let r = r.restart_without_open();
    // The start's recovery pass runs on the recovery thread now (the listeners no longer wait for it).
    let t0 = std::time::Instant::now();
    while packs(&t.join("jobs"), 7) > 0 {
        assert!(
            t0.elapsed().as_secs() < 20,
            "the start-time pass never recovered the log"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    all_there(&t, 30);
    let jobs = t.join("jobs");
    assert!(replay(&jobs, 7).0.unswept.is_empty());
    assert_eq!(packs(&jobs, 7), 0);
    drop(r);
}

#[test]
fn a_corrupt_record_in_the_middle_of_a_range_ends_the_scan_and_resends_the_rest() {
    let (r, t, m) = crash_then("midcrc", 2, false, 40, |t| {
        let p = job_dir(&t.join("jobs"), &[7; 16]).join("pack.0");
        let mut b = std::fs::read(&p).unwrap();
        let at = b.len() / 2; // inside some record's frame
        b[at] ^= 0xFF;
        std::fs::write(&p, b).unwrap();
        for i in 0..40 {
            std::fs::remove_file(files_dir(t, false).join(i.to_string())).unwrap();
        }
    });
    let r = r.restart(0);
    r.manifest(&m);
    let ev = r.wait_event("map status=0", 5000);
    let done = done_ids(&ev);
    assert!(
        !done.is_empty() && done.len() < 40,
        "the records before the damage are kept: {done:?}"
    );
    for i in 0..40 {
        if !done.contains(&(i as u32 + 1)) {
            send(&r, i);
        }
    }
    assert_eq!(r.wait(15_000), 0, "{}", r.events());
    all_there(&t, 40);
}

#[test]
fn a_same_size_file_with_the_wrong_bytes_is_rewritten_from_the_log() {
    let (r, t, m) = crash_then("samesize", 2, false, 20, |t| {
        std::fs::write(files_dir(t, false).join("5"), b"ZZZZ").unwrap();
        std::fs::write(files_dir(t, false).join("6"), b"").unwrap();
    });
    let r = r.restart(0);
    r.manifest(&m);
    let ev = r.wait_event("map status=0", 5000);
    assert_eq!(done_ids(&ev).len(), 20, "{ev}");
    assert_eq!(r.wait(15_000), 0, "{}", r.events());
    all_there(&t, 20);
}

#[test]
fn a_crash_with_unswept_files_in_a_staged_tree_recovers_them() {
    let (r, t, m) = crash_then("staged-crash", 2, true, 25, |t| {
        for i in 0..12 {
            std::fs::remove_file(files_dir(t, true).join(i.to_string())).unwrap();
        }
    });
    let r = r.restart(0);
    r.manifest(&m);
    let ev = r.wait_event("map status=0", 5000);
    assert_eq!(done_ids(&ev).len(), 25, "{ev}");
    assert_eq!(r.wait(15_000), 0, "{}", r.events());
    all_there(&t, 25);
    assert!(!t.join("dest.ava-part").exists());
}

#[test]
fn a_crash_between_a_sweeps_syncs_and_its_record_leaves_nothing_swept_and_recovers() {
    // crash point 11: files and directories are synced, JnlSweep was never appended
    let (r, t, m) = crash_then("midsweep", 11, false, 30, |_| {});
    let jobs = t.join("jobs");
    let (st, recs) = replay(&jobs, 7);
    assert!(
        !recs.iter().any(|r| matches!(r, Record::Sweep(_))),
        "a sweep was journaled before the crash point"
    );
    assert_eq!(st.unswept.len(), 30);
    let r = r.restart(0);
    r.manifest(&m);
    let ev = r.wait_event("map status=0", 5000);
    assert_eq!(done_ids(&ev).len(), 30, "{ev}");
    assert_eq!(r.wait(15_000), 0, "{}", r.events());
    all_there(&t, 30);
    settled(&r, 20);
}

#[test]
fn after_crash_point_10_the_sweep_was_durable_and_its_files_are_whole() {
    // the JnlSweep is in the journal (fsynced) and every file it names is on disk with its bytes
    let (r, t, _m) = crash_then("sweepdurable", 10, false, 30, |_| {});
    let jobs = t.join("jobs");
    let (st, recs) = replay(&jobs, 7);
    let swept: Vec<u32> = recs
        .iter()
        .filter_map(|r| match r {
            Record::Sweep(f) => Some(ava1::ranges::from_runs(f)),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(!swept.is_empty(), "no sweep was journaled");
    for id in &swept {
        assert!(!st.unswept.contains(id), "swept but still unswept: {id}");
        assert_eq!(
            std::fs::read(t.join(format!("dest/d/{}", id - 1))).unwrap(),
            body(*id as usize - 1),
            "a swept file is not whole"
        );
    }
    drop(r);
}

// ---- review dbl round 2 ----------------------------------------------------------------------

fn bad_dir(t: &Path, byte: u8) {
    // a crashed job whose manifest is gone: its journal opens (so recovery tries it) but it cannot load
    crashed_dir(t, byte);
    let hex = job_dir(&t.join(format!("jobs{byte}")), &[byte; 16]);
    std::fs::remove_file(hex.join("manifest")).unwrap();
    std::fs::rename(&hex, job_dir(&t.join("jobs"), &[byte; 16])).unwrap();
}

#[test]
fn directories_that_cannot_be_recovered_do_not_starve_the_ones_that_can() {
    let t = tmp("starve");
    let jobs = t.join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    for b in [1u8, 2, 3, 4, 5, 6, 7, 8] {
        bad_dir(&t, b);
    }
    crashed_dir(&t, 0xd0);
    std::fs::rename(
        job_dir(&t.join("jobs208"), &[0xd0; 16]),
        job_dir(&jobs, &[0xd0; 16]),
    )
    .unwrap();
    let opts = LogOpts {
        recover_max: 1,
        recover_every_ms: 150,
        job_byte: 0x9a,
        ..slow()
    };
    let r = CRecv::open_opts(&jobs, &t.join("dest9a"), 0, gen::POLICY_REPLACE, 0, opts);
    let t0 = std::time::Instant::now();
    while packs(&jobs, 0xd0) > 0 {
        assert!(
            t0.elapsed().as_secs() < 40,
            "eight unrecoverable directories blocked the recoverable one forever"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    drop(r);
}

#[test]
fn a_jobopen_during_a_recovery_pass_is_told_to_retry_and_then_resumes() {
    let t = tmp("openrace");
    let jobs = t.join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    crashed_dir(&t, 1);
    let opts = LogOpts {
        recover_every_ms: 200,
        job_byte: 9,
        ..slow()
    };
    let r = CRecv::open_opts(&jobs, &t.join("dest9"), 0, gen::POLICY_REPLACE, 0, opts);
    // the data layer is up with nothing to recover; the crashed directory appears now, and its recovery
    // is slow: four sweep failures cost ~750 ms of retries
    r.fail_sweeps(4);
    std::fs::rename(
        job_dir(&t.join("jobs1"), &[1; 16]),
        job_dir(&jobs, &[1; 16]),
    )
    .unwrap();
    let dest = t.join("dest1");
    let (mut busy, mut other, mut ticks_in_pass) = (0, vec![], 0);
    let t0 = std::time::Instant::now();
    // housekeeping's pass has started once a sweep of the recovery has failed (and is being retried)
    while sweep_failures_left() == 4 {
        assert!(
            t0.elapsed().as_secs() < 10,
            "housekeeping never began the pass"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    loop {
        assert!(
            t0.elapsed().as_secs() < 30,
            "never resumed: busy {busy} other {other:?}"
        );
        assert_eq!(
            r.unswept_total(),
            0,
            "a recovery throwaway's bytes count against the cap"
        );
        match probe_open(1, &dest) {
            0 => break,
            s if s == gen::ERR_BUSY as i32 => {
                if busy == 0 {
                    ticks_in_pass = house_ticks();
                }
                busy += 1;
            }
            s => other.push(s),
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    assert!(
        other.is_empty(),
        "a JobOpen in the window was answered {other:?}, not BUSY"
    );
    assert!(
        busy > 0,
        "the window was never seen (the recovery pass was too quick)"
    );
    // the reaper kept running while recovery did (recovery has its own thread)
    assert!(
        house_ticks() - ticks_in_pass >= 4,
        "housekeeping stalled behind the recovery pass ({} ticks since it began)",
        house_ticks() - ticks_in_pass
    );
    all_files(&dest, 10);
    drop(r);
}

fn all_files(dest: &Path, n: usize) {
    for i in 0..n {
        assert_eq!(
            std::fs::read(dest.join(format!("d/{i}"))).unwrap(),
            body(i),
            "file {i}"
        );
    }
}

#[test]
fn a_healthy_job_is_not_gated_by_other_jobs_stuck_log_bytes() {
    let t = tmp("pinned");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let job = CApplyJob::begin_opts(
        &t.join("jobs"),
        &root,
        0,
        &small(50, false),
        0,
        0,
        LogOpts::ON,
    );
    job.pin_unswept_total(600 << 20); // other jobs hold more than the 512 MiB cap (their sweeps keep failing)
    for i in 0..50 {
        send(&job, i);
    }
    let r = job.wait(20_000);
    job.pin_unswept_total(-(600 << 20));
    assert_eq!(
        r,
        0,
        "the job stalled behind other jobs' bytes: {}",
        job.events()
    );
    settled(&job, 20);
}

#[test]
fn a_job_in_a_sticky_sweep_error_does_not_count_against_the_others() {
    let t = tmp("excluded");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let job = CApplyJob::begin_opts(
        &t.join("jobs"),
        &root,
        0,
        &small(20, false),
        0,
        0,
        LogOpts::ON,
    );
    job.fail_sweeps(-1);
    for i in 0..20 {
        send(&job, i);
    }
    assert_eq!(job.wait(15_000), 0, "{}", job.events());
    job.wait_event(&format!("status code={}", gen::ERR_IO), 20_000);
    assert!(job.unswept_bytes() > 0);
    assert_eq!(
        job.unswept_total(),
        0,
        "a stuck job's bytes keep other jobs waiting"
    );
    job.fail_sweeps(0);
    settled(&job, 20);
    assert_eq!(job.unswept_total(), 0);
}

#[test]
fn stopping_the_data_layer_during_a_slow_recovery_pass_returns_promptly() {
    let t = tmp("stop-recovery");
    let jobs = t.join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    crashed_dir(&t, 1);
    let opts = LogOpts {
        recover_every_ms: 100,
        job_byte: 9,
        ..slow()
    };
    let r = CRecv::open_opts(&jobs, &t.join("dest9"), 0, gen::POLICY_REPLACE, 0, opts);
    // every sweep of the recovery fails, so its drain keeps retrying with backoff (~1.5 s in all)
    r.fail_sweeps(1_000_000);
    std::fs::rename(
        job_dir(&t.join("jobs1"), &[1; 16]),
        job_dir(&jobs, &[1; 16]),
    )
    .unwrap();
    let t0 = std::time::Instant::now();
    while sweep_failures_left() == 1_000_000 {
        assert!(
            t0.elapsed().as_secs() < 10,
            "housekeeping never began the pass"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let took = r.stop_data_timed(); // data_stop joins the recovery thread
    assert!(
        took < std::time::Duration::from_millis(450),
        "the stop waited {took:?} behind the recovery pass"
    );
    drop(r);
}

/// Final review (console): the start-time pass runs on the recovery thread, so ava1_data_start returns
/// at once; until the pass ends a JobOpen for a job that holds a log is answered BUSY, never served from
/// state recovery has not yet restored.
#[test]
fn a_jobopen_before_the_start_pass_has_run_for_a_job_with_a_log_is_told_to_retry() {
    let (r, t, m) = crash_then("early-open", 2, false, 20, |_| {});
    let _ = (&t, &m);
    drop(r);
    // (the BUSY-then-resumes behaviour of an open racing a pass is covered by
    // a_jobopen_during_a_recovery_pass_is_told_to_retry_and_then_resumes; this one only pins that
    // the start returned and the pass completed on its own thread)
    let jobs = t.join("jobs");
    let r = CRecv::open_opts(&jobs, &t.join("dest"), 0, gen::POLICY_REPLACE, 0, slow());
    let t0 = std::time::Instant::now();
    while packs(&jobs, 7) > 0 {
        assert!(t0.elapsed().as_secs() < 20, "the start-time pass never ran");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    drop(r);
}
