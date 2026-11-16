//! PS5Upload's converter as a program that runs on the PS5 itself.
//!
//! The helper starts it as a separate process (through the console's ELF loader, the way it
//! starts the archive extractor), so a crash or a memory spike in a conversion never takes the
//! helper down. The two talk through files in one folder, [`job`]: the helper writes
//! `request.json`, this program rewrites `status.json`, and a `cancel` file asks it to stop.
//!
//! It inspects a source ([`inspect`]) and builds one ([`job`]): a game folder, `.exfat`,
//! `.ffpkg`, `.ffpfs` or `.ffpfsc`, or the game folder inside a `.zip` or `.7z` (read in place,
//! nothing unpacked), on the console into a `.exfat`, `.ffpkg`, `.ffpfs`, a `.ffpfsc` holding one
//! of those, a folder (zstd packs optional) or a debug `.pkg` (written, not installed), read back
//! before it gets its name. A `.rar` is refused ([`RAR_REFUSAL`] says why). The build code is
//! `ps5upload-convert-core` (and `ps5upload-fpkg`'s package builder), the same the engine runs
//! on a computer.

#[cfg(any(target_env = "ps5", test))]
pub mod alloc;
pub mod job;

use std::path::Path;
use std::sync::Arc;

use ps5upload_fpkg::source::{self, SourceTree};

/// Why a `.rar` is not converted on the console. UnRAR's licence may not be combined with
/// third-party GPL code that carries no UnRAR exception, and this program links some (the AMPR
/// emulation port in `ps5upload-fpkg`): the engine keeps UnRAR in a program of its own for that
/// reason (LICENSES/UnRAR-exception.md), and a PS5 payload can't start a helper program to talk
/// to over pipes (it can't fork). PS5 Dump Forge links UnRAR into its PS5 program because all of
/// its own code carries the exception; ours does not.
pub const RAR_REFUSAL: &str = "a .rar can't be converted on the console: UnRAR may not be linked \
                               into the converter (its licence), so unpack it first (Files) or \
                               convert it on a computer";

/// One line to the log (stderr; on the PS5 that is `convert.log` in the job folder). Never
/// `eprintln!`: that panics when the write fails, and a full drive must not end a job that way.
pub fn log(line: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// What a source is, as the console page shows it before a conversion is started.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Inspection {
    pub path: String,
    /// `folder`, `exfat`, `ffpkg`, `ffpfs`, `ffpfsc`, `zip` or `7z`.
    pub format: String,
    /// The image file's size, or for a folder the sum of its files.
    pub size: u64,
    pub files: u64,
    /// The sum of the files' sizes: what a conversion has to read.
    pub data_bytes: u64,
    pub empty_dirs: u64,
    pub title_id: Option<String>,
    pub content_id: Option<String>,
    pub title: Option<String>,
    pub content_version: Option<String>,
    pub master_version: Option<String>,
    /// `lz4` or `zstd` when the game's files are inside asset packs already.
    pub packed: Option<String>,
    pub has_eboot: bool,
    pub warnings: Vec<String>,
}

/// Why a source was not inspected. `cause` is the token the apps translate (the helper's
/// `fs_extract_*` causes work the same way); `detail` is for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub cause: &'static str,
    pub detail: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.cause, self.detail)
    }
}

impl std::error::Error for Refusal {}

/// The formats this program reads: a game folder, the images that hold one unencrypted (a
/// `.ffpfsc` too, the read-back of one written here reads it the same way), and a `.zip` or
/// `.7z` holding a game folder. A `.pkg` stays with the engine on a computer; a `.rar` is
/// refused ([`RAR_REFUSAL`]).
fn format_of(path: &Path) -> Result<&'static str, Refusal> {
    let unsupported = |detail: String| Refusal {
        cause: "convert_unsupported_source",
        detail,
    };
    // `smb://host/...` and the like are the engine's remote sources. Nothing is registered to
    // open them here, and the console page only offers paths on the console.
    if ps5upload_fpkg::remote_source::is_remote(path) {
        return Err(unsupported(format!(
            "{} is not on this console",
            path.display()
        )));
    }
    let meta = std::fs::metadata(path).map_err(|e| Refusal {
        cause: "convert_source_unreadable",
        detail: format!("{}: {e}", path.display()),
    })?;
    if meta.is_dir() {
        return Ok("folder");
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "exfat" => Ok("exfat"),
        "ffpkg" => Ok("ffpkg"),
        "ffpfs" => Ok("ffpfs"),
        "ffpfsc" => Ok("ffpfsc"),
        "zip" => Ok("zip"),
        "7z" => Ok("7z"),
        "rar" => Err(Refusal {
            cause: "convert_rar_unsupported",
            detail: format!("{}: {RAR_REFUSAL}", path.display()),
        }),
        _ => Err(unsupported(format!(
            "{} is neither a game folder, a .exfat, .ffpkg, .ffpfs or .ffpfsc image, nor a .zip \
             or .7z",
            path.display()
        ))),
    }
}

/// Whether `format` (as [`format_of`] names it) is an archive read in place.
pub fn is_archive(format: &str) -> bool {
    matches!(format, "zip" | "7z")
}

/// Why an archive could not be opened as a game, as a cause: an image or package inside it (it
/// has to be unpacked first), no game or more than one, a password, else damaged or unreadable.
pub fn archive_refusal(path: &Path, detail: String) -> Refusal {
    let cause = if detail.starts_with(ps5upload_convert_core::archive_tree::NEEDS_UNPACK) {
        "convert_archive_needs_unpack"
    } else if detail.starts_with("no game in the archive")
        || detail.starts_with("more than one game in the archive")
    {
        "convert_archive_no_game"
    } else if detail.contains("password-protected") {
        "convert_archive_password"
    } else {
        "convert_source_unreadable"
    };
    Refusal {
        cause,
        detail: format!("{}: {detail}", path.display()),
    }
}

/// The game folder in the archive at `path`, read in place; `stop` ends a read waiting on it.
pub fn open_archive(
    path: &Path,
    stop: ps5upload_archive::stream::Stop,
) -> Result<Box<dyn SourceTree>, Refusal> {
    ps5upload_convert_core::archive_tree::open(path, None, stop, None)
        .map(|t| Box::new(t) as Box<dyn SourceTree>)
        .map_err(|detail| archive_refusal(path, detail))
}

/// A string field of a `param.json`.
fn param_str(param: &serde_json::Value, key: &str) -> Option<String> {
    param.get(key)?.as_str().map(str::to_string)
}

/// The title as the console shows it: `localizedParameters`' entry for its default language,
/// else a top-level `titleName` (what a converted PS4 game's `param.json` carries).
fn title_of(param: &serde_json::Value) -> Option<String> {
    let localized = param.get("localizedParameters");
    let default = localized
        .and_then(|l| l.get("defaultLanguage"))
        .and_then(|d| d.as_str());
    default
        .and_then(|lang| localized?.get(lang)?.get("titleName")?.as_str())
        .or_else(|| param.get("titleName")?.as_str())
        .map(str::to_string)
}

/// The sum of `files`' sizes. They are the source's own claims (an image's directory says
/// them), and the console build has no overflow checks: a sum past `u64` is refused, never
/// wrapped to something small.
pub fn data_bytes(files: &[source::SourceFile]) -> Result<u64, String> {
    files
        .iter()
        .try_fold(0u64, |sum, f| sum.checked_add(f.size))
        .ok_or_else(|| "its files' sizes add up to more than any drive holds".to_string())
}

/// Inspects the game folder or image at `path`. Reads the file list and `sce_sys/param.json`;
/// never the game's data.
pub fn inspect(path: &Path) -> Result<Inspection, Refusal> {
    inspect_keeping(path, Arc::new(|| false)).map(|(inspection, _)| inspection)
}

/// [`inspect`], with `stop` ending a read waiting on an archive (a `.7z`'s param file may sit
/// behind a large solid block), and handing back the archive it opened (`None` for a folder or
/// an image) so a build reads it without opening it a second time.
pub fn inspect_keeping(
    path: &Path,
    stop: ps5upload_archive::stream::Stop,
) -> Result<(Inspection, Option<Box<dyn SourceTree>>), Refusal> {
    let format = format_of(path)?;
    let unreadable = |e: ps5upload_fpkg::Error| Refusal {
        cause: "convert_source_unreadable",
        detail: format!("{}: {e}", path.display()),
    };
    let mut tree = if is_archive(format) {
        // The listing and the game's param file: never the game's data.
        open_archive(path, stop)?
    } else {
        source::open(path).map_err(unreadable)?
    };
    let files = tree.files().len() as u64;
    let data_bytes = data_bytes(tree.files()).map_err(|detail| Refusal {
        cause: "convert_source_unreadable",
        detail: format!("{}: {detail}", path.display()),
    })?;
    let has = |tree: &dyn SourceTree, p: &str| tree.files().iter().any(|f| f.path == p);

    let mut out = Inspection {
        path: path.display().to_string(),
        format: format.to_string(),
        size: if format == "folder" {
            data_bytes
        } else {
            std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
        },
        files,
        data_bytes,
        empty_dirs: tree.empty_dirs().len() as u64,
        title_id: None,
        content_id: None,
        title: None,
        content_version: None,
        master_version: None,
        packed: None,
        has_eboot: has(tree.as_ref(), "eboot.bin"),
        // What the conversion leaves out of an archive (files beside its game folder).
        warnings: tree.notes(),
    };

    // A PS4 game folder has a param.sfo instead; reading it comes with the engine's full
    // inspection, so for now such a source is listed without its title.
    const PARAM: &str = "sce_sys/param.json";
    if has(tree.as_ref(), PARAM) {
        // The size cap is fpkg's: an image that claims a huge param.json is refused, not read.
        match source::read_small(tree.as_mut(), PARAM, source::MAX_PARAM_BYTES) {
            Ok(bytes) => {
                // Some dumps pad the file with NULs, and it may start with a BOM.
                let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
                let text = String::from_utf8_lossy(&bytes[..end]);
                match serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}'))
                {
                    Ok(param) => {
                        out.title_id = param_str(&param, "titleId");
                        out.content_id = param_str(&param, "contentId");
                        out.content_version = param_str(&param, "contentVersion");
                        out.master_version = param_str(&param, "masterVersion");
                        out.title = title_of(&param);
                    }
                    Err(e) => out.warnings.push(format!("{PARAM} is not valid JSON: {e}")),
                }
            }
            Err(e) => out.warnings.push(format!("{PARAM}: {e}")),
        }
    } else {
        out.warnings.push(format!("no {PARAM}"));
    }
    if !out.has_eboot {
        out.warnings.push("no eboot.bin".to_string());
    }
    match source::packed_kind(tree.as_mut()) {
        Ok(kind) => out.packed = kind.map(str::to_string),
        Err(e) => out.warnings.push(format!("asset pack index: {e}")),
    }
    Ok((out, is_archive(format).then_some(tree)))
}

#[cfg(test)]
pub(crate) mod testing {
    use std::path::{Path, PathBuf};

    /// A folder of this test's own under the system temp folder, removed when dropped.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("ps5upload-convert-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A small game folder: param.json, an eboot, one data file and an empty folder.
    pub fn game_folder(root: &Path) -> PathBuf {
        let game = root.join("PPSA01234");
        std::fs::create_dir_all(game.join("sce_sys")).unwrap();
        std::fs::create_dir_all(game.join("data/empty")).unwrap();
        std::fs::write(
            game.join("sce_sys/param.json"),
            // A BOM in front and NULs behind, as real dumps have them.
            "\u{feff}{\"titleId\":\"PPSA01234\",\"contentId\":\"UP0000-PPSA01234_00-TESTGAME00000000\",\
             \"contentVersion\":\"01.002.000\",\"masterVersion\":\"01.00\",\
             \"localizedParameters\":{\"defaultLanguage\":\"en-US\",\"en-US\":{\"titleName\":\"Test Game\"},\
             \"ja-JP\":{\"titleName\":\"テスト\"}}}\0\0\0",
        )
        .unwrap();
        std::fs::write(game.join("eboot.bin"), [0x7F, b'E', b'L', b'F', 0, 0, 0, 0]).unwrap();
        std::fs::write(game.join("data/level.bin"), vec![7u8; 100_000]).unwrap();
        game
    }

    /// The files under `dir`, as `(path relative to dir, bytes)`, sorted.
    fn files_of(dir: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(root: &Path, at: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for e in std::fs::read_dir(at).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(root, &p, out);
                } else {
                    let rel = p
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push((rel, std::fs::read(&p).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    /// `game` (a folder) as a deflate `.zip` at `path`, inside a folder of its own name (as
    /// dumps are packed).
    pub fn zip_of(game: &Path, path: &Path) {
        use std::io::Write;
        let name = game.file_name().unwrap().to_string_lossy().into_owned();
        let mut zw = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (rel, bytes) in files_of(game) {
            zw.start_file(format!("{name}/{rel}"), opts).unwrap();
            zw.write_all(&bytes).unwrap();
        }
        zw.add_directory(format!("{name}/data/empty/"), opts)
            .unwrap();
        zw.finish().unwrap();
    }

    /// `game` as a `.7z` at `path`, every file in one LZMA2 block (`solid`) or one each.
    pub fn sevenz_of(game: &Path, path: &Path, solid: bool) {
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter, SourceReader};
        let name = game.file_name().unwrap().to_string_lossy().into_owned();
        let files = files_of(game);
        let mut w = ArchiveWriter::create(path).unwrap();
        if solid {
            let entries: Vec<ArchiveEntry> = (files.iter())
                .map(|(p, _)| ArchiveEntry::new_file(&format!("{name}/{p}")))
                .collect();
            let readers: Vec<SourceReader<std::io::Cursor<Vec<u8>>>> = (files.iter())
                .map(|(_, d)| SourceReader::new(std::io::Cursor::new(d.clone())))
                .collect();
            w.push_archive_entries(entries, readers).unwrap();
        } else {
            for (p, d) in &files {
                w.push_archive_entry(
                    ArchiveEntry::new_file(&format!("{name}/{p}")),
                    Some(d.as_slice()),
                )
                .unwrap();
            }
        }
        // The empty folder, after the files (a folder inside a solid block is refused).
        w.push_archive_entry::<&[u8]>(
            ArchiveEntry::new_directory(&format!("{name}/data/empty")),
            None,
        )
        .unwrap();
        w.finish().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expect_game(i: &Inspection, format: &str) {
        assert_eq!(i.format, format);
        assert_eq!(i.title_id.as_deref(), Some("PPSA01234"));
        assert_eq!(
            i.content_id.as_deref(),
            Some("UP0000-PPSA01234_00-TESTGAME00000000")
        );
        assert_eq!(i.title.as_deref(), Some("Test Game"));
        assert_eq!(i.content_version.as_deref(), Some("01.002.000"));
        assert_eq!(i.master_version.as_deref(), Some("01.00"));
        assert_eq!(i.files, 3);
        assert_eq!(i.empty_dirs, 1);
        assert_eq!(i.packed, None);
        assert!(i.has_eboot);
        assert!(i.warnings.is_empty(), "{:?}", i.warnings);
    }

    #[test]
    fn sizes_that_add_up_past_u64_are_refused_not_wrapped() {
        let file = |size| source::SourceFile {
            path: "a".into(),
            size,
        };
        assert_eq!(data_bytes(&[file(1), file(2)]), Ok(3));
        assert_eq!(data_bytes(&[]), Ok(0));
        assert!(data_bytes(&[file(u64::MAX), file(1)]).is_err());
        assert!(data_bytes(&[file(1 << 63), file(1 << 63)]).is_err());
    }

    /// A folder, and the same game read back out of an exFAT image built from it: the image
    /// reader is the format code the console build has to carry.
    #[test]
    fn inspects_a_folder_and_an_image_of_it() {
        let tmp = testing::TempDir::new("inspect");
        let game = testing::game_folder(&tmp.0);
        let folder = inspect(&game).unwrap();
        expect_game(&folder, "folder");
        assert_eq!(folder.size, folder.data_bytes);

        let image = tmp.0.join("game.exfat");
        let mut tree = source::open(&game).unwrap();
        ps5upload_fpkg::exfat_write::build_exfat(
            tree.as_mut(),
            &image,
            None,
            &mut |_, _| {},
            &|| false,
        )
        .unwrap();
        let from_image = inspect(&image).unwrap();
        expect_game(&from_image, "exfat");
        assert_eq!(from_image.data_bytes, folder.data_bytes);
        assert_eq!(from_image.size, std::fs::metadata(&image).unwrap().len());
    }

    /// The game folder inside a `.zip` and a `.7z` (solid and not), read in place: what the
    /// folder itself says, from the listing and the param file alone.
    #[test]
    fn inspects_the_game_in_a_zip_and_a_7z() {
        let tmp = testing::TempDir::new("inspectarchive");
        let game = testing::game_folder(&tmp.0);
        let folder = inspect(&game).unwrap();
        for (name, solid) in [("g.zip", false), ("g.7z", false), ("s.7z", true)] {
            let path = tmp.0.join(name);
            if name.ends_with(".zip") {
                testing::zip_of(&game, &path);
            } else {
                testing::sevenz_of(&game, &path, solid);
            }
            let i = inspect(&path).unwrap();
            assert_eq!(i.format, &name[name.find('.').unwrap() + 1..], "{name}");
            assert_eq!(i.title.as_deref(), Some("Test Game"), "{name}");
            assert_eq!((i.files, i.data_bytes), (folder.files, folder.data_bytes));
            assert_eq!(i.size, std::fs::metadata(&path).unwrap().len());
            assert!(
                i.has_eboot && i.warnings.is_empty(),
                "{name}: {:?}",
                i.warnings
            );
        }
    }

    /// What the console does not convert from an archive, each with its own cause: a `.rar`, an
    /// archive cut short, one holding an image (unpack it first), one with no game.
    #[test]
    fn refuses_archives_it_cannot_convert_from() {
        let tmp = testing::TempDir::new("refusearchive");
        let game = testing::game_folder(&tmp.0);
        let cause = |p: &Path| inspect(p).unwrap_err().cause;
        let rar = tmp.0.join("g.rar");
        std::fs::write(&rar, b"Rar!\x1a\x07\x01\x00").unwrap();
        assert_eq!(cause(&rar), "convert_rar_unsupported");
        for name in ["cut.zip", "cut.7z"] {
            let path = tmp.0.join(name);
            if name.ends_with(".zip") {
                testing::zip_of(&game, &path);
            } else {
                testing::sevenz_of(&game, &path, true);
            }
            let bytes = std::fs::read(&path).unwrap();
            std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
            assert_eq!(cause(&path), "convert_source_unreadable", "{name}");
        }
        let one = |name: &str, entry: &str| {
            use std::io::Write;
            let path = tmp.0.join(name);
            let mut zw = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            zw.start_file(entry, zip::write::SimpleFileOptions::default())
                .unwrap();
            zw.write_all(b"x").unwrap();
            zw.finish().unwrap();
            path
        };
        assert_eq!(
            cause(&one("image.zip", "dump/Game.exfat")),
            "convert_archive_needs_unpack"
        );
        assert_eq!(
            cause(&one("notes.zip", "readme.txt")),
            "convert_archive_no_game"
        );
    }

    #[test]
    fn a_folder_that_is_not_a_game_is_listed_with_warnings() {
        let tmp = testing::TempDir::new("notgame");
        std::fs::write(tmp.0.join("readme.txt"), b"hello").unwrap();
        let i = inspect(&tmp.0).unwrap();
        assert_eq!((i.files, i.title_id.as_deref()), (1, None));
        assert_eq!(i.warnings, ["no sce_sys/param.json", "no eboot.bin"]);
    }

    #[test]
    fn refuses_what_the_console_version_does_not_read() {
        let tmp = testing::TempDir::new("refuse");
        for name in ["game.pkg", "game.tar"] {
            let path = tmp.0.join(name);
            std::fs::write(&path, b"x").unwrap();
            assert_eq!(
                inspect(&path).unwrap_err().cause,
                "convert_unsupported_source",
                "{name}"
            );
        }
        let missing = inspect(&tmp.0.join("missing.exfat")).unwrap_err();
        assert_eq!(missing.cause, "convert_source_unreadable");
        // An image that is not one: the reader's error, not a panic.
        let bad = tmp.0.join("bad.exfat");
        std::fs::write(&bad, vec![0u8; 4096]).unwrap();
        assert_eq!(
            inspect(&bad).unwrap_err().cause,
            "convert_source_unreadable"
        );
    }
}
