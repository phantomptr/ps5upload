//! A game folder as a packed game: what an image writer is given in place of the folder.
//!
//! [`prepare`] packs the folder's files into volumes in a spool folder and returns an
//! [`Overlay`]: the folder minus every packed original, plus a regenerated `ampr_emu.index`
//! (listing the original files, which is what `ampr_emu` resolves), the manifest and its
//! sidecars, and the volumes. A folder emptied by packing is kept as an empty folder.

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::config::{pack_output_glob, Config};
use super::{glob, pack, runtime_support, Control, Output};
use crate::ampr_index;
use crate::source::{SourceFile, SourceTree};
use crate::{format_err, Error, Result};

/// Where `ampr_emu` lives in a backported game.
pub const RUNTIME_MODULE: &str = "fakelib/libSceAmpr.sprx";

/// Whether a libSceAmpr title can be packed, as the Convert screen shows it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Readiness {
    /// The version the game's `ampr_emu` reports, when its module says.
    pub runtime_version: Option<String>,
    /// Why packing is refused; `None` when it can go ahead.
    pub refusal: Option<String>,
    /// `ok`; `runtime` (the game's ampr_emu is missing or too old: installing
    /// [`RUNTIME_VERSION`] into the image makes packs possible); `no_ampr` (the game does not
    /// read through AMPR, so nothing could serve packs: .ffpfsc is its compression).
    pub kind: &'static str,
    /// The game reads through AMPR and ships no emulator at all: it won't start from an image
    /// or folder without one, packs or not ([`emulator_needed_at`]).
    pub emulator_missing: bool,
}

/// The ampr_emu release the engine can put into an image (`fakelib/libSceAmpr.sprx`) when the
/// game's own is missing or predates asset packs; the game's own copy is kept beside it as
/// `libSceAmpr.sprx.orig`. drakmor/ampr_emu, GPL-3.0; fetched on first use, never bundled.
pub const RUNTIME_VERSION: &str = "0.4.2.1";
pub const RUNTIME_URL: &str =
    "https://github.com/drakmor/ampr_emu/releases/download/0.4.2.1/libSceAmpr.sprx";
pub const RUNTIME_SHA256: &str = "69e6c4d5e4f5fb83c9e01815db5861c4c75734acbf4595cafa50d4c218116d1a";
/// Where the game's own module goes when the engine installs the release one.
pub const RUNTIME_MODULE_ORIG: &str = "fakelib/libSceAmpr.sprx.orig";

/// `Some(no_ampr)` when the title does not import libSceAmpr: nothing but `ampr_emu` reads
/// asset packs. Otherwise whether the `libSceAmpr.sprx` the game ships serves them — an
/// older `ampr_emu` (0.3) would find every packed file missing. The module is recognised by
/// its own strings (see [`runtime_support`]).
pub fn readiness(tree: &mut dyn SourceTree) -> Option<Readiness> {
    let emulator_missing = emulator_needed_at(tree).is_some();
    readiness_for_packs(tree).map(|r| Readiness {
        emulator_missing,
        ..r
    })
}

fn readiness_for_packs(tree: &mut dyn SourceTree) -> Option<Readiness> {
    if !crate::source::imports_ampr(tree, "eboot.bin") {
        return Some(Readiness {
            runtime_version: None,
            refusal: Some(
                "this game does not read its files through AMPR, so nothing could serve \
                 asset packs; compress it into a .ffpfsc instead"
                    .into(),
            ),
            kind: "no_ampr",
            emulator_missing: false,
        });
    }
    let Some(module) = tree
        .files()
        .iter()
        .find(|f| f.path.eq_ignore_ascii_case(RUNTIME_MODULE))
        .map(|f| f.path.clone())
    else {
        return Some(Readiness {
            runtime_version: None,
            refusal: Some(format!(
                "LZ4 asset packs need ampr_emu 0.4 or later with pack support in the game's \
                 {RUNTIME_MODULE}, and the folder has none"
            )),
            kind: "runtime",
            emulator_missing: false,
        });
    };
    let support = match tree.read(&module) {
        Ok(bytes) => runtime_support(&bytes),
        Err(e) => {
            return Some(Readiness {
                runtime_version: None,
                refusal: Some(format!("{module} could not be read: {e}")),
                kind: "runtime",
                emulator_missing: false,
            })
        }
    };
    let refusal = (!support.packs).then(|| {
        format!(
            "the game's {module} ({}) does not serve asset packs; LZ4 packs need ampr_emu 0.4 \
             or later with pack support",
            support
                .version
                .as_deref()
                .map_or("unknown version".to_string(), |v| format!("ampr_emu {v}"))
        )
    });
    Some(Readiness {
        kind: if refusal.is_some() { "runtime" } else { "ok" },
        emulator_missing: false,
        runtime_version: support.version,
        refusal,
    })
}

/// Where a game's emulator goes when a new one takes the place of any it has: its `fakelib2` when
/// it has one, else its `fakelib`, in the game's own spelling.
pub fn emulator_home(tree: &dyn SourceTree) -> String {
    for dir in ["fakelib2", "fakelib"] {
        let prefix = format!("{dir}/");
        for file in tree.files() {
            if let Some(found) = file.path.get(..prefix.len()) {
                if found.eq_ignore_ascii_case(&prefix) {
                    return format!("{found}libSceAmpr.sprx");
                }
            }
        }
    }
    RUNTIME_MODULE.to_string()
}

/// Where an AMPR emulator has to go for a game that reads its files through libSceAmpr and ships
/// no emulator; `None` for any other game. Without one the game fails to start from an image or
/// folder (Alembic's console test: `APR_FATAL_ERROR_ASYNC`, or an abort in eboot.bin), and
/// ShadowMount+ doesn't add one. A game with a `fakelib2` loads from there, in its own spelling.
pub fn emulator_needed_at(tree: &mut dyn SourceTree) -> Option<String> {
    if !crate::source::imports_ampr(tree, "eboot.bin") {
        return None;
    }
    emulator_place(&*tree)
}

fn emulator_place(tree: &dyn SourceTree) -> Option<String> {
    let mut home = None;
    for dir in ["fakelib2", "fakelib"] {
        let prefix = format!("{dir}/");
        for file in tree.files() {
            let Some(found) = file.path.get(..prefix.len()) else {
                continue;
            };
            if !found.eq_ignore_ascii_case(&prefix) {
                continue;
            }
            if file.path[prefix.len()..].eq_ignore_ascii_case("libSceAmpr.sprx") {
                return None;
            }
            home.get_or_insert_with(|| found.to_string());
        }
    }
    let home = home.unwrap_or_else(|| "fakelib/".to_string());
    Some(format!("{home}libSceAmpr.sprx"))
}

/// A game in LZ4 asset packs read as the game it was: every packed file by its own name, decoded
/// chunk by chunk from the volumes, which are hidden with the rest of the pack set. Lets a packed
/// game go straight into another pack format, with no copy on disk.
pub struct Lz4Unpacked {
    inner: Box<dyn SourceTree>,
    reader: super::read::Reader,
    files: Vec<SourceFile>,
    packed: HashMap<String, usize>,
    /// The chunk read last: reads run in small pieces, chunks in larger ones.
    last: Option<(usize, u32, Vec<u8>)>,
}

/// What [`Lz4Unpacked::open`] made of a game.
pub enum Unpacking {
    Decoded(Box<Lz4Unpacked>),
    /// It has no pack set: the game as it was given.
    NotPacked(Box<dyn SourceTree>),
}

impl Lz4Unpacked {
    /// `inner` decoded, when it has a pack set.
    pub fn open(mut inner: Box<dyn SourceTree>) -> Result<Unpacking> {
        let index_name = Config::default().index_name;
        if !crate::source::lz4_packed(inner.as_mut())? {
            return Ok(Unpacking::NotPacked(inner));
        }
        let reader = super::read::Reader::open(inner.as_mut(), &index_name)?;
        let m = &reader.manifest;
        let mut packed = HashMap::new();
        let mut names = BTreeSet::new();
        for id in 0..m.packs.len() {
            names.insert(m.pack_name(id)?.to_ascii_lowercase());
        }
        let mut files: Vec<SourceFile> = inner
            .files()
            .iter()
            .filter(|f| {
                let lower = f.path.to_ascii_lowercase();
                !names.contains(&lower) && !lower.starts_with(&index_name)
            })
            .cloned()
            .collect();
        for (i, f) in m.files.iter().enumerate() {
            if !f.packed() {
                continue;
            }
            let path = m.path(i)?;
            let path = path.strip_prefix("/app0/").unwrap_or(path).to_string();
            if !files
                .iter()
                .any(|known| known.path.eq_ignore_ascii_case(&path))
            {
                files.push(SourceFile {
                    path: path.clone(),
                    size: f.logical_size,
                });
                packed.insert(path, i);
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Unpacking::Decoded(Box::new(Self {
            inner,
            reader,
            files,
            packed,
            last: None,
        })))
    }
}

impl SourceTree for Lz4Unpacked {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }
    fn empty_dirs(&self) -> &[String] {
        self.inner.empty_dirs()
    }
    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        match self.packed.get(path) {
            Some(&i) => self.reader.read_file(i, self.inner.as_mut(), ""),
            None => self.inner.read(path),
        }
    }
    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let Some(&i) = self.packed.get(path) else {
            return self.inner.read_range(path, offset, len);
        };
        let f = &self.reader.manifest.files[i];
        let size = f.logical_size;
        let chunk_size = 1u64 << f.block_shift;
        let end = offset.saturating_add(len as u64).min(size);
        let mut out = Vec::with_capacity(end.saturating_sub(offset) as usize);
        let mut at = offset;
        while at < end {
            let local = (at / chunk_size) as u32;
            if !matches!(&self.last, Some((file, chunk, _)) if *file == i && *chunk == local) {
                let raw = self.reader.read_chunk(i, local, self.inner.as_mut(), "")?;
                self.last = Some((i, local, raw));
            }
            let Some((_, _, raw)) = &self.last else {
                break;
            };
            let from = (at % chunk_size) as usize;
            let take = raw.len().saturating_sub(from).min((end - at) as usize);
            if take == 0 {
                break;
            }
            out.extend_from_slice(&raw[from..from + take]);
            at += take as u64;
        }
        Ok(out)
    }
    fn describe(&self) -> String {
        format!("{} (LZ4 packs read)", self.inner.describe())
    }
}

/// A game with the release ampr_emu in place of its own `fakelib/libSceAmpr.sprx` (kept as
/// `libSceAmpr.sprx.orig`), or added when it had none. `bytes` must serve packs.
pub struct RuntimeInstalled {
    inner: Box<dyn SourceTree>,
    files: Vec<crate::source::SourceFile>,
    runtime: Vec<u8>,
    orig_path: Option<String>,
    module: String,
    orig_name: String,
}

impl RuntimeInstalled {
    pub fn new(inner: Box<dyn SourceTree>, runtime: Vec<u8>) -> crate::Result<Self> {
        Self::new_at(inner, runtime, RUNTIME_MODULE)
    }

    /// [`Self::new`] at `module`: a game with a `fakelib2` keeps its emulator there.
    pub fn new_at(
        inner: Box<dyn SourceTree>,
        runtime: Vec<u8>,
        module: &str,
    ) -> crate::Result<Self> {
        let orig_name = format!("{module}.orig");
        let support = runtime_support(&runtime);
        if !support.packs {
            return Err(crate::Error::Format(
                "the ampr_emu to install does not serve asset packs".into(),
            ));
        }
        let orig_path = inner
            .files()
            .iter()
            .find(|f| f.path.eq_ignore_ascii_case(module))
            .map(|f| f.path.clone());
        let mut files: Vec<crate::source::SourceFile> = inner
            .files()
            .iter()
            .filter(|f| {
                !f.path.eq_ignore_ascii_case(module) && !f.path.eq_ignore_ascii_case(&orig_name)
            })
            .cloned()
            .collect();
        if let Some(orig) = &orig_path {
            let size = inner
                .files()
                .iter()
                .find(|f| &f.path == orig)
                .map_or(0, |f| f.size);
            files.push(crate::source::SourceFile {
                path: orig_name.clone(),
                size,
            });
        }
        files.push(crate::source::SourceFile {
            path: module.to_string(),
            size: runtime.len() as u64,
        });
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Self {
            inner,
            files,
            runtime,
            orig_path,
            module: module.to_string(),
            orig_name,
        })
    }

    fn resolve(&self, path: &str) -> Option<&str> {
        if path.eq_ignore_ascii_case(&self.orig_name) {
            return self.orig_path.as_deref();
        }
        None
    }
}

impl SourceTree for RuntimeInstalled {
    fn files(&self) -> &[crate::source::SourceFile] {
        &self.files
    }
    fn empty_dirs(&self) -> &[String] {
        self.inner.empty_dirs()
    }
    fn read(&mut self, path: &str) -> crate::Result<Vec<u8>> {
        if path.eq_ignore_ascii_case(&self.module) {
            return Ok(self.runtime.clone());
        }
        if let Some(orig) = self.resolve(path).map(str::to_string) {
            return self.inner.read(&orig);
        }
        self.inner.read(path)
    }
    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> crate::Result<Vec<u8>> {
        if path.eq_ignore_ascii_case(&self.module) {
            let start = (offset as usize).min(self.runtime.len());
            let end = start.saturating_add(len).min(self.runtime.len());
            return Ok(self.runtime[start..end].to_vec());
        }
        if let Some(orig) = self.resolve(path).map(str::to_string) {
            return self.inner.read_range(&orig, offset, len);
        }
        self.inner.read_range(path, offset, len)
    }
    fn describe(&self) -> String {
        format!(
            "{} (ampr_emu {RUNTIME_VERSION} installed)",
            self.inner.describe()
        )
    }
}

/// Why `tree` cannot be packed for `ampr_emu`, or `None` when it can.
pub fn refusal(tree: &mut dyn SourceTree) -> Option<String> {
    match readiness(tree) {
        None => Some(
            "LZ4 asset packs are read by ampr_emu, and this game's eboot.bin does not import \
             libSceAmpr"
                .into(),
        ),
        Some(r) => r.refusal,
    }
}

/// Files an earlier AMPR build or run left, which a fresh index and pack set replace.
fn generated(path: &str, config: &Config, pack_glob: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let index = config.index_name.to_ascii_lowercase();
    lower == ampr_index::NAME
        || lower == "ampr_commands.bin"
        || lower == "apr_emu.log"
        || lower == index
        || lower == format!("{index}.crc")
        || lower == format!("{index}.runtime")
        || glob::matches_any(path, &[pack_glob])
}

/// What the image holds instead of the folder.
pub struct Overlay<'a> {
    source: &'a mut dyn SourceTree,
    files: Vec<SourceFile>,
    empty_dirs: Vec<String>,
    memory: HashMap<String, Vec<u8>>,
    volumes: HashMap<String, PathBuf>,
    open: Option<(String, std::fs::File)>,
    label: String,
}

impl SourceTree for Overlay<'_> {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let size = self
            .files
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.size)
            .ok_or_else(|| Error::Format(format!("{path} is not in the packed image")))?;
        self.read_range(path, 0, size as usize)
    }

    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        if let Some(bytes) = self.memory.get(path) {
            let at = (offset as usize).min(bytes.len());
            return Ok(bytes[at..(at + len).min(bytes.len())].to_vec());
        }
        if let Some(file) = self.volumes.get(path) {
            if self.open.as_ref().is_none_or(|(p, _)| p != path) {
                self.open = Some((path.to_string(), std::fs::File::open(file)?));
            }
            let (_, f) = self.open.as_mut().unwrap();
            f.seek(SeekFrom::Start(offset))?;
            // `len` can come from the image's records: reserve at most a few MiB up front, and let
            // read_to_end grow the buffer to what the file really holds.
            let mut buf = Vec::with_capacity(crate::prealloc(len));
            f.by_ref().take(len as u64).read_to_end(&mut buf)?;
            return Ok(buf);
        }
        self.source.read_range(path, offset, len)
    }

    fn empty_dirs(&self) -> &[String] {
        &self.empty_dirs
    }

    fn describe(&self) -> String {
        self.label.clone()
    }
}

/// Pack `source` by `config` into `spool` (which must not exist yet) and return the tree the
/// image is written from, with the pack report. Every file is listed in the regenerated
/// `ampr_emu.index` at `mtime`.
pub fn prepare<'a>(
    source: &'a mut dyn SourceTree,
    config: &Config,
    spool: &Path,
    mtime: i64,
    control: &mut Control,
) -> Result<(Overlay<'a>, Output)> {
    let pack_glob = pack_output_glob(&config.pack_pattern)?;
    if source.files().iter().any(|f| {
        f.path.eq_ignore_ascii_case(&config.index_name) || glob::matches_any(&f.path, &[&pack_glob])
    }) {
        return format_err(
            "the folder already carries AMPR asset packs; its packed files are not there to \
             pack again",
        );
    }
    let logical: Vec<ampr_index::Entry> = source
        .files()
        .iter()
        .filter(|f| !generated(&f.path, config, &pack_glob))
        .map(|f| ampr_index::Entry {
            path: f.path.clone(),
            size: f.size,
            mtime,
        })
        .collect();
    let index = ampr_index::build_entries(&logical).map_err(Error::Format)?;
    let entries = ampr_index::parse(&index)
        .ok_or_else(|| Error::Format("the generated index does not read back".into()))?;
    std::fs::create_dir(spool).map_err(|e| Error::Format(format!("{}: {e}", spool.display())))?;
    let output = match pack::build(&entries, source, config, spool, control) {
        Ok(o) => o,
        Err(e) => {
            let _ = std::fs::remove_dir_all(spool);
            return Err(e);
        }
    };

    let sizes: HashMap<&str, u64> = source
        .files()
        .iter()
        .map(|f| (f.path.as_str(), f.size))
        .collect();
    let mut files: Vec<SourceFile> = output
        .loose
        .iter()
        .map(|p| SourceFile {
            path: p.clone(),
            size: sizes[p.as_str()],
        })
        .collect();
    let mut memory = HashMap::new();
    files.push(SourceFile {
        path: ampr_index::NAME.to_string(),
        size: index.len() as u64,
    });
    memory.insert(ampr_index::NAME.to_string(), index);
    for (name, bytes) in output.small_files() {
        files.push(SourceFile {
            path: name.clone(),
            size: bytes.len() as u64,
        });
        memory.insert(name, bytes.to_vec());
    }
    let mut volumes = HashMap::new();
    for (name, path) in &output.volumes {
        files.push(SourceFile {
            path: name.clone(),
            size: std::fs::metadata(path)?.len(),
        });
        volumes.insert(name.clone(), path.clone());
    }

    // Folders packing emptied stay, as empty folders: only the innermost need naming.
    let mut kept: BTreeSet<String> = BTreeSet::new();
    let source_empty: Vec<String> = source
        .empty_dirs()
        .iter()
        .map(|d| d.trim_matches('/').to_string())
        .collect();
    for d in &source_empty {
        kept.insert(d.clone());
    }
    for path in files
        .iter()
        .map(|f| f.path.as_str())
        .chain(source_empty.iter().map(|d| d.as_str()))
    {
        let mut p = path;
        while let Some((dir, _)) = p.rsplit_once('/') {
            kept.insert(dir.to_string());
            p = dir;
        }
    }
    let mut emptied: BTreeSet<String> = BTreeSet::new();
    for p in &output.packed {
        let mut p = p.as_str();
        while let Some((dir, _)) = p.rsplit_once('/') {
            if !kept.contains(dir) {
                emptied.insert(dir.to_string());
            }
            p = dir;
        }
    }
    let mut empty_dirs = source_empty;
    for d in &emptied {
        let prefix = format!("{d}/");
        if !emptied.iter().any(|o| o.starts_with(&prefix)) {
            empty_dirs.push(d.clone());
        }
    }
    let label = format!("{} with LZ4 asset packs", source.describe());
    Ok((
        Overlay {
            source,
            files,
            empty_dirs,
            memory,
            volumes,
            open: None,
            label,
        },
        output,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The image holds the loose files and the pack set, not the packed originals; a folder
    /// packing emptied stays (only the innermost is named), and the folder's own empty folders
    /// are kept as they were.
    #[test]
    fn the_overlay_swaps_packed_files_for_the_pack_set() {
        let root = std::env::temp_dir().join(format!(
            "ampr-overlay-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("game");
        for (p, d) in [
            ("eboot.bin", vec![1u8; 100]),
            ("d/a.bin", vec![b'a'; 70_000]),
            ("x/keep.txt", b"keep".to_vec()),
            ("x/y/z.bin", vec![b'z'; 1000]),
            // A stale index and a run's trace are replaced, never carried.
            ("ampr_emu.index", b"stale".to_vec()),
            ("ampr_commands.bin", b"trace".to_vec()),
        ] {
            let f = src.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, d).unwrap();
        }
        std::fs::create_dir_all(src.join("d/shaders")).unwrap();
        let mut tree = crate::source::FolderSource::open(&src).unwrap();
        let config = super::super::default_profile(9, 16);
        let spool = root.join("spool");
        let (mut overlay, out) =
            prepare(&mut tree, &config, &spool, 7, &mut Control::default()).unwrap();
        let mut names: Vec<String> = overlay.files().iter().map(|f| f.path.clone()).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "ampr_assets-000.pak",
                "ampr_assets.index",
                "ampr_assets.index.crc",
                "ampr_emu.index",
                "eboot.bin",
                "x/keep.txt"
            ]
        );
        let mut empty = overlay.empty_dirs().to_vec();
        empty.sort();
        assert_eq!(empty, ["d/shaders", "x/y"]);
        assert_eq!(out.packed.len(), 2);
        let index = ampr_index::parse(&overlay.read("ampr_emu.index").unwrap()).unwrap();
        let mut listed: Vec<&str> = index.iter().map(|e| e.path.as_str()).collect();
        listed.sort();
        assert_eq!(listed, ["d/a.bin", "eboot.bin", "x/keep.txt", "x/y/z.bin"]);
        assert!(index.iter().all(|e| e.mtime == 7));
        // A volume reads back whole through the overlay.
        let pak = overlay.read("ampr_assets-000.pak").unwrap();
        assert_eq!(
            pak,
            std::fs::read(spool.join("ampr_assets-000.pak")).unwrap()
        );
        drop(overlay);

        // A folder that already carries packs is refused: its packed originals are gone.
        std::fs::write(src.join("ampr_assets.index"), b"x").unwrap();
        let mut tree = crate::source::FolderSource::open(&src).unwrap();
        let e = prepare(
            &mut tree,
            &config,
            &root.join("spool2"),
            7,
            &mut Control::default(),
        )
        .err()
        .unwrap();
        assert!(e.to_string().contains("already carries"), "{e}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The inspection's view: nothing for a title that does not use AMPR; for one that does,
    /// the runtime's version and whether it serves packs.
    #[test]
    fn readiness_reads_the_games_ampr_emu() {
        let root = std::env::temp_dir().join(format!(
            "ampr-ready-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("fakelib")).unwrap();
        std::fs::write(root.join("eboot.bin"), vec![0u8; 4096]).unwrap();
        let check = || readiness(&mut crate::source::FolderSource::open(&root).unwrap());
        assert_eq!(check().unwrap().kind, "no_ampr");

        let mut eboot = vec![0u8; 4096];
        eboot[100..114].copy_from_slice(b"libSceAmpr.prx");
        std::fs::write(root.join("eboot.bin"), eboot).unwrap();
        assert!(check().unwrap().refusal.unwrap().contains("has none"));

        std::fs::write(
            root.join("fakelib/libSceAmpr.sprx"),
            b"x\x000.4.2.1 (c) Drakmor\0..AMPRPAK4..ampr_assets.index",
        )
        .unwrap();
        assert_eq!(
            check(),
            Some(Readiness {
                runtime_version: Some("0.4.2.1".into()),
                refusal: None,
                kind: "ok",
                emulator_missing: false,
            })
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The release runtime goes in as the game's module, the game's own kept as `.orig`;
    /// a game that had none just gains it.
    #[test]
    fn the_emulator_goes_where_the_game_loads_it() {
        struct Files(Vec<SourceFile>);
        impl SourceTree for Files {
            fn files(&self) -> &[SourceFile] {
                &self.0
            }
            fn read(&mut self, path: &str) -> crate::Result<Vec<u8>> {
                Ok(
                    if path == "eboot.bin" && self.0.iter().any(|f| f.path == path) {
                        b"..libSceAmpr.prx..".to_vec()
                    } else {
                        Vec::new()
                    },
                )
            }
            fn describe(&self) -> String {
                "test".into()
            }
        }
        let at = |paths: &[&str]| {
            let files = paths
                .iter()
                .map(|p| SourceFile {
                    path: (*p).to_string(),
                    size: 18,
                })
                .collect();
            emulator_needed_at(&mut Files(files))
        };
        let need = |p: &str| Some(p.to_string());
        assert_eq!(at(&["eboot.bin"]), need("fakelib/libSceAmpr.sprx"));
        assert_eq!(
            at(&["eboot.bin", "fakelib/libA.sprx"]),
            need("fakelib/libSceAmpr.sprx")
        );
        assert_eq!(
            at(&["eboot.bin", "fakelib/libA.sprx", "FakeLib2/libB.sprx"]),
            need("FakeLib2/libSceAmpr.sprx")
        );
        assert_eq!(at(&["eboot.bin", "fakelib/LIBSCEAMPR.SPRX"]), None);
        assert_eq!(at(&["eboot.bin", "fakelib2/libSceAmpr.sprx"]), None);
        assert_eq!(at(&["other.bin"]), None, "no eboot, no AMPR");
    }

    #[test]
    fn installing_the_runtime_replaces_and_keeps_the_original() {
        use crate::source::SourceTree;
        let root = std::env::temp_dir().join(format!("ps5upload-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("fakelib")).unwrap();
        std::fs::write(root.join("eboot.bin"), b"libSceAmpr.prx").unwrap();
        std::fs::write(root.join("fakelib/libSceAmpr.sprx"), b"old module").unwrap();
        let good = b"x\x000.4.2.1 (c) Drakmor\0..AMPRPAK4..ampr_assets.index".to_vec();
        let tree: Box<dyn SourceTree> = Box::new(crate::source::FolderSource::open(&root).unwrap());
        assert!(RuntimeInstalled::new(
            Box::new(crate::source::FolderSource::open(&root).unwrap()),
            b"not a runtime".to_vec()
        )
        .is_err());
        let mut t = RuntimeInstalled::new(tree, good.clone()).unwrap();
        let paths: Vec<&str> = t.files().iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&RUNTIME_MODULE) && paths.contains(&RUNTIME_MODULE_ORIG));
        assert_eq!(t.read(RUNTIME_MODULE).unwrap(), good);
        assert_eq!(t.read(RUNTIME_MODULE_ORIG).unwrap(), b"old module");
        assert_eq!(t.read_range(RUNTIME_MODULE, 2, 7).unwrap(), b"0.4.2.1");
        assert_eq!(readiness(&mut t).unwrap().kind, "ok");
        let _ = std::fs::remove_dir_all(&root);
    }
}
