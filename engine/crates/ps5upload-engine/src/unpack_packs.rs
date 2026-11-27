//! A game in asset packs (LZ4 or zstd) read as the plain game again: what "Unpack the asset
//! packs" writes as a folder, an image or a package, with no copy on disk in between.
//!
//! The code is `ps5upload_convert_core::unpack_packs`, shared with the converter that runs on
//! the PS5, and is re-exported here. What stays in the engine: fetching the emulator that takes
//! the place of one that has to go (`fpkg_api::open_unpacked`), and the tests below, which
//! unpack through the engine's own folder, image and package builds.

pub(crate) use ps5upload_convert_core::unpack_packs::*;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::zstd_runtime::PackRuntime;
    use ps5upload_fpkg::ampr_index;
    use ps5upload_fpkg::sample::VerifyMode;
    use ps5upload_fpkg::source::FolderSource;
    use ps5upload_fpkg::source::SourceTree;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;

    /// drakmor's ampr_emu as [`ng::classify`] knows it, at `version`.
    pub(crate) fn ampr_emu(version: &str) -> Vec<u8> {
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

    pub(crate) fn game(root: &Path, ampr: bool) -> PathBuf {
        let src = root.join("PPSA99996-app");
        std::fs::create_dir_all(src.join("sce_sys")).unwrap();
        std::fs::create_dir_all(src.join("data/sub")).unwrap();
        let mut eboot = vec![0u8; 200_000];
        if ampr {
            eboot[1000..1014].copy_from_slice(b"libSceAmpr.prx");
        }
        std::fs::write(src.join("eboot.bin"), eboot).unwrap();
        std::fs::write(src.join("sce_sys/param.json"), PARAM).unwrap();
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
    pub(crate) fn zstd_packed(src: &Path, to: &Path) {
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

    /// The fixture game's `param.json`: with a content id, as a package needs one.
    pub(crate) const PARAM: &[u8] = br#"{"contentId":"UP0000-PPSA99996_00-UNPACKTEST000000","contentVersion":"01.000.000","titleId":"PPSA99996","applicationCategoryType":0}"#;

    /// A scratch folder that goes when the test ends, passed or failed.
    pub(crate) struct Scratch(pub(crate) PathBuf);

    impl Scratch {
        pub(crate) fn new(name: &str) -> Self {
            Self(temp(name))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp(name: &str) -> PathBuf {
        let root =
            crate::test_tmp::temp_path(format!("ps5upload-unpack-{name}-{}", std::process::id()));
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

    /// A package of the game at `src` read through `reader`, at Kraken's fast level: the
    /// package, opened as the game it holds.
    fn package(
        src: &Path,
        out: &Path,
        reader: ps5upload_fpkg::build::SourceReader,
    ) -> Box<dyn SourceTree> {
        let built = package_report(src, out, Some(reader));
        ps5upload_fpkg::source::open(&built.path).unwrap()
    }

    /// [`package`]'s build report; without a `reader` the game goes in as it is, packs and all.
    fn package_report(
        src: &Path,
        out: &Path,
        reader: Option<ps5upload_fpkg::build::SourceReader>,
    ) -> ps5upload_fpkg::build::BuildReport {
        std::fs::create_dir_all(out).unwrap();
        let mut request = ps5upload_fpkg::build::BuildRequest::new(src, out);
        request.level = ps5upload_fpkg::kraken::Level::Fast;
        request.reader = reader;
        ps5upload_fpkg::build::build(&request, &mut |_| {})
            .unwrap_or_else(|e| panic!("{}: {e}", src.display()))
    }

    /// Unpacking as Convert does it, `emulator` replacing one that only suits packs (`None`:
    /// nothing may be fetched).
    fn unpacking(emulator: Option<Vec<u8>>) -> ps5upload_fpkg::build::SourceReader {
        ps5upload_fpkg::build::SourceReader(std::sync::Arc::new(move |tree| {
            let fetch = || match &emulator {
                Some(bytes) => Ok(bytes.clone()),
                None => panic!("the game's own emulator is kept: nothing is fetched"),
            };
            open(
                tree,
                Some(&Emulator {
                    ng: false,
                    fetch: &fetch,
                }),
                7,
            )
        }))
    }

    /// Unpacking with the AMPR emulator left out.
    fn unpacking_without_emulator() -> ps5upload_fpkg::build::SourceReader {
        ps5upload_fpkg::build::SourceReader(std::sync::Arc::new(|tree| {
            open_without_emulator(tree, 7)
        }))
    }

    fn names_of(tree: &dyn SourceTree) -> Vec<String> {
        tree.files().iter().map(|f| f.path.clone()).collect()
    }

    /// The game's own files in the package byte for byte, and none of the packs, their
    /// indexes, the run's journal or its log.
    fn assert_packaged_plain(pkg: &mut dyn SourceTree, src: &Path) {
        for name in [
            "eboot.bin",
            "data/sub/table.bin",
            "data/settings.json",
            "data/empty.bin",
        ] {
            assert_eq!(
                pkg.read(name).unwrap(),
                std::fs::read(src.join(name)).unwrap(),
                "{name}"
            );
        }
        let names = names_of(pkg);
        for n in &names {
            let lower = n.to_ascii_lowercase();
            assert!(
                !lower.starts_with("ampr_zstd") && !lower.starts_with("ampr_assets"),
                "{n} in {names:?}"
            );
            assert!(
                lower != "ampr_commands.bin" && lower != "ampr_emu.log",
                "{n} in {names:?}"
            );
        }
    }

    /// The package's `ampr_emu.index` lists exactly the files it carries, at their sizes.
    fn assert_index_lists_the_package(pkg: &mut dyn SourceTree) {
        let listed = ampr_index::parse(&pkg.read(ampr_index::NAME).unwrap()).unwrap();
        let mut got: Vec<(String, u64)> = listed.into_iter().map(|e| (e.path, e.size)).collect();
        let mut want: Vec<(String, u64)> = pkg
            .files()
            .iter()
            .filter(|f| f.path != ampr_index::NAME)
            .map(|f| (f.path.clone(), f.size))
            .collect();
        got.sort();
        want.sort();
        assert_eq!(got, want);
    }

    /// "Unpack the asset packs" for a package (#933): a zstd-packed libSceAmpr game packaged
    /// as its plain files, byte for byte, with ampr-emu-ng (it came with the packs) replaced
    /// by the chosen emulator and an index of what the package carries. With the emulator left
    /// out: no `libSceAmpr.sprx` (nor a kept `.orig`), nothing fetched, and the index kept.
    #[test]
    fn a_zstd_packed_game_packages_as_plain_files() {
        let scratch = Scratch::new("pkg-zstd");
        let root = &scratch.0;
        let src = game(root, true);
        let packed = root.join("packed");
        zstd_packed(&src, &packed);
        std::fs::write(packed.join("fakelib/libSceAmpr.sprx.orig"), b"OLD").unwrap();
        assert!(packed.join("ampr_zstd-000.pak").is_file());
        assert!(!packed.join("data/sub/table.bin").exists());

        let emu = ampr_emu("0.4.2.1");
        let mut kept = package(&packed, &root.join("kept"), unpacking(Some(emu.clone())));
        assert_packaged_plain(&mut *kept, &src);
        assert_eq!(kept.read("fakelib/libSceAmpr.sprx").unwrap(), emu);
        assert_index_lists_the_package(&mut *kept);

        let mut bare = package(&packed, &root.join("bare"), unpacking_without_emulator());
        assert_packaged_plain(&mut *bare, &src);
        let names = names_of(&*bare);
        assert!(!names.iter().any(|n| is_emulator_file(n)), "{names:?}");
        assert_index_lists_the_package(&mut *bare);
    }

    /// A zstd-packed game whose own `ampr_emu.index` no longer describes it (a loose file
    /// resized and another gone since the packs were made). The check of the source says so,
    /// and says where it matters: in what keeps the packs. A package with the packs unpacked
    /// carries a fresh index of its own files and reports no warning (the report that
    /// "7 listed missing, 10 sizes differ" was shown for a package that installed and
    /// played); a package that keeps the packs carries the stale index and still warns.
    #[test]
    fn a_stale_index_warns_only_where_it_ships() {
        const CHECK: &str = "ampr_emu.index for a libSceAmpr title";
        let scratch = Scratch::new("pkg-stale");
        let root = &scratch.0;
        let src = game(root, true);
        // A loose file the index will list and the game will then lose.
        std::fs::write(src.join("dlc_emu.ini"), b"gone later").unwrap();
        let packed = root.join("packed");
        zstd_packed(&src, &packed);
        // What a run left is not in the index either way; it would only add "unlisted".
        std::fs::remove_file(packed.join("ampr_commands.bin")).unwrap();
        std::fs::remove_file(packed.join("ampr_emu.log")).unwrap();
        let fresh = ps5upload_fpkg::build::inspect(&packed, root).unwrap();
        let before = fresh.checks.iter().find(|c| c.name == CHECK).unwrap();
        assert!(before.ok, "{}", before.detail);

        // The source goes stale: a loose data file grows, another goes. eboot.bin grows too,
        // as signing it does in any converted game: the system loads it, not ampr_emu, so its
        // record is no part of the finding.
        std::fs::write(packed.join("data/empty.bin"), b"grown").unwrap();
        let mut eboot = std::fs::read(packed.join("eboot.bin")).unwrap();
        eboot.extend_from_slice(&[0u8; 4096]);
        std::fs::write(packed.join("eboot.bin"), &eboot).unwrap();
        std::fs::remove_file(packed.join("dlc_emu.ini")).unwrap();
        let stale = std::fs::read(packed.join(ampr_index::NAME)).unwrap();

        let inspection = ps5upload_fpkg::build::inspect(&packed, root).unwrap();
        assert_eq!(inspection.packs, Some("zstd"));
        let check = inspection.checks.iter().find(|c| c.name == CHECK).unwrap();
        assert!(!check.ok);
        assert!(
            check
                .detail
                .contains("is stale (1 listed missing, 0 unlisted, 1 sizes differ;"),
            "{}",
            check.detail
        );
        assert_eq!(
            check.applies_to,
            Some(ps5upload_fpkg::source::applies::PACKS_KEPT)
        );

        // Unpacked into a package: a fresh index of what the package carries, and no warning.
        // (ampr-emu-ng came with the packs and is replaced, as in any unpack.)
        let reader = unpacking(Some(ampr_emu("0.4.2.1")));
        let unpacked = package_report(&packed, &root.join("unpacked"), Some(reader));
        assert!(
            !unpacked.warnings.iter().any(|w| w.contains(CHECK)),
            "{:?}",
            unpacked.warnings
        );
        let mut pkg = ps5upload_fpkg::source::open(&unpacked.path).unwrap();
        assert_ne!(pkg.read(ampr_index::NAME).unwrap(), stale);
        assert_index_lists_the_package(&mut *pkg);
        assert_eq!(pkg.read("eboot.bin").unwrap(), eboot);

        // The packs kept: the stale index is what the package carries, and the build says so.
        let kept = package_report(&packed, &root.join("kept"), None);
        assert!(
            kept.warnings
                .iter()
                .any(|w| w.starts_with(CHECK) && w.contains("is stale")),
            "{:?}",
            kept.warnings
        );
        let mut pkg = ps5upload_fpkg::source::open(&kept.path).unwrap();
        assert_eq!(pkg.read(ampr_index::NAME).unwrap(), stale);
    }

    /// The same for an LZ4-packed image of a libSceAmpr game: its ampr_emu, new enough, is
    /// kept as it is, or left out when asked.
    #[test]
    fn an_lz4_packed_game_packages_as_plain_files() {
        use crate::image_build::{build_packed, ImageFormat, Lz4Packs, DEFAULT_VERIFY_CHUNK};
        let scratch = Scratch::new("pkg-lz4");
        let root = &scratch.0;
        let src = game(root, true);
        // ampr_emu 0.4.2.1 with pack support, which LZ4 packs need.
        let mut module = vec![0u8; 4096];
        module[100..108].copy_from_slice(b"AMPRPAK4");
        module[200..217].copy_from_slice(b"ampr_assets.index");
        module[300..321].copy_from_slice(b"\x000.4.2.1 (c) Drakmor\0");
        std::fs::create_dir_all(src.join("fakelib")).unwrap();
        std::fs::write(src.join("fakelib/libSceAmpr.sprx"), &module).unwrap();
        let image = root.join("PPSA99996-lz4.exfat");
        build_packed(
            ImageFormat::Exfat,
            None,
            ps5upload_fpkg::ffpfsc_block::Level::default(),
            &mut FolderSource::open(&src).unwrap(),
            &image,
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
        let mut packed = ps5upload_fpkg::source::open(&image).unwrap();
        assert_eq!(
            ps5upload_fpkg::source::packed_kind(&mut *packed).unwrap(),
            Some("lz4")
        );
        assert!(!names_of(&*packed).iter().any(|n| n == "data/sub/table.bin"));

        let mut kept = package(&image, &root.join("kept"), unpacking(None));
        assert_packaged_plain(&mut *kept, &src);
        assert_eq!(kept.read("fakelib/libSceAmpr.sprx").unwrap(), module);
        assert_index_lists_the_package(&mut *kept);

        let mut bare = package(&image, &root.join("bare"), unpacking_without_emulator());
        assert_packaged_plain(&mut *bare, &src);
        let names = names_of(&*bare);
        assert!(!names.iter().any(|n| is_emulator_file(n)), "{names:?}");
        assert_index_lists_the_package(&mut *bare);
    }

    /// A game that doesn't read through libSceAmpr was given ampr-emu-ng only to read its
    /// packs: unpacked into a package it keeps it unless left out, and left out it has no
    /// emulator and no index.
    #[test]
    fn a_game_without_ampr_packages_with_nothing_of_the_emulator_left() {
        let scratch = Scratch::new("pkg-noampr");
        let root = &scratch.0;
        let src = game(root, false);
        let packed = root.join("packed");
        zstd_packed(&src, &packed);

        let mut kept = package(&packed, &root.join("kept"), unpacking(None));
        assert_packaged_plain(&mut *kept, &src);
        assert_eq!(
            kept.read("fakelib/libSceAmpr.sprx").unwrap(),
            b"NG EMULATOR"
        );

        let mut bare = package(&packed, &root.join("bare"), unpacking_without_emulator());
        assert_packaged_plain(&mut *bare, &src);
        let names = names_of(&*bare);
        assert!(!names.iter().any(|n| is_emulator_file(n)), "{names:?}");
        assert!(!names.iter().any(|n| n == ampr_index::NAME), "{names:?}");
    }

    #[test]
    fn the_emulators_files_are_named_in_any_case() {
        for yes in [
            "fakelib/libSceAmpr.sprx",
            "FAKELIB/libsceampr.SPRX",
            "fakelib2/libSceAmpr.sprx",
            "fakelib/libSceAmpr.sprx.orig",
        ] {
            assert!(is_emulator_file(yes), "{yes}");
        }
        for no in [
            "fakelib/libScePlayGo.sprx",
            "libSceAmpr.sprx",
            "data/fakelib/libSceAmpr.sprx",
            "fakelib/sub/libSceAmpr.sprx",
            "ampr_emu.index",
        ] {
            assert!(!is_emulator_file(no), "{no}");
        }
    }
}
