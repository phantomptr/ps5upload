//! What a game's `fakelib/` holds, and leaving its backport out of a conversion (Convert's
//! "Remove backport libraries").
//!
//! Ported from ps5-dump-forge's `crates/ps5-dump-forge-core/src/backport.rs` and the
//! `leave_out_backport` step of its `convert.rs` (GPLv3, as this is).
//!
//! ShadowMountPlus mounts `fakelib/` (or `fakelib2/`) over the game's system libraries. Most
//! dumps carry homebrew emulators there, not a backport: each one names its project, in the
//! file it reads from the game root (`ampr_emu.index`, `dlc_emu.ini`, `playgo_stub.dat`) or its
//! build path. The rest are system libraries from newer firmware: the backport proper. Leaving
//! it out is only safe when `eboot.bin`'s SDK was left alone; a backport that lowered it cannot
//! be undone here, the original values are gone. Nothing is restored: a game whose executables
//! were changed is refused, with the reason.

use std::collections::HashSet;

use ps5upload_fpkg::source::{SourceFile, SourceTree};
use serde::Serialize;

/// An emulator's marker (its project's name, in its files and build path), and its name.
const EMULATORS: &[(&[u8], &str)] = &[
    (b"ampr_emu", "AMPR"),
    (b"ampr-emu-ng", "AMPR"),
    (b"dlc_emu", "DLC"),
    (b"playgo_stub", "PlayGo"),
    (b"PlayGo PRX stub", "PlayGo"),
];

/// Libraries are a few hundred KiB; the marker is searched in this much of one.
const MAX_SCAN: u64 = 64 * 1024 * 1024;

/// A file under a top-level `fakelib/` or `fakelib2/`, any case, as ShadowMountPlus sees them.
fn is_fakelib(path: &str) -> bool {
    path.split_once('/').is_some_and(|(top, _)| {
        top.eq_ignore_ascii_case("fakelib") || top.eq_ignore_ascii_case("fakelib2")
    })
}

/// A homebrew emulator in a game's fakelib, which a removal keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Emulator {
    pub(crate) path: String,
    /// `AMPR`, `DLC`, `PlayGo`, or `Other` for one that could not be told.
    pub(crate) name: String,
}

/// A `fakelib`/`fakelib2`'s files: backport libraries, and the emulators kept beside them.
#[derive(Debug, Default)]
pub(crate) struct Fakelib {
    pub(crate) libs: Vec<String>,
    pub(crate) emulators: Vec<Emulator>,
}

/// The emulator `path` is, by its marker; `None` for a backport library. Fails closed: a file
/// that can't be read whole counts as an emulator (`Other`), so it is kept. `/app0/` is the
/// homebrew signal (emulators read their own files from the game root); Sony system libraries
/// are expected not to carry it, and one that does is kept, the safe side.
fn emulator(tree: &mut dyn SourceTree, path: &str, size: u64) -> Option<&'static str> {
    const OTHER: Option<&str> = Some("Other");
    let Some(len) = usize::try_from(size).ok().filter(|_| size <= MAX_SCAN) else {
        return OTHER;
    };
    let Ok(bytes) = tree.read_range(path, 0, len) else {
        return OTHER;
    };
    let has = |marker: &[u8]| bytes.windows(marker.len()).any(|w| w == marker);
    EMULATORS
        .iter()
        .find(|(marker, _)| has(marker))
        .map(|(_, name)| *name)
        .or_else(|| has(b"/app0/").then_some("Other"))
}

/// A module the system loads: `.sprx` or `.prx`, any case.
fn is_library(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("sprx") || ext.eq_ignore_ascii_case("prx"))
}

/// What `tree`'s fakelib folders hold. A file that is not a library (a `.orig` kept by an
/// emulator install, a config, a note) is neither: it stays where it is.
pub(crate) fn classify(tree: &mut dyn SourceTree) -> Fakelib {
    let files: Vec<SourceFile> = tree
        .files()
        .iter()
        .filter(|f| is_fakelib(&f.path) && is_library(&f.path))
        .cloned()
        .collect();
    let mut found = Fakelib::default();
    for f in files {
        match emulator(tree, &f.path, f.size) {
            Some(name) => found.emulators.push(Emulator {
                path: f.path,
                name: name.to_string(),
            }),
            None => found.libs.push(f.path),
        }
    }
    found
}

/// `0x07000038` as `7.00`, `0x10500040` as `10.50`: the top two bytes are BCD.
fn firmware(sdk: u32) -> Option<String> {
    let [major, minor, _, _] = sdk.to_be_bytes();
    let bcd = |b: u8| b >> 4 <= 9 && b & 0xF <= 9;
    (bcd(major) && bcd(minor) && sdk != 0).then(|| format!("{major:x}.{minor:02x}"))
}

/// `0x0900000000000000` (or the 32-bit `0x09000000`) as `0x09000000`: the word an executable's
/// SDK compares with (BCD major.minor, then the build).
fn sdk_word(word: &str) -> Option<u32> {
    let hex = word
        .strip_prefix("0x")
        .or_else(|| word.strip_prefix("0X"))?;
    let width = if hex.len() <= 8 { 8 } else { 16 };
    let hex = format!("{hex:0>width$}");
    let digits = hex.bytes().all(|b| b.is_ascii_hexdigit());
    if word.len() == 2
        || hex.len() != width
        || !digits
        || !hex[..4].bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    u32::from_str_radix(&hex[..8], 16).ok()
}

/// The game's `sce_sys/param.json`, when it has a readable one.
fn param_json(tree: &mut dyn SourceTree) -> Option<serde_json::Value> {
    let path = tree
        .files()
        .iter()
        .find(|f| f.path.eq_ignore_ascii_case("sce_sys/param.json"))
        .map(|f| f.path.clone())?;
    serde_json::from_slice(&tree.read(&path).ok()?).ok()
}

/// Why the backport can't be removed: `eboot.bin`'s SDK is lower than `param.json`'s
/// `sdkVersion` (the whole SDK word), or either is unreadable. `None`: removable. `eboot.bin`
/// alone: a backport patches it first, untouched modules can't hide that, and other files
/// (`sce_sys/about/right.sprx` is SDK 1.00) legitimately sit lower.
pub(crate) fn blocked(tree: &mut dyn SourceTree) -> Option<String> {
    let param = param_json(tree);
    let declared = param
        .as_ref()
        .and_then(|p| p.get("sdkVersion"))
        .and_then(serde_json::Value::as_str);
    let Some(want) = declared.and_then(sdk_word) else {
        return Some(
            "param.json has no readable sdkVersion, so whether the backport lowered the \
             executables' SDK can't be told"
                .to_string(),
        );
    };
    let eboot = tree
        .files()
        .iter()
        .find(|f| f.path.eq_ignore_ascii_case("eboot.bin"))
        .map(|f| f.path.clone());
    let sdk = eboot
        .and_then(|p| crate::fpkg_firmware::raw_module_pair(tree, &p))
        .map(|(_, ps5)| ps5);
    let Some(have) = sdk.filter(|&w| firmware(w).is_some()) else {
        return Some(
            "eboot.bin has no readable SDK version, so whether the backport lowered it can't \
             be told"
                .to_string(),
        );
    };
    // BCD compares as numbers; the whole word, so a lower build of the same SDK counts.
    (have < want).then(|| {
        format!(
            "eboot.bin's SDK {} ({have:#010x}) is lower than param.json's sdkVersion {} \
             ({want:#010x}): the backport lowered it and the original can't be restored",
            firmware(have).unwrap_or_default(),
            firmware(want).unwrap_or_default(),
        )
    })
}

/// How the game's files are packed, when they are: the pack set's own list names the backport
/// libraries, so they cannot be left out while the packs are kept.
pub(crate) fn packed(tree: &mut dyn SourceTree) -> Option<&'static str> {
    if ps5upload_fpkg::source::lz4_packed(tree).unwrap_or(false) {
        Some("lz4")
    } else if ps5_dump_forge_zstd::reader::detect(tree) {
        Some("zstd")
    } else {
        None
    }
}

/// Why a game packed as `kind` keeps its backport, said to the user.
fn packed_refusal(kind: &str) -> String {
    match kind {
        "lz4" => "the game is in LZ4 asset packs, whose manifest lists the backport libraries: \
                  convert it with zstd packs (which unpacks it first) to leave the backport out"
            .to_string(),
        _ => "the game is in zstd asset packs, whose ampr_emu.index lists the backport \
              libraries: convert it with zstd packs again (which unpacks it first) to leave the \
              backport out"
            .to_string(),
    }
}

/// What the inspection says about a game's backport, for Convert's "Remove backport libraries".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct BackportReport {
    /// The backport libraries in `fakelib`/`fakelib2`, by path: what a removal leaves out.
    pub(crate) libraries: Vec<String>,
    /// The emulators beside them, which a removal keeps.
    pub(crate) emulators: Vec<Emulator>,
    /// Why they can't be left out whatever the conversion: the backport lowered eboot.bin's
    /// SDK (or that can't be told). Absent when they can.
    pub(crate) blocked: Option<String>,
    /// `lz4` or `zstd` when the game is in asset packs: its backport can be left out only by a
    /// conversion that unpacks it (one that writes zstd packs).
    pub(crate) packed: Option<&'static str>,
}

/// The game's backport, or `None` when its fakelib holds no backport library (nothing to offer).
pub(crate) fn report(tree: &mut dyn SourceTree) -> Option<BackportReport> {
    let found = classify(tree);
    if found.libs.is_empty() {
        return None;
    }
    Some(BackportReport {
        libraries: found.libs,
        emulators: found.emulators,
        blocked: blocked(tree),
        packed: packed(tree),
    })
}

/// The backport libraries a conversion of `tree` leaves out: empty when its fakelib holds
/// none (said in the log, and the conversion goes on), an error with the reason when they
/// cannot go. `tree` is the game as the conversion reads it, packs it unpacks already decoded.
pub(crate) fn removable(tree: &mut dyn SourceTree) -> Result<Vec<String>, String> {
    let found = classify(tree);
    let kept: Vec<&str> = found.emulators.iter().map(|e| e.name.as_str()).collect();
    if found.libs.is_empty() {
        crate::log_at!(
            Info,
            "convert",
            "remove backport: nothing to remove, no backport library in fakelib"
        );
        return Ok(Vec::new());
    }
    if let Some(kind) = packed(tree) {
        return Err(format!(
            "the backport can't be removed: {}",
            packed_refusal(kind)
        ));
    }
    if let Some(why) = blocked(tree) {
        return Err(format!("the backport can't be removed: {why}"));
    }
    for lib in &found.libs {
        crate::log_at!(Info, "convert", "remove backport: leaving out {lib}");
    }
    if !kept.is_empty() {
        crate::log_at!(
            Info,
            "convert",
            "remove backport: keeping emulators {}",
            kept.join(", ")
        );
    }
    Ok(found.libs)
}

/// `tree` without the files `removable` listed (as it is when there are none).
pub(crate) fn without(tree: Box<dyn SourceTree>, libs: &[String]) -> Box<dyn SourceTree> {
    if libs.is_empty() {
        tree
    } else {
        Box::new(Without::new(tree, libs))
    }
}

/// A source without some of its files. A top-level `fakelib`/`fakelib2` left with nothing in
/// it goes too, empty folders inside included: an empty overlay would hide the console's own
/// system libraries.
pub(crate) struct Without {
    inner: Box<dyn SourceTree>,
    files: Vec<SourceFile>,
    empty_dirs: Vec<String>,
}

impl Without {
    pub(crate) fn new(inner: Box<dyn SourceTree>, drop: &[String]) -> Self {
        let drop: HashSet<&str> = drop.iter().map(String::as_str).collect();
        let files: Vec<SourceFile> = inner
            .files()
            .iter()
            .filter(|f| !drop.contains(f.path.as_str()))
            .cloned()
            .collect();
        let top = |p: &str| p.split('/').next().unwrap_or(p).to_string();
        let busy: HashSet<String> = files.iter().map(|f| top(&f.path)).collect();
        let empty_dirs = inner
            .empty_dirs()
            .iter()
            .filter(|d| {
                let t = top(d);
                let fakelib =
                    t.eq_ignore_ascii_case("fakelib") || t.eq_ignore_ascii_case("fakelib2");
                !fakelib || busy.contains(&t)
            })
            .cloned()
            .collect();
        Self {
            inner,
            files,
            empty_dirs,
        }
    }
}

impl SourceTree for Without {
    fn release(&mut self) {
        self.inner.release();
    }

    fn files(&self) -> &[SourceFile] {
        &self.files
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

    fn empty_dirs(&self) -> &[String] {
        &self.empty_dirs
    }

    fn describe(&self) -> String {
        format!("{} without its backport libraries", self.inner.describe())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A raw ELF with one PT_SCE_PROCPARAM segment whose param block carries the PS5 SDK `sdk`.
    fn eboot(sdk: u32) -> Vec<u8> {
        let mut e = vec![0u8; 0x1000];
        e[0..4].copy_from_slice(b"\x7fELF");
        e[0x20..0x28].copy_from_slice(&0x40u64.to_le_bytes()); // e_phoff
        e[0x36..0x38].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        e[0x38..0x3A].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
        let ph = 0x40;
        e[ph..ph + 4].copy_from_slice(&0x6100_0001u32.to_le_bytes()); // PT_SCE_PROCPARAM
        e[ph + 8..ph + 16].copy_from_slice(&0x800u64.to_le_bytes()); // p_offset
        e[ph + 0x20..ph + 0x28].copy_from_slice(&0x40u64.to_le_bytes()); // p_filesz
        e[0x808..0x80C].copy_from_slice(&0x4942_524Fu32.to_le_bytes()); // "ORBI"
        e[0x810..0x814].copy_from_slice(&0x0904_0001u32.to_le_bytes());
        e[0x814..0x818].copy_from_slice(&sdk.to_le_bytes());
        e
    }

    struct Files {
        data: Vec<(String, Vec<u8>)>,
        listed: Vec<SourceFile>,
        empty: Vec<String>,
    }

    fn files(items: Vec<(&str, Vec<u8>)>) -> Files {
        Files {
            listed: items
                .iter()
                .map(|(p, d)| SourceFile {
                    path: p.to_string(),
                    size: d.len() as u64,
                })
                .collect(),
            data: items.into_iter().map(|(p, d)| (p.to_string(), d)).collect(),
            empty: Vec::new(),
        }
    }

    impl SourceTree for Files {
        fn files(&self) -> &[SourceFile] {
            &self.listed
        }
        fn read(&mut self, path: &str) -> ps5upload_fpkg::Result<Vec<u8>> {
            self.data
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, d)| d.clone())
                .ok_or_else(|| ps5upload_fpkg::Error::Format(format!("no {path}")))
        }
        fn empty_dirs(&self) -> &[String] {
            &self.empty
        }
        fn describe(&self) -> String {
            "test".into()
        }
    }

    const PARAM_12: &[u8] = br#"{"titleId":"PPSA01234","sdkVersion":"0x1200000000000000"}"#;

    /// A game declared for SDK 12.00 whose eboot carries `sdk`, with a two-library backport,
    /// an AMPR emulator and a second fakelib.
    fn game(sdk: u32) -> Files {
        files(vec![
            ("eboot.bin", eboot(sdk)),
            ("sce_sys/param.json", PARAM_12.to_vec()),
            (
                "fakelib/libSceAgc.sprx",
                b"\x7fELF a system library".to_vec(),
            ),
            (
                "fakelib/libSceAmpr.sprx",
                b"reads /app0/ampr_emu.index".to_vec(),
            ),
            ("fakelib2/libSceGnm.sprx", b"\x7fELF another".to_vec()),
            ("ampr_emu.index", b"index".to_vec()),
        ])
    }

    fn paths(tree: &dyn SourceTree) -> Vec<&str> {
        tree.files().iter().map(|f| f.path.as_str()).collect()
    }

    #[test]
    fn emulators_are_told_from_backport_libraries() {
        let mut t = files(vec![
            ("fakelib/libSceAgc.sprx", b"\x7fELF system".to_vec()),
            ("fakelib/libSceAmpr.sprx", b"..ampr_emu..".to_vec()),
            (
                "FAKELIB2/libSceGameUpdate.sprx",
                b"/build/dlc_emu/x".to_vec(),
            ),
            ("fakelib/libScePlayGo.sprx", b"PlayGo PRX stub".to_vec()),
            ("fakelib/libMystery.sprx", b"opens /app0/own.cfg".to_vec()),
            ("fakelib/libNg.sprx", b"ampr-emu-ng 0.4.0".to_vec()),
            (
                "fakelib/libSceOld.PRX",
                b"\x7fELF system, older suffix".to_vec(),
            ),
            // Not a library: a config, a note, an emulator install's `.orig`; kept, unlisted.
            ("fakelib/ampr_emu.ini", b"\x7fELF-free config".to_vec()),
            ("fakelib/README.txt", b"how to install".to_vec()),
            (
                "fakelib/libSceAmpr.sprx.orig",
                b"\x7fELF the game's own".to_vec(),
            ),
            ("fakelib2/notes", b"plain".to_vec()),
            // Not in a top-level fakelib: none of this module's business.
            ("data/fakelib/libSceAgc.sprx", b"\x7fELF".to_vec()),
            ("fakelibs/libSceAgc.sprx", b"\x7fELF".to_vec()),
        ]);
        let found = classify(&mut t);
        assert_eq!(
            found.libs,
            ["fakelib/libSceAgc.sprx", "fakelib/libSceOld.PRX"]
        );
        // Left out, the rest of the fakelib stays.
        let libs = found.libs.clone();
        let out = without(Box::new(t), &libs);
        for stays in [
            "fakelib/ampr_emu.ini",
            "fakelib/README.txt",
            "fakelib/libSceAmpr.sprx.orig",
            "fakelib2/notes",
        ] {
            assert!(paths(&*out).contains(&stays), "{stays}");
        }
        let names: Vec<(&str, &str)> = found
            .emulators
            .iter()
            .map(|e| (e.path.as_str(), e.name.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("fakelib/libSceAmpr.sprx", "AMPR"),
                ("FAKELIB2/libSceGameUpdate.sprx", "DLC"),
                ("fakelib/libScePlayGo.sprx", "PlayGo"),
                ("fakelib/libMystery.sprx", "Other"),
                ("fakelib/libNg.sprx", "AMPR"),
            ]
        );
    }

    /// A library that cannot be read is kept, never left out on a guess.
    #[test]
    fn an_unreadable_library_counts_as_an_emulator() {
        let mut t = files(vec![("fakelib/libSceAgc.sprx", b"\x7fELF".to_vec())]);
        t.data.clear();
        let found = classify(&mut t);
        assert!(found.libs.is_empty());
        assert_eq!(found.emulators[0].name, "Other");
    }

    #[test]
    fn a_lowered_sdk_blocks_removal() {
        assert_eq!(blocked(&mut game(0x1200_0038)), None);
        assert_eq!(blocked(&mut game(0x1250_0000)), None);
        let lowered = blocked(&mut game(0x0900_0040)).unwrap();
        assert!(
            lowered.contains("SDK 9.00 (0x09000040) is lower than param.json's sdkVersion 12.00"),
            "{lowered}"
        );
        // A lower build of the declared SDK counts too.
        let with_param = |sdk: u32, param: &[u8]| {
            files(vec![
                ("eboot.bin", eboot(sdk)),
                ("sce_sys/param.json", param.to_vec()),
            ])
        };
        let built = br#"{"sdkVersion":"0x0900004000000000"}"#;
        assert!(blocked(&mut with_param(0x0900_0038, built)).is_some());
        assert_eq!(blocked(&mut with_param(0x0900_0040, built)), None);
        // 32-bit words are read too.
        let short = br#"{"sdkVersion":"0x09000000"}"#;
        assert_eq!(blocked(&mut with_param(0x0900_0040, short)), None);
        // Unknown either way: refused.
        for bad in [
            &br#"{"sdkVersion":"12.00"}"#[..],
            br#"{"sdkVersion":"0x"}"#,
            br#"{"titleId":"PPSA01234"}"#,
            b"not json",
        ] {
            let why = blocked(&mut with_param(0x1200_0038, bad)).unwrap();
            assert!(why.contains("no readable sdkVersion"), "{why}");
        }
        let mut no_param = files(vec![("eboot.bin", eboot(0x1200_0038))]);
        assert!(blocked(&mut no_param)
            .unwrap()
            .contains("no readable sdkVersion"));
        let mut no_exe = files(vec![
            ("eboot.bin", vec![0; 16]),
            ("sce_sys/param.json", PARAM_12.to_vec()),
        ]);
        assert!(blocked(&mut no_exe).unwrap().contains("eboot.bin has no"));
        let mut no_eboot = files(vec![("sce_sys/param.json", PARAM_12.to_vec())]);
        assert!(blocked(&mut no_eboot).unwrap().contains("eboot.bin has no"));
    }

    #[test]
    fn removal_keeps_emulators_and_drops_an_emptied_fakelib() {
        let mut t = game(0x1200_0038);
        t.empty = vec![
            "fakelib2/leftover".into(),
            "fakelib/notes".into(),
            "data/empty".into(),
        ];
        let mut boxed: Box<dyn SourceTree> = Box::new(t);
        let libs = removable(&mut *boxed).unwrap();
        assert_eq!(libs, ["fakelib/libSceAgc.sprx", "fakelib2/libSceGnm.sprx"]);
        let mut out = without(boxed, &libs);
        assert_eq!(
            paths(&*out),
            [
                "eboot.bin",
                "sce_sys/param.json",
                "fakelib/libSceAmpr.sprx",
                "ampr_emu.index"
            ]
        );
        // fakelib2 has nothing left, so its empty folder goes with it; fakelib's stays.
        assert_eq!(out.empty_dirs(), ["fakelib/notes", "data/empty"]);
        assert_eq!(
            out.read("fakelib/libSceAmpr.sprx").unwrap(),
            b"reads /app0/ampr_emu.index"
        );
        assert!(out.describe().contains("without its backport"));
    }

    /// The whole way, as Convert does it: a game folder becomes an image without its backport,
    /// and that image a folder, each written and read back by the real builders. The folder
    /// build's read-back opens its source again, so it has to leave the same files out.
    #[test]
    fn an_image_and_a_folder_are_built_and_read_back_without_the_backport() {
        use crate::image_build::{self, ImageFormat};
        use ps5upload_fpkg::sample::VerifyMode;
        use std::sync::atomic::AtomicBool;

        let root = std::env::temp_dir().join(format!("ps5upload-rmbp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let game_dir = root.join("PPSA01234");
        let mut on_disk = game(0x1200_0038);
        for (path, bytes) in on_disk.data.drain(..) {
            let to = game_dir.join(&path);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::write(to, bytes).unwrap();
        }
        let open = |path: &std::path::Path| ps5upload_fpkg::source::open(path).unwrap();
        let listed = |path: &std::path::Path| -> Vec<String> {
            let mut all: Vec<String> = open(path).files().iter().map(|f| f.path.clone()).collect();
            all.sort();
            all
        };
        let want = [
            "ampr_emu.index",
            "eboot.bin",
            "fakelib/libSceAmpr.sprx",
            "sce_sys/param.json",
        ];

        // Folder -> image, the backport left out.
        let mut tree = open(&game_dir);
        let libs = removable(&mut *tree).unwrap();
        assert_eq!(libs.len(), 2);
        let mut tree = without(tree, &libs);
        let image = root.join("PPSA01234.exfat");
        image_build::build(
            ImageFormat::Exfat,
            None,
            &mut *tree,
            &image,
            VerifyMode::Full,
            8 << 20,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(listed(&image), want);
        // The game folder itself is as it was.
        assert!(game_dir.join("fakelib/libSceAgc.sprx").is_file());
        assert!(game_dir.join("fakelib2/libSceGnm.sprx").is_file());

        // Image (with the backport still in it) -> folder, the backport left out.
        let full = root.join("full.exfat");
        let mut tree = open(&game_dir);
        image_build::build(
            ImageFormat::Exfat,
            None,
            &mut *tree,
            &full,
            VerifyMode::Full,
            8 << 20,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        let mut tree = open(&full);
        let libs = removable(&mut *tree).unwrap();
        let mut tree = without(tree, &libs);
        let folder = root.join("out/PPSA01234");
        std::fs::create_dir_all(folder.parent().unwrap()).unwrap();
        let reopen = || Ok(without(open(&full), &libs));
        crate::folder_build::build(
            &mut *tree,
            &reopen,
            &folder,
            VerifyMode::Full,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(listed(&folder), want);
        assert!(
            !folder.join("fakelib2").exists(),
            "an emptied fakelib2 goes"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nothing_to_remove_leaves_the_game_as_it_is() {
        let mut t: Box<dyn SourceTree> = Box::new(files(vec![
            ("eboot.bin", eboot(0x0400_0031)),
            ("fakelib/libSceAmpr.sprx", b"ampr_emu".to_vec()),
        ]));
        // No backport library: not refused, even though this eboot's SDK could not be judged.
        assert_eq!(removable(&mut *t).unwrap(), Vec::<String>::new());
        assert_eq!(report(&mut *t), None);
        let t = without(t, &[]);
        assert_eq!(t.describe(), "test");
    }

    #[test]
    fn a_lowered_sdk_or_a_packed_game_is_refused_with_the_reason() {
        let err = removable(&mut game(0x0900_0040)).unwrap_err();
        assert!(
            err.starts_with("the backport can't be removed: eboot.bin's SDK 9.00"),
            "{err}"
        );
        let r = report(&mut game(0x0900_0040)).unwrap();
        assert_eq!(
            r.libraries,
            ["fakelib/libSceAgc.sprx", "fakelib2/libSceGnm.sprx"]
        );
        assert_eq!(r.emulators.len(), 1);
        assert!(r.blocked.unwrap().contains("lower than"));
        assert_eq!(r.packed, None);
        let r = report(&mut game(0x1200_0038)).unwrap();
        assert_eq!((r.blocked, r.packed), (None, None));

        assert!(packed_refusal("lz4").contains("LZ4 asset packs"));
        assert!(packed_refusal("zstd").contains("ampr_emu.index"));
    }
}
