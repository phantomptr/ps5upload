//! A game in asset packs (LZ4 or zstd) read as the plain game again: every packed file under
//! its own name, the pack volumes and their indexes left out. What "Unpack the asset packs"
//! writes as a folder or an image, with no copy on disk in between.
//!
//! What happens to the emulator and to `ampr_emu.index` follows quer3q/ps5-dump-forge v0.5.4's
//! `--zstd-unpack` / `--lz4-unpack` (`ps5-dump-forge-core/src/lz4.rs`, `open`, `prepare` and
//! `Runtime::action`, GPL-3.0-or-later):
//!
//! - a game that reads through libSceAmpr keeps an ampr_emu as new as the one this app installs;
//!   ampr-emu-ng (it came with the zstd packs and has nothing left to read), an older ampr_emu
//!   or a build this app can't name is replaced by the emulator Convert's settings choose, and
//!   a game with none gets it;
//! - a game that doesn't read through libSceAmpr keeps whatever it has;
//! - `ampr_commands.bin` and the emulator's logs are left out (the journal's file ids follow
//!   the old index's order), and a fresh `ampr_emu.index` lists every file written.

use std::collections::HashMap;

use ps5upload_fpkg::ampr_index;
use ps5upload_fpkg::ampr_pack::image::{emulator_home, Lz4Unpacked, Unpacking, RUNTIME_VERSION};
use ps5upload_fpkg::ampr_pack::ng;
use ps5upload_fpkg::source::{SourceFile, SourceTree};

/// What a run of the game wrote: not part of the game, and stale once the index is new.
const LEFT_OUT: [&str; 3] = ["ampr_commands.bin", "ampr_emu.log", "apr_emu.log"];

/// The emulator an unpacked game gets when its own has to go: the one Convert's settings
/// choose, fetched when it is needed.
pub(crate) struct Emulator<'a> {
    /// It is ampr-emu-ng: a game that already has ampr-emu-ng then keeps it.
    pub ng: bool,
    pub fetch: &'a dyn Fn() -> Result<Vec<u8>, String>,
}

/// What an unpack does with the game's `libSceAmpr.sprx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeAction {
    /// Not a libSceAmpr game: whatever it has stays.
    Untouched,
    /// It has an emulator that runs the plain game.
    Keep,
    /// It has none: the chosen one goes in.
    Install,
    /// The chosen one takes its place.
    Replace,
}

/// The action for a game that reads through libSceAmpr (`ampr`) or not, by the emulator it has
/// (`current`: [`ng::classify`]'s kind and version, `None` when it has none) and whether the
/// chosen replacement is ampr-emu-ng (`ng_chosen`). ps5-dump-forge's `Runtime::action(ampr,
/// false)`; the one difference is that ampr-emu-ng stays when it is also what would go in.
fn runtime_action(
    ampr: bool,
    current: Option<(&str, Option<&str>)>,
    ng_chosen: bool,
) -> RuntimeAction {
    if !ampr {
        return RuntimeAction::Untouched;
    }
    match current {
        None => RuntimeAction::Install,
        Some(("ampr-emu-ng", _)) if ng_chosen => RuntimeAction::Keep,
        Some(("ampr_emu", Some(v))) if !ng::older(v, RUNTIME_VERSION) => RuntimeAction::Keep,
        Some(_) => RuntimeAction::Replace,
    }
}

/// The unpacked game: the decoded tree with the emulator and index this module decided on.
struct Plain {
    inner: Box<dyn SourceTree>,
    files: Vec<SourceFile>,
    generated: HashMap<String, Vec<u8>>,
}

impl SourceTree for Plain {
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
        match self.generated.get(path) {
            Some(bytes) => Ok(bytes.clone()),
            None => self.inner.read(path),
        }
    }
    fn read_range(
        &mut self,
        path: &str,
        offset: u64,
        len: usize,
    ) -> ps5upload_fpkg::Result<Vec<u8>> {
        match self.generated.get(path) {
            Some(bytes) => {
                let start = usize::try_from(offset)
                    .unwrap_or(usize::MAX)
                    .min(bytes.len());
                let end = start.saturating_add(len).min(bytes.len());
                Ok(bytes[start..end].to_vec())
            }
            None => self.inner.read_range(path, offset, len),
        }
    }
    fn describe(&self) -> String {
        format!("{} (asset packs unpacked)", self.inner.describe())
    }
}

/// `tree` with its asset packs decoded, or why it has none to decode. `emulator` is what
/// replaces an emulator that has to go; `None` (Convert's "Add the AMPR emulator" is off)
/// leaves the game's own where it is. Every record of the new `ampr_emu.index` gets `mtime`: a
/// read-back that opens the source again passes the same one and gets the same bytes.
pub(crate) fn open(
    tree: Box<dyn SourceTree>,
    emulator: Option<&Emulator>,
    mtime: i64,
) -> Result<Box<dyn SourceTree>, String> {
    let mut kinds = Vec::new();
    let mut tree = match Lz4Unpacked::open(tree).map_err(|e| format!("the LZ4 packs: {e}"))? {
        Unpacking::Decoded(decoded) => {
            kinds.push("LZ4");
            decoded as Box<dyn SourceTree>
        }
        Unpacking::NotPacked(plain) => plain,
    };
    if ps5_dump_forge_zstd::reader::detect(&*tree) {
        tree = Box::new(
            ps5_dump_forge_zstd::reader::unpack(tree)
                .map_err(|e| format!("the zstd packs: {e}"))?,
        );
        kinds.push("zstd");
    }
    if kinds.is_empty() {
        return Err(
            "this game is not in asset packs (it has no ampr_assets.index or ampr_zstd.index \
             pack set): there is nothing to unpack"
                .to_string(),
        );
    }
    let ampr = ps5upload_fpkg::source::imports_ampr(&mut *tree, "eboot.bin");
    let module = ng::module_path(&*tree);
    let current = match &module {
        Some(path) => Some(
            ps5upload_fpkg::source::read_small(&mut *tree, path, 64 << 20)
                .map(|bytes| ng::classify(&bytes))
                .map_err(|e| format!("{path}: {e}"))?,
        ),
        None => None,
    };
    let named = |c: &(&'static str, Option<String>, bool)| match &c.1 {
        Some(v) => format!("{} {v}", c.0),
        None => "an emulator this app doesn't know".to_string(),
    };
    let mut generated: HashMap<String, Vec<u8>> = HashMap::new();
    let mut replaced: Option<String> = None;
    let action = match emulator {
        Some(e) => runtime_action(ampr, current.as_ref().map(|c| (c.0, c.1.as_deref())), e.ng),
        // The user turned emulators off: the game's own stays, whatever it is.
        None => RuntimeAction::Untouched,
    };
    match (action, emulator) {
        (RuntimeAction::Install, Some(e)) => {
            let bytes = (e.fetch)().map_err(|why| {
                format!(
                    "this game reads its files through AMPR and has no emulator, and one could \
                     not be fetched: {why}. Connect to the internet, or turn off \"Add the AMPR \
                     emulator\" in Convert's options"
                )
            })?;
            let at = emulator_home(&*tree);
            crate::log_at!(
                Info,
                "convert",
                "unpack packs: the game has no emulator; one goes in as {at}"
            );
            generated.insert(at.clone(), bytes);
            replaced = Some(at);
        }
        (RuntimeAction::Replace, Some(e)) => {
            let at = module.clone().unwrap_or_else(|| emulator_home(&*tree));
            let was = current.as_ref().map(named).unwrap_or_default();
            match (e.fetch)() {
                Ok(bytes) => {
                    crate::log_at!(
                        Info,
                        "convert",
                        "unpack packs: {was} at {at} is replaced by {}",
                        if e.ng { "ampr-emu-ng" } else { "ampr_emu" }
                    );
                    generated.insert(at.clone(), bytes);
                    replaced = Some(at);
                }
                // The game ran with the one it has: better kept than no game at all.
                Err(why) => crate::log_at!(
                    Warn,
                    "convert",
                    "unpack packs: {was} at {at} stays, the emulator to replace it with could \
                     not be fetched: {why}"
                ),
            }
        }
        _ => {
            if let (Some(path), Some(c)) = (&module, &current) {
                crate::log_at!(
                    Info,
                    "convert",
                    "unpack packs: {} at {path} stays",
                    named(c)
                );
            }
        }
    }
    let mut left_out = Vec::new();
    let mut files: Vec<SourceFile> = Vec::with_capacity(tree.files().len() + 2);
    for f in tree.files() {
        if LEFT_OUT.iter().any(|n| f.path.eq_ignore_ascii_case(n)) {
            left_out.push(f.path.clone());
        } else if !f.path.eq_ignore_ascii_case(ampr_index::NAME)
            && !replaced
                .as_deref()
                .is_some_and(|m| f.path.eq_ignore_ascii_case(m))
        {
            files.push(f.clone());
        }
    }
    if let Some(at) = &replaced {
        files.push(SourceFile {
            path: at.clone(),
            size: generated.get(at).map_or(0, |b| b.len() as u64),
        });
    }
    let index = ampr_index::build(&ampr_index::listing(&files), mtime)?;
    files.push(SourceFile {
        path: ampr_index::NAME.to_string(),
        size: index.len() as u64,
    });
    generated.insert(ampr_index::NAME.to_string(), index);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    crate::log_at!(
        Info,
        "convert",
        "unpack packs: {} packs decoded, {} plain files, a new {} lists them{}",
        kinds.join(" and "),
        files.len(),
        ampr_index::NAME,
        if left_out.is_empty() {
            String::new()
        } else {
            format!("; left out {}", left_out.join(", "))
        }
    );
    Ok(Box::new(Plain {
        inner: tree,
        files,
        generated,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zstd_runtime::PackRuntime;
    use ps5upload_fpkg::sample::VerifyMode;
    use ps5upload_fpkg::source::FolderSource;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;

    /// drakmor's ampr_emu as [`ng::classify`] knows it, at `version`.
    fn ampr_emu(version: &str) -> Vec<u8> {
        let mut m = format!("\0\0ampr_emu {version} (c) Drakmor\0").into_bytes();
        m.extend_from_slice(&[0u8; 32]);
        m
    }

    #[test]
    fn the_emulator_follows_forges_unpack_rule() {
        use RuntimeAction::*;
        let ng = Some(("ampr-emu-ng", Some("0.4.1")));
        // Not a libSceAmpr game: untouched, whatever it has.
        assert_eq!(runtime_action(false, ng, false), Untouched);
        assert_eq!(runtime_action(false, None, false), Untouched);
        assert_eq!(runtime_action(true, None, false), Install);
        // ampr-emu-ng has nothing left to read; it stays only when it is what would go in.
        assert_eq!(runtime_action(true, ng, false), Replace);
        assert_eq!(runtime_action(true, ng, true), Keep);
        assert_eq!(
            runtime_action(true, Some(("ampr_emu", Some("0.4.2.1"))), false),
            Keep
        );
        assert_eq!(
            runtime_action(true, Some(("ampr_emu", Some("0.5"))), false),
            Keep
        );
        assert_eq!(
            runtime_action(true, Some(("ampr_emu", Some("0.3.9"))), false),
            Replace
        );
        assert_eq!(
            runtime_action(true, Some(("unknown", None)), false),
            Replace
        );
    }

    fn game(root: &Path, ampr: bool) -> PathBuf {
        let src = root.join("PPSA99996-app");
        std::fs::create_dir_all(src.join("sce_sys")).unwrap();
        std::fs::create_dir_all(src.join("data/sub")).unwrap();
        let mut eboot = vec![0u8; 200_000];
        if ampr {
            eboot[1000..1014].copy_from_slice(b"libSceAmpr.prx");
        }
        std::fs::write(src.join("eboot.bin"), eboot).unwrap();
        std::fs::write(
            src.join("sce_sys/param.json"),
            b"{\"titleId\":\"PPSA99996\"}",
        )
        .unwrap();
        let text: Vec<u8> = (0..1_000_000u32)
            .flat_map(|i| format!("asset {} ", i % 89).into_bytes())
            .take(700_000)
            .collect();
        std::fs::write(src.join("data/sub/table.bin"), &text).unwrap();
        std::fs::write(src.join("data/settings.json"), b"{\"a\":1}").unwrap();
        std::fs::write(src.join("data/empty.bin"), b"").unwrap();
        src
    }

    fn ng_runtime() -> PackRuntime {
        PackRuntime {
            name: "ampr-emu-ng",
            version: "9.9.9".into(),
            source: "pinned",
            fallback_reason: None,
            bytes: b"NG EMULATOR".to_vec(),
        }
    }

    /// `src` as a folder in zstd packs at `to`, with a journal and a log as a run leaves them.
    fn zstd_packed(src: &Path, to: &Path) {
        let open = || -> Result<Box<dyn SourceTree>, String> {
            Ok(Box::new(
                FolderSource::open(src).map_err(|e| e.to_string())?,
            ))
        };
        let mut tree = open().unwrap();
        crate::zstd_packs::build_folder(
            5,
            None,
            &ng_runtime(),
            &mut *tree,
            &open,
            to,
            VerifyMode::Full,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        std::fs::write(to.join("ampr_commands.bin"), b"journal").unwrap();
        std::fs::write(to.join("ampr_emu.log"), b"log").unwrap();
    }

    fn temp(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("ps5upload-unpack-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// The game's own files as the unpacked tree must give them back.
    fn assert_plain(dir: &Path, src: &Path) {
        for name in [
            "eboot.bin",
            "sce_sys/param.json",
            "data/sub/table.bin",
            "data/settings.json",
            "data/empty.bin",
        ] {
            assert_eq!(
                std::fs::read(dir.join(name)).unwrap(),
                std::fs::read(src.join(name)).unwrap(),
                "{name}"
            );
        }
        let names: Vec<String> = FolderSource::open(dir)
            .unwrap()
            .files()
            .iter()
            .map(|f| f.path.clone())
            .collect();
        for gone in [
            "ampr_zstd.index",
            "ampr_zstd-000.pak",
            "ampr_commands.bin",
            "ampr_emu.log",
        ] {
            assert!(!names.iter().any(|n| n == gone), "{gone} in {names:?}");
        }
        // The new index lists exactly the files written, at their sizes.
        let listed =
            ampr_index::parse(&std::fs::read(dir.join(ampr_index::NAME)).unwrap()).unwrap();
        let mut want: Vec<(String, u64)> = FolderSource::open(dir)
            .unwrap()
            .files()
            .iter()
            .filter(|f| f.path != ampr_index::NAME)
            .map(|f| (f.path.clone(), f.size))
            .collect();
        let mut got: Vec<(String, u64)> = listed.into_iter().map(|e| (e.path, e.size)).collect();
        want.sort();
        got.sort();
        assert_eq!(got, want);
    }

    /// A zstd-packed folder unpacked into a folder, through the folder build and its read-back:
    /// the plain game again, ampr-emu-ng replaced by the chosen ampr_emu, a new index, and no
    /// packs, journal or log.
    #[test]
    fn a_zstd_packed_folder_unpacks_into_the_plain_game() {
        let root = temp("zstd");
        let src = game(&root, true);
        let packed = root.join("packed");
        zstd_packed(&src, &packed);
        assert!(packed.join("ampr_zstd-000.pak").is_file());
        assert!(!packed.join("data/sub/table.bin").exists());

        let emu = ampr_emu("0.4.2.1");
        let fetch = || Ok(emu.clone());
        let emulator = Emulator {
            ng: false,
            fetch: &fetch,
        };
        let open_plain = || -> Result<Box<dyn SourceTree>, String> {
            let tree = FolderSource::open(&packed).map_err(|e| e.to_string())?;
            open(Box::new(tree), Some(&emulator), 7)
        };
        let out = root.join("plain");
        let mut tree = open_plain().unwrap();
        assert!(tree.describe().contains("asset packs unpacked"));
        crate::folder_build::build(
            &mut *tree,
            &open_plain,
            &out,
            VerifyMode::Full,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_plain(&out, &src);
        assert_eq!(
            std::fs::read(out.join("fakelib/libSceAmpr.sprx")).unwrap(),
            emu
        );
        // Unpacked, it is not a packed game any more, and there is nothing left to unpack.
        let again = FolderSource::open(&out).unwrap();
        assert!(!ps5_dump_forge_zstd::reader::detect(&again));
        let err = open(Box::new(again), Some(&emulator), 7).err().unwrap();
        assert!(err.contains("nothing to unpack"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The same into an image, read back as any image is: every file as the game had it.
    #[test]
    fn a_zstd_packed_folder_unpacks_into_a_plain_image() {
        use crate::image_build::ImageFormat;
        let root = temp("zstd-image");
        let src = game(&root, true);
        let packed = root.join("packed");
        zstd_packed(&src, &packed);
        let emu = ampr_emu("0.4.2.1");
        let fetch = || Ok(emu.clone());
        let emulator = Emulator {
            ng: false,
            fetch: &fetch,
        };
        for format in [ImageFormat::Exfat, ImageFormat::Ffpkg] {
            let out = root.join(crate::image_build::output_names(&src, format, false).0);
            let folder = FolderSource::open(&packed).unwrap();
            let mut tree = open(Box::new(folder), Some(&emulator), 7).unwrap();
            crate::image_build::build(
                format,
                None,
                &mut *tree,
                &out,
                VerifyMode::Full,
                crate::image_build::DEFAULT_VERIFY_CHUNK,
                &AtomicBool::new(false),
                &mut |_, _, _| {},
            )
            .unwrap_or_else(|e| panic!("{format:?}: {e}"));
            let mut image = ps5upload_fpkg::source::open(&out).unwrap();
            assert!(!ps5_dump_forge_zstd::reader::detect(&*image), "{format:?}");
            for name in ["data/sub/table.bin", "data/settings.json", "eboot.bin"] {
                assert_eq!(
                    image.read(name).unwrap(),
                    std::fs::read(src.join(name)).unwrap(),
                    "{name} in {format:?}"
                );
            }
            assert_eq!(image.read("fakelib/libSceAmpr.sprx").unwrap(), emu);
            assert!(image.read("ampr_commands.bin").is_err(), "{format:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The emulator's fate: kept with emulators turned off, kept when ampr-emu-ng is also the
    /// choice, kept (with a warning) when its replacement can't be fetched.
    #[test]
    fn the_games_emulator_stays_when_it_should() {
        let root = temp("keep");
        let src = game(&root, true);
        let packed = root.join("packed");
        zstd_packed(&src, &packed);
        let read_module = |emulator: Option<&Emulator>| {
            let folder = FolderSource::open(&packed).unwrap();
            let mut tree = open(Box::new(folder), emulator, 7).unwrap();
            tree.read("fakelib/libSceAmpr.sprx").unwrap()
        };
        assert_eq!(read_module(None), b"NG EMULATOR");
        // The test runtime is not a release this app knows by digest: it counts as unknown,
        // and unknown is replaced, so the ampr-emu-ng choice is told by a failing fetch.
        let failing = || Err("offline".to_string());
        assert_eq!(
            read_module(Some(&Emulator {
                ng: false,
                fetch: &failing
            })),
            b"NG EMULATOR"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Not unpacked, a zstd-packed game goes into an image or folder as it is: its
    /// `ampr_emu.index`, which lists the files inside the packs, is not made again from the
    /// files on disk (that one would leave them out).
    #[test]
    fn a_packed_game_written_as_it_is_keeps_its_index() {
        let root = temp("asis");
        let src = game(&root, true);
        let packed = root.join("packed");
        zstd_packed(&src, &packed);
        let mut tree = FolderSource::open(&packed).unwrap();
        assert!(crate::image_build::AmprIndexed::wrap_at(&mut tree, 9)
            .unwrap()
            .is_none());
        let listed =
            ampr_index::parse(&std::fs::read(packed.join(ampr_index::NAME)).unwrap()).unwrap();
        assert!(listed.iter().any(|e| e.path == "data/sub/table.bin"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A game that doesn't read through libSceAmpr keeps the emulator it was packed with.
    #[test]
    fn a_game_without_ampr_keeps_its_emulator() {
        let root = temp("noampr");
        let src = game(&root, false);
        let packed = root.join("packed");
        zstd_packed(&src, &packed);
        let fetch = || -> Result<Vec<u8>, String> { panic!("nothing is fetched for it") };
        let folder = FolderSource::open(&packed).unwrap();
        let mut tree = open(
            Box::new(folder),
            Some(&Emulator {
                ng: false,
                fetch: &fetch,
            }),
            7,
        )
        .unwrap();
        assert_eq!(
            tree.read("fakelib/libSceAmpr.sprx").unwrap(),
            b"NG EMULATOR"
        );
        assert_eq!(
            tree.read("data/sub/table.bin").unwrap(),
            std::fs::read(src.join("data/sub/table.bin")).unwrap()
        );
        assert!(tree.files().iter().any(|f| f.path == ampr_index::NAME));
        let _ = std::fs::remove_dir_all(&root);
    }
}
