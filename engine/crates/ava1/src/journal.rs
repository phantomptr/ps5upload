//! The receiver's journal (SPEC.md §14): data sync → journal append → Durable. One format,
//! written by both the engine and the console, so either can resume a job the other started.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, Write};
use std::path::{Path, PathBuf};

use crate::gen::{
    FileRun, JnlBatch, JnlDone, JnlOpen, JnlReset, JnlSnapshot, JnlSweep, ManifestEntry, PackRef,
    RootItem,
};
use crate::manifest::{Entry, Manifest};
use crate::ranges::{from_runs, runs, runs_within_limit, Need, RangeSet};
use crate::wire::{Message, Reader, Writer};

pub const MAGIC: &[u8; 8] = b"AVA1JNL1";
pub const K_OPEN: u8 = 1;
pub const K_BATCH: u8 = 2;
pub const K_RESET: u8 = 3;
pub const K_SNAPSHOT: u8 = 4;
pub const K_DONE: u8 = 5;
/// Durable-by-log (SPEC.md §15.7): files now durable in place.
pub const K_SWEEP: u8 = 6;
/// The journal is compacted once it passes this size (SPEC.md §14).
pub const COMPACT_AT: u64 = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Open(JnlOpen),
    Batch(JnlBatch),
    Reset(u32),
    Snapshot(JnlSnapshot),
    Done(u16),
    Sweep(Vec<FileRun>),
}

impl Record {
    fn encode(&self) -> io::Result<(u8, Vec<u8>)> {
        let (k, b) = match self {
            Record::Open(o) => (K_OPEN, o.to_bytes()),
            Record::Batch(b) => (K_BATCH, b.to_bytes()),
            Record::Reset(f) => (K_RESET, JnlReset { file_id: *f }.to_bytes()),
            Record::Snapshot(s) => (K_SNAPSHOT, s.to_bytes()),
            Record::Done(s) => (K_DONE, JnlDone { status: *s }.to_bytes()),
            Record::Sweep(f) => (K_SWEEP, JnlSweep { files: f.clone() }.to_bytes()),
        };
        let body = b.map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        Ok((k, body))
    }

    fn decode(kind: u8, b: &[u8]) -> Option<Record> {
        Some(match kind {
            K_OPEN => Record::Open(JnlOpen::decode(b).ok()?),
            K_BATCH => {
                let b = JnlBatch::decode(b).ok()?;
                if !runs_within_limit(&b.files) {
                    return None;
                }
                Record::Batch(b)
            }
            K_RESET => Record::Reset(JnlReset::decode(b).ok()?.file_id),
            K_SNAPSHOT => {
                // Fail closed: an `unswept` or `segments` stream that does not decode must stop the replay,
                // never read as "everything is swept" (a compaction would persist that).
                let s = JnlSnapshot::decode(b).ok()?;
                if !runs_within_limit(&s.done) {
                    return None;
                }
                if let Some(u) = &s.unswept {
                    if !runs_within_limit(&item_stream::<FileRun>(u)?) {
                        return None;
                    }
                }
                if let Some(g) = &s.segments {
                    item_stream::<PackRef>(g)?;
                }
                Record::Snapshot(s)
            }
            K_DONE => Record::Done(JnlDone::decode(b).ok()?.status),
            K_SWEEP => {
                let f = JnlSweep::decode(b).ok()?.files;
                if !runs_within_limit(&f) {
                    return None;
                }
                Record::Sweep(f)
            }
            _ => return None,
        })
    }
}

fn frame(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(body.len() + 9);
    v.extend_from_slice(&((body.len() + 1) as u32).to_le_bytes());
    v.push(kind);
    v.extend_from_slice(body);
    let mut c = vec![kind];
    c.extend_from_slice(body);
    v.extend_from_slice(&crate::crc32c::crc32c(&c).to_le_bytes());
    v
}

/// A single writer per directory: `create`, `open`, `append` and `compact` all assume no other
/// process holds this job's journal.
pub struct Journal {
    f: File,
    dir: PathBuf,
    len: u64,
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

fn open_for_append(p: &Path) -> io::Result<File> {
    let mut f = OpenOptions::new().read(true).write(true).open(p)?;
    f.seek(io::SeekFrom::End(0))?;
    Ok(f)
}

impl Journal {
    /// A brand-new job's journal. Clobbers any existing one in `dir`: callers resume with
    /// `open`, never with `create`.
    pub fn create(dir: &Path, open: &JnlOpen) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let (k, b) = Record::Open(open.clone()).encode()?;
        let mut all = MAGIC.to_vec();
        all.extend(frame(k, &b));
        let tmp = dir.join("journal.tmp");
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&all)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, dir.join("journal"))?; // same directory: never cross a device
        sync_dir(dir)?;
        let f = open_for_append(&dir.join("journal"))?;
        Ok(Self {
            f,
            dir: dir.to_path_buf(),
            len: all.len() as u64,
        })
    }

    /// Replays every intact record and truncates a torn tail so appends continue cleanly.
    pub fn open(dir: &Path) -> io::Result<(Self, Vec<Record>)> {
        let p = dir.join("journal");
        let b = fs::read(&p)?;
        if b.len() < 8 || &b[..8] != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not an AVA1 journal",
            ));
        }
        let mut at = 8usize;
        let mut recs = Vec::new();
        while b.len() - at >= 4 {
            let len = u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize;
            if len == 0 {
                break;
            }
            let Some(end) = at.checked_add(8).and_then(|x| x.checked_add(len)) else {
                break;
            };
            if end > b.len() {
                break;
            }
            let body = &b[at + 4..at + 4 + len];
            let crc = u32::from_le_bytes(b[at + 4 + len..end].try_into().unwrap());
            if crate::crc32c::crc32c(body) != crc {
                break;
            }
            let Some(r) = Record::decode(body[0], &body[1..]) else {
                break;
            };
            recs.push(r);
            at = end;
        }
        let f = OpenOptions::new().read(true).write(true).open(&p)?;
        f.set_len(at as u64)?;
        f.sync_all()?;
        let mut j = Self {
            f,
            dir: dir.to_path_buf(),
            len: at as u64,
        };
        j.seek_end()?;
        Ok((j, recs))
    }

    fn seek_end(&mut self) -> io::Result<()> {
        self.f.seek(io::SeekFrom::Start(self.len))?;
        Ok(())
    }

    pub fn append(&mut self, r: &Record) -> io::Result<()> {
        let (k, b) = r.encode()?;
        let fr = frame(k, &b);
        self.f.write_all(&fr)?;
        // sync_all, not sync_data: GC reads the journal's mtime as its age source, and
        // fdatasync does not flush an mtime — after a crash a long-lived journal could
        // look days older than it is and be collected.
        self.f.sync_all()?;
        self.len += fr.len() as u64;
        Ok(())
    }

    /// Rewrites the journal as Open ‖ Snapshot ‖ Done. The snapshot carries no terminal
    /// status, so a compaction of a finished job would lose it; the Done record is
    /// written back for exactly that reason (SPEC.md §14.2 — compaction loses nothing).
    pub fn compact(&mut self, open: &JnlOpen, st: &State) -> io::Result<()> {
        let mut all = MAGIC.to_vec();
        let (k, b) = Record::Open(open.clone()).encode()?;
        all.extend(frame(k, &b));
        let (k, b) = Record::Snapshot(st.snapshot()).encode()?;
        all.extend(frame(k, &b));
        if let Some(status) = st.finished {
            let (k, b) = Record::Done(status).encode()?;
            all.extend(frame(k, &b));
        }
        let tmp = self.dir.join("journal.tmp");
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&all)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, self.dir.join("journal"))?;
        sync_dir(&self.dir)?;
        self.f = open_for_append(&self.dir.join("journal"))?;
        self.len = all.len() as u64;
        Ok(())
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len <= 8
    }
}

/// A `records` item stream without its total-length prefix, as a snapshot extension carries it.
fn item_bytes<M: Message>(items: &[M]) -> Option<Vec<u8>> {
    let mut w = Writer::new();
    w.records(items).ok()?;
    Some(w.buf[4..].to_vec())
}

/// The inverse of `item_bytes`; `None` for a stream that does not decode in full.
fn item_stream<M: Message>(b: &[u8]) -> Option<Vec<M>> {
    let mut framed = (b.len() as u32).to_le_bytes().to_vec();
    framed.extend_from_slice(b);
    let mut r = Reader::new(&framed);
    let v = r.records().ok()?;
    r.finish().ok()?;
    Some(v)
}

/// What a replay knows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub open: Option<JnlOpen>,
    pub done: BTreeSet<u32>,
    pub ranges: BTreeMap<u32, RangeSet>,
    pub roots: BTreeMap<u32, [u8; 32]>,
    pub finished: Option<u16>,
    /// Done files whose bytes are only in the pack log so far (SPEC.md §15.7).
    pub unswept: BTreeSet<u32>,
    /// The pack ranges whose files are not all swept (a ref is live while an unswept id lies in
    /// `first_file..first_file + count`).
    pub packs: Vec<PackRef>,
}

impl State {
    pub fn apply(&mut self, r: &Record) {
        match r {
            Record::Open(o) => self.open = Some(o.clone()),
            Record::Batch(b) => {
                // A file marked done drops its ranges (SPEC.md §14.2): the bytes are complete.
                self.done.extend(from_runs(&b.files));
                for x in &b.ranges {
                    self.ranges
                        .entry(x.file_id)
                        .or_default()
                        .insert(x.offset, x.offset + x.len);
                }
                for x in &b.roots {
                    self.roots.insert(x.file_id, x.root);
                }
                for f in from_runs(&b.files) {
                    self.ranges.remove(&f);
                }
                if let (Some(segment), Some(offset), Some(len)) =
                    (b.pack_segment, b.pack_offset, b.pack_len)
                {
                    let files = from_runs(&b.files);
                    if let (Some(&lo), Some(&hi)) = (files.first(), files.last()) {
                        self.unswept.extend(files.iter().copied());
                        self.packs.push(PackRef {
                            segment,
                            offset,
                            len,
                            first_file: lo,
                            count: hi - lo + 1,
                        });
                    }
                }
            }
            Record::Sweep(f) => {
                for id in from_runs(f) {
                    self.unswept.remove(&id);
                }
                self.prune_packs();
            }
            Record::Reset(f) => {
                self.done.remove(f);
                self.unswept.remove(f);
                self.prune_packs();
                self.ranges.remove(f);
                self.roots.remove(f);
            }
            Record::Snapshot(s) => {
                self.done = from_runs(&s.done);
                self.ranges.clear();
                for x in &s.ranges {
                    self.ranges
                        .entry(x.file_id)
                        .or_default()
                        .insert(x.offset, x.offset + x.len);
                }
                self.roots = s.roots.iter().map(|x| (x.file_id, x.root)).collect();
                // (Record::decode already refused a snapshot whose streams do not decode.)
                self.unswept = s
                    .unswept
                    .as_deref()
                    .and_then(item_stream::<FileRun>)
                    .map(|v| from_runs(&v))
                    .unwrap_or_default();
                self.packs = s
                    .segments
                    .as_deref()
                    .and_then(item_stream::<PackRef>)
                    .unwrap_or_default();
            }
            Record::Done(s) => self.finished = Some(*s),
        }
    }

    fn prune_packs(&mut self) {
        let unswept = &self.unswept;
        self.packs.retain(|p| {
            unswept
                .range(p.first_file..p.first_file.saturating_add(p.count))
                .next()
                .is_some()
        });
    }

    pub fn snapshot(&self) -> JnlSnapshot {
        JnlSnapshot {
            unswept: (!self.unswept.is_empty())
                .then(|| item_bytes(&runs(&self.unswept)))
                .flatten(),
            segments: (!self.packs.is_empty())
                .then(|| item_bytes(&self.packs))
                .flatten(),
            done: runs(&self.done),
            ranges: self
                .ranges
                .iter()
                .flat_map(|(f, r)| {
                    r.iter().map(move |(s, e)| crate::gen::FileRange {
                        file_id: *f,
                        offset: s,
                        len: e - s,
                    })
                })
                .collect(),
            roots: self
                .roots
                .iter()
                .map(|(f, r)| RootItem {
                    file_id: *f,
                    root: *r,
                })
                .collect(),
        }
    }

    /// The receiver's answer for a resume: done files, plus the durable ranges of the rest.
    pub fn need(&self) -> Need {
        Need {
            done: self.done.clone(),
            partial: self.ranges.clone().into_iter().collect(),
            held: 0,
        }
    }
}

/// The job directory name is the lowercase hex of the job id (the same bytes C's
/// `ava1_job_dir` produces).
pub fn job_dir(jobs_dir: &Path, job: &[u8; 16]) -> PathBuf {
    jobs_dir.join(crate::hex::encode(job))
}

pub fn write_manifest(dir: &Path, m: &Manifest) -> io::Result<()> {
    let mut w = Writer::new();
    let entries: Vec<ManifestEntry> = m
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| ManifestEntry {
            file_id: i as u32,
            kind: e.kind,
            mode: e.mode,
            size: e.size,
            mtime: e.mtime,
            path: e.path.clone(),
            root: e.root,
        })
        .collect();
    w.records(&entries)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let tmp = dir.join("manifest.tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(&w.buf[4..])?; // the item stream, without the records length prefix
        f.sync_all()?;
    }
    fs::rename(&tmp, dir.join("manifest"))?;
    sync_dir(dir)
}

pub fn read_manifest(dir: &Path) -> io::Result<Manifest> {
    let b = fs::read(dir.join("manifest"))?;
    let mut framed = (b.len() as u32).to_le_bytes().to_vec();
    framed.extend_from_slice(&b);
    let entries: Vec<ManifestEntry> = Reader::new(&framed)
        .records()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Manifest {
        entries: entries
            .into_iter()
            .map(|w| Entry {
                kind: w.kind,
                mode: w.mode,
                size: w.size,
                mtime: w.mtime,
                path: w.path,
                root: w.root,
            })
            .collect(),
    })
}

/// Removes job directories idle for more than `max_age_s` as seen from `now_unix`; returns how
/// many were removed. A directory whose mtime cannot be read is left alone — never deleted on a
/// guess.
pub fn gc(jobs_dir: &Path, now_unix: u64, max_age_s: u64) -> io::Result<usize> {
    gc_except(jobs_dir, now_unix, max_age_s, &|_| false)
}

/// `gc`, leaving alone every directory whose name `live` accepts (the job is running in this
/// process: its journal is never swept, however old its mtime). An error on one directory does
/// not stop the sweep of the rest; the first error is returned after it.
pub fn gc_except(
    jobs_dir: &Path,
    now_unix: u64,
    max_age_s: u64,
    live: &dyn Fn(&str) -> bool,
) -> io::Result<usize> {
    let mut n = 0;
    let mut first_err = None;
    let Ok(rd) = fs::read_dir(jobs_dir) else {
        return Ok(0);
    };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        if e.file_name().to_str().is_some_and(live) {
            continue;
        }
        let Some(last) = last_write(&p) else { continue };
        if now_unix.saturating_sub(last) > max_age_s {
            match fs::remove_dir_all(&p) {
                Ok(()) => n += 1,
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(n),
    }
}

/// The newest mtime of the directory itself and its journal, or `None` if neither is readable.
fn last_write(dir: &Path) -> Option<u64> {
    let mut newest: Option<u64> = None;
    for q in [dir.to_path_buf(), dir.join("journal")] {
        if let Ok(t) = fs::metadata(&q).and_then(|m| m.modified()) {
            if let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) {
                let s = d.as_secs();
                newest = Some(newest.map_or(s, |n| n.max(s)));
            }
        }
    }
    newest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{FileRange, FileRun, RootItem};
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ava1-jnl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_snapshot_with_undecodable_unswept_bytes_is_refused_not_read_as_all_swept() {
        // Failing open: an unswept list that does not decode must stop the replay (a torn record), never
        // become "nothing is unswept", which a compaction would then persist.
        let d = tmp("badsnap");
        let mut j = Journal::create(&d, &open_rec()).unwrap();
        let mut st = State::default();
        st.done.insert(3);
        st.unswept.insert(3);
        let mut snap = st.snapshot();
        assert!(snap.unswept.is_some());
        snap.unswept = Some(vec![9, 9, 9]); // not an item stream
        j.append(&Record::Snapshot(snap)).unwrap();
        drop(j);
        let (_, recs) = Journal::open(&d).unwrap();
        assert_eq!(recs.len(), 1, "only the Open survived: {recs:?}");
        // and the segments list the same
        let mut j = Journal::create(&d, &open_rec()).unwrap();
        let mut snap = st.snapshot();
        snap.segments = Some(vec![1, 0, 0, 0, 7]);
        j.append(&Record::Snapshot(snap)).unwrap();
        drop(j);
        assert_eq!(Journal::open(&d).unwrap().1.len(), 1);
    }

    #[test]
    fn a_hostile_run_length_is_refused_by_replay_and_bounded_everywhere() {
        let huge = vec![FileRun {
            first: 0,
            count: u32::MAX,
        }];
        let t = std::time::Instant::now();
        let set = crate::ranges::from_runs(&huge);
        assert!(set.len() as u64 <= crate::ranges::MAX_RUN_IDS);
        assert!(
            t.elapsed().as_secs() < 20,
            "from_runs spun on a hostile run"
        );
        let d = tmp("hugerun");
        let mut j = Journal::create(&d, &open_rec()).unwrap();
        j.append(&Record::Sweep(huge)).unwrap();
        drop(j);
        assert_eq!(
            Journal::open(&d).unwrap().1.len(),
            1,
            "the record is refused"
        );
    }

    fn pack_batch(files: Vec<FileRun>, segment: u32, offset: u64, len: u64) -> Record {
        Record::Batch(JnlBatch {
            files,
            ranges: vec![],
            roots: vec![],
            pack_segment: Some(segment),
            pack_offset: Some(offset),
            pack_len: Some(len),
        })
    }

    #[test]
    fn a_pack_batch_leaves_files_unswept_until_a_sweep_and_snapshots_keep_them() {
        // Durable-by-log (SPEC.md §15.7): replay = done, minus swept, plus the pack ranges that
        // still hold unswept files.
        let mut st = State::default();
        st.apply(&pack_batch(vec![FileRun { first: 1, count: 3 }], 0, 8, 400));
        st.apply(&pack_batch(vec![FileRun { first: 7, count: 1 }], 1, 8, 90));
        assert_eq!(st.unswept.iter().copied().collect::<Vec<_>>(), [1, 2, 3, 7]);
        assert_eq!(st.packs.len(), 2);
        assert!(st.done.contains(&2) && st.done.contains(&7));
        // a snapshot round trips both lists
        let snap = st.snapshot();
        let mut again = State::default();
        again.apply(&Record::Snapshot(snap));
        assert_eq!(again.unswept, st.unswept);
        assert_eq!(again.packs, st.packs);
        // a sweep of some files keeps the range that still has one; of all, drops it
        st.apply(&Record::Sweep(vec![FileRun { first: 1, count: 2 }]));
        assert_eq!(st.unswept.iter().copied().collect::<Vec<_>>(), [3, 7]);
        assert_eq!(st.packs.len(), 2);
        st.apply(&Record::Sweep(vec![FileRun { first: 3, count: 1 }]));
        assert_eq!(st.packs.len(), 1);
        assert_eq!(st.packs[0].segment, 1);
        // a reset takes a file out of both
        st.apply(&Record::Reset(7));
        assert!(st.unswept.is_empty() && st.packs.is_empty());
        assert!(!st.done.contains(&7));
    }

    #[test]
    fn the_sweep_record_and_the_pack_extension_survive_the_file() {
        let d = tmp("sweep");
        let mut j = Journal::create(&d, &open_rec()).unwrap();
        let b = pack_batch(vec![FileRun { first: 0, count: 2 }], 3, 8, 77);
        j.append(&b).unwrap();
        j.append(&Record::Sweep(vec![FileRun { first: 0, count: 2 }]))
            .unwrap();
        drop(j);
        let (_, recs) = Journal::open(&d).unwrap();
        assert_eq!(recs[1], b);
        assert_eq!(recs[2], Record::Sweep(vec![FileRun { first: 0, count: 2 }]));
    }

    fn open_rec() -> JnlOpen {
        JnlOpen {
            job_id: [1; 16],
            manifest_hash: [2; 32],
            kind: 1,
            flags: 0,
            staged: 1,
            root: "/data/x".into(),
        }
    }

    fn batch(f: u32, off: u64) -> Record {
        Record::Batch(JnlBatch {
            files: vec![FileRun { first: f, count: 1 }],
            ranges: vec![FileRange {
                file_id: 9,
                offset: off,
                len: 1 << 20,
            }],
            roots: vec![RootItem {
                file_id: 9,
                root: [off as u8; 32],
            }],
            pack_len: None,
            pack_offset: None,
            pack_segment: None,
        })
    }

    #[test]
    fn records_replay_in_order() {
        let d = tmp("order");
        let mut j = Journal::create(&d, &open_rec()).unwrap();
        j.append(&batch(0, 0)).unwrap();
        j.append(&Record::Reset(9)).unwrap(); // drops file 9's range and root
        j.append(&batch(1, 1 << 20)).unwrap();
        j.append(&Record::Done(0)).unwrap();
        drop(j);
        let (_, recs) = Journal::open(&d).unwrap();
        let mut st = State::default();
        for r in &recs {
            st.apply(r);
        }
        assert_eq!(st.open, Some(open_rec()));
        assert_eq!(st.done.iter().copied().collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(
            st.ranges[&9].iter().collect::<Vec<_>>(),
            vec![(1 << 20, 2 << 20)]
        );
        assert_eq!(st.finished, Some(0));
    }

    #[test]
    fn journal_torn_tail_is_ignored() {
        let d = tmp("torn");
        let mut j = Journal::create(&d, &open_rec()).unwrap();
        j.append(&batch(0, 0)).unwrap();
        j.append(&batch(1, 1 << 20)).unwrap();
        let good = j.len();
        j.append(&batch(2, 2 << 20)).unwrap();
        drop(j);
        let p = d.join("journal");
        let mut b = std::fs::read(&p).unwrap();
        b.truncate(b.len() - 3); // cuts into the third record
        b.extend_from_slice(&[0xff; 40]);
        std::fs::write(&p, &b).unwrap();
        let (mut j, recs) = Journal::open(&d).unwrap();
        assert_eq!(recs.len(), 3);
        assert_eq!(j.len(), good); // the torn tail was truncated away
        j.append(&batch(3, 3 << 20)).unwrap();
        drop(j);
        let (_, recs) = Journal::open(&d).unwrap();
        assert_eq!(recs.len(), 4);
        // One flipped CRC byte in the middle of the file stops replay at that record.
        let mut b = std::fs::read(&p).unwrap();
        let at = good as usize - 1;
        b[at] ^= 0x01;
        std::fs::write(&p, &b).unwrap();
        let (_, recs) = Journal::open(&d).unwrap();
        assert_eq!(recs.len(), 2);
    }

    #[test]
    fn compaction_keeps_the_state_and_shrinks_the_file() {
        let d = tmp("compact");
        let mut j = Journal::create(&d, &open_rec()).unwrap();
        let mut st = State::default();
        st.apply(&Record::Open(open_rec()));
        // A 400-run batch is ~5.6 KB on disk, so this genuinely crosses 1 MiB. (The plan's
        // 20,000 one-run batches only reach ~700 KB and never trigger compaction.)
        let mut i = 0u32;
        while j.len() <= COMPACT_AT {
            let files: Vec<FileRun> = (0..400)
                .map(|k| FileRun {
                    first: i * 400 + k,
                    count: 1,
                })
                .collect();
            let r = Record::Batch(JnlBatch {
                files,
                ..Default::default()
            });
            st.apply(&r);
            j.append(&r).unwrap();
            i += 1;
            assert!(i < 10_000, "the journal never crossed COMPACT_AT");
        }
        // A finished job compacts too: the snapshot carries no status, so if compact did
        // not write the Done record back the terminal state would vanish on replay.
        let done = Record::Done(7);
        st.apply(&done);
        j.append(&done).unwrap();
        // And the compared state holds a range and a root, so the equality below is not
        // vacuous about the snapshot's two records fields.
        let extra = Record::Batch(JnlBatch {
            ranges: vec![crate::gen::FileRange {
                file_id: 9,
                offset: 0,
                len: 1 << 20,
            }],
            roots: vec![RootItem {
                file_id: 9,
                root: [0x5a; 32],
            }],
            ..Default::default()
        });
        st.apply(&extra);
        j.append(&extra).unwrap();
        j.compact(&open_rec(), &st).unwrap();
        assert!(j.len() < 1024);
        drop(j);
        let (_, recs) = Journal::open(&d).unwrap();
        assert_eq!(recs.len(), 3); // open + snapshot + done
        let mut st2 = State::default();
        for r in &recs {
            st2.apply(r);
        }
        assert_eq!(st2, st, "a compaction must lose no state");
    }

    #[test]
    fn gc_removes_only_job_dirs_idle_for_seven_days() {
        let d = tmp("gc");
        let old = job_dir(&d, &[1; 16]);
        let new = job_dir(&d, &[2; 16]);
        Journal::create(&old, &open_rec()).unwrap();
        // mtimes have 1 s granularity: a 2.1 s gap makes `old` >= 2 s idle and `new` <= 1 s,
        // so a 1 s threshold separates them whatever the clock's fractional phase.
        std::thread::sleep(std::time::Duration::from_millis(2100));
        Journal::create(&new, &open_rec()).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(gc(&d, now, 7 * 86_400).unwrap(), 0); // both fresh
        assert_eq!(gc(&d, now, 1).unwrap(), 1); // only `old` is > 1 s idle
        assert!(!old.exists() && new.exists());
        assert_eq!(gc(&d, now + 8 * 86_400, 7 * 86_400).unwrap(), 1); // the 7-day path
        assert!(!new.exists());
    }
}
