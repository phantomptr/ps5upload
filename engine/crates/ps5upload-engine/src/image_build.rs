//! Game images from a game folder: `.exfat`, `.ffpkg` (UFS2) or `.ffpfs` (PFS), optionally
//! compressed into a `.ffpfsc` as they are written.
//!
//! The build code (planning, writing, the read-back, checkpoints, LZ4 packs) is
//! `ps5upload_convert_core::image_build`, shared with the converter that runs on the PS5, and is
//! re-exported here. What stays in the engine: the streams straight to a console
//! ([`to_console`], AVA1) and to a saved server ([`to_server`]).

pub(crate) use ps5upload_convert_core::image_build::*;

mod to_console;
mod to_server;
#[cfg(test)]
pub(crate) use to_console::tests as to_console_tests;
pub(crate) use to_console::{
    console_stamp, forget_console_stamp, stream_to_console, ConsoleTarget, Streamed,
};
pub(crate) use to_server::{server_stamp, stream_to_server, ServerImage, ToServer};

#[cfg(test)]
mod tests {
    use super::*;
    use ps5upload_fpkg::sample::VerifyMode;
    use ps5upload_fpkg::source::{FolderSource, SourceTree};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;

    /// The core's list of image extensions is the one archive extraction knows as games.
    #[test]
    fn the_image_extensions_are_the_archive_extractors() {
        assert_eq!(
            GAME_FILE_EXTS,
            ps5upload_core::archive_extract::GAME_FILE_EXTS
        );
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

    /// "Unpack the asset packs" on an LZ4-packed image: the plain game again, the packs and
    /// their index left out, a new `ampr_emu.index`, and the game's ampr_emu (as new as the one
    /// this app installs) kept.
    #[test]
    fn an_lz4_packed_image_unpacks_into_the_plain_game() {
        let root = std::env::temp_dir().join(format!("ps5upload-imgunlz4-{}", std::process::id()));
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
        let fetch = || -> Result<Vec<u8>, String> { panic!("the game's ampr_emu is kept") };
        let emulator = crate::unpack_packs::Emulator {
            ng: false,
            fetch: &fetch,
        };
        let mut plain = crate::unpack_packs::open(image, Some(&emulator), 7).unwrap();
        let names: Vec<String> = plain.files().iter().map(|f| f.path.clone()).collect();
        assert!(
            !names.iter().any(|n| n.starts_with("ampr_assets")),
            "{names:?}"
        );
        let mut source = FolderSource::open(&src).unwrap();
        for f in source.files().to_vec() {
            if f.path == ps5upload_fpkg::ampr_index::NAME {
                continue;
            }
            assert_eq!(
                plain.read(&f.path).unwrap(),
                source.read(&f.path).unwrap(),
                "{}",
                f.path
            );
        }
        let index = plain.read(ps5upload_fpkg::ampr_index::NAME).unwrap();
        let listed = ps5upload_fpkg::ampr_index::parse(&index).unwrap();
        assert_eq!(listed.len(), names.len() - 1, "every file but the index");
        assert!(listed
            .iter()
            .any(|e| e.path == "data/only_packed/table.bin"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
