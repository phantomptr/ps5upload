//! A Stored (uncompressed) zip written directly, so a download into it can resume
//! (review 003 §5, P3 Task 15; SPEC.md §10).
//!
//! Every entry is `local header ‖ data ‖ data descriptor`, zip64 throughout, in manifest
//! order, then the empty files, then the central directory. Because Stored data is the
//! file's own bytes and the manifest carries every size up front, the archive's layout is
//! a pure function of the manifest: entry `i` starts at the sum of the entries before it.
//! That is what makes resume cheap and journal-free:
//!
//! - The receiver's journal already says which files are durable and how many bytes of the
//!   one in flight (`Sink::position`). Offsets are computed, not recorded.
//! - A finished entry's CRC-32 sits in its own data descriptor, which the sink reads back.
//! - The in-flight entry's running CRC-32 is rebuilt by reading its durable bytes back.
//! - The local header leaves size and CRC to the descriptor, so a cut entry holds nothing
//!   stale; the archive is truncated to `data_offset + durable bytes` and continues.
//!
//! The archive is built in a `.ava-part` sibling and renamed over the destination by
//! `finish`, so the final path never holds a partial archive.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ava1::gen;
use ava1::manifest::Manifest;
use ava1::ranges::RangeSet;
use ava1::recv::Sink;

use crate::download::{check_shape, invalid, zip_entry_name, zip_restart};

const LOCAL_SIG: u32 = 0x0403_4b50;
const DESC_SIG: u32 = 0x0807_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const EOCD64_SIG: u32 = 0x0606_4b50;
const LOC64_SIG: u32 = 0x0706_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;
/// Data descriptor (bit 3) and UTF-8 names (bit 11).
const FLAGS: u16 = 0x0808;
const LOCAL_EXTRA: usize = 20;
const DESC_LEN: u64 = 24;
const READBACK: usize = 1 << 20;

/// The DOS date and time of a unix mtime (UTC), clamped to what DOS can express; 0 is
/// 1980-01-01, the same "unknown" the zip crate writes.
fn dos_datetime(unix: u64) -> (u16, u16) {
    if unix == 0 {
        return (0, 33);
    }
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    if y < 1980 {
        return (0, 33);
    }
    let y = y.min(2107);
    let date = (((y - 1980) as u16) << 9) | ((m as u16) << 5) | d as u16;
    let time =
        ((secs / 3600) as u16) << 11 | (((secs / 60) % 60) as u16) << 5 | (secs % 60 / 2) as u16;
    (time, date)
}

/// One entry of the archive, as the layout places it.
#[derive(Clone, Debug)]
struct Slot {
    id: u32,
    name: String,
    size: u64,
    mode: u32,
    mtime: u64,
    hdr_off: u64,
    data_off: u64,
}

impl Slot {
    fn end(&self) -> u64 {
        self.data_off + self.size + DESC_LEN
    }

    fn header(&self) -> Vec<u8> {
        let (t, d) = dos_datetime(self.mtime);
        let mut h = Vec::with_capacity(30 + self.name.len() + LOCAL_EXTRA);
        h.extend_from_slice(&LOCAL_SIG.to_le_bytes());
        h.extend_from_slice(&45u16.to_le_bytes());
        h.extend_from_slice(&FLAGS.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // Stored
        h.extend_from_slice(&t.to_le_bytes());
        h.extend_from_slice(&d.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes()); // crc: in the descriptor
        h.extend_from_slice(&u32::MAX.to_le_bytes()); // sizes: zip64 extra, descriptor
        h.extend_from_slice(&u32::MAX.to_le_bytes());
        h.extend_from_slice(&(self.name.len() as u16).to_le_bytes());
        h.extend_from_slice(&(LOCAL_EXTRA as u16).to_le_bytes());
        h.extend_from_slice(self.name.as_bytes());
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(&0u64.to_le_bytes());
        h.extend_from_slice(&0u64.to_le_bytes());
        h
    }

    fn header_len(&self) -> u64 {
        (30 + self.name.len() + LOCAL_EXTRA) as u64
    }

    fn descriptor(&self, crc: u32) -> [u8; DESC_LEN as usize] {
        let mut d = [0u8; DESC_LEN as usize];
        d[0..4].copy_from_slice(&DESC_SIG.to_le_bytes());
        d[4..8].copy_from_slice(&crc.to_le_bytes());
        d[8..16].copy_from_slice(&self.size.to_le_bytes());
        d[16..24].copy_from_slice(&self.size.to_le_bytes());
        d
    }

    fn central(&self, crc: u32, out: &mut Vec<u8>) {
        let (t, d) = dos_datetime(self.mtime);
        out.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
        out.extend_from_slice(&((3u16 << 8) | 45).to_le_bytes()); // made by: unix, 4.5
        out.extend_from_slice(&45u16.to_le_bytes());
        out.extend_from_slice(&FLAGS.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&t.to_le_bytes());
        out.extend_from_slice(&d.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&u32::MAX.to_le_bytes());
        out.extend_from_slice(&u32::MAX.to_le_bytes());
        out.extend_from_slice(&(self.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&28u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment
        out.extend_from_slice(&0u16.to_le_bytes()); // disk
        out.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        let mode = (self.mode & 0o7777) | 0o100_000;
        out.extend_from_slice(&(mode << 16).to_le_bytes());
        out.extend_from_slice(&u32::MAX.to_le_bytes()); // offset: zip64 extra
        out.extend_from_slice(self.name.as_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&self.size.to_le_bytes());
        out.extend_from_slice(&self.size.to_le_bytes());
        out.extend_from_slice(&self.hdr_off.to_le_bytes());
    }
}

/// Where every entry sits: a pure function of the manifest and the naming rule.
#[derive(Debug)]
struct Layout {
    /// Non-empty files, in manifest order, with their offsets.
    slots: Vec<Slot>,
    /// Empty files: appended at `finish`, after every slot.
    empties: Vec<Slot>,
    by_id: HashMap<u32, usize>,
}

impl Layout {
    fn build(m: &Manifest, single: bool, base: &str) -> io::Result<Layout> {
        let mut slots = Vec::new();
        let mut empties = Vec::new();
        let mut off = 0u64;
        for (i, e) in m.entries.iter().enumerate() {
            if e.kind != gen::ENTRY_FILE {
                continue;
            }
            let name = zip_entry_name(single, base, &e.path);
            if name.len() > u16::MAX as usize {
                return Err(invalid(format!("{:?} is too long for a zip entry", e.path)));
            }
            let mut s = Slot {
                id: i as u32,
                name,
                size: e.size,
                mode: e.mode,
                mtime: e.mtime,
                hdr_off: 0,
                data_off: 0,
            };
            if e.size == 0 {
                empties.push(s);
            } else {
                s.hdr_off = off;
                s.data_off = off + s.header_len();
                off = s.end();
                slots.push(s);
            }
        }
        let by_id = slots.iter().enumerate().map(|(k, s)| (s.id, k)).collect();
        Ok(Layout {
            slots,
            empties,
            by_id,
        })
    }

    /// Where the non-empty entries end: the offset an empty-file entry or the central
    /// directory starts at.
    fn end(&self) -> u64 {
        self.slots.last().map_or(0, Slot::end)
    }
}

#[cfg(unix)]
fn read_exact_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, off)
}

#[cfg(unix)]
fn write_all_at(f: &File, buf: &[u8], off: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(f, buf, off)
}

#[cfg(windows)]
fn read_exact_at(f: &File, mut buf: &mut [u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_read(buf, off)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf = &mut buf[n..];
        off += n as u64;
    }
    Ok(())
}

#[cfg(windows)]
fn write_all_at(f: &File, mut buf: &[u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_write(buf, off)?;
        buf = &buf[n..];
        off += n as u64;
    }
    Ok(())
}

/// Everything after the non-empty entries: the empty files' entries, the central directory,
/// the zip64 end record, its locator and the classic end record (whose fields are all
/// sentinels, so a reader must take the zip64 values). `at` is where the tail starts.
fn tail_bytes(layout: &Layout, crcs: &[u32], at: u64) -> Vec<u8> {
    let mut tail: Vec<u8> = Vec::new();
    let mut at = at;
    let mut empties = layout.empties.clone();
    for s in &mut empties {
        s.hdr_off = at;
        s.data_off = at + s.header_len();
        tail.extend_from_slice(&s.header());
        tail.extend_from_slice(&s.descriptor(0));
        at = s.end();
    }
    let cd_off = at;
    let mut cd = Vec::new();
    for (s, crc) in layout.slots.iter().zip(crcs) {
        s.central(*crc, &mut cd);
    }
    for s in &empties {
        s.central(0, &mut cd);
    }
    let n = (layout.slots.len() + empties.len()) as u64;
    let eocd64_off = cd_off + cd.len() as u64;
    tail.extend_from_slice(&cd);
    tail.extend_from_slice(&EOCD64_SIG.to_le_bytes());
    tail.extend_from_slice(&44u64.to_le_bytes());
    tail.extend_from_slice(&((3u16 << 8) | 45).to_le_bytes());
    tail.extend_from_slice(&45u16.to_le_bytes());
    tail.extend_from_slice(&0u32.to_le_bytes());
    tail.extend_from_slice(&0u32.to_le_bytes());
    tail.extend_from_slice(&n.to_le_bytes());
    tail.extend_from_slice(&n.to_le_bytes());
    tail.extend_from_slice(&(cd.len() as u64).to_le_bytes());
    tail.extend_from_slice(&cd_off.to_le_bytes());
    tail.extend_from_slice(&LOC64_SIG.to_le_bytes());
    tail.extend_from_slice(&0u32.to_le_bytes());
    tail.extend_from_slice(&eocd64_off.to_le_bytes());
    tail.extend_from_slice(&1u32.to_le_bytes());
    tail.extend_from_slice(&EOCD_SIG.to_le_bytes());
    tail.extend_from_slice(&0u16.to_le_bytes());
    tail.extend_from_slice(&0u16.to_le_bytes());
    tail.extend_from_slice(&u16::MAX.to_le_bytes());
    tail.extend_from_slice(&u16::MAX.to_le_bytes());
    tail.extend_from_slice(&u32::MAX.to_le_bytes());
    tail.extend_from_slice(&u32::MAX.to_le_bytes());
    tail.extend_from_slice(&0u16.to_le_bytes());
    tail
}

struct State {
    m: Option<Arc<Manifest>>,
    layout: Arc<Layout>,
    file: Option<Arc<File>>,
    /// The next byte to write. Only meaningful once `positioned`.
    pos: u64,
    /// `prepare` leaves the part file as it found it (a resume reads it); the first write
    /// of a job nobody positioned starts the archive over.
    positioned: bool,
    /// The slot being written, how much of it is in the archive, and its running CRC-32.
    current: Option<usize>,
    written: u64,
    crc: crc32fast::Hasher,
    /// Finished slots' CRC-32s (from this run's writes, or the descriptors on resume).
    crcs: Vec<Option<u32>>,
    /// Highest file id started: ordered delivery never goes back.
    last: Option<u32>,
    started: usize,
    finished: bool,
}

/// An ordered download appended into a Stored `.zip` that resumes (see the module docs).
pub struct StoredZipSink {
    dest: PathBuf,
    part: PathBuf,
    single: bool,
    base: String,
    st: Mutex<State>,
}

impl StoredZipSink {
    /// A folder download: entries are `<prefix>/<root-relative path>`.
    pub fn new(path: PathBuf, prefix: &str) -> Self {
        Self::build(path, prefix, false)
    }

    /// A single-file download: the one entry is exactly `name`.
    pub fn single(path: PathBuf, name: &str) -> Self {
        Self::build(path, name, true)
    }

    fn build(dest: PathBuf, base: &str, single: bool) -> Self {
        let mut part = dest.clone().into_os_string();
        part.push(".ava-part");
        Self {
            dest,
            part: PathBuf::from(part),
            single,
            base: base.to_owned(),
            st: Mutex::new(State {
                m: None,
                layout: Arc::new(Layout {
                    slots: Vec::new(),
                    empties: Vec::new(),
                    by_id: HashMap::new(),
                }),
                file: None,
                pos: 0,
                positioned: false,
                current: None,
                written: 0,
                crc: crc32fast::Hasher::new(),
                crcs: Vec::new(),
                last: None,
                started: 0,
                finished: false,
            }),
        }
    }

    /// The archive's layout for `m`: `(file id, local header offset, data offset, size)`
    /// of every non-empty file. What a resume recomputes instead of journaling.
    pub fn layout_of(&self, m: &Manifest) -> io::Result<Vec<(u32, u64, u64, u64)>> {
        let l = Layout::build(m, self.single, &self.base)?;
        Ok(l.slots
            .iter()
            .map(|s| (s.id, s.hdr_off, s.data_off, s.size))
            .collect())
    }

    /// Writes the descriptor of slot `k` (the current file, every byte written) with its CRC
    /// and closes it. Shared by `commit` and by `append` when the next file arrives first.
    fn finish_current(st: &mut State, k: usize) -> io::Result<()> {
        let layout = st.layout.clone();
        let slot = &layout.slots[k];
        let file = Self::file(st)?;
        let crc = st.crc.clone().finalize();
        write_all_at(&file, &slot.descriptor(crc), st.pos)?;
        st.pos += DESC_LEN;
        st.crcs[k] = Some(crc);
        st.current = None;
        Ok(())
    }

    fn file(st: &State) -> io::Result<Arc<File>> {
        st.file.clone().ok_or_else(|| invalid("archive not open"))
    }

    fn start_over(st: &mut State) -> io::Result<()> {
        let f = Self::file(st)?;
        f.set_len(0)?;
        st.pos = 0;
        st.positioned = true;
        st.current = None;
        st.written = 0;
        st.last = None;
        st.started = 0;
        st.crcs = vec![None; st.layout.slots.len()];
        Ok(())
    }

    fn append(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
        let mut st = self.st.lock().unwrap();
        let layout = st.layout.clone();
        let Some(m) = st.m.clone() else {
            return Err(invalid("data before the manifest"));
        };
        let e = m
            .entry(id)
            .filter(|e| e.kind == gen::ENTRY_FILE)
            .ok_or_else(|| invalid(format!("data for {id}, which is not a file")))?;
        if e.size == 0 {
            // Written at `finish`; the receiver's up-front empty write lands here.
            return if data.is_empty() {
                Ok(())
            } else {
                Err(invalid(format!("data for empty file {id}")))
            };
        }
        if !st.positioned {
            Self::start_over(&mut st)?;
        }
        let k = layout.by_id[&id];
        let slot = &layout.slots[k];
        let file = Self::file(&st)?;
        if st.current != Some(k) {
            if st.last.is_some_and(|l| id <= l) {
                // From the start of a file already written: a retry, not a violation.
                return Err(if off == 0 {
                    zip_restart(format!("file {id} was sent again"))
                } else {
                    invalid(format!("file {id} arrived out of order"))
                });
            }
            if let Some(cur) = st.current {
                let s = &layout.slots[cur];
                if st.written != s.size {
                    return Err(invalid(format!(
                        "file {} ended at {} of {} bytes",
                        s.id, st.written, s.size
                    )));
                }
                // Whole but not finished: a resume took it as in flight with every byte
                // durable, and its `commit` has not come yet when the next file's data does
                // (the order the receiver reaches them in). Finish it here, as `commit` would.
                Self::finish_current(&mut st, cur)?;
            }
            if off != 0 {
                return Err(invalid(format!(
                    "file {id} starts at offset {off}: a gap in the archive stream"
                )));
            }
            if st.pos != slot.hdr_off {
                return Err(invalid(format!(
                    "file {id} would start at {} but the layout puts it at {}",
                    st.pos, slot.hdr_off
                )));
            }
            write_all_at(&file, &slot.header(), st.pos)?;
            st.pos += slot.header_len();
            st.current = Some(k);
            st.last = Some(id);
            st.written = 0;
            st.crc = crc32fast::Hasher::new();
            st.started += 1;
        } else if off == 0 && st.written > 0 {
            // The current file again from its first byte (a retry after a mismatch).
            return Err(zip_restart(format!("file {id} was sent again")));
        } else if off != st.written {
            // A gap or an overlap (a duplicate range): never append it.
            return Err(invalid(format!(
                "file {id} wrote at offset {off} but the archive is at {}",
                st.written
            )));
        }
        if st.written + data.len() as u64 > slot.size {
            return Err(invalid(format!("file {id} wrote past its size")));
        }
        write_all_at(&file, data, st.pos)?;
        st.pos += data.len() as u64;
        st.crc.update(data);
        st.written += data.len() as u64;
        if st.written == slot.size {
            let crc = st.crc.clone().finalize();
            write_all_at(&file, &slot.descriptor(crc), st.pos)?;
            st.pos += DESC_LEN;
            st.crcs[k] = Some(crc);
            st.current = None;
        }
        Ok(())
    }
}

impl Drop for StoredZipSink {
    fn drop(&mut self) {
        let finished = self.st.lock().map(|s| s.finished).unwrap_or(false);
        if !finished {
            let _ = std::fs::remove_file(&self.part);
        }
    }
}

impl Sink for StoredZipSink {
    /// Opens the part file WITHOUT truncating it: a resume reads it (`read_at`, then
    /// `position`). A job nobody positioned starts the archive over on its first write.
    fn prepare(&self, m: &Manifest) -> io::Result<()> {
        check_shape(m, self.single)?;
        let layout = Layout::build(m, self.single, &self.base)?;
        let f = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.part)?;
        let mut st = self.st.lock().unwrap();
        st.m = Some(Arc::new(m.clone()));
        st.crcs = vec![None; layout.slots.len()];
        st.layout = Arc::new(layout);
        st.file = Some(Arc::new(f));
        st.pos = 0;
        st.positioned = false;
        st.current = None;
        st.written = 0;
        st.last = None;
        st.started = 0;
        Ok(())
    }

    fn write_at(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
        self.append(id, off, data)
    }

    fn write_whole(&self, id: u32, data: &[u8]) -> io::Result<()> {
        self.append(id, 0, data)
    }

    /// One fsync of the archive covers every file of the batch (and the bytes the loop has
    /// written since): the journal's record that follows is never ahead of the disk.
    fn sync(&self, _ids: &[u32]) -> io::Result<()> {
        let f = {
            let st = self.st.lock().unwrap();
            Self::file(&st)?
        };
        f.sync_data()
    }

    fn read_at(&self, id: u32, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let (f, slot) = {
            let st = self.st.lock().unwrap();
            let k = st
                .layout
                .by_id
                .get(&id)
                .copied()
                .ok_or_else(|| io::Error::from(io::ErrorKind::Unsupported))?;
            (Self::file(&st)?, st.layout.slots[k].clone())
        };
        if off + buf.len() as u64 > slot.size {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        read_exact_at(&f, buf, slot.data_off + off)?;
        Ok(buf.len())
    }

    /// Finishes a file that was whole but unfinished at a resume: writes its descriptor
    /// with the rebuilt CRC. Any other commit is a no-op (`append` finishes a file itself).
    fn commit(&self, id: u32) -> io::Result<()> {
        let mut st = self.st.lock().unwrap();
        let layout = st.layout.clone();
        let Some(&k) = layout.by_id.get(&id) else {
            return Ok(());
        };
        let slot = &layout.slots[k];
        if st.current == Some(k) && st.written == slot.size {
            Self::finish_current(&mut st, k)?;
        }
        Ok(())
    }

    /// Cuts the archive back to what the journal holds (see the module docs). Verifies every
    /// finished entry's header and descriptor and the in-flight entry's header; any
    /// disagreement is an `Err`, which makes the receiver start the job over.
    fn position(&self, done: &BTreeSet<u32>, partial: &BTreeMap<u32, RangeSet>) -> io::Result<()> {
        let mut st = self.st.lock().unwrap();
        let layout = st.layout.clone();
        let file = Self::file(&st)?;
        if done.is_empty() && partial.is_empty() {
            return Self::start_over(&mut st);
        }
        let bad = |why: String| io::Error::new(io::ErrorKind::InvalidData, why);
        // Files are finished in manifest order, so the finished non-empty files are a prefix.
        let k = layout
            .slots
            .iter()
            .take_while(|s| done.contains(&s.id))
            .count();
        let finished_nonempty = done.iter().filter(|i| layout.by_id.contains_key(i)).count();
        if finished_nonempty != k {
            return Err(bad(format!(
                "{finished_nonempty} finished files are not the first {k} of the archive"
            )));
        }
        let mut cut = layout.slots[..k].last().map_or(0, Slot::end);
        let mut partial_bytes = 0u64;
        let mut in_flight = None;
        // The journal can hold the bytes of several files whose `Done` it has not recorded yet
        // (a file's `Done` waits on its verification, and the next file's bytes keep arriving
        // and syncing meanwhile): every such file but the last is whole, the last is a prefix.
        // They are contiguous after the finished files, in archive order.
        let mut parts: Vec<(usize, u32, &RangeSet)> = partial
            .iter()
            .filter_map(|(id, rs)| layout.by_id.get(id).map(|&pk| (pk, *id, rs)))
            .collect();
        parts.sort_by_key(|p| p.0);
        let mut whole: Vec<usize> = Vec::new();
        let mut need = cut;
        for (j, (pk, id, rs)) in parts.iter().enumerate() {
            if *pk != k + j {
                return Err(bad(format!("file {id} is partly durable out of order")));
            }
            let runs: Vec<(u64, u64)> = rs.iter().collect();
            let [(0, x)] = runs[..] else {
                return Err(bad(format!("file {id}'s durable bytes are not a prefix")));
            };
            let slot = &layout.slots[*pk];
            if x > slot.size {
                return Err(bad(format!("file {id} is longer than its size")));
            }
            if j + 1 < parts.len() {
                if x != slot.size {
                    return Err(bad(format!(
                        "file {id} is partly durable but not the last in flight"
                    )));
                }
                whole.push(*pk);
                need = slot.data_off + slot.size;
            } else {
                // x == size: every byte is durable but the Done is not; the descriptor was
                // never written (or is cut), and `commit` writes it (review 005 section 5).
                partial_bytes = x;
                cut = slot.data_off + x;
                need = cut;
                in_flight = Some(*pk);
            }
        }
        if in_flight.is_none() {
            if let Some(&last) = whole.last() {
                cut = layout.slots[last].end();
            }
        }
        if file.metadata()?.len() < need {
            return Err(bad(format!(
                "the archive is {} bytes; the journal needs {need}",
                file.metadata()?.len()
            )));
        }
        let mut crcs: Vec<Option<u32>> = vec![None; layout.slots.len()];
        for (i, s) in layout.slots[..k].iter().enumerate() {
            let want = s.header();
            let mut got = vec![0u8; want.len()];
            read_exact_at(&file, &mut got, s.hdr_off)?;
            let mut desc = [0u8; DESC_LEN as usize];
            read_exact_at(&file, &mut desc, s.end() - DESC_LEN)?;
            let crc = u32::from_le_bytes(desc[4..8].try_into().unwrap());
            if got != want || desc != s.descriptor(crc) {
                return Err(bad(format!(
                    "entry {} is not what the archive recorded",
                    s.id
                )));
            }
            crcs[i] = Some(crc);
        }
        // Whole files without a `Done`: verify the header, rebuild the CRC from the durable
        // bytes and write the descriptor now (the receiver's `commit` for them is then a no-op).
        for &wk in &whole {
            let s = &layout.slots[wk];
            let want = s.header();
            let mut got = vec![0u8; want.len()];
            read_exact_at(&file, &mut got, s.hdr_off)?;
            if got != want {
                return Err(bad(format!("entry {} has a different header", s.id)));
            }
            let mut crc = crc32fast::Hasher::new();
            let mut buf = vec![0u8; READBACK];
            let mut at = 0u64;
            while at < s.size {
                let n = ((s.size - at) as usize).min(READBACK);
                read_exact_at(&file, &mut buf[..n], s.data_off + at)?;
                crc.update(&buf[..n]);
                at += n as u64;
            }
            let crc = crc.finalize();
            write_all_at(&file, &s.descriptor(crc), s.data_off + s.size)?;
            crcs[wk] = Some(crc);
        }
        let mut crc = crc32fast::Hasher::new();
        if let Some(pk) = in_flight {
            let s = &layout.slots[pk];
            let want = s.header();
            let mut got = vec![0u8; want.len()];
            read_exact_at(&file, &mut got, s.hdr_off)?;
            if got != want {
                return Err(bad(format!("entry {} has a different header", s.id)));
            }
            let mut buf = vec![0u8; READBACK];
            let mut at = 0u64;
            while at < partial_bytes {
                let n = ((partial_bytes - at) as usize).min(READBACK);
                read_exact_at(&file, &mut buf[..n], s.data_off + at)?;
                crc.update(&buf[..n]);
                at += n as u64;
            }
        }
        file.set_len(cut)?;
        st.pos = cut;
        st.positioned = true;
        st.current = in_flight;
        st.written = partial_bytes;
        st.crc = crc;
        st.crcs = crcs;
        let finished = k + whole.len();
        st.last = match in_flight {
            Some(pk) => Some(layout.slots[pk].id),
            None => finished.checked_sub(1).map(|i| layout.slots[i].id),
        };
        st.started = finished + usize::from(in_flight.is_some());
        Ok(())
    }

    fn finish(&self) -> io::Result<()> {
        let mut st = self.st.lock().unwrap();
        let layout = st.layout.clone();
        if st.m.is_none() {
            return Err(invalid("finished before the manifest"));
        }
        if let Some(cur) = st.current {
            let s = &layout.slots[cur];
            return Err(invalid(format!(
                "file {} ended at {} of {}",
                s.id, st.written, s.size
            )));
        }
        if st.started != layout.slots.len() || st.crcs.iter().any(Option::is_none) {
            return Err(invalid(format!(
                "the archive holds {} of {} files",
                st.started,
                layout.slots.len()
            )));
        }
        let file = Self::file(&st)?;
        if !st.positioned {
            Self::start_over(&mut st)?; // an archive of empty files only
        }
        debug_assert_eq!(st.pos, layout.end());
        let crcs: Vec<u32> = st.crcs.iter().map(|c| c.expect("checked above")).collect();
        let tail = tail_bytes(&layout, &crcs, st.pos);
        let pos = st.pos;
        write_all_at(&file, &tail, pos)?;
        file.set_len(pos + tail.len() as u64)?;
        file.sync_all()?;
        std::fs::rename(&self.part, &self.dest)?; // same directory by construction
        st.finished = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ava1::manifest::Entry;
    use std::io::Read;

    fn file_entry(path: &str, size: u64) -> Entry {
        Entry {
            kind: gen::ENTRY_FILE,
            mode: 0o644,
            size,
            mtime: 1_700_000_000,
            path: path.into(),
            root: None,
        }
    }

    fn data(id: u32, n: usize) -> Vec<u8> {
        (0..n).map(|i| (i as u32 * 31 + id * 7) as u8).collect()
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("p5a-zs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn manifest() -> Manifest {
        Manifest {
            entries: vec![
                file_entry("a", 3000),
                file_entry("e", 0),
                file_entry("b/c", 5000),
                file_entry("d", 1234),
            ],
        }
    }

    fn check_zip(path: &std::path::Path, m: &Manifest) {
        let mut z = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        assert_eq!(z.len(), m.entries.len());
        for (i, e) in m.entries.iter().enumerate() {
            let mut f = z.by_name(&format!("P/{}", e.path)).unwrap();
            assert_eq!(f.compression(), zip::CompressionMethod::Stored);
            let mut got = Vec::new();
            f.read_to_end(&mut got).unwrap();
            assert_eq!(got, data(i as u32, e.size as usize), "{}", e.path);
        }
    }

    fn feed(s: &StoredZipSink, m: &Manifest, id: u32, from: u64, to: u64) {
        let d = data(id, m.entry(id).unwrap().size as usize);
        s.write_at(id, from, &d[from as usize..to as usize])
            .unwrap();
    }

    #[test]
    fn a_stored_archive_opens_with_the_zip_crate() {
        let d = dir("plain");
        let m = manifest();
        let s = StoredZipSink::new(d.join("o.zip"), "P");
        s.prepare(&m).unwrap();
        s.position(&BTreeSet::new(), &BTreeMap::new()).unwrap();
        s.write_whole(1, &[]).unwrap();
        feed(&s, &m, 0, 0, 3000);
        feed(&s, &m, 2, 0, 5000);
        feed(&s, &m, 3, 0, 1234);
        s.finish().unwrap();
        check_zip(&d.join("o.zip"), &m);
        assert!(!d.join("o.zip.ava-part").exists());
    }

    #[test]
    fn the_layout_matches_the_bytes_the_writer_lays_down() {
        // "ZipEntry replay": the offsets a resume recomputes equal the offsets the writer
        // used, and the archive's size before its tail is the last entry's end.
        let d = dir("layout");
        let m = manifest();
        let s = StoredZipSink::new(d.join("o.zip"), "P");
        s.prepare(&m).unwrap();
        s.position(&BTreeSet::new(), &BTreeMap::new()).unwrap();
        for id in [0, 2, 3] {
            feed(&s, &m, id, 0, m.entry(id).unwrap().size);
        }
        let lay = s.layout_of(&m).unwrap();
        let on_disk = std::fs::metadata(d.join("o.zip.ava-part")).unwrap().len();
        let (_, _, data_off, size) = *lay.last().unwrap();
        assert_eq!(on_disk, data_off + size + DESC_LEN);
        // Each entry's header is at its recorded offset, followed by its data.
        let raw = std::fs::read(d.join("o.zip.ava-part")).unwrap();
        for (id, hdr, doff, size) in lay {
            assert_eq!(
                &raw[hdr as usize..hdr as usize + 4],
                &LOCAL_SIG.to_le_bytes()
            );
            assert_eq!(
                &raw[doff as usize..(doff + size) as usize],
                &data(id, size as usize)[..]
            );
        }
    }

    fn done(ids: &[u32]) -> BTreeSet<u32> {
        ids.iter().copied().collect()
    }

    fn part(id: u32, x: u64) -> BTreeMap<u32, RangeSet> {
        let mut r = RangeSet::new();
        r.insert(0, x);
        BTreeMap::from([(id, r)])
    }

    #[test]
    fn resume_at_an_entry_boundary_keeps_the_finished_entries() {
        let d = dir("entry");
        let m = manifest();
        {
            let s = StoredZipSink::new(d.join("o.zip"), "P");
            s.prepare(&m).unwrap();
            s.position(&BTreeSet::new(), &BTreeMap::new()).unwrap();
            feed(&s, &m, 0, 0, 3000);
            feed(&s, &m, 2, 0, 2000); // in flight when the connection dies
            s.sync(&[]).unwrap();
            std::mem::forget(s); // a dead process leaves the part file
        }
        let s = StoredZipSink::new(d.join("o.zip"), "P");
        s.prepare(&m).unwrap();
        // The journal knows file 0 only: entry 2's bytes are cut.
        s.position(&done(&[0]), &BTreeMap::new()).unwrap();
        let lay = s.layout_of(&m).unwrap();
        assert_eq!(
            std::fs::metadata(d.join("o.zip.ava-part")).unwrap().len(),
            lay[0].2 + lay[0].3 + DESC_LEN
        );
        s.write_whole(1, &[]).unwrap();
        feed(&s, &m, 2, 0, 5000);
        feed(&s, &m, 3, 0, 1234);
        s.finish().unwrap();
        check_zip(&d.join("o.zip"), &m);
    }

    #[test]
    fn resume_mid_entry_continues_the_crc_and_the_bytes() {
        let d = dir("mid");
        let m = manifest();
        {
            let s = StoredZipSink::new(d.join("o.zip"), "P");
            s.prepare(&m).unwrap();
            s.position(&BTreeSet::new(), &BTreeMap::new()).unwrap();
            feed(&s, &m, 0, 0, 3000);
            feed(&s, &m, 2, 0, 3000);
            s.sync(&[]).unwrap();
            std::mem::forget(s);
        }
        let s = StoredZipSink::new(d.join("o.zip"), "P");
        s.prepare(&m).unwrap();
        // 1024 of entry 2 is durable (the journal is behind the 3000 on disk).
        s.position(&done(&[0]), &part(2, 1024)).unwrap();
        s.write_whole(1, &[]).unwrap();
        assert!(
            s.write_at(2, 0, &[0]).is_err(),
            "restarting a resumed entry is a retry, not a write"
        );
        feed(&s, &m, 2, 1024, 5000);
        feed(&s, &m, 3, 0, 1234);
        s.finish().unwrap();
        check_zip(&d.join("o.zip"), &m);
    }

    #[test]
    fn a_resume_the_archive_cannot_honour_is_an_error() {
        let d = dir("short");
        let m = manifest();
        let s = StoredZipSink::new(d.join("o.zip"), "P");
        s.prepare(&m).unwrap();
        // The journal claims file 0 is done but the archive is empty.
        assert!(s.position(&done(&[0]), &BTreeMap::new()).is_err());
        // Out of order, and a non-prefix partial.
        assert!(s.position(&done(&[2]), &BTreeMap::new()).is_err());
        assert!(s.position(&BTreeSet::new(), &part(2, 100)).is_err());
        // Nothing durable always works (and starts the archive over).
        s.position(&BTreeSet::new(), &BTreeMap::new()).unwrap();
    }

    #[test]
    fn a_bit_rotted_finished_entry_is_an_error() {
        let d = dir("rot");
        let m = manifest();
        {
            let s = StoredZipSink::new(d.join("o.zip"), "P");
            s.prepare(&m).unwrap();
            s.position(&BTreeSet::new(), &BTreeMap::new()).unwrap();
            feed(&s, &m, 0, 0, 3000);
            s.sync(&[]).unwrap();
            std::mem::forget(s);
        }
        let p = d.join("o.zip.ava-part");
        let mut raw = std::fs::read(&p).unwrap();
        let n = raw.len();
        raw[n - 12] ^= 0xff; // the descriptor's size field
        std::fs::write(&p, raw).unwrap();
        let s = StoredZipSink::new(d.join("o.zip"), "P");
        s.prepare(&m).unwrap();
        assert!(s.position(&done(&[0]), &BTreeMap::new()).is_err());
    }

    fn u16at(b: &[u8], o: usize) -> u16 {
        u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
    }
    fn u32at(b: &[u8], o: usize) -> u32 {
        u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
    }
    fn u64at(b: &[u8], o: usize) -> u64 {
        u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
    }

    /// Checks the last 22 + 20 bytes of an archive tail: the classic end record is all
    /// sentinels and the locator points at a zip64 end record holding the real values.
    /// `base` is the archive offset of `tail[0]`. Returns (entries, cd offset, cd size).
    fn check_end_records(tail: &[u8], base: u64) -> (u64, u64, u64) {
        let n = tail.len();
        let e = &tail[n - 22..];
        assert_eq!(u32at(e, 0), EOCD_SIG);
        assert_eq!((u16at(e, 4), u16at(e, 6)), (0, 0), "disk numbers");
        assert_eq!(
            (u16at(e, 8), u16at(e, 10)),
            (0xFFFF, 0xFFFF),
            "entry count sentinels"
        );
        assert_eq!(
            (u32at(e, 12), u32at(e, 16)),
            (u32::MAX, u32::MAX),
            "size/offset sentinels"
        );
        assert_eq!(u16at(e, 20), 0, "no comment");
        let l = &tail[n - 42..n - 22];
        assert_eq!(u32at(l, 0), LOC64_SIG);
        assert_eq!(u32at(l, 4), 0);
        assert_eq!(u32at(l, 16), 1, "one disk");
        let z = (u64at(l, 8) - base) as usize;
        assert_eq!(
            z + 56,
            n - 42,
            "the zip64 end record sits right before its locator"
        );
        let r = &tail[z..z + 56];
        assert_eq!(u32at(r, 0), EOCD64_SIG);
        assert_eq!(u64at(r, 4), 44);
        assert_eq!(u16at(r, 14), 45, "version needed");
        assert_eq!(u64at(r, 24), u64at(r, 32), "entries on this disk == total");
        (u64at(r, 32), u64at(r, 48), u64at(r, 40))
    }

    #[test]
    fn a_file_over_4_gib_gets_zip64_fields_everywhere() {
        const GIB5: u64 = 5 << 30;
        let m = Manifest {
            entries: vec![
                file_entry("big", GIB5),
                file_entry("small", 100),
                file_entry("empty", 0),
            ],
        };
        let l = Layout::build(&m, false, "P").unwrap();
        let (big, small) = (&l.slots[0], &l.slots[1]);
        assert_eq!(small.hdr_off, big.end());
        assert!(
            small.hdr_off > u32::MAX as u64,
            "the second entry starts past 4 GiB"
        );
        // Local header: version 4.5, data-descriptor + UTF-8 flags, Stored, sentinel sizes,
        // and a zip64 extra of two zero u64s (the descriptor carries the real sizes).
        let h = big.header();
        assert_eq!(u32at(&h, 0), LOCAL_SIG);
        assert_eq!((u16at(&h, 4), u16at(&h, 6), u16at(&h, 8)), (45, 0x0808, 0));
        assert_eq!(
            (u32at(&h, 14), u32at(&h, 18), u32at(&h, 22)),
            (0, u32::MAX, u32::MAX)
        );
        let (nl, el) = (u16at(&h, 26) as usize, u16at(&h, 28) as usize);
        assert_eq!(el, LOCAL_EXTRA);
        assert_eq!(h.len(), 30 + nl + el);
        let x = &h[30 + nl..];
        assert_eq!(
            (u16at(x, 0), u16at(x, 2), u64at(x, 4), u64at(x, 12)),
            (1, 16, 0, 0)
        );
        // Descriptor: 8-byte sizes.
        let d = big.descriptor(0xAABB_CCDD);
        assert_eq!((u32at(&d, 0), u32at(&d, 4)), (DESC_SIG, 0xAABB_CCDD));
        assert_eq!((u64at(&d, 8), u64at(&d, 16)), (GIB5, GIB5));
        // Central record of the entry that lives past 4 GiB.
        let mut c = Vec::new();
        small.central(7, &mut c);
        assert_eq!(u32at(&c, 0), CENTRAL_SIG);
        assert_eq!((u16at(&c, 6), u16at(&c, 8)), (45, 0x0808));
        assert_eq!(u32at(&c, 16), 7, "crc");
        assert_eq!(
            (u32at(&c, 20), u32at(&c, 24)),
            (u32::MAX, u32::MAX),
            "size sentinels"
        );
        assert_eq!(u32at(&c, 42), u32::MAX, "offset sentinel");
        let nl = u16at(&c, 28) as usize;
        assert_eq!(u16at(&c, 30), 28);
        let x = &c[46 + nl..];
        assert_eq!((u16at(x, 0), u16at(x, 2)), (1, 24));
        assert_eq!(
            (u64at(x, 4), u64at(x, 12), u64at(x, 20)),
            (100, 100, small.hdr_off)
        );
        // The tail: the empty file's entry, the directory and the three end records.
        let at = l.end();
        let tail = tail_bytes(&l, &[1, 2], at);
        let (n, cd_off, cd_len) = check_end_records(&tail, at);
        assert_eq!(n, 3);
        assert!(cd_off > GIB5);
        assert_eq!(cd_off - at + cd_len + 56 + 20 + 22, tail.len() as u64);
        let first = &tail[(cd_off - at) as usize..];
        assert_eq!(u32at(first, 0), CENTRAL_SIG);
        let x = &first[46 + u16at(first, 28) as usize..];
        assert_eq!(u64at(x, 4), GIB5, "the 5 GiB size lives in the zip64 extra");
    }

    #[test]
    fn more_than_65535_entries_round_trip_and_list_in_other_readers() {
        const N: usize = 70_000;
        let d = dir("many");
        let mut entries: Vec<Entry> = (0..N).map(|i| file_entry(&format!("d/f{i}"), 1)).collect();
        for i in 0..5 {
            entries.push(file_entry(&format!("d/empty{i}"), 0));
        }
        let m = Manifest { entries };
        let s = StoredZipSink::new(d.join("m.zip"), "P");
        s.prepare(&m).unwrap();
        s.position(&BTreeSet::new(), &BTreeMap::new()).unwrap();
        for i in 0..N {
            s.write_at(i as u32, 0, &[(i % 251) as u8]).unwrap();
        }
        for i in 0..5 {
            s.write_whole((N + i) as u32, &[]).unwrap();
        }
        s.finish().unwrap();
        let path = d.join("m.zip");
        let raw = std::fs::read(&path).unwrap();
        let (n, _, _) = check_end_records(&raw, 0);
        assert_eq!(n, (N + 5) as u64);
        let mut z = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        assert_eq!(z.len(), N + 5);
        for i in [0usize, 1, 65_534, 65_535, 65_536, N - 1] {
            let mut b = Vec::new();
            z.by_name(&format!("P/d/f{i}"))
                .unwrap()
                .read_to_end(&mut b)
                .unwrap();
            assert_eq!(b, [(i % 251) as u8], "entry {i}");
        }
        assert!(z.by_name("P/d/empty4").unwrap().size() == 0);
        // Other readers, when installed: bsdtar lists every entry; Info-ZIP unzip tests them all.
        let run = |cmd: &str, args: &[&str]| {
            std::process::Command::new(cmd)
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
        };
        let p = path.to_str().unwrap();
        let mut other = 0;
        if let Some(o) = run("bsdtar", &["-tf", p]) {
            assert_eq!(
                String::from_utf8_lossy(&o.stdout).lines().count(),
                N + 5,
                "bsdtar"
            );
            other += 1;
        }
        if let Some(o) = run("unzip", &["-tq", p]) {
            let out = String::from_utf8_lossy(&o.stdout).to_string();
            assert!(out.contains("No errors detected"), "unzip: {out}");
            other += 1;
        }
        eprintln!("zip64 archive also read by {other} non-zip-crate reader(s)");
    }

    #[test]
    fn an_in_flight_entry_cut_exactly_at_the_journal_resumes() {
        // The disk holds exactly what the journal says, no more: nothing to truncate.
        let d = dir("exact");
        let m = manifest();
        {
            let s = StoredZipSink::new(d.join("o.zip"), "P");
            s.prepare(&m).unwrap();
            s.position(&BTreeSet::new(), &BTreeMap::new()).unwrap();
            feed(&s, &m, 0, 0, 3000);
            feed(&s, &m, 2, 0, 1024);
            s.sync(&[]).unwrap();
            std::mem::forget(s);
        }
        let s = StoredZipSink::new(d.join("o.zip"), "P");
        s.prepare(&m).unwrap();
        s.position(&done(&[0]), &part(2, 1024)).unwrap();
        s.write_whole(1, &[]).unwrap();
        feed(&s, &m, 2, 1024, 5000);
        feed(&s, &m, 3, 0, 1234);
        s.finish().unwrap();
        check_zip(&d.join("o.zip"), &m);
    }

    #[test]
    fn a_whole_but_unfinished_file_is_finished_when_the_next_file_arrives_first() {
        // As above, but the next file's data reaches the sink before commit(2) does (seen on
        // a CI runner): the sink must finish file 2 itself, not refuse file 3.
        let d = dir("whole-next");
        let m = manifest();
        {
            let s = StoredZipSink::new(d.join("o.zip"), "P");
            s.prepare(&m).unwrap();
            s.position(&BTreeSet::new(), &BTreeMap::new()).unwrap();
            feed(&s, &m, 0, 0, 3000);
            feed(&s, &m, 2, 0, 5000);
            s.sync(&[]).unwrap();
            std::mem::forget(s);
        }
        let s = StoredZipSink::new(d.join("o.zip"), "P");
        s.prepare(&m).unwrap();
        s.position(&done(&[0]), &part(2, 5000)).unwrap();
        s.write_whole(1, &[]).unwrap();
        feed(&s, &m, 3, 0, 1234);
        // The late commit is a no-op now.
        s.commit(2).unwrap();
        s.finish().unwrap();
        check_zip(&d.join("o.zip"), &m);
    }

    #[test]
    fn a_whole_but_unfinished_file_resumes_and_commit_writes_its_descriptor() {
        // The restart window (review 005 section 5): every byte of file 2 is journaled but
        // its Done is not. The sink takes it as in flight with written == size; the
        // receiver's commit(2) (the FileRoot path) writes the descriptor and the archive
        // carries on with file 3, never starting over.
        let d = dir("whole");
        let m = manifest();
        {
            let s = StoredZipSink::new(d.join("o.zip"), "P");
            s.prepare(&m).unwrap();
            s.position(&BTreeSet::new(), &BTreeMap::new()).unwrap();
            feed(&s, &m, 0, 0, 3000);
            feed(&s, &m, 2, 0, 5000);
            s.sync(&[]).unwrap();
            std::mem::forget(s);
        }
        let s = StoredZipSink::new(d.join("o.zip"), "P");
        s.prepare(&m).unwrap();
        s.position(&done(&[0]), &part(2, 5000)).unwrap();
        let lay = s.layout_of(&m).unwrap();
        // Cut at the end of the data: the descriptor was never written.
        assert_eq!(
            std::fs::metadata(d.join("o.zip.ava-part")).unwrap().len(),
            lay[1].2 + lay[1].3
        );
        assert!(
            s.write_at(2, 0, &[0]).is_err(),
            "the whole file is not rewritable"
        );
        assert!(s.position(&done(&[0]), &part(2, 5001)).is_err());
        s.write_whole(1, &[]).unwrap();
        s.commit(2).unwrap();
        feed(&s, &m, 3, 0, 1234);
        s.finish().unwrap();
        check_zip(&d.join("o.zip"), &m);
    }

    fn parts(list: &[(u32, u64)]) -> BTreeMap<u32, RangeSet> {
        list.iter()
            .map(|&(id, x)| {
                let mut r = RangeSet::new();
                r.insert(0, x);
                (id, r)
            })
            .collect()
    }

    fn write_then_die(d: &std::path::Path, m: &Manifest, upto: &[(u32, u64)]) {
        let s = StoredZipSink::new(d.join("o.zip"), "P");
        s.prepare(m).unwrap();
        s.position(&BTreeSet::new(), &BTreeMap::new()).unwrap();
        for &(id, x) in upto {
            feed(&s, m, id, 0, x);
        }
        s.sync(&[]).unwrap();
        std::mem::forget(s);
    }

    #[test]
    fn several_whole_files_without_a_done_and_a_prefix_resume_without_starting_over() {
        // The journal records a file's Done after its verification, while the next files' bytes
        // keep syncing: at a drop it can hold files 0 and 2 whole (no Done) and 3 partly. That
        // used to be refused as "out of order" and restarted the whole archive (a flake of the
        // resume-size test under load).
        let d = dir("several");
        let m = manifest();
        write_then_die(&d, &m, &[(0, 3000), (2, 5000), (3, 600)]);
        let s = StoredZipSink::new(d.join("o.zip"), "P");
        s.prepare(&m).unwrap();
        s.position(&BTreeSet::new(), &parts(&[(0, 3000), (2, 5000), (3, 600)]))
            .expect("resumes where the journal says");
        // The receiver's commits for the whole files are no-ops now.
        s.commit(0).unwrap();
        s.commit(2).unwrap();
        s.write_whole(1, &[]).unwrap();
        feed(&s, &m, 3, 600, 1234);
        s.finish().unwrap();
        check_zip(&d.join("o.zip"), &m);
    }

    #[test]
    fn several_whole_files_and_no_prefix_resume_too() {
        let d = dir("severalwhole");
        let m = manifest();
        write_then_die(&d, &m, &[(0, 3000), (2, 5000)]);
        let s = StoredZipSink::new(d.join("o.zip"), "P");
        s.prepare(&m).unwrap();
        s.position(&done(&[]), &parts(&[(0, 3000), (2, 5000)]))
            .unwrap();
        s.commit(0).unwrap();
        s.commit(2).unwrap();
        s.write_whole(1, &[]).unwrap();
        feed(&s, &m, 3, 0, 1234);
        s.finish().unwrap();
        check_zip(&d.join("o.zip"), &m);
    }

    #[test]
    fn a_hole_or_a_second_prefix_is_still_refused() {
        let d = dir("hole");
        let m = manifest();
        write_then_die(&d, &m, &[(0, 3000), (2, 5000), (3, 600)]);
        let s = StoredZipSink::new(d.join("o.zip"), "P");
        s.prepare(&m).unwrap();
        // File 2 missing from the journal while file 3 has bytes: a hole.
        assert!(s.position(&done(&[0]), &parts(&[(3, 600)])).is_err());
        // Two prefixes: only the last may be partial.
        assert!(s
            .position(&done(&[]), &parts(&[(0, 100), (2, 100)]))
            .is_err());
    }

    #[test]
    fn dos_dates_are_utc_and_clamped() {
        assert_eq!(dos_datetime(0), (0, 33));
        // 2023-11-14 22:13:20 UTC
        let (t, d) = dos_datetime(1_700_000_000);
        assert_eq!(d, ((2023 - 1980) << 9) | (11 << 5) | 14);
        assert_eq!(t, (22 << 11) | (13 << 5) | 10);
        assert_eq!(dos_datetime(100), (0, 33)); // before 1980
    }
}
