//! A 7z archive as a forward-only AVA1 source (SPEC.md section 17).
//!
//! The manifest is built from the archive header (no decoding). `pass` walks the
//! archive's *folders* (7z's solid blocks) in index order from the restart folder:
//! a folder in which every wanted file is `Skip` is not opened at all (so a
//! non-solid archive resumes in O(1)), otherwise it is decoded front to back and
//! files the receiver already holds are read and discarded (a solid stream cannot
//! be entered mid-way). Decoding stops after the last wanted file of a folder.
//!
//! Memory: threads default to 1 (`PS5UPLOAD_7Z_THREADS`, see
//! `ps5upload_core::transfer::sevenz_decode_threads`): multi-threaded LZMA2 decode
//! buffers a whole solid stream, so RAM scales with the archive, not the dictionary.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ava1::gen;
use ava1::manifest::{self, Entry, Manifest};
use ava1::seq::{EntrySink, Keep, Restart, SeqSource};
use ava1::source::{ReadAt, Source, SourceMeta};
use sevenz_rust2::{Archive, BlockDecoder, Error as SzError, Password};

/// Why an archive cannot be uploaded, carried inside an `io::Error` (`InvalidData`) so
/// it survives the sender's error chain and the upload can give it a stable reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SevenzFault {
    /// The header or a decoded stream is damaged, truncated or fails its CRC.
    #[error("the 7z archive is corrupt: {0}")]
    Corrupt(String),
    /// Password-protected (AES); the app has no password input for 7z.
    #[error("the 7z archive is encrypted: {0}")]
    Encrypted(String),
    /// An entry name that would escape the destination (or the manifest refuses).
    #[error("the 7z archive has an unsafe entry path: {0}")]
    UnsafePath(String),
    /// A solid block with stream-less entries between its files (the crate's walk would
    /// drop files); the archive is refused rather than partly uploaded.
    #[error("{}", ps5upload_core::transfer::SEVENZ_LAYOUT_UNSUPPORTED)]
    UnsupportedLayout,
    /// An unsupported coder method or header feature (the engine fails the job with `7z_unsupported`).
    #[error("the 7z archive is not usable as an AVA1 source: {0}")]
    Unsupported(String),
    /// A duplicate name, a path that is both a file and a directory, or a decoder memory
    /// limit: retrying cannot fix it, so this is terminal (`ava1_7z_unsupported`).
    #[error("the 7z archive cannot be uploaded: {0}")]
    Conflict(String),
}

fn fault(f: SevenzFault) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, f)
}

/// The [`SevenzFault`] inside an error chain, if any.
pub fn fault_of<'a>(e: &'a (dyn std::error::Error + 'static)) -> Option<&'a SevenzFault> {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(c) = cur {
        if let Some(f) = c.downcast_ref::<SevenzFault>() {
            return Some(f);
        }
        if let Some(io) = c.downcast_ref::<io::Error>() {
            if let Some(f) = io.get_ref().and_then(|i| i.downcast_ref::<SevenzFault>()) {
                return Some(f);
            }
        }
        cur = c.source();
    }
    None
}

/// 7z AES coder method id.
const AES_ID: [u8; 4] = [0x06, 0xF1, 0x07, 0x01];

fn map_open_error(e: SzError) -> io::Error {
    match e {
        SzError::Io(e, _) | SzError::FileOpen(e, _) => e,
        SzError::PasswordRequired | SzError::MaybeBadPassword(_) => {
            fault(SevenzFault::Encrypted("a password is required".into()))
        }
        SzError::UnsupportedVersion { .. }
        | SzError::ExternalUnsupported
        | SzError::UnsupportedCompressionMethod(_)
        | SzError::Unsupported(_) => fault(SevenzFault::Unsupported(e.to_string())),
        SzError::MaxMemLimited { .. } => fault(SevenzFault::Conflict(e.to_string())),
        other => fault(SevenzFault::Corrupt(other.to_string())),
    }
}

/// A failure from a decoder read: a damaged or short stream is corruption; anything
/// else (the disk) is an ordinary I/O error.
fn read_error(e: io::Error) -> io::Error {
    let bad = matches!(
        e.kind(),
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
    ) || e
        .get_ref()
        .and_then(|i| i.downcast_ref::<SzError>())
        .is_some_and(|s| matches!(s, SzError::ChecksumVerificationFailed));
    if bad {
        fault(SevenzFault::Corrupt(format!("an entry is damaged: {e}")))
    } else {
        e
    }
}

/// One entry of a folder, in decode order. `path` is `None` for an excluded file.
struct Member {
    /// The archive's own name, checked against the decoder's entry (a drifted walk
    /// must fail, never write bytes under another path).
    raw: String,
    path: Option<String>,
    size: u64,
}

pub struct SevenzSource {
    path: PathBuf,
    archive: Archive,
    /// Per folder: its files in decode order.
    folders: Vec<Vec<Member>>,
    /// Files without a stream (empty files): (path, never decoded).
    empties: Vec<String>,
    /// Restart point per manifest entry (`u64::MAX` for entries that need no decode).
    restarts: Vec<u64>,
    threads: u32,
    identity: [u8; 32],
    folders_opened: AtomicU64,
    decoded: AtomicU64,
    skipped: AtomicU64,
}

impl SevenzSource {
    pub fn open(path: &Path, excludes: &[String]) -> io::Result<(Manifest, Self)> {
        let file = File::open(path)?;
        let meta = file.metadata()?;
        let mut head = [0u8; 32];
        let mut f = BufReader::new(file);
        f.read_exact(&mut head).map_err(|e| {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                fault(SevenzFault::Corrupt("the file is too short".into()))
            } else {
                e
            }
        })?;
        f.seek(SeekFrom::Start(0))?;
        let archive = Archive::read(&mut f, &Password::empty()).map_err(map_open_error)?;
        // The next header lists every file's name, size and CRC-32: with the start
        // header and the length it names this archive's contents.
        let off = u64::from_le_bytes(head[12..20].try_into().unwrap());
        let len = u64::from_le_bytes(head[20..28].try_into().unwrap());
        let mut next_header = Vec::new();
        if len <= meta.len() {
            f.seek(SeekFrom::Start(32u64.saturating_add(off)))?;
            f.by_ref().take(len).read_to_end(&mut next_header)?;
        }
        let mut h = blake3::Hasher::new();
        h.update(&meta.len().to_le_bytes());
        // Not the mtime: a copied or touched identical archive keeps its resume.
        h.update(&head);
        h.update(&next_header);
        let identity = *h.finalize().as_bytes();

        if archive
            .blocks
            .iter()
            .any(|b| b.coders.iter().any(|c| c.encoder_method_id() == AES_ID))
        {
            return Err(fault(SevenzFault::Encrypted(
                "the archive's data is AES-encrypted".into(),
            )));
        }

        let nblocks = archive.blocks.len();
        let mut folders: Vec<Vec<Member>> = (0..nblocks).map(|_| Vec::new()).collect();
        let mut empties = Vec::new();
        let mut files: BTreeMap<String, (Option<usize>, u64, u64)> = BTreeMap::new();
        let mut dirs: BTreeSet<String> = BTreeSet::new();
        for (fi, e) in archive.files.iter().enumerate() {
            let block = archive
                .stream_map
                .file_block_index
                .get(fi)
                .copied()
                .flatten();
            if block.is_some() && !e.has_stream() {
                // A directory or empty file between the streamed files of one block: the
                // crate's per-block walk covers fewer entries than the block spans and
                // would skip the last files, so refuse before sending anything.
                return Err(fault(SevenzFault::UnsupportedLayout));
            }
            if e.is_directory() {
                // An unsafe directory name is ignored: its files are refused by name.
                if let Ok(rel) = sanitize(e.name()) {
                    if !ps5upload_core::excludes::is_excluded_strings(Path::new(&rel), excludes) {
                        dirs.insert(rel);
                    }
                }
                continue;
            }
            let rel = sanitize(e.name())?;
            let excluded = ps5upload_core::excludes::is_excluded_strings(Path::new(&rel), excludes);
            let size = e.size();
            match block {
                Some(b) => folders[b].push(Member {
                    raw: e.name().to_string(),
                    path: (!excluded).then(|| rel.clone()),
                    size,
                }),
                None if !excluded => empties.push(rel.clone()),
                None => {}
            }
            if excluded {
                continue;
            }
            let mtime = if e.has_last_modified_date {
                crate::archive_time::nt_to_unix(u64::from(e.last_modified_date()))
            } else {
                0
            };
            if files.insert(rel.clone(), (block, size, mtime)).is_some() {
                return Err(fault(SevenzFault::Conflict(format!("{rel} appears twice"))));
            }
        }
        if files.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "7z has no extractable files (after exclusions): {}",
                    path.display()
                ),
            ));
        }
        for p in files.keys() {
            let parts: Vec<&str> = p.split('/').collect();
            let mut acc = String::new();
            for c in &parts[..parts.len() - 1] {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(c);
                dirs.insert(acc.clone());
            }
        }
        if files.keys().any(|p| dirs.contains(p)) {
            return Err(fault(SevenzFault::Conflict(
                "a path is both a file and a directory".into(),
            )));
        }
        let mut entries: Vec<Entry> = Vec::with_capacity(files.len() + dirs.len());
        for d in &dirs {
            entries.push(Entry {
                kind: gen::ENTRY_DIR,
                mode: 0o755,
                size: 0,
                mtime: 0,
                path: d.clone(),
                root: None,
            });
        }
        for (p, (_, size, mtime)) in &files {
            entries.push(Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: *size,
                mtime: *mtime,
                path: p.clone(),
                root: None,
            });
        }
        entries.sort_by(|a, b| a.path.split('/').cmp(b.path.split('/')));
        for e in &entries {
            manifest::check_path(&e.path)
                .map_err(|x| fault(SevenzFault::UnsafePath(format!("{}: {x}", e.path))))?;
        }
        let restarts = entries
            .iter()
            .map(|e| match files.get(&e.path) {
                Some((Some(b), _, _)) if e.kind == gen::ENTRY_FILE => *b as u64,
                _ => u64::MAX,
            })
            .collect();
        Ok((
            Manifest { entries },
            Self {
                path: path.to_owned(),
                archive,
                folders,
                empties,
                restarts,
                threads: ps5upload_core::transfer::sevenz_decode_threads(),
                identity,
                folders_opened: AtomicU64::new(0),
                decoded: AtomicU64::new(0),
                skipped: AtomicU64::new(0),
            },
        ))
    }

    /// Names this archive's contents (size, mtime, start header): a changed archive
    /// with the same listing and sizes has a different identity.
    pub fn identity(&self) -> [u8; 32] {
        self.identity
    }

    /// Folders opened for decoding over this source's lifetime (a folder whose files
    /// were all skipped is not opened).
    pub fn folders_opened(&self) -> u64 {
        self.folders_opened.load(Ordering::Relaxed)
    }

    /// Bytes the decoder produced (delivered plus discarded).
    pub fn bytes_decoded(&self) -> u64 {
        self.decoded.load(Ordering::Relaxed)
    }

    /// Bytes decoded only to be discarded (the "skipping data the console already
    /// has" phase).
    pub fn bytes_skipped(&self) -> u64 {
        self.skipped.load(Ordering::Relaxed)
    }

    pub fn folder_count(&self) -> usize {
        self.folders.len()
    }

    fn feed(
        &self,
        member: &Member,
        keep: &Keep,
        rd: &mut dyn Read,
        sink: &mut dyn EntrySink,
        cancel: &AtomicBool,
        buf: &mut [u8],
    ) -> io::Result<()> {
        let skipping = matches!(keep, Keep::Skip);
        if let (false, Some(p)) = (skipping, &member.path) {
            sink.begin(p)?;
        }
        let mut total = 0u64;
        loop {
            // At most one buffer (256 KiB) of input between polls, skipping included.
            if cancel.load(Ordering::Relaxed) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
            }
            let n = loop {
                match rd.read(buf) {
                    Ok(n) => break n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(read_error(e)),
                }
            };
            if n == 0 {
                break;
            }
            total += n as u64;
            self.decoded.fetch_add(n as u64, Ordering::Relaxed);
            if skipping {
                self.skipped.fetch_add(n as u64, Ordering::Relaxed);
            } else {
                sink.data(&buf[..n])?;
            }
        }
        if total != member.size {
            return Err(fault(SevenzFault::Corrupt(format!(
                "an entry ended after {total} of {} bytes",
                member.size
            ))));
        }
        if !skipping && member.path.is_some() {
            sink.end()?;
        }
        Ok(())
    }
}

/// The destination-relative path for an entry name, or an `UnsafePath` fault when the
/// name would escape it (or is empty).
fn sanitize(name: &str) -> io::Result<String> {
    ps5upload_core::transfer::sanitize_7z_entry(name)
        .ok_or_else(|| fault(SevenzFault::UnsafePath(format!("{name:?}"))))
}

impl SeqSource for SevenzSource {
    fn pass(
        &self,
        restart: Restart,
        want: &mut dyn FnMut(&str, u64) -> Keep,
        sink: &mut dyn EntrySink,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let stopped = || io::Error::new(io::ErrorKind::Interrupted, "stopped");
        let mut src = BufReader::with_capacity(256 << 10, File::open(&self.path)?);
        let pw = Password::empty();
        let mut buf = vec![0u8; 256 << 10];
        let first = usize::try_from(restart.0).unwrap_or(usize::MAX);
        for b in first..self.folders.len() {
            if cancel.load(Ordering::Relaxed) {
                return Err(stopped());
            }
            let members = &self.folders[b];
            let keeps: Vec<Keep> = members
                .iter()
                .map(|m| match &m.path {
                    Some(p) => want(p, m.size),
                    None => Keep::Skip,
                })
                .collect();
            let Some(last) = keeps.iter().rposition(|k| !matches!(k, Keep::Skip)) else {
                continue; // nothing wanted: the folder is never opened
            };
            self.folders_opened.fetch_add(1, Ordering::Relaxed);
            let dec = BlockDecoder::new(self.threads, b, &self.archive, &pw, &mut src);
            let mut j = 0usize;
            let mut failure: Option<io::Error> = None;
            let mut reached_last = false;
            let res = dec.for_each_entries(&mut |entry, rd| {
                if !entry.has_stream() {
                    return Ok(true); // a directory or empty file inside the block
                }
                let i = j;
                j += 1;
                let (Some(m), Some(k)) = (members.get(i), keeps.get(i)) else {
                    failure = Some(fault(SevenzFault::Corrupt(
                        "a folder holds more entries than its header lists".into(),
                    )));
                    return Ok(false);
                };
                if entry.name() != m.raw {
                    failure = Some(fault(SevenzFault::Corrupt(format!(
                        "the decoder yielded {:?} where the header lists {:?}",
                        entry.name(),
                        m.raw
                    ))));
                    return Ok(false);
                }
                if let Err(e) = self.feed(m, k, rd, sink, cancel, &mut buf) {
                    failure = Some(e);
                    return Ok(false);
                }
                // Nothing wanted after `last`: do not decode the rest of the folder.
                reached_last = i >= last;
                Ok(i < last)
            });
            if let Some(e) = failure {
                return Err(e);
            }
            if let Err(e) = res {
                return Err(map_pass_error(e));
            }
            if !reached_last {
                // Stream-less entries inside a folder were refused at open, so this is a
                // damaged archive (or a crate change), which any decoder would hit.
                return Err(short_folder(j, members.len()));
            }
        }
        for p in &self.empties {
            if cancel.load(Ordering::Relaxed) {
                return Err(stopped());
            }
            if !matches!(want(p, 0), Keep::Skip) {
                sink.begin(p)?;
                sink.end()?;
            }
        }
        Ok(())
    }

    fn restart_for(&self, file_id: u32) -> Restart {
        Restart(self.restarts.get(file_id as usize).copied().unwrap_or(0))
    }
}

/// A folder whose decode ended before its last wanted entry.
fn short_folder(yielded: usize, listed: usize) -> io::Error {
    fault(SevenzFault::Corrupt(format!(
        "the folder yielded {yielded} of {listed} entries"
    )))
}

fn map_pass_error(e: SzError) -> io::Error {
    match e {
        SzError::Io(e, _) | SzError::FileOpen(e, _) => read_error(e),
        SzError::ChecksumVerificationFailed => {
            fault(SevenzFault::Corrupt("an entry fails its CRC-32".into()))
        }
        other => map_open_error(other),
    }
}

/// `SendOptions.seq` replaces the readers, so the sender's `Source` is never read; it
/// only has to exist.
pub struct NoSource;

impl Source for NoSource {
    fn open(&self, rel: &str) -> io::Result<Box<dyn ReadAt>> {
        Err(io::Error::other(format!(
            "{rel}: a sequential source has no random access"
        )))
    }
    fn list(&self, _rel: &str) -> io::Result<Vec<(String, SourceMeta)>> {
        Ok(Vec::new())
    }
    fn stat(&self, rel: &str) -> io::Result<SourceMeta> {
        Err(io::Error::other(format!("{rel}: no random access")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_folder_is_corrupt_not_unsupported_layout() {
        let e = short_folder(2, 5);
        assert_eq!(
            fault_of(&e),
            Some(&SevenzFault::Corrupt(
                "the folder yielded 2 of 5 entries".into()
            ))
        );
        assert!(e.to_string().contains("yielded 2 of 5"), "{e}");
    }

    #[test]
    fn only_coder_methods_are_unsupported_rest_are_conflicts() {
        let mem = SzError::MaxMemLimited {
            max_kb: 1,
            actaul_kb: 2,
        };
        assert!(matches!(
            fault_of(&map_open_error(mem)),
            Some(SevenzFault::Conflict(_))
        ));
        let m = SzError::UnsupportedCompressionMethod("x".into());
        assert!(matches!(
            fault_of(&map_open_error(m)),
            Some(SevenzFault::Unsupported(_))
        ));
    }
}
