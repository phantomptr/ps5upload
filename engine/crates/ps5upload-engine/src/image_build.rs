//! Game images from a game folder: `.exfat`, `.ffpkg` (UFS2) or `.ffpfs` (PFS), optionally
//! compressed into a `.ffpfsc` as they are written.
//!
//! Who writes what. exFAT is ps5upload's own writer (`ps5upload_fpkg::exfat_write`). UFS2 and
//! PFS are quer3q's, from PS5 Dump Forge (`ps5-dump-forge-ufs2`, `ps5-dump-forge-pfs`): each
//! reproduces the layout of the reference tool (newfs/makefs, MkPFS), which is the thing that
//! matters for a filesystem the console mounts. The `.ffpfsc` container a folder is compressed
//! into is Dump Forge's streaming one, so the image never exists uncompressed on disk;
//! compressing an image file that already exists stays with our own `ps5upload_fpkg::ffpfsc`.
//!
//! The ideas taken from Dump Forge, implemented here: every image is planned (names checked,
//! size known) before a byte is written; it is written to a `.partial` and read back through
//! its own reader before it gets its name; and the read-back compares BLAKE3 hashes, not only
//! the list of names and sizes: of every slice by default or on request (full), of a seeded
//! sample otherwise (fast, see `ps5upload_fpkg::sample`). The source is hashed while the writer reads it,
//! so it is normally read once.

use std::collections::HashMap;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use ps5upload_fpkg::ffpfsc_block::Level;
use ps5upload_fpkg::sample::{self, VerifyMode, VerifySummary, SLICE};
use ps5upload_fpkg::source::{SourceFile, SourceTree};

mod to_console;
mod to_server;
#[cfg(test)]
pub(crate) use to_console::tests as to_console_tests;
pub(crate) use to_console::{
    console_stamp, forget_console_stamp, stream_to_console, ConsoleTarget, Streamed,
};
pub(crate) use to_server::{server_stamp, stream_to_server, ServerImage, ToServer};

/// The filesystems a game image can have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageFormat {
    Exfat,
    /// UFS2: what ShadowMountPlus recommends.
    Ffpkg,
    /// PFS: experimental in ShadowMountPlus 1.7; ASCII names only.
    Ffpfs,
}

impl ImageFormat {
    pub(crate) fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("exfat") => Ok(Self::Exfat),
            Some("ffpkg") | Some("ufs") | Some("ufs2") => Ok(Self::Ffpkg),
            Some("ffpfs") | Some("pfs") => Ok(Self::Ffpfs),
            Some(other) => Err(format!(
                "unknown image format {other:?}: use exfat, ffpkg or ffpfs"
            )),
        }
    }

    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Exfat => "exfat",
            Self::Ffpkg => "ffpkg",
            Self::Ffpfs => "ffpfs",
        }
    }
}

/// ShadowMountPlus does not mount a PFS-based image (`.ffpfs`, `.ffpfsc`) whose file name is
/// longer than this, in bytes.
pub(crate) const PFS_NAME_MAX: usize = 63;

/// The longest stem of any image, in bytes: ShadowMountPlus mounts it at
/// `/mnt/shadowmnt/<stem>_<8 hex>`, and FreeBSD's mount path holds 87 bytes (MNAMELEN 88), so a
/// longer name builds and then fails to mount with ENAMETOOLONG. (Rule from PS5 Dump Forge.)
const IMAGE_STEM_MAX: usize = 63;

/// A safe file stem from a game folder's name.
/// Image files a build may read as its source (an .exfat into a .ffpkg, say), and a package
/// made with the debug key, which `source::open` reads like an image.
pub(crate) fn is_image_file(path: &Path) -> bool {
    path.is_file() && has_image_extension(path)
}

/// [`is_image_file`]'s name check alone, for a path on a server or console.
pub(crate) fn has_image_extension(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ps5upload_core::archive_extract::GAME_FILE_EXTS.contains(&e.to_ascii_lowercase().as_str())
    })
}

pub(crate) fn stem_of(source: &Path) -> String {
    let name = source
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // An image source names its output after itself without the image extension.
    let name = match name.rsplit_once('.') {
        Some((base, ext))
            if !base.is_empty()
                && matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "exfat" | "ffpkg" | "ufs2" | "ffpfs" | "ffpfsc" | "pkg"
                ) =>
        {
            base.to_string()
        }
        _ => name,
    };
    // Common batch layout: <game>/game.pkg. Using only the generic file
    // stem makes unrelated queue entries compete for game.exfat.
    let name = if name.eq_ignore_ascii_case("game")
        && source
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("pkg"))
    {
        source
            .parent()
            .and_then(Path::file_name)
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or(name)
    } else {
        name
    };
    let stem: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "\"*/:<>?\\|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let stem = stem.trim().trim_matches('.').to_string();
    if stem.is_empty() {
        "game".into()
    } else {
        stem
    }
}

/// `stem.ext`, the stem cut at a character boundary so the whole name fits `max` bytes.
fn fit(stem: &str, ext: &str, max: Option<usize>) -> String {
    let mut stem = stem.to_string();
    if let Some(max) = max {
        let room = max.saturating_sub(ext.len() + 1);
        while stem.len() > room {
            stem.pop();
        }
        let trimmed = stem.trim_end().trim_end_matches('.').to_string();
        stem = if trimmed.is_empty() {
            "game".into()
        } else {
            trimmed
        };
    }
    format!("{stem}.{ext}")
}

/// The output file name, and for a `.ffpfsc` the name the image carries inside it (which
/// tells ShadowMountPlus its filesystem).
pub(crate) fn output_names(
    source: &Path,
    format: ImageFormat,
    compress: bool,
) -> (String, Option<String>) {
    let stem = stem_of(source);
    if compress {
        let outer = fit(&stem, "ffpfsc", Some(PFS_NAME_MAX));
        let inner = fit(&stem, format.extension(), Some(PFS_NAME_MAX));
        (outer, Some(inner))
    } else {
        let ext = format.extension();
        let max = if format == ImageFormat::Ffpfs {
            PFS_NAME_MAX
        } else {
            IMAGE_STEM_MAX + 1 + ext.len()
        };
        (fit(&stem, ext, Some(max)), None)
    }
}

/// Wraps a source and hashes each file in 8 MiB slices as a writer reads it front to back, so
/// verifying does not read the source a second time. The hashing runs on its own thread, fed a
/// copy of each read, so the writer's thread only reads. A file read any other way is hashed
/// afterwards.
struct HashingTree<'a> {
    inner: &'a mut dyn SourceTree,
    sizes: HashMap<String, u64>,
    /// To the hashing thread: `None` once it is done or gone.
    feed: Option<std::sync::mpsc::SyncSender<(String, u64, Vec<u8>)>>,
    hasher: Option<std::thread::JoinHandle<HashMap<String, Vec<blake3::Hash>>>>,
    /// Time the writer spent in the source's reads.
    read_time: std::time::Duration,
}

/// Reads queued for the hashing thread before a read waits for it: 32 of the writers' 1 MiB.
const HASH_QUEUE: usize = 32;

/// The hashing thread's state: bytes seen so far per file, the slice being hashed, and the
/// slices finished.
#[derive(Default)]
struct SliceHasher {
    sizes: HashMap<String, u64>,
    in_flight: HashMap<String, (u64, blake3::Hasher, Vec<blake3::Hash>)>,
    done: HashMap<String, Vec<blake3::Hash>>,
}

impl SliceHasher {
    fn observe(&mut self, path: &str, offset: u64, bytes: &[u8]) {
        let Some(&size) = self.sizes.get(path) else {
            return;
        };
        if offset == 0 {
            self.in_flight
                .insert(path.to_string(), (0, blake3::Hasher::new(), Vec::new()));
        }
        let finished = match self.in_flight.get_mut(path) {
            Some((next, hasher, slices)) if *next == offset => {
                let mut rest = bytes;
                while !rest.is_empty() && *next < size {
                    let room = (SLICE - *next % SLICE) as usize;
                    let (head, tail) = rest.split_at(room.min(rest.len()));
                    hash_update(hasher, head);
                    *next += head.len() as u64;
                    if *next % SLICE == 0 || *next >= size {
                        slices.push(std::mem::replace(hasher, blake3::Hasher::new()).finalize());
                    }
                    rest = tail;
                }
                *next >= size
            }
            // Out of order: forget it, it is hashed afterwards.
            Some(_) => {
                self.in_flight.remove(path);
                return;
            }
            None => return,
        };
        if finished {
            if let Some((_, _, slices)) = self.in_flight.remove(path) {
                self.done.insert(path.to_string(), slices);
            }
        }
    }
}

impl<'a> HashingTree<'a> {
    fn new(inner: &'a mut dyn SourceTree) -> Self {
        let sizes: HashMap<String, u64> = inner
            .files()
            .iter()
            .map(|f| (f.path.clone(), f.size))
            .collect();
        let (feed, reads) = std::sync::mpsc::sync_channel::<(String, u64, Vec<u8>)>(HASH_QUEUE);
        let mut state = SliceHasher {
            sizes: sizes.clone(),
            ..Default::default()
        };
        let lane = ps5upload_fpkg::workshop::Lane::current();
        let hasher = std::thread::Builder::new()
            .name("image-hash".into())
            .spawn(move || {
                lane.enter();
                for (path, offset, bytes) in reads {
                    ps5upload_fpkg::workshop::sync_thread_priority();
                    state.observe(&path, offset, &bytes);
                }
                state.done
            })
            .ok();
        Self {
            inner,
            sizes,
            feed: hasher.is_some().then_some(feed),
            hasher,
            read_time: std::time::Duration::ZERO,
        }
    }

    fn observe(&mut self, path: &str, offset: u64, bytes: &[u8]) {
        if !self.sizes.contains_key(path) {
            return;
        }
        if let Some(feed) = &self.feed {
            // A gone hasher only means these files are hashed afterwards.
            if feed
                .send((path.to_string(), offset, bytes.to_vec()))
                .is_err()
            {
                self.feed = None;
            }
        }
    }

    /// Every file's slice hashes: the ones seen whole while writing, the rest read now.
    fn hashes(mut self, cancel: &AtomicBool) -> Result<HashMap<String, Vec<blake3::Hash>>, String> {
        self.hashes_now(cancel)
    }

    /// [`Self::hashes`], keeping the tree: what it reads from now on is no longer hashed (an
    /// image written again over a dropped connection reuses these).
    fn hashes_now(
        &mut self,
        cancel: &AtomicBool,
    ) -> Result<HashMap<String, Vec<blake3::Hash>>, String> {
        self.feed = None;
        let mut done = match self.hasher.take().map(|h| h.join()) {
            Some(Ok(done)) => done,
            Some(Err(_)) => return Err("the source hashing thread failed".into()),
            None => HashMap::new(),
        };
        let missing: Vec<(String, u64)> = self
            .sizes
            .iter()
            .filter(|(p, _)| !done.contains_key(*p))
            .map(|(p, s)| (p.clone(), *s))
            .collect();
        for (path, size) in missing {
            let mut slices = Vec::new();
            hash_slices(
                &mut *self.inner,
                &path,
                size,
                0,
                size.div_ceil(SLICE),
                DEFAULT_VERIFY_CHUNK,
                cancel,
                &mut |_, h| {
                    slices.push(h);
                    Ok(())
                },
            )?;
            done.insert(path, slices);
        }
        Ok(done)
    }
}

impl SourceTree for HashingTree<'_> {
    fn files(&self) -> &[SourceFile] {
        self.inner.files()
    }
    fn read(&mut self, path: &str) -> ps5upload_fpkg::Result<Vec<u8>> {
        let t = std::time::Instant::now();
        let all = self.inner.read(path)?;
        self.read_time += t.elapsed();
        self.observe(path, 0, &all);
        Ok(all)
    }
    fn read_range(
        &mut self,
        path: &str,
        offset: u64,
        len: usize,
    ) -> ps5upload_fpkg::Result<Vec<u8>> {
        let t = std::time::Instant::now();
        let bytes = self.inner.read_range(path, offset, len)?;
        self.read_time += t.elapsed();
        self.observe(path, offset, &bytes);
        Ok(bytes)
    }
    fn empty_dirs(&self) -> &[String] {
        self.inner.empty_dirs()
    }
    fn describe(&self) -> String {
        self.inner.describe()
    }
}

/// How much of a file the read-back check reads per call when the caller does not say.
/// Measured on a 10 GB exFAT image in page cache: reads alone go 7 GiB/s at 1–8 MiB, hashing
/// on all cores peaks at 8 MiB (3.7 GiB/s against 1.3 single-threaded); 1–32 MiB are within
/// 15% of each other, and the FAT-walk fix in the exFAT reader mattered far more.
pub(crate) const DEFAULT_VERIFY_CHUNK: u64 = 8 << 20;
/// What a request may ask for, in KiB: 1 MiB to 64 MiB.
pub(crate) const VERIFY_CHUNK_KIB_RANGE: std::ops::RangeInclusive<u32> = 1024..=65536;

/// The read-back chunk a request asks for (`verify_chunk_kib`), or the default.
pub(crate) fn verify_chunk(kib: Option<u32>) -> Result<u64, String> {
    match kib {
        None => Ok(DEFAULT_VERIFY_CHUNK),
        Some(k) if VERIFY_CHUNK_KIB_RANGE.contains(&k) => Ok(u64::from(k) << 10),
        Some(k) => Err(format!(
            "verify_chunk_kib {k} is not {} to {}",
            VERIFY_CHUNK_KIB_RANGE.start(),
            VERIFY_CHUNK_KIB_RANGE.end()
        )),
    }
}

/// Feeds `bytes` to a hasher, on every core when there is enough to split (BLAKE3's own
/// advice: multi-threading pays from about 128 KiB).
fn hash_update(h: &mut blake3::Hasher, bytes: &[u8]) {
    if bytes.len() >= 128 << 10 {
        h.update_rayon(bytes);
    } else {
        h.update(bytes);
    }
}

/// The BLAKE3 hash of each [`SLICE`] of a file (`size` bytes) from slice `from` up to `to`,
/// handed to `on_slice` as each one ends. Reads are `chunk` bytes and may span slices, so a
/// read-back chunk above the slice size still reads that much at a time.
#[allow(clippy::too_many_arguments)]
fn hash_slices(
    tree: &mut dyn SourceTree,
    path: &str,
    size: u64,
    from: u64,
    to: u64,
    chunk: u64,
    cancel: &AtomicBool,
    on_slice: &mut dyn FnMut(u64, blake3::Hash) -> Result<(), String>,
) -> Result<(), String> {
    let mut h = blake3::Hasher::new();
    let end = (to * SLICE).min(size);
    let mut at = from * SLICE;
    while at < end {
        if ps5upload_fpkg::workshop::checkpoint(cancel) {
            return Err("cancelled".into());
        }
        let want = (end - at).min(chunk) as usize;
        let buf = tree
            .read_range(path, at, want)
            .map_err(|e| format!("{path}: {e}"))?;
        if buf.is_empty() {
            return Err(format!("{path} ended at {at} of its {end} bytes"));
        }
        let mut rest = &buf[..(buf.len() as u64).min(end - at) as usize];
        while !rest.is_empty() {
            let room = (SLICE - at % SLICE) as usize;
            let (head, tail) = rest.split_at(room.min(rest.len()));
            hash_update(&mut h, head);
            at += head.len() as u64;
            if at.is_multiple_of(SLICE) || at >= end {
                let done = std::mem::replace(&mut h, blake3::Hasher::new()).finalize();
                on_slice((at - 1) / SLICE, done)?;
            }
            rest = tail;
        }
    }
    Ok(())
}

/// Opens a finished image through the reader for its kind.
fn open_image(
    path: &Path,
    format: ImageFormat,
    compressed: bool,
) -> Result<Box<dyn SourceTree>, String> {
    let label = path.display().to_string();
    let opened: ps5upload_fpkg::Result<Box<dyn SourceTree>> = if compressed {
        std::fs::File::open(path)
            .map_err(ps5upload_fpkg::Error::from)
            .and_then(|f| {
                ps5_dump_forge_pfs::open_ffpfsc(Box::new(std::io::BufReader::new(f)), &label)
                    .map(|(tree, _)| tree)
            })
    } else {
        match format {
            ImageFormat::Exfat => ps5upload_fpkg::exfat::ExFatSource::open(path)
                .map(|t| Box::new(t) as Box<dyn SourceTree>),
            ImageFormat::Ffpkg => ps5upload_fpkg::ufs2_source::Ufs2Source::open(path)
                .map(|t| Box::new(t) as Box<dyn SourceTree>),
            ImageFormat::Ffpfs => ps5_dump_forge_pfs::PfsSource::open(path)
                .map(|t| Box::new(t) as Box<dyn SourceTree>),
        }
    };
    opened.map_err(|e| format!("the image could not be read back: {e}"))
}

/// Reads the image back and compares it with the source: the same files at the same sizes and
/// the same empty folders always, and the BLAKE3 hash of each 8 MiB slice of file data the
/// `mode` reads (all of it, or a seeded sample). `chunk` bytes per read.
#[allow(clippy::too_many_arguments)]
fn verify(
    image: &mut dyn SourceTree,
    want: &HashMap<String, Vec<blake3::Hash>>,
    want_sizes: &HashMap<String, u64>,
    want_empty: &[String],
    mode: VerifyMode,
    chunk: u64,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<VerifySummary, String> {
    let got: Vec<SourceFile> = image.files().to_vec();
    if got.len() != want_sizes.len() {
        return Err(format!(
            "the image read back with {} files where the folder has {}",
            got.len(),
            want_sizes.len()
        ));
    }
    let mut got_empty: Vec<String> = image
        .empty_dirs()
        .iter()
        .map(|d| d.trim_matches('/').to_string())
        .collect();
    let mut want_empty: Vec<String> = want_empty
        .iter()
        .map(|d| d.trim_matches('/').to_string())
        .collect();
    got_empty.sort();
    want_empty.sort();
    if got_empty != want_empty {
        return Err(format!(
            "the image read back with empty folders {got_empty:?} where the folder has {want_empty:?}"
        ));
    }
    for f in &got {
        let Some(&size) = want_sizes.get(&f.path) else {
            return Err(format!(
                "the image holds {} which the folder does not",
                f.path
            ));
        };
        if size != f.size {
            return Err(format!(
                "{} is {} bytes in the image and {size} in the folder",
                f.path, f.size
            ));
        }
    }
    let listed: Vec<(String, u64)> = got.iter().map(|f| (f.path.clone(), f.size)).collect();
    let summary = sample::summary(&listed, mode);
    // The slices to read: every one, or the ones the sampling plan names.
    let mut slices: Vec<(usize, u64)> = Vec::new();
    match mode {
        VerifyMode::Full => {
            for (i, f) in got.iter().enumerate() {
                slices.extend((0..f.size.div_ceil(SLICE)).map(|s| (i, s)));
            }
        }
        VerifyMode::Fast { seed, seam } => {
            for s in sample::plan(&listed, seed, seam) {
                match s.slice {
                    Some(n) => slices.push((s.file_idx, n)),
                    None => {
                        let n = got[s.file_idx].size.div_ceil(SLICE);
                        slices.extend((0..n).map(|k| (s.file_idx, k)));
                    }
                }
            }
        }
    }
    let total = summary.checked_bytes;
    let mut done = 0u64;
    progress(0, total);
    // Runs of consecutive slices of one file are read together, `chunk` bytes at a time.
    let mut runs: Vec<(usize, u64, u64)> = Vec::new();
    for (i, s) in slices {
        match runs.last_mut() {
            Some((ri, _, to)) if *ri == i && *to == s => *to += 1,
            _ => runs.push((i, s, s + 1)),
        }
    }
    for (i, from, to) in runs {
        let f = &got[i];
        let wanted = want.get(&f.path);
        hash_slices(
            image,
            &f.path,
            f.size,
            from,
            to,
            chunk,
            cancel,
            &mut |s, h| {
                let (at, len) = (s * SLICE, SLICE.min(f.size - s * SLICE));
                if wanted.and_then(|w| w.get(s as usize)) != Some(&h) {
                    let range = if f.size > SLICE {
                        format!(" (bytes {at}..{})", at + len)
                    } else {
                        String::new()
                    };
                    return Err(format!(
                        "{} differs between the image and the folder{range}",
                        f.path
                    ));
                }
                done += len;
                progress(done, total);
                Ok(())
            },
        )?;
    }
    Ok(summary)
}

/// A libSceAmpr game's folder with its `ampr_emu.index` regenerated from the files that go
/// into the image (#449). The index ampr_emu reads must list every file at its size; a dump's
/// copy is often stale (files added, trimmed or resized since), and a plain image used to
/// carry it as it was, which crashes the game once it touches a file the index has wrong.
/// LZ4 builds regenerate theirs in `ampr_pack::image::prepare`; this is the plain build's.
pub(crate) struct AmprIndexed<'a> {
    inner: &'a mut dyn SourceTree,
    files: Vec<SourceFile>,
    index: Vec<u8>,
}

impl<'a> AmprIndexed<'a> {
    /// `None` when the game does not read through libSceAmpr and ships no index: nothing to
    /// regenerate, the folder goes in as it is.
    #[cfg(test)]
    pub(crate) fn wrap(inner: &'a mut dyn SourceTree) -> Result<Option<Self>, String> {
        let mtime = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        Self::wrap_at(inner, mtime)
    }

    /// [`Self::wrap`] with every file stamped `mtime`.
    pub(crate) fn wrap_at(
        inner: &'a mut dyn SourceTree,
        mtime: i64,
    ) -> Result<Option<Self>, String> {
        let name = ps5upload_fpkg::ampr_index::NAME;
        // An LZ4-packed game's index also lists the files inside its packs: it stays as it is.
        if ps5upload_fpkg::source::lz4_packed(inner).map_err(|e| e.to_string())? {
            return Ok(None);
        }
        let had = inner
            .files()
            .iter()
            .any(|f| f.path.eq_ignore_ascii_case(name));
        if !had && !ps5upload_fpkg::source::imports_ampr(inner, "eboot.bin") {
            return Ok(None);
        }
        let mut files: Vec<SourceFile> = inner
            .files()
            .iter()
            .filter(|f| !f.path.eq_ignore_ascii_case(name))
            .cloned()
            .collect();
        let listed = ps5upload_fpkg::ampr_index::listing(&files);
        let index = ps5upload_fpkg::ampr_index::build(&listed, mtime)?;
        files.push(SourceFile {
            path: name.to_string(),
            size: index.len() as u64,
        });
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Some(Self {
            inner,
            files,
            index,
        }))
    }
}

impl AmprIndexed<'_> {
    /// The index this image carries.
    #[cfg(test)]
    pub(crate) fn index(&self) -> &[u8] {
        &self.index
    }
}

impl SourceTree for AmprIndexed<'_> {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }
    fn empty_dirs(&self) -> &[String] {
        self.inner.empty_dirs()
    }
    fn read(&mut self, path: &str) -> ps5upload_fpkg::Result<Vec<u8>> {
        if path.eq_ignore_ascii_case(ps5upload_fpkg::ampr_index::NAME) {
            return Ok(self.index.clone());
        }
        self.inner.read(path)
    }
    fn read_range(
        &mut self,
        path: &str,
        offset: u64,
        len: usize,
    ) -> ps5upload_fpkg::Result<Vec<u8>> {
        if path.eq_ignore_ascii_case(ps5upload_fpkg::ampr_index::NAME) {
            let start = (offset as usize).min(self.index.len());
            let end = start.saturating_add(len).min(self.index.len());
            return Ok(self.index[start..end].to_vec());
        }
        self.inner.read_range(path, offset, len)
    }
    fn describe(&self) -> String {
        format!("{} (ampr_emu.index regenerated)", self.inner.describe())
    }
}

/// What a build wrote.
pub(crate) struct ImageBuilt {
    pub image_bytes: u64,
    pub files: u64,
    pub detail: String,
}

/// A writer for one planned image, so the same code fills a file or a `.ffpfsc` stream.
enum Planned {
    Exfat(ps5upload_fpkg::exfat_write::ExfatPlan),
    Ffpkg(ps5_dump_forge_ufs2::Layout),
    Ffpfs(ps5_dump_forge_pfs::Layout),
}

impl Planned {
    /// `time`: what a PFS image's inodes carry; `None` is now.
    fn plan(
        format: ImageFormat,
        tree: &dyn SourceTree,
        time: Option<i64>,
        cancel: &AtomicBool,
    ) -> Result<Self, String> {
        let e = |e: ps5upload_fpkg::Error| e.to_string();
        Ok(match format {
            ImageFormat::Exfat => {
                Self::Exfat(ps5upload_fpkg::exfat_write::plan_exfat(tree, None, 0).map_err(e)?)
            }
            ImageFormat::Ffpkg => Self::Ffpkg(
                ps5_dump_forge_ufs2::plan(tree, &Default::default(), cancel).map_err(e)?,
            ),
            ImageFormat::Ffpfs => {
                let opts = ps5_dump_forge_pfs::Options { time };
                Self::Ffpfs(ps5_dump_forge_pfs::plan(tree, &opts, cancel).map_err(e)?)
            }
        })
    }

    fn image_size(&self) -> u64 {
        match self {
            Self::Exfat(p) => p.image_bytes,
            Self::Ffpkg(l) => l.image_size,
            Self::Ffpfs(l) => l.image_size,
        }
    }

    fn free_bytes(&self) -> Option<u64> {
        match self {
            Self::Exfat(p) => Some(p.free_bytes),
            Self::Ffpkg(l) => Some(l.free_bytes),
            Self::Ffpfs(_) => None,
        }
    }

    /// Writes the image into `out`, which drops the first `skip` bytes (on disk already, from a
    /// crashed build): file data that ends by then is not read from the source.
    fn write<W: Write + Seek>(
        &self,
        tree: &mut dyn SourceTree,
        out: &mut W,
        skip: u64,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64, u64),
    ) -> ps5upload_fpkg::Result<u64> {
        match self {
            Self::Exfat(p) => ps5upload_fpkg::exfat_write::write_exfat_after(
                p,
                tree,
                out,
                skip,
                progress,
                &|| ps5upload_fpkg::workshop::checkpoint(cancel),
            ),
            Self::Ffpkg(l) => {
                ps5_dump_forge_ufs2::write_after(tree, l, out, skip, cancel, progress)
                    .map(|r| r.image_size)
            }
            Self::Ffpfs(l) => ps5_dump_forge_pfs::write_after(tree, l, out, skip, cancel, progress)
                .map(|r| r.image_size),
        }
    }
}

/// How much of an image gets written between checkpoints (of a `.ffpfsc`, of the container).
const CHECKPOINT: u64 = 1 << 30;

/// A checkpoint older than this is given up on: its partial image goes in the next sweep.
pub(crate) const RESUME_KEPT: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// What a build left to go on from after a crash, `<output>.partial.resume`. Any format, plain or
/// in a `.ffpfsc`: each writer lays its image out from the files alone, writes it front to back,
/// and pins what would differ from one run to the next (a PFS image's time, a generated index's
/// mtime) to the stamp beside the checkpoint ([`index_stamp`]). A cancel or a failed build
/// removes it with the partial file; only a crash or a killed engine leaves one.
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
struct Checkpoint {
    /// The files, the layout, the format and its options; anything else starts over.
    key: String,
    /// The bytes on disk for certain.
    done: u64,
    /// Of a `.ffpfsc`: the inner image's 64 KiB blocks stored in those bytes, whose offsets are in
    /// the container's table in the file.
    #[serde(default)]
    blocks: u64,
}

pub(crate) fn checkpoint_path(partial: &Path) -> PathBuf {
    let mut name = partial.file_name().unwrap_or_default().to_os_string();
    name.push(".resume");
    partial.with_file_name(name)
}

/// What a checkpoint must match to be gone on from: this engine's writers, the format and its
/// options, the time pinned into the image, every file at its size and the image's size.
fn resume_key(
    format: ImageFormat,
    inner_name: Option<&str>,
    level: Level,
    time: Option<i64>,
    sizes: &HashMap<String, u64>,
    image_size: u64,
) -> String {
    let mut rows: Vec<_> = sizes.iter().collect();
    rows.sort();
    let mut h = blake3::Hasher::new();
    h.update(env!("CARGO_PKG_VERSION").as_bytes());
    h.update(format.extension().as_bytes());
    if let Some(name) = inner_name {
        h.update(b"\0ffpfsc\0");
        h.update(name.as_bytes());
        h.update(level.name().as_bytes());
    }
    h.update(&time.unwrap_or(0).to_le_bytes());
    h.update(&image_size.to_le_bytes());
    for (path, size) in rows {
        h.update(path.as_bytes());
        h.update(&size.to_le_bytes());
    }
    h.finalize().to_hex().to_string()
}

/// Where to go on from, when the partial file holds what its checkpoint says for this plan.
fn resume_point(partial: &Path, key: &str) -> Option<Checkpoint> {
    let saved: Checkpoint =
        serde_json::from_slice(&std::fs::read(checkpoint_path(partial)).ok()?).ok()?;
    let on_disk = std::fs::metadata(partial).ok()?.len();
    (saved.key == key && saved.done > 0 && saved.done <= on_disk).then_some(saved)
}

/// Says in the checkpoint file that `done` bytes (`blocks` of a `.ffpfsc`) are on disk. Only
/// worth having, not needed: a locked file costs a later resume, not this build.
fn save_checkpoint(path: &Path, key: &str, done: u64, blocks: u64) {
    let saved = serde_json::to_vec(&Checkpoint {
        key: key.to_string(),
        done,
        blocks,
    })
    .map_err(std::io::Error::other)
    .and_then(|bytes| {
        let tmp = path.with_extension("resume.tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)
    });
    if let Err(e) = saved {
        crate::log_at!(
            Warn,
            "convert",
            "image build: no checkpoint at {done} bytes: {e}"
        );
    }
}

/// How a resumable build makes checkpoints: every `every` bytes, and in a test, a crash once
/// `stop_after` bytes are written (the partial file and its checkpoint are left as a killed
/// engine leaves them).
pub(crate) struct Checkpoints {
    every: u64,
    stop_after: Option<u64>,
    stopped: AtomicBool,
}

impl Checkpoints {
    fn new() -> Self {
        Self {
            every: CHECKPOINT,
            stop_after: None,
            stopped: AtomicBool::new(false),
        }
    }

    /// The crash a test asked for, once `at` bytes are out.
    fn crash(&self, at: u64) -> std::io::Result<()> {
        if self.stop_after.is_some_and(|n| at >= n) {
            self.stopped.store(true, Ordering::Relaxed);
            return Err(std::io::Error::other("stopped as a crash would (test)"));
        }
        Ok(())
    }
}

/// Drops the first `skip` bytes it is given, writes the rest, and every `every` bytes makes them
/// durable and says so in the checkpoint file. It seeks forward only, as the writers do: a seek
/// that lands at or before `skip` moves nothing in the file, which already sits there.
struct CheckpointWriter<'a> {
    out: BufWriter<&'a mut std::fs::File>,
    skip: u64,
    pos: u64,
    /// The furthest byte written: the end a writer that checks it starts empty sees.
    end: u64,
    since: u64,
    path: PathBuf,
    key: String,
    every: u64,
    crash: Option<&'a Checkpoints>,
}

impl CheckpointWriter<'_> {
    fn checkpoint(&mut self) -> std::io::Result<()> {
        self.out.flush()?;
        self.out.get_ref().sync_data()?;
        self.since = 0;
        save_checkpoint(&self.path, &self.key, self.pos, 0);
        Ok(())
    }
}

impl Write for CheckpointWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(c) = self
            .crash
            .filter(|c| c.stop_after.is_some_and(|n| self.pos >= n))
        {
            // What the page cache held when the engine died: on disk, past the checkpoint.
            self.out.flush()?;
            c.crash(self.pos)?;
        }
        if self.pos < self.skip {
            let n = (self.skip - self.pos).min(buf.len() as u64) as usize;
            self.pos += n as u64;
            self.end = self.end.max(self.pos);
            return Ok(n);
        }
        let n = self.out.write(buf)?;
        self.pos += n as u64;
        self.end = self.end.max(self.pos);
        self.since += n as u64;
        if self.since >= self.every {
            self.checkpoint()?;
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

impl Seek for CheckpointWriter<'_> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => self.end.checked_add_signed(d),
        };
        let Some(t) = target.filter(|t| *t >= self.pos) else {
            return Err(std::io::Error::other(
                "an image going on from a checkpoint is written forward only",
            ));
        };
        // The file sits at the furthest of `pos` and `skip`; it moves only to go past both.
        if t > self.pos.max(self.skip) {
            self.out.seek(SeekFrom::Start(t))?;
        }
        self.pos = t;
        Ok(t)
    }
}

/// Builds `source` as an image at `output`: planned, written to `<output>.partial` (straight
/// into a `.ffpfsc` container when `inner_name` is given), read back and hash-checked, then
/// renamed. `stage(id, done, total)` reports `plan`, `write` (or `compress`) and `verify`.
/// `verify_chunk`: how much the read-back check reads per call (see [`verify_chunk`]); `mode`:
/// how much of the data it reads back.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build(
    format: ImageFormat,
    inner_name: Option<&str>,
    source: &mut dyn SourceTree,
    output: &Path,
    mode: VerifyMode,
    verify_chunk: u64,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<ImageBuilt, String> {
    build_at(
        format,
        inner_name,
        Level::default(),
        source,
        output,
        mode,
        verify_chunk,
        cancel,
        stage,
    )
}

/// [`build`], a `.ffpfsc` compressed at `level` (ignored for a plain image).
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_at(
    format: ImageFormat,
    inner_name: Option<&str>,
    level: Level,
    source: &mut dyn SourceTree,
    output: &Path,
    mode: VerifyMode,
    verify_chunk: u64,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<ImageBuilt, String> {
    build_any(
        format,
        inner_name,
        level,
        None,
        source,
        output,
        mode,
        verify_chunk,
        cancel,
        stage,
        None,
    )
}

/// [`build_at`] that goes on from a crash's checkpoint, for a source that gives the same bytes
/// each time, in any format, plain or compressed. Not for packs: their archive IDs are new each
/// run. `time` is [`index_stamp`]'s: a generated index in the source and a PFS image's inodes
/// carry it, so going on gives the same bytes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_resumable(
    format: ImageFormat,
    inner_name: Option<&str>,
    level: Level,
    time: i64,
    source: &mut dyn SourceTree,
    output: &Path,
    mode: VerifyMode,
    verify_chunk: u64,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<ImageBuilt, String> {
    build_any(
        format,
        inner_name,
        level,
        Some(time),
        source,
        output,
        mode,
        verify_chunk,
        cancel,
        stage,
        Some(&Checkpoints::new()),
    )
}

/// The time a generated `ampr_emu.index` and a PFS image carry in the image at `partial`: the one
/// the crashed build used, so going on gives the same bytes, else now, saved for the next try.
pub(crate) fn index_stamp(partial: &Path) -> i64 {
    let path = stamp_path(partial);
    if checkpoint_path(partial).is_file() {
        if let Some(saved) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| t.trim().parse().ok())
        {
            return saved;
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let _ = std::fs::write(&path, now.to_string());
    now
}

fn stamp_path(partial: &Path) -> PathBuf {
    let mut name = partial.file_name().unwrap_or_default().to_os_string();
    name.push(".stamp");
    partial.with_file_name(name)
}

/// A build's checkpoint and what goes with it.
pub(crate) fn remove_checkpoint(partial: &Path) {
    let _ = std::fs::remove_file(checkpoint_path(partial));
    let _ = std::fs::remove_file(stamp_path(partial));
}

/// One build, and a second from the start when going on from a checkpoint failed: the source
/// changed behind it in a way the key cannot see (a file edited to the same size), or the bytes
/// on disk were not what the checkpoint said. The full read-back after going on is what finds it.
#[allow(clippy::too_many_arguments)]
fn build_any(
    format: ImageFormat,
    inner_name: Option<&str>,
    level: Level,
    time: Option<i64>,
    source: &mut dyn SourceTree,
    output: &Path,
    mode: VerifyMode,
    verify_chunk: u64,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
    checkpoints: Option<&Checkpoints>,
) -> Result<ImageBuilt, String> {
    let (result, resumed) = build_once(
        format,
        inner_name,
        level,
        time,
        source,
        output,
        mode,
        verify_chunk,
        cancel,
        stage,
        checkpoints,
    );
    let stopped = checkpoints.is_some_and(|c| c.stopped.load(Ordering::Relaxed));
    match result {
        Err(e) if resumed && !stopped && !cancel.load(Ordering::Relaxed) => {
            crate::log_at!(
                Warn,
                "convert",
                "image build: {e}. Building {} again now",
                output.display()
            );
            // The time stays the one the image was planned with; a crash in this build goes
            // on with it.
            let partial = PathBuf::from(format!("{}.partial", output.display()));
            if let Some(t) = time {
                let _ = std::fs::write(stamp_path(&partial), t.to_string());
            }
            build_once(
                format,
                inner_name,
                level,
                time,
                source,
                output,
                mode,
                verify_chunk,
                cancel,
                stage,
                checkpoints,
            )
            .0
        }
        r => r,
    }
}

/// [`build_any`]'s one try, and whether it went on from a checkpoint.
#[allow(clippy::too_many_arguments)]
fn build_once(
    format: ImageFormat,
    inner_name: Option<&str>,
    level: Level,
    time: Option<i64>,
    source: &mut dyn SourceTree,
    output: &Path,
    mode: VerifyMode,
    verify_chunk: u64,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
    checkpoints: Option<&Checkpoints>,
) -> (Result<ImageBuilt, String>, bool) {
    if source.files().is_empty() {
        return (
            Err(format!("{} has no files in it", source.describe())),
            false,
        );
    }
    let started = std::time::Instant::now();
    stage("plan", 0, 0);
    let planned = match Planned::plan(format, &*source, time, cancel) {
        Ok(p) => p,
        Err(e) => return (settle(Err(e), cancel.load(Ordering::Relaxed)), false),
    };
    let want_sizes: HashMap<String, u64> = source
        .files()
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    let want_empty: Vec<String> = source.empty_dirs().to_vec();
    let data: u64 = want_sizes.values().sum();

    // The caller has claimed this name (`fpkg_api::Writing`), clearing any leftover; a file
    // here now is another build's, and `create_new` refuses it rather than wreck it.
    let partial = PathBuf::from(format!("{}.partial", output.display()));
    let key = resume_key(
        format,
        inner_name,
        level,
        time,
        &want_sizes,
        planned.image_size(),
    );
    let resume = checkpoints
        .and_then(|_| resume_point(&partial, &key))
        // A `.ffpfsc` goes on from whole blocks.
        .filter(|c| inner_name.is_none() || c.blocks > 0);
    // The key can't see a file edited to the same size: going on, every byte gets checked.
    let mode = if resume.is_some() {
        VerifyMode::Full
    } else {
        mode
    };
    if resume.is_none() {
        // A checkpoint for something else: its partial file is this name's leftover.
        if checkpoint_path(&partial).is_file() {
            let _ = std::fs::remove_file(&partial);
            let _ = std::fs::remove_file(checkpoint_path(&partial));
        }
    }
    let resumed = resume.is_some();
    // Opened before the cleanup below applies: a file that is not ours is never removed.
    let opened = if let Some(c) = &resume {
        crate::engine_log::record(
            "info",
            format!(
                "image build: going on from {} bytes in {}",
                c.done,
                partial.display()
            ),
        );
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&partial)
            .and_then(|mut f| {
                // A `.ffpfsc` cuts its own file where its table says.
                if inner_name.is_none() {
                    f.set_len(c.done)?;
                    f.seek(SeekFrom::Start(c.done))?;
                }
                Ok(f)
            })
    } else {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&partial)
    };
    let mut file = match opened {
        Ok(f) => f,
        Err(e) => return (Err(format!("{}: {e}", partial.display())), false),
    };
    let skip = resume.as_ref().map_or(0, |c| c.done);
    let skip_blocks = resume.as_ref().map_or(0, |c| c.blocks);
    let result = (|| -> Result<ImageBuilt, String> {
        let planned_at = started.elapsed();
        let mut hashing = HashingTree::new(source);
        let write_stage = if inner_name.is_some() {
            "compress"
        } else {
            "write"
        };
        let mut report = |done: u64, total: u64| stage(write_stage, done, total);
        let mut wrap_opts = ps5_dump_forge_pfs::WrapOptions::at(level);
        wrap_opts.time = time;
        let (image_bytes, stored, wrapped) = match (inner_name, checkpoints) {
            (None, Some(c)) => {
                let mut out = CheckpointWriter {
                    out: BufWriter::with_capacity(8 << 20, &mut file),
                    skip,
                    pos: 0,
                    end: 0,
                    since: 0,
                    path: checkpoint_path(&partial),
                    key: key.clone(),
                    every: c.every,
                    crash: c.stop_after.is_some().then_some(c),
                };
                let n = planned
                    .write(&mut hashing, &mut out, skip, cancel, &mut report)
                    .map_err(|e| e.to_string())?;
                out.flush().map_err(|e| e.to_string())?;
                (n, n, None)
            }
            (None, None) => {
                let mut out = BufWriter::with_capacity(8 << 20, &mut file);
                let n = planned
                    .write(&mut hashing, &mut out, 0, cancel, &mut report)
                    .map_err(|e| e.to_string())?;
                out.flush().map_err(|e| e.to_string())?;
                (n, n, None)
            }
            (Some(name), Some(c)) => {
                let path = checkpoint_path(&partial);
                let saved = |blocks: u64, done: u64| {
                    save_checkpoint(&path, &key, done, blocks);
                    c.crash(done)
                };
                let resume = ps5_dump_forge_pfs::Resume {
                    blocks: skip_blocks,
                    every: c.every,
                    saved: &saved,
                };
                let inner_skip = skip_blocks * ps5_dump_forge_pfs::BLOCK;
                let (n, wrapped) = ps5_dump_forge_pfs::wrap_resumable(
                    name,
                    planned.image_size(),
                    &mut file,
                    &wrap_opts,
                    cancel,
                    &resume,
                    |s| planned.write(&mut hashing, s, inner_skip, cancel, &mut report),
                )
                .map_err(|e| e.to_string())?;
                (n, wrapped.image_size, Some(wrapped))
            }
            (Some(name), None) => {
                let (n, wrapped) = ps5_dump_forge_pfs::wrap(
                    name,
                    planned.image_size(),
                    &mut file,
                    &wrap_opts,
                    cancel,
                    |s| planned.write(&mut hashing, s, 0, cancel, &mut report),
                )
                .map_err(|e| e.to_string())?;
                (n, wrapped.image_size, Some(wrapped))
            }
        };
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        let read_time = hashing.read_time;
        let hashes = hashing.hashes(cancel)?;

        let written_at = started.elapsed();
        let mut image = open_image(&partial, format, inner_name.is_some())?;
        let summary = verify(
            &mut *image,
            &hashes,
            &want_sizes,
            &want_empty,
            mode,
            verify_chunk,
            cancel,
            &mut |d, t| stage("verify", d, t),
        )?;
        drop(image);
        std::fs::rename(&partial, output).map_err(|e| e.to_string())?;
        remove_checkpoint(&partial);
        crate::engine_log::record("info", format!("verify: {}", log_line(&summary)));
        crate::engine_log::record("info", format!(
            "image build: format={} compressed={} files={} data_bytes={} output_bytes={} plan_ms={} write_ms={} read_ms={} verify_ms={} verify_mode={mode:?} resumed_from={skip}",
            format.extension(), inner_name.is_some(), want_sizes.len(), data, stored,
            planned_at.as_millis(), (written_at - planned_at).as_millis(), read_time.as_millis(),
            (started.elapsed() - written_at).as_millis(),
        ));
        if let Some(w) = &wrapped {
            crate::engine_log::record(
                "info",
                format!(
                    "ffpfsc: compression={} {} read_ms={} verify_ms={} verify_mode={} block_check=inflate-every-compressed-block",
                    level.name(),
                    wrap_log_line(&wrap_opts, w),
                    read_time.as_millis(),
                    (started.elapsed() - written_at).as_millis(),
                    log_line(&summary),
                ),
            );
        }
        let mut detail = format!(
            "{}, {} files, {data} data bytes, {}",
            format.extension(),
            want_sizes.len(),
            summary.describe()
        );
        if let Some(free) = planned.free_bytes() {
            detail.push_str(&format!(", {free} bytes free"));
        }
        if inner_name.is_some() {
            detail.push_str(&format!(", {image_bytes} bytes before compression"));
        }
        if resumed {
            detail.push_str(&format!(
                ", went on from {skip} bytes after an interruption"
            ));
        }
        Ok(ImageBuilt {
            image_bytes: stored,
            files: want_sizes.len() as u64,
            detail,
        })
    })();
    // A crash a test asked for leaves everything as a killed engine would.
    let stopped = checkpoints.is_some_and(|c| c.stopped.load(Ordering::Relaxed));
    if result.is_err() && !stopped {
        let _ = std::fs::remove_file(&partial);
        remove_checkpoint(&partial);
    }
    // What was on disk from before a crash didn't check out: gone now, so the next try starts over.
    let result = match result {
        Err(e) if resumed && !cancel.load(Ordering::Relaxed) => Err(format!(
            "going on from an interrupted build failed: {e}. The partial image is removed, so \
             building it again starts from the beginning"
        )),
        r => r,
    };
    (settle(result, cancel.load(Ordering::Relaxed)), resumed)
}

/// What the log says of a `.ffpfsc` stream: how it was encoded, what each block became, and
/// where the time went (`feed`: the inner writer reading the source and cutting blocks, of
/// which `read` is the source's reads; `feed_wait`: it waiting for the workers; `encode`: summed
/// over the workers, each compressed block's inflate-back check included; `write`: the writer
/// thread's writes; `stream`: all of it, wall clock).
fn wrap_log_line(
    o: &ps5_dump_forge_pfs::WrapOptions,
    w: &ps5_dump_forge_pfs::WrapReport,
) -> String {
    let raw = w.blocks - w.compressed_blocks;
    format!(
        "level={} min_gain={}% encoder={} threads={} blocks={} compressed={} raw={} zero={} prechecked_raw={} strong_wins={} console_refused={} zlib_fallback={} raw_bytes={} stored_bytes={} feed_ms={} feed_wait_ms={} encode_ms={} write_ms={} stream_ms={}",
        o.level, o.min_block_gain, o.encoder(), w.threads, w.blocks, w.compressed_blocks, raw,
        w.zero_blocks, w.prechecked_blocks, w.strong_blocks, w.refused_blocks, w.fallback_blocks,
        w.raw_size, w.stored_size,
        w.feed.as_millis(), w.feed_wait.as_millis(), w.encode.as_millis(), w.write.as_millis(),
        w.wall.as_millis(),
    )
}

/// What the log says of a read-back: `fast, 2.1 GiB of 80.9 GiB in 41 samples (seed 7), policy 1`.
fn log_line(s: &VerifySummary) -> String {
    match s.mode {
        VerifyMode::Full => "full, every byte".into(),
        VerifyMode::Fast { .. } => {
            format!("fast, {}", s.describe().replacen("fast-checked ", "", 1))
        }
    }
}

/// A failed build that was cancelled says so; a cancel that came after the image was
/// finished and renamed changes nothing.
fn settle(result: Result<ImageBuilt, String>, cancelled: bool) -> Result<ImageBuilt, String> {
    match result {
        Err(_) if cancelled => Err("cancelled".into()),
        r => r,
    }
}

/// AMPR LZ4 asset packs for an image (see `ps5upload_fpkg::ampr_pack`): the game's files
/// packed into seekable LZ4 volumes that a backport's `ampr_emu` serves, the originals left out.
pub(crate) struct Lz4Packs {
    /// drakmor's profile format; the built-in profile (from `level` and `block_shift`) when
    /// absent.
    pub profile: Option<ps5upload_fpkg::ampr_pack::Config>,
    /// 1 (fastest) to 12 (smallest).
    pub level: u8,
    /// log2 of the block size, 14 (16 KiB) to 20 (1 MiB).
    pub block_shift: u8,
}

/// The profile an LZ4 build packs with, and its name for the log: a measured or user-given
/// profile always wins; otherwise the game's engine picks a built-in one.
fn lz4_profile(
    packs: &Lz4Packs,
    files: &[ps5upload_fpkg::source::SourceFile],
) -> (&'static str, ps5upload_fpkg::ampr_pack::Config) {
    use ps5upload_fpkg::ampr_pack;
    match &packs.profile {
        Some(given) => ("given", given.clone()),
        None => (
            ampr_pack::default_profile_name(files.iter().map(|f| f.path.as_str())),
            ampr_pack::default_profile_for(
                files.iter().map(|f| (f.path.as_str(), f.size)),
                packs.level,
                packs.block_shift,
            ),
        ),
    }
}

/// The spool an LZ4 build packs into, next to the image it is for.
pub(crate) fn lz4_spool(output: &Path) -> PathBuf {
    PathBuf::from(format!("{}.lz4spool", output.display()))
}

/// [`build`], with the game's files packed first. Only exFAT: the packs are already
/// compressed, so the `.ffpfsc` container has nothing left to gain. The volumes are spooled
/// beside the output and removed once the image is written and read back, or has failed.
/// `stage` reports `pack` before the image's own stages.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_packed(
    format: ImageFormat,
    source: &mut dyn SourceTree,
    output: &Path,
    packs: &Lz4Packs,
    mode: VerifyMode,
    verify_chunk: u64,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<ImageBuilt, String> {
    use ps5upload_fpkg::ampr_pack;
    if format != ImageFormat::Exfat {
        return Err("LZ4 asset packs are written into exFAT images only".into());
    }
    if let Some(why) = ampr_pack::image::refusal(source) {
        return Err(why);
    }
    let (profile_name, config) = lz4_profile(packs, source.files());
    // Claimed by the caller like the image's `.partial`, so nothing else writes here.
    let spool = lz4_spool(output);
    let mtime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    crate::log_at!(
        Info,
        "convert",
        "lz4 packs: profile={profile_name} chosen for {} files ({} rules) -> {}",
        source.files().len(),
        config.rules.len(),
        output.display()
    );
    stage("pack", 0, 0);
    let prepared = {
        let mut progress = |done: u64, total: u64| stage("pack", done, total);
        let mut control = ampr_pack::Control {
            progress: Some(&mut progress),
            cancel: Some(cancel),
        };
        ampr_pack::image::prepare(source, &config, &spool, mtime, &mut control)
    };
    let result = match prepared {
        Ok((mut overlay, report)) => build(
            format,
            None,
            &mut overlay,
            output,
            mode,
            verify_chunk,
            cancel,
            stage,
        )
        .map(|b| (b, report)),
        Err(e) => Err(e.to_string()),
    };
    let _ = std::fs::remove_dir_all(&spool);
    let (mut built, report) = result?;
    let s = &report.stats;
    if s.files_packed == 0 {
        crate::log_at!(
            Warn,
            "convert",
            "lz4 packs: profile={profile_name} packed no files ({} loose) for {}",
            s.files_loose,
            output.display()
        );
    } else {
        crate::log_at!(
            Info,
            "convert",
            "lz4 packs: profile={profile_name} packed {} files ({} loose) for {}",
            s.files_packed,
            s.files_loose,
            output.display()
        );
    }
    built.detail.push_str(&format!(
        ", LZ4 packs: {} files packed into {} volume(s) ({} -> {} bytes, {} LZ4 / {} raw \
         chunks), {} loose",
        s.files_packed,
        report.volumes.len(),
        s.logical_bytes,
        s.stored_bytes,
        s.chunks_lz4,
        s.chunks_raw,
        s.files_loose
    ));
    Ok(built)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_given_lz4_profile_overrides_engine_detection() {
        use ps5upload_fpkg::ampr_pack::{self, Config};
        use ps5upload_fpkg::source::SourceFile;
        let files: Vec<SourceFile> = [
            ("Media/globalgamemanagers", 1 << 10),
            ("Media/StreamingAssets/a.bundle", 1 << 30),
        ]
        .into_iter()
        .map(|(p, size)| SourceFile {
            path: p.into(),
            size,
        })
        .collect();
        let mut packs = super::Lz4Packs {
            profile: None,
            level: 9,
            block_shift: 16,
        };
        let (name, config) = super::lz4_profile(&packs, &files);
        assert_eq!(name, "unity");
        assert_eq!(config, ampr_pack::unity_profile(9, 16));
        let given = Config::from_toml(
            "[[rule]]\naction = \"compress\"\ninclude = [\"**\"]\n",
            None,
        )
        .unwrap();
        packs.profile = Some(given.clone());
        let (name, config) = super::lz4_profile(&packs, &files);
        assert_eq!(name, "given");
        assert_eq!(config, given);
    }

    #[test]
    fn nested_generic_packages_keep_game_folder_names() {
        assert_eq!(
            output_names(Path::new("/games/007/game.pkg"), ImageFormat::Exfat, false).0,
            "007.exfat"
        );
        assert_eq!(
            output_names(
                Path::new("/games/Saros/game.pkg"),
                ImageFormat::Exfat,
                false
            )
            .0,
            "Saros.exfat"
        );
    }

    #[test]
    fn verify_chunk_takes_1_to_64_mib_in_kib_and_defaults_to_8() {
        assert_eq!(verify_chunk(None).unwrap(), 8 << 20);
        assert_eq!(verify_chunk(Some(1024)).unwrap(), 1 << 20);
        assert_eq!(verify_chunk(Some(65536)).unwrap(), 64 << 20);
        assert!(verify_chunk(Some(512))
            .unwrap_err()
            .contains("1024 to 65536"));
        assert!(verify_chunk(Some(0)).is_err());
        assert!(verify_chunk(Some(1 << 20)).is_err());
    }

    use super::*;
    use ps5upload_fpkg::source::FolderSource;

    #[test]
    fn formats_parse_and_name_their_files() {
        assert_eq!(ImageFormat::parse(None), Ok(ImageFormat::Exfat));
        assert_eq!(ImageFormat::parse(Some("FFPKG")), Ok(ImageFormat::Ffpkg));
        assert_eq!(ImageFormat::parse(Some("pfs")), Ok(ImageFormat::Ffpfs));
        assert!(ImageFormat::parse(Some("iso")).is_err());
        let src = Path::new("/games/PPSA01234-app");
        assert_eq!(
            output_names(src, ImageFormat::Ffpkg, false),
            ("PPSA01234-app.ffpkg".into(), None)
        );
        assert_eq!(
            output_names(Path::new("/games/What: a game?"), ImageFormat::Exfat, false).0,
            "What_ a game_.exfat"
        );
        assert_eq!(
            output_names(Path::new("/"), ImageFormat::Exfat, false).0,
            "game.exfat"
        );
        // An image source: the output carries its name, not its old extension.
        assert_eq!(
            output_names(Path::new("/g/PPSA29343.exfat"), ImageFormat::Ffpkg, false).0,
            "PPSA29343.ffpkg"
        );
        assert_eq!(
            output_names(src, ImageFormat::Exfat, true),
            (
                "PPSA01234-app.ffpfsc".into(),
                Some("PPSA01234-app.exfat".into())
            )
        );
    }

    #[test]
    fn pfs_based_names_fit_what_shadowmount_mounts() {
        let long =
            Path::new("/g/A very long game folder name that goes on and on past sixty-three bytes");
        let (pfs, _) = output_names(long, ImageFormat::Ffpfs, false);
        assert!(
            pfs.len() <= PFS_NAME_MAX && pfs.ends_with(".ffpfs"),
            "{pfs}"
        );
        let (outer, inner) = output_names(long, ImageFormat::Ffpkg, true);
        assert!(
            outer.len() <= PFS_NAME_MAX && outer.ends_with(".ffpfsc"),
            "{outer}"
        );
        assert!(inner.unwrap().ends_with(".ffpkg"));
        // A UFS2 or exFAT image has its own limit, on the stem: the mount path has to fit.
        for (format, ext) in [
            (ImageFormat::Ffpkg, ".ffpkg"),
            (ImageFormat::Exfat, ".exfat"),
        ] {
            let (name, _) = output_names(long, format, false);
            let stem = name.strip_suffix(ext).expect(&name);
            assert!(stem.len() <= IMAGE_STEM_MAX, "{name}");
            // Shorter names are left as they are.
            let (short, _) = output_names(Path::new("/g/PPSA01234-app"), format, false);
            assert_eq!(short, format!("PPSA01234-app{ext}"));
        }
        // Multi-byte characters are never cut in half.
        let wide = Path::new("/g/ゲームゲームゲームゲームゲームゲームゲームゲームゲームゲーム");
        let (w, _) = output_names(wide, ImageFormat::Ffpfs, false);
        assert!(w.len() <= PFS_NAME_MAX && w.ends_with(".ffpfs"), "{w}");
    }

    fn game(root: &Path) -> PathBuf {
        let game = root.join("PPSA99999-app");
        std::fs::create_dir_all(game.join("sce_sys")).unwrap();
        std::fs::create_dir_all(game.join("data/sub")).unwrap();
        std::fs::create_dir_all(game.join("empty")).unwrap();
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(game.join("eboot.bin"), &big).unwrap();
        std::fs::write(
            game.join("sce_sys/param.json"),
            b"{\"titleId\":\"PPSA99999\"}",
        )
        .unwrap();
        std::fs::write(game.join("data/sub/a.bin"), b"").unwrap();
        std::fs::write(game.join("data/zeros.bin"), vec![0u8; 200_000]).unwrap();
        game
    }

    /// Every format, plain and compressed, writes a folder that reads back hash for hash.
    #[test]
    fn every_format_round_trips_plain_and_compressed() {
        let root = std::env::temp_dir().join(format!("ps5upload-imgb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = game(&root);
        for format in [ImageFormat::Exfat, ImageFormat::Ffpkg, ImageFormat::Ffpfs] {
            for compress in [false, true] {
                let (name, inner) = output_names(&src, format, compress);
                let out = root.join(&name);
                let mut tree = FolderSource::open(&src).unwrap();
                let cancel = AtomicBool::new(false);
                let mut stages: Vec<String> = Vec::new();
                let built = build(
                    format,
                    inner.as_deref(),
                    &mut tree,
                    &out,
                    VerifyMode::Full,
                    DEFAULT_VERIFY_CHUNK,
                    &cancel,
                    &mut |s, _, _| {
                        if stages.last().map(String::as_str) != Some(s) {
                            stages.push(s.to_string());
                        }
                    },
                )
                .unwrap_or_else(|e| panic!("{format:?} compress={compress}: {e}"));
                assert_eq!(built.files, 4, "{format:?}");
                let write = if compress { "compress" } else { "write" };
                assert_eq!(
                    stages,
                    ["plan", write, "verify"],
                    "{format:?} compress={compress}"
                );
                assert!(out.is_file() && !root.join(format!("{name}.partial")).exists());
                let _ = std::fs::remove_file(&out);
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A game in LZ4 packs reads back as the game it was, a piece at a time as an image writer
    /// and the zstd packer read it: what switching pack formats rests on.
    #[test]
    fn an_lz4_packed_image_reads_as_the_game_it_was() {
        use ps5upload_fpkg::ampr_pack::image::{Lz4Unpacked, Unpacking};
        let root = std::env::temp_dir().join(format!("ps5upload-imglz4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = ampr_game(&root);
        let out = root.join("PPSA99999-app.exfat");
        let mut tree = FolderSource::open(&src).unwrap();
        build_packed(
            ImageFormat::Exfat,
            &mut tree,
            &out,
            &Lz4Packs {
                profile: Some(ps5upload_fpkg::ampr_pack::default_profile(9, 16)),
                level: 9,
                block_shift: 16,
            },
            VerifyMode::Fast {
                seed: 1,
                seam: false,
            },
            DEFAULT_VERIFY_CHUNK,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        let image: Box<dyn SourceTree> =
            Box::new(ps5upload_fpkg::exfat::ExFatSource::open(&out).unwrap());
        let Unpacking::Decoded(mut game) = Lz4Unpacked::open(image).unwrap() else {
            panic!("the image has packs");
        };
        let names: Vec<String> = game.files().iter().map(|f| f.path.clone()).collect();
        assert!(
            !names.iter().any(|n| n.starts_with("ampr_assets.")),
            "{names:?}"
        );
        let mut source = FolderSource::open(&src).unwrap();
        for f in source.files().to_vec() {
            if f.path == ps5upload_fpkg::ampr_index::NAME {
                continue;
            }
            let want = source.read(&f.path).unwrap();
            let listed = game.files().iter().find(|g| g.path == f.path);
            assert_eq!(listed.map(|g| g.size), Some(f.size), "{}", f.path);
            assert_eq!(game.read(&f.path).unwrap(), want, "{}", f.path);
            // Across a chunk's edge (64 KiB), in small pieces, as the packer's reads go.
            let mut pieces = Vec::new();
            let mut at = 0u64;
            while at < f.size {
                let piece = game.read_range(&f.path, at, 40_000).unwrap();
                assert!(!piece.is_empty(), "{} at {at}", f.path);
                at += piece.len() as u64;
                pieces.extend(piece);
            }
            assert_eq!(pieces, want, "{} in pieces", f.path);
        }
        let plain: Box<dyn SourceTree> = Box::new(FolderSource::open(&src).unwrap());
        assert!(matches!(
            Lz4Unpacked::open(plain).unwrap(),
            Unpacking::NotPacked(_)
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A crash partway: the next build goes on from the checkpoint without reading the files
    /// already on disk, and the image comes out the same bytes. Any other game starts over.
    #[test]
    fn a_plain_exfat_goes_on_from_a_checkpoint() {
        let root = std::env::temp_dir().join(format!("ps5upload-imgres-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("game");
        std::fs::create_dir_all(src.join("sce_sys")).unwrap();
        std::fs::write(
            src.join("sce_sys/param.json"),
            b"{\"titleId\":\"PPSA99999\"}",
        )
        .unwrap();
        std::fs::write(src.join("a.bin"), vec![1u8; 3 << 20]).unwrap();
        std::fs::write(src.join("b.bin"), vec![2u8; 3 << 20]).unwrap();
        std::fs::write(src.join("c.bin"), vec![3u8; 3 << 20]).unwrap();
        let cancel = AtomicBool::new(false);
        let run = |out: &Path| {
            let mut tree = FolderSource::open(&src).unwrap();
            build_resumable(
                ImageFormat::Exfat,
                None,
                Level::default(),
                STAMP,
                &mut tree,
                out,
                VerifyMode::Full,
                DEFAULT_VERIFY_CHUNK,
                &cancel,
                &mut |_, _, _| {},
            )
        };
        let whole = root.join("whole.exfat");
        run(&whole).unwrap();
        let bytes = std::fs::read(&whole).unwrap();

        // What a crash leaves: most of the image, junk past the checkpoint, and the checkpoint.
        let out = root.join("resumed.exfat");
        let partial = PathBuf::from(format!("{}.partial", out.display()));
        let filled_with = |b: u8| {
            bytes
                .chunks(4096)
                .position(|c| c.iter().all(|x| *x == b))
                .expect("the file's data is in the image")
                * 4096
        };
        // Partway through the last file, with the two before it behind the checkpoint.
        let done = (filled_with(3) + (1 << 20)) as u64;
        let mut left = bytes[..done as usize].to_vec();
        left.extend_from_slice(&[0xEE; 4096]);
        std::fs::write(&partial, &left).unwrap();
        let tree = FolderSource::open(&src).unwrap();
        let sizes: HashMap<String, u64> = tree
            .files()
            .iter()
            .map(|f| (f.path.clone(), f.size))
            .collect();
        let plan = Planned::plan(ImageFormat::Exfat, &tree, Some(STAMP), &cancel).unwrap();
        let key = resume_key(
            ImageFormat::Exfat,
            None,
            Level::default(),
            Some(STAMP),
            &sizes,
            plan.image_size(),
        );
        let saved = |key: &str, done: u64| {
            serde_json::to_vec(&Checkpoint {
                key: key.to_string(),
                done,
                blocks: 0,
            })
            .unwrap()
        };
        std::fs::write(checkpoint_path(&partial), saved(&key, done)).unwrap();
        run(&out).unwrap();
        assert!(std::fs::read(&out).unwrap() == bytes, "the same image");
        assert!(!partial.exists() && !checkpoint_path(&partial).exists());

        // A checkpoint of something else: from the start, and still right.
        let other = root.join("other.exfat");
        let partial = PathBuf::from(format!("{}.partial", other.display()));
        std::fs::write(&partial, vec![9u8; 1 << 20]).unwrap();
        std::fs::write(checkpoint_path(&partial), saved("another game", 1 << 19)).unwrap();
        run(&other).unwrap();
        assert!(std::fs::read(&other).unwrap() == bytes);

        // Bytes that changed behind the checkpoint: the read-back refuses what is on disk, and the
        // image is built again from the start.
        let bad = root.join("bad.exfat");
        let partial = PathBuf::from(format!("{}.partial", bad.display()));
        let mut wrong = bytes[..done as usize].to_vec();
        wrong[filled_with(2) + 100] ^= 0xFF;
        std::fs::write(&partial, &wrong).unwrap();
        std::fs::write(checkpoint_path(&partial), saved(&key, done)).unwrap();
        let built = run(&bad).expect("built again from the start");
        assert!(!built.detail.contains("went on from"), "{}", built.detail);
        assert!(std::fs::read(&bad).unwrap() == bytes);
        assert!(!partial.exists() && !checkpoint_path(&partial).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The time a resumable test build pins into its image.
    const STAMP: i64 = 1_700_000_000;

    /// A game whose data deflate cannot shrink, so a `.ffpfsc` of it is about its size and a
    /// crash lands partway through its files in every format.
    fn noisy_game(root: &Path) -> PathBuf {
        let src = root.join("PPSA99998-app");
        std::fs::create_dir_all(src.join("sce_sys")).unwrap();
        std::fs::write(
            src.join("sce_sys/param.json"),
            b"{\"titleId\":\"PPSA99998\"}",
        )
        .unwrap();
        let mut x = 0x2545_F491u32;
        for (name, size) in [("a.bin", 3 << 20), ("b.bin", 3 << 20), ("c.bin", 5 << 20)] {
            let bytes: Vec<u8> = (0..size)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x as u8
                })
                .collect();
            std::fs::write(src.join(name), bytes).unwrap();
        }
        std::fs::write(src.join("d.bin"), vec![4u8; 1 << 20]).unwrap();
        src
    }

    /// A resumable build of `src` as `format` (in a `.ffpfsc` when `compress`), checkpointing
    /// every MiB and stopping like a crash once `stop_after` bytes are out.
    fn resumable_build(
        format: ImageFormat,
        compress: bool,
        src: &Path,
        out: &Path,
        stop_after: Option<u64>,
    ) -> Result<ImageBuilt, String> {
        let (_, inner) = output_names(src, format, compress);
        let mut tree = FolderSource::open(src).unwrap();
        let checkpoints = Checkpoints {
            every: 1 << 20,
            stop_after,
            stopped: AtomicBool::new(false),
        };
        build_any(
            format,
            inner.as_deref(),
            Level::default(),
            Some(STAMP),
            &mut tree,
            out,
            VerifyMode::Full,
            DEFAULT_VERIFY_CHUNK,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
            Some(&checkpoints),
        )
    }

    fn saved_checkpoint(partial: &Path) -> Checkpoint {
        serde_json::from_slice(&std::fs::read(checkpoint_path(partial)).unwrap()).unwrap()
    }

    const EVERY_KIND: [(ImageFormat, bool); 6] = [
        (ImageFormat::Exfat, false),
        (ImageFormat::Ffpkg, false),
        (ImageFormat::Ffpfs, false),
        (ImageFormat::Exfat, true),
        (ImageFormat::Ffpkg, true),
        (ImageFormat::Ffpfs, true),
    ];

    /// Every format, plain and in a `.ffpfsc`: a build stopped partway (its partial file and
    /// checkpoint left as a killed engine leaves them) goes on from the checkpoint, and the image
    /// is byte for byte the one an uninterrupted build makes.
    #[test]
    fn every_format_goes_on_from_a_crash_byte_for_byte() {
        let root = std::env::temp_dir().join(format!("ps5upload-imgcrash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = noisy_game(&root);
        for (format, compress) in EVERY_KIND {
            let what = format!("{format:?} compress={compress}");
            let ext = if compress {
                "ffpfsc"
            } else {
                format.extension()
            };
            let whole = root.join(format!("whole.{ext}"));
            resumable_build(format, compress, &src, &whole, None)
                .unwrap_or_else(|e| panic!("{what}: {e}"));
            let want = std::fs::read(&whole).unwrap();

            let out = root.join(format!("resumed.{ext}"));
            let partial = PathBuf::from(format!("{}.partial", out.display()));
            let stopped = resumable_build(format, compress, &src, &out, Some(6 << 20));
            assert!(stopped.is_err(), "{what}: the build should have stopped");
            assert!(partial.is_file() && !out.exists(), "{what}");
            let saved = saved_checkpoint(&partial);
            assert!(saved.done >= 1 << 20, "{what}: {saved:?}");
            assert_eq!(saved.blocks > 0, compress, "{what}: {saved:?}");

            let built = resumable_build(format, compress, &src, &out, None)
                .unwrap_or_else(|e| panic!("{what}: going on: {e}"));
            assert!(
                built
                    .detail
                    .contains(&format!("went on from {}", saved.done)),
                "{what}: {}",
                built.detail
            );
            assert!(
                std::fs::read(&out).unwrap() == want,
                "{what}: the same image"
            );
            assert!(
                !partial.exists() && !checkpoint_path(&partial).exists(),
                "{what}"
            );
            let _ = std::fs::remove_file(&whole);
            let _ = std::fs::remove_file(&out);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A source that changed between the crash and the next build is not gone on from: another
    /// size changes the key and the build starts over; a file edited to the same size behind the
    /// checkpoint fails the full read-back and the build starts over. Either way the image is
    /// the new source's, byte for byte.
    #[test]
    fn a_changed_source_refuses_the_resume_and_rebuilds() {
        let root = std::env::temp_dir().join(format!("ps5upload-imgchg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (format, compress) in EVERY_KIND {
            let what = format!("{format:?} compress={compress}");
            let ext = if compress {
                "ffpfsc"
            } else {
                format.extension()
            };
            for edit in ["size", "same size"] {
                let _ = std::fs::remove_dir_all(&root);
                let src = noisy_game(&root);
                let out = root.join(format!("out.{ext}"));
                let partial = PathBuf::from(format!("{}.partial", out.display()));
                assert!(resumable_build(format, compress, &src, &out, Some(6 << 20)).is_err());
                assert!(checkpoint_path(&partial).is_file(), "{what}");
                // a.bin is laid out first in every format: behind the checkpoint.
                let mut a = std::fs::read(src.join("a.bin")).unwrap();
                match edit {
                    "size" => a.truncate(a.len() - 1000),
                    _ => a[100] ^= 0xFF,
                }
                std::fs::write(src.join("a.bin"), &a).unwrap();

                let built = resumable_build(format, compress, &src, &out, None)
                    .unwrap_or_else(|e| panic!("{what} {edit}: {e}"));
                assert!(
                    !built.detail.contains("went on from"),
                    "{what} {edit}: {}",
                    built.detail
                );
                let fresh = root.join(format!("fresh.{ext}"));
                resumable_build(format, compress, &src, &fresh, None).unwrap();
                assert!(
                    std::fs::read(&out).unwrap() == std::fs::read(&fresh).unwrap(),
                    "{what} {edit}: the new source's image"
                );
                assert!(!partial.exists() && !checkpoint_path(&partial).exists());
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The writer seeks forward the way the UFS2 writer does: a seek inside what is on disk moves
    /// nothing, one past it leaves a hole, and going back is refused.
    #[test]
    fn the_checkpoint_writer_seeks_forward_over_what_is_on_disk() {
        let root = std::env::temp_dir().join(format!("ps5upload-imgcks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("x.partial");
        std::fs::write(&path, b"ABCDEF").unwrap();
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(6)).unwrap();
        let mut w = CheckpointWriter {
            out: BufWriter::new(&mut file),
            skip: 6,
            pos: 0,
            end: 0,
            since: 0,
            path: checkpoint_path(&path),
            key: "k".into(),
            every: 1 << 20,
            crash: None,
        };
        assert_eq!(w.seek(SeekFrom::End(0)).unwrap(), 0, "starts empty");
        w.write_all(b"ab").unwrap();
        assert_eq!(w.seek(SeekFrom::Start(4)).unwrap(), 4);
        w.write_all(b"efgh").unwrap();
        assert_eq!(w.seek(SeekFrom::Start(10)).unwrap(), 10);
        w.write_all(b"k").unwrap();
        assert!(w.seek(SeekFrom::Start(3)).is_err());
        w.flush().unwrap();
        drop(w);
        assert_eq!(std::fs::read(&path).unwrap(), b"ABCDEFgh\0\0k");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A generated index keeps its time across a crash, which going on depends on; a build with
    /// no checkpoint behind it gets a new one.
    #[test]
    fn an_index_stamp_lasts_only_as_long_as_its_checkpoint() {
        let root = std::env::temp_dir().join(format!("ps5upload-imgstamp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let partial = root.join("x.exfat.partial");
        std::fs::write(stamp_path(&partial), "123").unwrap();
        assert_ne!(index_stamp(&partial), 123, "no checkpoint: a new one");
        std::fs::write(stamp_path(&partial), "123").unwrap();
        std::fs::write(checkpoint_path(&partial), "{}").unwrap();
        assert_eq!(index_stamp(&partial), 123);
        remove_checkpoint(&partial);
        assert!(!stamp_path(&partial).exists() && !checkpoint_path(&partial).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The writer drops what is on disk already, whichever way the writes are cut.
    #[test]
    fn the_checkpoint_writer_skips_then_writes_and_records() {
        let root = std::env::temp_dir().join(format!("ps5upload-imgckw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("x.partial");
        let mut file = std::fs::File::create(&path).unwrap();
        let mut w = CheckpointWriter {
            out: BufWriter::new(&mut file),
            skip: 5,
            pos: 0,
            end: 0,
            since: 0,
            path: checkpoint_path(&path),
            key: "k".into(),
            every: 4,
            crash: None,
        };
        w.write_all(b"abc").unwrap();
        w.write_all(b"defgh").unwrap();
        w.write_all(b"ij").unwrap();
        w.flush().unwrap();
        drop(w);
        assert_eq!(std::fs::read(&path).unwrap(), b"fghij");
        let saved: Checkpoint =
            serde_json::from_slice(&std::fs::read(checkpoint_path(&path)).unwrap()).unwrap();
        assert_eq!(
            saved,
            Checkpoint {
                key: "k".into(),
                done: 10,
                blocks: 0,
            }
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A PS4 game's image holds the param.json ShadowMount+ needs, and it survives the read-back
    /// (Alembic's console test: without it the game mounts and then won't start).
    #[test]
    fn a_ps4_game_gets_its_param_json_in_every_format() {
        let root = std::env::temp_dir().join(format!("ps5upload-imgb-ps4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("CUSA00000-app");
        std::fs::create_dir_all(src.join("sce_sys")).unwrap();
        std::fs::write(src.join("eboot.bin"), vec![7u8; 90_000]).unwrap();
        let (key, value) = (b"TITLE_ID\0", b"CUSA00000\0");
        let mut sfo = b"\0PSF\x01\x01\0\0".to_vec();
        sfo.extend_from_slice(&36u32.to_le_bytes());
        sfo.extend_from_slice(&(36 + key.len() as u32).to_le_bytes());
        sfo.extend_from_slice(&1u32.to_le_bytes());
        sfo.extend_from_slice(&0u16.to_le_bytes());
        sfo.extend_from_slice(&0x0204u16.to_le_bytes());
        sfo.extend_from_slice(&(value.len() as u32).to_le_bytes());
        sfo.extend_from_slice(&(value.len() as u32).to_le_bytes());
        sfo.extend_from_slice(&0u32.to_le_bytes());
        sfo.extend_from_slice(key);
        sfo.extend_from_slice(value);
        std::fs::write(src.join("sce_sys/param.sfo"), sfo).unwrap();
        for format in [ImageFormat::Exfat, ImageFormat::Ffpkg, ImageFormat::Ffpfs] {
            for compress in [false, true] {
                let (name, inner) = output_names(&src, format, compress);
                let out = root.join(&name);
                let mut tree = FolderSource::open(&src).unwrap();
                let mut with = ps5upload_fpkg::ps4_param::Ps4Params::wrap(&mut tree)
                    .unwrap()
                    .expect("a PS4 game");
                let built = build(
                    format,
                    inner.as_deref(),
                    &mut with,
                    &out,
                    VerifyMode::Full,
                    DEFAULT_VERIFY_CHUNK,
                    &AtomicBool::new(false),
                    &mut |_, _, _| {},
                )
                .unwrap_or_else(|e| panic!("{format:?} compress={compress}: {e}"));
                assert_eq!(built.files, 3, "{format:?}");
                let mut image = ps5upload_fpkg::source::open(&out).unwrap();
                let json = image.read("sce_sys/param.json").unwrap();
                let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
                assert_eq!(v["titleId"], "CUSA00000", "{format:?}");
                drop(image);
                let _ = std::fs::remove_file(&out);
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Another build's `.partial` under the same name is neither written into nor removed;
    /// and a cancel that lands after the image was finished does not turn it into a failure.
    #[test]
    fn another_builds_partial_is_left_alone_and_a_late_cancel_changes_nothing() {
        let root = std::env::temp_dir().join(format!("ps5upload-imgp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = game(&root);
        let out = root.join("x.exfat");
        let partial = root.join("x.exfat.partial");
        std::fs::write(&partial, b"theirs").unwrap();
        let mut tree = FolderSource::open(&src).unwrap();
        let cancel = AtomicBool::new(false);
        assert!(build(
            ImageFormat::Exfat,
            None,
            &mut tree,
            &out,
            VerifyMode::Full,
            DEFAULT_VERIFY_CHUNK,
            &cancel,
            &mut |_, _, _| {}
        )
        .is_err());
        assert_eq!(std::fs::read(&partial).unwrap(), b"theirs");
        std::fs::remove_file(&partial).unwrap();

        let done = || ImageBuilt {
            image_bytes: 1,
            files: 1,
            detail: String::new(),
        };
        assert!(settle(Ok(done()), true).is_ok());
        assert_eq!(
            settle(Err("disk full".into()), true).err().unwrap(),
            "cancelled"
        );
        assert_eq!(
            settle(Err("disk full".into()), false).err().unwrap(),
            "disk full"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A file that is not read front to back while the image is written is still checked.
    #[test]
    fn a_file_read_out_of_order_is_hashed_afterwards() {
        let root = std::env::temp_dir().join(format!("ps5upload-imgh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = game(&root);
        let mut tree = FolderSource::open(&src).unwrap();
        let cancel = AtomicBool::new(false);
        let mut h = HashingTree::new(&mut tree);
        let _ = h.read_range("eboot.bin", 1000, 10).unwrap();
        let _ = h.read("sce_sys/param.json").unwrap();
        let all = h.hashes(&cancel).unwrap();
        let mut again = FolderSource::open(&src).unwrap();
        let mut direct = Vec::new();
        hash_slices(
            &mut again,
            "eboot.bin",
            300_000,
            0,
            1,
            DEFAULT_VERIFY_CHUNK,
            &cancel,
            &mut |_, h| {
                direct.push(h);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(all["eboot.bin"], direct);
        assert_eq!(all.len(), 4);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Fast verification reads a seeded sample: damage in a slice the plan leaves out passes it
    /// and fails a full check, damage in the first or last slice fails both (naming the bytes),
    /// and a small file is read whole.
    #[test]
    fn a_fast_verify_catches_what_it_samples_and_a_full_one_everything() {
        const MIB: usize = 1 << 20;
        let root = std::env::temp_dir().join(format!("ps5upload-imgf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = game(&root);
        // 48 MiB in six slices, each stamped with a marker the image can be searched for.
        let mut big = vec![0u8; 48 * MIB];
        for (i, b) in big.iter_mut().enumerate() {
            *b = (i % 253) as u8;
        }
        for s in 0..6usize {
            let at = s * (SLICE as usize) + 4 * MIB;
            big[at..at + 16].copy_from_slice(format!("SLICEMARK-{s:06}").as_bytes());
        }
        std::fs::write(src.join("data/big.bin"), &big).unwrap();
        let out = root.join("f.exfat");
        let mut tree = FolderSource::open(&src).unwrap();
        build(
            ImageFormat::Exfat,
            None,
            &mut tree,
            &out,
            VerifyMode::Full,
            DEFAULT_VERIFY_CHUNK,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        // A fast build says so in its detail.
        let mut again = FolderSource::open(&src).unwrap();
        let fast = build(
            ImageFormat::Exfat,
            None,
            &mut again,
            &root.join("g.exfat"),
            VerifyMode::Fast {
                seed: 3,
                seam: false,
            },
            DEFAULT_VERIFY_CHUNK,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        assert!(fast.detail.contains("fast-checked"), "{}", fast.detail);
        let image_bytes = std::fs::read(&out).unwrap();
        let mut tree = FolderSource::open(&src).unwrap();
        let sizes: HashMap<String, u64> = tree
            .files()
            .iter()
            .map(|f| (f.path.clone(), f.size))
            .collect();
        let listed: Vec<(String, u64)> = sizes.iter().map(|(p, s)| (p.clone(), *s)).collect();
        let empty: Vec<String> = tree.empty_dirs().to_vec();
        let hashes = HashingTree::new(&mut tree)
            .hashes(&AtomicBool::new(false))
            .unwrap();
        let seed = 21;
        let mode = VerifyMode::Fast { seed, seam: false };
        let planned = sample::plan(&listed, seed, false);
        let big_idx = listed.iter().position(|f| f.0 == "data/big.bin").unwrap();
        let sampled = |s: u64| {
            planned
                .iter()
                .any(|p| p.file_idx == big_idx && p.slice == Some(s))
        };
        assert!(sampled(0) && sampled(5));
        let hidden = (1..5usize)
            .find(|s| !sampled(*s as u64))
            .expect("the plan leaves a slice out");

        // Flips one byte of slice `s` in a copy of the image and verifies it in `mode`,
        // `chunk` bytes per read.
        let check_with = |s: usize, mode: VerifyMode, chunk: u64| {
            let mark = format!("SLICEMARK-{s:06}");
            let at = image_bytes
                .windows(16)
                .position(|w| w == mark.as_bytes())
                .expect("the marker is in the image");
            let mut damaged = image_bytes.clone();
            damaged[at + 100] ^= 0xFF;
            let path = root.join("damaged.exfat");
            std::fs::write(&path, damaged).unwrap();
            let mut image = open_image(&path, ImageFormat::Exfat, false).unwrap();
            verify(
                &mut *image,
                &hashes,
                &sizes,
                &empty,
                mode,
                chunk,
                &AtomicBool::new(false),
                &mut |_, _| {},
            )
        };
        let check = |s: usize, mode: VerifyMode| check_with(s, mode, DEFAULT_VERIFY_CHUNK);
        // A sampled slice: caught, with the bytes named.
        for s in [0usize, 5] {
            let e = check(s, mode).unwrap_err();
            let (a, b) = (s as u64 * SLICE, (s as u64 + 1) * SLICE);
            assert!(
                e.contains("data/big.bin differs between the image and the folder")
                    && e.contains(&format!("(bytes {a}..{b})")),
                "{e}"
            );
        }
        // A slice left out: the sample passes, the full check does not.
        let summary = check(hidden, mode).expect("the sample does not reach it");
        assert!(summary.checked_bytes < summary.total_bytes && summary.samples > 0);
        assert!(check(hidden, VerifyMode::Full).is_err());
        // A 32 MiB read spans four slices and still names the one that differs.
        let e = check_with(5, VerifyMode::Full, 32 << 20).unwrap_err();
        assert!(
            e.contains(&format!("(bytes {}..{})", 5 * SLICE, 6 * SLICE)),
            "{e}"
        );
        // Small files are read whole, so damage in one is always caught: here the image is
        // checked against a source whose small file differs.
        let mut wrong = hashes.clone();
        wrong.get_mut("eboot.bin").unwrap()[0] = blake3::hash(b"other");
        let mut image = open_image(&out, ImageFormat::Exfat, false).unwrap();
        let e = verify(
            &mut *image,
            &wrong,
            &sizes,
            &empty,
            mode,
            DEFAULT_VERIFY_CHUNK,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(e.contains("eboot.bin differs"), "{e}");
        // The structure is checked in both modes: a missing size is a mismatch either way.
        let mut short = sizes.clone();
        short.insert("eboot.bin".into(), 1);
        assert!(verify(
            &mut *image,
            &hashes,
            &short,
            &empty,
            mode,
            DEFAULT_VERIFY_CHUNK,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A PFS image holds ASCII names only; the build refuses before writing anything.
    #[test]
    fn a_name_pfs_cannot_hold_is_refused_before_writing() {
        let root = std::env::temp_dir().join(format!("ps5upload-imgn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = game(&root);
        std::fs::write(src.join("données.bin"), b"x").unwrap();
        let mut tree = FolderSource::open(&src).unwrap();
        let out = root.join("x.ffpfs");
        let e = build(
            ImageFormat::Ffpfs,
            None,
            &mut tree,
            &out,
            VerifyMode::Full,
            DEFAULT_VERIFY_CHUNK,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .err()
        .expect("refused");
        assert!(e.contains("données.bin"), "{e}");
        assert!(!out.exists() && !root.join("x.ffpfs.partial").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A backported AMPR title: its eboot imports libSceAmpr and its fakelib carries a
    /// pack-capable ampr_emu.
    fn ampr_game(root: &Path) -> PathBuf {
        let src = game(root);
        let mut eboot: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        eboot[1000..1014].copy_from_slice(b"libSceAmpr.prx");
        std::fs::write(src.join("eboot.bin"), eboot).unwrap();
        std::fs::create_dir_all(src.join("fakelib")).unwrap();
        let mut module = vec![0u8; 4096];
        module[100..108].copy_from_slice(b"AMPRPAK4");
        module[200..217].copy_from_slice(b"ampr_assets.index");
        module[300..321].copy_from_slice(b"\x000.4.2.1 (c) Drakmor\0");
        std::fs::write(src.join("fakelib/libSceAmpr.sprx"), module).unwrap();
        let text: Vec<u8> = (0..400_000u32)
            .flat_map(|i| format!("asset {} ", i % 977).into_bytes())
            .take(400_000)
            .collect();
        std::fs::create_dir_all(src.join("data/only_packed")).unwrap();
        std::fs::write(src.join("data/only_packed/table.bin"), &text).unwrap();
        std::fs::write(src.join("data/text.bin"), &text[..150_000]).unwrap();
        std::fs::write(src.join("data/settings.json"), b"{\"a\":1}").unwrap();
        src
    }

    /// An Unreal Engine title keeps everything but its IoStore containers loose: with its
    /// project descriptor packed, Star Wars Galactic Racer stopped at boot ("Failed to open
    /// descriptor file ../../../griffin/griffin.uproject").
    #[test]
    fn an_unreal_title_packs_only_its_iostore_containers() {
        let root = std::env::temp_dir().join(format!("ps5upload-imgu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = ampr_game(&root);
        let paks = src.join("griffin/Content/Paks");
        std::fs::create_dir_all(&paks).unwrap();
        let text: Vec<u8> = (0..300_000u32)
            .flat_map(|i| format!("uasset {} ", i % 911).into_bytes())
            .take(300_000)
            .collect();
        std::fs::write(src.join("griffin/griffin.uproject"), b"{\"FileVersion\":3}").unwrap();
        for name in [
            "pakchunk0-ps5.pak",
            "pakchunk0-ps5.utoc",
            "pakchunk0-ps5.ucas",
            "pakchunk0-ps5.sig",
            "global.utoc",
            "global.ucas",
        ] {
            std::fs::write(paks.join(name), &text).unwrap();
        }
        let out = root.join("PPSA28416-app.exfat");
        let mut tree = FolderSource::open(&src).unwrap();
        build_packed(
            ImageFormat::Exfat,
            &mut tree,
            &out,
            &Lz4Packs {
                profile: None,
                level: 9,
                block_shift: 16,
            },
            VerifyMode::Full,
            DEFAULT_VERIFY_CHUNK,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        let mut image = ps5upload_fpkg::exfat::ExFatSource::open(&out).unwrap();
        let in_image: Vec<String> = image.files().iter().map(|f| f.path.clone()).collect();
        let reader = ps5upload_fpkg::ampr_pack::Reader::open(&mut image, "ampr_assets.index")
            .expect("the containers are packed");
        let mut packed: Vec<&str> = (0..reader.manifest.files.len())
            .filter(|&i| reader.manifest.files[i].packed())
            .map(|i| {
                let p = reader.manifest.path(i).unwrap();
                p.strip_prefix("/app0/").unwrap_or(p)
            })
            .collect();
        packed.sort();
        assert_eq!(
            packed,
            [
                "griffin/Content/Paks/global.ucas",
                "griffin/Content/Paks/pakchunk0-ps5.ucas"
            ]
        );
        for loose in [
            "griffin/griffin.uproject",
            "griffin/Content/Paks/pakchunk0-ps5.pak",
            "griffin/Content/Paks/pakchunk0-ps5.utoc",
            "griffin/Content/Paks/pakchunk0-ps5.sig",
            "griffin/Content/Paks/global.utoc",
            "data/text.bin",
        ] {
            assert!(in_image.contains(&loose.to_string()), "{loose} is loose");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An LZ4-packed exFAT image, read back through the exFAT reader and then the pack reader,
    /// gives every packed file's source bytes; the packed originals are not in the image, the
    /// loose files are, and the regenerated ampr_emu.index lists the original files.
    #[test]
    fn an_lz4_packed_exfat_reads_back_to_the_source() {
        use ps5upload_fpkg::ampr_pack::Reader;
        let root = std::env::temp_dir().join(format!("ps5upload-imgl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = ampr_game(&root);
        let out = root.join("PPSA99999-app.exfat");
        let mut tree = FolderSource::open(&src).unwrap();
        let cancel = AtomicBool::new(false);
        let mut stages: Vec<String> = Vec::new();
        let built = build_packed(
            ImageFormat::Exfat,
            &mut tree,
            &out,
            // The folder-wide profile, so the small fixture files are packed: the built-in
            // profile for a game of no known engine packs only files of 16 MiB or more.
            &Lz4Packs {
                profile: Some(ps5upload_fpkg::ampr_pack::default_profile(9, 16)),
                level: 9,
                block_shift: 16,
            },
            VerifyMode::Full,
            DEFAULT_VERIFY_CHUNK,
            &cancel,
            &mut |s, _, _| {
                if stages.last().map(String::as_str) != Some(s) {
                    stages.push(s.to_string());
                }
            },
        )
        .unwrap();
        assert_eq!(stages, ["pack", "plan", "write", "verify"]);
        assert!(built.detail.contains("LZ4 packs"), "{}", built.detail);
        assert!(!lz4_spool(&out).exists(), "the spool is removed");

        let mut image = ps5upload_fpkg::exfat::ExFatSource::open(&out).unwrap();
        let in_image: Vec<String> = image.files().iter().map(|f| f.path.clone()).collect();
        let reader = Reader::open(&mut image, "ampr_assets.index").unwrap();
        let mut source = FolderSource::open(&src).unwrap();
        let mut packed = 0;
        for f in source.files().to_vec() {
            let i = reader
                .find(&f.path)
                .expect("every source file is in the manifest");
            let want = source.read(&f.path).unwrap();
            if reader.manifest.files[i].packed() {
                packed += 1;
                assert!(
                    !in_image.contains(&f.path),
                    "{} is packed and loose",
                    f.path
                );
                assert_eq!(
                    reader.read_file(i, &mut image, "").unwrap(),
                    want,
                    "{}",
                    f.path
                );
            } else {
                assert_eq!(image.read(&f.path).unwrap(), want, "{}", f.path);
            }
        }
        assert!(packed >= 3, "data files are packed ({packed})");
        // The default profile keeps the module, system files and text config loose.
        for loose in [
            "eboot.bin",
            "fakelib/libSceAmpr.sprx",
            "sce_sys/param.json",
            "data/settings.json",
        ] {
            assert!(in_image.contains(&loose.to_string()), "{loose}");
        }
        // A folder packing emptied is still there.
        assert!(image
            .empty_dirs()
            .iter()
            .any(|d| d.trim_matches('/') == "data/only_packed"));
        let index =
            ps5upload_fpkg::ampr_index::parse(&image.read("ampr_emu.index").unwrap()).unwrap();
        assert_eq!(index.len(), source.files().len());
        assert!(index
            .iter()
            .any(|e| e.path == "data/only_packed/table.bin" && e.size == 400_000));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A plain image of a libSceAmpr game carries a fresh index listing the image's files
    /// (#449), never the dump's stale one.
    #[test]
    fn a_plain_image_regenerates_a_stale_ampr_index() {
        let root = std::env::temp_dir().join(format!("ps5upload-imgi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = ampr_game(&root);
        std::fs::write(src.join("ampr_emu.index"), b"AMPRIDX3 stale").unwrap();
        let out = root.join("plain.exfat");
        let mut tree = FolderSource::open(&src).unwrap();
        let files_in_source = tree.files().len();
        let mut indexed = AmprIndexed::wrap(&mut tree).unwrap().expect("an AMPR game");
        build(
            ImageFormat::Exfat,
            None,
            &mut indexed,
            &out,
            VerifyMode::Full,
            DEFAULT_VERIFY_CHUNK,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        let mut image = open_image(&out, ImageFormat::Exfat, false).unwrap();
        let index =
            ps5upload_fpkg::ampr_index::parse(&image.read("ampr_emu.index").unwrap()).unwrap();
        // Every file but the index itself, each at its size.
        assert_eq!(index.len(), files_in_source - 1);
        assert!(index
            .iter()
            .any(|e| e.path == "eboot.bin" && e.size == 300_000));
        // A game that does not read through libSceAmpr is left alone.
        let plain = game(&root.join("noampr"));
        let mut t2 = FolderSource::open(&plain).unwrap();
        assert!(AmprIndexed::wrap(&mut t2).unwrap().is_none());
        // An LZ4-packed game keeps its index: it also lists the files inside the packs.
        std::fs::write(src.join("ampr_assets.index"), b"AMPRPAK4 and the rest").unwrap();
        let mut t3 = FolderSource::open(&src).unwrap();
        assert!(AmprIndexed::wrap(&mut t3).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Packing is exFAT-only and leaves nothing behind when it is refused.
    #[test]
    fn lz4_packs_are_refused_for_other_formats() {
        let root = std::env::temp_dir().join(format!("ps5upload-imgr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = ampr_game(&root);
        let out = root.join("x.ffpkg");
        let mut tree = FolderSource::open(&src).unwrap();
        let e = build_packed(
            ImageFormat::Ffpkg,
            &mut tree,
            &out,
            &Lz4Packs {
                profile: None,
                level: 9,
                block_shift: 16,
            },
            VerifyMode::Full,
            DEFAULT_VERIFY_CHUNK,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .err()
        .expect("refused");
        assert!(e.contains("exFAT"), "{e}");
        assert!(!out.exists() && !lz4_spool(&out).exists());

        let exfat = root.join("x.exfat");
        let attempt = |src: &Path| {
            let mut tree = FolderSource::open(src).unwrap();
            build_packed(
                ImageFormat::Exfat,
                &mut tree,
                &exfat,
                &Lz4Packs {
                    profile: None,
                    level: 9,
                    block_shift: 16,
                },
                VerifyMode::Full,
                DEFAULT_VERIFY_CHUNK,
                &AtomicBool::new(false),
                &mut |_, _, _| {},
            )
            .err()
            .expect("refused")
        };
        // An ampr_emu without pack support (0.3) would see every packed file missing.
        std::fs::write(
            src.join("fakelib/libSceAmpr.sprx"),
            b"\x000.3.1 (c) Drakmor\0AMPRIDX3",
        )
        .unwrap();
        let e = attempt(&src);
        assert!(
            e.contains("ampr_emu 0.3.1") && e.contains("does not serve"),
            "{e}"
        );
        // No ampr_emu in the game at all.
        std::fs::remove_file(src.join("fakelib/libSceAmpr.sprx")).unwrap();
        assert!(attempt(&src).contains("has none"));
        // A title that does not read through libSceAmpr.
        let plain = game(&root.join("plain"));
        assert!(attempt(&plain).contains("does not read its files through AMPR"));
        assert!(!exfat.exists() && !lz4_spool(&exfat).exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
