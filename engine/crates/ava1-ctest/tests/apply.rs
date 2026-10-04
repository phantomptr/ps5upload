#![cfg(unix)]
use std::path::{Path, PathBuf};

use ava1::gen::{ENTRY_DIR, ENTRY_FILE};
use ava1::manifest::{Entry, Manifest};
use ava1::verify::GROUP;
use ava1_ctest::*;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ava1-apply-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
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

fn send_large(job: &CApplyJob, id: u32, d: &[u8], reverse: bool) {
    let g = GROUP as usize;
    let mut offs: Vec<usize> = (0..d.len()).step_by(g).collect();
    if reverse {
        offs.reverse();
    }
    for o in offs {
        job.chunk(id, o as u64, &d[o..(o + g).min(d.len())]);
    }
    job.root(id, *blake3::hash(d).as_bytes());
}

#[test]
fn a_large_file_assembles_out_of_order_and_commits() {
    let t = tmp("large");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap(); // existing root: merge mode
    let d = data(5 * GROUP as usize + 17, 1);
    let m = Manifest {
        entries: vec![dir("sub"), file("sub/big.bin", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    send_large(&job, 1, &d, true);
    assert_eq!(job.wait(10_000), 0);
    let out = root.join("sub/big.bin");
    assert_eq!(std::fs::read(&out).unwrap(), d);
    assert!(!root.join("sub/big.bin.ava-part").exists());
    let md = std::fs::metadata(&out).unwrap();
    assert_eq!(std::os::unix::fs::MetadataExt::mtime(&md), 1_600_000_000);
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&md.permissions()) & 0o777,
        0o640
    );
    let ev = job.events();
    assert!(ev.contains("durable files=1+1"), "{ev}");
    assert!(ev.ends_with("done 0\n"), "{ev}");
}

#[test]
fn a_root_that_arrives_after_every_range_is_durable_still_commits() {
    // The root travels on the control connection, the chunks on the lanes: it can land
    // after the batch that made the last range durable. The file must still commit.
    let t = tmp("late-root");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(3 * GROUP as usize + 5, 2);
    let m = Manifest {
        entries: vec![file("late.bin", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    for o in (0..d.len()).step_by(GROUP as usize) {
        job.chunk(0, o as u64, &d[o..(o + GROUP as usize).min(d.len())]);
    }
    job.wait_event(" ranges=1", 10_000); // all ranges journaled, no root yet
    assert!(!root.join("late.bin").exists() || std::fs::read(root.join("late.bin")).unwrap() != d);
    std::thread::sleep(std::time::Duration::from_millis(600)); // several idle batches
    job.root(0, *blake3::hash(&d).as_bytes());
    assert_eq!(job.wait(10_000), 0);
    assert_eq!(std::fs::read(root.join("late.bin")).unwrap(), d);
    // a re-sent identical root changes nothing
    assert!(job.events().ends_with("done 0\n"));
}

#[test]
fn tiny_files_apply_in_parallel_and_become_durable_in_batches() {
    let t = tmp("tiny");
    let root = t.join("dest");
    let n = 2000;
    let mut entries = vec![dir("a")];
    for i in 0..n {
        entries.push(file(&format!("a/f{i:04}"), (i % 300) as u64));
    }
    let m = Manifest { entries };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    for i in 0..n {
        let d = data(i % 300, i as u8);
        job.record(i as u32 + 1, &d, *blake3::hash(&d).as_bytes());
    }
    assert_eq!(job.wait(30_000), 0);
    // staged: the tree appears at once, complete
    assert!(!t.join("dest.ava-part").exists());
    for i in (0..n).step_by(97) {
        assert_eq!(
            std::fs::read(root.join(format!("a/f{i:04}"))).unwrap(),
            data(i % 300, i as u8)
        );
    }
    let ev = job.events();
    assert!(
        ev.matches("durable").count() >= 2,
        "expected several batches: {ev}"
    );
}

#[test]
fn a_wrong_root_resets_the_file_and_asks_for_it_again() {
    let t = tmp("badroot");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(3 * GROUP as usize, 9);
    let m = Manifest {
        entries: vec![file("x", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    for o in (0..d.len()).step_by(GROUP as usize) {
        job.chunk(0, o as u64, &d[o..o + GROUP as usize]);
    }
    job.root(0, [0xee; 32]);
    job.wait_event("retry 0 1", 10_000);
    assert!(!root.join("x").exists());
    send_large(&job, 0, &d, false);
    assert_eq!(job.wait(10_000), 0);
    assert_eq!(std::fs::read(root.join("x")).unwrap(), d);
}

#[test]
fn a_bad_record_root_is_refused() {
    let t = tmp("badrec");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let m = Manifest {
        entries: vec![file("s", 3)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    job.record(0, b"abc", [0; 32]);
    job.wait_event("retry 0 1", 10_000);
    assert!(!root.join("s").exists());
}

#[test]
fn c_commit_refuses_cross_device_rename() {
    let t = tmp("xdev");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(2 * GROUP as usize + 1, 3);
    let m = Manifest {
        entries: vec![file("big", d.len() as u64)],
    };
    c_set_same_device(0); // every st_dev comparison reports "crosses"
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    send_large(&job, 0, &d, false);
    assert_eq!(job.wait(10_000), ava1::gen::ERR_CROSS_DEVICE as i32);
    c_set_same_device(1);
    assert!(root.join("big.ava-part").exists());
    assert!(!root.join("big").exists());
}

#[test]
fn c_commit_refuses_an_unknown_device_and_never_renames() {
    // review 007 #4 (HW-1): "could not tell" must fail closed like "another drive".
    let t = tmp("xdev-unknown");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(2 * GROUP as usize + 1, 3);
    let m = Manifest {
        entries: vec![file("big", d.len() as u64)],
    };
    c_set_same_device(-1); // the device query itself failed
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    send_large(&job, 0, &d, false);
    assert_eq!(job.wait(10_000), ava1::gen::ERR_IO as i32);
    c_set_same_device(1);
    assert!(root.join("big.ava-part").exists());
    assert!(!root.join("big").exists());
}

#[test]
fn c_commit_with_no_device_hook_refuses_and_never_renames() {
    // final review fs #1: a missing same_device hook is not "no guard needed".
    let t = tmp("xdev-nohook");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(2 * GROUP as usize + 1, 3);
    let m = Manifest {
        entries: vec![file("big", d.len() as u64)],
    };
    c_set_same_device(-2); // the shim installs no hook at all
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    send_large(&job, 0, &d, false);
    assert_eq!(job.wait(10_000), ava1::gen::ERR_IO as i32);
    c_set_same_device(1);
    assert!(root.join("big.ava-part").exists());
    assert!(!root.join("big").exists());
}

#[test]
fn a_staged_tree_is_not_moved_when_the_device_is_unknown_or_another() {
    for (v, code) in [(-1, ava1::gen::ERR_IO), (0, ava1::gen::ERR_CROSS_DEVICE)] {
        let t = tmp(&format!("tree-xdev{v}"));
        let root = t.join("dest");
        let m = Manifest {
            entries: vec![file("a", 1), file("b", 1)],
        };
        c_set_same_device(v);
        let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
        job.record(0, b"1", *blake3::hash(b"1").as_bytes());
        job.record(1, b"2", *blake3::hash(b"2").as_bytes());
        assert_eq!(job.wait(10_000), code as i32, "same_device={v}");
        c_set_same_device(1);
        assert!(!root.exists(), "the tree was renamed into place (v={v})");
        assert!(Path::new(&format!("{}.ava-part", root.display()))
            .join("a")
            .exists());
    }
}

#[test]
fn a_staged_tree_is_not_moved_over_a_root_that_appeared() {
    let t = tmp("exists");
    let root = t.join("dest");
    let m = Manifest {
        entries: vec![file("a", 1), file("b", 1)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    job.record(0, b"1", *blake3::hash(b"1").as_bytes());
    std::fs::create_dir_all(&root).unwrap(); // someone made it meanwhile
    job.record(1, b"2", *blake3::hash(b"2").as_bytes());
    assert_eq!(job.wait(10_000), ava1::gen::ERR_EXISTS as i32);
    assert!(Path::new(&format!("{}.ava-part", root.display()))
        .join("a")
        .exists());
}

#[test]
fn a_single_file_lands_through_its_part_file() {
    let t = tmp("single");
    let dest = t.join("one.pkg");
    let d = data(GROUP as usize * 2 + 5, 4);
    let m = Manifest {
        entries: vec![file("one.pkg", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &dest, ava1::gen::JF_SINGLE_FILE, &m, 0);
    send_large(&job, 0, &d, true);
    assert_eq!(job.wait(10_000), 0);
    assert_eq!(std::fs::read(&dest).unwrap(), d);
    assert!(!t.join("one.pkg.ava-part").exists());
}

// ---- fix round 1 ------------------------------------------------------------------

/// AVA1_E_PROTO, what the apply entry points answer for a frame that breaks the rules.
const E_PROTO: i32 = -11;

#[test]
fn a_stop_mid_batch_journals_and_acknowledges_nothing() {
    let t = tmp("stopbatch");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let n = 20;
    let m = Manifest {
        entries: (0..n).map(|i| file(&format!("f{i:02}"), 3)).collect(),
    };
    // every fsync takes 5 s: the first batch is still syncing when the job is stopped
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 5_000_000);
    for i in 0..n {
        job.record(i, b"abc", *blake3::hash(b"abc").as_bytes());
    }
    std::thread::sleep(std::time::Duration::from_millis(1000));
    let t0 = std::time::Instant::now();
    let ev = job.end();
    assert!(t0.elapsed().as_secs() < 5, "the stop waited out the fsyncs");
    assert!(!ev.contains("durable"), "{ev}");
    let (_, recs) =
        ava1::journal::Journal::open(&ava1::journal::job_dir(&t.join("jobs"), &[7; 16])).unwrap();
    assert!(
        !recs
            .iter()
            .any(|r| matches!(r, ava1::journal::Record::Batch(_))),
        "{recs:?}"
    );
}

#[test]
fn a_duplicate_chunk_racing_the_commit_does_not_fail_the_job() {
    let t = tmp("dupcommit");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let g = GROUP as usize;
    let (d0, d1) = (data(2 * g, 5), data(2 * g, 6));
    let m = Manifest {
        entries: vec![file("a", d0.len() as u64), file("b", d1.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    // a duplicate of file 0's first group lands after its root is verified, before it is
    // marked committed
    job.dup_on_commit(0, 0, &d0[..g]);
    send_large(&job, 0, &d0, false);
    job.chunk(1, 0, &d1[..g]);
    job.chunk(1, g as u64, &d1[g..]);
    job.wait_event("durable files=0+1", 10_000);
    std::thread::sleep(std::time::Duration::from_millis(400)); // a few more batches
    job.root(1, *blake3::hash(&d1).as_bytes());
    assert_eq!(job.wait(10_000), 0, "{}", job.events());
    assert_eq!(std::fs::read(root.join("a")).unwrap(), d0);
    assert_eq!(std::fs::read(root.join("b")).unwrap(), d1);
}

#[test]
fn a_bundle_record_naming_a_directory_is_refused() {
    let t = tmp("recdir");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let m = Manifest {
        entries: vec![dir("d"), file("f", 1)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    assert_eq!(
        job.try_record(0, b"x", *blake3::hash(b"x").as_bytes()),
        E_PROTO
    );
    assert_eq!(
        job.try_record(9, b"x", *blake3::hash(b"x").as_bytes()),
        E_PROTO
    );
    job.record(1, b"1", *blake3::hash(b"1").as_bytes());
    assert_eq!(job.wait(10_000), 0, "{}", job.events());
}

#[test]
fn a_truncated_bundle_is_refused() {
    let t = tmp("rectrunc");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let m = Manifest {
        entries: vec![file("f", 4)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    let mut rec = Vec::new();
    rec.extend_from_slice(&40u32.to_le_bytes()); // claims 40 bytes, carries 6
    rec.extend_from_slice(&[0, 0, 0, 0, 1, 2]);
    assert_eq!(job.raw_bundle(&rec, 1), E_PROTO);
    assert_eq!(job.raw_bundle(&[], 1), E_PROTO); // a count the records do not match
    job.record(0, b"abcd", *blake3::hash(b"abcd").as_bytes());
    assert_eq!(job.wait(10_000), 0, "{}", job.events());
}

fn lines_in_order(ev: &str, want: &[&str]) {
    let mut at = 0;
    for w in want {
        match ev[at..].find(w) {
            Some(i) => at += i + w.len(),
            None => panic!("{w:?} missing or out of order in {ev}"),
        }
    }
}

#[test]
fn a_rename_is_synced_in_its_directory_before_it_is_journaled() {
    // a large file in merge mode: verify, rename, sync the directory, journal, drop the outboard
    let t = tmp("dirsync");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(2 * GROUP as usize + 3, 8);
    let m = Manifest {
        entries: vec![file("big", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    job.trace(true);
    send_large(&job, 0, &d, false);
    assert_eq!(job.wait(10_000), 0);
    lines_in_order(
        &job.events(),
        &[
            "hook 1 0\n",
            "hook 2 0\n",
            "hook 3 0\n",
            "hook 4 0\n",
            "hook 5 0\n",
            "done 0\n",
        ],
    );
    drop(job);
    // a staged tree: the staging rename, its directory sync, then JobDone
    let root2 = t.join("staged");
    let m = Manifest {
        entries: vec![file("s", 1)],
    };
    let job = CApplyJob::begin(&t.join("jobs2"), &root2, 0, &m, 0);
    job.trace(true);
    job.record(0, b"s", *blake3::hash(b"s").as_bytes());
    assert_eq!(job.wait(10_000), 0);
    lines_in_order(
        &job.events(),
        &["hook 2 4294967295\n", "hook 3 4294967295\n", "done 0\n"],
    );
}

#[test]
fn a_final_rename_onto_a_directory_reports_exists() {
    let t = tmp("renexists");
    let root = t.join("dest");
    std::fs::create_dir_all(root.join("x")).unwrap();
    std::fs::write(root.join("x/keep"), b"k").unwrap();
    let d = data(2 * GROUP as usize + 1, 2);
    let m = Manifest {
        entries: vec![file("x", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    send_large(&job, 0, &d, false);
    assert_eq!(job.wait(10_000), ava1::gen::ERR_EXISTS as i32);
    assert!(root.join("x/keep").exists());
}

#[test]
fn chunks_outside_the_group_rules_are_refused() {
    let t = tmp("chunkrules");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let g = GROUP;
    let m = Manifest {
        entries: vec![dir("d"), file("f", 3 * g + 5)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    let wraps = u64::MAX - g + 1; // group-aligned; off + len wraps past zero
    assert_eq!(job.try_chunk(1, wraps, &[0; 16]), E_PROTO);
    assert_eq!(job.try_chunk(1, 0, &vec![0; (g / 2) as usize]), E_PROTO); // short mid-file
    assert_eq!(job.try_chunk(1, 4 * g, &[0; 1]), E_PROTO); // past the size
    assert_eq!(job.try_chunk(1, 3 * g, &[0; 6]), E_PROTO); // longer than the tail
    assert_eq!(job.try_chunk(1, 5, &[0; 1]), E_PROTO); // unaligned
    assert_eq!(job.try_chunk(0, 0, &[0; 1]), E_PROTO); // a directory
    assert_eq!(job.try_chunk(7, 0, &[0; 1]), E_PROTO); // no such file
    assert_eq!(job.try_chunk(1, 3 * g, &[0; 5]), 0); // the short final chunk is fine
}

#[test]
fn new_small_files_have_their_directories_synced_before_the_journal() {
    // two directories gain new entries in one batch: data fsync, then each directory once,
    // then the journal
    let t = tmp("newdirsync");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let m = Manifest {
        entries: vec![
            dir("a"),
            file("a/1", 1),
            file("a/2", 1),
            dir("b"),
            file("b/1", 1),
            file("b/2", 1),
        ],
    };
    let job = CApplyJob::begin_opts(&t.join("jobs"), &root, 0, &m, 0, 0, LogOpts::OFF);
    job.trace(true);
    job.hold_batches(true);
    for id in [1, 2, 4, 5] {
        job.record(id, b"x", *blake3::hash(b"x").as_bytes());
    }
    job.wait_pending(4, 5000);
    job.hold_batches(false);
    assert_eq!(job.wait(10_000), 0, "{}", job.events());
    let ev = job.events();
    lines_in_order(
        &ev,
        &["hook 6 ", "hook 7 ", "hook 7 ", "hook 8 ", "durable"],
    );
    assert_eq!(ev.matches("hook 7 ").count(), 2, "{ev}");
    let dirs: Vec<&str> = ev
        .lines()
        .filter_map(|l| l.strip_prefix("hook 7 "))
        .collect();
    assert_ne!(dirs[0], dirs[1], "{ev}");
}

#[test]
fn a_worker_failure_ends_the_job_once_after_its_durables() {
    let t = tmp("workerfail");
    let root = t.join("dest");
    std::fs::create_dir_all(root.join("f1")).unwrap(); // file 1's path is a folder: EISDIR
    let m = Manifest {
        entries: vec![file("f0", 1), file("f1", 1), file("f2", 1)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    job.record(0, b"a", *blake3::hash(b"a").as_bytes());
    job.wait_event("durable", 5000);
    job.record(1, b"b", *blake3::hash(b"b").as_bytes());
    job.record(2, b"c", *blake3::hash(b"c").as_bytes());
    assert_eq!(job.wait(10_000), ava1::gen::ERR_IO as i32);
    std::thread::sleep(std::time::Duration::from_millis(600)); // any late message would show
    let ev = job.events();
    assert_eq!(ev.matches("done ").count(), 1, "{ev}");
    let done_at = ev.find("done ").unwrap();
    assert!(
        ev.rfind("durable").is_none_or(|d| d < done_at),
        "a Durable after JobDone: {ev}"
    );
}

#[test]
fn a_parent_sync_failure_after_the_staging_rename_still_reports_the_tree() {
    let t = tmp("syncfault");
    let root = t.join("staged");
    let m = Manifest {
        entries: vec![file("s", 1)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    job.fail_dir_sync(u32::MAX); // the staging rename's parent sync fails with EIO
    job.record(0, b"s", *blake3::hash(b"s").as_bytes());
    assert_eq!(job.wait(10_000), 0, "{}", job.events());
    assert_eq!(std::fs::read(root.join("s")).unwrap(), b"s");
    assert!(job.events().contains("msg "), "{}", job.events());
}

// ---- G4: a transient fsync error is retried, a real one is not (SPEC.md §12.6) -----------

/// Sony's kernel reports ENOENT as 0x80020002 on some fsync failures of a USB drive.
const SONY_TRANSIENT: i32 = 0x8002_0002u32 as i32;

#[test]
fn a_transient_data_fsync_error_is_retried_and_the_job_succeeds() {
    let t = tmp("fsyncretry");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let m = Manifest {
        entries: vec![file("a", 3), file("b", 3)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    job.hold_batches(true);
    job.record(0, b"aaa", *blake3::hash(b"aaa").as_bytes());
    job.record(1, b"bbb", *blake3::hash(b"bbb").as_bytes());
    job.wait_pending(2, 5000);
    let before = job.fsync_retries();
    job.fault_fsync(None, 2, SONY_TRANSIENT); // the batch's first two fsync tries fail
    job.hold_batches(false);
    assert_eq!(job.wait(15_000), 0, "{}", job.events());
    assert_eq!(job.fsync_faults_left(), 0, "the fault was never reached");
    assert!(job.fsync_retries() >= before + 2, "no retry was made");
    assert_eq!(std::fs::read(root.join("a")).unwrap(), b"aaa");
    assert_eq!(std::fs::read(root.join("b")).unwrap(), b"bbb");
    assert!(job.events().contains("durable"), "{}", job.events());
}

#[test]
fn a_transient_fsync_error_that_never_clears_fails_the_job_after_its_retries() {
    let t = tmp("fsyncgiveup");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let m = Manifest {
        entries: vec![file("a", 1)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    job.hold_batches(true);
    job.record(0, b"a", *blake3::hash(b"a").as_bytes());
    job.wait_pending(1, 5000);
    let before = job.fsync_retries();
    job.fault_fsync(None, 1000, SONY_TRANSIENT);
    job.hold_batches(false);
    assert_eq!(
        job.wait(15_000),
        ava1::gen::ERR_IO as i32,
        "{}",
        job.events()
    );
    assert_eq!(
        job.fsync_retries() - before,
        4,
        "four retries, then the failure"
    );
    assert!(job.events().contains("fsync failed"), "{}", job.events());
    assert!(
        !job.events().contains("durable"),
        "nothing may be acknowledged: {}",
        job.events()
    );
}

#[test]
fn an_eio_from_fsync_is_never_retried() {
    let t = tmp("fsynceio");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let m = Manifest {
        entries: vec![file("a", 1)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    job.hold_batches(true);
    job.record(0, b"a", *blake3::hash(b"a").as_bytes());
    job.wait_pending(1, 5000);
    let before = job.fsync_retries();
    job.fault_fsync(None, 1000, 5); // EIO: the data did not reach the drive
    job.hold_batches(false);
    assert_eq!(
        job.wait(15_000),
        ava1::gen::ERR_IO as i32,
        "{}",
        job.events()
    );
    assert_eq!(job.fsync_retries(), before, "EIO must not be retried");
}

#[test]
fn a_transient_journal_fsync_error_is_retried() {
    let t = tmp("fsyncjnl");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let m = Manifest {
        entries: vec![file("a", 1)],
    };
    let job = CApplyJob::begin_opts(&t.join("jobs"), &root, 0, &m, 0, 0, LogOpts::OFF);
    job.trace(true);
    job.hold_batches(true);
    job.record(0, b"a", *blake3::hash(b"a").as_bytes());
    job.wait_pending(1, 5000);
    let before = job.fsync_retries();
    // hook 7 (the new directory's sync) is followed by the journal append and its fsync
    job.fault_fsync(Some(7), 1, SONY_TRANSIENT);
    job.hold_batches(false);
    assert_eq!(job.wait(15_000), 0, "{}", job.events());
    assert_eq!(job.fsync_faults_left(), 0, "{}", job.events());
    assert!(
        job.fsync_retries() > before,
        "the journal fsync was not retried"
    );
    assert!(job.events().contains("durable"), "{}", job.events());
}

#[test]
fn a_retried_fsync_that_left_a_small_file_wrong_fails_instead_of_acknowledging() {
    // The failed attempt may have dropped the dirty pages: the retry then "succeeds" on
    // nothing. The engine reads the file back; a file that no longer matches its root is
    // never acknowledged.
    let t = tmp("fsyncdrop");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let m = Manifest {
        entries: vec![file("a", 4)],
    };
    let job = CApplyJob::begin_opts(&t.join("jobs"), &root, 0, &m, 0, 0, LogOpts::OFF);
    job.hold_batches(true);
    job.record(0, b"good", *blake3::hash(b"good").as_bytes());
    job.wait_pending(1, 5000);
    std::fs::write(root.join("a"), [0u8; 4]).unwrap(); // the pages were lost: zeros on disk
    job.fault_fsync(None, 1, SONY_TRANSIENT);
    job.hold_batches(false);
    assert_eq!(
        job.wait(15_000),
        ava1::gen::ERR_IO as i32,
        "{}",
        job.events()
    );
    assert!(!job.events().contains("durable"), "{}", job.events());
}

#[test]
fn a_retried_fsync_on_a_large_file_is_reread_against_the_outboard() {
    let t = tmp("fsynclarge");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(2 * GROUP as usize + 5, 3);
    let m = Manifest {
        entries: vec![file("big", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    job.hold_batches(true);
    send_large(&job, 0, &d, false);
    std::thread::sleep(std::time::Duration::from_millis(500)); // the chunks are written
    job.fault_fsync(None, 1, SONY_TRANSIENT);
    job.hold_batches(false);
    assert_eq!(job.wait(15_000), 0, "{}", job.events());
    assert_eq!(std::fs::read(root.join("big")).unwrap(), d);

    // and a part file that no longer matches its outboard after the retry fails the job
    let t2 = tmp("fsynclarge2");
    let root2 = t2.join("dest");
    std::fs::create_dir_all(&root2).unwrap();
    drop(job);
    let job = CApplyJob::begin(&t2.join("jobs"), &root2, 0, &m, 0);
    job.hold_batches(true);
    send_large(&job, 0, &d, false);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let part = root2.join("big.ava-part");
    let mut bad = d.clone();
    bad[GROUP as usize + 7] ^= 0xff;
    std::fs::write(&part, &bad).unwrap();
    job.fault_fsync(None, 1, SONY_TRANSIENT);
    job.hold_batches(false);
    assert_eq!(
        job.wait(15_000),
        ava1::gen::ERR_IO as i32,
        "{}",
        job.events()
    );
}

extern "C" {
    fn ava1_test_apply_fail_batch_alloc(on: i32);
    fn ava1_pend_in_use() -> u32;
}

/// Final review (console): a sync batch that ran out of memory before its lists existed went
/// `goto out` with the descriptor count still 0, so ava1_pend_release(0) left every reservation of the
/// pending small files taken for good, and the pending-fd budget shrank with each failed batch.
#[test]
fn a_sync_batch_that_runs_out_of_memory_gives_its_pending_slots_back() {
    let t = tmp("batch-oom");
    let root = t.join("dest");
    let n = 40usize;
    let mut entries = vec![dir("a")];
    for i in 0..n {
        entries.push(file(&format!("a/f{i:03}"), 5));
    }
    let m = Manifest { entries };
    let before = unsafe { ava1_pend_in_use() };
    {
        let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
        unsafe { ava1_test_apply_fail_batch_alloc(1) };
        for i in 0..n {
            job.record(i as u32 + 1, b"hello", *blake3::hash(b"hello").as_bytes());
        }
        assert_eq!(job.wait(15_000), ava1::gen::ERR_IO as i32);
    }
    unsafe { ava1_test_apply_fail_batch_alloc(0) };
    assert_eq!(
        unsafe { ava1_pend_in_use() },
        before,
        "the failed batch leaked its pending-fd reservations"
    );
}
