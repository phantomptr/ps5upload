#![cfg(unix)]
//! Durable-by-log on the engine's receiver (review 003 §3.2, SPEC.md §15.7): a download of many small
//! files from the C sender through `LocalSink` with the pack log on — the journal records pack batches and
//! sweeps, the job ends settled with no pack file left, and the per-file path (log off) still works.
mod common;
use ava1_ctest::TempDir;

use std::sync::Arc;
use std::time::{Duration, Instant};

use ava1::journal::{job_dir, Journal, Record, State};
use ava1::manifest::Manifest;
use ava1::packlog::{LoggedGroup, PackOpts};
use ava1::recv::{download_job, LocalSink, RecvOptions, Sink};
use ava1::session::connect;
use ava1_ctest::CServer;
use common::*;

fn ro(jobs: &std::path::Path) -> RecvOptions {
    RecvOptions {
        credit: 64 << 20,
        flags: 0,
        jobs_dir: jobs.into(),
        ordered: false,
        progress: Arc::default(),
        cancel: Arc::default(),
        progress_deadline: None,
    }
}

async fn run(tag: &str, id: u8, log: bool) -> (TempDir, std::path::PathBuf, Vec<Record>) {
    ava1_ctest::c_set_read_allowed(true);
    let d = dir(tag);
    let src = d.join("console/game");
    write_tree(&src, 600, |i| 1 + i % 900);
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        0,
    );
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let mut link = s.job([id; 16]);
    let sink = Arc::new(LocalSink::new(d.join("got"), false).with_log(
        log,
        PackOpts {
            segment: 64 << 10, // segments roll several times
            max_unswept: 1 << 20,
            age: Duration::from_millis(20),
        },
    ));
    let t0 = Instant::now();
    let r = tokio::time::timeout(
        Duration::from_secs(120),
        download_job(
            &mut link,
            src.to_str().unwrap(),
            0,
            sink,
            ro(&d.join("ejobs")),
        ),
    )
    .await
    .expect("the download finished in time")
    .unwrap();
    assert_eq!(r.files, 600, "{:?}", t0.elapsed());
    assert!(same_tree(&src, &d.join("got")));
    let jd = job_dir(&d.join("ejobs"), &[id; 16]);
    let (_, recs) = Journal::open(&jd).unwrap();
    (d, jd, recs)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_through_the_pack_log_journals_batches_and_sweeps_and_ends_settled() {
    let (_d, jd, recs) = run("dl-logged", 0x6c, true).await;
    let packed = recs
        .iter()
        .filter(|r| matches!(r, Record::Batch(b) if b.pack_segment.is_some()))
        .count();
    let sweeps = recs
        .iter()
        .filter(|r| matches!(r, Record::Sweep(_)))
        .count();
    assert!(packed >= 1, "no batch carried the pack extension");
    assert!(sweeps >= 1, "no sweep was journaled");
    let mut st = State::default();
    for r in &recs {
        st.apply(r);
    }
    assert_eq!(st.done.len(), 600);
    assert!(st.unswept.is_empty() && st.packs.is_empty(), "{st:?}");
    assert_eq!(st.finished, Some(0));
    let left = std::fs::read_dir(&jd)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("pack."))
        .count();
    assert_eq!(left, 0, "pack files left behind");
}

#[tokio::test(flavor = "multi_thread")]
async fn with_the_log_off_the_per_file_path_is_unchanged() {
    let (_d, _jd, recs) = run("dl-unlogged", 0x6d, false).await;
    assert!(recs.iter().all(
        |r| !matches!(r, Record::Batch(b) if b.pack_segment.is_some())
            && !matches!(r, Record::Sweep(_))
    ));
}

/// Watches the journal while a real download runs: a batch's pack record is durable before its files are
/// reported done to the sink (I2), and a file is swept only after the batch that made it done (I1/I3).
struct Spy {
    inner: LocalSink,
    jd: std::path::PathBuf,
    pack_batches_before: std::sync::Mutex<usize>,
    violations: std::sync::Mutex<Vec<String>>,
    sweeps_seen: std::sync::atomic::AtomicUsize,
}

impl Spy {
    fn state(&self) -> (State, usize) {
        let (_, recs) = Journal::open(&self.jd).unwrap();
        let mut st = State::default();
        let mut packed = 0;
        for r in &recs {
            if matches!(r, Record::Batch(b) if b.pack_segment.is_some()) {
                packed += 1;
            }
            st.apply(r);
        }
        (st, packed)
    }
    fn bad(&self, m: String) {
        self.violations.lock().unwrap().push(m);
    }
}

impl Sink for Spy {
    fn prepare(&self, m: &Manifest) -> std::io::Result<()> {
        self.inner.prepare(m)
    }
    fn write_at(&self, id: u32, off: u64, d: &[u8]) -> std::io::Result<()> {
        self.inner.write_at(id, off, d)
    }
    fn write_whole(&self, id: u32, d: &[u8]) -> std::io::Result<()> {
        self.inner.write_whole(id, d)
    }
    fn write_whole_root(&self, id: u32, r: &[u8; 32], d: &[u8]) -> std::io::Result<()> {
        self.inner.write_whole_root(id, r, d)
    }
    fn sync(&self, ids: &[u32]) -> std::io::Result<()> {
        self.inner.sync(ids)
    }
    fn read_at(&self, id: u32, off: u64, b: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read_at(id, off, b)
    }
    fn commit(&self, id: u32) -> std::io::Result<()> {
        self.inner.commit(id)
    }
    fn finish(&self) -> std::io::Result<()> {
        let (st, _) = self.state();
        if !st.unswept.is_empty() {
            self.bad(format!("finish with {} files unswept", st.unswept.len()));
        }
        self.inner.finish()
    }
    fn enable_log(&self, dir: &std::path::Path) {
        self.inner.enable_log(dir)
    }
    fn sync_batch(&self, small: &[u32], large: &[u32]) -> std::io::Result<Vec<LoggedGroup>> {
        *self.pack_batches_before.lock().unwrap() = self.state().1;
        self.inner.sync_batch(small, large)
    }
    fn batch_journaled(&self, groups: &[LoggedGroup]) {
        let before = *self.pack_batches_before.lock().unwrap();
        let (_, now) = self.state();
        if now < before + groups.len() {
            self.bad(format!(
                "batch_journaled with {} pack records journaled for {} groups (had {before})",
                now - before,
                groups.len()
            ));
        }
        self.inner.batch_journaled(groups)
    }
    fn unswept(&self) -> usize {
        self.inner.unswept()
    }
    fn log_pressure(&self) -> bool {
        self.inner.log_pressure()
    }
    fn sweep(&self, force: bool) -> std::io::Result<Vec<u32>> {
        let ids = self.inner.sweep(force)?;
        self.sweeps_seen
            .fetch_add(ids.len(), std::sync::atomic::Ordering::Relaxed);
        let (st, _) = self.state();
        if let Some(id) = ids.iter().find(|i| !st.done.contains(i)) {
            self.bad(format!(
                "file {id} was swept before its batch was journaled"
            ));
        }
        Ok(ids)
    }
    fn sweep_journaled(&self, ids: &[u32]) {
        let (st, _) = self.state();
        if let Some(id) = ids.iter().find(|i| st.unswept.contains(i)) {
            self.bad(format!(
                "segments released while {id} was still unswept in the journal"
            ));
        }
        self.inner.sweep_journaled(ids)
    }
    fn recover_log(&self, st: &State) -> std::io::Result<Vec<u32>> {
        self.inner.recover_log(st)
    }
    fn log_cleanup(&self) {
        self.inner.log_cleanup()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_receive_loop_journals_before_it_reports_and_sweeps_before_it_releases() {
    ava1_ctest::c_set_read_allowed(true);
    let d = dir("dl-spy");
    let src = d.join("console/game");
    write_tree(&src, 500, |i| 1 + i % 700);
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        0,
    );
    let s = connect(&srv.addr(), me, mine, "rust", calm())
        .await
        .unwrap();
    let mut link = s.job([0x6e; 16]);
    let jd = job_dir(&d.join("ejobs"), &[0x6e; 16]);
    let spy = Arc::new(Spy {
        inner: LocalSink::new(d.join("got"), false).with_log(
            true,
            PackOpts {
                segment: 32 << 10,
                max_unswept: 1 << 20,
                age: Duration::from_millis(20),
            },
        ),
        jd,
        pack_batches_before: Default::default(),
        violations: Default::default(),
        sweeps_seen: Default::default(),
    });
    let sink: Arc<dyn Sink> = spy.clone();
    let r = tokio::time::timeout(
        Duration::from_secs(120),
        download_job(
            &mut link,
            src.to_str().unwrap(),
            0,
            sink,
            ro(&d.join("ejobs")),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(r.files, 500);
    assert!(same_tree(&src, &d.join("got")));
    assert!(
        spy.sweeps_seen.load(std::sync::atomic::Ordering::Relaxed) > 0,
        "nothing was swept"
    );
    let v = spy.violations.lock().unwrap().clone();
    assert!(v.is_empty(), "{v:?}");
}
