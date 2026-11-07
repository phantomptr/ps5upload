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

/// ampr-emu-ng's release: fetched once, and only kept or used when its SHA-256 matches.
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

/// Builds `source` as a plain image at `output` with its files in zstd packs at `level`:
/// measured (stage `pack`), then written and read back as any image, then the packs read
/// back through the same checks the emulator makes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build(
    format: ImageFormat,
    level: u8,
    source: &mut dyn SourceTree,
    output: &Path,
    mode: VerifyMode,
    verify_chunk: u64,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<ImageBuilt, String> {
    let runtime = crate::ampr_trace::ng_runtime()?;
    let mtime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (mut prepared, packed) = prepare(source, runtime, mtime)?;
    let raw: u64 = packed.iter().map(|f| f.size).sum();
    // Each file counts twice in the writer's progress: one pass counted as two.
    let total = raw.saturating_mul(2);
    stage("pack", 0, total);
    let index = ps5_dump_forge_zstd::writer::measure(
        &mut prepared,
        &packed,
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
        output.display()
    );
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
    ps5_dump_forge_zstd::reader::unpack(image)
        .map_err(|e| format!("reading the zstd packs back from the image: {e}"))?;
    Ok(built)
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
}
