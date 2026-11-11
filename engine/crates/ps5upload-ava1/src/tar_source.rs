//! A tar (plain or compressed) or a lone `.zst` as an AVA1 source. The reading is
//! `ps5upload_core::tar_archive`'s; this maps it onto the sender.
//!
//! A plain `.tar` is a random-access source: every file is a byte range of the archive, so the
//! sender reads it like a folder (parallel lanes, resume inside a large file, nothing decoded).
//!
//! A compressed tar (zstd, gzip, xz, bzip2) and a `.zst` are sequential sources (SPEC.md §17):
//! one decode thread walks the stream in order. The whole stream is one solid block, so a
//! resume decodes from the start and discards what the console already holds, as a solid 7z or
//! RAR does; the skip is reported up front (`bytes_to_skip`) so the UI shows it as progress.
//!
//! The manifest comes from the listing (`tar_layout_watched`): instant for a plain tar, one
//! decode of the stream for a compressed one, shown as the "listing" phase and cancellable. The
//! listing is cached, so an archive the Upload screen already inspected is not decoded for it
//! again.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crate::gen;
use ava1::manifest::{self, Entry, Manifest};
use ava1::seq::{EntrySink, Keep, Restart, SeqSource};
use ava1::source::{ReadAt, Source, SourceMeta};
use ps5upload_core::tar_archive::{
    fault, tar_layout_watched, tar_walk, TarFault, TarFile, TarLayout, TarVisit, TarWatch,
};

pub struct TarSource {
    path: PathBuf,
    layout: Arc<TarLayout>,
    /// Manifest path → index in `layout.files` (kept and not excluded).
    by_path: BTreeMap<String, usize>,
    dirs: BTreeSet<String>,
    /// Which of `layout.files` the manifest carries.
    sent: Vec<bool>,
    skipped: AtomicU64,
}

impl TarSource {
    /// Lists the archive (from the cache when it holds it) and builds the manifest. Errors carry
    /// a `TarFault` for an archive that cannot be sent, and are plain I/O errors otherwise; a
    /// raised `watch.cancel` ends the listing with `TAR_LISTING_CANCELLED`.
    pub fn open(
        path: &Path,
        excludes: &[String],
        watch: TarWatch<'_>,
    ) -> io::Result<(Manifest, Self)> {
        let layout = tar_layout_watched(path, watch)?;
        let excluded = |p: &str| {
            !excludes.is_empty()
                && ps5upload_core::excludes::is_excluded_strings(Path::new(p), excludes)
        };
        let mut by_path = BTreeMap::new();
        let mut sent = vec![false; layout.files.len()];
        for (i, f) in layout.files.iter().enumerate() {
            if f.kept && !excluded(&f.path) {
                by_path.insert(f.path.clone(), i);
                sent[i] = true;
            }
        }
        if by_path.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the archive has no files to unpack (after exclusions): {}",
                    path.display()
                ),
            ));
        }
        let mut dirs: BTreeSet<String> = layout
            .dirs
            .iter()
            .filter(|d| !excluded(d))
            .cloned()
            .collect();
        for p in by_path.keys() {
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
        // An allow-list ("!*.pkg") picks files out of folders; the folders that hold none of
        // them must not be created on the console as empty shells (as the RAR source does).
        if excludes.iter().any(|e| e.starts_with('!')) {
            dirs.retain(|d| {
                let prefix = format!("{d}/");
                by_path.keys().any(|f| f.starts_with(&prefix))
            });
        }
        let mut entries: Vec<Entry> = Vec::with_capacity(by_path.len() + dirs.len());
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
        for (p, &i) in &by_path {
            let f = &layout.files[i];
            entries.push(Entry {
                kind: gen::ENTRY_FILE,
                mode: f.mode,
                size: f.size,
                mtime: f.mtime,
                path: p.clone(),
                root: None,
            });
        }
        entries.sort_by(|a, b| a.path.split('/').cmp(b.path.split('/')));
        for e in &entries {
            manifest::check_path(&e.path)
                .map_err(|x| fault(TarFault::UnsafePath(format!("{}: {x}", e.path))))?;
        }
        Ok((
            Manifest { entries },
            Self {
                path: path.to_owned(),
                layout,
                by_path,
                dirs,
                sent,
                skipped: AtomicU64::new(0),
            },
        ))
    }

    /// Anything but a plain tar: read in order by one decode thread.
    pub fn sequential(&self) -> bool {
        self.layout.sequential()
    }

    /// Names the archive's contents (see `TarLayout::identity`).
    pub fn identity(&self) -> [u8; 32] {
        self.layout.identity
    }

    /// Symbolic and hard links, devices and FIFOs the archive holds and the upload leaves out.
    pub fn left_out(&self) -> (u64, u64) {
        (self.layout.links, self.layout.special)
    }

    /// Bytes decoded only to be discarded (a resume's done prefix, a replaced or excluded file).
    pub fn bytes_skipped(&self) -> u64 {
        self.skipped.load(Ordering::Relaxed)
    }
}

/// One walk's visitor: the sender's `want` and sink.
struct Visit<'a> {
    src: &'a TarSource,
    want: &'a mut dyn FnMut(&str, u64) -> Keep,
    sink: &'a mut dyn EntrySink,
}

impl TarVisit for Visit<'_> {
    fn want(&mut self, index: usize, file: &TarFile) -> bool {
        self.src.sent[index] && !matches!((self.want)(&file.path, file.size), Keep::Skip)
    }
    fn begin(&mut self, file: &TarFile) -> io::Result<()> {
        self.sink.begin(&file.path)
    }
    fn data(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.sink.data(bytes)
    }
    fn end(&mut self) -> io::Result<()> {
        self.sink.end()
    }
    fn skipped(&mut self, n: u64) {
        self.src.skipped.fetch_add(n, Ordering::Relaxed);
    }
}

impl SeqSource for TarSource {
    fn pass(
        &self,
        _restart: Restart,
        want: &mut dyn FnMut(&str, u64) -> Keep,
        sink: &mut dyn EntrySink,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let mut v = Visit {
            src: self,
            want,
            sink,
        };
        tar_walk(&self.path, &self.layout, 0, &mut v, cancel)
    }

    /// One solid stream: every pass starts at the beginning.
    fn restart_for(&self, _file_id: u32) -> Restart {
        Restart::START
    }

    fn bytes_skipped(&self) -> u64 {
        TarSource::bytes_skipped(self)
    }

    /// Every file the pass decodes and does not deliver: the ones `want` skips, the replaced
    /// copies and the excluded files.
    fn bytes_to_skip(
        &self,
        _restart: Restart,
        want: &mut dyn FnMut(&str, u64) -> Keep,
    ) -> Option<u64> {
        let mut n = 0u64;
        for (i, f) in self.layout.files.iter().enumerate() {
            let delivered = self.sent[i] && !matches!(want(&f.path, f.size), Keep::Skip);
            if !delivered {
                n = n.saturating_add(f.size);
            }
        }
        Some(n)
    }
}

/// A file of a plain tar: its byte range of the archive.
struct Range {
    file: File,
    start: u64,
    size: u64,
}

impl ReadAt for Range {
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || off >= self.size {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(self.size - off) as usize;
        self.file.seek(SeekFrom::Start(self.start + off))?;
        // Filled whole: a short read is taken for the end of the file.
        self.file
            .read_exact(&mut buf[..want])
            .map_err(|e| match e.kind() {
                io::ErrorKind::UnexpectedEof => fault(TarFault::Corrupt(
                    "the tar ends inside a file: it is cut short".into(),
                )),
                _ => e,
            })?;
        Ok(want)
    }
}

impl Source for TarSource {
    fn open(&self, rel: &str) -> io::Result<Box<dyn ReadAt>> {
        if self.sequential() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{rel}: a compressed tar is read in order"),
            ));
        }
        let i = *self
            .by_path
            .get(rel)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, rel.to_owned()))?;
        let f = &self.layout.files[i];
        Ok(Box::new(Range {
            file: File::open(&self.path)?,
            start: f.data_offset,
            size: f.size,
        }))
    }

    fn list(&self, rel: &str) -> io::Result<Vec<(String, SourceMeta)>> {
        if self.sequential() {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        if !rel.is_empty() && !self.dirs.contains(rel) {
            return Err(io::Error::new(io::ErrorKind::NotFound, rel.to_owned()));
        }
        let prefix = if rel.is_empty() {
            String::new()
        } else {
            format!("{rel}/")
        };
        let mut out = BTreeMap::new();
        for d in &self.dirs {
            if let Some(rest) = d.strip_prefix(&prefix) {
                if !rest.contains('/') {
                    out.insert(
                        rest.to_owned(),
                        SourceMeta {
                            is_dir: true,
                            mode: 0o755,
                            ..SourceMeta::default()
                        },
                    );
                }
            }
        }
        for (p, &i) in &self.by_path {
            if let Some(rest) = p.strip_prefix(&prefix) {
                if !rest.contains('/') {
                    out.insert(rest.to_owned(), meta(&self.layout.files[i]));
                }
            }
        }
        Ok(out.into_iter().collect())
    }

    fn stat(&self, rel: &str) -> io::Result<SourceMeta> {
        if self.sequential() {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        if let Some(&i) = self.by_path.get(rel) {
            return Ok(meta(&self.layout.files[i]));
        }
        if rel.is_empty() || self.dirs.contains(rel) {
            return Ok(SourceMeta {
                is_dir: true,
                mode: 0o755,
                ..SourceMeta::default()
            });
        }
        Err(io::Error::new(io::ErrorKind::NotFound, rel.to_owned()))
    }
}

fn meta(f: &TarFile) -> SourceMeta {
    SourceMeta {
        size: f.size,
        mtime: f.mtime,
        mode: f.mode,
        is_dir: false,
    }
}
