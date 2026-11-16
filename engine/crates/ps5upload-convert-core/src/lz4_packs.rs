//! AMPR LZ4 asset packs (drakmor's format, `ps5upload_fpkg::ampr_pack`) in every output: a
//! plain `.exfat`, `.ffpkg` or `.ffpfs` image, any of them inside a `.ffpfsc`, and a plain
//! folder, as quer3q/ps5-dump-forge (GPL-3.0-or-later) offers them (`ps5-dump-forge-core/src/
//! convert.rs`: `--lz4-pack` with `--to folder|exfat|ffpkg|ffpfs|ffpfsc`). The engine streams
//! the same packed game to a console or a server.
//!
//! The volumes are packed into a spool folder first ([`spool`]), and the output is written from
//! the packed game that spool gives ([`Overlay`]). An image streamed to a console or a server
//! takes no spool ([`measured`]): as Forge does, a measure pass places every chunk (identical
//! blocks stored once per lane, chunks placed by lane and I/O page), and the manifest it gives
//! is that layout, so each chunk is compressed again into its measured place as the volume is
//! read, checked against the measure (`ampr_pack::regen`).
//!
//! Whatever the output, it is read back twice: as written (the image's or folder's own check
//! against the packed game), then through the packs ([`read_back`]): every file of the game
//! decoded from the volumes, CRC-checked, and hashed against the source.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use ps5upload_fpkg::ampr_index;
use ps5upload_fpkg::ampr_pack::image::{replaced_by_packing, Lz4Unpacked, Overlay, Unpacking};
use ps5upload_fpkg::ampr_pack::{self, Config, Output};
use ps5upload_fpkg::sample::VerifyMode;
use ps5upload_fpkg::source::{SourceFile, SourceTree};

/// AMPR LZ4 asset packs for an image or folder (see `ps5upload_fpkg::ampr_pack`): the game's
/// files packed into seekable LZ4 volumes that a backport's `ampr_emu` serves, the originals
/// left out.
pub struct Lz4Packs {
    /// drakmor's profile format; the built-in profile (from `level` and `block_shift`) when
    /// absent.
    pub profile: Option<Config>,
    /// 1 (fastest) to 12 (smallest).
    pub level: u8,
    /// log2 of the block size, 14 (16 KiB) to 20 (1 MiB).
    pub block_shift: u8,
}

/// The profile an LZ4 build packs with, and its name for the log: a measured or user-given
/// profile always wins; otherwise the game's engine picks a built-in one.
pub(crate) fn lz4_profile(packs: &Lz4Packs, files: &[SourceFile]) -> (&'static str, Config) {
    match &packs.profile {
        Some(given) => ("given", given.clone()),
        None => (
            ampr_pack::default_profile_name(files.iter().map(|f| f.path.as_str())),
            ampr_pack::default_profile_for(
                files.iter().map(|f| (f.path.as_str(), f.size)),
                packs.level,
                packs.block_shift,
            ),
        ),
    }
}

/// The spool an LZ4 build packs into, next to the image or folder it is for.
pub fn lz4_spool(output: &Path) -> PathBuf {
    PathBuf::from(format!("{}.lz4spool", output.display()))
}

/// A game packed into a spool: the packed game an output is written from, what packing did, and
/// the profile it packed with.
pub struct Spooled<'a> {
    pub tree: Overlay<'a>,
    pub report: Output,
    pub config: Config,
}

/// Packs `source` into `spool` (which must not exist yet; the caller has claimed it) by the
/// profile `packs` gives, every file listed in the new `ampr_emu.index` at `mtime`. Stage
/// `pack`. Refused, with the reason, for a game whose ampr_emu can't serve packs. `to` names the
/// output, for the log.
pub fn spool<'a>(
    source: &'a mut dyn SourceTree,
    packs: &Lz4Packs,
    spool: &Path,
    mtime: i64,
    to: &str,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<Spooled<'a>, String> {
    pack_game(source, packs, Some(spool), mtime, to, cancel, stage)
}

/// [`spool`] with nothing written on this computer: the packs are measured (stage `pack`), and
/// the packed game's volumes are made again from `source` as the output reads them
/// (`ampr_pack::image::prepare_streamed`), the same bytes a spool holds, every chunk checked
/// against the measure; a read that cannot make one as measured fails
/// (`ampr_pack::regen::not_made_again` says so). Twice the compression.
pub fn measured<'a>(
    source: &'a mut dyn SourceTree,
    packs: &Lz4Packs,
    mtime: i64,
    to: &str,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<Spooled<'a>, String> {
    pack_game(source, packs, None, mtime, to, cancel, stage)
}

/// [`spool`] into `spool`, or [`measured`] without one.
fn pack_game<'a>(
    source: &'a mut dyn SourceTree,
    packs: &Lz4Packs,
    spool: Option<&Path>,
    mtime: i64,
    to: &str,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<Spooled<'a>, String> {
    if let Some(why) = ampr_pack::image::refusal(source) {
        return Err(why);
    }
    let (profile_name, config) = lz4_profile(packs, source.files());
    crate::log_at!(
        Info,
        "convert",
        "lz4 packs: profile={profile_name} chosen for {} files ({} rules) -> {to}",
        source.files().len(),
        config.rules.len(),
    );
    stage("pack", 0, 0);
    let prepared = {
        let mut progress = |done: u64, total: u64| stage("pack", done, total);
        let mut control = ampr_pack::Control {
            progress: Some(&mut progress),
            cancel: Some(cancel),
        };
        match spool {
            Some(spool) => ampr_pack::image::prepare(source, &config, spool, mtime, &mut control),
            None => ampr_pack::image::prepare_streamed(source, &config, mtime, &mut control),
        }
    };
    let (tree, report) = prepared.map_err(|e| e.to_string())?;
    let s = &report.stats;
    if s.files_packed == 0 {
        crate::log_at!(
            Warn,
            "convert",
            "lz4 packs: profile={profile_name} packed no files ({} loose) for {to}",
            s.files_loose,
        );
    } else {
        crate::log_at!(
            Info,
            "convert",
            "lz4 packs: profile={profile_name} packed {} files ({} loose) for {to}",
            s.files_packed,
            s.files_loose,
        );
    }
    Ok(Spooled {
        tree,
        report,
        config,
    })
}

/// What a finished build's detail says of its packs.
pub fn detail(report: &Output) -> String {
    let s = &report.stats;
    format!(
        ", LZ4 packs: {} files packed into {} volume(s) ({} -> {} bytes, {} LZ4 / {} raw \
         chunks), {} loose",
        s.files_packed,
        report.volumes.len(),
        s.logical_bytes,
        s.stored_bytes,
        s.chunks_lz4,
        s.chunks_raw,
        s.files_loose
    )
}

/// What else a streamed image's bytes depend on, for its job id: the pack set's build ID (a hash
/// of the profile and every record), so a stream begun with other packs never goes on from them.
pub fn salt(report: &Output) -> String {
    let id: String = report.build_id.iter().map(|b| format!("{b:02x}")).collect();
    format!("lz4 build {id}")
}

/// `tree` with only the files `keep` takes, and no empty folders.
struct Only<'a> {
    inner: &'a mut dyn SourceTree,
    files: Vec<SourceFile>,
}

impl<'a> Only<'a> {
    fn new(inner: &'a mut dyn SourceTree, keep: impl Fn(&str) -> bool) -> Self {
        let files = inner
            .files()
            .iter()
            .filter(|f| keep(&f.path))
            .cloned()
            .collect();
        Self { inner, files }
    }
}

impl SourceTree for Only<'_> {
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
    fn describe(&self) -> String {
        self.inner.describe()
    }
}

/// The read-back through the packs: `written` (the image or folder as it was written, opened
/// afresh) read as the game it holds, every packed file decoded chunk by chunk from the volumes
/// (each chunk's CRC checked), must hold exactly the files of `source` that packing by `config`
/// keeps, at their sizes, and the SHA-256 of what `mode` samples (or of every byte) must match.
/// `progress(done, total)` as `folder_build::verify_trees` reports it.
pub fn read_back(
    written: Box<dyn SourceTree>,
    source: &mut dyn SourceTree,
    config: &Config,
    mode: VerifyMode,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<String, String> {
    let mut game =
        match Lz4Unpacked::open(written).map_err(|e| format!("reading the LZ4 packs back: {e}"))? {
            Unpacking::Decoded(game) => game,
            Unpacking::NotPacked(_) => {
                return Err(format!(
                    "reading the LZ4 packs back: no {} was written",
                    config.index_name
                ))
            }
        };
    let mut got = Only::new(&mut *game, |p| !p.eq_ignore_ascii_case(ampr_index::NAME));
    let mut want = Only::new(source, |p| !replaced_by_packing(p, config));
    let files = want.files().len();
    let checked = crate::folder_build::verify_trees(&mut got, &mut want, mode, cancel, progress)
        .map_err(|e| format!("reading the LZ4 packs back: {e}"))?;
    Ok(format!(
        "{files} files read back through the LZ4 packs, {checked}"
    ))
}

/// Builds `source` as a plain folder with its files in LZ4 packs: packed into `spool` (stage
/// `pack`; the caller has claimed it, and it is removed here whatever happens), written as
/// `folder_build::write_checked_to` writes any folder (`write`), then read back (`verify`): the
/// manifest, its sidecars and the new `ampr_emu.index` byte for byte, then every file of the
/// game through the packs against the source `reopen` opens afresh ([`read_back`]).
#[allow(clippy::too_many_arguments)]
pub fn build_folder_to(
    packs: &Lz4Packs,
    source: &mut dyn SourceTree,
    reopen: &dyn Fn() -> Result<Box<dyn SourceTree>, String>,
    out: &crate::folder_build::FolderOut,
    spool_dir: &Path,
    mode: VerifyMode,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<crate::folder_build::FolderBuilt, String> {
    let mtime = crate::zstd_packs::now_secs();
    let shown = out.describe();
    let result = (|| {
        let Spooled {
            mut tree,
            report,
            config,
        } = spool(source, packs, spool_dir, mtime, &shown, cancel, stage)?;
        // What packing made in memory, checked byte for byte where it lands.
        let mut made: Vec<(String, Vec<u8>)> = report
            .small_files()
            .into_iter()
            .map(|(name, bytes)| (name, bytes.to_vec()))
            .collect();
        made.push((
            ampr_index::NAME.to_string(),
            tree.read(ampr_index::NAME).map_err(|e| e.to_string())?,
        ));
        let mut built = crate::folder_build::write_checked_to(
            &mut tree,
            out,
            cancel,
            stage,
            &mut |written, _, _, stage| {
                stage("verify", 0, 0);
                let mut written: Box<dyn SourceTree> = match written {
                    crate::folder_build::Written::Dir(partial) => Box::new(
                        ps5upload_fpkg::source::FolderSource::open(partial)
                            .map_err(|e| format!("the folder could not be read back: {e}"))?,
                    ),
                    crate::folder_build::Written::Tree(t) => t,
                };
                for (name, bytes) in &made {
                    let got = written
                        .read(name)
                        .map_err(|e| format!("{name} did not read back: {e}"))?;
                    if &got != bytes {
                        return Err(format!(
                            "{name} reads back differently from what was packed"
                        ));
                    }
                }
                let mut fresh = reopen()?;
                read_back(written, &mut *fresh, &config, mode, cancel, &mut |d, t| {
                    stage("verify", d, t)
                })
            },
        )?;
        built.detail.push_str(&detail(&report));
        Ok(built)
    })();
    let _ = std::fs::remove_dir_all(spool_dir);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_build::{build_packed, ImageFormat, DEFAULT_VERIFY_CHUNK};
    use ps5upload_fpkg::source::FolderSource;

    /// A backported AMPR title whose data the folder-wide profile packs.
    fn ampr_game(root: &Path) -> PathBuf {
        let src = root.join("PPSA99998-app");
        std::fs::create_dir_all(src.join("sce_sys")).unwrap();
        std::fs::create_dir_all(src.join("data/only_packed")).unwrap();
        std::fs::create_dir_all(src.join("fakelib")).unwrap();
        let mut eboot: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        eboot[1000..1014].copy_from_slice(b"libSceAmpr.prx");
        std::fs::write(src.join("eboot.bin"), eboot).unwrap();
        std::fs::write(
            src.join("sce_sys/param.json"),
            b"{\"titleId\":\"PPSA99998\"}",
        )
        .unwrap();
        let mut module = vec![0u8; 4096];
        module[100..108].copy_from_slice(b"AMPRPAK4");
        module[200..217].copy_from_slice(b"ampr_assets.index");
        module[300..321].copy_from_slice(b"\x000.4.2.1 (c) Drakmor\0");
        std::fs::write(src.join("fakelib/libSceAmpr.sprx"), module).unwrap();
        let text: Vec<u8> = (0..400_000u32)
            .flat_map(|i| format!("asset {} ", i % 977).into_bytes())
            .take(400_000)
            .collect();
        std::fs::write(src.join("data/only_packed/table.bin"), &text).unwrap();
        std::fs::write(src.join("data/text.bin"), &text[..150_000]).unwrap();
        std::fs::write(src.join("data/settings.json"), b"{\"a\":1}").unwrap();
        // What a run of the game wrote: left out of the packed game.
        std::fs::write(src.join("ampr_commands.bin"), b"from a run").unwrap();
        src
    }

    fn packs() -> Lz4Packs {
        Lz4Packs {
            // The folder-wide profile, so the small fixture files are packed.
            profile: Some(ampr_pack::default_profile(9, 16)),
            level: 9,
            block_shift: 16,
        }
    }

    /// The game read back through the packs of `written` is the source: every file decoded.
    fn round_trip(written: Box<dyn SourceTree>, src: &Path) {
        let Unpacking::Decoded(mut game) = Lz4Unpacked::open(written).unwrap() else {
            panic!("no pack set");
        };
        let mut source = FolderSource::open(src).unwrap();
        let mut packed = 0;
        for f in source.files().to_vec() {
            if f.path == "ampr_commands.bin" {
                assert!(!game.files().iter().any(|g| g.path == f.path));
                continue;
            }
            let listed = game.files().iter().find(|g| g.path == f.path);
            assert_eq!(listed.map(|g| g.size), Some(f.size), "{}", f.path);
            assert_eq!(
                game.read(&f.path).unwrap(),
                source.read(&f.path).unwrap(),
                "{}",
                f.path
            );
            if f.path.starts_with("data/") && f.path != "data/settings.json" {
                packed += 1;
            }
        }
        assert_eq!(packed, 2);
        assert!(game.files().iter().any(|g| g.path == ampr_index::NAME));
    }

    /// LZ4 packs go into every image, plain and inside a `.ffpfsc`: each reads back through its
    /// filesystem (and container) and then through the packs as the game it was.
    #[test]
    fn lz4_packs_go_into_every_image_plain_and_compressed() {
        let root = std::env::temp_dir().join(format!("ps5upload-lz4all-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = ampr_game(&root);
        for format in [ImageFormat::Exfat, ImageFormat::Ffpkg, ImageFormat::Ffpfs] {
            for compress in [false, true] {
                let (name, inner) = crate::image_build::output_names(&src, format, compress);
                let out = root.join(&name);
                let mut tree = FolderSource::open(&src).unwrap();
                let mut stages: Vec<String> = Vec::new();
                let built = build_packed(
                    format,
                    inner.as_deref(),
                    ps5upload_fpkg::ffpfsc_block::Level::Fast,
                    &mut tree,
                    &out,
                    &packs(),
                    VerifyMode::Full,
                    DEFAULT_VERIFY_CHUNK,
                    &AtomicBool::new(false),
                    &mut |s, _, _| {
                        if stages.last().map(String::as_str) != Some(s) {
                            stages.push(s.to_string());
                        }
                    },
                )
                .unwrap_or_else(|e| panic!("{format:?} compress={compress}: {e}"));
                let write = if compress { "compress" } else { "write" };
                assert_eq!(stages, ["pack", "plan", write, "verify"], "{format:?}");
                assert!(built.detail.contains("LZ4 packs"), "{}", built.detail);
                assert!(
                    built.detail.contains("read back through the LZ4 packs"),
                    "{}",
                    built.detail
                );
                assert!(!lz4_spool(&out).exists(), "the spool is removed");
                let image = crate::image_build::open_image(&out, format, compress).unwrap();
                round_trip(image, &src);
                let _ = std::fs::remove_file(&out);
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The same game and profile pack to the same bytes: what lets a stream to a console or
    /// server that starts over in another engine process go on from what is already there.
    #[test]
    fn packing_again_gives_the_same_bytes() {
        let root = std::env::temp_dir().join(format!("ps5upload-lz4same-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = ampr_game(&root);
        let cancel = AtomicBool::new(false);
        let mut sets = Vec::new();
        for n in 0..2 {
            let mut tree = FolderSource::open(&src).unwrap();
            let dir = root.join(format!("spool{n}"));
            let s = spool(
                &mut tree,
                &packs(),
                &dir,
                7,
                "test",
                &cancel,
                &mut |_, _, _| {},
            )
            .unwrap();
            let salt = salt(&s.report);
            let mut tree = s.tree;
            let mut files: Vec<(String, Vec<u8>)> = tree
                .files()
                .to_vec()
                .into_iter()
                .map(|f| {
                    let bytes = tree.read(&f.path).unwrap();
                    (f.path, bytes)
                })
                .collect();
            files.sort();
            sets.push((salt, files));
        }
        assert!(sets[0] == sets[1], "two packings of one game differ");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A folder with LZ4 packs: the packs, the loose files and the new index, read back through
    /// the packs; the spool is gone and a damaged volume fails the read-back.
    #[test]
    fn an_lz4_packed_folder_reads_back_to_the_source() {
        let root = std::env::temp_dir().join(format!("ps5upload-lz4dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = ampr_game(&root);
        let target = root.join("out/PPSA99998-app");
        std::fs::create_dir_all(root.join("out")).unwrap();
        let spool_dir = lz4_spool(&target);
        let mut tree = FolderSource::open(&src).unwrap();
        let reopen = || -> Result<Box<dyn SourceTree>, String> {
            Ok(Box::new(
                FolderSource::open(&src).map_err(|e| e.to_string())?,
            ))
        };
        let mut stages: Vec<String> = Vec::new();
        let built = build_folder_to(
            &packs(),
            &mut tree,
            &reopen,
            &crate::folder_build::FolderOut::Local(&target),
            &spool_dir,
            VerifyMode::Full,
            &AtomicBool::new(false),
            &mut |s, _, _| {
                if stages.last().map(String::as_str) != Some(s) {
                    stages.push(s.to_string());
                }
            },
        )
        .unwrap();
        assert_eq!(stages, ["pack", "write", "verify"]);
        assert!(built.detail.contains("LZ4 packs"), "{}", built.detail);
        assert!(!spool_dir.exists(), "the spool is removed");
        assert!(target.join("ampr_assets.index").is_file());
        assert!(
            target.join("ampr_assets-000.pak").is_file() || {
                std::fs::read_dir(&target)
                    .unwrap()
                    .any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".pak"))
            }
        );
        assert!(!target.join("data/only_packed/table.bin").exists());
        round_trip(Box::new(FolderSource::open(&target).unwrap()), &src);

        // A volume damaged after packing: the read-back through the packs says so.
        let pak = std::fs::read_dir(&target)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|e| e == "pak"))
            .unwrap();
        let mut bytes = std::fs::read(&pak).unwrap();
        let at = bytes.len() / 2;
        for b in &mut bytes[at..at + 4096] {
            *b ^= 0x5a;
        }
        std::fs::write(&pak, bytes).unwrap();
        let mut source = FolderSource::open(&src).unwrap();
        let config = ampr_pack::default_profile(9, 16);
        let e = read_back(
            Box::new(FolderSource::open(&target).unwrap()),
            &mut source,
            &config,
            VerifyMode::Full,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(e.contains("LZ4 packs"), "{e}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
