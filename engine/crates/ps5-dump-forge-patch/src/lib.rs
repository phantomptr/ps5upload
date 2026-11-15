//! In-place image edits with an undo journal: the engine every format editor's writes go through.
//!
//! A format editor (`edit::plan` in the exFAT, UFS2 and PFS crates) turns [`Op`]s into [`Write`]s
//! and reads the image only. [`apply`] then writes them crash-safely:
//!
//! 1. the old bytes of every `live` region (their union: live writes may overlap, each byte's
//!    original kept once) go to `<image file name>.forge-undo` beside the image (exclusive
//!    create), which is synced with its folder;
//! 2. the free-space writes, then an image sync;
//! 3. the live writes (metadata, in-place replacements) in the order given, then an image sync.
//!
//! [`Applied::commit`] deletes the journal; [`Applied::rollback`] restores the old bytes and
//! deletes it. A journal left by a crash is handled by [`recover`], which every open of an image
//! for editing calls first: a complete journal is restored then deleted, an incomplete one (the
//! crash came before any live write) only deleted. Free space keeps whatever the free-space
//! writes put there: nothing references it once the live regions are restored.
//!
//! Journal layout, little-endian: `FORGEUNDO1`, the image length (u64), the journal's own length
//! (u64), the entry count (u32); per entry its offset (u64), length (u64) and old bytes; then the
//! trailer: `FORGEEND` and a CRC-32 of everything before the CRC. A journal with no trailer
//! marker at its end nor where its header says it ends (PS5Upload: whatever its length, and with
//! its header zeroed too) was cut short before it was synced (so before any live write: the
//! trailer is written and synced before the image's first write) and is deleted; any other that
//! fails a check (its CRC, its declared length either way, an entry) is damage: an error, the
//! journal kept.
//!
//! PS5Upload: beside the journal, `<journal>.id` ([`identity_path`]) says which image it was
//! written for, so a journal is never restored into another image of the same length that
//! replaced the one a crash left (the journal itself, shared with Forge, is unchanged and holds
//! no identity). Written before the journal's first byte: the image's length, a CRC of the
//! journal's regions (binding it to this journal), and CRCs of up to 64 small samples of the
//! image the edit never writes (outside every write, live or free), which read the same
//! before, during and after the edit. [`recover`] compares those samples with the image before
//! restoring and refuses on a difference, keeping both files. Without an identity file (a
//! journal Forge wrote), or with one that belongs to another journal or is damaged, recovery
//! is as before. Its limit: an image that differs from the original only inside the edited
//! regions (e.g. the same game rebuilt to the same layout) is not told apart.

use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const MAGIC: &[u8; 10] = b"FORGEUNDO1";
const HEADER_LEN: u64 = 10 + 8 + 8 + 4;
const END: &[u8; 8] = b"FORGEEND";
const TRAILER_LEN: u64 = 8 + 4;
const ENTRY_HEADER_LEN: u64 = 8 + 8;
/// Copies stream in pieces of at most this many bytes; cancel is checked between pieces.
const PIECE: usize = 8 << 20;

/// What core asks a format editor to do, after resolving `Edit`s (a host folder is expanded
/// into `Mkdir` + `Put`s; folder deletes/renames stay one op).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Delete a file, or a folder with everything in it.
    Delete { path: String },
    /// Move or rename a file or folder; `to` does not exist.
    Rename { from: String, to: String },
    /// A new empty folder (its parents exist or come first).
    Mkdir { path: String },
    /// Add or replace a file of `len` bytes whose content `Data` names.
    Put { path: String, len: u64, data: Data },
}

/// The bytes of a [`Write`] or a [`Op::Put`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Data {
    /// These bytes.
    Bytes(Vec<u8>),
    /// This many zero bytes.
    Zero(u64),
    /// `len` bytes at `offset` of the caller's source `id`, read through `apply`'s `read_source`.
    Source { id: u32, offset: u64, len: u64 },
}

impl Data {
    /// The number of bytes.
    pub fn len(&self) -> u64 {
        match self {
            Data::Bytes(b) => b.len() as u64,
            Data::Zero(n) => *n,
            Data::Source { len, .. } => *len,
        }
    }

    /// No bytes at all.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One write into the image. `live`: it overwrites bytes the current filesystem uses (metadata,
/// an in-place PFS replacement), so the journal keeps the old bytes; else it lands in free space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    pub offset: u64,
    pub data: Data,
    pub live: bool,
}

/// The caller's source of [`Data::Source`] bytes: fill the whole buffer from `offset` of `id`.
pub type ReadSource<'a> = &'a mut dyn FnMut(u32, u64, &mut [u8]) -> io::Result<()>;

/// How the engine makes its writes durable: `file` syncs the image or the journal, `dir` the
/// folder holding the journal (after it is created or deleted). Core passes its own on the PS5
/// (retrying syncs, where a retried sync fails the job); [`Syncs::STD`] elsewhere.
#[derive(Debug, Clone, Copy)]
pub struct Syncs {
    pub file: fn(&File) -> io::Result<()>,
    pub dir: fn(&Path) -> io::Result<()>,
}

impl Syncs {
    /// `File::sync_all`, and the folder opened and synced (unix; a no-op elsewhere).
    pub const STD: Syncs = Syncs {
        file: File::sync_all,
        dir: std_sync_dir,
    };
}

/// What [`Applied::commit`]'s error message starts with when the edit is in the image and only
/// syncing the journal's removal failed.
pub const APPLIED: &str = "applied; ";

/// An applied edit whose journal is still on disk: [`commit`](Self::commit) it once the image
/// checks out, else [`rollback`](Self::rollback). Dropped without either, the journal stays for
/// [`recover`].
#[derive(Debug)]
pub struct Applied {
    /// The image edited, kept open: a rollback restores this file even if its path now names
    /// another one (replaced while the edit was checked).
    image: File,
    journal: PathBuf,
}

/// The undo journal's path: `<image file name>.forge-undo` in the image's folder.
pub fn journal_path(image: &Path) -> PathBuf {
    let mut name = image.file_name().unwrap_or_default().to_os_string();
    name.push(".forge-undo");
    image.with_file_name(name)
}

/// PS5Upload: the identity file beside the journal: `<image file name>.forge-undo.id`.
pub fn identity_path(image: &Path) -> PathBuf {
    identity_of_journal(&journal_path(image))
}

fn identity_of_journal(journal: &Path) -> PathBuf {
    let mut name = journal.file_name().unwrap_or_default().to_os_string();
    name.push(".id");
    journal.with_file_name(name)
}

/// What [`recover`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// No journal: nothing to do.
    Clean,
    /// A complete journal: the old bytes were restored, then it was deleted.
    Restored,
    /// An incomplete journal (a crash while writing it, before any live write): deleted.
    Discarded,
}

/// Where a test stops `apply` as if the process died there; `All` is a normal run.
///
/// PS5Upload: public behind the `fault-injection` feature (with [`apply_until`] and
/// `LiveAfter`), so the engine's tests cut a real editor's plan at every live write.
#[cfg_attr(not(any(test, feature = "fault-injection")), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// After the journal's first entry, before its trailer.
    JournalHalf,
    /// After the free-space writes and their sync.
    Free,
    /// After half the live writes.
    LiveHalf,
    /// After this many live writes (none past the last: then like `All` without its sync).
    LiveAfter(usize),
    All,
}

/// [`apply`], stopped at `stop` as if the process died there (nothing cleaned up). Tests only.
#[cfg(feature = "fault-injection")]
pub fn apply_until_step(
    image: &mut File,
    image_path: &Path,
    writes: &[Write],
    read_source: ReadSource,
    cancel: &AtomicBool,
    sync: Syncs,
    stop: Step,
) -> io::Result<Applied> {
    apply_until(image, image_path, writes, read_source, cancel, sync, stop)
}

/// Writes `writes` into `image` (opened read-write, at `image_path`) crash-safely, as the crate
/// docs describe. Live writes may overlap each other (applied in order, so the last wins); a
/// free-space write must not overlap any other. All lie inside the image's current length (an
/// image never grows); zero-length writes are skipped. `read_source` serves [`Data::Source`];
/// copies stream in pieces of at most 8 MiB, `cancel` checked between them. A cancel or failure
/// before the live writes deletes the journal (the filesystem is untouched); one during them
/// rolls back; so does one seen after the last piece. An existing journal is refused:
/// [`recover`] first. `sync` makes each step durable.
pub fn apply(
    image: &mut File,
    image_path: &Path,
    writes: &[Write],
    read_source: ReadSource,
    cancel: &AtomicBool,
    sync: Syncs,
) -> io::Result<Applied> {
    apply_until(
        image,
        image_path,
        writes,
        read_source,
        cancel,
        sync,
        Step::All,
    )
}

fn apply_until(
    image: &mut File,
    image_path: &Path,
    writes: &[Write],
    read_source: ReadSource,
    cancel: &AtomicBool,
    sync: Syncs,
    stop: Step,
) -> io::Result<Applied> {
    let sync_image = |image: &File| (sync.file)(image).map_err(|e| ctx(e, "sync the image"));
    let image_len = image
        .metadata()
        .map_err(|e| ctx(e, "read the image's length"))?
        .len();
    check(writes, image_len)?;
    // The handle a rollback restores through, taken before anything is written.
    let kept = image
        .try_clone()
        .map_err(|e| ctx(e, "keep the image open"))?;
    let journal = journal_path(image_path);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&journal)
        .map_err(|e| match e.kind() {
            io::ErrorKind::AlreadyExists => io::Error::new(
                e.kind(),
                format!("{} exists: recover the image first", journal.display()),
            ),
            _ => ctx(e, &format!("create {}", journal.display())),
        })?;
    // Which image this journal is for, before its first byte (see the crate docs).
    let journaled = write_identity(&journal, image, image_len, writes, sync)
        .and_then(|()| write_journal(file, image, image_len, writes, cancel, sync, stop))
        .and_then(|()| sync_dir(&journal, sync));
    if let Err(e) = journaled {
        if stop != Step::JournalHalf {
            discard(&journal, sync);
        }
        return Err(e);
    }
    let free =
        put_all(image, writes, false, read_source, cancel, None).and_then(|()| sync_image(image));
    if let Err(e) = free {
        discard(&journal, sync);
        return Err(e);
    }
    if stop == Step::Free {
        return Err(crash());
    }
    let half = match stop {
        Step::LiveHalf => Some(live(writes).count() / 2),
        Step::LiveAfter(n) => Some(n),
        _ => None,
    };
    let wrote =
        put_all(image, writes, true, read_source, cancel, half).and_then(|()| sync_image(image));
    if let Err(e) = wrote {
        if half.is_some() {
            return Err(e);
        }
        return match restore(Some(image), None, &journal, sync) {
            Ok(_) => Err(e),
            Err(r) => Err(io::Error::new(
                e.kind(),
                format!("{e}; restoring the image failed too ({r}), the next open retries it"),
            )),
        };
    }
    Ok(Applied {
        image: kept,
        journal,
    })
}

impl Applied {
    /// Keeps the edit: deletes the journal, then syncs its folder. A failed delete leaves the
    /// journal (the next [`recover`] would undo the edit). A failed folder sync comes after the
    /// delete: the edit is in the image, and the error's message starts with [`APPLIED`]. Should
    /// the deleted journal reappear after a crash, the next open restores the original image,
    /// which is safe.
    pub fn commit(self, sync: Syncs) -> io::Result<()> {
        std::fs::remove_file(&self.journal)
            .map_err(|e| ctx(e, &format!("delete {}", self.journal.display())))?;
        let _ = std::fs::remove_file(identity_of_journal(&self.journal));
        sync_dir(&self.journal, sync).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "{APPLIED}the edit is in the image, only syncing the removal of its undo \
                     journal failed ({e}); if the journal is back after a crash, the next open \
                     restores the original"
                ),
            )
        })
    }

    /// Undoes the edit: the journal's old bytes go back into the image, then it is deleted.
    pub fn rollback(mut self, sync: Syncs) -> io::Result<()> {
        restore(Some(&mut self.image), None, &self.journal, sync).map(drop)
    }
}

/// Settles a journal a crash left beside `image_path` (see [`Recovery`]). Call it before every
/// open of an image for editing. A damaged journal, or one that doesn't fit the image, is an
/// error and stays.
pub fn recover(image_path: &Path, sync: Syncs) -> io::Result<Recovery> {
    let journal = journal_path(image_path);
    match std::fs::symlink_metadata(&journal) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Recovery::Clean),
        Err(e) => return Err(ctx(e, &format!("read {}", journal.display()))),
        Ok(_) => {}
    }
    restore(None, Some(image_path), &journal, sync)
}

/// Refuses writes past the image's end and a free write overlapping any other write (live
/// writes may overlap each other: they apply in order).
fn check(writes: &[Write], image_len: u64) -> io::Result<()> {
    let mut spans = Vec::with_capacity(writes.len());
    for w in writes.iter().filter(|w| !w.data.is_empty()) {
        let len = w.data.len();
        match w.offset.checked_add(len) {
            Some(end) if end <= image_len => spans.push((w.offset, end, w.live)),
            _ => {
                return Err(invalid(format!(
                    "a write of {len} bytes at offset {} ends past the image's {image_len} bytes",
                    w.offset
                )));
            }
        }
    }
    spans.sort_unstable();
    // Sorted by start, a free span overlaps something iff the furthest-reaching span before
    // it, or the next one, overlaps it.
    let mut reach: Option<(u64, u64)> = None;
    for (i, &(start, end, live)) in spans.iter().enumerate() {
        let prev = reach.filter(|r| start < r.1);
        let next = spans.get(i + 1).filter(|n| n.0 < end).map(|n| (n.0, n.1));
        if let Some(o) = prev.or(next).filter(|_| !live) {
            return Err(invalid(format!(
                "a free-space write overlaps another: {start}..{end} and {}..{}",
                o.0, o.1
            )));
        }
        if reach.is_none_or(|r| end > r.1) {
            reach = Some((start, end));
        }
    }
    if u32::try_from(journal_spans(writes).len()).is_err() {
        return Err(invalid("too many live regions for one journal".into()));
    }
    Ok(())
}

/// The union of the live writes' ranges, sorted and merged: what the journal keeps, each
/// byte's original value once.
fn journal_spans(writes: &[Write]) -> Vec<(u64, u64)> {
    let mut spans: Vec<(u64, u64)> = live(writes)
        .map(|w| (w.offset, w.offset + w.data.len()))
        .collect();
    spans.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        match out.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

/// The live writes that write anything, in order.
fn live(writes: &[Write]) -> impl Iterator<Item = &Write> {
    writes.iter().filter(|w| w.live && !w.data.is_empty())
}

/// A writer that keeps the CRC-32 of what went through it.
struct Hashed {
    out: BufWriter<File>,
    crc: crc32fast::Hasher,
}

impl Hashed {
    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.crc.update(bytes);
        self.out.write_all(bytes)
    }
}

/// The journal: header, the live regions' old bytes, trailer; then synced.
fn write_journal(
    file: File,
    image: &mut File,
    image_len: u64,
    writes: &[Write],
    cancel: &AtomicBool,
    sync: Syncs,
    stop: Step,
) -> io::Result<()> {
    let wrap = |e| ctx(e, "write the undo journal");
    let mut out = Hashed {
        out: BufWriter::with_capacity(1 << 20, file),
        crc: crc32fast::Hasher::new(),
    };
    let spans = journal_spans(writes);
    let entries: u64 = spans.iter().map(|s| ENTRY_HEADER_LEN + s.1 - s.0).sum();
    let journal_len = HEADER_LEN + entries + TRAILER_LEN;
    out.put(MAGIC).map_err(wrap)?;
    out.put(&image_len.to_le_bytes()).map_err(wrap)?;
    out.put(&journal_len.to_le_bytes()).map_err(wrap)?;
    out.put(&(spans.len() as u32).to_le_bytes()).map_err(wrap)?;
    let mut buf = Vec::new();
    for (i, &(offset, end)) in spans.iter().enumerate() {
        let len = end - offset;
        out.put(&offset.to_le_bytes()).map_err(wrap)?;
        out.put(&len.to_le_bytes()).map_err(wrap)?;
        image
            .seek(SeekFrom::Start(offset))
            .map_err(|e| ctx(e, "read the image"))?;
        let mut done = 0;
        while done < len {
            check_cancel(cancel)?;
            let piece = piece(&mut buf, len - done);
            image
                .read_exact(piece)
                .map_err(|e| ctx(e, "read the image"))?;
            out.put(piece).map_err(wrap)?;
            done += piece.len() as u64;
        }
        if stop == Step::JournalHalf && i == 0 {
            out.out.flush().map_err(wrap)?;
            return Err(crash());
        }
    }
    check_cancel(cancel)?;
    out.put(END).map_err(wrap)?;
    let crc = out.crc.finalize();
    let mut out = out.out;
    out.write_all(&crc.to_le_bytes()).map_err(wrap)?;
    let file = out.into_inner().map_err(|e| wrap(e.into_error()))?;
    (sync.file)(&file).map_err(|e| ctx(e, "sync the undo journal"))
}

/// The free (`live` false) or live writes in order; with `limit`, only that many, then a crash.
fn put_all(
    image: &mut File,
    writes: &[Write],
    live: bool,
    read_source: ReadSource,
    cancel: &AtomicBool,
    limit: Option<usize>,
) -> io::Result<()> {
    let mut buf = Vec::new();
    let todo = writes
        .iter()
        .filter(|w| w.live == live && !w.data.is_empty());
    for (i, w) in todo.enumerate() {
        if limit == Some(i) {
            return Err(crash());
        }
        image
            .seek(SeekFrom::Start(w.offset))
            .map_err(|e| ctx(e, "write the image"))?;
        let len = w.data.len();
        let mut done = 0;
        while done < len {
            check_cancel(cancel)?;
            let n = match &w.data {
                Data::Bytes(b) => {
                    let n = (len - done).min(PIECE as u64) as usize;
                    let at = done as usize;
                    image.write_all(&b[at..at + n]).map(|()| n)
                }
                Data::Zero(_) => {
                    let piece = piece(&mut buf, len - done);
                    piece.fill(0);
                    image.write_all(piece).map(|()| piece.len())
                }
                Data::Source { id, offset, .. } => {
                    let piece = piece(&mut buf, len - done);
                    read_source(*id, offset + done, piece)
                        .map_err(|e| ctx(e, &format!("read source {id}")))?;
                    image.write_all(piece).map(|()| piece.len())
                }
            };
            done += n.map_err(|e| ctx(e, "write the image"))? as u64;
        }
    }
    // A cancel during the last piece still counts.
    check_cancel(cancel)
}

/// Puts a journal's old bytes back into `image` and deletes it, or only deletes an incomplete
/// one. The whole journal is checked before the image is touched.
fn restore(
    image: Option<&mut File>,
    image_path: Option<&Path>,
    journal: &Path,
    sync: Syncs,
) -> io::Result<Recovery> {
    let name = journal.display();
    let rd = |e| ctx(e, &format!("read {name}"));
    let bad = |what: &str| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{name}: {what}; it is kept"),
        )
    };
    let mut file = File::open(journal).map_err(rd)?;
    let len = file.metadata().map_err(rd)?.len();
    let mut head = Vec::with_capacity(HEADER_LEN as usize);
    (&mut file)
        .take(HEADER_LEN)
        .read_to_end(&mut head)
        .map_err(rd)?;
    // PS5Upload: a crash before the journal was synced can leave it at any length with its
    // unwritten bytes zero, its header included.
    let zeroed = !head.is_empty() && head.iter().all(|&b| b == 0);
    if !zeroed && head.iter().zip(MAGIC).any(|(a, b)| a != b) {
        return Err(bad("not an undo journal"));
    }
    let header = Header::parse(&head).filter(|_| !zeroed);
    // Cut short when its trailer marker is neither at its end nor where its header says it
    // ends: the trailer is written, then the journal synced, then its folder, all before the
    // image's first write, so a journal without one was never synced and nothing was written
    // (PS5Upload: whatever its length; a full-length one with a zeroed tail included). Anything
    // else that fails a check is damage, and kept.
    let ended = has_end(&mut file, len).map_err(rd)?
        || match &header {
            Some(h) if h.len < len => has_end(&mut file, h.len).map_err(rd)?,
            _ => false,
        };
    if !ended {
        remove_journal(journal, sync)?;
        return Ok(Recovery::Discarded);
    }
    let whole = match &header {
        Some(h) if h.len == len && len >= HEADER_LEN + TRAILER_LEN => {
            crc_ok(&mut file, len).map_err(rd)?
        }
        _ => false,
    };
    let (Some(header), true) = (header, whole) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the undo journal {name} is damaged; the image may be part-edited: keep both files"
            ),
        ));
    };
    // Pass 1: every entry's place, by seeking over the old bytes.
    let end = len - TRAILER_LEN;
    let mut pos = HEADER_LEN;
    let mut entries = Vec::new();
    let mut reader = BufReader::new(&mut file);
    reader.seek(SeekFrom::Start(pos)).map_err(rd)?;
    for _ in 0..header.count {
        let (mut offset, mut n) = ([0u8; 8], [0u8; 8]);
        if end - pos < ENTRY_HEADER_LEN {
            return Err(bad("an entry runs past its end"));
        }
        reader.read_exact(&mut offset).map_err(rd)?;
        reader.read_exact(&mut n).map_err(rd)?;
        pos += ENTRY_HEADER_LEN;
        let (offset, n) = (u64::from_le_bytes(offset), u64::from_le_bytes(n));
        if n > end - pos || offset.checked_add(n).is_none_or(|e| e > header.image_len) {
            return Err(bad("an entry runs past its end or the image's"));
        }
        entries.push((offset, n, pos));
        pos += n;
        if let Ok(skip) = i64::try_from(n) {
            reader.seek_relative(skip).map_err(rd)?;
        } else {
            reader.seek(SeekFrom::Start(pos)).map_err(rd)?;
        }
    }
    if pos != end {
        return Err(bad("bytes left after its last entry"));
    }
    drop(reader);
    // A short journal was already discarded; only a complete, valid one needs the image.
    let mut opened;
    let opened_here = image.is_none();
    let image = match image {
        Some(image) => image,
        None => {
            opened = open_rw(image_path.expect("recover supplies the image path"))?;
            &mut opened
        }
    };
    let image_len = image
        .metadata()
        .map_err(|e| ctx(e, "read the image's length"))?
        .len();
    if header.image_len != image_len {
        return Err(bad("it was written for an image of another length"));
    }
    // A recovery after a crash: the image at the path must be the one the journal is for (a
    // rollback restores the very file it wrote, kept open).
    if let Some(path) = image_path.filter(|_| opened_here) {
        check_identity(image, path, journal, &header, &entries)?;
    }
    // Pass 2: the old bytes back.
    let mut buf = Vec::new();
    for (offset, n, at) in entries {
        file.seek(SeekFrom::Start(at)).map_err(rd)?;
        image
            .seek(SeekFrom::Start(offset))
            .map_err(|e| ctx(e, "restore the image"))?;
        let mut done = 0;
        while done < n {
            let piece = piece(&mut buf, n - done);
            file.read_exact(piece).map_err(rd)?;
            image
                .write_all(piece)
                .map_err(|e| ctx(e, "restore the image"))?;
            done += piece.len() as u64;
        }
    }
    (sync.file)(image).map_err(|e| ctx(e, "sync the image"))?;
    remove_journal(journal, sync)?;
    Ok(Recovery::Restored)
}

/// A journal header's fields after its magic.
struct Header {
    image_len: u64,
    len: u64,
    count: u32,
}

impl Header {
    /// The fields of `head`, or `None` when it is shorter than a header.
    fn parse(head: &[u8]) -> Option<Header> {
        let at = |i: usize| Some(u64::from_le_bytes(head.get(i..i + 8)?.try_into().ok()?));
        Some(Header {
            image_len: at(10)?,
            len: at(18)?,
            count: u32::from_le_bytes(head.get(26..30)?.try_into().ok()?),
        })
    }
}

/// Whether the `FORGEEND` marker sits where the trailer of a journal of `len` bytes starts.
fn has_end(file: &mut File, len: u64) -> io::Result<bool> {
    if len < TRAILER_LEN {
        return Ok(false);
    }
    let mut end = [0u8; 8];
    file.seek(SeekFrom::Start(len - TRAILER_LEN))?;
    file.read_exact(&mut end)?;
    Ok(&end == END)
}

/// Whether the last 4 bytes hold the CRC-32 of everything before them.
fn crc_ok(file: &mut File, len: u64) -> io::Result<bool> {
    file.seek(SeekFrom::Start(0))?;
    let mut crc = crc32fast::Hasher::new();
    let mut buf = Vec::new();
    let mut done = 0;
    while done < len - 4 {
        let piece = piece(&mut buf, len - 4 - done);
        file.read_exact(piece)?;
        crc.update(piece);
        done += piece.len() as u64;
    }
    let mut trailer = [0u8; 4];
    file.read_exact(&mut trailer)?;
    Ok(crc.finalize() == u32::from_le_bytes(trailer))
}

/// The next piece of `buf` for `left` more bytes: at most [`PIECE`], grown on first use.
fn piece(buf: &mut Vec<u8>, left: u64) -> &mut [u8] {
    let n = left.min(PIECE as u64) as usize;
    if buf.len() < n {
        buf.resize(n, 0);
    }
    &mut buf[..n]
}

fn remove_journal(journal: &Path, sync: Syncs) -> io::Result<()> {
    std::fs::remove_file(journal).map_err(|e| ctx(e, &format!("delete {}", journal.display())))?;
    // Its identity goes with it (best effort: one left over belongs to no journal).
    let _ = std::fs::remove_file(identity_of_journal(journal));
    sync_dir(journal, sync)
}

// ─── PS5Upload: which image a journal is for ─────────────────────────────────────────────────

const ID_MAGIC: &[u8; 8] = b"PS5UFID1";
/// Samples of the image an identity keeps, at most, and each one's length at most.
const ID_SAMPLES: u64 = 64;
const ID_SAMPLE_LEN: u64 = 4096;
/// An identity file's size: magic, image length, regions CRC, count; per sample its offset,
/// length and CRC; its own CRC.
const ID_HEAD_LEN: u64 = 8 + 8 + 4 + 4;
const ID_ENTRY_LEN: u64 = 8 + 8 + 4;
const ID_MAX_LEN: u64 = ID_HEAD_LEN + ID_SAMPLES * ID_ENTRY_LEN + 4;

/// A CRC of a journal's regions (offset, length), in order.
fn regions_crc(regions: impl Iterator<Item = (u64, u64)>) -> u32 {
    let mut crc = crc32fast::Hasher::new();
    for (offset, len) in regions {
        crc.update(&offset.to_le_bytes());
        crc.update(&len.to_le_bytes());
    }
    crc.finalize()
}

/// Up to [`ID_SAMPLES`] places (offset, length) spread over the bytes no write touches.
fn sample_places(writes: &[Write], image_len: u64) -> Vec<(u64, u64)> {
    let mut spans: Vec<(u64, u64)> = (writes.iter())
        .filter(|w| !w.data.is_empty())
        .map(|w| (w.offset, w.offset.saturating_add(w.data.len())))
        .collect();
    spans.sort_unstable();
    let mut gaps = Vec::new();
    let mut at = 0u64;
    for (start, end) in spans {
        if start > at {
            gaps.push((at, start.min(image_len)));
        }
        at = at.max(end);
    }
    if at < image_len {
        gaps.push((at, image_len));
    }
    gaps.retain(|g| g.1 > g.0);
    let total: u64 = gaps.iter().map(|g| g.1 - g.0).sum();
    let mut out: Vec<(u64, u64)> = Vec::new();
    if total == 0 {
        return out;
    }
    for i in 0..ID_SAMPLES {
        let spot = u128::from(total) * u128::from(2 * i + 1) / u128::from(2 * ID_SAMPLES);
        let mut target = u64::try_from(spot).unwrap_or(u64::MAX);
        for &(start, end) in &gaps {
            let n = end - start;
            if target < n {
                let offset = start + target;
                let len = ID_SAMPLE_LEN.min(end - offset);
                if out.last().is_none_or(|l| offset >= l.0 + l.1) {
                    out.push((offset, len));
                }
                break;
            }
            target -= n;
        }
    }
    out
}

/// The CRC-32 of `len` (at most [`ID_SAMPLE_LEN`]) bytes of `image` at `offset`.
fn crc_at(image: &mut File, offset: u64, len: u64) -> io::Result<u32> {
    let mut buf = vec![0u8; len.min(ID_SAMPLE_LEN) as usize];
    image.seek(SeekFrom::Start(offset))?;
    image.read_exact(&mut buf)?;
    Ok(crc32fast::hash(&buf))
}

/// Writes (and syncs) the identity of the image `writes` go into, beside `journal`.
fn write_identity(
    journal: &Path,
    image: &mut File,
    image_len: u64,
    writes: &[Write],
    sync: Syncs,
) -> io::Result<()> {
    let regions = journal_spans(writes).into_iter().map(|(s, e)| (s, e - s));
    let mut b = ID_MAGIC.to_vec();
    b.extend_from_slice(&image_len.to_le_bytes());
    b.extend_from_slice(&regions_crc(regions).to_le_bytes());
    let places = sample_places(writes, image_len);
    b.extend_from_slice(&(places.len() as u32).to_le_bytes());
    for (offset, len) in places {
        let crc = crc_at(image, offset, len).map_err(|e| ctx(e, "read the image"))?;
        b.extend_from_slice(&offset.to_le_bytes());
        b.extend_from_slice(&len.to_le_bytes());
        b.extend_from_slice(&crc.to_le_bytes());
    }
    let crc = crc32fast::hash(&b);
    b.extend_from_slice(&crc.to_le_bytes());
    let path = identity_of_journal(journal);
    let what = |e| ctx(e, &format!("write {}", path.display()));
    let mut file = File::create(&path).map_err(what)?;
    file.write_all(&b).map_err(what)?;
    (sync.file)(&file).map_err(|e| ctx(e, "sync the undo journal's identity"))
}

/// One sample of an identity: offset, length, CRC.
type Sample = (u64, u64, u32);

/// An identity file read back: (image length, regions CRC, samples). `None` when there is
/// none, or it is damaged (then it can't tell, and recovery goes on as without one).
fn read_identity(path: &Path) -> Option<(u64, u32, Vec<Sample>)> {
    let mut b = Vec::new();
    File::open(path)
        .ok()?
        .take(ID_MAX_LEN + 1)
        .read_to_end(&mut b)
        .ok()?;
    let len = b.len() as u64;
    if !(ID_HEAD_LEN + 4..=ID_MAX_LEN).contains(&len) || b.get(..8)? != ID_MAGIC {
        return None;
    }
    let (body, tail) = b.split_at(b.len() - 4);
    if crc32fast::hash(body) != u32::from_le_bytes(tail.try_into().ok()?) {
        return None;
    }
    let u64_at = |i: usize| Some(u64::from_le_bytes(body.get(i..i + 8)?.try_into().ok()?));
    let u32_at = |i: usize| Some(u32::from_le_bytes(body.get(i..i + 4)?.try_into().ok()?));
    let image_len = u64_at(8)?;
    let regions = u32_at(16)?;
    let count = u64::from(u32_at(20)?);
    if count > ID_SAMPLES || body.len() as u64 != ID_HEAD_LEN + count * ID_ENTRY_LEN {
        return None;
    }
    let mut samples = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let at = ID_HEAD_LEN as usize + i * ID_ENTRY_LEN as usize;
        let (offset, len, crc) = (u64_at(at)?, u64_at(at + 8)?, u32_at(at + 16)?);
        if len == 0 || len > ID_SAMPLE_LEN {
            return None;
        }
        samples.push((offset, len, crc));
    }
    Some((image_len, regions, samples))
}

/// Refuses to restore `journal` into `image` when its identity file says it was written for
/// another image (see the crate docs). `entries`: the journal's (offset, length, position).
fn check_identity(
    image: &mut File,
    image_path: &Path,
    journal: &Path,
    header: &Header,
    entries: &[(u64, u64, u64)],
) -> io::Result<()> {
    let id = identity_of_journal(journal);
    let Some((image_len, regions, samples)) = read_identity(&id) else {
        return Ok(());
    };
    // Another journal's (left by one whose own removal failed): it says nothing of this one.
    let ours = regions_crc(entries.iter().map(|&(offset, n, _)| (offset, n)));
    if image_len != header.image_len || regions != ours {
        return Ok(());
    }
    let same = samples.iter().all(|&(offset, len, crc)| {
        offset.checked_add(len).is_some_and(|end| end <= image_len)
            && crc_at(image, offset, len).is_ok_and(|got| got == crc)
    });
    if same {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{} is not the image the undo journal {} was written for (it was replaced after an \
             interrupted edit): nothing was restored and both are kept. If the image is right \
             as it is, delete {} and {}",
            image_path.display(),
            journal.display(),
            journal.display(),
            id.display()
        ),
    ))
}

/// Best effort after a failure: a journal left behind is settled by the next [`recover`].
fn discard(journal: &Path, sync: Syncs) {
    let _ = remove_journal(journal, sync);
}

/// Syncs the folder holding `path`, so a created or deleted journal survives a crash.
fn sync_dir(path: &Path, sync: Syncs) -> io::Result<()> {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    (sync.dir)(dir).map_err(|e| ctx(e, &format!("sync {}", dir.display())))
}

#[cfg(unix)]
fn std_sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

// ponytail: Windows has no directory fsync through std (it needs FILE_FLAG_BACKUP_SEMANTICS);
// NTFS journals the create and delete itself.
#[cfg(not(unix))]
fn std_sync_dir(_: &Path) -> io::Result<()> {
    Ok(())
}

fn open_rw(image: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(image)
        .map_err(|e| ctx(e, &format!("open {}", image.display())))
}

fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
    }
    Ok(())
}

/// What a test's simulated crash returns; `apply_until` leaves everything as it is.
fn crash() -> io::Error {
    io::Error::other("simulated crash")
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

fn ctx(e: io::Error, what: &str) -> io::Error {
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    const LEN: u64 = 20 << 20;
    const BIG: u64 = 9 << 20;

    /// A folder under the system temp dir, removed on drop.
    struct Tmp(PathBuf);

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A fresh image of `LEN` patterned bytes: its folder, path and bytes.
    fn image() -> (Tmp, PathBuf, Vec<u8>) {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("forge-patch-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("game.ffpkg");
        let bytes: Vec<u8> = (0..LEN).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        (Tmp(dir), path, bytes)
    }

    fn source_byte(id: u32, at: u64) -> u8 {
        (id as u64 + at * 3) as u8 ^ 0x5a
    }

    fn read_source(id: u32, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = source_byte(id, offset + i as u64);
        }
        Ok(())
    }

    /// Free writes of each kind, and three live ones (one over 8 MiB, so it takes pieces).
    fn writes() -> Vec<Write> {
        let w = |offset, data, live| Write { offset, data, live };
        vec![
            w(1 << 20, Data::Bytes(vec![0xee; 5000]), false),
            w(2 << 20, Data::Zero(70_000), false),
            w(
                3 << 20,
                Data::Source {
                    id: 1,
                    offset: 10,
                    len: 123_456,
                },
                false,
            ),
            w(4096, Data::Bytes(vec![0x11; 512]), true),
            w(
                5 << 20,
                Data::Source {
                    id: 2,
                    offset: 0,
                    len: BIG,
                },
                true,
            ),
            w(LEN - 100, Data::Zero(100), true),
            w(64, Data::Zero(0), true),
        ]
    }

    /// `original` with `writes` applied in memory.
    fn expected(original: &[u8], writes: &[Write]) -> Vec<u8> {
        let mut out = original.to_vec();
        for w in writes {
            let at = w.offset as usize;
            let n = w.data.len() as usize;
            for (i, b) in out[at..at + n].iter_mut().enumerate() {
                *b = match &w.data {
                    Data::Bytes(v) => v[i],
                    Data::Zero(_) => 0,
                    Data::Source { id, offset, .. } => source_byte(*id, offset + i as u64),
                };
            }
        }
        out
    }

    /// The image matches `original` everywhere but the free-space writes (unreferenced bytes).
    fn assert_restored(path: &Path, original: &[u8], writes: &[Write]) {
        let now = std::fs::read(path).unwrap();
        let free: Vec<_> = writes.iter().filter(|w| !w.live).collect();
        let free = expected(original, &free.into_iter().cloned().collect::<Vec<_>>());
        assert!(
            now == free,
            "the image differs from the original outside free space"
        );
        assert!(!journal_path(path).exists());
    }

    fn run(path: &Path, writes: &[Write], stop: Step) -> io::Result<Applied> {
        let mut f = open_rw(path).unwrap();
        apply_until(
            &mut f,
            path,
            writes,
            &mut read_source,
            &AtomicBool::new(false),
            Syncs::STD,
            stop,
        )
    }

    #[test]
    fn apply_then_commit() {
        let (_tmp, path, original) = image();
        let writes = writes();
        let applied = run(&path, &writes, Step::All).unwrap();
        assert!(journal_path(&path).exists());
        assert!(std::fs::read(&path).unwrap() == expected(&original, &writes));
        applied.commit(Syncs::STD).unwrap();
        assert!(!journal_path(&path).exists());
        assert!(std::fs::read(&path).unwrap() == expected(&original, &writes));
        assert_eq!(recover(&path, Syncs::STD).unwrap(), Recovery::Clean);
    }

    #[test]
    fn rollback_restores() {
        let (_tmp, path, original) = image();
        let writes = writes();
        run(&path, &writes, Step::All)
            .unwrap()
            .rollback(Syncs::STD)
            .unwrap();
        assert_restored(&path, &original, &writes);
        // Live writes only: byte for byte.
        let before = std::fs::read(&path).unwrap();
        let live: Vec<_> = writes.into_iter().filter(|w| w.live).collect();
        run(&path, &live, Step::All)
            .unwrap()
            .rollback(Syncs::STD)
            .unwrap();
        assert!(std::fs::read(&path).unwrap() == before);
    }

    #[test]
    fn recover_after_a_crash_at_each_step() {
        for stop in [Step::JournalHalf, Step::Free, Step::LiveHalf] {
            let (_tmp, path, original) = image();
            let writes = writes();
            assert!(run(&path, &writes, stop).is_err());
            assert!(journal_path(&path).exists(), "{stop:?}");
            let now = std::fs::read(&path).unwrap();
            let free_only = expected(&original, &writes[..3]);
            match stop {
                Step::JournalHalf => assert!(now == original, "nothing written before the journal"),
                Step::Free => assert!(now == free_only),
                _ => assert!(now != free_only, "some live bytes changed"),
            }
            let want = match stop {
                Step::JournalHalf => Recovery::Discarded,
                _ => Recovery::Restored,
            };
            assert_eq!(recover(&path, Syncs::STD).unwrap(), want, "{stop:?}");
            if stop == Step::JournalHalf {
                assert!(std::fs::read(&path).unwrap() == original);
                assert!(!journal_path(&path).exists());
            } else {
                assert_restored(&path, &original, &writes);
            }
            assert_eq!(recover(&path, Syncs::STD).unwrap(), Recovery::Clean);
        }
    }

    /// PS5Upload: a crash after every count of live writes (none, each one, all of them with
    /// the journal not yet deleted) recovers to the original outside free space.
    #[test]
    fn recover_after_a_crash_at_every_live_write() {
        let live = writes()
            .iter()
            .filter(|w| w.live && !w.data.is_empty())
            .count();
        for n in 0..=live + 1 {
            let (_tmp, path, original) = image();
            let writes = writes();
            match run(&path, &writes, Step::LiveAfter(n)) {
                Err(_) => assert!(n < live, "a crash before write {n}"),
                // Past the last live write: applied, then dropped (a crash before commit).
                Ok(applied) => {
                    assert!(n >= live);
                    drop(applied);
                }
            }
            assert!(journal_path(&path).exists(), "after {n}");
            assert_eq!(recover(&path, Syncs::STD).unwrap(), Recovery::Restored);
            assert_restored(&path, &original, &writes);
        }
    }

    #[test]
    fn truncated_journal_is_discarded() {
        let (_tmp, path, original) = image();
        let writes = writes();
        // Commit's journal kept aside, then cut short at a few lengths.
        let journal = journal_path(&path);
        let applied = run(&path, &writes, Step::All).unwrap();
        let whole = std::fs::read(&journal).unwrap();
        applied.rollback(Syncs::STD).unwrap();
        let restored = std::fs::read(&path).unwrap();
        for cut in [
            0,
            5,
            10,
            HEADER_LEN as usize + 3,
            whole.len() / 2,
            whole.len() - 1,
        ] {
            std::fs::write(&journal, &whole[..cut]).unwrap();
            assert_eq!(
                recover(&path, Syncs::STD).unwrap(),
                Recovery::Discarded,
                "cut at {cut}"
            );
            assert!(!journal.exists());
        }
        assert!(std::fs::read(&path).unwrap() == restored);
        // A complete journal restores again (idempotent).
        std::fs::write(&journal, &whole).unwrap();
        assert_eq!(recover(&path, Syncs::STD).unwrap(), Recovery::Restored);
        assert_restored(&path, &original, &writes);
    }

    #[test]
    fn truncated_journal_is_discarded_without_image() {
        let (_tmp, path, _) = image();
        let journal = journal_path(&path);
        std::fs::write(&journal, &MAGIC[..5]).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(recover(&path, Syncs::STD).unwrap(), Recovery::Discarded);
        assert!(!journal.exists());
    }

    #[test]
    fn overlap_and_out_of_bounds_are_refused() {
        let (_tmp, path, original) = image();
        let w = |offset, len, live| Write {
            offset,
            data: Data::Zero(len),
            live,
        };
        let bad = [
            vec![w(0, 100, true), w(99, 10, false)],
            vec![w(4096, 4096, false), w(4096, 1, true)],
            vec![w(0, 50, true), w(10, 5, false), w(40, 100, true)],
            vec![w(0, 4096, true), w(0, 100, false)],
            vec![w(LEN - 10, 11, true)],
            vec![w(LEN, 1, false)],
            vec![w(u64::MAX, 2, true)],
        ];
        for writes in bad {
            let e = run(&path, &writes, Step::All).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");
            assert!(!journal_path(&path).exists());
        }
        // Touching, empty and exactly-to-the-end writes are fine.
        let ok = [
            w(0, 100, true),
            w(100, 10, false),
            w(100, 0, true),
            w(LEN - 1, 1, true),
        ];
        run(&path, &ok, Step::All)
            .unwrap()
            .rollback(Syncs::STD)
            .unwrap();
        assert_restored(&path, &original, &ok);
    }

    #[test]
    fn an_existing_journal_is_refused() {
        let (_tmp, path, _) = image();
        let writes = writes();
        let applied = run(&path, &writes, Step::All).unwrap();
        let e = run(&path, &writes, Step::All).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{e}");
        applied.commit(Syncs::STD).unwrap();
    }

    #[test]
    fn cancel_before_and_during_live_writes() {
        let (_tmp, path, original) = image();
        let writes = writes();
        let mut f = open_rw(&path).unwrap();
        let e = apply(
            &mut f,
            &path,
            &writes,
            &mut read_source,
            &AtomicBool::new(true),
            Syncs::STD,
        )
        .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::Interrupted, "{e}");
        assert!(std::fs::read(&path).unwrap() == original);
        assert!(!journal_path(&path).exists());
        // Cancelled by the first piece of the 9 MiB live copy: rolled back.
        let cancel = AtomicBool::new(false);
        let mut source = |id, offset, buf: &mut [u8]| {
            if id == 2 {
                cancel.store(true, Ordering::Relaxed);
            }
            read_source(id, offset, buf)
        };
        let e = apply(&mut f, &path, &writes, &mut source, &cancel, Syncs::STD).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::Interrupted, "{e}");
        assert_restored(&path, &original, &writes);
    }

    #[test]
    fn a_damaged_journal_is_kept() {
        let (_tmp, path, _) = image();
        let journal = journal_path(&path);
        run(&path, &writes(), Step::All).unwrap();
        let edited = std::fs::read(&path).unwrap();
        let whole = std::fs::read(&journal).unwrap();
        let mut flipped = whole.clone();
        flipped[HEADER_LEN as usize + 20] ^= 1;
        // The header's own length corrupted, up and down: the trailer still says complete.
        let mut up = whole.clone();
        up[18..26].copy_from_slice(&(whole.len() as u64 + 4096).to_le_bytes());
        let mut down = whole.clone();
        down[18..26].copy_from_slice(&(whole.len() as u64 - 1).to_le_bytes());
        // A byte appended: no trailer at the end, but one where the header says it ends.
        let mut longer = whole.clone();
        longer.push(0);
        for damaged in [flipped, up, down, longer] {
            std::fs::write(&journal, &damaged).unwrap();
            let e = recover(&path, Syncs::STD).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
            assert!(e.to_string().contains("is damaged"), "{e}");
            assert!(journal.exists());
            assert!(std::fs::read(&path).unwrap() == edited);
        }
        // Shorter than declared, no trailer: cut short.
        std::fs::write(&journal, &whole[..whole.len() - 1]).unwrap();
        assert_eq!(recover(&path, Syncs::STD).unwrap(), Recovery::Discarded);
    }

    /// PS5Upload: a crash before the journal was synced can leave it at full length with its
    /// unwritten tail zero (no trailer), or all zero. The trailer and the syncs come before the
    /// image's first write, so such a journal predates every write: deleted, the image as it is.
    #[test]
    fn a_full_length_journal_without_its_trailer_is_discarded() {
        let (_tmp, path, original) = image();
        let journal = journal_path(&path);
        let applied = run(&path, &writes(), Step::All).unwrap();
        let whole = std::fs::read(&journal).unwrap();
        applied.rollback(Syncs::STD).unwrap();
        let restored = std::fs::read(&path).unwrap();
        let mut unended = whole.clone();
        let at = unended.len() - TRAILER_LEN as usize;
        unended[at..].fill(0);
        let mut zero_tail = whole.clone();
        let half = zero_tail.len() / 2;
        zero_tail[half..].fill(0);
        let zeros = vec![0u8; whole.len()];
        for (what, cut) in [
            ("unended", unended),
            ("zero tail", zero_tail),
            ("zeros", zeros),
        ] {
            std::fs::write(&journal, &cut).unwrap();
            assert_eq!(
                recover(&path, Syncs::STD).unwrap(),
                Recovery::Discarded,
                "{what}"
            );
            assert!(!journal.exists(), "{what}");
            assert!(std::fs::read(&path).unwrap() == restored, "{what}");
        }
        assert_restored(&path, &original, &writes());
    }

    /// PS5Upload: a journal is never restored into another image of the same length that took
    /// the crashed one's place: its identity file tells them apart, both files are kept and
    /// the image is not touched. The crashed image itself (or a journal with no identity, as
    /// Forge writes, or one whose identity is another journal's or damaged) restores as before.
    #[test]
    fn a_journal_is_never_restored_into_another_image() {
        let (_tmp, path, original) = image();
        let writes = writes();
        let (journal, id) = (journal_path(&path), identity_path(&path));
        assert!(run(&path, &writes, Step::LiveHalf).is_err());
        assert!(journal.exists() && id.exists());
        let crashed = std::fs::read(&path).unwrap();
        // Another image of the same length in its place.
        let other: Vec<u8> = (0..LEN).map(|i| (i * 13 % 241) as u8).collect();
        std::fs::write(&path, &other).unwrap();
        let e = recover(&path, Syncs::STD).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
        assert!(e.to_string().contains("is not the image"), "{e}");
        assert!(journal.exists() && id.exists());
        assert!(
            std::fs::read(&path).unwrap() == other,
            "the other image was touched"
        );
        // The crashed image back: restored, and both files go.
        std::fs::write(&path, &crashed).unwrap();
        assert_eq!(recover(&path, Syncs::STD).unwrap(), Recovery::Restored);
        assert_restored(&path, &original, &writes);
        assert!(!id.exists());

        // No identity (a journal Forge wrote), a damaged one, another journal's: as before.
        for identity in ["none", "damaged", "another"] {
            let (_tmp, path, original) = image();
            let (journal, id) = (journal_path(&path), identity_path(&path));
            assert!(run(&path, &writes, Step::LiveHalf).is_err());
            match identity {
                "none" => std::fs::remove_file(&id).unwrap(),
                "damaged" => {
                    let mut b = std::fs::read(&id).unwrap();
                    b[30] ^= 1;
                    std::fs::write(&id, b).unwrap();
                }
                _ => {
                    // An identity written for other writes (other regions).
                    let mut f = open_rw(&path).unwrap();
                    let few: Vec<Write> =
                        writes.iter().filter(|w| w.live).take(1).cloned().collect();
                    write_identity(&journal, &mut f, LEN, &few, Syncs::STD).unwrap();
                }
            }
            let other: Vec<u8> = std::fs::read(&path).unwrap();
            assert_eq!(
                recover(&path, Syncs::STD).unwrap(),
                Recovery::Restored,
                "{identity}"
            );
            assert!(std::fs::read(&path).unwrap() != other);
            assert_restored(&path, &original, &writes);
            assert!(!journal.exists() && !id.exists(), "{identity}");
        }
    }

    /// PS5Upload: the identity goes with its journal on a commit, a rollback and a discard.
    #[test]
    fn the_identity_goes_with_its_journal() {
        let (_tmp, path, _) = image();
        let writes = writes();
        let id = identity_path(&path);
        run(&path, &writes, Step::All)
            .unwrap()
            .commit(Syncs::STD)
            .unwrap();
        assert!(!id.exists());
        run(&path, &writes, Step::All)
            .unwrap()
            .rollback(Syncs::STD)
            .unwrap();
        assert!(!id.exists());
        assert!(run(&path, &writes, Step::JournalHalf).is_err());
        assert!(id.exists());
        assert_eq!(recover(&path, Syncs::STD).unwrap(), Recovery::Discarded);
        assert!(!id.exists());
        // Samples lie outside every write, inside the image.
        let places = sample_places(&writes, LEN);
        assert!(!places.is_empty() && places.len() as u64 <= ID_SAMPLES);
        for (offset, len) in places {
            assert!(offset + len <= LEN);
            assert!(
                writes
                    .iter()
                    .filter(|w| !w.data.is_empty())
                    .all(|w| { offset + len <= w.offset || offset >= w.offset + w.data.len() })
            );
        }
    }

    /// A journal for `image_len` with `count` entries and `body` as its entry table, whose
    /// declared length and trailer CRC are right.
    fn journal_bytes(image_len: u64, count: u32, body: &[u8]) -> Vec<u8> {
        let len = HEADER_LEN + body.len() as u64 + TRAILER_LEN;
        let mut b = MAGIC.to_vec();
        b.extend_from_slice(&image_len.to_le_bytes());
        b.extend_from_slice(&len.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
        b.extend_from_slice(body);
        b.extend_from_slice(END);
        let crc = crc32fast::hash(&b);
        b.extend_from_slice(&crc.to_le_bytes());
        b
    }

    #[test]
    fn a_short_or_garbled_journal_is_kept() {
        let (_tmp, path, original) = image();
        let journal = journal_path(&path);
        let image_len = original.len() as u64;
        // Too short for a header, yet ending with the trailer marker: damaged, not cut short.
        std::fs::write(&journal, b"FORGEUNDO1FORGEEND\0\0\0\0").unwrap();
        let e = recover(&path, Syncs::STD).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
        assert!(e.to_string().contains("is damaged"), "{e}");
        assert!(journal.exists());
        assert!(std::fs::read(&path).unwrap() == original);
        // Whole, CRC right, but the entry table is garbage.
        let far = [u64::MAX.to_le_bytes(), u64::MAX.to_le_bytes()].concat();
        for garbled in [
            journal_bytes(image_len, u32::MAX, &[]),
            journal_bytes(image_len, 1, &far),
            journal_bytes(image_len, 1, &[0xAB; 7]),
            journal_bytes(image_len, 0, &[0xAB; 5]),
        ] {
            std::fs::write(&journal, &garbled).unwrap();
            let e = recover(&path, Syncs::STD).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
            assert!(e.to_string().contains("it is kept"), "{e}");
            assert!(journal.exists());
            assert!(std::fs::read(&path).unwrap() == original);
        }
    }
}
