//! PS5Upload's converter as a program that runs on the PS5 itself.
//!
//! The helper starts it as a separate process (through the console's ELF loader, the way it
//! starts the archive extractor), so a crash or a memory spike in a conversion never takes the
//! helper down. The two talk through files in one folder, [`job`]: the helper writes
//! `request.json`, this program rewrites `status.json`, and a `cancel` file asks it to stop.
//!
//! It inspects a source ([`inspect`]) and builds one ([`job`]): a game folder, `.exfat`,
//! `.ffpkg` or `.ffpfs` on the console into a `.exfat`, `.ffpkg`, `.ffpfs` or a folder, zstd
//! packs optional, read back before it gets its name. The build code is
//! `ps5upload-convert-core`, the same the engine runs on a computer.

#[cfg(any(target_env = "ps5", test))]
pub mod alloc;
pub mod job;

use std::path::Path;

use ps5upload_fpkg::source::{self, SourceTree};

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
    /// `folder`, `exfat`, `ffpkg` or `ffpfs`.
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

/// The formats this program reads: a game folder and the images that hold one unencrypted.
/// A `.pkg`, an archive and a `.ffpfsc` stay with the engine on a computer for now.
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
        _ => Err(unsupported(format!(
            "{} is neither a game folder nor a .exfat, .ffpkg or .ffpfs image",
            path.display()
        ))),
    }
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
    let format = format_of(path)?;
    let unreadable = |e: ps5upload_fpkg::Error| Refusal {
        cause: "convert_source_unreadable",
        detail: format!("{}: {e}", path.display()),
    };
    let mut tree = source::open(path).map_err(unreadable)?;
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
        warnings: Vec::new(),
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
    Ok(out)
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
        for name in ["game.pkg", "game.ffpfsc", "game.7z"] {
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
