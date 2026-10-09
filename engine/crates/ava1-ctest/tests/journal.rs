#![cfg(unix)]
use ava1::gen::{FileRange, FileRun, JnlBatch, JnlOpen, RootItem};
use ava1::journal::{Journal, Record, State};
use ava1_ctest::*;

fn tmp(tag: &str) -> TempDir {
    TempDir::new(format!("ava1-cjnl-{tag}-{}", std::process::id()))
}

/// The exact text `ava1_test_journal_dump` produces for the same state.
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

#[test]
fn c_replays_a_rust_journal() {
    let d = tmp("r2c");
    let o = JnlOpen {
        job_id: [3; 16],
        manifest_hash: [4; 32],
        kind: 1,
        flags: 0,
        staged: 0,
        root: "/data/t".into(),
    };
    let mut j = Journal::create(&d, &o).unwrap();
    let mut st = State::default();
    st.apply(&Record::Open(o.clone()));
    for i in 0..50u32 {
        let r = Record::Batch(JnlBatch {
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
        });
        st.apply(&r);
        j.append(&r).unwrap();
    }
    j.append(&Record::Reset(4)).unwrap();
    st.apply(&Record::Reset(4));
    drop(j);
    assert_eq!(c_journal_dump(&d), c_style_dump(&st));
}

#[test]
fn the_c_compaction_replays_to_the_same_state_in_rust() {
    // The one record kind the cross-tests never write is Snapshot, and the C compact had
    // no test at all. Here the C writer rewrites the journal and the Rust reader judges
    // the result — including the Done record the snapshot cannot carry.
    use ava1::gen::JnlDone;
    use ava1::wire::Message;
    let d = tmp("ccompact");
    let o = JnlOpen {
        job_id: [3; 16],
        manifest_hash: [4; 32],
        kind: 1,
        flags: 0,
        staged: 0,
        root: "/data/t".into(),
    };
    let mut j = Journal::create(&d, &o).unwrap();
    let mut st = State::default();
    st.apply(&Record::Open(o.clone()));
    for i in 0..20u32 {
        let r = Record::Batch(JnlBatch {
            files: vec![FileRun { first: i, count: 1 }],
            ranges: vec![FileRange {
                file_id: 7,
                offset: (i as u64) << 20,
                len: 1 << 20,
            }],
            roots: vec![RootItem {
                file_id: 7,
                root: [i as u8; 32],
            }],
            pack_len: None,
            pack_offset: None,
            pack_segment: None,
        });
        st.apply(&r);
        j.append(&r).unwrap();
    }
    let done = Record::Done(9);
    st.apply(&done);
    j.append(&done).unwrap();
    drop(j);

    let open_b = o.to_bytes().unwrap();
    let snap_b = st.snapshot().to_bytes().unwrap();
    let done_b = JnlDone { status: 9 }.to_bytes().unwrap();
    assert_eq!(c_journal_compact(&d, &open_b, &snap_b, Some(&done_b)), 0);

    let (j2, recs) = Journal::open(&d).unwrap();
    assert_eq!(recs.len(), 3); // open + snapshot + done
    let mut st2 = State::default();
    for r in &recs {
        st2.apply(r);
    }
    assert_eq!(st2, st);
    assert!(j2.len() < snap_b.len() as u64 + open_b.len() as u64 + done_b.len() as u64 + 64);
}

#[test]
fn c_journal_torn_tail_is_ignored() {
    let d = tmp("c2r");
    assert_eq!(c_journal_write_sample(&d), 0); // open + files 0..9 + reset 3 + done 0
    let (_, recs) = Journal::open(&d).unwrap();
    let mut st = State::default();
    for r in &recs {
        st.apply(r);
    }
    assert_eq!(
        st.done.iter().copied().collect::<Vec<_>>(),
        vec![0, 1, 2, 4, 5, 6, 7, 8, 9]
    );
    assert_eq!(st.finished, Some(0));
    let p = d.join("journal");
    let mut b = std::fs::read(&p).unwrap();
    b.truncate(b.len() - 2);
    std::fs::write(&p, &b).unwrap();
    assert!(c_journal_dump(&d).ends_with("finished=none"));
}
