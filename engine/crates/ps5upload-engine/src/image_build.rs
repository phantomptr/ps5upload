//! Game images from a game folder: `.exfat`, `.ffpkg` (UFS2) or `.ffpfs` (PFS), optionally
//! compressed into a `.ffpfsc` as they are written.
//!
//! The build code (planning, writing, the read-back, checkpoints, LZ4 packs) is
//! `ps5upload_convert_core::image_build`, shared with the converter that runs on the PS5, and is
//! re-exported here. What stays in the engine: the streams straight to a console
//! ([`to_console`], AVA1) and to a saved server ([`to_server`]), plain or in a `.ffpfsc`
//! ([`ffpfsc_out`]).

pub(crate) use ps5upload_convert_core::image_build::*;

mod ffpfsc_out;
mod to_console;
mod to_server;
pub(crate) use ffpfsc_out::{FfpfscOut, MeasuredFfpfsc};
#[cfg(test)]
pub(crate) use to_console::tests as to_console_tests;
pub(crate) use to_console::{
    console_stamp, forget_console_stamp, stream_to_console, ConsoleTarget, Streamed,
};
pub(crate) use to_server::{server_stamp, stream_to_server, ServerImage, ToServer};

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use ps5upload_fpkg::sample::VerifyMode;
use ps5upload_fpkg::source::SourceTree;

/// Where an LZ4 build that goes straight to a console or a server packs its volumes when they
/// can't be made as they are sent (see [`stream_lz4`]): in the engine's data folder (the temp
/// folder without one), one spool per destination, `claimed` (what the job claimed for it).
pub(crate) fn stream_spool(claimed: &Path) -> PathBuf {
    let base = crate::remote::store::data_dir().unwrap_or_else(std::env::temp_dir);
    let h = blake3::hash(claimed.to_string_lossy().as_bytes()).to_hex();
    base.join("lz4spool").join(format!("{}.lz4spool", &h[..32]))
}

/// Streams the packed game (with what else its bytes depend on) wherever it goes.
pub(crate) type SendPacked<'a, T> =
    dyn FnMut(&mut dyn SourceTree, &str, &mut dyn FnMut(&str, u64, u64)) -> anyhow::Result<T> + 'a;

/// An image with LZ4 packs straight to a console or a server, as ps5-dump-forge writes packs
/// into an image on another device: the packs measured with `mtime` pinned on the new index, and
/// each volume made again from `source` as it is sent (`lz4_packs::measured`), so nothing is
/// stored on this computer and making them again gives the same bytes (the pack set's build ID
/// goes into the job's id through the salt `send` gets); `send` streams the packed game and reads
/// the image back there against it; then the game is read back through the packs of the copy
/// there (`reopen`) against `source`. An image that fails that is removed (`remove` says how
/// that went) from `place`. When a volume can't be made again as measured (the source changed,
/// or a chunk compressed another way), it goes again with the packs made into `spool` on this
/// computer first, which is removed whatever happens. Returns what `send` did and the line the
/// job's detail gets.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stream_lz4<T>(
    packs: &Lz4Packs,
    source: &mut dyn SourceTree,
    mtime: i64,
    spool: &Path,
    to: &str,
    mode: VerifyMode,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
    send: &mut SendPacked<T>,
    reopen: &dyn Fn() -> Result<Box<dyn SourceTree>, String>,
    remove: &dyn Fn() -> String,
    place: &str,
) -> anyhow::Result<(T, String)> {
    // Ours (the job claimed it): what a killed engine left there goes.
    let _ = std::fs::remove_dir_all(spool);
    let streamed = stream_lz4_once(
        packs, source, mtime, None, to, mode, cancel, stage, send, reopen, remove, place,
    );
    match streamed {
        Err(e)
            if !cancel.load(Ordering::Relaxed)
                && ps5upload_fpkg::ampr_pack::regen::not_made_again(&format!("{e:#}")) =>
        {
            crate::log_at!(
                Warn,
                "convert",
                "lz4 packs: {e:#}; packing them on this computer first instead"
            );
            stream_lz4_once(
                packs,
                source,
                mtime,
                Some(spool),
                to,
                mode,
                cancel,
                stage,
                send,
                reopen,
                remove,
                place,
            )
        }
        r => r,
    }
}

/// [`stream_lz4`]'s one try: the packs measured and made again as they are sent, or (with
/// `spool`) made into it first.
#[allow(clippy::too_many_arguments)]
fn stream_lz4_once<T>(
    packs: &Lz4Packs,
    source: &mut dyn SourceTree,
    mtime: i64,
    spool: Option<&Path>,
    to: &str,
    mode: VerifyMode,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
    send: &mut SendPacked<T>,
    reopen: &dyn Fn() -> Result<Box<dyn SourceTree>, String>,
    remove: &dyn Fn() -> String,
    place: &str,
) -> anyhow::Result<(T, String)> {
    if let Some(parent) = spool.and_then(Path::parent) {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("{}: {e}", parent.display()))?;
    }
    let result = (|| {
        let crate::lz4_packs::Spooled {
            mut tree,
            report,
            config,
        } = match spool {
            Some(spool) => crate::lz4_packs::spool(source, packs, spool, mtime, to, cancel, stage),
            None => crate::lz4_packs::measured(source, packs, mtime, to, cancel, stage),
        }
        .map_err(anyhow::Error::msg)?;
        let salt = crate::lz4_packs::salt(&report);
        let streamed = send(&mut tree, &salt, stage)?;
        drop(tree);
        let checked = reopen().and_then(|image| {
            crate::lz4_packs::read_back(image, source, &config, mode, cancel, &mut |d, t| {
                stage("verify", d, t)
            })
        });
        match checked {
            Ok(line) => Ok((
                streamed,
                format!(
                    ", {line}{}{}",
                    crate::lz4_packs::detail(&report),
                    if spool.is_some() {
                        ", packed on this computer first"
                    } else {
                        ""
                    }
                ),
            )),
            // A Stop leaves nothing: an image not checked through its packs doesn't stay there
            // looking like a good one.
            Err(e) if cancel.load(Ordering::Relaxed) => {
                let removed = remove();
                anyhow::bail!(
                    "cancelled while reading the LZ4 packs back on {place} ({e}); {removed}"
                )
            }
            Err(e) => {
                let removed = remove();
                anyhow::bail!("{e}, on {place}; {removed}")
            }
        }
    })();
    if let Some(spool) = spool {
        let _ = std::fs::remove_dir_all(spool);
    }
    result
}

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

    /// The game read through the LZ4 packs of `image` is the source at `src`.
    fn reads_as_the_source(image: Box<dyn SourceTree>, src: &Path, what: &str) {
        use ps5upload_fpkg::ampr_pack::image::{Lz4Unpacked, Unpacking};
        let Unpacking::Decoded(mut game) = Lz4Unpacked::open(image).unwrap() else {
            panic!("{what}: no LZ4 packs");
        };
        let mut source = FolderSource::open(src).unwrap();
        for f in source.files().to_vec() {
            if f.path == ps5upload_fpkg::ampr_index::NAME {
                continue;
            }
            assert_eq!(
                game.read(&f.path).unwrap(),
                source.read(&f.path).unwrap(),
                "{what}: {}",
                f.path
            );
        }
    }

    /// An image with LZ4 packs, in every format, streamed straight to the console and to a
    /// server: written there, read back there through its filesystem and then through its
    /// packs; the spool is gone; and packing again with the same time gives the same bytes, so
    /// a stream that starts over goes on from what is there.
    #[tokio::test(flavor = "multi_thread")]
    async fn lz4_packed_images_stream_to_the_console_and_a_server() {
        use crate::image_build::to_console_tests::{cfg, Loopback};
        let lb = Loopback::start("lz4", None).await;
        let src = ampr_game(&lb.dir);
        let packs = Lz4Packs {
            profile: Some(ps5upload_fpkg::ampr_pack::default_profile(9, 16)),
            level: 9,
            block_shift: 16,
        };
        let cancel = AtomicBool::new(false);
        let c = cfg();
        let mut first = None;
        for format in [ImageFormat::Exfat, ImageFormat::Ffpkg, ImageFormat::Ffpfs] {
            let name = format!("dst/G.{}", format.extension());
            let spool = lb
                .dir
                .join(format!("spool-{}.lz4spool", format.extension()));
            let mut stages: Vec<String> = Vec::new();
            let (streamed, line) = tokio::task::block_in_place(|| {
                let mut tree = FolderSource::open(&src).unwrap();
                lb.with_target(&c, &name, |t| {
                    stream_lz4(
                        &packs,
                        &mut tree,
                        7,
                        &spool,
                        &t.path,
                        VerifyMode::Full,
                        &cancel,
                        &mut |s, _, _| {
                            if stages.last().map(String::as_str) != Some(s) {
                                stages.push(s.to_string());
                            }
                        },
                        &mut |tree, salt, stage| {
                            stream_to_console(
                                format,
                                None,
                                Some(7),
                                salt,
                                tree,
                                t,
                                VerifyMode::Full,
                                DEFAULT_VERIFY_CHUNK,
                                &cancel,
                                stage,
                            )
                        },
                        &|| {
                            ps5upload_fpkg::remote_source::open_remote(t.files.clone(), &t.path)
                                .map_err(|e| e.to_string())
                        },
                        &|| "removed".into(),
                        "the PS5",
                    )
                })
            })
            .unwrap_or_else(|e| panic!("{format:?}: {e:#}"));
            assert_eq!(stages, ["pack", "plan", "write", "verify"], "{format:?}");
            assert!(line.contains("read back through the LZ4 packs"), "{line}");
            assert!(!spool.exists(), "the spool is removed");
            let on_console = lb.share().join(&name);
            assert_eq!(
                std::fs::metadata(&on_console).unwrap().len(),
                streamed.built.image_bytes
            );
            let image = open_image(&on_console, format, false).unwrap();
            reads_as_the_source(image, &src, &format!("{format:?} on the console"));
            if format == ImageFormat::Exfat {
                first = Some(std::fs::read(&on_console).unwrap());
            }
        }

        // To a server: the same .exfat byte for byte (the same time, the same packs), read back
        // there, no partial file left.
        let mem = std::sync::Arc::new(crate::remote::MemFs::new(&[]));
        let remote =
            crate::remote::pool::testing::remote_with_shared(std::sync::Arc::clone(&mem), None);
        let id = remote
            .store
            .add(
                crate::remote::store::conn("NAS", crate::remote::store::Protocol::Smb),
                crate::remote::store::Secret::None,
            )
            .unwrap()
            .conn
            .id;
        let dest = crate::server_out::ServerDest::with(
            remote,
            &id,
            "/games/ps5",
            tokio::runtime::Handle::current(),
        )
        .unwrap();
        let target = ServerImage {
            dest: &dest,
            name: "G.exfat".into(),
            data: None,
        };
        let path = target.path();
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let spool = lb.dir.join("server.lz4spool");
        tokio::task::block_in_place(|| {
            let mut tree = FolderSource::open(&src).unwrap();
            stream_lz4(
                &packs,
                &mut tree,
                7,
                &spool,
                &path,
                VerifyMode::Full,
                &cancel,
                &mut |_, _, _| {},
                &mut |tree, salt, stage| {
                    stream_to_server(
                        ImageFormat::Exfat,
                        None,
                        7,
                        salt,
                        tree,
                        &target,
                        VerifyMode::Full,
                        DEFAULT_VERIFY_CHUNK,
                        &cancel,
                        stage,
                    )
                    .map_err(anyhow::Error::msg)
                },
                &|| {
                    let files = dest.files().map_err(|e| crate::server_out::said(&e))?;
                    ps5upload_fpkg::remote_source::open_remote(files, &path)
                        .map_err(|e| e.to_string())
                },
                &|| "removed".into(),
                "NAS",
            )
        })
        .unwrap_or_else(|e| panic!("{e:#}"));
        assert!(
            mem.bytes("/games/ps5/G.exfat").unwrap() == first.unwrap(),
            "the server's packed image differs from the console's"
        );
        assert!(mem.bytes("/games/ps5/G.exfat.partial").is_none());
        assert!(!spool.exists());
    }

    /// One LZ4 stream to the console: packed into `spool` first when given, else measured and
    /// made as it is sent; `before` runs as the packed game starts to go out.
    fn lz4_to_console(
        lb: &crate::image_build::to_console_tests::Loopback,
        src: &Path,
        name: &str,
        spool: Option<&Path>,
        before: &mut dyn FnMut(),
    ) -> anyhow::Result<String> {
        let packs = Lz4Packs {
            profile: Some(ps5upload_fpkg::ampr_pack::default_profile(9, 16)),
            level: 9,
            block_shift: 16,
        };
        let cancel = AtomicBool::new(false);
        let c = crate::image_build::to_console_tests::cfg();
        let mut tree = FolderSource::open(src).unwrap();
        lb.with_target(&c, name, |t| {
            let mut send =
                |tree: &mut dyn SourceTree, salt: &str, stage: &mut dyn FnMut(&str, u64, u64)| {
                    before();
                    stream_to_console(
                        ImageFormat::Exfat,
                        None,
                        Some(7),
                        salt,
                        tree,
                        t,
                        VerifyMode::Full,
                        DEFAULT_VERIFY_CHUNK,
                        &cancel,
                        stage,
                    )
                };
            let reopen = || {
                ps5upload_fpkg::remote_source::open_remote(t.files.clone(), &t.path)
                    .map_err(|e| e.to_string())
            };
            let remove = || "removed".to_string();
            match spool {
                Some(spool) => stream_lz4_once(
                    &packs,
                    &mut tree,
                    7,
                    Some(spool),
                    &t.path,
                    VerifyMode::Full,
                    &cancel,
                    &mut |_, _, _| {},
                    &mut send,
                    &reopen,
                    &remove,
                    "the PS5",
                ),
                None => stream_lz4(
                    &packs,
                    &mut tree,
                    7,
                    &lb.dir.join("fallback.lz4spool"),
                    &t.path,
                    VerifyMode::Full,
                    &cancel,
                    &mut |_, _, _| {},
                    &mut send,
                    &reopen,
                    &remove,
                    "the PS5",
                ),
            }
            .map(|(_, line)| line)
        })
    }

    /// Streamed with nothing spooled, the image on the console is the one the spool gives, byte
    /// for byte, and no spool is made.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_lz4_stream_without_a_spool_is_the_spooled_one() {
        let lb = crate::image_build::to_console_tests::Loopback::start("lz4-nospool", None).await;
        let src = ampr_game(&lb.dir);
        let spool = lb.dir.join("one.lz4spool");
        let (spooled, streamed) = tokio::task::block_in_place(|| {
            let spooled = lz4_to_console(&lb, &src, "dst/A.exfat", Some(&spool), &mut || {})
                .unwrap_or_else(|e| panic!("{e:#}"));
            let fallback = lb.dir.join("fallback.lz4spool");
            let streamed = lz4_to_console(&lb, &src, "dst/B.exfat", None, &mut || {
                assert!(!fallback.exists(), "nothing is spooled");
            })
            .unwrap_or_else(|e| panic!("{e:#}"));
            (spooled, streamed)
        });
        assert!(
            spooled.contains("packed on this computer first"),
            "{spooled}"
        );
        assert!(
            !streamed.contains("packed on this computer first"),
            "{streamed}"
        );
        assert!(!spool.exists());
        let a = std::fs::read(lb.share().join("dst/A.exfat")).unwrap();
        let b = std::fs::read(lb.share().join("dst/B.exfat")).unwrap();
        assert!(
            a == b,
            "the image streamed without a spool differs from the spooled one"
        );
    }

    /// A packed file that changes after the measure: its volume can't be made as measured, the
    /// stream stops before sending a wrong chunk, and goes again with the packs spooled here.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_lz4_stream_whose_source_changes_goes_again_with_a_spool() {
        let lb = crate::image_build::to_console_tests::Loopback::start("lz4-changed", None).await;
        let src = ampr_game(&lb.dir);
        let table = src.join("data/only_packed/table.bin");
        let mut sends = 0;
        let line = tokio::task::block_in_place(|| {
            lz4_to_console(&lb, &src, "dst/C.exfat", None, &mut || {
                sends += 1;
                if sends == 1 {
                    // The same size, other bytes.
                    let mut b = std::fs::read(&table).unwrap();
                    b[300_000] ^= 0xFF;
                    std::fs::write(&table, b).unwrap();
                }
            })
        })
        .unwrap_or_else(|e| panic!("{e:#}"));
        assert_eq!(sends, 2, "sent again after the first stopped");
        assert!(line.contains("packed on this computer first"), "{line}");
        assert!(!lb.dir.join("fallback.lz4spool").exists());
        let image = open_image(&lb.share().join("dst/C.exfat"), ImageFormat::Exfat, false).unwrap();
        reads_as_the_source(image, &src, "the image sent again");
    }

    /// A Stop while the image there is read back through its packs removes it, as a failed
    /// read-back does: a cancel leaves nothing that looks like a good image.
    #[test]
    fn a_cancel_during_the_pack_read_back_removes_the_image() {
        let root = std::env::temp_dir().join(format!("ps5upload-lz4cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = ampr_game(&root);
        let packs = Lz4Packs {
            profile: Some(ps5upload_fpkg::ampr_pack::default_profile(9, 16)),
            level: 9,
            block_shift: 16,
        };
        let cancel = AtomicBool::new(false);
        let removed = AtomicBool::new(false);
        let spool = root.join("c.lz4spool");
        let mut tree = FolderSource::open(&src).unwrap();
        let e = stream_lz4(
            &packs,
            &mut tree,
            7,
            &spool,
            "dst/G.exfat",
            VerifyMode::Full,
            &cancel,
            &mut |_, _, _| {},
            &mut |_, _, _| Ok(()),
            // The Stop comes as the image is opened for the read-back.
            &|| {
                cancel.store(true, Ordering::Relaxed);
                Err("cancelled".into())
            },
            &|| {
                removed.store(true, Ordering::Relaxed);
                "the image there is removed".into()
            },
            "the PS5",
        )
        .unwrap_err();
        assert!(removed.load(Ordering::Relaxed), "{e:#}");
        assert!(e.to_string().contains("removed"), "{e:#}");
        assert!(!spool.exists());
        let _ = std::fs::remove_dir_all(&root);
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
            None,
            ps5upload_fpkg::ffpfsc_block::Level::default(),
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
