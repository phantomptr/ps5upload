//! perf-apply (review 003 §2.1, §3.3, §6): the receive/apply path's latency fixes, each proved
//! through the C engine's test hooks rather than by timing.
#![cfg(unix)]

use ava1::gen::{self, ENTRY_DIR, ENTRY_FILE};
use ava1::journal::{job_dir, Journal, Record};
use ava1::manifest::{Entry, Manifest};
use ava1::verify::GROUP;
use ava1_ctest::*;

fn tmp(tag: &str) -> TempDir {
    TempDir::new(format!("ava1-perf-{tag}-{}", std::process::id()))
}

fn file(path: &str, size: u64) -> Entry {
    Entry {
        kind: ENTRY_FILE,
        mode: 0o640,
        size,
        mtime: 1_600_000_000,
        path: path.into(),
        root: None,
    }
}

fn dir(path: &str) -> Entry {
    Entry {
        kind: ENTRY_DIR,
        mode: 0o755,
        size: 0,
        mtime: 0,
        path: path.into(),
        root: None,
    }
}

fn data(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(13).wrapping_add(seed))
        .collect()
}

fn send_large(job: &CApplyJob, id: u32, d: &[u8]) {
    let g = GROUP as usize;
    for o in (0..d.len()).step_by(g).rev() {
        job.chunk(id, o as u64, &d[o..(o + g).min(d.len())]);
    }
    job.root(id, *blake3::hash(d).as_bytes());
}

#[test]
fn preallocation_happens_outside_the_job_mutex() {
    // Review 003 §2.1: a 4 GiB preallocation under j->mu stalled the feeder and every other
    // worker for minutes on a slow drive. The hook fires right before each preallocation and
    // the shim checks, from that very thread, whether j->mu is held.
    let t = tmp("prealloc");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let a = data(4 * GROUP as usize + 3, 1);
    let b = data(3 * GROUP as usize + 1, 2);
    let m = Manifest {
        entries: vec![file("a.bin", a.len() as u64), file("b.bin", b.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    send_large(&job, 0, &a);
    send_large(&job, 1, &b);
    assert_eq!(job.wait(10_000), 0, "{}", job.events());
    assert_eq!(std::fs::read(root.join("a.bin")).unwrap(), a);
    assert_eq!(std::fs::read(root.join("b.bin")).unwrap(), b);
    let p = job.probe();
    assert_eq!(p.prealloc_calls, 2, "once per file, however many chunks");
    assert_eq!(
        p.prealloc_with_job_mutex_held, 0,
        "preallocated under j->mu"
    );
    drop(dir("unused"));
}

#[test]
fn commits_run_on_the_workers_not_the_job_thread() {
    // Review 003 §3.3 item 2: commit_large (four fsyncs) ran inline on the job thread, so a
    // batch's commits stopped every other batch. They now run on the worker pool.
    let t = tmp("commits");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let n = 8usize;
    let files: Vec<Vec<u8>> = (0..n)
        .map(|i| data(2 * GROUP as usize + 5 + i, i as u8))
        .collect();
    let m = Manifest {
        entries: files
            .iter()
            .enumerate()
            .map(|(i, d)| file(&format!("f{i}.bin"), d.len() as u64))
            .collect(),
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    for (i, d) in files.iter().enumerate() {
        send_large(&job, i as u32, d);
    }
    assert_eq!(job.wait(15_000), 0, "{}", job.events());
    for (i, d) in files.iter().enumerate() {
        assert_eq!(&std::fs::read(root.join(format!("f{i}.bin"))).unwrap(), d);
    }
    let p = job.probe();
    assert_eq!(p.commits, n as u64);
    assert_eq!(p.commits_on_job_thread, 0, "a commit ran on the job thread");
}

/// The ids a map event line reports as done ("map status=0 last=1 done=0+2,5+1, partial=N").
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

/// Several large files commit on the workers at once and the process dies at `crash_at`; the
/// resumed job must finish every file with the right bytes and lose none, whichever commit was
/// cut and wherever (renamed, not journaled; or not started).
fn worker_commit_crash(tag: &str, crash_at: i32) {
    let t = tmp(tag);
    std::fs::create_dir_all(t.join("dest")).unwrap(); // merge mode: the commit really renames
    let n = 6usize;
    let files: Vec<Vec<u8>> = (0..n)
        .map(|i| data(2 * GROUP as usize + 11 + i, 40 + i as u8))
        .collect();
    let m = Manifest {
        entries: files
            .iter()
            .enumerate()
            .map(|(i, d)| file(&format!("big{i}"), d.len() as u64))
            .collect(),
    };
    let r = CRecv::open(
        &t.join("jobs"),
        &t.join("dest"),
        0,
        gen::POLICY_REPLACE,
        crash_at,
    );
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    for (i, d) in files.iter().enumerate() {
        send_large(&r, i as u32, d);
    }
    r.wait_stopped(10_000); // the first commit to reach the crash point stopped the job
    let r = r.restart(0);
    r.manifest(&m);
    let ev = r.wait_event("map status=0", 5000);
    let done = done_ids(&ev);
    for (i, d) in files.iter().enumerate() {
        if !done.contains(&(i as u32)) {
            // what the sender does for a file the map does not list as done: send it again
            send_large(&r, i as u32, d);
        }
    }
    assert_eq!(r.wait(15_000), 0, "{}", r.events());
    for (i, d) in files.iter().enumerate() {
        assert_eq!(
            &std::fs::read(t.join(format!("dest/big{i}"))).unwrap(),
            d,
            "file {i} after the crash at {crash_at}"
        );
        assert!(!t.join(format!("dest/big{i}.ava-part")).exists());
    }
}

#[test]
fn a_worker_commit_cut_between_rename_and_journal_loses_no_file() {
    worker_commit_crash("wcommit-renamed", 4); // AVA1_CRASH_COMMIT_RENAMED
}

#[test]
fn a_worker_commit_cut_before_it_starts_loses_no_file() {
    worker_commit_crash("wcommit-before", 5); // AVA1_CRASH_BEFORE_COMMIT
}

fn small_dirs(ndirs: usize, per: usize) -> Manifest {
    let mut entries = vec![];
    for d in 0..ndirs {
        entries.push(dir(&format!("d{d:02}")));
        for i in 0..per {
            entries.push(file(&format!("d{d:02}/f{i}"), 4));
        }
    }
    Manifest { entries }
}

fn small_ids(ndirs: usize, per: usize) -> Vec<u32> {
    (0..ndirs * (per + 1))
        .filter(|i| i % (per + 1) != 0)
        .map(|i| i as u32)
        .collect()
}

fn body(id: u32) -> Vec<u8> {
    format!("{id:04}").into_bytes()
}

#[test]
fn a_batchs_directory_syncs_run_on_the_workers_not_the_job_thread() {
    // Review 003 §3.3 item 1: sync_new_dirs fsynced every directory the batch touched serially
    // on the job thread. They are striped over the workers like the data fsyncs.
    let t = tmp("dirstripe");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let (nd, per) = (16usize, 2usize);
    let m = small_dirs(nd, per);
    let job = CApplyJob::begin_opts(&t.join("jobs"), &root, 0, &m, 0, 0, LogOpts::OFF); // the per-file path
    job.hook_sleep(20);
    job.hold_batches(true);
    let ids = small_ids(nd, per);
    for &id in &ids {
        job.record(id, &body(id), *blake3::hash(&body(id)).as_bytes());
    }
    job.wait_pending(ids.len() as u32, 5000);
    job.hold_batches(false);
    assert_eq!(job.wait(15_000), 0, "{}", job.events());
    let p = job.probe();
    assert_eq!(p.batch_dir_syncs, nd as u64, "each directory once");
    assert_eq!(
        p.batch_dir_syncs_on_workers, p.batch_dir_syncs,
        "ran on the job thread"
    );
    assert!(
        p.batch_dir_sync_threads >= 2,
        "all {} directory syncs ran on one thread",
        p.batch_dir_syncs
    );
}

#[test]
fn prepares_directory_syncs_run_on_the_workers() {
    // Review 003 §4 item 1: prepare fsynced each unique parent serially before the map was sent.
    let t = tmp("prepstripe");
    let mut entries = vec![];
    for d in 0..24 {
        entries.push(dir(&format!("p{d:02}")));
        entries.push(dir(&format!("p{d:02}/c")));
    }
    let m = Manifest { entries };
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.hook_sleep(20);
    r.manifest(&m);
    r.wait_event("map status=0", 15_000);
    let (calls, on_workers, threads) = r.probe_prepare_dirs();
    assert!(calls >= 24, "{calls} directory syncs in prepare");
    assert_eq!(
        on_workers, calls,
        "prepare synced directories on its own thread"
    );
    assert!(threads >= 2, "one thread did every directory sync");
}

/// A crash with directories not (all) synced must journal and acknowledge nothing: the resumed
/// job resends every file. (Crash 8: after the data fsync, before any directory. Crash 9: after
/// the first directory, before the rest.)
fn dir_crash(tag: &str, crash_at: i32) {
    let t = tmp(tag);
    let (nd, per) = (6usize, 3usize);
    let m = small_dirs(nd, per);
    let ids = small_ids(nd, per);
    let r = CRecv::open_opts(
        &t.join("jobs"),
        &t.join("dest"),
        0,
        gen::POLICY_REPLACE,
        crash_at,
        LogOpts::OFF, // the per-file path: its crash points sit in the directory syncs
    );
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.hold_batches(true);
    for &id in &ids {
        r.record(id, &body(id), *blake3::hash(&body(id)).as_bytes());
    }
    r.wait_pending(ids.len() as u32, 5000);
    r.hold_batches(false);
    r.wait_stopped(10_000);
    let r = r.restart(0);
    r.manifest(&m);
    let ev = r.wait_event("map status=0", 5000);
    assert!(
        done_ids(&ev).is_empty(),
        "crash {crash_at}: the journal named files whose directories were not synced: {ev}"
    );
    for &id in &ids {
        r.record(id, &body(id), *blake3::hash(&body(id)).as_bytes());
    }
    assert_eq!(r.wait(15_000), 0, "{}", r.events());
    for d in 0..nd {
        for i in 0..per {
            let id = (d * (per + 1) + 1 + i) as u32;
            assert_eq!(
                std::fs::read(t.join(format!("dest/d{d:02}/f{i}"))).unwrap(),
                body(id)
            );
        }
    }
}

#[test]
fn a_crash_after_the_data_sync_before_the_directories_journals_nothing() {
    dir_crash("crash-data", 8);
}

#[test]
fn a_crash_between_a_batchs_directory_syncs_journals_nothing() {
    dir_crash("crash-middirs", 9);
}

fn send_range(job: &CApplyJob, id: u32, d: &[u8]) {
    let g = GROUP as usize;
    for o in (0..d.len()).step_by(g) {
        job.chunk(id, o as u64, &d[o..(o + g).min(d.len())]);
    }
    job.root(id, *blake3::hash(d).as_bytes());
}

#[test]
fn a_drive_whose_batch_fsync_outlasts_the_window_switches_to_per_chunk_fsync() {
    // Review 003 §6: when the data fsync of a batch takes longer than the credit window holds
    // data at the current rate, the sender stalls on credit while the drive idles between
    // fsyncs. The job then fsyncs each chunk right after writing it. The test drive is slow by
    // fsync_delay_us (every batch fsync takes 3 s); 12 MiB arrive within a quarter second.
    let t = tmp("slowdrive");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let a = data(12 * GROUP as usize, 3);
    let b = data(5 * GROUP as usize + 9, 4);
    let m = Manifest {
        entries: vec![file("a.bin", a.len() as u64), file("b.bin", b.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 3_000_000);
    assert!(!job.probe().per_chunk_fsync);
    send_range(&job, 0, &a);
    let t0 = std::time::Instant::now();
    while !job.probe().per_chunk_fsync {
        assert!(
            t0.elapsed().as_secs() < 20,
            "never switched: {}",
            job.events()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // the second file is written in per-chunk mode and must still arrive intact
    send_range(&job, 1, &b);
    assert_eq!(job.wait(30_000), 0, "{}", job.events());
    assert_eq!(std::fs::read(root.join("a.bin")).unwrap(), a);
    assert_eq!(std::fs::read(root.join("b.bin")).unwrap(), b);
}

#[test]
fn a_fast_drive_keeps_the_batch_fsync() {
    let t = tmp("fastdrive");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let a = data(12 * GROUP as usize, 5);
    let m = Manifest {
        entries: vec![file("a.bin", a.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    send_range(&job, 0, &a);
    assert_eq!(job.wait(15_000), 0, "{}", job.events());
    assert!(
        !job.probe().per_chunk_fsync,
        "switched on a drive that keeps up"
    );
    assert_eq!(std::fs::read(root.join("a.bin")).unwrap(), a);
}

// ---- fix round (review perf-apply) ----

fn six_large(tag: &str) -> (TempDir, Vec<Vec<u8>>, Manifest) {
    let t = tmp(tag);
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let files: Vec<Vec<u8>> = (0..6)
        .map(|i| data(2 * GROUP as usize + 3 + i, 70 + i as u8))
        .collect();
    let m = Manifest {
        entries: files
            .iter()
            .enumerate()
            .map(|(i, d)| file(&format!("f{i}.bin"), d.len() as u64))
            .collect(),
    };
    (t, files, m)
}

fn wait_commits(job: &CApplyJob, n: u64) {
    let t0 = std::time::Instant::now();
    while job.probe().commits < n.min(4) {
        assert!(t0.elapsed().as_secs() < 10, "commits never started");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn a_failing_commit_leaves_no_journal_record_or_durable_after_the_jobs_end() {
    let (t, files, m) = six_large("failcommit");
    // file 0's final path is a folder: its rename fails with ERR_EXISTS while five siblings are
    // in flight
    std::fs::create_dir_all(t.join("dest/f0.bin")).unwrap();
    std::fs::write(t.join("dest/f0.bin/keep"), b"k").unwrap();
    let job = CApplyJob::begin(&t.join("jobs"), &t.join("dest"), 0, &m, 0);
    job.hold_commits(true);
    for (i, d) in files.iter().enumerate() {
        send_large(&job, i as u32, d);
    }
    wait_commits(&job, 6);
    job.hold_commits(false);
    assert_eq!(
        job.wait(15_000),
        ava1::gen::ERR_EXISTS as i32,
        "{}",
        job.events()
    );
    let ev = job.events();
    let done_at = ev.find("\ndone ").or_else(|| ev.find("done ")).unwrap();
    assert!(
        !ev[done_at..].contains("durable"),
        "a Durable after JobDone: {ev}"
    );
    drop(job);
    let recs = Journal::open(&job_dir(&t.join("jobs"), &[7; 16]))
        .unwrap()
        .1;
    let first_done = recs
        .iter()
        .position(|r| matches!(r, Record::Done(_)))
        .expect("Done journaled");
    assert_eq!(first_done, recs.len() - 1, "a record after Done: {recs:?}");
    // whatever committed is byte-exact; the blocked one is untouched
    for (i, d) in files.iter().enumerate().skip(1) {
        let p = t.join(format!("dest/f{i}.bin"));
        if p.exists() {
            assert_eq!(&std::fs::read(p).unwrap(), d, "file {i}");
        }
    }
    assert!(t.join("dest/f0.bin/keep").exists());
}

#[test]
fn compaction_is_skipped_while_a_commit_is_in_flight_and_happens_once_it_drains() {
    let t = tmp("compact-commit");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let d = data(2 * GROUP as usize + 1, 9);
    let m = Manifest {
        entries: vec![file("a.bin", d.len() as u64), file("never.bin", 10)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &t.join("dest"), 0, &m, 0);
    job.hold_commits(true);
    send_large(&job, 0, &d);
    wait_commits(&job, 1);
    assert!(job.commits_inflight() >= 1);
    assert_eq!(job.compact_now(), -1, "compacted with a commit in flight");
    job.hold_commits(false);
    let t0 = std::time::Instant::now();
    while job.commits_inflight() != 0 {
        assert!(t0.elapsed().as_secs() < 10, "the commit never drained");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(job.compact_now(), 0, "not compacted once drained");
    assert_eq!(std::fs::read(t.join("dest/a.bin")).unwrap(), d);
}

#[test]
fn a_waiter_on_an_opening_file_gets_the_openers_enospc_and_does_not_hang() {
    let t = tmp("opening-enospc");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let g = GROUP as usize;
    let d = data(3 * g, 1);
    let m = Manifest {
        entries: vec![file("x.bin", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &t.join("dest"), 0, &m, 0);
    job.fault_prealloc(0); // the first open's preallocation reports a full drive (the hook pauses 40 ms)
    for o in (0..d.len()).step_by(g) {
        job.chunk(0, o as u64, &d[o..o + g]); // several workers want the same file at once
    }
    assert_eq!(
        job.wait(15_000),
        ava1::gen::ERR_NO_SPACE as i32,
        "{}",
        job.events()
    );
}

#[test]
fn a_durable_for_a_file_is_never_sent_before_its_own_commit_finished() {
    let (t, files, m) = six_large("durable-order");
    let job = CApplyJob::begin(&t.join("jobs"), &t.join("dest"), 0, &m, 0);
    job.trace(true);
    for (i, d) in files.iter().enumerate() {
        send_large(&job, i as u32, d);
    }
    assert_eq!(job.wait(15_000), 0, "{}", job.events());
    let ev = job.events();
    let lines: Vec<&str> = ev.lines().collect();
    for i in 0..files.len() {
        let journaled = lines
            .iter()
            .position(|l| *l == format!("hook 5 {i}")) // outboard unlinked: after the journal record
            .unwrap();
        let durable = lines
            .iter()
            .position(|l| l.starts_with(&format!("durable files={i}+1")))
            .unwrap_or_else(|| panic!("no durable for {i}: {ev}"));
        assert!(
            journaled < durable,
            "Durable for {i} before its commit: {ev}"
        );
    }
}
