//! Leaving the AMPR emulator out of a package of a game that is NOT in asset packs: a plain
//! folder dump that still carries `fakelib/libSceAmpr.sprx` (ampr_emu or ampr-emu-ng), its
//! `ampr_emu.index` and what a run of it wrote. [`crate::unpack_packs`] does the same for a
//! packed game it unpacks; this is the plain game's way.
//!
//! Why a package can do without it. An installed package is a real PFS, which the console's own
//! libSceAmpr reads; the emulator stands in for it where a game is played from ShadowMount+'s
//! mount. Measured, not assumed: Spider-Man 2 (PPSA03016, a plain dump that imports libSceAmpr
//! and carries ampr_emu) played on FW 5.10 from the user's reference package, which leaves
//! `fakelib/libSceAmpr.sprx` out, and from one of ours built the same way (2026-09-25). One game
//! on one firmware, so it is the user's choice, never the default; a package of a game that
//! imports libSceAmpr still carries a fresh `ampr_emu.index` (the package build makes one, as
//! that reference package had), which nothing reads once the emulator is gone.
//!
//! Where it is not offered, and why ([`Refusal`]):
//! - a `libSceAmpr.sprx` there that is not an AMPR emulator: it may be a newer system library a
//!   backport put in place of the console's own, and a backport's library set is load-bearing
//!   (Venus Vacation PRISM ran 4/4 with its own set and 0/4 with another, 2026-09-10);
//! - pack files at the root, or an `ampr_emu.index` that lists files the game doesn't have:
//!   something there is read only through the emulator (a packed game's index lists the files
//!   inside its packs).
//!
//! Only `libSceAmpr.sprx` (and a `.orig` copy an earlier build kept) in `fakelib` and
//! `fakelib2`, our trace marker beside it, `ampr_emu.index` (and the `.bak` a rebuild kept) and
//! the run's journal and logs go; every other fakelib stays.

use ps5upload_fpkg::ampr_index;
use ps5upload_fpkg::ampr_pack::ng;
use ps5upload_fpkg::source::{SourceFile, SourceTree};
use serde::{Deserialize, Serialize};

use crate::unpack_packs::{is_emulator_file, LEFT_OUT};

/// The marker the engine's "Measure how it plays" puts beside the debug emulator it places in
/// `fakelib2` (`ampr_trace.rs`): it goes with that emulator.
const TRACE_MARKER: &str = "fakelib2/.ps5upload-trace";

/// The largest `libSceAmpr.sprx` read to tell what it is (an emulator is about 1 MiB).
const MAX_MODULE: u64 = 64 << 20;
/// The largest `ampr_emu.index` read (a game of 300,000 files has one of about 15 MiB).
const MAX_INDEX: u64 = 256 << 20;

/// How a refusal to leave the emulator out starts, so a caller can tell it from another error.
pub const CANT_LEAVE_OUT: &str = "the AMPR emulator can't be left out of this game";

/// Why the emulator can't be left out of a plain game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// A `libSceAmpr.sprx` in `fakelib`/`fakelib2` is not an AMPR emulator: it may be a system
    /// library a backport needs.
    NotAnEmulator,
    /// Asset pack files are at the game's root, though they are not a pack set this app reads.
    PackFiles,
    /// The game's `ampr_emu.index` lists files it doesn't have: they may be inside packs only
    /// the emulator reads.
    IndexListsMissing,
}

/// What a plain game carries of an AMPR emulator, and whether a package can leave it out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlainEmulator {
    /// Its modules: every `libSceAmpr.sprx` in `fakelib` and `fakelib2`.
    pub modules: Vec<String>,
    /// What it is, for people: `ampr_emu 0.4.2.1`, `ampr-emu-ng 0.4.1`, `an AMPR emulator`, or
    /// for a module that is none, `a libSceAmpr.sprx that is not an AMPR emulator`.
    pub emulator: String,
    /// The game's eboot imports libSceAmpr: without the emulator the console's own serves it.
    pub imports_ampr: bool,
    /// Why it can't be left out; `None` when it can.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
    /// With a refusal, the facts behind it in a line (a module's path, a file the index lists).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl PlainEmulator {
    /// A package can leave it out.
    pub fn offered(&self) -> bool {
        self.refusal.is_none()
    }
}

/// What `module` (a `libSceAmpr.sprx`) is by name, `None` when it is not an AMPR emulator. One
/// this app knows by release is named with it; another counts as an emulator when it names one
/// of the two projects, which a Sony library never does.
fn emulator_name(module: &[u8]) -> Option<String> {
    match ng::classify(module) {
        (kind @ ("ampr_emu" | "ampr-emu-ng"), Some(v), _) => Some(format!("{kind} {v}")),
        _ => {
            let has = |m: &[u8]| module.windows(m.len()).any(|w| w == m);
            (has(b"ampr_emu") || has(b"ampr-emu-ng")).then(|| "an AMPR emulator".to_string())
        }
    }
}

/// A root file that belongs to asset packs: a pack index or volume, in any letter case.
fn is_pack_file(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    !p.contains('/')
        && (p == "ampr_assets.index"
            || p == "ampr_zstd.index"
            || ((p.starts_with("ampr_assets") || p.starts_with("ampr_zstd"))
                && p.ends_with(".pak")))
}

/// Whether `path` is one of the emulator's files a package without it leaves out: its modules
/// (and `.orig` copies), our trace marker, `ampr_emu.index` and its `.bak`, and a run's journal
/// and logs.
pub fn is_left_out(path: &str) -> bool {
    is_emulator_file(path)
        || path.eq_ignore_ascii_case(TRACE_MARKER)
        || path.eq_ignore_ascii_case(ampr_index::NAME)
        || path.eq_ignore_ascii_case(ampr_index::BACKUP)
        || LEFT_OUT.iter().any(|n| path.eq_ignore_ascii_case(n))
}

/// What `tree` carries of an AMPR emulator. `None` for a game in asset packs (Unpack the asset
/// packs has its own way, [`crate::unpack_packs::open_without_emulator`]) and for one with no
/// `libSceAmpr.sprx` in `fakelib` or `fakelib2`. Reads the modules, the eboot's imports and the
/// index; never the game's other data.
pub fn assess(tree: &mut dyn SourceTree) -> Result<Option<PlainEmulator>, String> {
    if ps5upload_fpkg::source::packed_kind(tree)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Ok(None);
    }
    let modules: Vec<String> = tree
        .files()
        .iter()
        .filter(|f| is_emulator_file(&f.path) && !f.path.to_ascii_lowercase().ends_with(".orig"))
        .map(|f| f.path.clone())
        .collect();
    if modules.is_empty() {
        return Ok(None);
    }
    let mut names = Vec::new();
    let mut refusal = None;
    let mut detail = None;
    for path in &modules {
        let size = tree
            .files()
            .iter()
            .find(|f| &f.path == path)
            .map_or(0, |f| f.size);
        let name = if size > MAX_MODULE {
            None
        } else {
            let bytes = ps5upload_fpkg::source::read_small(tree, path, MAX_MODULE)
                .map_err(|e| format!("{path}: {e}"))?;
            emulator_name(&bytes)
        };
        match name {
            Some(n) => names.push(n),
            None => {
                refusal = Some(Refusal::NotAnEmulator);
                detail = Some(path.clone());
                names.push("a libSceAmpr.sprx that is not an AMPR emulator".to_string());
            }
        }
    }
    names.dedup();
    if refusal.is_none() {
        if let Some(f) = tree.files().iter().find(|f| is_pack_file(&f.path)) {
            refusal = Some(Refusal::PackFiles);
            detail = Some(f.path.clone());
        }
    }
    if refusal.is_none() {
        if let Some(missing) = index_lists_missing(tree)? {
            refusal = Some(Refusal::IndexListsMissing);
            detail = Some(missing);
        }
    }
    let imports_ampr = ps5upload_fpkg::source::imports_ampr(tree, "eboot.bin");
    Ok(Some(PlainEmulator {
        modules,
        emulator: names.join(", "),
        imports_ampr,
        refusal,
        detail,
    }))
}

/// The first file the game's `ampr_emu.index` lists that it doesn't have, and how many there
/// are, in a line; `None` when it lists none (or has no index, or one that can't be read, which
/// the package build makes again anyway).
fn index_lists_missing(tree: &mut dyn SourceTree) -> Result<Option<String>, String> {
    let Some(at) = tree
        .files()
        .iter()
        .find(|f| f.path.eq_ignore_ascii_case(ampr_index::NAME) && f.size <= MAX_INDEX)
        .map(|f| f.path.clone())
    else {
        return Ok(None);
    };
    let bytes = ps5upload_fpkg::source::read_small(tree, &at, MAX_INDEX)
        .map_err(|e| format!("{at}: {e}"))?;
    let Some(entries) = ampr_index::parse(&bytes) else {
        return Ok(None);
    };
    let have: std::collections::HashSet<String> = tree
        .files()
        .iter()
        .map(|f| f.path.to_ascii_lowercase())
        .collect();
    let missing: Vec<&str> = entries
        .iter()
        .map(|e| e.path.as_str())
        .filter(|p| !have.contains(&p.to_ascii_lowercase()))
        .collect();
    Ok(missing.first().map(|first| {
        format!(
            "{} lists {} file{} this game doesn't have, {first} among them",
            ampr_index::NAME,
            missing.len(),
            if missing.len() == 1 { "" } else { "s" }
        )
    }))
}

/// Why [`assess`]'s answer refuses, in a sentence that starts with [`CANT_LEAVE_OUT`].
pub fn refusal_text(found: &PlainEmulator) -> Option<String> {
    let why = match found.refusal? {
        Refusal::NotAnEmulator => {
            "its libSceAmpr.sprx is not an AMPR emulator. It may be a newer system library a \
             backport needs, so it stays"
        }
        Refusal::PackFiles => {
            "it has asset pack files at its root, which only the emulator reads, so it stays"
        }
        Refusal::IndexListsMissing => {
            "its ampr_emu.index lists files the game doesn't have, which may be inside packs \
             only the emulator reads, so it stays"
        }
    };
    Some(match &found.detail {
        Some(d) => format!("{CANT_LEAVE_OUT}: {why} ({d})"),
        None => format!("{CANT_LEAVE_OUT}: {why}"),
    })
}

/// The game without its emulator's files ([`is_left_out`]); everything else as it is.
struct Without {
    inner: Box<dyn SourceTree>,
    files: Vec<SourceFile>,
}

impl SourceTree for Without {
    fn release(&mut self) {
        self.inner.release();
    }
    fn files(&self) -> &[SourceFile] {
        &self.files
    }
    fn empty_dirs(&self) -> &[String] {
        self.inner.empty_dirs()
    }
    fn read(&mut self, path: &str) -> ps5upload_fpkg::Result<Vec<u8>> {
        self.inner.read(path)
    }
    fn read_range(
        &mut self,
        path: &str,
        offset: u64,
        len: usize,
    ) -> ps5upload_fpkg::Result<Vec<u8>> {
        self.inner.read_range(path, offset, len)
    }
    fn read_heads(
        &mut self,
        paths: &[String],
        len: usize,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> ps5upload_fpkg::Result<Vec<Vec<u8>>> {
        self.inner.read_heads(paths, len, progress)
    }
    fn describe(&self) -> String {
        format!("{} (AMPR emulator left out)", self.inner.describe())
    }
    fn notes(&self) -> Vec<String> {
        self.inner.notes()
    }
}

/// `tree` with its AMPR emulator left out, for a package: refused (an error that starts with
/// [`CANT_LEAVE_OUT`]) where [`assess`] doesn't offer it, and for a game that has none or is in
/// asset packs. The package build then makes a fresh `ampr_emu.index` for a game that imports
/// libSceAmpr, from exactly the files it packages.
pub fn open_without_emulator(mut tree: Box<dyn SourceTree>) -> Result<Box<dyn SourceTree>, String> {
    let found = assess(&mut *tree)?.ok_or_else(|| {
        if ps5upload_fpkg::source::packed_kind(&mut *tree)
            .ok()
            .flatten()
            .is_some()
        {
            format!(
                "{CANT_LEAVE_OUT} on its own: it is in asset packs, which only the emulator \
                 reads. Unpack the asset packs to leave it out"
            )
        } else {
            format!("{CANT_LEAVE_OUT}: it has none (no libSceAmpr.sprx in fakelib or fakelib2)")
        }
    })?;
    if let Some(why) = refusal_text(&found) {
        return Err(why);
    }
    let mut left_out = Vec::new();
    let mut files = Vec::with_capacity(tree.files().len());
    for f in tree.files() {
        if is_left_out(&f.path) {
            left_out.push(f.path.clone());
        } else {
            files.push(f.clone());
        }
    }
    crate::log_at!(
        Info,
        "convert",
        "leave out the AMPR emulator ({}): left out {}{}",
        found.emulator,
        left_out.join(", "),
        if found.imports_ampr {
            "; the game imports libSceAmpr and reads its plain files through the console's own"
        } else {
            ""
        }
    );
    Ok(Box::new(Without { inner: tree, files }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps5upload_fpkg::source::FolderSource;
    use std::path::{Path, PathBuf};

    /// A scratch folder that goes when the test ends, passed or failed.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let root = crate::test_tmp::temp_path(format!(
                "ps5upload-core-plain-emu-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn ampr_emu(version: &str) -> Vec<u8> {
        let mut m = format!("\0\0ampr_emu {version} (c) Drakmor\0").into_bytes();
        m.extend_from_slice(&[0u8; 32]);
        m
    }

    /// A plain game whose eboot imports libSceAmpr (`ampr`) or not, with `fakelib` holding
    /// `emulator` as libSceAmpr.sprx (none when `None`), a backport library beside it, an
    /// ampr_emu.index of its files and a run's journal and log.
    fn game(root: &Path, ampr: bool, emulator: Option<&[u8]>) -> PathBuf {
        let src = root.join("PPSA99993-app");
        std::fs::create_dir_all(src.join("sce_sys")).unwrap();
        std::fs::create_dir_all(src.join("data")).unwrap();
        std::fs::create_dir_all(src.join("fakelib")).unwrap();
        let mut eboot = vec![0u8; 4096];
        if ampr {
            eboot[1000..1014].copy_from_slice(b"libSceAmpr.prx");
        }
        std::fs::write(src.join("eboot.bin"), eboot).unwrap();
        std::fs::write(
            src.join("sce_sys/param.json"),
            b"{\"titleId\":\"PPSA99993\"}",
        )
        .unwrap();
        std::fs::write(src.join("data/table.bin"), b"asset bytes").unwrap();
        std::fs::write(
            src.join("fakelib/libSceAgc.sprx"),
            b"a newer system library",
        )
        .unwrap();
        if let Some(e) = emulator {
            std::fs::write(src.join("fakelib/libSceAmpr.sprx"), e).unwrap();
        }
        std::fs::write(src.join("ampr_commands.bin"), b"journal").unwrap();
        std::fs::write(src.join("ampr_emu.log"), b"log").unwrap();
        reindex(&src);
        src
    }

    /// Writes `src`'s ampr_emu.index of the files it has now.
    fn reindex(src: &Path) {
        let _ = std::fs::remove_file(src.join(ampr_index::NAME));
        let tree = FolderSource::open(src).unwrap();
        let index = ampr_index::build(&ampr_index::listing(tree.files()), 5).unwrap();
        std::fs::write(src.join(ampr_index::NAME), index).unwrap();
    }

    fn folder(path: &Path) -> Box<dyn SourceTree> {
        Box::new(FolderSource::open(path).unwrap())
    }

    fn paths(tree: &dyn SourceTree) -> Vec<String> {
        tree.files().iter().map(|f| f.path.clone()).collect()
    }

    #[test]
    fn a_plain_games_emulator_is_left_out_and_nothing_else() {
        let ng: &[u8] = b"\0ampr-emu-ng runtime build\0";
        for (name, ampr, emulator, called) in [
            ("drakmor", true, ampr_emu("0.4.2.1"), "ampr_emu 0.4.2.1"),
            ("ng", true, ng.to_vec(), "an AMPR emulator"),
            ("no-import", false, ampr_emu("0.3.6"), "ampr_emu 0.3.6"),
        ] {
            let scratch = Scratch::new(name);
            let src = game(&scratch.0, ampr, Some(&emulator));
            // A trace build in fakelib2 too, with our marker, and a .orig an earlier build kept.
            std::fs::create_dir_all(src.join("fakelib2")).unwrap();
            std::fs::write(src.join("fakelib2/libSceAmpr.sprx"), ampr_emu("0.4.2.1")).unwrap();
            std::fs::write(src.join(TRACE_MARKER), b"").unwrap();
            std::fs::write(src.join("fakelib/libSceAmpr.sprx.orig"), b"old").unwrap();
            reindex(&src);

            let found = assess(&mut *folder(&src)).unwrap().expect(name);
            assert!(found.offered(), "{name}: {found:?}");
            assert_eq!(found.imports_ampr, ampr, "{name}");
            assert!(
                found.emulator.starts_with(called),
                "{name}: {}",
                found.emulator
            );
            assert_eq!(
                found.modules,
                vec!["fakelib/libSceAmpr.sprx", "fakelib2/libSceAmpr.sprx"],
                "{name}"
            );

            let mut bare = open_without_emulator(folder(&src)).unwrap();
            let mut got = paths(&*bare);
            got.sort();
            assert_eq!(
                got,
                vec![
                    "data/table.bin",
                    "eboot.bin",
                    "fakelib/libSceAgc.sprx",
                    "sce_sys/param.json"
                ],
                "{name}: the emulator, its index, marker, journal and log go; the backport stays"
            );
            assert_eq!(bare.read("data/table.bin").unwrap(), b"asset bytes");
            assert_eq!(
                bare.read("fakelib/libSceAgc.sprx").unwrap(),
                b"a newer system library"
            );
        }
    }

    #[test]
    fn a_libsceampr_that_is_not_an_emulator_stays() {
        let scratch = Scratch::new("sony");
        let src = game(&scratch.0, true, Some(b"\x7fELF a system library"));
        let found = assess(&mut *folder(&src)).unwrap().unwrap();
        assert_eq!(found.refusal, Some(Refusal::NotAnEmulator));
        assert_eq!(found.detail.as_deref(), Some("fakelib/libSceAmpr.sprx"));
        let err = open_without_emulator(folder(&src)).err().unwrap();
        assert!(err.starts_with(CANT_LEAVE_OUT), "{err}");
        assert!(err.contains("backport"), "{err}");
    }

    #[test]
    fn an_index_that_lists_files_the_game_lacks_keeps_the_emulator() {
        let scratch = Scratch::new("missing");
        let src = game(&scratch.0, true, Some(&ampr_emu("0.4.2.1")));
        // Files that were inside packs: listed, not on disk.
        let tree = FolderSource::open(&src).unwrap();
        let mut listed = ampr_index::listing(tree.files());
        listed.retain(|(p, _)| p != ampr_index::NAME);
        listed.push(("data/packed_only.bin".to_string(), 9));
        std::fs::write(
            src.join(ampr_index::NAME),
            ampr_index::build(&listed, 5).unwrap(),
        )
        .unwrap();
        let found = assess(&mut *folder(&src)).unwrap().unwrap();
        assert_eq!(found.refusal, Some(Refusal::IndexListsMissing));
        assert!(
            found
                .detail
                .as_deref()
                .unwrap()
                .contains("data/packed_only.bin"),
            "{found:?}"
        );
        assert!(open_without_emulator(folder(&src))
            .err()
            .unwrap()
            .starts_with(CANT_LEAVE_OUT));
    }

    #[test]
    fn pack_files_at_the_root_keep_the_emulator() {
        let scratch = Scratch::new("pak");
        let src = game(&scratch.0, true, Some(&ampr_emu("0.4.2.1")));
        std::fs::write(src.join("ampr_assets-000.pak"), b"volume").unwrap();
        reindex(&src);
        let found = assess(&mut *folder(&src)).unwrap().unwrap();
        assert_eq!(found.refusal, Some(Refusal::PackFiles));
        assert!(open_without_emulator(folder(&src)).is_err());
    }

    #[test]
    fn a_game_with_no_emulator_has_nothing_to_leave_out() {
        let scratch = Scratch::new("none");
        let src = game(&scratch.0, true, None);
        assert_eq!(assess(&mut *folder(&src)).unwrap(), None);
        let err = open_without_emulator(folder(&src)).err().unwrap();
        assert!(err.contains("it has none"), "{err}");
    }

    #[test]
    fn the_answer_serializes_as_the_apps_read_it() {
        let found = PlainEmulator {
            modules: vec!["fakelib/libSceAmpr.sprx".into()],
            emulator: "ampr_emu 0.4.2.1".into(),
            imports_ampr: true,
            refusal: Some(Refusal::IndexListsMissing),
            detail: Some("x".into()),
        };
        let v = serde_json::to_value(&found).unwrap();
        assert_eq!(v["refusal"], "index_lists_missing");
        assert_eq!(v["imports_ampr"], true);
        let offered = PlainEmulator {
            refusal: None,
            detail: None,
            ..found
        };
        let v = serde_json::to_value(&offered).unwrap();
        assert!(v.get("refusal").is_none());
    }
}
