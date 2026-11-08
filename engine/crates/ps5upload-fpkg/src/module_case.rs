//! Module files whose name differs only in letter case from the path `eboot.bin` opens.
//!
//! Unity titles load their code with `sceKernelLoadStartModule("%s/Modules/Il2CppUserAssemblies.prx")`
//! while some dumps ship the file as `Il2cppUserAssemblies.prx`. An exFAT image, a PFS image and
//! a package ignore letter case, so the game finds its module there; a folder on the console's
//! `/data` and a UFS2 image (`.ffpkg`) do not, the load fails, and the game's first call into
//! the module jumps to address 0 (SIGSEGV, `RIP: 0x0`) a second after it starts. Seen on four
//! titles (PPSA22999, PPSA30883, PPSA31909, PPSA33090), each of which played once the file
//! carried the eboot's spelling.
//!
//! Only names the module spells as a path (`Modules/X.prx`, `/app0/sce_module/X.prx`) count:
//! the same eboot also carries the bare `Il2cppUserAssemblies.prx` (the module's own name,
//! resolved by the loader without touching the file system), which must not drive a rename.

use crate::source::{SourceFile, SourceTree};
use std::collections::{BTreeSet, HashMap};

/// One rename: the file at `from` takes the name in `to` (same directory, same bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseFix {
    pub from: String,
    pub to: String,
}

/// Bytes read per window.
const WINDOW: usize = 1024 * 1024;
/// The longest path looked for; a run of path characters longer than this is not one.
const MAX_TOKEN: usize = 256;
/// How much of a module is read. A real eboot is well under this; it bounds a stray giant.
const SCAN_LIMIT: u64 = 1 << 30;

fn path_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b'%')
}

/// Every `…/name.prx` / `…/name.sprx` path in `window` that ends after `from` and at or
/// before `upto`.
fn collect(window: &[u8], from: usize, upto: usize, out: &mut BTreeSet<String>) {
    let mut at = 0;
    while let Some(i) = window[at..].windows(3).position(|w| w == b"prx") {
        let end = at + i + 3;
        at = at + i + 1;
        let head = &window[..end - 3];
        if !(head.ends_with(b".") || head.ends_with(b".s"))
            || end <= from
            || end > upto
            || window.get(end).copied().is_some_and(path_byte)
        {
            continue;
        }
        let mut start = end - 3;
        while start > 0 && path_byte(window[start - 1]) && end - start < MAX_TOKEN {
            start -= 1;
        }
        if start > 0 && path_byte(window[start - 1]) {
            continue; // longer than any path: not one
        }
        if let Ok(token) = std::str::from_utf8(&window[start..end]) {
            if token.contains('/') {
                out.insert(token.to_string());
            }
        }
    }
}

/// The module paths `read_range` (a module of `size` bytes) spells out.
pub fn module_paths(
    size: u64,
    read_range: &mut dyn FnMut(u64, usize) -> Option<Vec<u8>>,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let end = size.min(SCAN_LIMIT);
    let mut offset = 0u64;
    let mut carry: Vec<u8> = Vec::new();
    // Matches ending at or before this point of the window were taken in the previous one.
    let mut taken = 0usize;
    while offset < end {
        let want = WINDOW.min((end - offset) as usize);
        let Some(chunk) = read_range(offset, want) else {
            break;
        };
        // A short read is not the end: a file read may return less than asked mid-file.
        let last = chunk.is_empty() || offset + chunk.len() as u64 >= end;
        offset += chunk.len() as u64;
        let mut window = carry;
        window.extend_from_slice(&chunk);
        // A path ending within MAX_TOKEN of the window's end may continue in the next read.
        let upto = if last {
            window.len()
        } else {
            window.len().saturating_sub(MAX_TOKEN)
        };
        collect(&window, taken, upto, &mut out);
        if last || chunk.is_empty() {
            break;
        }
        let keep = upto.saturating_sub(MAX_TOKEN);
        taken = upto - keep;
        carry = window.split_off(keep);
    }
    out
}

fn split(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

/// The renames that give `files` the spellings in `paths`. A directory gets one when it holds
/// the name only in another case and its own name matches the path's directory (when the path
/// names one: `%s/Modules/X.prx` says `Modules`, `%s/X.prx` says nothing).
pub fn fixes<'a>(paths: &BTreeSet<String>, files: impl Iterator<Item = &'a str>) -> Vec<CaseFix> {
    let files: Vec<&str> = files.collect();
    let mut out: Vec<CaseFix> = Vec::new();
    for want in paths {
        let parts: Vec<&str> = want.split('/').filter(|p| !p.is_empty()).collect();
        let Some((&name, dirs)) = parts.split_last() else {
            continue;
        };
        let parent = dirs
            .last()
            .copied()
            .filter(|d| !d.contains('%') && *d != "." && *d != "..");
        // Directory -> its files named like `name` in any case.
        let mut by_dir: HashMap<&str, Vec<&str>> = HashMap::new();
        for f in &files {
            let (dir, base) = split(f);
            let dir_name = split(dir).1;
            if base.eq_ignore_ascii_case(name)
                && parent.is_none_or(|p| dir_name.eq_ignore_ascii_case(p))
            {
                by_dir.entry(dir).or_default().push(f);
            }
        }
        for (dir, found) in by_dir {
            if found.iter().any(|f| split(f).1 == name) || found.len() != 1 {
                continue; // already right, or two spellings: nothing safe to pick
            }
            let to = if dir.is_empty() {
                name.to_string()
            } else {
                format!("{dir}/{name}")
            };
            if !out.iter().any(|c| c.from == found[0]) {
                out.push(CaseFix {
                    from: found[0].to_string(),
                    to,
                });
            }
        }
    }
    out.sort_by(|a, b| a.from.cmp(&b.from));
    out
}

/// The renames `tree` needs, from its root `eboot.bin`. None when it has no eboot.
pub fn tree_fixes(tree: &mut dyn SourceTree) -> Vec<CaseFix> {
    let Some(size) = tree
        .files()
        .iter()
        .find(|f| f.path == "eboot.bin")
        .map(|f| f.size)
    else {
        return Vec::new();
    };
    let paths = module_paths(size, &mut |off, len| {
        tree.read_range("eboot.bin", off, len).ok()
    });
    if paths.is_empty() {
        return Vec::new();
    }
    let names: Vec<String> = tree.files().iter().map(|f| f.path.clone()).collect();
    fixes(&paths, names.iter().map(String::as_str))
}

/// [`crate::source::open`] with the renames applied and unsigned executables fake-signed
/// ([`crate::fself`]), and what was done. Every read of a build's source (the write and, later,
/// the verify that reopens it) goes through this, so both see the same names and bytes.
pub fn open(
    path: &std::path::Path,
) -> crate::Result<(Box<dyn SourceTree>, Vec<CaseFix>, crate::fself::Signing)> {
    Ok(wrap(crate::source::open(path)?))
}

/// [`open`] for a PS4 game at `game` with the update at `update` merged in
/// ([`crate::ps4_merge`]), or for `game` alone when there is no update.
pub fn open_with_update(
    game: &std::path::Path,
    update: Option<&std::path::Path>,
) -> crate::Result<(Box<dyn SourceTree>, Vec<CaseFix>, crate::fself::Signing)> {
    match update {
        Some(update) => Ok(wrap(Box::new(crate::ps4_merge::open(game, update)?))),
        None => open(game),
    }
}

/// A tree with [`open`]'s renames and signing.
pub fn wrap(
    tree: Box<dyn SourceTree>,
) -> (Box<dyn SourceTree>, Vec<CaseFix>, crate::fself::Signing) {
    let (tree, fixes) = CaseFixed::wrap(tree);
    let (tree, signing) = crate::fself::wrap(tree);
    (tree, fixes, signing)
}

/// One line per rename, for a log or a readiness detail.
pub fn describe(fixes: &[CaseFix]) -> String {
    fixes
        .iter()
        .map(|c| format!("{} -> {}", c.from, split(&c.to).1))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A tree with [`CaseFix`]es applied: the file list carries the new names, reads of a new name
/// come from the old one. The source itself is never touched.
pub struct CaseFixed {
    inner: Box<dyn SourceTree>,
    files: Vec<SourceFile>,
    /// New name -> name in `inner`.
    back: HashMap<String, String>,
    fixes: Vec<CaseFix>,
}

impl CaseFixed {
    /// `tree` with its renames applied, and the renames; the tree unchanged when it needs none.
    pub fn wrap(mut tree: Box<dyn SourceTree>) -> (Box<dyn SourceTree>, Vec<CaseFix>) {
        let fixes = tree_fixes(tree.as_mut());
        if fixes.is_empty() {
            return (tree, fixes);
        }
        let to: HashMap<&str, &str> = fixes
            .iter()
            .map(|c| (c.from.as_str(), c.to.as_str()))
            .collect();
        let mut files: Vec<SourceFile> = tree
            .files()
            .iter()
            .map(|f| SourceFile {
                path: to
                    .get(f.path.as_str())
                    .map_or(f.path.clone(), |t| t.to_string()),
                size: f.size,
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let back = fixes
            .iter()
            .map(|c| (c.to.clone(), c.from.clone()))
            .collect();
        let wrapped = Self {
            inner: tree,
            files,
            back,
            fixes: fixes.clone(),
        };
        (Box::new(wrapped), fixes)
    }

    fn source_name<'p>(&'p self, path: &'p str) -> &'p str {
        self.back.get(path).map_or(path, String::as_str)
    }
}

impl SourceTree for CaseFixed {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }
    fn empty_dirs(&self) -> &[String] {
        self.inner.empty_dirs()
    }
    fn read(&mut self, path: &str) -> crate::Result<Vec<u8>> {
        let from = self.source_name(path).to_string();
        self.inner.read(&from)
    }
    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> crate::Result<Vec<u8>> {
        let from = self.source_name(path).to_string();
        self.inner.read_range(&from, offset, len)
    }
    fn describe(&self) -> String {
        format!(
            "{} (renamed to the eboot's spelling: {})",
            self.inner.describe(),
            describe(&self.fixes)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths_in(bytes: &[u8], window: usize) -> BTreeSet<String> {
        let mut read = |off: u64, len: usize| {
            let start = (off as usize).min(bytes.len());
            let end = (start + len.min(window)).min(bytes.len());
            Some(bytes[start..end].to_vec())
        };
        module_paths(bytes.len() as u64, &mut read)
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The bytes around the real strings in PPSA22999's eboot: the path it loads and, elsewhere,
    /// the module's bare name in the other case.
    fn unity_eboot() -> Vec<u8> {
        let mut b = vec![0u8; 3 * WINDOW + 77];
        let mut put = |at: usize, s: &[u8]| b[at..at + s.len()].copy_from_slice(s);
        put(1000, b"Il2cppUserAssemblies.prx\0");
        put(WINDOW - 20, b"%s/Modules/Il2CppUserAssemblies.prx\0"); // straddles a read
        put(2 * WINDOW + 5, b"%s/Modules/PS5Util.prx\0");
        put(2 * WINDOW + 100, b"/app0/sce_module/libc.prx\0");
        put(2 * WINDOW + 200, b"libSceAgc.sprx\0");
        put(2 * WINDOW + 300, b"/app0/sce_module/libSceFoo.sprx\0");
        put(2 * WINDOW + 400, b"%s/Modules/notamodule.prxy\0");
        b
    }

    #[test]
    fn only_paths_count_and_a_path_across_reads_is_found_whole() {
        let want = set(&[
            "%s/Modules/Il2CppUserAssemblies.prx",
            "%s/Modules/PS5Util.prx",
            "/app0/sce_module/libc.prx",
            "/app0/sce_module/libSceFoo.sprx",
        ]);
        // Read sizes that do and do not line up with the scan's own windows.
        for window in [WINDOW, 4096, 1000] {
            assert_eq!(paths_in(&unity_eboot(), window), want, "window {window}");
        }
    }

    #[test]
    fn the_unity_dump_gets_the_eboot_spelling() {
        let paths = paths_in(&unity_eboot(), WINDOW);
        let files = [
            "eboot.bin",
            "Media/Modules/Il2cppUserAssemblies.prx",
            "Media/Modules/PS5Util.prx",
            "sce_module/libc.prx",
            // A dump's decrypted copies sit in their own Modules directory: renamed too.
            "decrypted/Media/Modules/Il2cppUserAssemblies.prx",
            // A backup beside it is a different name.
            "Media/Modules/Il2cppUserAssemblies.prx.bak_20260719_135349",
        ];
        assert_eq!(
            fixes(&paths, files.into_iter()),
            vec![
                CaseFix {
                    from: "Media/Modules/Il2cppUserAssemblies.prx".into(),
                    to: "Media/Modules/Il2CppUserAssemblies.prx".into(),
                },
                CaseFix {
                    from: "decrypted/Media/Modules/Il2cppUserAssemblies.prx".into(),
                    to: "decrypted/Media/Modules/Il2CppUserAssemblies.prx".into(),
                },
            ]
        );
    }

    #[test]
    fn a_right_name_or_two_spellings_or_another_directory_is_left_alone() {
        let paths = set(&["%s/Modules/Il2CppUserAssemblies.prx"]);
        let right = ["Media/Modules/Il2CppUserAssemblies.prx"];
        assert!(fixes(&paths, right.into_iter()).is_empty());
        let both = [
            "Media/Modules/Il2CppUserAssemblies.prx",
            "Media/Modules/IL2CPPUSERASSEMBLIES.PRX",
        ];
        assert!(fixes(&paths, both.into_iter()).is_empty());
        let two_wrong = [
            "Media/Modules/il2cppuserassemblies.prx",
            "Media/Modules/IL2CPPUSERASSEMBLIES.PRX",
        ];
        assert!(fixes(&paths, two_wrong.into_iter()).is_empty());
        let elsewhere = ["Media/Plugins/Il2cppUserAssemblies.prx"];
        assert!(fixes(&paths, elsewhere.into_iter()).is_empty());
    }

    struct Tree {
        files: Vec<SourceFile>,
        data: Vec<Vec<u8>>,
    }
    impl SourceTree for Tree {
        fn files(&self) -> &[SourceFile] {
            &self.files
        }
        fn read(&mut self, path: &str) -> crate::Result<Vec<u8>> {
            let i = self.files.iter().position(|f| f.path == path);
            i.map(|i| self.data[i].clone())
                .ok_or_else(|| crate::Error::Format(format!("no {path}")))
        }
        fn describe(&self) -> String {
            "tree".into()
        }
    }

    #[test]
    fn the_wrapped_tree_lists_the_new_name_and_reads_the_old_file() {
        let entries = vec![
            ("eboot.bin", unity_eboot()),
            ("Media/Modules/Il2cppUserAssemblies.prx", b"module".to_vec()),
            ("Media/Modules/PS5Util.prx", b"util".to_vec()),
        ];
        let tree = Tree {
            files: entries
                .iter()
                .map(|(p, b)| SourceFile {
                    path: p.to_string(),
                    size: b.len() as u64,
                })
                .collect(),
            data: entries.into_iter().map(|(_, b)| b).collect(),
        };
        let (mut fixed, applied) = CaseFixed::wrap(Box::new(tree));
        assert_eq!(applied.len(), 1);
        let names: Vec<&str> = fixed.files().iter().map(|f| f.path.as_str()).collect();
        assert!(names.contains(&"Media/Modules/Il2CppUserAssemblies.prx"));
        assert!(!names.contains(&"Media/Modules/Il2cppUserAssemblies.prx"));
        assert_eq!(
            fixed
                .read("Media/Modules/Il2CppUserAssemblies.prx")
                .unwrap(),
            b"module"
        );
        assert_eq!(fixed.read("Media/Modules/PS5Util.prx").unwrap(), b"util");
        assert!(fixed
            .describe()
            .contains("Il2cppUserAssemblies.prx -> Il2CppUserAssemblies.prx"));
    }
}

/// `PS5UPLOAD_CASE_SOURCE=<folder or image> cargo test -p ps5upload-fpkg --lib real_source -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn real_source() {
    let path = std::env::var("PS5UPLOAD_CASE_SOURCE").expect("PS5UPLOAD_CASE_SOURCE");
    let mut tree = crate::source::open(std::path::Path::new(&path)).unwrap();
    let t = std::time::Instant::now();
    let found = tree_fixes(tree.as_mut());
    println!("{} in {:?}: {}", path, t.elapsed(), describe(&found));
}
