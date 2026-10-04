//! Exhaustive torn-write sweeps through the console C (review 009 #2b): the journal and the
//! pack log are cut at EVERY byte length (and the last record flipped at every offset), and the
//! C replay and the C `ava1_pack_recover` must do what the Rust receivers do.
#![cfg(unix)]
use std::path::{Path, PathBuf};

use ava1::gen::{self, FileRange, FileRun, JnlBatch, JnlOpen, RootItem, ENTRY_DIR, ENTRY_FILE};
use ava1::journal::{job_dir, Journal, Record, State};
use ava1::manifest::{Entry, Manifest};
use ava1_ctest::*;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ava1-torn-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn c_style_dump(st: &State) -> String {
    let mut s = String::new();
    for f in &st.done {
        s.push_str(&format!("done {f}\n"));
    }
    for (f, r) in &st.ranges {
        for (a, b) in r.iter() {
            s.push_str(&format!("range {f} {a} {b}\n"));
        }
    }
    for (f, r) in &st.roots {
        s.push_str(&format!("root {f} {:02x}\n", r[0]));
    }
    match st.finished {
        Some(x) => s.push_str(&format!("finished={x}")),
        None => s.push_str("finished=none"),
    }
    s
}

fn open_of(r: &Record) -> JnlOpen {
    match r {
        Record::Open(o) => o.clone(),
        _ => unreachable!(),
    }
}

fn state_of(recs: &[Record]) -> State {
    let mut st = State::default();
    for r in recs {
        st.apply(r);
    }
    st
}

// ---- the journal: the C replay against the Rust replay, at every cut -----------------------

fn journal_fixture(tag: &str) -> (Vec<u8>, Vec<Record>) {
    let d = tmp(tag);
    let o = JnlOpen {
        job_id: [3; 16],
        manifest_hash: [4; 32],
        kind: 1,
        flags: 0,
        staged: 0,
        root: "/data/t".into(),
    };
    let mut j = Journal::create(&d, &o).unwrap();
    let mut recs = vec![Record::Open(o)];
    for i in 0..9u32 {
        recs.push(Record::Batch(JnlBatch {
            files: vec![FileRun {
                first: i * 2,
                count: 1,
            }],
            ranges: vec![FileRange {
                file_id: 1000,
                offset: (i as u64) << 20,
                len: 1 << 20,
            }],
            roots: vec![RootItem {
                file_id: 1000 + i,
                root: [i as u8; 32],
            }],
            pack_len: None,
            pack_offset: None,
            pack_segment: None,
        }));
    }
    recs.push(Record::Reset(4));
    recs.push(Record::Done(0));
    for r in &recs[1..] {
        j.append(r).unwrap();
    }
    drop(j);
    (std::fs::read(d.join("journal")).unwrap(), recs)
}

#[test]
fn the_c_replay_matches_the_rust_replay_at_every_truncation_length() {
    let (bytes, _) = journal_fixture("jnl-src-cut");
    let (rd, cd) = (tmp("jnl-rust"), tmp("jnl-c"));
    for l in 0..=bytes.len() {
        std::fs::write(rd.join("journal"), &bytes[..l]).unwrap();
        std::fs::write(cd.join("journal"), &bytes[..l]).unwrap();
        let rust = Journal::open(&rd);
        let dump = c_journal_dump(&cd);
        let clen = std::fs::metadata(cd.join("journal")).unwrap().len();
        match rust {
            Err(_) => {
                assert!(l < 8, "cut at {l}: Rust refused a journal with a header");
                assert_eq!(
                    clen as usize, l,
                    "cut at {l}: C must not touch a non-journal"
                );
            }
            Ok((_, recs)) => {
                assert_eq!(
                    dump,
                    c_style_dump(&state_of(&recs)),
                    "cut at {l}: C replay differs from Rust"
                );
                assert_eq!(
                    clen,
                    std::fs::metadata(rd.join("journal")).unwrap().len(),
                    "cut at {l}: C leaves the file at a different boundary"
                );
            }
        }
    }
}

#[test]
fn the_c_replay_rejects_a_flipped_byte_in_the_last_record_like_rust() {
    let (bytes, recs) = journal_fixture("jnl-src-flip");
    // the last record is Done(0): find its start by replaying all but it in Rust
    let want = c_style_dump(&state_of(&recs[..recs.len() - 1]));
    // where the last record starts: the length of the journal without it
    let last_start = {
        let d = tmp("jnl-flip-src");
        let mut j = Journal::create(&d, &open_of(&recs[0])).unwrap();
        for r in &recs[1..recs.len() - 1] {
            j.append(r).unwrap();
        }
        j.len() as usize
    };
    let cd = tmp("jnl-flip");
    for at in last_start..bytes.len() {
        for mask in [0x01u8, 0x80, 0xff] {
            let mut b = bytes.clone();
            b[at] ^= mask;
            std::fs::write(cd.join("journal"), &b).unwrap();
            assert_eq!(
                c_journal_dump(&cd),
                want,
                "flip {mask:#x} at {at}: C accepted a damaged record"
            );
            assert_eq!(
                std::fs::metadata(cd.join("journal")).unwrap().len() as usize,
                last_start,
                "flip at {at}"
            );
        }
    }
}

// ---- the pack log: ava1_pack_recover over a pack cut at every length -----------------------

fn small(n: usize) -> Manifest {
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
    Manifest { entries }
}

fn body(i: usize) -> Vec<u8> {
    format!("{i:04}").into_bytes()
}

fn slow() -> LogOpts {
    LogOpts {
        sweep_age_ms: 600_000,
        ..LogOpts::ON
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
    }
}

/// `(file index, start, end)` of each record of an intact pack segment, read off its frames
/// (records sit in arrival order, which is not the file order).
fn pack_spans(pack: &[u8]) -> Vec<(usize, usize, usize)> {
    let mut v = vec![];
    let mut at = 8;
    while at + 4 <= pack.len() {
        let body = u32::from_le_bytes(pack[at..at + 4].try_into().unwrap()) as usize;
        let id = u32::from_le_bytes(pack[at + 5..at + 9].try_into().unwrap()) as usize;
        v.push((id - 1, at, at + body + 8));
        at += body + 8;
    }
    assert_eq!(at, pack.len(), "the fixture pack is whole records");
    v
}

const N: usize = 6;

/// Runs one recovery: the crashed job (byte 7) in a fresh jobs dir with `pack.0` replaced by
/// `pack`, the destination emptied, and a start-time recovery pass on a different job. Returns
/// which files exist afterwards and the journal's replayed state.
fn recover_with(tag: &str, snap: &Path, dest_snap: &Path, pack: &[u8]) -> (Vec<usize>, State) {
    let t = tmp(tag);
    let jobs = t.join("jobs");
    let hex = job_dir(&jobs, &[7; 16]);
    copy_dir(&job_dir(snap, &[7; 16]), &hex);
    std::fs::write(hex.join("pack.0"), pack).unwrap();
    // The recovery re-makes files at the root the journal names: the snapshot's destination.
    let _ = std::fs::remove_dir_all(dest_snap.join("d"));
    std::fs::create_dir_all(dest_snap).unwrap();
    let opts = LogOpts {
        recover_every_ms: 600_000,
        job_byte: 9,
        ..slow()
    };
    let r = CRecv::open_opts(&jobs, &t.join("dest9"), 0, gen::POLICY_REPLACE, 0, opts);
    let t0 = std::time::Instant::now();
    while std::fs::read_dir(&hex)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("pack."))
    {
        assert!(
            t0.elapsed().as_secs() < 30,
            "recovery never finished ({tag})"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    drop(r);
    let present = (0..N)
        .filter(|i| std::fs::read(dest_snap.join(format!("d/{i}"))).ok() == Some(body(*i)))
        .collect();
    let (_, recs) = Journal::open(&hex).unwrap();
    (present, state_of(&recs))
}

#[test]
fn ava1_pack_recover_remakes_exactly_the_whole_records_at_every_cut_length() {
    // one crashed job: N small files logged in pack.0, journaled, never swept
    let t = tmp("pack-src");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let r = CRecv::open_opts(
        &t.join("jobs"),
        &t.join("dest"),
        0,
        gen::POLICY_REPLACE,
        2,
        slow(),
    );
    r.manifest(&small(N));
    r.wait_event("map status=0", 5000);
    r.hold_batches(true);
    for i in 0..N {
        r.record(i as u32 + 1, &body(i), *blake3::hash(&body(i)).as_bytes());
    }
    r.wait_pending(N as u32, 5000);
    r.hold_batches(false);
    r.wait_stopped(10_000);
    drop(r);
    let snap = t.join("jobs");
    let pack = std::fs::read(job_dir(&snap, &[7; 16]).join("pack.0")).unwrap();
    let spans = pack_spans(&pack);
    assert_eq!(spans.len(), N, "every file is one record");
    let (jst, _) = {
        let (_, recs) = Journal::open(&job_dir(&snap, &[7; 16])).unwrap();
        (state_of(&recs), 0)
    };
    assert_eq!(jst.unswept.len(), N, "the crash left every file unswept");
    let dest = t.join("dest");

    for l in 0..=pack.len() {
        let (present, st) = recover_with("pack-cut", &snap, &dest, &pack[..l]);
        let mut whole: Vec<usize> = spans
            .iter()
            .filter(|(_, _, e)| *e <= l)
            .map(|(i, _, _)| *i)
            .collect();
        whole.sort();
        assert_eq!(present, whole, "cut at {l}: only whole records are re-made");
        assert!(
            st.unswept.is_empty(),
            "cut at {l}: nothing may stay unswept"
        );
        for i in 0..N {
            assert_eq!(
                st.done.contains(&(i as u32 + 1)),
                whole.contains(&i),
                "cut at {l}: file {i}: done iff its record was whole (the rest are reset to be resent)"
            );
        }
    }

    // a flipped byte anywhere in the last record: that file is lost, the others are kept
    let (lost, start, end) = *spans.last().unwrap();
    for at in start..end {
        for mask in [0x01u8, 0x80] {
            let mut b = pack.clone();
            b[at] ^= mask;
            let (present, st) = recover_with("pack-flip", &snap, &dest, &b);
            assert_eq!(
                present,
                (0..N).filter(|i| *i != lost).collect::<Vec<_>>(),
                "flip {mask:#x} at {at}: the damaged record must not be re-made"
            );
            assert!(
                !st.done.contains(&(lost as u32 + 1)),
                "flip at {at}: file {lost} stays done"
            );
        }
    }
}
