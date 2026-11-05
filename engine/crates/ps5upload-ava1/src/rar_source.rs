//! A RAR archive (any volume set, optional password) as a sequential AVA1 source
//! (SPEC.md §17). Desktop only: the `ps5upload-unrar` helper that decodes it does not exist
//! on Android.
//!
//! UnRAR can only be read forward. One `pass` opens the archive and walks it once on
//! the calling (decode) thread, delivering the entries the sender wants. Resume is
//! entry-granular and costs what the format allows: a non-solid archive seeks past
//! every entry the console already holds (header reads only), a solid archive restarts
//! at the beginning and decodes-and-discards what the console has. No decoder state is
//! checkpointed.
//!
//! The password lives in this struct for the job's lifetime (so every resume pass of
//! the same job has it) and is never logged, formatted or put in an error.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::gen;
use ava1::manifest::{self, Entry, Manifest};
use ava1::seq::{EntrySink, Keep, Restart, SeqSource};
use ava1::source::{ReadAt, Source, SourceMeta};
use ps5upload_core::transfer::{
    rar_layout_watched, rar_walk, RarFailKind, RarLayoutWatch, RarWalkError, RarWalkSink,
    RAR_LAYOUT_CANCELLED,
};

/// Why a RAR could not be uploaded. Terminal for the job: retrying the same bytes
/// with the same password cannot help.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RarReason {
    /// Encrypted and no password was given (also: an engine restart lost it).
    PasswordRequired,
    PasswordWrong,
    /// Damaged data (a bad CRC, a truncated volume, a broken header).
    Corrupt,
    MissingVolume,
    /// The archive lists its entries in a different order than it extracts them; a
    /// resume cannot trust entry positions.
    Reordered,
    Other,
}

impl RarReason {
    /// The machine-readable `error_reason`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PasswordRequired => "ava1_rar_password_required",
            Self::PasswordWrong => "ava1_rar_password_wrong",
            Self::Corrupt => "ava1_rar_corrupt",
            Self::MissingVolume => "ava1_rar_missing_volume",
            Self::Reordered => "ava1_rar_reordered",
            Self::Other => "ava1_rar_failed",
        }
    }
}

/// A [`RarReason`] with the human message; carried inside an `io::Error` through the
/// sender and recovered with [`rar_failure`].
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct RarFailure {
    pub reason: RarReason,
    pub message: String,
}

fn failure(reason: RarReason, message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, RarFailure { reason, message })
}

/// The [`RarFailure`] in `e` or anything it wraps.
pub fn rar_failure<'a>(e: &'a (dyn std::error::Error + 'static)) -> Option<&'a RarFailure> {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(c) = cur {
        if let Some(f) = c.downcast_ref::<RarFailure>() {
            return Some(f);
        }
        if let Some(io) = c.downcast_ref::<io::Error>() {
            if let Some(f) = io.get_ref().and_then(|i| i.downcast_ref::<RarFailure>()) {
                return Some(f);
            }
        }
        cur = c.source();
    }
    None
}

fn reason_of(kind: RarFailKind) -> RarReason {
    match kind {
        RarFailKind::PasswordRequired => RarReason::PasswordRequired,
        RarFailKind::PasswordWrong => RarReason::PasswordWrong,
        RarFailKind::Corrupt => RarReason::Corrupt,
        RarFailKind::MissingVolume => RarReason::MissingVolume,
        RarFailKind::Other => RarReason::Other,
    }
}

/// Map a planning (header) error from `rar_layout` onto a typed failure.
pub(crate) fn plan_error(e: &anyhow::Error) -> RarFailure {
    let m = format!("{e:#}");
    let reason = if m.contains("rar_password_required") {
        RarReason::PasswordRequired
    } else if m.contains("rar_password_wrong") {
        RarReason::PasswordWrong
    } else if m.contains("rar_missing_volume") {
        RarReason::MissingVolume
    } else {
        RarReason::Other
    };
    RarFailure { reason, message: m }
}

pub struct RarSource {
    path: PathBuf,
    password: Option<String>,
    excludes: Vec<String>,
    solid: bool,
    /// Archive-order position of each file, by path.
    ordinal: HashMap<String, u64>,
    /// Archive-order paths (the inverse), to detect a listing/extraction mismatch.
    order: Vec<String>,
    /// Sizes in archive order (measured when the header's was unknown).
    sizes: Vec<u64>,
    /// Manifest id -> path.
    by_id: Vec<String>,
    /// Bytes decoded only to be discarded (solid: the entries a pass skips).
    skipped: AtomicU64,
    /// What a solid walk decodes of the excluded files (`None`: a size is unknown).
    excluded_bytes: Option<u64>,
}

impl RarSource {
    /// Plans the archive from its headers (no decoding). Errors are `RarFailure`
    /// (typed) for password/volume problems and `InvalidData` for a path the manifest
    /// refuses or a duplicate entry.
    pub fn open(
        path: &Path,
        password: Option<&str>,
        excludes: &[String],
    ) -> Result<(Manifest, Self), RarOpenError> {
        Self::open_watched(path, password, excludes, RarLayoutWatch::default())
    }

    /// [`Self::open`] for a job: entries of unknown size are decoded to measure them, which
    /// `watch` can stop and shows the progress of.
    pub fn open_watched(
        path: &Path,
        password: Option<&str>,
        excludes: &[String],
        watch: RarLayoutWatch<'_>,
    ) -> Result<(Manifest, Self), RarOpenError> {
        let layout = rar_layout_watched(path, password, excludes, watch).map_err(|e| {
            if format!("{e:#}").contains(RAR_LAYOUT_CANCELLED) {
                RarOpenError::Cancelled
            } else {
                RarOpenError::Plan(plan_error(&e))
            }
        })?;
        let mut files: BTreeMap<String, u64> = BTreeMap::new();
        // Console filesystems may fold case: two entries that differ only in case
        // would overwrite each other there.
        let mut folded: HashMap<String, String> = HashMap::new();
        let mut ordinal = HashMap::new();
        let mut order = Vec::new();
        let mut mtimes: HashMap<String, u64> = HashMap::new();
        for (i, (p, size)) in layout.files.iter().enumerate() {
            manifest::check_path(p).map_err(|e| RarOpenError::Unsupported(format!("{p}: {e}")))?;
            if let Some(other) = folded.insert(p.to_lowercase(), p.clone()) {
                if other != *p {
                    return Err(RarOpenError::Unsupported(format!(
                        "{other} and {p} differ only in case"
                    )));
                }
            }
            if files.insert(p.clone(), *size).is_some() {
                return Err(RarOpenError::Unsupported(format!(
                    "{p} appears more than once in the archive"
                )));
            }
            ordinal.insert(p.clone(), i as u64);
            if let Some(t) = layout.mtimes.get(i) {
                mtimes.insert(p.clone(), *t);
            }
            order.push(p.clone());
        }
        let mut dirs: BTreeSet<String> = BTreeSet::new();
        for d in layout.dirs.iter().map(String::as_str) {
            manifest::check_path(d).map_err(|e| RarOpenError::Unsupported(format!("{d}: {e}")))?;
            dirs.insert(d.to_owned());
        }
        for p in files.keys() {
            let parts: Vec<&str> = p.split('/').collect();
            let mut parent = String::new();
            for c in &parts[..parts.len() - 1] {
                if !parent.is_empty() {
                    parent.push('/');
                }
                parent.push_str(c);
                dirs.insert(parent.clone());
            }
        }
        if files.keys().any(|p| dirs.contains(p)) {
            return Err(RarOpenError::Unsupported(
                "a path is both a file and a directory".into(),
            ));
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
        for (p, size) in &files {
            entries.push(Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: *size,
                mtime: mtimes.get(p).copied().unwrap_or(0),
                path: p.clone(),
                root: None,
            });
        }
        entries.sort_by(|a, b| a.path.split('/').cmp(b.path.split('/')));
        let by_id = entries.iter().map(|e| e.path.clone()).collect();
        Ok((
            Manifest { entries },
            Self {
                path: path.to_owned(),
                password: password.map(str::to_owned),
                excludes: excludes.to_vec(),
                solid: layout.solid,
                ordinal,
                order,
                sizes: layout.files.iter().map(|(_, s)| *s).collect(),
                by_id,
                skipped: AtomicU64::new(0),
                excluded_bytes: layout.excluded_bytes,
            },
        ))
    }

    pub fn is_solid(&self) -> bool {
        self.solid
    }

    /// Bytes decoded only to be discarded (the "skipping data the console already has"
    /// phase of a solid resume).
    pub fn bytes_skipped(&self) -> u64 {
        self.skipped.load(Ordering::Relaxed)
    }

    /// Test seam: replace the archive-order listing the entry positions come from
    /// (simulates an archive whose listing and extraction orders disagree).
    #[doc(hidden)]
    pub fn with_listing_order_for_test(mut self, order: Vec<String>) -> Self {
        self.order = order;
        self
    }
}

/// Why [`RarSource::open`] failed.
#[derive(Debug)]
pub enum RarOpenError {
    /// Password, volume or header trouble (terminal, typed).
    Plan(RarFailure),
    /// A shape AVA1's manifest cannot carry (duplicate or unsafe path).
    Unsupported(String),
    /// The job was stopped while entries of unknown size were being measured.
    Cancelled,
}

struct Adapter<'a> {
    src: &'a RarSource,
    want: &'a mut dyn FnMut(&str, u64) -> Keep,
    sink: &'a mut dyn EntrySink,
    /// Set when the walk's order disagrees with the listing.
    reordered: Option<String>,
    /// Only a non-solid resume (`start > 0`) skips entries by ordinal; any other
    /// pass binds by path, so a listing/extraction disagreement is harmless.
    enforce_order: bool,
}

impl RarWalkSink for Adapter<'_> {
    fn visit(&mut self, ordinal: u64, path: &str) -> bool {
        if !self.enforce_order {
            return true;
        }
        match self.src.order.get(ordinal as usize) {
            Some(p) if p == path => true,
            _ => {
                self.reordered = Some(path.to_owned());
                false
            }
        }
    }

    fn want(&mut self, ordinal: u64, path: &str, size: u64) -> bool {
        // An unknown-size header carries a sentinel; the layout measured the real one.
        let size = self
            .src
            .sizes
            .get(ordinal as usize)
            .copied()
            .unwrap_or(size);
        !matches!((self.want)(path, size), Keep::Skip)
    }

    fn begin(&mut self, path: &str) -> io::Result<()> {
        self.sink.begin(path)
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

impl SeqSource for RarSource {
    fn pass(
        &self,
        restart: Restart,
        want: &mut dyn FnMut(&str, u64) -> Keep,
        sink: &mut dyn EntrySink,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let start = if self.solid { 0 } else { restart.0 };
        let mut a = Adapter {
            src: self,
            want,
            sink,
            reordered: None,
            enforce_order: !self.solid && start > 0,
        };
        let r = rar_walk(
            &self.path,
            self.password.as_deref(),
            &self.excludes,
            start,
            &mut a,
            cancel,
        );
        if let Some(p) = a.reordered {
            return Err(failure(
                RarReason::Reordered,
                format!(
                    "the archive lists its entries in a different order than it extracts them \
                     ({p}); restart the upload with Overwrite"
                ),
            ));
        }
        match r {
            Ok(()) => Ok(()),
            Err(RarWalkError::Cancelled) => Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "the RAR decode was stopped",
            )),
            Err(RarWalkError::Sink(e)) => Err(e),
            Err(RarWalkError::Failed { kind, message }) => Err(failure(reason_of(kind), message)),
        }
    }

    fn restart_for(&self, file_id: u32) -> Restart {
        if self.solid {
            return Restart::START;
        }
        self.by_id
            .get(file_id as usize)
            .and_then(|p| self.ordinal.get(p))
            .map_or(Restart::START, |o| Restart(*o))
    }

    fn bytes_skipped(&self) -> u64 {
        RarSource::bytes_skipped(self)
    }

    /// Non-solid: nothing (a pass seeks past what it does not want). Solid: every entry the
    /// pass does not want, plus the excluded files, all decoded from the start.
    fn bytes_to_skip(
        &self,
        _restart: Restart,
        want: &mut dyn FnMut(&str, u64) -> Keep,
    ) -> Option<u64> {
        if !self.solid {
            return Some(0);
        }
        let mut n = self.excluded_bytes?;
        for (p, size) in self.order.iter().zip(&self.sizes) {
            if matches!(want(p, *size), Keep::Skip) {
                n = n.saturating_add(*size);
            }
        }
        Some(n)
    }
}

/// The sender takes a `Source` even when a `SeqSource` feeds it (only `close()` is
/// used); a RAR has no random access.
impl Source for RarSource {
    fn open(&self, rel: &str) -> io::Result<Box<dyn ReadAt>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{rel}: a RAR is read sequentially"),
        ))
    }

    fn list(&self, _rel: &str) -> io::Result<Vec<(String, SourceMeta)>> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    fn stat(&self, _rel: &str) -> io::Result<SourceMeta> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}
