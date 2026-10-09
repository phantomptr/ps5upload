#![cfg(unix)]

use ava1::gen::{self, ENTRY_DIR, ENTRY_FILE};
use ava1::journal::{job_dir, Journal, Record};
use ava1::manifest::{Entry, Manifest};
use ava1::verify::GROUP;
use ava1_ctest::*;

fn tmp(tag: &str) -> TempDir {
    TempDir::new(format!("ava1-recv-{tag}-{}", std::process::id()))
}

fn f(path: &str, size: u64, mtime: u64) -> Entry {
    Entry {
        kind: ENTRY_FILE,
        mode: 0o644,
        size,
        mtime,
        path: path.into(),
        root: None,
    }
}

fn small_files(n: usize) -> Manifest {
    let mut entries = vec![Entry {
        kind: ENTRY_DIR,
        mode: 0o755,
        size: 0,
        mtime: 0,
        path: "d".into(),
        root: None,
    }];
    for i in 0..n {
        entries.push(f(&format!("d/{i}"), 4, 1_600_000_000));
    }
    Manifest { entries }
}

fn body(i: usize) -> Vec<u8> {
    format!("{i:04}").into_bytes()
}

fn send_all(r: &CRecv, n: usize) {
    for i in 0..n {
        r.record(i as u32 + 1, &body(i), *blake3::hash(&body(i)).as_bytes());
    }
}

fn journal(t: &std::path::Path) -> Vec<Record> {
    Journal::open(&job_dir(&t.join("jobs"), &[7; 16]))
        .unwrap()
        .1
}

#[test]
fn a_new_folder_opens_staged_with_an_empty_map() {
    let t = tmp("new");
    let m = small_files(3);
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    assert_eq!(r.ack_staged(), 1);
    r.manifest(&m);
    r.wait_event("map status=0 last=1 done= partial=0", 5000);
    send_all(&r, 3);
    assert_eq!(r.wait(10_000), 0);
    assert_eq!(std::fs::read(t.join("dest/d/2")).unwrap(), body(2));
    assert!(!t.join("dest.ava-part").exists());
}

#[test]
fn c_crash_between_sync_and_journal_resends_the_batch() {
    for (crash, expect_done) in [(1, false), (2, true)] {
        let t = tmp(&format!("crash{crash}"));
        let m = small_files(50);
        let r = CRecv::open(
            &t.join("jobs"),
            &t.join("dest"),
            0,
            gen::POLICY_REPLACE,
            crash,
        );
        r.manifest(&m);
        r.wait_event("map status=0", 5000);
        // all 50 in one batch, deterministically: no batch starts until they are pending
        r.hold_batches(true);
        send_all(&r, 50);
        r.wait_pending(50, 5000);
        r.hold_batches(false);
        r.wait_stopped(10_000); // the injected crash stopped the job thread
        let r = r.restart(0); // payload restart: memory gone, disk kept
        r.manifest(&m);
        let ev = r.wait_event("map status=0", 5000);
        assert_eq!(
            ev.contains("done=1+50"),
            expect_done,
            "crash point {crash}: {ev}"
        );
        if !expect_done {
            send_all(&r, 50);
        }
        // the destination this job took as its lock (an empty folder) is recognised as ours
        assert_eq!(r.wait(10_000), 0, "{}", r.events());
        assert_eq!(std::fs::read(t.join("dest/d/49")).unwrap(), body(49));
    }
}

#[test]
fn c_changed_file_loses_its_progress() {
    let t = tmp("changed");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let d: Vec<u8> = (0..3 * GROUP as usize).map(|i| i as u8).collect();
    let m = Manifest {
        entries: vec![f("big", d.len() as u64, 100)],
    };
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.chunk(0, 0, &d[..GROUP as usize]);
    r.wait_event("durable", 5000);
    let r = r.restart(0);
    r.manifest(&m);
    assert!(r.wait_event("map status=0", 5000).contains("partial=1"));
    let r = r.restart(0);
    let mut m2 = m.clone();
    m2.entries[0].mtime = 101; // the source changed while we were away
    r.manifest(&m2);
    assert!(r.wait_event("map status=0", 5000).contains("partial=0"));
    // the old bytes are gone before any new range is asked for, and the reset is journaled
    assert!(!t.join("dest/big.ava-part").exists());
    let d2: Vec<u8> = d.iter().map(|b| b ^ 0x5a).collect();
    for o in (0..d2.len()).step_by(GROUP as usize) {
        r.chunk(0, o as u64, &d2[o..o + GROUP as usize]);
    }
    r.root(0, *blake3::hash(&d2).as_bytes());
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    assert_eq!(std::fs::read(t.join("dest/big")).unwrap(), d2);
    drop(r);
    assert!(journal(&t).iter().any(|x| matches!(x, Record::Reset(0))));
}

#[test]
fn a_changed_manifest_keeps_unchanged_files_by_path() {
    let t = tmp("remap");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let g = GROUP as usize;
    let d: Vec<u8> = (0..2 * g + 7).map(|i| (i * 3) as u8).collect();
    let m = Manifest {
        entries: vec![f("a", 4, 9), f("big", d.len() as u64, 9)],
    };
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.chunk(1, 0, &d[..g]);
    r.wait_event("durable", 5000);
    let r = r.restart(0);
    // a new file sorts first: "big" moves from id 1 to id 2 and keeps its group
    let m2 = Manifest {
        entries: vec![f("0new", 4, 9), f("a", 4, 9), f("big", d.len() as u64, 9)],
    };
    r.manifest(&m2);
    let ev = r.wait_event("map status=0", 5000);
    assert!(ev.contains("partial=1"), "{ev}");
    r.chunk(2, g as u64, &d[g..]);
    r.root(2, *blake3::hash(&d).as_bytes());
    r.record(0, b"new!", *blake3::hash(b"new!").as_bytes());
    r.record(1, b"aaaa", *blake3::hash(b"aaaa").as_bytes());
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    assert_eq!(std::fs::read(t.join("dest/big")).unwrap(), d);
}

#[test]
fn skip_existing_and_verify_mark_matching_files_done() {
    let t = tmp("policy");
    let dest = t.join("dest");
    std::fs::create_dir_all(dest.join("d")).unwrap();
    std::fs::write(dest.join("d/0"), body(0)).unwrap();
    std::fs::write(dest.join("d/1"), b"XXXX").unwrap();
    for p in ["d/0", "d/1"] {
        let ft = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        std::fs::File::options()
            .write(true)
            .open(dest.join(p))
            .unwrap()
            .set_modified(ft)
            .unwrap();
    }
    let m = small_files(2);
    let r = CRecv::open(&t.join("jobs"), &dest, 0, gen::POLICY_SKIP_EXISTING, 0);
    r.manifest(&m);
    assert!(
        r.wait_event("map status=0", 5000).contains("done=1+2"),
        "both match by size+mtime"
    );
    drop(r);
    let t2 = tmp("policy2");
    let dest2 = t2.join("dest");
    std::fs::create_dir_all(dest2.join("d")).unwrap();
    std::fs::write(dest2.join("d/0"), body(0)).unwrap();
    std::fs::write(dest2.join("d/1"), b"XXXX").unwrap();
    let mut mv = small_files(2);
    mv.entries[1].root = Some(*blake3::hash(&body(0)).as_bytes());
    mv.entries[2].root = Some(*blake3::hash(&body(1)).as_bytes());
    let r = CRecv::open(&t2.join("jobs"), &dest2, 0, gen::POLICY_VERIFY, 0);
    r.manifest(&mv);
    let ev = r.wait_event("map status=0", 5000);
    assert!(
        ev.contains("done=1+1,") && !ev.contains("done=1+2"),
        "only d/0 hashes equal: {ev}"
    );
}

#[test]
fn a_torn_tail_group_fails_the_resume_check() {
    let t = tmp("verify");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let d: Vec<u8> = (0..3 * GROUP as usize).map(|i| (i * 7) as u8).collect();
    let m = Manifest {
        entries: vec![f("big", d.len() as u64, 5)],
    };
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.chunk(0, 0, &d[..2 * GROUP as usize]);
    r.wait_event("durable", 5000);
    let r = r.restart(0);
    // Damage the second group on disk, as a crash mid-write could.
    let part = t.join("dest/big.ava-part");
    let mut b = std::fs::read(&part).unwrap();
    b[GROUP as usize + 10] ^= 0xff;
    std::fs::write(&part, &b).unwrap();
    r.manifest(&m);
    assert!(r.wait_event("map status=0", 5000).contains("partial=0"));
}

#[test]
fn an_intact_tail_passes_the_resume_check() {
    let t = tmp("intact");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let d: Vec<u8> = (0..3 * GROUP as usize).map(|i| (i * 5) as u8).collect();
    let m = Manifest {
        entries: vec![f("big", d.len() as u64, 5)],
    };
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.chunk(0, 0, &d[..2 * GROUP as usize]);
    r.wait_event("durable", 5000);
    let r = r.restart(0);
    r.manifest(&m);
    assert!(r.wait_event("map status=0", 5000).contains("partial=1"));
    r.chunk(0, 2 * GROUP, &d[2 * GROUP as usize..]);
    r.root(0, *blake3::hash(&d).as_bytes());
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    assert_eq!(std::fs::read(t.join("dest/big")).unwrap(), d);
}

#[test]
fn a_manifest_that_does_not_match_its_end_is_refused() {
    let t = tmp("badend");
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest_with_hash(&small_files(2), [9; 32]);
    r.wait_event(&format!("map status={}", gen::ERR_PROTOCOL), 5000);
}

#[test]
fn an_empty_folder_is_a_valid_job() {
    let t = tmp("empty");
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&Manifest { entries: vec![] });
    r.wait_event("map status=0 last=1 done= partial=0", 5000);
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    assert!(t.join("dest").is_dir());
    assert!(!t.join("dest.ava-part").exists());
}

#[test]
fn an_entry_count_past_the_cap_is_refused_at_open() {
    let t = tmp("cap");
    assert_eq!(
        c_recv_open_status(&t.join("jobs"), &t.join("dest"), OPEN_OK.entries(4_000_001)),
        gen::ERR_PROTOCOL as i32
    );
    assert_eq!(
        c_recv_open_status(&t.join("jobs"), &t.join("dest"), OPEN_OK.entries(10)),
        0
    );
}

#[test]
fn a_destination_that_appears_before_prepare_is_refused() {
    let t = tmp("taken");
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    assert_eq!(r.ack_staged(), 1);
    std::fs::create_dir_all(t.join("dest")).unwrap(); // someone else's, made after JobOpen
    r.manifest(&small_files(1));
    r.wait_event(&format!("map status={}", gen::ERR_EXISTS), 5000);
    assert_eq!(r.wait(10_000), gen::ERR_EXISTS as i32);
    // nothing was written into the folder that is not ours
    assert_eq!(std::fs::read_dir(t.join("dest")).unwrap().count(), 0);
}

#[test]
fn a_crash_after_taking_the_destination_resumes_cleanly() {
    let t = tmp("takecrash");
    let m = small_files(3);
    // stops right after mkdir(dest) and its directory sync, before the journal exists
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 3);
    r.manifest(&m);
    r.wait_stopped(10_000);
    assert!(t.join("dest").is_dir());
    let r = r.restart(0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    send_all(&r, 3);
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    assert_eq!(std::fs::read(t.join("dest/d/1")).unwrap(), body(1));
}

#[test]
fn a_finished_job_reopened_answers_all_done_and_its_status() {
    let t = tmp("finished");
    let m = small_files(3);
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    send_all(&r, 3);
    assert_eq!(r.wait(10_000), 0);
    let r = r.restart(0);
    r.manifest(&m);
    let ev = r.wait_event("map status=0", 5000);
    assert!(ev.contains("done=1+3"), "{ev}");
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    assert_eq!(std::fs::read(t.join("dest/d/2")).unwrap(), body(2));
}

#[test]
fn resume_with_the_stored_manifest_answers_the_map() {
    let t = tmp("resume");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let m = small_files(4);
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.hold_batches(true);
    send_all(&r, 2);
    r.wait_pending(2, 5000);
    r.hold_batches(false);
    r.wait_event("durable", 5000);
    let r = r.restart(0);
    r.resume([1; 32]); // not the manifest it holds
    r.wait_event(&format!("map status={}", gen::ERR_UNKNOWN_JOB), 5000);
    let r = r.restart(0);
    r.resume(m.hash());
    let ev = r.wait_event("map status=0", 5000);
    assert!(ev.contains("done=1+2"), "{ev}");
    r.record(3, &body(2), *blake3::hash(&body(2)).as_bytes());
    r.record(4, &body(3), *blake3::hash(&body(3)).as_bytes());
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
}

#[test]
fn a_large_map_is_paged() {
    let t = tmp("paged");
    let dest = t.join("dest");
    std::fs::create_dir_all(dest.join("d")).unwrap();
    // every other file exists: 2,001 done runs, more than one JobMap page holds
    let n = 4002;
    let ft = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
    let m = small_files(n);
    for e in m.entries.iter().skip(1).step_by(2) {
        let p = dest.join(&e.path);
        std::fs::write(&p, b"zzzz").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(ft)
            .unwrap();
    }
    let r = CRecv::open(&t.join("jobs"), &dest, 0, gen::POLICY_SKIP_EXISTING, 0);
    r.manifest(&m);
    let ev = r.wait_event("map status=0 last=1", 10_000);
    let first = ev.find("map status=0 last=0").expect(&ev);
    assert!(first < ev.find("map status=0 last=1").unwrap());
}

#[test]
fn a_single_file_lands_at_its_root() {
    let t = tmp("single");
    let root = t.join("sub/file.bin");
    let m = Manifest {
        entries: vec![f("file.bin", 4, 7)],
    };
    let r = CRecv::open(
        &t.join("jobs"),
        &root,
        gen::JF_SINGLE_FILE,
        gen::POLICY_REPLACE,
        0,
    );
    assert_eq!(r.ack_staged(), 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.record(0, b"wxyz", *blake3::hash(b"wxyz").as_bytes());
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    assert_eq!(std::fs::read(&root).unwrap(), b"wxyz");
}

#[test]
fn a_held_destination_that_gains_files_is_not_replaced() {
    let t = tmp("heldfull");
    let m = small_files(1);
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    // our empty lock folder gains someone's file mid-upload
    std::fs::write(t.join("dest/theirs"), b"keep").unwrap();
    send_all(&r, 1);
    assert_eq!(r.wait(10_000), gen::ERR_EXISTS as i32);
    assert_eq!(std::fs::read(t.join("dest/theirs")).unwrap(), b"keep");
    assert!(t.join("dest.ava-part/d/0").exists());
    // the rename's own errno is in the message
    let ev = r.events();
    assert!(
        ev.contains("msg ") && {
            let l = ev.to_lowercase();
            l.contains("not empty") || l.contains("exists")
        },
        "{ev}"
    );
}

// ---- fix round 1 ------------------------------------------------------------------

fn big(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn send_big(r: &CRecv, id: u32, d: &[u8]) {
    for o in (0..d.len()).step_by(GROUP as usize) {
        r.chunk(id, o as u64, &d[o..(o + GROUP as usize).min(d.len())]);
    }
    r.root(id, *blake3::hash(d).as_bytes());
}

const OPEN_OK: OpenArgs = OpenArgs {
    kind: gen::JOB_UPLOAD,
    flags: 0,
    policy: gen::POLICY_REPLACE,
    entries: 0,
};

#[test]
fn c1_a_crash_between_the_commit_rename_and_its_journal_keeps_the_file() {
    let t = tmp("c1");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    std::fs::write(t.join("dest/big"), b"the old version").unwrap();
    let d = big(2 * GROUP as usize + 5, 1);
    let m = Manifest {
        entries: vec![f("big", d.len() as u64, 3)],
    };
    // dies after rename(big.ava-part, big) and its directory sync, before the journal
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 4);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    send_big(&r, 0, &d);
    r.wait_stopped(10_000);
    assert_eq!(std::fs::read(t.join("dest/big")).unwrap(), d);
    let r = r.restart(0);
    r.manifest(&m);
    let ev = r.wait_event("map status=0", 5000);
    assert!(ev.contains("done=0+1"), "{ev}");
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    assert_eq!(std::fs::read(t.join("dest/big")).unwrap(), d);
}

#[test]
fn c2_a_crash_before_the_commit_finishes_on_resume_without_resending() {
    let t = tmp("c2");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let d = big(3 * GROUP as usize, 2);
    let m = Manifest {
        entries: vec![f("big", d.len() as u64, 3)],
    };
    // every range and the root are journaled; dies as the commit starts
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 5);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    send_big(&r, 0, &d);
    r.wait_stopped(10_000);
    let r = r.restart(0);
    r.manifest(&m);
    assert!(r.wait_event("map status=0", 5000).contains("partial=1"));
    assert_eq!(r.wait(10_000), 0, "{}", r.events()); // nothing sent again
    assert!(!r.events().contains("retry"), "{}", r.events());
    assert_eq!(std::fs::read(t.join("dest/big")).unwrap(), d);
}

#[test]
fn i1_a_crash_after_the_staging_rename_finishes_ok() {
    let t = tmp("i1");
    let m = small_files(2);
    // dies after rename(dest.ava-part, dest) and its sync, before Done is journaled
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 6);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    send_all(&r, 2);
    r.wait_stopped(10_000);
    assert!(t.join("dest/d/1").exists());
    let r = r.restart(0);
    r.manifest(&m);
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    assert_eq!(std::fs::read(t.join("dest/d/1")).unwrap(), body(1));
}

#[test]
fn i2_a_failed_job_still_held_by_its_session_is_busy_not_reused() {
    let t = tmp("i2");
    std::fs::create_dir_all(t.join("dest/f1")).unwrap(); // file 1 lands on a folder: ERR_IO
    let m = Manifest {
        entries: vec![f("f0", 1, 1), f("f1", 1, 1)],
    };
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.record(1, b"b", *blake3::hash(b"b").as_bytes());
    assert_eq!(r.wait(10_000), gen::ERR_IO as i32);
    // the failed job's old session still holds it: its threads may still run
    assert_eq!(r.reopen(false), gen::ERR_BUSY as i32);
    std::fs::remove_dir(t.join("dest/f1")).unwrap();
    assert_eq!(r.reopen(true), 0); // released: retired, reloaded from its journal
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.record(0, b"a", *blake3::hash(b"a").as_bytes());
    r.record(1, b"b", *blake3::hash(b"b").as_bytes());
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
}

#[test]
fn i3_files_removed_from_the_manifest_leave_nothing_in_the_staged_tree() {
    let t = tmp("i3");
    let d = big(2 * GROUP as usize + 1, 3);
    let m = Manifest {
        entries: vec![
            f("big", d.len() as u64, 1),
            Entry {
                kind: ENTRY_DIR,
                mode: 0o755,
                size: 0,
                mtime: 0,
                path: "gone".into(),
                root: None,
            },
            f("gone/x", 4, 1),
            f("keep", 4, 1),
        ],
    };
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.chunk(0, 0, &d[..GROUP as usize]);
    r.record(2, b"xxxx", *blake3::hash(b"xxxx").as_bytes());
    r.wait_event("durable files=2+1", 5000);
    r.wait_event("ranges=1", 5000);
    let r = r.restart(0);
    let m2 = Manifest {
        entries: vec![f("keep", 4, 1)],
    };
    r.manifest(&m2);
    r.wait_event("map status=0", 5000);
    r.record(0, b"kkkk", *blake3::hash(b"kkkk").as_bytes());
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    let mut names: Vec<String> = std::fs::read_dir(t.join("dest"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, vec!["keep".to_string()]);
}

#[test]
fn i3_merge_mode_never_deletes_the_users_files() {
    let t = tmp("i3m");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let d = big(2 * GROUP as usize + 1, 4);
    let m = Manifest {
        entries: vec![f("a", 4, 1), f("big", d.len() as u64, 1), f("keep", 4, 1)],
    };
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.record(0, b"aaaa", *blake3::hash(b"aaaa").as_bytes());
    r.chunk(1, 0, &d[..GROUP as usize]);
    r.wait_event("ranges=1", 5000);
    let r = r.restart(0);
    r.manifest(&Manifest {
        entries: vec![f("keep", 4, 1)],
    });
    r.wait_event("map status=0", 5000);
    r.record(0, b"kkkk", *blake3::hash(b"kkkk").as_bytes());
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    assert_eq!(std::fs::read(t.join("dest/a")).unwrap(), b"aaaa"); // in place: the user's now
    assert!(!t.join("dest/big.ava-part").exists());
}

#[test]
fn i4_a_changed_manifest_during_a_tiny_file_backlog_does_not_deadlock() {
    let t = tmp("i4");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let n = 700;
    let m = small_files(n);
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.hold_batches(true);
    send_all(&r, n);
    r.wait_pending(512, 5000); // full: a worker now waits in pend_add
    let mut m2 = m.clone();
    m2.entries[n].mtime += 1;
    r.manifest(&m2);
    let t0 = std::time::Instant::now();
    while r.events().matches("map status=0").count() < 2 {
        assert!(t0.elapsed().as_secs() < 10, "no second map: {}", r.events());
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    r.hold_batches(false);
}

#[test]
fn i5_a_journal_dropped_for_its_manifest_keeps_the_staging_choice() {
    let t = tmp("i5");
    let m = small_files(2);
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    let jd = job_dir(&t.join("jobs"), &[7; 16]);
    let r = r.restart(0);
    // a crash between the manifest write and the journal's compaction: they disagree
    ava1::journal::write_manifest(&jd, &small_files(3)).unwrap();
    let r = r.restart(0);
    assert_eq!(
        r.ack_staged(),
        1,
        "our own lock folder is not a merge target"
    );
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    send_all(&r, 2);
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    assert!(!t.join("dest.ava-part").exists());
}

#[test]
fn i6_job_open_fields_are_validated() {
    let t = tmp("i6");
    let (j, ok) = (t.join("jobs"), t.join("dest"));
    let p = gen::ERR_PROTOCOL as i32;
    for bad in ["dest/", "a/../dest", "a//dest", "a/./dest"] {
        let root = std::path::PathBuf::from(format!("{}/{bad}", t.display()));
        assert_eq!(
            c_recv_open_status(&j, &root, OPEN_OK),
            gen::ERR_PATH as i32,
            "{bad}"
        );
    }
    assert_eq!(
        c_recv_open_status(&j, &ok, OpenArgs { kind: 0, ..OPEN_OK }),
        p
    );
    assert_eq!(
        c_recv_open_status(&j, &ok, OpenArgs { kind: 9, ..OPEN_OK }),
        p
    );
    assert_eq!(
        c_recv_open_status(
            &j,
            &ok,
            OpenArgs {
                policy: 3,
                ..OPEN_OK
            }
        ),
        p
    );
    assert_eq!(
        c_recv_open_status(
            &j,
            &ok,
            OpenArgs {
                flags: 0x100,
                ..OPEN_OK
            }
        ),
        p
    );
    assert_eq!(c_recv_open_status(&j, &ok, OPEN_OK), 0);
}

#[test]
fn i7_reattach_gives_the_free_credit_and_checks_the_owner() {
    let t = tmp("i7a");
    let m = small_files(1);
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    let c = r.ack_credit();
    assert!(c >= 8 << 20, "{c}");
    r.reserve(1 << 20); // a frame in memory, not yet freed
    r.set_owner(2);
    assert_eq!(r.reopen(false), gen::ERR_UNKNOWN_JOB as i32);
    r.set_owner(1);
    assert_eq!(r.reopen(false), 0); // the same job, a new session
    assert_eq!(r.ack_credit(), c - (1 << 20));
    r.unreserve(1 << 20);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    send_all(&r, 1);
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
}

#[test]
fn i7_a_job_open_for_another_root_is_refused_and_the_journal_kept() {
    let t = tmp("i7b");
    let m = small_files(1);
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    let jd = job_dir(&t.join("jobs"), &[7; 16]);
    let before = std::fs::read(jd.join("journal")).unwrap();
    r.set_root(&t.join("elsewhere"));
    assert_eq!(r.reopen(false), gen::ERR_PROTOCOL as i32); // in memory
    let r = r.restart_any(0); // memory gone: the open is refused from the journal on disk
    assert_eq!(r.last_open(), gen::ERR_PROTOCOL as i32);
    assert_eq!(std::fs::read(jd.join("journal")).unwrap(), before);
    r.set_root(&t.join("dest"));
    assert_eq!(r.reopen(true), 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    send_all(&r, 1);
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
}

#[test]
fn i7_writing_where_it_is_not_allowed_is_refused() {
    let t = tmp("i7c");
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.deny_write(true);
    r.set_root(&t.join("other"));
    assert_eq!(r.reopen(true), gen::ERR_PATH as i32);
    r.deny_write(false);
}

#[test]
fn i7_dying_mid_remap_resumes() {
    let t = tmp("i7d");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let g = GROUP as usize;
    let d = big(2 * g + 9, 5);
    let m = Manifest {
        entries: vec![f("a", 4, 9), f("big", d.len() as u64, 9)],
    };
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 7);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.chunk(1, 0, &d[..g]);
    r.wait_event("durable", 5000);
    let r = r.restart(7);
    let m2 = Manifest {
        entries: vec![f("0new", 4, 9), f("a", 4, 9), f("big", d.len() as u64, 9)],
    };
    r.manifest(&m2); // dies after the outboard renames, before the manifest and journal
    r.wait_stopped(10_000);
    let r = r.restart(0);
    r.manifest(&m2);
    r.wait_event("map status=0", 5000);
    r.record(0, b"new!", *blake3::hash(b"new!").as_bytes());
    r.record(1, b"aaaa", *blake3::hash(b"aaaa").as_bytes());
    send_big(&r, 2, &d); // whatever the map says, the whole file is acceptable
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    assert_eq!(std::fs::read(t.join("dest/big")).unwrap(), d);
}

#[test]
fn m1_the_resume_check_follows_a_file_to_its_new_id() {
    let t = tmp("m1");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let d = big(3 * GROUP as usize, 6);
    let m = Manifest {
        entries: vec![f("big", d.len() as u64, 5)],
    };
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.chunk(0, 0, &d[..2 * GROUP as usize]);
    r.wait_event("durable", 5000);
    let r = r.restart(0);
    let part = t.join("dest/big.ava-part");
    let mut b = std::fs::read(&part).unwrap();
    b[GROUP as usize + 10] ^= 0xff;
    std::fs::write(&part, &b).unwrap();
    let m2 = Manifest {
        entries: vec![f("a", 1, 5), f("big", d.len() as u64, 5)],
    };
    r.manifest(&m2); // big moves to id 1; its torn tail is still caught
    assert!(r.wait_event("map status=0", 5000).contains("partial=0"));
}

#[test]
fn m9_a_policy_match_drops_the_part_in_progress() {
    let t = tmp("m9");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let d = big(3 * GROUP as usize, 7);
    let m = Manifest {
        entries: vec![f("big", d.len() as u64, 1_600_000_000)],
    };
    let r = CRecv::open(
        &t.join("jobs"),
        &t.join("dest"),
        0,
        gen::POLICY_SKIP_EXISTING,
        0,
    );
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.chunk(0, 0, &d[..GROUP as usize]);
    r.wait_event("durable", 5000);
    let r = r.restart(0);
    // the file turned up complete meanwhile
    std::fs::write(t.join("dest/big"), &d).unwrap();
    let ft = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
    std::fs::File::options()
        .write(true)
        .open(t.join("dest/big"))
        .unwrap()
        .set_modified(ft)
        .unwrap();
    r.manifest(&m);
    assert!(r.wait_event("map status=0", 5000).contains("done=0+1"));
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    assert!(!t.join("dest/big.ava-part").exists());
    assert!(!job_dir(&t.join("jobs"), &[7; 16]).join("0.ob").exists());
}

#[test]
fn n1_a_root_one_byte_past_the_path_cap_is_refused() {
    // 1,025 bytes: `ava1_path_ok` sees 1,024 after the '/', but `j->root` holds 1,024 + NUL,
    // so it would be cut short into another path.
    let t = tmp("n1");
    let mut root = t.display().to_string();
    while root.len() < 1025 {
        let room = 1025 - root.len() - 1;
        root.push('/');
        root.push_str(&"r".repeat(room.min(100)));
    }
    assert_eq!(root.len(), 1025);
    assert_eq!(
        c_recv_open_status(&t.join("jobs"), std::path::Path::new(&root), OPEN_OK),
        gen::ERR_PATH as i32
    );
}

#[test]
fn n2_a_folders_only_manifest_creates_its_folders() {
    // No files: "every file is done" holds before anything was made. The staged tree must
    // still be prepared, so the declared folders exist when the job ends.
    let t = tmp("n2");
    let d = |p: &str| Entry {
        kind: ENTRY_DIR,
        mode: 0o755,
        size: 0,
        mtime: 1,
        path: p.into(),
        root: None,
    };
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&Manifest {
        entries: vec![d("a"), d("a/b")],
    });
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    assert!(t.join("dest/a/b").is_dir(), "{}", r.events());
    assert!(!t.join("dest.ava-part").exists());
}

#[test]
fn gc_removes_the_half_written_copy_of_an_abandoned_staged_upload() {
    // An upload given up on part-way leaves dest.ava-part. Only its job could resume it; once the
    // job is collected nothing else ever removes it, and it holds storage on the console for good.
    let t = tmp("gcpart");
    let m = small_files(3);
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.record(1, &body(0), *blake3::hash(&body(0)).as_bytes());
    r.wait_event("durable", 10_000);
    drop(r);
    assert!(
        t.join("dest.ava-part").exists(),
        "the abandoned copy is there"
    );
    assert_eq!(jobs_gc(&t.join("jobs"), 3 * 86_400, 86_400), 1);
    assert!(!job_dir(&t.join("jobs"), &[7; 16]).exists());
    assert!(
        !t.join("dest.ava-part").exists(),
        "gc removed the abandoned copy"
    );
}

#[test]
fn gc_keeps_a_recent_staged_upload_and_its_copy() {
    let t = tmp("gcpartyoung");
    let m = small_files(3);
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.record(1, &body(0), *blake3::hash(&body(0)).as_bytes());
    r.wait_event("durable", 10_000);
    drop(r);
    assert_eq!(jobs_gc(&t.join("jobs"), 0, 86_400), 0);
    assert!(t.join("dest.ava-part").exists());
}
