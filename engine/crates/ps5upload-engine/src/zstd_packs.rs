//! zstd asset packs in a game image: `ampr_zstd.index` and `ampr_zstd-NNN.pak`, read by quer3q's
//! ampr-emu-ng. Smaller than LZ4 packs, and written straight into the image: a measure pass
//! fixes every chunk's size, then `ps5_dump_forge_zstd::tree::PackedTree` makes the volumes on
//! demand from the source, so nothing is spooled to disk.
//!
//! Which files are packed and what the game gets follow quer3q/ps5-dump-forge v0.5.4
//! (`ps5-dump-forge-core/src/zstd_pack.rs`, GPL-3.0-or-later): every non-empty file but system
//! files, because ampr-emu-ng serves a fully packed game; ampr-emu-ng as the game's emulator; a
//! fresh `ampr_emu.index` listing every file, loose and packed.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use ps5_dump_forge_zstd::tree::PackedTree;
use ps5upload_fpkg::ampr_index;
use ps5upload_fpkg::ampr_pack::image::emulator_home;
use ps5upload_fpkg::sample::VerifyMode;
use ps5upload_fpkg::source::{SourceFile, SourceTree};

use crate::image_build::{ImageBuilt, ImageFormat};
use crate::zstd_runtime::PackRuntime;

/// ampr-emu-ng's tested release: fetched once, and only kept or used when its SHA-256 matches.
/// What a pack build falls back to when the newest release can't be used (`zstd_runtime`).
pub(crate) const NG_VERSION: &str = "0.3.2";
pub(crate) const NG_URL: &str =
    "https://github.com/quer3q/ampr-emu-ng/releases/download/v0.3.2/libSceAmpr.sprx";
pub(crate) const NG_SHA256: &str =
    "9a8f553a68d4e6e6be11bd9f8d5519b5faa68560761881966abff1dd65bf19f7";

const ARTIFACTS: [&str; 8] = [
    "ampr_assets.index",
    "ampr_assets.index.crc",
    "ampr_assets.index.runtime",
    ampr_index::NAME,
    "ampr_commands.bin",
    "ampr_emu.log",
    "apr_emu.log",
    ps5_dump_forge_zstd::INDEX_NAME,
];

/// Files the job leaves out: what a run of the game wrote.
const LEFT_OUT: [&str; 3] = ["ampr_commands.bin", "ampr_emu.log", "apr_emu.log"];

fn has_suffix(path: &str, suffixes: &[&str]) -> bool {
    let p = path.as_bytes();
    suffixes
        .iter()
        .any(|s| p.len() >= s.len() && p[p.len() - s.len()..].eq_ignore_ascii_case(s.as_bytes()))
}

/// A name the zstd writer puts at the game's root.
fn is_pack_name(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    !p.contains('/')
        && (p == ps5_dump_forge_zstd::INDEX_NAME
            || (p.starts_with(ps5_dump_forge_zstd::VOLUME_PREFIX)
                && p.ends_with(ps5_dump_forge_zstd::VOLUME_SUFFIX)))
}

/// Never packed: loaded by the console or the emulator before or outside the pack layer. Names
/// compare ASCII case-insensitively, because exFAT and PFS images fold case.
pub(crate) fn always_loose(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    let first = path.split('/').next().unwrap_or("").to_ascii_lowercase();
    file.eq_ignore_ascii_case("eboot.bin")
        || has_suffix(path, &[".elf", ".self", ".prx", ".sprx"])
        || (path.contains('/')
            && (first == "sce_sys" || first == "sce_module" || first.starts_with("fakelib")))
        || ARTIFACTS.iter().any(|a| a.eq_ignore_ascii_case(path))
        || is_pack_name(path)
}

/// The game as the packs are made from it: `ampr-emu-ng` and a fresh index in, what a run of the
/// game wrote out.
struct Prepared<'a> {
    inner: &'a mut dyn SourceTree,
    files: Vec<SourceFile>,
    generated: HashMap<String, Vec<u8>>,
}

impl SourceTree for Prepared<'_> {
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
        format!("{} (zstd packs)", self.inner.describe())
    }
}

/// `source` ready for packing, with the files that go into the packs.
fn prepare<'a>(
    source: &'a mut dyn SourceTree,
    runtime: Vec<u8>,
    mtime: i64,
) -> Result<(Prepared<'a>, Vec<SourceFile>), String> {
    let module = emulator_home(&*source);
    let taken: Vec<&str> = source
        .files()
        .iter()
        .map(|f| f.path.as_str())
        .filter(|p| is_pack_name(p))
        .collect();
    if !taken.is_empty() {
        return Err(format!(
            "the game has files named like zstd packs: {}",
            taken.join(", ")
        ));
    }
    let mut files: Vec<SourceFile> = source
        .files()
        .iter()
        .filter(|f| {
            !f.path.eq_ignore_ascii_case(&module)
                && !f.path.eq_ignore_ascii_case(ampr_index::NAME)
                && !LEFT_OUT.iter().any(|n| f.path.eq_ignore_ascii_case(n))
        })
        .cloned()
        .collect();
    files.push(SourceFile {
        path: module.clone(),
        size: runtime.len() as u64,
    });
    let index = ampr_index::build(&ampr_index::listing(&files), mtime)?;
    files.push(SourceFile {
        path: ampr_index::NAME.to_string(),
        size: index.len() as u64,
    });
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let packed = files
        .iter()
        .filter(|f| f.size > 0 && !always_loose(&f.path))
        .cloned()
        .collect();
    let generated = HashMap::from([(module, runtime), (ampr_index::NAME.to_string(), index)]);
    Ok((
        Prepared {
            inner: source,
            files,
            generated,
        },
        packed,
    ))
}

/// A game read for packing: refused unless its eboot reads through AMPR (nothing else could
/// serve packs), a game in LZ4 or zstd packs decoded, so the new packs are made from its files.
/// The image and folder builds open their source through this, and a folder build's read-back
/// opens it again the same way.
pub(crate) fn readable_for_packing(
    mut tree: Box<dyn SourceTree>,
) -> Result<Box<dyn SourceTree>, String> {
    if !ps5upload_fpkg::source::imports_ampr(&mut *tree, "eboot.bin") {
        return Err(
            "this game does not read its files through AMPR, so nothing could serve asset \
             packs; compress it into a .ffpfsc instead"
                .to_string(),
        );
    }
    tree = match ps5upload_fpkg::ampr_pack::image::Lz4Unpacked::open(tree)
        .map_err(|e| format!("reading the game's LZ4 packs: {e}"))?
    {
        ps5upload_fpkg::ampr_pack::image::Unpacking::Decoded(decoded) => decoded,
        ps5upload_fpkg::ampr_pack::image::Unpacking::NotPacked(plain) => plain,
    };
    if ps5_dump_forge_zstd::reader::detect(&*tree) {
        tree = Box::new(
            ps5_dump_forge_zstd::reader::unpack(tree)
                .map_err(|e| format!("reading the game's zstd packs: {e}"))?,
        );
    }
    Ok(tree)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The measure pass (stage `pack`): every packed chunk's codec and size, so the packs can be
/// made on demand from the source.
fn measure(
    prepared: &mut Prepared,
    packed: &[SourceFile],
    level: u8,
    to: &Path,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<ps5_dump_forge_zstd::Index, String> {
    let raw: u64 = packed.iter().map(|f| f.size).sum();
    // Each file counts twice in the writer's progress: one pass counted as two.
    let total = raw.saturating_mul(2);
    stage("pack", 0, total);
    let index = ps5_dump_forge_zstd::writer::measure(
        prepared,
        packed,
        level,
        0,
        &mut |done| stage("pack", done.min(total), total),
        cancel,
    )
    .map_err(|e| e.to_string())?;
    stage("pack", total, total);
    let stored: u64 = index
        .volume_sizes
        .iter()
        .map(|s| s.saturating_sub(ps5_dump_forge_zstd::VOLUME_HEADER_SIZE as u64))
        .sum();
    crate::log_info!(
        "zstd packs: {} files, {raw} bytes to {stored} in {} volumes, level {level} -> {}",
        index.files.len(),
        index.volume_sizes.len(),
        to.display()
    );
    Ok(index)
}

/// Builds `source` as a plain image at `output` with its files in zstd packs at `level`, and
/// `runtime` as its emulator: measured (stage `pack`), then written and read back as any image,
/// then the packs read back through the same checks the emulator makes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build(
    format: ImageFormat,
    level: u8,
    runtime: &PackRuntime,
    source: &mut dyn SourceTree,
    output: &Path,
    mode: VerifyMode,
    verify_chunk: u64,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<ImageBuilt, String> {
    let (mut prepared, packed) = prepare(source, runtime.bytes.clone(), now_secs())?;
    let index = measure(&mut prepared, &packed, level, output, cancel, stage)?;
    let built = {
        let mut tree =
            PackedTree::new(&mut prepared, &index, 0, cancel).map_err(|e| e.to_string())?;
        crate::image_build::build(
            format,
            None,
            &mut tree,
            output,
            mode,
            verify_chunk,
            cancel,
            stage,
        )?
    };
    // What the emulator will check: the index and every volume's header, as the image holds them.
    let image = ps5upload_fpkg::source::open(output).map_err(|e| e.to_string())?;
    if let Err(e) = ps5_dump_forge_zstd::reader::unpack(image) {
        // Finished and named by now, and no use: it doesn't stay looking like a good image.
        let _ = std::fs::remove_file(output);
        return Err(format!("reading the zstd packs back from the image: {e}"));
    }
    Ok(built)
}

/// Replaces the measure pass's random archive ID (what ties the volumes to their index) with
/// one derived from the index itself: the same packs get the same ID, so a stream that starts
/// over in another engine process gives the same bytes and goes on from what the console holds.
fn pin_archive_id(index: &mut ps5_dump_forge_zstd::Index) -> Result<(), String> {
    index.archive_id = [0; 16];
    let bytes = ps5_dump_forge_zstd::encode_index(index).map_err(|e| e.to_string())?;
    let mut h = blake3::Hasher::new();
    h.update(b"ps5upload zstd archive id\0");
    h.update(&bytes);
    index
        .archive_id
        .copy_from_slice(&h.finalize().as_bytes()[..16]);
    Ok(())
}

/// [`build`] straight to the console (`image_build::stream_to_console`): the packs made the
/// same way, with `mtime` pinned on the index so a stream that has to start over gives the same
/// bytes, and the packs read back from the console's copy through the emulator's checks. An
/// image that fails them is removed from the console.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stream_to_console(
    format: ImageFormat,
    level: u8,
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
        prepare(source, runtime.bytes.clone(), mtime).map_err(anyhow::Error::msg)?;
    let mut index = measure(&mut prepared, &packed, level, Path::new(to), cancel, stage)
        .map_err(anyhow::Error::msg)?;
    pin_archive_id(&mut index).map_err(anyhow::Error::msg)?;
    let streamed = {
        let mut tree = PackedTree::new(&mut prepared, &index, 0, cancel)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        // The bytes also follow the level and the emulator: a different one is a different job.
        let salt = format!(
            "zstd level {level} emulator {}",
            blake3::hash(&runtime.bytes).to_hex()
        );
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

/// Builds `source` as a plain folder `target` with its files in zstd packs at `level` and
/// `runtime` as its emulator: what [`build`] puts in an image, as files. Measured (stage
/// `pack`), then the packs made on demand from the source as the folder is written (`write`),
/// then read back (`verify`): the index and every volume header through the emulator's checks,
/// and every file of the game, read through the packs, against the source `reopen` opens
/// afresh (SHA-256, of a sample or all of it as `mode` says). The caller has claimed the
/// folder's `.partial`.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_folder(
    level: u8,
    runtime: &PackRuntime,
    source: &mut dyn SourceTree,
    reopen: &dyn Fn() -> Result<Box<dyn SourceTree>, String>,
    target: &Path,
    mode: VerifyMode,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<crate::folder_build::FolderBuilt, String> {
    build_folder_to(
        level,
        runtime,
        source,
        reopen,
        &crate::folder_build::FolderOut::Local(target),
        mode,
        cancel,
        stage,
    )
}

/// [`build_folder`], to a folder on this computer or on a saved server.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_folder_to(
    level: u8,
    runtime: &PackRuntime,
    source: &mut dyn SourceTree,
    reopen: &dyn Fn() -> Result<Box<dyn SourceTree>, String>,
    out: &crate::folder_build::FolderOut,
    mode: VerifyMode,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<crate::folder_build::FolderBuilt, String> {
    // A PS4 game gets the param.json ShadowMount+ reads, as its folder and image do.
    let mut ps4 = ps5upload_fpkg::ps4_param::Ps4Params::wrap(&mut *source)
        .map_err(|e| format!("param.sfo: {e}"))?;
    let source: &mut dyn SourceTree = match ps4.as_mut() {
        Some(p) => p,
        None => source,
    };
    // The read-back makes the same index, so it keeps this time.
    let mtime = now_secs();
    let (mut prepared, packed) = prepare(source, runtime.bytes.clone(), mtime)?;
    let shown = out.describe();
    let index = measure(
        &mut prepared,
        &packed,
        level,
        Path::new(&shown),
        cancel,
        stage,
    )?;
    let mut tree = PackedTree::new(&mut prepared, &index, 0, cancel).map_err(|e| e.to_string())?;
    crate::folder_build::write_checked_to(
        &mut tree,
        out,
        cancel,
        stage,
        &mut |written, _, _, stage| {
            stage("verify", 0, 0);
            let written: Box<dyn SourceTree> = match written {
                crate::folder_build::Written::Dir(partial) => Box::new(
                    ps5upload_fpkg::source::FolderSource::open(partial)
                        .map_err(|e| format!("the folder could not be read back: {e}"))?,
                ),
                crate::folder_build::Written::Tree(t) => t,
            };
            let mut game = ps5_dump_forge_zstd::reader::unpack(written)
                .map_err(|e| format!("reading the zstd packs back from the folder: {e}"))?;
            let (packs, volumes) = (game.index().files.len(), game.index().volume_sizes.len());
            let mut fresh = reopen()?;
            let mut ps4 = ps5upload_fpkg::ps4_param::Ps4Params::wrap(&mut *fresh)
                .map_err(|e| format!("param.sfo: {e}"))?;
            let fresh: &mut dyn SourceTree = match ps4.as_mut() {
                Some(p) => p,
                None => &mut *fresh,
            };
            let (mut want, _) = prepare(fresh, runtime.bytes.clone(), mtime)?;
            let checked = crate::folder_build::verify_trees(
                &mut game,
                &mut want,
                mode,
                cancel,
                &mut |d, t| stage("verify", d, t),
            )?;
            Ok(format!(
                "{packs} files in {volumes} zstd volumes read back through the packs, {checked}"
            ))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps5upload_fpkg::source::FolderSource;

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

    #[test]
    fn system_files_stay_loose_and_the_rest_is_packed() {
        for p in [
            "eboot.bin",
            "EBOOT.BIN",
            "a/b.sprx",
            "sce_sys/param.json",
            "SCE_MODULE/x",
            "fakelib2/x",
            "ampr_emu.index",
            "ampr_zstd.index",
            "ampr_zstd-000.pak",
        ] {
            assert!(always_loose(p), "{p}");
        }
        for p in ["data/a.bin", "movie.mp4", "root.bin", "sub/sce_sys/x"] {
            assert!(!always_loose(p), "{p}");
        }
    }

    /// Every plain format: the image holds the packs and the emulator, and through the packs
    /// the game reads back as it was.
    #[test]
    fn a_game_reads_back_through_its_packs_in_every_format() {
        let root = std::env::temp_dir().join(format!("ps5upload-zstd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = game(&root);
        for format in [ImageFormat::Exfat, ImageFormat::Ffpkg, ImageFormat::Ffpfs] {
            let out = root.join(crate::image_build::output_names(&src, format, false).0);
            let mut tree = FolderSource::open(&src).unwrap();
            let cancel = AtomicBool::new(false);
            let (mut prepared, packed) = prepare(&mut tree, b"EMULATOR".to_vec(), 7).unwrap();
            assert!(packed.iter().any(|f| f.path == "data/sub/table.bin"));
            assert!(packed.iter().all(|f| f.path != "data/empty.bin"));
            let index = ps5_dump_forge_zstd::writer::measure(
                &mut prepared,
                &packed,
                5,
                0,
                &mut |_| {},
                &cancel,
            )
            .unwrap();
            let mut packs = PackedTree::new(&mut prepared, &index, 0, &cancel).unwrap();
            crate::image_build::build(
                format,
                None,
                &mut packs,
                &out,
                VerifyMode::Full,
                crate::image_build::DEFAULT_VERIFY_CHUNK,
                &cancel,
                &mut |_, _, _| {},
            )
            .unwrap_or_else(|e| panic!("{format:?}: {e}"));
            let packed_size = std::fs::metadata(&out).unwrap().len();
            assert!(packed_size > 0);

            let image = ps5upload_fpkg::source::open(&out).unwrap();
            let names: Vec<String> = image.files().iter().map(|f| f.path.clone()).collect();
            assert!(names.iter().any(|n| n == "ampr_zstd.index"), "{format:?}");
            assert!(names.iter().any(|n| n == "ampr_zstd-000.pak"), "{format:?}");
            assert!(
                !names.iter().any(|n| n == "data/sub/table.bin"),
                "{format:?}"
            );
            let mut game = ps5_dump_forge_zstd::reader::unpack(image).unwrap();
            let want = std::fs::read(src.join("data/sub/table.bin")).unwrap();
            assert_eq!(game.read("data/sub/table.bin").unwrap(), want, "{format:?}");
            assert_eq!(
                game.read_range("data/sub/table.bin", 300_000, 9).unwrap(),
                want[300_000..300_009],
                "{format:?}"
            );
            assert_eq!(game.read("data/settings.json").unwrap(), b"{\"a\":1}");
            assert_eq!(
                game.read("fakelib/libSceAmpr.sprx").unwrap(),
                b"EMULATOR",
                "{format:?}"
            );
            assert!(game.read("ampr_emu.log").is_err(), "{format:?}");
            let _ = std::fs::remove_file(&out);
        }
        let _ = std::fs::remove_dir_all(&root);
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

    fn open_game(dir: &Path) -> Result<Box<dyn SourceTree>, String> {
        let tree = FolderSource::open(dir).map_err(|e| e.to_string())?;
        readable_for_packing(Box::new(tree))
    }

    /// A game folder packed into a folder: packs, loose system files and the emulator on disk;
    /// the folder detects as zstd-packed and every file reads back through the packs as it was.
    /// That folder packed again (its packs decoded first) gives the same game.
    #[test]
    fn a_game_folder_packs_into_a_folder_that_reads_back() {
        let root =
            std::env::temp_dir().join(format!("ps5upload-zstd-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = game(&root);
        let cancel = AtomicBool::new(false);
        let target = root.join("packed");
        let mut stages: Vec<String> = Vec::new();
        let mut tree = open_game(&src).unwrap();
        let built = build_folder(
            5,
            &runtime(),
            &mut *tree,
            &|| open_game(&src),
            &target,
            VerifyMode::Full,
            &cancel,
            &mut |id, _, _| {
                if stages.last().map(String::as_str) != Some(id) {
                    stages.push(id.to_string());
                }
            },
        )
        .unwrap();
        assert_eq!(stages, ["pack", "write", "verify"]);
        assert!(built.detail.contains("zstd volumes"), "{}", built.detail);
        assert!(!crate::folder_build::partial_of(&target).exists());

        // On disk: the packs, what stays loose, the emulator; the packed data and the run's log
        // are not there as files.
        assert!(target.join("ampr_zstd.index").is_file());
        assert!(target.join("ampr_zstd-000.pak").is_file());
        assert!(target.join("ampr_emu.index").is_file());
        assert_eq!(
            std::fs::read(target.join("fakelib/libSceAmpr.sprx")).unwrap(),
            b"EMULATOR"
        );
        assert_eq!(
            std::fs::read(target.join("eboot.bin")).unwrap(),
            std::fs::read(src.join("eboot.bin")).unwrap()
        );
        assert!(target.join("sce_sys/param.json").is_file());
        assert!(!target.join("data/sub/table.bin").exists());
        assert!(!target.join("data/settings.json").exists());
        assert!(!target.join("ampr_emu.log").exists());
        assert!(!target.join(crate::folder_build::MARKER).exists());

        let check = |dir: &Path| {
            let folder = FolderSource::open(dir).unwrap();
            assert!(
                ps5_dump_forge_zstd::reader::detect(&folder),
                "{}",
                dir.display()
            );
            let mut game = ps5_dump_forge_zstd::reader::unpack(Box::new(folder)).unwrap();
            for name in [
                "eboot.bin",
                "sce_sys/param.json",
                "data/sub/table.bin",
                "data/settings.json",
                "data/empty.bin",
            ] {
                assert_eq!(
                    game.read(name).unwrap(),
                    std::fs::read(src.join(name)).unwrap(),
                    "{name} in {}",
                    dir.display()
                );
            }
            assert_eq!(game.read("fakelib/libSceAmpr.sprx").unwrap(), b"EMULATOR");
            assert!(game.read("ampr_emu.log").is_err());
        };
        check(&target);

        // The packed folder as the source: decoded, packed again at another level.
        let again = root.join("packed-again");
        let mut tree = open_game(&target).unwrap();
        build_folder(
            1,
            &runtime(),
            &mut *tree,
            &|| open_game(&target),
            &again,
            VerifyMode::from_full(false, false),
            &cancel,
            &mut |_, _, _| {},
        )
        .unwrap();
        check(&again);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The read-back compares the game read through the packs with the source opened again: a
    /// source that reads differently the second time fails the build and leaves nothing.
    #[test]
    fn a_folder_that_does_not_read_back_as_the_source_is_not_kept() {
        let root =
            std::env::temp_dir().join(format!("ps5upload-zstd-folder-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = game(&root);
        let changed = root.join("changed");
        std::fs::create_dir_all(&changed).unwrap();
        // The same game with one byte of a packed file different.
        let copy = |from: &Path, to: &Path| {
            for f in FolderSource::open(from).unwrap().files() {
                let dest = to.join(&f.path);
                std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                std::fs::copy(from.join(&f.path), dest).unwrap();
            }
        };
        copy(&src, &changed);
        let mut table = std::fs::read(changed.join("data/sub/table.bin")).unwrap();
        table[5] ^= 1;
        std::fs::write(changed.join("data/sub/table.bin"), table).unwrap();
        let target = root.join("packed");
        let mut tree = open_game(&src).unwrap();
        let err = build_folder(
            5,
            &runtime(),
            &mut *tree,
            &|| open_game(&changed),
            &target,
            VerifyMode::Full,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap_err();
        assert!(err.contains("data/sub/table.bin"), "{err}");
        assert!(!target.exists());
        assert!(!crate::folder_build::partial_of(&target).exists());
        let _ = std::fs::remove_dir_all(&root);
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
            let (mut prepared, packed) = prepare(&mut tree, runtime().bytes, 7).unwrap();
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
