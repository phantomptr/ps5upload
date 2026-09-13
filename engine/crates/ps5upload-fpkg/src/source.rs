//! Walking a source folder into the file list both images are planned from.

use std::path::Path;

use crate::{Error, Result};

/// One file of the source tree: path relative to the user root, `/`-separated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub size: u64,
}

/// Junk no package wants, skipped by name at any depth.
fn is_junk(name: &str) -> bool {
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

/// The module magics a launchable title may carry.
pub mod magic {
    pub const RAW_ELF: [u8; 4] = [0x7F, b'E', b'L', b'F'];
    pub const FAKE_SELF: [u8; 4] = [0x54, 0x14, 0xF5, 0xEE];
    pub const SELF: [u8; 4] = [0x53, 0x43, 0x45, 0x00];
}

/// The content id a `param.json` declares, if any.
pub fn content_id(root: &Path) -> Option<String> {
    let raw = std::fs::read(root.join("sce_sys/param.json")).ok()?;
    let text = String::from_utf8_lossy(&raw);
    let json: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let id = json.get("contentId")?.as_str()?;
    Some(id.to_string())
}

/// The content version (`MM.mmm.ppp`) a `param.json` declares, packed as the 2-3-3 BCD
/// word the finalized-image header echoes at `0x9C`.
pub fn content_version_word(root: &Path) -> Option<u32> {
    let raw = std::fs::read(root.join("sce_sys/param.json")).ok()?;
    let text = String::from_utf8_lossy(&raw);
    let json: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
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

fn module_magic(root: &Path, rel: &str) -> Option<[u8; 4]> {
    let mut buf = [0u8; 4];
    let mut file = std::fs::File::open(root.join(rel)).ok()?;
    std::io::Read::read_exact(&mut file, &mut buf).ok()?;
    Some(buf)
}

/// Report readiness for a source tree. Never blocks: the caller decides which findings
/// matter for the build it is about to run.
pub fn readiness(root: &Path, files: &[SourceFile]) -> Readiness {
    let mut r = Readiness::default();
    let has = |path: &str| files.iter().any(|f| f.path == path);

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
    match content_id(root) {
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
    if let Some(m) = module_magic(root, "eboot.bin") {
        let kind = if m == magic::RAW_ELF {
            "raw ELF"
        } else if m == magic::FAKE_SELF {
            "fake SELF"
        } else if m == magic::SELF {
            "SELF"
        } else {
            "unknown"
        };
        r.push(
            "eboot.bin module magic",
            m == magic::RAW_ELF || m == magic::FAKE_SELF,
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
        let word = content_version_word(&dir);
        let id = content_id(&dir);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(word, Some(0x0100_1000));
        assert_eq!(id.as_deref(), Some("UP0000-PPSA01234_00-TESTGAME00000000"));
    }
}
