//! The source of the game: a folder, or a mount image the same tree is read out of.

use std::path::{Path, PathBuf};

use crate::{format_err, Error, Result};

/// One file of the source tree: path relative to the user root, `/`-separated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub size: u64,
}

/// The game files to convert, whatever they live in. Sizes are known up front, so the
/// plan fixes every offset before the first byte is read. Images seek, so reading takes
/// `&mut self`.
pub trait SourceTree {
    fn files(&self) -> &[SourceFile];

    /// The whole file at `path`.
    fn read(&mut self, path: &str) -> Result<Vec<u8>>;

    /// Up to `len` bytes at `offset`. Sources override this so a four-byte module magic
    /// check does not read a 100 MB `eboot.bin` into memory.
    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let all = self.read(path)?;
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(all.len());
        let end = start.saturating_add(len).min(all.len());
        Ok(all[start..end].to_vec())
    }

    /// One line for logs: what the source is and where it came from.
    fn describe(&self) -> String;
}

/// A game folder on the filesystem.
pub struct FolderSource {
    root: PathBuf,
    files: Vec<SourceFile>,
}

impl FolderSource {
    pub fn open(root: &Path) -> Result<Self> {
        Ok(Self {
            root: root.to_path_buf(),
            files: scan(root)?,
        })
    }
}

impl SourceTree for FolderSource {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        std::fs::read(self.root.join(path))
            .map_err(|e| Error::Io(std::io::Error::new(e.kind(), format!("{path}: {e}"))))
    }

    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::File::open(self.root.join(path))
            .map_err(|e| Error::Io(std::io::Error::new(e.kind(), format!("{path}: {e}"))))?;
        file.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; len];
        let read = file.read(&mut buf)?;
        buf.truncate(read);
        Ok(buf)
    }

    fn describe(&self) -> String {
        format!("folder {}", self.root.display())
    }
}

/// Open whatever `path` names as a source tree.
pub fn open(path: &Path) -> Result<Box<dyn SourceTree>> {
    if path.is_dir() {
        return Ok(Box::new(FolderSource::open(path)?));
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "exfat" => Ok(Box::new(crate::exfat::ExFatSource::open(path)?)),
        "ffpkg" | "ufs2" => Ok(Box::new(crate::ufs2_source::Ufs2Source::open(path)?)),
        _ => format_err(format!(
            "{} is neither a folder nor a supported image (.exfat, .ffpkg)",
            path.display()
        )),
    }
}

/// Junk no package wants, skipped by name at any depth.
pub(crate) fn is_junk(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == ".ds_store"
        || lower == "thumbs.db"
        || lower == "desktop.ini"
        || lower == "system volume information"
        || lower == ".fseventsd"
        || lower == ".spotlight-v100"
        || name.starts_with("._")
}

/// Walk `root` (a game folder) into its file list, sizes from the filesystem only.
pub fn scan(root: &Path) -> Result<Vec<SourceFile>> {
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<SourceFile>) -> Result<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {e}", dir.display()),
        ))
    })?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_junk(&name) {
            continue;
        }
        let path = entry.path();
        let ty = entry.file_type()?;
        if ty.is_dir() {
            walk(root, &path, out)?;
        } else if ty.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|_| Error::Format(format!("{} escaped the source root", path.display())))?
                .to_string_lossy()
                .replace('\\', "/");
            out.push(SourceFile {
                path: rel,
                size: entry.metadata()?.len(),
            });
        }
    }
    Ok(())
}

/// One readiness finding: what was checked, whether it holds, and what was seen.
#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Default)]
pub struct Readiness {
    pub checks: Vec<Check>,
}

impl Readiness {
    /// True when every check holds. Warnings are checks with `ok = false`.
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|c| !c.ok)
    }

    fn push(&mut self, name: &str, ok: bool, detail: impl Into<String>) {
        self.checks.push(Check {
            name: name.to_string(),
            ok,
            detail: detail.into(),
        });
    }
}

/// The module magics a launchable title may carry. As the payload's own note says
/// (`payload/include/elf_param.h`), a SELF magic means the module is *wrapped*, not that it
/// is encrypted — which console family the wrapper is for is what the magic distinguishes.
/// Measured: 17 of the 27 real game mounts carry the PS5 magic, 10 the PS4 one.
pub mod magic {
    pub const RAW_ELF: [u8; 4] = [0x7F, b'E', b'L', b'F'];
    pub const SELF_PS5: [u8; 4] = [0x54, 0x14, 0xF5, 0xEE];
    pub const SELF_PS4: [u8; 4] = [0x4F, 0x15, 0x3D, 0x1D];
    /// A genuine (Sony-signed) SELF.
    pub const SIGNED_SELF: [u8; 4] = [0x53, 0x43, 0x45, 0x00];
}

/// A `param.json`'s bytes, parsed (it may carry a BOM).
fn parse_param_json(bytes: &[u8]) -> Option<serde_json::Value> {
    let text = String::from_utf8_lossy(bytes);
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// The content id a `param.json` declares, if any.
pub fn content_id(param_json: &[u8]) -> Option<String> {
    let json = parse_param_json(param_json)?;
    let id = json.get("contentId")?.as_str()?;
    Some(id.to_string())
}

/// The content version (`MM.mmm.ppp`) a `param.json` declares, packed as the 2-3-3 BCD
/// word the finalized-image header echoes at `0x9C`.
pub fn content_version_word(param_json: &[u8]) -> Option<u32> {
    let json = parse_param_json(param_json)?;
    let version = json.get("contentVersion")?.as_str()?;
    let digits: Vec<u8> = version
        .chars()
        .filter(|c| c.is_ascii_digit())
        .map(|c| c as u8 - b'0')
        .collect();
    if digits.len() != 8 || digits.iter().any(|d| *d > 9) {
        return None;
    }
    let byte = |a: u8, b: u8| (a << 4) | b;
    Some(u32::from_be_bytes([
        byte(digits[0], digits[1]),
        byte(digits[2], digits[3]),
        byte(digits[4], digits[5]),
        byte(digits[6], digits[7]),
    ]))
}

fn module_magic(tree: &mut dyn SourceTree, rel: &str) -> Option<[u8; 4]> {
    let head = tree.read_range(rel, 0, 4).ok()?;
    head.try_into().ok()
}

/// Report readiness for a source tree. Never blocks: the caller decides which findings
/// matter for the build it is about to run.
pub fn readiness(tree: &mut dyn SourceTree) -> Readiness {
    let mut r = Readiness::default();
    let files = tree.files().to_vec();
    let has = |path: &str| files.iter().any(|f| f.path == path);

    let bytes: u64 = files.iter().map(|f| f.size).sum();
    r.push(
        "source",
        true,
        format!(
            "{}: {} files, {:.1} GiB",
            tree.describe(),
            files.len(),
            bytes as f64 / (1u64 << 30) as f64
        ),
    );

    r.push(
        "eboot.bin present",
        has("eboot.bin"),
        "the title module is the package root's eboot.bin",
    );
    r.push(
        "param.json present",
        has("sce_sys/param.json"),
        "a PS5 title uses param.json, not param.sfo",
    );
    r.push(
        "no param.sfo",
        !has("sce_sys/param.sfo"),
        "a param.sfo makes the launch path treat the title as PS4",
    );
    let param = tree.read("sce_sys/param.json").unwrap_or_default();
    match content_id(&param) {
        Some(id) => r.push(
            "content id",
            id.len() == 36 && id.is_ascii(),
            format!("{id} ({} chars)", id.len()),
        ),
        None => r.push("content id", false, "no contentId in sce_sys/param.json"),
    }
    r.push(
        "icon0.png and icon0.dds present",
        has("sce_sys/icon0.png") && has("sce_sys/icon0.dds"),
        "both are carried as container entries",
    );
    r.push(
        "sce_sys/about/right.sprx present",
        has("sce_sys/about/right.sprx"),
        "the rights module a debug package ships",
    );
    if let Some(m) = module_magic(tree, "eboot.bin") {
        let (kind, launchable) = match m {
            magic::RAW_ELF => ("raw ELF", true),
            magic::SELF_PS5 => ("PS5 SELF wrapper", true),
            magic::SELF_PS4 => ("PS4 SELF wrapper", true),
            magic::SIGNED_SELF => ("genuine SELF", false),
            _ => ("unknown", false),
        };
        r.push(
            "eboot.bin module magic",
            launchable,
            format!("{kind} ({m:02x?})"),
        );
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junk_is_skipped_and_sizes_are_recorded() {
        let dir = std::env::temp_dir().join(format!("fpkg-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sce_sys/about")).unwrap();
        std::fs::write(dir.join("eboot.bin"), [0u8; 10]).unwrap();
        std::fs::write(dir.join("sce_sys/about/right.sprx"), [0u8; 5]).unwrap();
        std::fs::write(dir.join(".DS_Store"), [0u8; 3]).unwrap();
        std::fs::write(dir.join("._eboot.bin"), [0u8; 3]).unwrap();
        std::fs::create_dir_all(dir.join(".Spotlight-V100")).unwrap();
        std::fs::write(dir.join(".Spotlight-V100/x"), [0u8; 3]).unwrap();
        let files = scan(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            files,
            vec![
                SourceFile {
                    path: "eboot.bin".into(),
                    size: 10
                },
                SourceFile {
                    path: "sce_sys/about/right.sprx".into(),
                    size: 5
                },
            ]
        );
    }

    #[test]
    fn content_version_packs_as_bcd() {
        let dir = std::env::temp_dir().join(format!("fpkg-ver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sce_sys")).unwrap();
        std::fs::write(
            dir.join("sce_sys/param.json"),
            br#"{"contentId":"UP0000-PPSA01234_00-TESTGAME00000000","contentVersion":"01.001.000"}"#,
        )
        .unwrap();
        let param = std::fs::read(dir.join("sce_sys/param.json")).unwrap();
        let word = content_version_word(&param);
        let id = content_id(&param);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(word, Some(0x0100_1000));
        assert_eq!(id.as_deref(), Some("UP0000-PPSA01234_00-TESTGAME00000000"));
    }

    #[test]
    fn a_folder_source_reads_ranges_and_reports_itself() {
        let dir = std::env::temp_dir().join(format!("fpkg-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("eboot.bin"), b"0123456789").unwrap();
        let mut tree = open(&dir).unwrap();
        assert_eq!(tree.read_range("eboot.bin", 2, 3).unwrap(), b"234");
        assert_eq!(tree.read_range("eboot.bin", 8, 99).unwrap(), b"89");
        assert_eq!(tree.read_range("eboot.bin", 99, 4).unwrap(), b"");
        assert_eq!(tree.read("eboot.bin").unwrap(), b"0123456789");
        assert!(tree.describe().starts_with("folder "));
        assert_eq!(tree.files().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn readiness_names_the_source_and_its_size() {
        let dir = std::env::temp_dir().join(format!("fpkg-named-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sce_sys")).unwrap();
        std::fs::write(dir.join("eboot.bin"), [0u8; 1024]).unwrap();
        std::fs::write(dir.join("sce_sys/param.json"), b"{}").unwrap();
        let mut tree = open(&dir).unwrap();
        let readiness = readiness(tree.as_mut());
        let line = readiness
            .checks
            .iter()
            .find(|c| c.name == "source")
            .expect("a source line");
        assert!(line.ok);
        assert!(line.detail.contains("folder"), "{}", line.detail);
        assert!(line.detail.contains("2 files"), "{}", line.detail);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unknown_source_kind_is_an_error() {
        let dir = std::env::temp_dir().join(format!("fpkg-kind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("game.iso");
        std::fs::write(&img, [0u8; 8]).unwrap();
        let Err(err) = open(&img) else {
            panic!("a .iso must not open as a source");
        };
        let err = err.to_string();
        assert!(err.contains(".exfat"), "{err}");
        assert!(err.contains(".ffpkg"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
