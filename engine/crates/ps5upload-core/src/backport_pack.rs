//! Recognising a community "backport pack" on disk.
//!
//! A pack is the format backports are actually distributed in, and it is much
//! more than a library set: alongside `fakelib/` it ships an `eboot.bin` that
//! has ALREADY been patched to the backport SDK pair, replacement
//! `sce_module/` modules, and the game's own engine plugins.
//!
//! Classification is on the RELATIVE PATH, never the basename. The corpus
//! importer used to decide "is this a library?" from the extension alone, and
//! every pack carries `.prx` files outside `fakelib/` — `sce_module/libc.prx`,
//! a `prx/` folder of engine plugins, `sce_sys/about/right.sprx`. Pointing that
//! importer at a pack folder silently produced a set mixing all of them
//! together, which is a combination no game has ever run.
//!
//! Packs vary in what they carry: some ship only `fakelib/`, `eboot.bin` and
//! `sce_module/`, others add dozens of engine plugins under `prx/`. Every
//! installable role below has to be recognised or those files are silently
//! left behind.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::fakelibs::is_library_name;

/// What one file inside a pack is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackRole {
    /// `fakelib/<name>.sprx` — the reusable library set, and the only part
    /// that belongs in the content-addressed corpus.
    Library,
    /// A pre-patched `eboot.bin` at the pack root.
    Eboot,
    /// `sce_module/<name>.prx` — replacement system modules installed beside
    /// the eboot. Emphatically NOT libraries.
    SceModule,
    /// `prx/<name>.prx` — the game's own engine plugins, re-signed by the pack
    /// author. Title-specific, so never corpus material, but they MUST be
    /// installed: a pack that ships them expects them to be there.
    GamePrx,
    /// `sce_sys/about/<name>` — shipped by every pack seen so far.
    SceSys,
    /// Everything else the pack happens to carry.
    Other,
}

impl PackRole {
    /// Does installing a pack write this file to the console?
    pub fn is_installable(self) -> bool {
        !matches!(self, PackRole::Other)
    }
}

/// Classify one pack-relative path.
///
/// Rejects anything that tries to escape the pack root: a pack is a folder the
/// user downloaded from a third party, so `..` and absolute paths are hostile
/// input, not edge cases.
pub fn classify(rel_path: &str) -> PackRole {
    let norm = rel_path.replace('\\', "/");
    if norm.starts_with('/') || norm.split('/').any(|c| c == ".." || c == ".") {
        return PackRole::Other;
    }
    let parts: Vec<&str> = norm.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        // These folders are flat: a nested path under one is not something any
        // pack ships, and treating it as installable would let a crafted
        // archive place a name we never vetted.
        ["fakelib", name] if is_library_name(name) => PackRole::Library,
        ["eboot.bin"] => PackRole::Eboot,
        ["sce_module", name] if is_library_name(name) => PackRole::SceModule,
        ["prx", name] if is_library_name(name) => PackRole::GamePrx,
        ["sce_sys", "about", name] if !name.starts_with('.') => PackRole::SceSys,
        _ => PackRole::Other,
    }
}

/// A title id mentioned in a pack's folder name.
///
/// The packs seen in the wild carry no manifest — the folder name is the only
/// statement of which game they are for (e.g.
/// `[SITE]-FW 4xx PPSA19534 (v01.000.016) backport files`). That makes this a
/// HINT and never an authority: the caller must confirm against the title the
/// user actually picked before overwriting a 256 MB eboot.
pub fn title_id_hint(folder_name: &str) -> Option<String> {
    let bytes = folder_name.as_bytes();
    for start in 0..bytes.len() {
        let rest = &folder_name[start..];
        if rest.len() < 9 {
            break;
        }
        let (head, tail) = rest.split_at(4);
        if !(head.eq_ignore_ascii_case("PPSA") || head.eq_ignore_ascii_case("CUSA")) {
            continue;
        }
        let digits: String = tail.chars().take(5).collect();
        if digits.len() == 5 && digits.chars().all(|c| c.is_ascii_digit()) {
            return Some(format!("{}{}", head.to_ascii_uppercase(), digits));
        }
    }
    None
}

/// One file found inside a pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackFile {
    /// Path relative to the pack root, with `/` separators.
    pub rel_path: String,
    pub size: u64,
}

/// What a folder on disk turned out to contain.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackContents {
    pub libraries: Vec<PackFile>,
    pub eboot: Option<PackFile>,
    pub sce_modules: Vec<PackFile>,
    pub game_prx: Vec<PackFile>,
    pub sce_sys: Vec<PackFile>,
    pub other: Vec<PackFile>,
    /// Title id read out of the folder name, when it states one.
    pub title_id_hint: Option<String>,
}

impl PackContents {
    /// A pack with no `fakelib/` is not a pack. Saying so beats importing
    /// nothing and reporting success, which is how a user ends up believing
    /// the corpus grew when it did not.
    pub fn is_pack(&self) -> bool {
        !self.libraries.is_empty()
    }

    /// Everything an install would write, in the order it would write it.
    pub fn installable(&self) -> Vec<&PackFile> {
        let mut out: Vec<&PackFile> = Vec::new();
        out.extend(self.libraries.iter());
        out.extend(self.sce_modules.iter());
        out.extend(self.game_prx.iter());
        out.extend(self.sce_sys.iter());
        // The eboot goes last — see `planPackInstall` on the client for why.
        out.extend(self.eboot.iter());
        out
    }

    pub fn total_bytes(&self) -> u64 {
        self.installable().iter().map(|f| f.size).sum()
    }
}

/// Walk `dir` and sort what is in it into pack roles.
///
/// Descends at most two levels: every role lives at the root, one directory
/// down, or (for `sce_sys/about/`) two. An unbounded walk over a folder the
/// user pointed at is a way to spend minutes stat-ing a games library by
/// mistake.
pub fn inspect(dir: &Path) -> Result<PackContents> {
    if !dir.is_dir() {
        return Err(anyhow!("{} is not a folder", dir.display()));
    }
    let mut out = PackContents {
        title_id_hint: dir
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(title_id_hint),
        ..Default::default()
    };

    let mut stack: Vec<(PathBuf, String, usize)> = vec![(dir.to_path_buf(), String::new(), 0)];
    while let Some((path, prefix, depth)) = stack.pop() {
        let entries =
            std::fs::read_dir(&path).map_err(|e| anyhow!("reading {}: {e}", path.display()))?;
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            // AppleDouble sidecars and .DS_Store ride along with anything
            // copied off a Mac and are never part of the pack.
            if name.starts_with('.') {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                // Two levels: `sce_sys/about/` is the deepest role there is.
                if depth < 2 {
                    stack.push((entry.path(), rel, depth + 1));
                }
                continue;
            }
            let file = PackFile {
                rel_path: rel.clone(),
                size: meta.len(),
            };
            match classify(&rel) {
                PackRole::Library => out.libraries.push(file),
                PackRole::Eboot => out.eboot = Some(file),
                PackRole::SceModule => out.sce_modules.push(file),
                PackRole::GamePrx => out.game_prx.push(file),
                PackRole::SceSys => out.sce_sys.push(file),
                PackRole::Other => out.other.push(file),
            }
        }
    }
    for v in [
        &mut out.libraries,
        &mut out.sce_modules,
        &mut out.game_prx,
        &mut out.sce_sys,
        &mut out.other,
    ] {
        v.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_fakelib_contents_are_libraries() {
        // Every one of these ends in .sprx/.prx, so an extension-only check
        // would sweep them all into the library set.
        assert_eq!(classify("fakelib/libSceAgc.sprx"), PackRole::Library);
        assert_eq!(classify("sce_module/libc.prx"), PackRole::SceModule);
        assert_eq!(
            classify("Engine.Render.Core2.PlatformPs5.retail.prx"),
            PackRole::Other
        );
        assert_eq!(
            classify("SP/Engine.Render.Core2.PlatformPs5.retail.prx"),
            PackRole::Other
        );
    }

    #[test]
    fn game_plugins_under_prx_are_installed_not_dropped() {
        // Some packs ship dozens of these; they must be installed, not
        // dropped as unrecognised.
        assert_eq!(classify("prx/akdelay.prx"), PackRole::GamePrx);
        assert_eq!(classify("prx/masteringsuite.prx"), PackRole::GamePrx);
        assert!(PackRole::GamePrx.is_installable());
        // Still not corpus material — title-specific, not a shared library.
        assert_ne!(classify("prx/akdelay.prx"), PackRole::Library);
    }

    #[test]
    fn sce_sys_about_is_reached_two_levels_down() {
        assert_eq!(classify("sce_sys/about/right.sprx"), PackRole::SceSys);
        assert!(PackRole::SceSys.is_installable());
    }

    #[test]
    fn the_root_eboot_is_recognised() {
        assert_eq!(classify("eboot.bin"), PackRole::Eboot);
        // Only at the root: a nested one belongs to something else.
        assert_eq!(classify("SP/eboot.bin"), PackRole::Other);
    }

    #[test]
    fn windows_separators_classify_the_same() {
        assert_eq!(classify("fakelib\\libSceAgc.sprx"), PackRole::Library);
        assert_eq!(classify("prx\\akdelay.prx"), PackRole::GamePrx);
    }

    #[test]
    fn traversal_and_absolute_paths_are_never_installable() {
        // A pack is third-party data; these must not reach the console.
        assert_eq!(classify("../fakelib/libSceAgc.sprx"), PackRole::Other);
        assert_eq!(classify("fakelib/../../etc/passwd"), PackRole::Other);
        assert_eq!(classify("/fakelib/libSceAgc.sprx"), PackRole::Other);
        assert_eq!(classify("fakelib/sub/libSceAgc.sprx"), PackRole::Other);
        assert_eq!(classify("prx/../../etc/passwd"), PackRole::Other);
        assert!(!PackRole::Other.is_installable());
    }

    #[test]
    fn appledouble_sidecars_are_not_libraries() {
        assert_eq!(classify("fakelib/._libSceAgc.sprx"), PackRole::Other);
        assert_eq!(classify("sce_sys/about/._right.sprx"), PackRole::Other);
    }

    #[test]
    fn the_title_id_comes_out_of_the_folder_name() {
        assert_eq!(
            title_id_hint("[DLPSGAME.COM]-FW 4xx PPSA19534 (v01.000.016) backport files")
                .as_deref(),
            Some("PPSA19534")
        );
        // The second pack uses underscores rather than spaces.
        assert_eq!(
            title_id_hint("[DLPSGAME.COM]-FW_4xx_PPSA29343_Beast_of_Reincarnation_01_000_000")
                .as_deref(),
            Some("PPSA29343")
        );
        assert_eq!(
            title_id_hint("CUSA07842 backport").as_deref(),
            Some("CUSA07842")
        );
        assert_eq!(title_id_hint("no title here"), None);
        // Too few digits is not a title id.
        assert_eq!(title_id_hint("PPSA123"), None);
    }

    #[test]
    fn a_folder_without_fakelib_is_not_a_pack() {
        let contents = PackContents {
            sce_modules: vec![PackFile {
                rel_path: "sce_module/libc.prx".into(),
                size: 10,
            }],
            ..Default::default()
        };
        assert!(!contents.is_pack());
    }

    #[test]
    fn inspect_sorts_a_real_pack_layout() {
        // The richer pack shape: every role present at once.
        let dir = std::env::temp_dir().join(format!("packtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["fakelib", "sce_module", "prx", "sce_sys/about"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::write(dir.join("fakelib/libSceAgc.sprx"), b"aaa").unwrap();
        std::fs::write(dir.join("fakelib/libkernel.sprx"), b"bb").unwrap();
        std::fs::write(dir.join("sce_module/libc.prx"), b"cccc").unwrap();
        std::fs::write(dir.join("prx/akdelay.prx"), b"dd").unwrap();
        std::fs::write(dir.join("prx/akgain.prx"), b"e").unwrap();
        std::fs::write(dir.join("sce_sys/about/right.sprx"), b"ff").unwrap();
        std::fs::write(dir.join("eboot.bin"), b"ggggg").unwrap();
        std::fs::write(dir.join("Engine.Render.retail.prx"), b"h").unwrap();

        let got = inspect(&dir).unwrap();
        assert!(got.is_pack());
        assert_eq!(got.libraries.len(), 2);
        assert_eq!(got.sce_modules.len(), 1);
        assert_eq!(got.game_prx.len(), 2, "prx/ plugins must be picked up");
        assert_eq!(got.sce_sys.len(), 1, "sce_sys/about is two levels down");
        assert_eq!(got.eboot.as_ref().unwrap().size, 5);
        // The loose engine .prx at the root is NOT installable.
        assert_eq!(got.other.len(), 1);
        // 3+2 libs, 4 module, 2+1 prx, 2 sce_sys, 5 eboot = 19; the root .prx
        // is excluded because nothing installs it.
        assert_eq!(got.total_bytes(), 19);
        // Eboot last, so a failure never leaves a swapped eboot behind.
        assert_eq!(got.installable().last().unwrap().rel_path, "eboot.bin");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn inspect_rejects_a_file_or_missing_folder() {
        assert!(inspect(Path::new("/definitely/not/here")).is_err());
    }
}
