//! Durable-by-log for small files, engine side (SPEC.md §15.7): the pack log a receiver appends
//! small files to so a batch costs one fsync of the log instead of one per file, and the
//! bookkeeping of which files are done but not yet durable in place (`unswept`).
//!
//! This module owns the log files and the queue; the caller (`LocalSink`, the receive loop)
//! writes the files themselves, fsyncs, journals (`JnlBatch` with the pack extension,
//! `JnlSweep`) and tells it what happened. It touches no journal.
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::crc32c::crc32c;
use crate::gen::BundleRecord;
use crate::journal::State;
use crate::wire::Message;

pub const MAGIC: &[u8; 8] = b"AVA1PCK1";
const HDR: u64 = 8;
const REC_MAX: u32 = 1 << 26;

/// Where one small file's record sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loc {
    pub id: u32,
    pub seg: u32,
    pub off: u64,
    /// The record's whole length on disk (frame included).
    pub len: u32,
}

/// The files of one batch whose records sit in one segment, with the byte range that holds
/// them: what the journal's pack extension names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedGroup {
    pub segment: u32,
    pub offset: u64,
    pub len: u64,
    pub files: Vec<Loc>,
}

impl LoggedGroup {
    pub fn ids(&self) -> BTreeSet<u32> {
        self.files.iter().map(|l| l.id).collect()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PackOpts {
    /// Bytes per segment; a record that would pass it starts the next one.
    pub segment: u64,
    /// Pack bytes of files not yet swept before the receiver sweeps instead of taking more.
    pub max_unswept: u64,
    /// A done file is swept once its batch is this old (every file when the job is ending).
    pub age: Duration,
}

impl Default for PackOpts {
    fn default() -> Self {
        Self {
            segment: 64 << 20,
            max_unswept: 256 << 20,
            age: Duration::from_secs(3),
        }
    }
}

struct Seg {
    f: Option<File>,
    tail: u64,
    nusw: u32,
    dirty: bool,
    closed: bool,
}

struct Usw {
    loc: Loc,
    at: Instant,
}

pub struct PackLog {
    dir: PathBuf,
    opts: PackOpts,
    segs: Vec<Seg>,
    /// Appended, not yet claimed by a batch.
    pending: Vec<Loc>,
    /// Done files waiting for the sweep, oldest first.
    usw: VecDeque<Usw>,
    /// Taken by a sweep that has not been journaled yet.
    sweeping: HashMap<u32, Vec<Loc>>,
    unswept_bytes: u64,
}

fn frame(body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(body.len() + 9);
    v.extend_from_slice(&((body.len() + 1) as u32).to_le_bytes());
    v.push(1);
    v.extend_from_slice(body);
    let mut c = vec![1u8];
    c.extend_from_slice(body);
    v.extend_from_slice(&crc32c(&c).to_le_bytes());
    v
}

/// The record at `off` of `f` (a whole frame of `len` bytes), checked: frame, CRC, decode.
fn read_frame(f: &File, off: u64, len: u32) -> io::Result<BundleRecord> {
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    if !(9..=REC_MAX).contains(&len) {
        return Err(bad("pack record length"));
    }
    let mut b = vec![0u8; len as usize];
    crate::verify::read_exact_at(f, &mut b, off)?;
    let body = u32::from_le_bytes(b[..4].try_into().unwrap()) as usize;
    if body + 8 != b.len() || b[4] != 1 {
        return Err(bad("pack record frame"));
    }
    let crc = u32::from_le_bytes(b[4 + body..].try_into().unwrap());
    if crc32c(&b[4..4 + body]) != crc {
        return Err(bad("pack record crc"));
    }
    BundleRecord::decode(&b[5..4 + body]).map_err(|e| bad(&e.to_string()))
}

fn pack_name(dir: &Path, seg: u32) -> PathBuf {
    dir.join(format!("pack.{seg}"))
}

impl PackLog {
    pub fn new(dir: &Path, opts: PackOpts) -> Self {
        Self {
            dir: dir.to_path_buf(),
            opts,
            segs: Vec::new(),
            pending: Vec::new(),
            usw: VecDeque::new(),
            sweeping: HashMap::new(),
            unswept_bytes: 0,
        }
    }

    pub fn opts(&self) -> PackOpts {
        self.opts
    }

    /// Files done but not yet swept (the sweep in flight included).
    pub fn unswept(&self) -> usize {
        self.usw.len() + self.sweeping.values().map(Vec::len).sum::<usize>()
    }

    /// Pack bytes of files not yet swept, pending ones included.
    pub fn unswept_bytes(&self) -> u64 {
        self.unswept_bytes
    }

    fn remove_if_free(&mut self, seg: u32) {
        let s = &mut self.segs[seg as usize];
        if s.closed && s.nusw == 0 && s.f.is_some() {
            s.f = None;
            let _ = std::fs::remove_file(pack_name(&self.dir, seg));
        }
    }

    /// Starts the next segment: the file with its magic, its directory entry synced so a
    /// journal record may name it.
    fn roll(&mut self) -> io::Result<()> {
        if let Some(last) = self.segs.last_mut() {
            last.closed = true;
        }
        if let Some(n) = self.segs.len().checked_sub(1) {
            self.remove_if_free(n as u32);
        }
        let n = self.segs.len() as u32;
        let p = pack_name(&self.dir, n);
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&p)?;
        crate::verify::write_all_at(&f, MAGIC, 0)?;
        #[cfg(unix)]
        File::open(&self.dir)?.sync_all()?;
        self.segs.push(Seg {
            f: Some(f),
            tail: HDR,
            nusw: 0,
            dirty: true,
            closed: false,
        });
        Ok(())
    }

    /// Appends one record; it counts against its segment and the unswept cap until swept (or
    /// dropped with `forget`).
    pub fn append(&mut self, rec: &BundleRecord) -> io::Result<Loc> {
        let body = rec
            .to_bytes()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let fr = frame(&body);
        let need_roll = match self.segs.last() {
            None => true,
            Some(s) => s.closed || (s.tail > HDR && s.tail + fr.len() as u64 > self.opts.segment),
        };
        if need_roll {
            self.roll()?;
        }
        let seg = (self.segs.len() - 1) as u32;
        let s = self.segs.last_mut().unwrap();
        let off = s.tail;
        crate::verify::write_all_at(s.f.as_ref().unwrap(), &fr, off)?;
        s.tail += fr.len() as u64;
        s.dirty = true;
        s.nusw += 1;
        self.unswept_bytes += fr.len() as u64;
        let loc = Loc {
            id: rec.file_id,
            seg,
            off,
            len: fr.len() as u32,
        };
        self.pending.push(loc);
        Ok(loc)
    }

    /// The file's own write failed after its record was appended: it will never be journaled.
    pub fn forget(&mut self, loc: Loc) {
        self.pending.retain(|l| *l != loc);
        self.unref(loc);
    }

    fn unref(&mut self, loc: Loc) {
        self.unswept_bytes = self.unswept_bytes.saturating_sub(loc.len as u64);
        if let Some(s) = self.segs.get_mut(loc.seg as usize) {
            s.nusw = s.nusw.saturating_sub(1);
        }
        if (loc.seg as usize) < self.segs.len() {
            self.remove_if_free(loc.seg);
        }
    }

    /// Claims the pending records of `small` for a batch: the dirty segments to fsync (cloned
    /// handles, so the fsync runs outside the caller's lock) and the groups to journal.
    /// Records of files not in `small` (their file write is still in flight) stay pending.
    pub fn take_batch(&mut self, small: &[u32]) -> io::Result<(Vec<File>, Vec<LoggedGroup>)> {
        let want: BTreeSet<u32> = small.iter().copied().collect();
        let (mine, rest): (Vec<Loc>, Vec<Loc>) =
            self.pending.drain(..).partition(|l| want.contains(&l.id));
        self.pending = rest;
        let mut fds = Vec::new();
        for s in &mut self.segs {
            if s.dirty {
                if let Some(f) = &s.f {
                    fds.push(f.try_clone()?);
                    s.dirty = false;
                }
            }
        }
        let mut mine = mine;
        mine.sort_by_key(|l| (l.seg, l.off));
        let mut groups: Vec<LoggedGroup> = Vec::new();
        for l in mine {
            match groups.last_mut() {
                Some(g) if g.segment == l.seg => {
                    g.len = l.off + l.len as u64 - g.offset;
                    g.files.push(l);
                }
                _ => groups.push(LoggedGroup {
                    segment: l.seg,
                    offset: l.off,
                    len: l.len as u64,
                    files: vec![l],
                }),
            }
        }
        Ok((fds, groups))
    }

    /// The batch's `JnlBatch` is durable: its files are done, and wait for the sweep.
    pub fn journaled(&mut self, groups: &[LoggedGroup]) {
        let now = Instant::now();
        for g in groups {
            for l in &g.files {
                self.usw.push_back(Usw { loc: *l, at: now });
            }
        }
    }

    /// Up to `max` files due for the sweep (all of them when `force`), moved to the in-flight set.
    pub fn due(&mut self, force: bool, max: usize) -> Vec<Loc> {
        let mut out = Vec::new();
        while out.len() < max {
            match self.usw.front() {
                Some(u) if force || u.at.elapsed() >= self.opts.age => {
                    let u = self.usw.pop_front().unwrap();
                    self.sweeping.entry(u.loc.id).or_default().push(u.loc);
                    out.push(u.loc);
                }
                _ => break,
            }
        }
        out
    }

    /// A sweep failed before it was journaled: its files go back to the front of the queue.
    pub fn due_failed(&mut self, locs: &[Loc]) {
        for l in locs.iter().rev() {
            let Some(v) = self.sweeping.get_mut(&l.id) else {
                continue;
            };
            if let Some(at) = v.iter().position(|x| x == l) {
                v.remove(at);
                self.usw.push_front(Usw {
                    loc: *l,
                    at: Instant::now() - self.opts.age,
                });
            }
            if v.is_empty() {
                self.sweeping.remove(&l.id);
            }
        }
    }

    /// The record of `loc`, checked.
    pub fn read(&self, loc: &Loc) -> io::Result<BundleRecord> {
        let f = self
            .segs
            .get(loc.seg as usize)
            .and_then(|s| s.f.as_ref())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "pack segment is gone"))?;
        read_frame(f, loc.off, loc.len)
    }

    /// The sweep of `ids` is journaled: they stop holding their segments (I3: a segment
    /// nobody holds goes).
    pub fn swept(&mut self, ids: &[u32]) {
        for id in ids {
            // every record of the file (a file sent again before its first record was swept has two)
            for l in self.sweeping.remove(id).unwrap_or_default() {
                self.unref(l);
            }
        }
    }

    /// Crash recovery (SPEC.md §15.7 "Recovery"): the done files `st` says are unswept are
    /// found again in the pack ranges it kept and handed to `remake`, which makes the file
    /// right in place (it returns whether the record was usable); each found file joins the
    /// sweep queue, due at once. Returns the files whose record could not be found.
    pub fn recover(
        &mut self,
        st: &State,
        mut remake: impl FnMut(&BundleRecord) -> io::Result<bool>,
    ) -> io::Result<Vec<u32>> {
        let mut found: BTreeSet<u32> = BTreeSet::new();
        for p in &st.packs {
            let Ok(f) = File::open(pack_name(&self.dir, p.segment)) else {
                continue;
            };
            let flen = f.metadata()?.len();
            let n = p.segment as usize;
            while self.segs.len() <= n {
                self.segs.push(Seg {
                    f: None,
                    tail: 0,
                    nusw: 0,
                    dirty: false,
                    closed: true,
                });
            }
            if self.segs[n].f.is_none() {
                self.segs[n] = Seg {
                    f: Some(f.try_clone()?),
                    tail: flen,
                    nusw: 0,
                    dirty: false,
                    closed: false,
                };
            }
            let end = (p.offset + p.len).min(flen);
            let mut pos = p.offset;
            while pos + 9 <= end {
                let mut hdr = [0u8; 4];
                if crate::verify::read_exact_at(&f, &mut hdr, pos).is_err() {
                    break;
                }
                let body = u32::from_le_bytes(hdr);
                if !(1..=REC_MAX).contains(&body) || pos + 8 + body as u64 > end {
                    break;
                }
                let len = body + 8;
                let Ok(rec) = read_frame(&f, pos, len) else {
                    break; // a torn tail ends the range
                };
                if st.unswept.contains(&rec.file_id)
                    && !found.contains(&rec.file_id)
                    && *blake3::hash(&rec.data).as_bytes() == rec.root
                    && remake(&rec)?
                {
                    found.insert(rec.file_id);
                    let loc = Loc {
                        id: rec.file_id,
                        seg: p.segment,
                        off: pos,
                        len,
                    };
                    self.segs[n].nusw += 1;
                    self.unswept_bytes += len as u64;
                    self.usw.push_back(Usw {
                        loc,
                        at: Instant::now() - self.opts.age,
                    });
                }
                pos += len as u64;
            }
        }
        Ok(st
            .unswept
            .iter()
            .copied()
            .filter(|i| !found.contains(i))
            .collect())
    }

    /// Nothing is unswept any more: every pack file in the job directory goes (a crashed run's
    /// stray segments included).
    pub fn cleanup(&mut self) {
        if self.unswept() != 0 || !self.pending.is_empty() {
            return;
        }
        for s in &mut self.segs {
            s.f = None;
        }
        self.segs.clear();
        self.unswept_bytes = 0;
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with("pack.") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: u32, data: &[u8]) -> BundleRecord {
        BundleRecord {
            file_id: id,
            root: *blake3::hash(data).as_bytes(),
            data: data.to_vec(),
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ava1-pack-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn records_round_trip_roll_and_are_deleted_once_swept() {
        let d = tmp("roll");
        let mut p = PackLog::new(
            &d,
            PackOpts {
                segment: 200,
                max_unswept: 1 << 20,
                age: Duration::ZERO,
            },
        );
        let mut locs = vec![];
        for i in 0..6u32 {
            locs.push(p.append(&rec(i, &[i as u8; 60])).unwrap());
        }
        assert!(locs.last().unwrap().seg >= 2, "segments rolled: {locs:?}");
        for l in &locs {
            assert_eq!(p.read(l).unwrap().data, vec![l.id as u8; 60]);
        }
        let small: Vec<u32> = (0..6).collect();
        let (fds, groups) = p.take_batch(&small).unwrap();
        assert!(!fds.is_empty());
        assert!(groups.len() >= 3, "one group per segment: {groups:?}");
        assert_eq!(
            groups.iter().map(|g| g.files.len()).sum::<usize>(),
            6,
            "every file is in a group"
        );
        p.journaled(&groups);
        let due = p.due(true, 100);
        assert_eq!(due.len(), 6);
        let ids: Vec<u32> = due.iter().map(|l| l.id).collect();
        p.swept(&ids);
        assert_eq!(p.unswept(), 0);
        assert_eq!(p.unswept_bytes(), 0);
        // closed, fully swept segments are gone; the open tail stays until cleanup
        let left: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().collect();
        assert!(left.len() <= 1, "{left:?}");
        p.cleanup();
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
    }

    #[test]
    fn a_file_logged_twice_releases_both_records_once_swept() {
        // a file reset and sent again before its first record was swept: two records, one id
        let d = tmp("dup");
        let mut p = PackLog::new(
            &d,
            PackOpts {
                segment: 4096,
                max_unswept: 1 << 20,
                age: Duration::ZERO,
            },
        );
        p.append(&rec(5, b"first")).unwrap();
        p.append(&rec(5, b"second")).unwrap();
        let (_, groups) = p.take_batch(&[5]).unwrap();
        assert_eq!(
            groups.iter().map(|g| g.files.len()).sum::<usize>(),
            2,
            "{groups:?}"
        );
        p.journaled(&groups);
        let due = p.due(true, 10);
        assert_eq!(due.len(), 2);
        p.swept(&[5]);
        assert_eq!(p.unswept(), 0, "an in-flight sweep entry was lost");
        assert_eq!(
            p.unswept_bytes(),
            0,
            "bytes stayed counted for a swept file"
        );
        p.cleanup();
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
    }

    #[test]
    fn a_torn_record_ends_recovery_and_its_file_is_reported_lost() {
        let d = tmp("torn");
        let mut p = PackLog::new(&d, PackOpts::default());
        let a = p.append(&rec(1, b"alpha")).unwrap();
        let b = p.append(&rec(2, b"bravo")).unwrap();
        let (_, groups) = p.take_batch(&[1, 2]).unwrap();
        assert_eq!(groups.len(), 1);
        let g = groups[0].clone();
        drop(p);
        // cut the last record
        let path = pack_name(&d, 0);
        let f = OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(b.off + b.len as u64 - 3).unwrap();
        let st = State {
            unswept: [1, 2].into_iter().collect(),
            packs: vec![crate::gen::PackRef {
                segment: g.segment,
                offset: g.offset,
                len: g.len,
                first_file: 1,
                count: 2,
            }],
            ..State::default()
        };
        let mut q = PackLog::new(&d, PackOpts::default());
        let mut remade = vec![];
        let lost = q
            .recover(&st, |r| {
                remade.push(r.file_id);
                Ok(true)
            })
            .unwrap();
        assert_eq!(remade, vec![1]);
        assert_eq!(lost, vec![2]);
        assert_eq!(q.unswept(), 1);
        let _ = a;
    }

    /// A one-segment pack of five records of different sizes: the file bytes, each record's
    /// `(id, start, end)` and the journal state that names the whole range (review 009 #2b).
    fn sweep_fixture(tag: &str) -> (Vec<u8>, Vec<(u32, u64, u64)>, State) {
        let d = tmp(tag);
        let mut p = PackLog::new(&d, PackOpts::default());
        let mut locs = vec![];
        for (i, n) in [5usize, 1, 40, 0, 17].into_iter().enumerate() {
            locs.push(p.append(&rec(i as u32 + 1, &vec![i as u8 + 1; n])).unwrap());
        }
        let ids: Vec<u32> = locs.iter().map(|l| l.id).collect();
        let (_, groups) = p.take_batch(&ids).unwrap();
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        let st = State {
            unswept: ids.iter().copied().collect(),
            packs: vec![crate::gen::PackRef {
                segment: g.segment,
                offset: g.offset,
                len: g.len,
                first_file: 1,
                count: ids.len() as u32,
            }],
            ..State::default()
        };
        drop(p);
        let bytes = std::fs::read(pack_name(&d, 0)).unwrap();
        let spans = locs
            .iter()
            .map(|l| (l.id, l.off, l.off + l.len as u64))
            .collect();
        (bytes, spans, st)
    }

    /// Recovers `bytes` as segment 0 in a fresh dir: which files were re-made, which were lost,
    /// and what the queue then holds.
    fn recover_bytes(tag: &str, bytes: &[u8], st: &State) -> (Vec<u32>, Vec<u32>, usize) {
        let d = tmp(tag);
        std::fs::write(pack_name(&d, 0), bytes).unwrap();
        let mut q = PackLog::new(&d, PackOpts::default());
        let mut remade = vec![];
        let lost = q
            .recover(st, |r| {
                assert_eq!(*blake3::hash(&r.data).as_bytes(), r.root, "a remade record");
                remade.push(r.file_id);
                Ok(true)
            })
            .unwrap_or_else(|e| panic!("recovery errored: {e}"));
        (remade, lost, q.unswept())
    }

    #[test]
    fn recovery_survives_a_truncation_at_every_byte_length() {
        let (bytes, spans, st) = sweep_fixture("sweep-cut-src");
        for l in 0..=bytes.len() {
            let (remade, lost, queued) = recover_bytes("sweep-cut", &bytes[..l], &st);
            let whole: Vec<u32> = spans
                .iter()
                .filter(|(_, _, e)| *e <= l as u64)
                .map(|(i, _, _)| *i)
                .collect();
            assert_eq!(remade, whole, "cut at {l}: only whole records are re-made");
            let gone: Vec<u32> = st
                .unswept
                .iter()
                .copied()
                .filter(|i| !whole.contains(i))
                .collect();
            assert_eq!(lost, gone, "cut at {l}: the rest are reported lost");
            assert_eq!(
                queued,
                whole.len(),
                "cut at {l}: found files join the sweep queue"
            );
        }
    }

    #[test]
    fn a_flipped_byte_in_the_last_pack_record_loses_that_file_only() {
        let (bytes, spans, st) = sweep_fixture("sweep-flip-src");
        let (last, start, end) = *spans.last().unwrap();
        for at in start..end {
            for mask in [0x01u8, 0x80, 0xff] {
                let mut b = bytes.clone();
                b[at as usize] ^= mask;
                let (remade, lost, _) = recover_bytes("sweep-flip", &b, &st);
                let want: Vec<u32> = spans[..spans.len() - 1].iter().map(|s| s.0).collect();
                assert_eq!(
                    remade, want,
                    "flip {mask:#x} at {at}: the damaged record is not re-made"
                );
                assert_eq!(lost, vec![last], "flip at {at}");
            }
        }
    }
}
