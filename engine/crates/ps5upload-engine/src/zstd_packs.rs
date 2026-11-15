//! zstd asset packs in a game image: `ampr_zstd.index` and `ampr_zstd-NNN.pak`, read by quer3q's
//! ampr-emu-ng. Smaller than LZ4 packs, and written straight into the image: a measure pass
//! fixes every chunk's size, then `ps5_dump_forge_zstd::tree::PackedTree` makes the volumes on
//! demand from the source, so nothing is spooled to disk.
//!
//! Which files are packed and what the game gets follow quer3q/ps5-dump-forge v0.5.4
//! (`ps5-dump-forge-core/src/zstd_pack.rs`, GPL-3.0-or-later): every non-empty file but system
//! files, because ampr-emu-ng serves a fully packed game; ampr-emu-ng as the game's emulator; a
//! fresh `ampr_emu.index` listing every file, loose and packed. A game whose eboot does not
//! import libSceAmpr is packed the same way, as Forge packs it (`zstd_pack.rs` `prepare`: "A
//! title without AMPR is packed too ... ShadowMount+ loads the injected ampr-emu-ng"): nothing
//! else is written for it, and it is new enough to be marked untested in the app.
//!
//! The packing and the local builds are `ps5upload_convert_core::zstd_packs`, shared with the
//! converter that runs on the PS5. What stays here: the emulator's release this engine downloads
//! (`zstd_runtime`), whose bytes it hands to the builder, and the streams to a console or server.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use ps5_dump_forge_zstd::tree::PackedTree;
use ps5upload_convert_core::zstd_packs::{measure, pin_archive_id, prepare};
pub(crate) use ps5upload_convert_core::zstd_packs::{readable_for_packing, Rules};
use ps5upload_fpkg::sample::VerifyMode;
use ps5upload_fpkg::source::SourceTree;

use crate::image_build::{ImageBuilt, ImageFormat};
use crate::zstd_runtime::PackRuntime;

/// ampr-emu-ng's tested release: fetched once, and only kept or used when its SHA-256 matches.
/// What a pack build falls back to when the newest release can't be used (`zstd_runtime`).
/// 0.4.1: releases before 0.3.4 hang some games at startup (ASTRO BOT, ampr-emu-ng#17), and
/// 0.4.0 refuses very large games (its 48 MiB index cap; FF16, Demon's Souls, Returnal).
pub(crate) const NG_VERSION: &str = "0.4.1";
pub(crate) const NG_URL: &str =
    "https://github.com/quer3q/ampr-emu-ng/releases/download/v0.4.1/libSceAmpr.sprx";
pub(crate) const NG_SHA256: &str =
    "9c7c41b3216bc4f877d67e3c2e5c9c8f9ffabdd5065f4f3fe0dd98fa7228386f";

/// [`ps5upload_convert_core::zstd_packs::build_at`] with the emulator this engine fetched:
/// straight into a `.ffpfsc` when `inner_name` is given, compressed at `compression`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build(
    format: ImageFormat,
    inner_name: Option<&str>,
    compression: ps5upload_fpkg::ffpfsc_block::Level,
    level: u8,
    rules: Option<&Rules>,
    runtime: &PackRuntime,
    source: &mut dyn SourceTree,
    output: &Path,
    mode: VerifyMode,
    verify_chunk: u64,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<ImageBuilt, String> {
    ps5upload_convert_core::zstd_packs::build_at(
        format,
        inner_name,
        compression,
        level,
        rules,
        &runtime.bytes,
        source,
        output,
        mode,
        verify_chunk,
        cancel,
        stage,
        0,
    )
}

/// [`ps5upload_convert_core::zstd_packs::build_folder`] with the emulator this engine fetched.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_folder(
    level: u8,
    rules: Option<&Rules>,
    runtime: &PackRuntime,
    source: &mut dyn SourceTree,
    reopen: &dyn Fn() -> Result<Box<dyn SourceTree>, String>,
    target: &Path,
    mode: VerifyMode,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<crate::folder_build::FolderBuilt, String> {
    ps5upload_convert_core::zstd_packs::build_folder(
        level,
        rules,
        &runtime.bytes,
        source,
        reopen,
        target,
        mode,
        cancel,
        stage,
        0,
    )
}

/// [`ps5upload_convert_core::zstd_packs::build_folder_to`] with the emulator this engine fetched,
/// to a folder on this computer or on a saved server.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_folder_to(
    level: u8,
    rules: Option<&Rules>,
    runtime: &PackRuntime,
    source: &mut dyn SourceTree,
    reopen: &dyn Fn() -> Result<Box<dyn SourceTree>, String>,
    out: &crate::folder_build::FolderOut,
    mode: VerifyMode,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<crate::folder_build::FolderBuilt, String> {
    ps5upload_convert_core::zstd_packs::build_folder_to(
        level,
        rules,
        &runtime.bytes,
        source,
        reopen,
        out,
        mode,
        cancel,
        stage,
        0,
    )
}

/// [`build`] straight to the console (`image_build::stream_to_console`): the packs made the
/// same way, with `mtime` pinned on the index so a stream that has to start over gives the same
/// bytes, and the packs read back from the console's copy through the emulator's checks. An
/// image that fails them is removed from the console.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stream_to_console(
    format: ImageFormat,
    level: u8,
    rules: Option<&Rules>,
    runtime: &PackRuntime,
    source: &mut dyn SourceTree,
    mtime: i64,
    target: &crate::image_build::ConsoleTarget,
    mode: VerifyMode,
    verify_chunk: u64,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> anyhow::Result<crate::image_build::Streamed> {
    stream_packed(
        level,
        rules,
        runtime,
        source,
        mtime,
        &target.path,
        cancel,
        stage,
        &mut |tree, salt, stage| {
            crate::image_build::stream_to_console(
                format,
                Some(mtime),
                salt,
                tree,
                target,
                mode,
                verify_chunk,
                cancel,
                stage,
            )
        },
        &|| {
            ps5upload_fpkg::remote_source::open_remote(target.files.clone(), &target.path)
                .map_err(|e| e.to_string())
        },
        &|| match (target.remove)(&target.path) {
            Ok(()) => "it was removed from the PS5".to_string(),
            Err(r) => format!("removing it from the PS5 failed too ({r:#}): delete it by hand"),
        },
        "the PS5",
    )
}

/// [`build`] straight to a server (`image_build::stream_to_server`), as [`stream_to_console`]
/// does it: the packs read back from the server's copy, which is removed if they fail.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stream_to_server(
    format: ImageFormat,
    level: u8,
    rules: Option<&Rules>,
    runtime: &PackRuntime,
    source: &mut dyn SourceTree,
    mtime: i64,
    target: &crate::image_build::ServerImage,
    mode: VerifyMode,
    verify_chunk: u64,
    cancel: &std::sync::Arc<AtomicBool>,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<crate::image_build::ToServer, String> {
    let dest = target.dest;
    let path = target.path();
    stream_packed(
        level,
        rules,
        runtime,
        source,
        mtime,
        &path,
        cancel,
        stage,
        &mut |tree, salt, stage| {
            crate::image_build::stream_to_server(
                format,
                mtime,
                salt,
                tree,
                target,
                mode,
                verify_chunk,
                cancel,
                stage,
            )
            .map_err(anyhow::Error::msg)
        },
        &|| {
            let files = dest.files().map_err(|e| crate::server_out::said(&e))?;
            ps5upload_fpkg::remote_source::open_remote(files, &path).map_err(|e| e.to_string())
        },
        &|| match dest.fs() {
            Ok(fs) => dest.remove(&*fs, &path),
            Err(e) => format!(
                "removing it from {} failed too ({e}): delete {path} by hand",
                dest.label
            ),
        },
        &dest.label,
    )
    .map_err(|e| format!("{e:#}"))
}

/// Streams the packed game (with the salt its bytes also depend on) wherever it goes.
type SendPacked<'a, T> =
    dyn FnMut(&mut dyn SourceTree, &str, &mut dyn FnMut(&str, u64, u64)) -> anyhow::Result<T> + 'a;

/// The packs made as [`build`] makes them, with `mtime` pinned on the index and the archive ID
/// drawn from the packs themselves, so making them again gives the same bytes; `send` streams
/// the packed game (with what else its bytes depend on) wherever it goes, then the packs are
/// read back from there (`reopen`) through the emulator's checks. An image that fails them is
/// removed (`remove` says how that went) from `place`.
#[allow(clippy::too_many_arguments)]
fn stream_packed<T>(
    level: u8,
    rules: Option<&Rules>,
    runtime: &PackRuntime,
    source: &mut dyn SourceTree,
    mtime: i64,
    to: &str,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
    send: &mut SendPacked<T>,
    reopen: &dyn Fn() -> Result<Box<dyn SourceTree>, String>,
    remove: &dyn Fn() -> String,
    place: &str,
) -> anyhow::Result<T> {
    let (mut prepared, packed) =
        prepare(source, runtime.bytes.clone(), mtime, rules).map_err(anyhow::Error::msg)?;
    let mut index = measure(
        &mut prepared,
        &packed,
        level,
        Path::new(to),
        cancel,
        stage,
        0,
    )
    .map_err(anyhow::Error::msg)?;
    pin_archive_id(&mut index).map_err(anyhow::Error::msg)?;
    let streamed = {
        let mut tree = PackedTree::new(&mut prepared, &index, 0, cancel)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        // The bytes also follow the level, the emulator and the profile: a different one is a
        // different job. Without a profile the salt is what it was before profiles, so a
        // stream begun by an older build of this app still goes on.
        let mut salt = format!(
            "zstd level {level} emulator {}",
            blake3::hash(&runtime.bytes).to_hex()
        );
        if let Some(p) = rules {
            salt.push_str(&format!(
                " profile {}",
                blake3::hash(&p.canonical_json()).to_hex()
            ));
        }
        send(&mut tree, &salt, stage)?
    };
    let unpacked = reopen().and_then(|image| {
        ps5_dump_forge_zstd::reader::unpack(image)
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
    if let Err(e) = unpacked {
        let removed = remove();
        anyhow::bail!("reading the zstd packs back from the image on {place}: {e}; {removed}");
    }
    Ok(streamed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps5upload_fpkg::source::FolderSource;

    /// The game `ps5upload_convert_core::zstd_packs`'s tests pack.
    fn game(root: &Path) -> std::path::PathBuf {
        let src = root.join("PPSA99997-app");
        std::fs::create_dir_all(src.join("sce_sys")).unwrap();
        std::fs::create_dir_all(src.join("data/sub")).unwrap();
        let mut eboot = vec![0u8; 200_000];
        eboot[1000..1014].copy_from_slice(b"libSceAmpr.prx");
        std::fs::write(src.join("eboot.bin"), eboot).unwrap();
        std::fs::write(
            src.join("sce_sys/param.json"),
            b"{\"titleId\":\"PPSA99997\"}",
        )
        .unwrap();
        let text: Vec<u8> = (0..1_000_000u32)
            .flat_map(|i| format!("asset {} ", i % 97).into_bytes())
            .take(900_000)
            .collect();
        std::fs::write(src.join("data/sub/table.bin"), &text).unwrap();
        std::fs::write(src.join("data/settings.json"), b"{\"a\":1}").unwrap();
        std::fs::write(src.join("data/empty.bin"), b"").unwrap();
        std::fs::write(src.join("ampr_emu.log"), b"from a run").unwrap();
        src
    }

    fn runtime() -> PackRuntime {
        PackRuntime {
            name: "ampr-emu-ng",
            version: "9.9.9".into(),
            source: "pinned",
            fallback_reason: None,
            bytes: b"EMULATOR".to_vec(),
        }
    }

    /// zstd packs go straight to the console and to a server in a `.ffpkg` and a `.ffpfs` too,
    /// as ps5-dump-forge writes them into any image on another device: each read back there,
    /// and the game read through its packs as it was.
    #[tokio::test(flavor = "multi_thread")]
    async fn packed_ufs2_and_pfs_images_stream_to_the_console_and_a_server() {
        use crate::image_build::to_console_tests::{cfg, Loopback};
        let lb = Loopback::start("zstdany", None).await;
        let src = game(&lb.dir);
        let want = std::fs::read(src.join("data/sub/table.bin")).unwrap();
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let c = cfg();
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
        for format in [ImageFormat::Ffpkg, ImageFormat::Ffpfs] {
            let name = format!("G.{}", format.extension());
            let on_console = format!("dst/{name}");
            tokio::task::block_in_place(|| {
                let mut tree = FolderSource::open(&src).unwrap();
                lb.with_target(&c, &on_console, |t| {
                    stream_to_console(
                        format,
                        5,
                        None,
                        &runtime(),
                        &mut tree,
                        7,
                        t,
                        VerifyMode::Full,
                        crate::image_build::DEFAULT_VERIFY_CHUNK,
                        &cancel,
                        &mut |_, _, _| {},
                    )
                })
            })
            .unwrap_or_else(|e| panic!("{format:?}: {e:#}"));
            let image =
                crate::image_build::open_image(&lb.share().join(&on_console), format, false)
                    .unwrap();
            let mut game = ps5_dump_forge_zstd::reader::unpack(image).unwrap();
            assert_eq!(game.read("data/sub/table.bin").unwrap(), want, "{format:?}");

            let target = crate::image_build::ServerImage {
                dest: &dest,
                name: name.clone(),
                data: None,
            };
            tokio::task::block_in_place(|| {
                let mut tree = FolderSource::open(&src).unwrap();
                stream_to_server(
                    format,
                    5,
                    None,
                    &runtime(),
                    &mut tree,
                    7,
                    &target,
                    VerifyMode::Full,
                    crate::image_build::DEFAULT_VERIFY_CHUNK,
                    &cancel,
                    &mut |_, _, _| {},
                )
            })
            .unwrap_or_else(|e| panic!("{format:?}: {e}"));
            let got = mem.bytes(&format!("/games/ps5/{name}")).unwrap();
            assert!(
                got == std::fs::read(lb.share().join(&on_console)).unwrap(),
                "{format:?}: the server's image differs from the console's"
            );
        }
    }

    /// An .exfat with zstd packs streamed to the console is the image a local build writes, and
    /// its packs read back there.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_packed_exfat_streams_to_the_console_as_built_here() {
        use crate::image_build::to_console_tests::{cfg, Loopback};
        let lb = Loopback::start("zstd", None).await;
        let src = game(&lb.dir);
        let cancel = AtomicBool::new(false);
        let local = lb.dir.join("local.exfat");
        {
            let mut tree = FolderSource::open(&src).unwrap();
            let (mut prepared, packed) = prepare(&mut tree, runtime().bytes, 7, None).unwrap();
            let mut index = ps5_dump_forge_zstd::writer::measure(
                &mut prepared,
                &packed,
                5,
                0,
                &mut |_| {},
                &cancel,
            )
            .unwrap();
            // The measure pass draws a random ID; pinned, the same packs have the same one.
            let drawn = index.archive_id;
            pin_archive_id(&mut index).unwrap();
            let pinned = index.archive_id;
            pin_archive_id(&mut index).unwrap();
            assert_eq!(index.archive_id, pinned);
            assert_ne!(pinned, drawn);
            let mut packs = PackedTree::new(&mut prepared, &index, 0, &cancel).unwrap();
            crate::image_build::build(
                ImageFormat::Exfat,
                None,
                &mut packs,
                &local,
                VerifyMode::Full,
                crate::image_build::DEFAULT_VERIFY_CHUNK,
                &cancel,
                &mut |_, _, _| {},
            )
            .unwrap();
        }
        let want = std::fs::read(&local).unwrap();
        let c = cfg();
        let mut stages: Vec<String> = Vec::new();
        let r = tokio::task::block_in_place(|| {
            let mut tree = FolderSource::open(&src).unwrap();
            lb.with_target(&c, "dst/G.exfat", |t| {
                stream_to_console(
                    ImageFormat::Exfat,
                    5,
                    None,
                    &runtime(),
                    &mut tree,
                    7,
                    t,
                    VerifyMode::Full,
                    crate::image_build::DEFAULT_VERIFY_CHUNK,
                    &cancel,
                    &mut |s, _, _| {
                        if stages.last().map(String::as_str) != Some(s) {
                            stages.push(s.to_string());
                        }
                    },
                )
            })
        })
        .unwrap_or_else(|e| panic!("{e:#}"));
        assert_eq!(stages, ["pack", "plan", "write", "verify"]);
        assert_eq!(r.built.image_bytes, want.len() as u64);
        let got = std::fs::read(lb.share().join("dst/G.exfat")).unwrap();
        assert!(
            got == want,
            "the console's packed image differs from the local build"
        );

        // To a server: the same image, its packs read back there, and no partial file left.
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
        let target = crate::image_build::ServerImage {
            dest: &dest,
            name: "G.exfat".into(),
            data: None,
        };
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let r = tokio::task::block_in_place(|| {
            let mut tree = FolderSource::open(&src).unwrap();
            stream_to_server(
                ImageFormat::Exfat,
                5,
                None,
                &runtime(),
                &mut tree,
                7,
                &target,
                VerifyMode::Full,
                crate::image_build::DEFAULT_VERIFY_CHUNK,
                &cancel,
                &mut |_, _, _| {},
            )
        })
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.built.image_bytes, want.len() as u64);
        assert!(
            mem.bytes("/games/ps5/G.exfat").unwrap() == want,
            "the server's packed image differs from the local build"
        );
        assert!(mem.bytes("/games/ps5/G.exfat.partial").is_none());
    }
}
