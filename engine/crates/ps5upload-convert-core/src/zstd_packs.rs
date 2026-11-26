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
//! The emulator that goes into the game (`runtime`) is given as its bytes: the engine downloads
//! it (`ps5upload-engine`'s `zstd_runtime`), the console program reads it from a file the app
//! put there. Streaming the packs to a console or a server stays in the engine.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use ps5_dump_forge_zstd::tree::PackedTree;
use ps5upload_fpkg::ampr_index;
use ps5upload_fpkg::ampr_pack::image::emulator_home;
use ps5upload_fpkg::ffpfsc_block::Level;
use ps5upload_fpkg::sample::VerifyMode;
use ps5upload_fpkg::source::{SourceFile, SourceTree};

use crate::image_build::{ImageBuilt, ImageFormat};

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

/// Never packed: loaded by the console or the emulator before or outside the pack layer, and
/// `*.ini` (`dlc_emu.ini` and other settings a person edits by hand, as ps5-dump-forge keeps
/// them). Names compare ASCII case-insensitively, because exFAT and PFS images fold case.
pub fn always_loose(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    let first = path.split('/').next().unwrap_or("").to_ascii_lowercase();
    file.eq_ignore_ascii_case("eboot.bin")
        || has_suffix(path, &[".elf", ".self", ".prx", ".sprx", ".ini"])
        || (path.contains('/')
            && (first == "sce_sys" || first == "sce_module" || first.starts_with("fakelib")))
        || ARTIFACTS.iter().any(|a| a.eq_ignore_ascii_case(path))
        || is_pack_name(path)
}

/// A pack profile: drakmor's `ampr_pack` TOML, the one LZ4 packs take, deciding which files
/// stay loose (ps5-dump-forge's `--pack-profile` with `--zstd-pack`, `docs/packing.md`).
pub type Rules = ps5upload_fpkg::ampr_pack::Config;

/// Whether `file` goes into the packs. Without `rules`, every non-empty file but the system
/// files. With them the profile decides, as in ps5-dump-forge (`ps5-dump-forge-lz4/src/rules.rs`
/// `select_everything` and `profile_spec`, from `zstd_pack.rs` `prepare`): the last rule that
/// matches the path, else the profile's `default_action` (`loose` when it names none), and a
/// file is packed unless that says `loose`. Empty files and [`always_loose`] stay loose
/// whatever it says. What a profile sets that zstd packs have no use for is ignored: block
/// size (chunks are always 256 KiB), `hot`, `layout`, groups, the LZ4 level and the auto-loose
/// sampling; a `store` file is packed as any other (a chunk zstd can't shrink is kept raw
/// anyway).
fn is_packed(file: &SourceFile, rules: Option<&Rules>) -> bool {
    use ps5upload_fpkg::ampr_pack::config::Action;
    file.size > 0
        && !always_loose(&file.path)
        && rules.is_none_or(|p| p.select(&file.path).action != Action::Loose)
}

/// The game as the packs are made from it: `ampr-emu-ng` and a fresh index in, what a run of the
/// game wrote out.
pub struct Prepared<'a> {
    inner: &'a mut dyn SourceTree,
    files: Vec<SourceFile>,
    generated: HashMap<String, Vec<u8>>,
}

impl SourceTree for Prepared<'_> {
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
        format!("{} (zstd packs)", self.inner.describe())
    }
}

/// `source` ready for packing, with the files that go into the packs (`rules`: [`is_packed`]).
pub fn prepare<'a>(
    source: &'a mut dyn SourceTree,
    runtime: Vec<u8>,
    mtime: i64,
    rules: Option<&Rules>,
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
    let packed: Vec<SourceFile> = files
        .iter()
        .filter(|f| is_packed(f, rules))
        .cloned()
        .collect();
    if let Some(p) = rules {
        // What the profile kept out of the packs, beyond what always stays loose.
        let kept: Vec<&SourceFile> = files
            .iter()
            .filter(|f| is_packed(f, None) && !is_packed(f, rules))
            .collect();
        crate::log_at!(
            Info,
            "convert",
            "zstd packs: the pack profile ({} rules) keeps {} files loose ({} bytes); {} files \
             go into the packs",
            p.rules.len(),
            kept.len(),
            kept.iter().map(|f| f.size).sum::<u64>(),
            packed.len()
        );
    }
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

/// Why `tree` can't be put in zstd packs at all, or `None`: the packs are read by ampr-emu-ng,
/// a PS5 library ShadowMount+ loads into a PS5 game. A PS4 game (a `param.sfo` and no
/// `param.json`; the one a PS4 image gets is added after this) has no use for it, and without
/// an `eboot.bin` at its root there is no game to load it into.
fn not_packable(tree: &dyn SourceTree) -> Option<&'static str> {
    let has = |name: &str| {
        tree.files()
            .iter()
            .any(|f| f.path.eq_ignore_ascii_case(name))
    };
    if has("sce_sys/param.sfo") && !has("sce_sys/param.json") {
        return Some(
            "this is a PS4 game: zstd asset packs are read by ampr-emu-ng, a PS5 library, so \
             they are for PS5 games only",
        );
    }
    if !has("eboot.bin") {
        return Some("this has no eboot.bin at its root: zstd asset packs are made of a PS5 game");
    }
    None
}

/// A game read for packing: a game in LZ4 or zstd packs decoded, so the new packs are made from
/// its files. A game whose eboot does not import libSceAmpr is packed too, as ps5-dump-forge
/// packs it (`zstd_pack.rs` `prepare`): ampr-emu-ng goes in all the same and ShadowMount+ loads
/// it, so the log says so. The image and folder builds open their source through this, and a
/// folder build's read-back opens it again the same way.
pub fn readable_for_packing(mut tree: Box<dyn SourceTree>) -> Result<Box<dyn SourceTree>, String> {
    if let Some(why) = not_packable(&*tree) {
        return Err(why.to_string());
    }
    if !ps5upload_fpkg::source::imports_ampr(&mut *tree, "eboot.bin") {
        crate::log_at!(
            Info,
            "convert",
            "zstd packs: eboot.bin does not import libSceAmpr; ampr-emu-ng is injected all the \
             same (not tested on every game: keep the original until the packed one has started)"
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

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The measure pass (stage `pack`): every packed chunk's codec and size, so the packs can be
/// made on demand from the source.
pub fn measure(
    prepared: &mut Prepared,
    packed: &[SourceFile],
    level: u8,
    to: &Path,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
    threads: usize,
) -> Result<ps5_dump_forge_zstd::Index, String> {
    let raw: u64 = packed.iter().map(|f| f.size).sum();
    // Each file counts twice in the writer's progress: one pass counted as two.
    let total = raw.saturating_mul(2);
    stage("pack", 0, total);
    let index = ps5_dump_forge_zstd::writer::measure(
        prepared,
        packed,
        level,
        threads,
        &mut |done| stage("pack", done.min(total), total),
        cancel,
    )
    .map_err(|e| e.to_string())?;
    stage("pack", total, total);
    // What ampr-emu-ng checks when it mounts the packs: every packed file has its record in
    // ampr_emu.index at its logical size. Refused here rather than shipped.
    if let Some(emu) = prepared.generated.get(ampr_index::NAME) {
        let files = index
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.logical_size));
        ampr_index::check_zstd(emu, files)?;
    }
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
    // Only now is what the packs come to known: a volume the drive can't hold as one file
    // (FAT32 on the console) is refused before anything is written.
    let largest = index.volume_sizes.iter().copied().max().unwrap_or(0);
    if let Some(why) = crate::safe_write::too_big_for(to, largest, "pack volume") {
        return Err(why);
    }
    Ok(index)
}

/// Builds `source` as a plain image at `output` with its files in zstd packs at `level`, and
/// `runtime` as its emulator: measured (stage `pack`), then written and read back as any image,
/// then the packs read back through the same checks the emulator makes, and every file of the
/// game read through the packs against the source.
/// `threads` compress (0: every core; the console program passes its own limit).
#[allow(clippy::too_many_arguments)]
pub fn build(
    format: ImageFormat,
    level: u8,
    rules: Option<&Rules>,
    runtime: &[u8],
    source: &mut dyn SourceTree,
    output: &Path,
    mode: VerifyMode,
    verify_chunk: u64,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
    threads: usize,
) -> Result<ImageBuilt, String> {
    build_at(
        format,
        None,
        Level::default(),
        level,
        rules,
        runtime,
        source,
        output,
        mode,
        verify_chunk,
        cancel,
        stage,
        threads,
    )
}

/// [`build`], straight into a `.ffpfsc` container when `inner_name` is given (the image inside
/// it named so, compressed at `compression`), as ps5-dump-forge writes zstd packs into any
/// target (`convert.rs`: `--zstd-pack` with `--to ffpfsc`).
#[allow(clippy::too_many_arguments)]
pub fn build_at(
    format: ImageFormat,
    inner_name: Option<&str>,
    compression: Level,
    level: u8,
    rules: Option<&Rules>,
    runtime: &[u8],
    source: &mut dyn SourceTree,
    output: &Path,
    mode: VerifyMode,
    verify_chunk: u64,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
    threads: usize,
) -> Result<ImageBuilt, String> {
    // The read-back makes the same index, so it keeps this time.
    let mtime = now_secs();
    let (mut prepared, packed) = prepare(source, runtime.to_vec(), mtime, rules)?;
    let index = measure(
        &mut prepared,
        &packed,
        level,
        output,
        cancel,
        stage,
        threads,
    )?;
    let (mut built, unnamed) = {
        let mut tree =
            PackedTree::new(&mut prepared, &index, threads, cancel).map_err(|e| e.to_string())?;
        crate::image_build::build_unnamed(
            format,
            inner_name,
            compression,
            &mut tree,
            output,
            mode,
            verify_chunk,
            cancel,
            stage,
        )?
    };
    drop(prepared);
    // What the emulator will check: the index and every volume's header, as the image holds
    // them; then the game read through them. Still at its `.partial`: an image that fails this
    // (or a crash during it) never looks like a good one under its name.
    let checked = (|| {
        let image =
            crate::image_build::open_image(unnamed.partial(), format, inner_name.is_some())?;
        let mut game = ps5_dump_forge_zstd::reader::unpack(image)
            .map_err(|e| format!("reading the zstd packs back from the image: {e}"))?;
        let (packs, volumes) = (game.index().files.len(), game.index().volume_sizes.len());
        let (mut want, _) = prepare(source, runtime.to_vec(), mtime, rules)?;
        let checked =
            crate::folder_build::verify_trees(&mut game, &mut want, mode, cancel, &mut |d, t| {
                stage("verify", d, t)
            })
            .map_err(|e| format!("reading the zstd packs back from the image: {e}"))?;
        Ok::<_, String>(format!(
            ", {packs} files in {volumes} zstd volumes read back through the packs, {checked}"
        ))
    })();
    // Dropped unnamed on a failure: its partial file is removed.
    built.detail.push_str(&checked?);
    unnamed.name()?;
    Ok(built)
}

/// Replaces the measure pass's random archive ID (what ties the volumes to their index) with
/// one derived from the index itself: the same packs get the same ID, so a stream that starts
/// over in another engine process gives the same bytes and goes on from what the console holds.
pub fn pin_archive_id(index: &mut ps5_dump_forge_zstd::Index) -> Result<(), String> {
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

/// Builds `source` as a plain folder `target` with its files in zstd packs at `level` and
/// `runtime` as its emulator: what [`build`] puts in an image, as files. Measured (stage
/// `pack`), then the packs made on demand from the source as the folder is written (`write`),
/// then read back (`verify`): the index and every volume header through the emulator's checks,
/// and every file of the game, read through the packs, against the source `reopen` opens
/// afresh (SHA-256, of a sample or all of it as `mode` says). The caller has claimed the
/// folder's `.partial`. `threads` compress (0: every core; the console program passes its own limit).
#[allow(clippy::too_many_arguments)]
pub fn build_folder(
    level: u8,
    rules: Option<&Rules>,
    runtime: &[u8],
    source: &mut dyn SourceTree,
    reopen: &dyn Fn() -> Result<Box<dyn SourceTree>, String>,
    target: &Path,
    mode: VerifyMode,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
    threads: usize,
) -> Result<crate::folder_build::FolderBuilt, String> {
    build_folder_to(
        level,
        rules,
        runtime,
        source,
        reopen,
        &crate::folder_build::FolderOut::Local(target),
        mode,
        cancel,
        stage,
        threads,
    )
}

/// [`build_folder`], to a folder on this computer or on a saved server.
#[allow(clippy::too_many_arguments)]
pub fn build_folder_to(
    level: u8,
    rules: Option<&Rules>,
    runtime: &[u8],
    source: &mut dyn SourceTree,
    reopen: &dyn Fn() -> Result<Box<dyn SourceTree>, String>,
    out: &crate::folder_build::FolderOut,
    mode: VerifyMode,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
    threads: usize,
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
    let (mut prepared, packed) = prepare(source, runtime.to_vec(), mtime, rules)?;
    let shown = out.describe();
    let index = measure(
        &mut prepared,
        &packed,
        level,
        Path::new(&shown),
        cancel,
        stage,
        threads,
    )?;
    let mut tree =
        PackedTree::new(&mut prepared, &index, threads, cancel).map_err(|e| e.to_string())?;
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
            let (mut want, _) = prepare(fresh, runtime.to_vec(), mtime, rules)?;
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
            // Settings a person edits by hand stay files.
            "dlc_emu.ini",
            "Config/Game.INI",
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
        let root = crate::test_tmp::temp_path(format!("ps5upload-zstd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = game(&root);
        for format in [ImageFormat::Exfat, ImageFormat::Ffpkg, ImageFormat::Ffpfs] {
            let out = root.join(crate::image_build::output_names(&src, format, false).0);
            let mut tree = FolderSource::open(&src).unwrap();
            let cancel = AtomicBool::new(false);
            let (mut prepared, packed) = prepare(&mut tree, b"EMULATOR".to_vec(), 7, None).unwrap();
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

    fn runtime() -> Vec<u8> {
        b"EMULATOR".to_vec()
    }

    /// The measure refuses packs whose files ampr_emu.index does not list at their logical
    /// size (the runtime would serve none of them), before anything is written; the index
    /// `prepare` makes always matches.
    #[test]
    fn packs_that_disagree_with_ampr_emu_index_are_refused() {
        let root =
            crate::test_tmp::temp_path(format!("ps5upload-zstd-join-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = game(&root);
        let mut tree = FolderSource::open(&src).unwrap();
        let cancel = AtomicBool::new(false);
        let (mut prepared, packed) = prepare(&mut tree, runtime(), 5, None).unwrap();
        let index = measure(
            &mut prepared,
            &packed,
            1,
            &root,
            &cancel,
            &mut |_, _, _| {},
            1,
        );
        assert!(index.is_ok(), "{:?}", index.err());
        // The same game, its index listing the packed table at another size.
        let listed: Vec<(String, u64)> = prepared
            .files
            .iter()
            .filter(|f| f.path != ampr_index::NAME)
            .map(|f| {
                let grown = f.path == "data/sub/table.bin";
                (f.path.clone(), f.size + u64::from(grown))
            })
            .collect();
        let stale = ampr_index::build(&listed, 5).unwrap();
        prepared
            .generated
            .insert(ampr_index::NAME.to_string(), stale);
        let e = measure(
            &mut prepared,
            &packed,
            1,
            &root,
            &cancel,
            &mut |_, _, _| {},
            1,
        )
        .unwrap_err();
        assert!(
            e.contains("data/sub/table.bin") && e.contains("serves no packed file"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// zstd packs go into every image, plain and inside a `.ffpfsc`, through [`build_at`]: each
    /// is read back through its filesystem (and container), then through the packs against the
    /// source; the result opens as the game it was.
    #[test]
    fn zstd_packs_go_into_every_image_plain_and_compressed() {
        let root = crate::test_tmp::temp_path(format!("ps5upload-zstdall-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = game(&root);
        for format in [ImageFormat::Exfat, ImageFormat::Ffpkg, ImageFormat::Ffpfs] {
            for compress in [false, true] {
                let (name, inner) = crate::image_build::output_names(&src, format, compress);
                let out = root.join(&name);
                let mut tree = FolderSource::open(&src).unwrap();
                let mut stages: Vec<String> = Vec::new();
                // The image gets its name only after the packs read back: never during a check.
                let mut named_early = false;
                let built = build_at(
                    format,
                    inner.as_deref(),
                    Level::Fast,
                    5,
                    None,
                    &runtime(),
                    &mut tree,
                    &out,
                    VerifyMode::Full,
                    crate::image_build::DEFAULT_VERIFY_CHUNK,
                    &AtomicBool::new(false),
                    &mut |s, _, _| {
                        named_early |= s == "verify" && out.exists();
                        if stages.last().map(String::as_str) != Some(s) {
                            stages.push(s.to_string());
                        }
                    },
                    0,
                )
                .unwrap_or_else(|e| panic!("{format:?} compress={compress}: {e}"));
                let write = if compress { "compress" } else { "write" };
                assert_eq!(stages, ["pack", "plan", write, "verify"], "{format:?}");
                assert!(!named_early, "{format:?} compress={compress}");
                assert!(!root.join(format!("{name}.partial")).exists());
                assert!(
                    built.detail.contains("read back through the packs"),
                    "{}",
                    built.detail
                );
                let image = ps5upload_fpkg::source::open(&out).unwrap();
                let mut game = ps5_dump_forge_zstd::reader::unpack(image).unwrap();
                let want = std::fs::read(src.join("data/sub/table.bin")).unwrap();
                assert_eq!(game.read("data/sub/table.bin").unwrap(), want, "{format:?}");
                assert_eq!(game.read("fakelib/libSceAmpr.sprx").unwrap(), b"EMULATOR");
                let _ = std::fs::remove_file(&out);
            }
        }
        let _ = std::fs::remove_dir_all(&root);
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
            crate::test_tmp::temp_path(format!("ps5upload-zstd-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = game(&root);
        let cancel = AtomicBool::new(false);
        let target = root.join("packed");
        let mut stages: Vec<String> = Vec::new();
        let mut tree = open_game(&src).unwrap();
        let built = build_folder(
            5,
            None,
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
            0,
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
            None,
            &runtime(),
            &mut *tree,
            &|| open_game(&target),
            &again,
            VerifyMode::from_full(false, false),
            &cancel,
            &mut |_, _, _| {},
            0,
        )
        .unwrap();
        check(&again);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A profile decides what stays loose: its `**/*.bik` rule leaves the videos out of the
    /// packs, as plain files in the folder and still listed in `ampr_emu.index`; the rest is
    /// packed as the profile's default says, and the system files stay loose whatever it says.
    #[test]
    fn a_profile_rule_keeps_its_files_loose() {
        let root =
            crate::test_tmp::temp_path(format!("ps5upload-zstd-rules-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = game(&root);
        std::fs::create_dir_all(src.join("movies")).unwrap();
        std::fs::write(src.join("movies/intro.bik"), vec![7u8; 5000]).unwrap();
        std::fs::write(src.join("data/sub/cut.bik"), vec![9u8; 3000]).unwrap();
        // LZ4-only keys (block size, hot, layout) are taken and have no effect here; the last
        // rule would pack the eboot and sce_sys, which never go into packs.
        let rules = Rules::from_toml(
            "[pack]\ndefault_action = \"compress\"\ndefault_block_size = \"128KiB\"\n\
             [[rule]]\naction = \"loose\"\ninclude = [\"**/*.bik\"]\nhot = true\n\
             layout = \"random\"\n\
             [[rule]]\naction = \"compress\"\ninclude = [\"eboot.bin\", \"sce_sys/**\"]\n",
            None,
        )
        .unwrap();
        {
            let mut tree = FolderSource::open(&src).unwrap();
            let (_, packed) = prepare(&mut tree, b"EMULATOR".to_vec(), 7, Some(&rules)).unwrap();
            let names: Vec<&str> = packed.iter().map(|f| f.path.as_str()).collect();
            assert_eq!(names, ["data/settings.json", "data/sub/table.bin"]);
        }
        // A profile with no default keeps what no rule matches loose, as the format says.
        {
            let only = Rules::from_toml(
                "[[rule]]\naction = \"compress\"\ninclude = [\"data/sub/**\"]\n",
                None,
            )
            .unwrap();
            let mut tree = FolderSource::open(&src).unwrap();
            let (_, packed) = prepare(&mut tree, b"EMULATOR".to_vec(), 7, Some(&only)).unwrap();
            let names: Vec<&str> = packed.iter().map(|f| f.path.as_str()).collect();
            assert_eq!(names, ["data/sub/cut.bik", "data/sub/table.bin"]);
        }
        let target = root.join("packed");
        let mut tree = open_game(&src).unwrap();
        build_folder(
            5,
            Some(&rules),
            &runtime(),
            &mut *tree,
            &|| open_game(&src),
            &target,
            VerifyMode::Full,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
            0,
        )
        .unwrap();
        // Loose on disk, byte for byte; the packed file is not.
        assert_eq!(
            std::fs::read(target.join("movies/intro.bik")).unwrap(),
            vec![7u8; 5000]
        );
        assert_eq!(
            std::fs::read(target.join("data/sub/cut.bik")).unwrap(),
            vec![9u8; 3000]
        );
        assert!(!target.join("data/sub/table.bin").exists());
        let folder = FolderSource::open(&target).unwrap();
        let game = ps5_dump_forge_zstd::reader::unpack(Box::new(folder)).unwrap();
        let in_packs: Vec<&str> = game.index().files.iter().map(|f| f.path.as_str()).collect();
        assert!(in_packs.contains(&"data/sub/table.bin"), "{in_packs:?}");
        assert!(
            !in_packs.iter().any(|p| p.ends_with(".bik")),
            "{in_packs:?}"
        );
        // ampr_emu.index lists them like every other file, so AMPR path ids still find them.
        let listed =
            ampr_index::parse(&std::fs::read(target.join(ampr_index::NAME)).unwrap()).unwrap();
        for want in ["movies/intro.bik", "data/sub/cut.bik", "data/sub/table.bin"] {
            assert!(listed.iter().any(|e| e.path == want), "{want}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A game whose eboot doesn't import libSceAmpr is packed like any other: the packs, the
    /// emulator and an index listing every file, and it reads back as it was. A PS4 game and
    /// a folder with no eboot are refused.
    #[test]
    fn a_game_that_does_not_import_ampr_is_packed_too() {
        let root =
            crate::test_tmp::temp_path(format!("ps5upload-zstd-noampr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = game(&root);
        std::fs::write(src.join("eboot.bin"), vec![0u8; 200_000]).unwrap();
        {
            let mut plain = FolderSource::open(&src).unwrap();
            assert!(!ps5upload_fpkg::source::imports_ampr(
                &mut plain,
                "eboot.bin"
            ));
        }
        let target = root.join("packed");
        let mut tree = open_game(&src).unwrap();
        build_folder(
            5,
            None,
            &runtime(),
            &mut *tree,
            &|| open_game(&src),
            &target,
            VerifyMode::Full,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
            0,
        )
        .unwrap();
        assert!(target.join("ampr_zstd-000.pak").is_file());
        assert!(!target.join("data/sub/table.bin").exists());
        assert_eq!(
            std::fs::read(target.join("fakelib/libSceAmpr.sprx")).unwrap(),
            b"EMULATOR"
        );
        // Nothing else is written for it: the folder holds the loose files, the emulator, the
        // two indexes and the volume.
        let mut names: Vec<String> = FolderSource::open(&target)
            .unwrap()
            .files()
            .iter()
            .map(|f| f.path.clone())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "ampr_emu.index",
                "ampr_zstd-000.pak",
                "ampr_zstd.index",
                "data/empty.bin",
                "eboot.bin",
                "fakelib/libSceAmpr.sprx",
                "sce_sys/param.json",
            ]
        );
        let listed =
            ampr_index::parse(&std::fs::read(target.join(ampr_index::NAME)).unwrap()).unwrap();
        assert!(listed.iter().any(|e| e.path == "data/sub/table.bin"));
        let folder = FolderSource::open(&target).unwrap();
        let mut game = ps5_dump_forge_zstd::reader::unpack(Box::new(folder)).unwrap();
        assert_eq!(
            game.read("data/sub/table.bin").unwrap(),
            std::fs::read(src.join("data/sub/table.bin")).unwrap()
        );

        // A PS4 game (param.sfo, no param.json) and a folder with no eboot are not packed.
        let ps4 = root.join("CUSA00001-app");
        std::fs::create_dir_all(ps4.join("sce_sys")).unwrap();
        std::fs::write(ps4.join("eboot.bin"), b"x").unwrap();
        std::fs::write(ps4.join("sce_sys/param.sfo"), b"x").unwrap();
        let err = open_game(&ps4).err().unwrap();
        assert!(err.contains("PS4 game"), "{err}");
        std::fs::remove_file(src.join("eboot.bin")).unwrap();
        let err = open_game(&src).err().unwrap();
        assert!(err.contains("no eboot.bin"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The read-back compares the game read through the packs with the source opened again: a
    /// source that reads differently the second time fails the build and leaves nothing.
    #[test]
    fn a_folder_that_does_not_read_back_as_the_source_is_not_kept() {
        let root =
            crate::test_tmp::temp_path(format!("ps5upload-zstd-folder-bad-{}", std::process::id()));
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
            None,
            &runtime(),
            &mut *tree,
            &|| open_game(&changed),
            &target,
            VerifyMode::Full,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
            0,
        )
        .unwrap_err();
        assert!(err.contains("data/sub/table.bin"), "{err}");
        assert!(!target.exists());
        assert!(!crate::folder_build::partial_of(&target).exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
