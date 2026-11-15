//! Looking inside a game image or debug package on this computer (#479): what is in one folder
//! of it, one folder of it as a tree of its own (to save it to the computer), and the image's
//! tree with files added, replaced or deleted (to write an edited copy).
//!
//! Nothing here writes to an image. An edit is a tree that a builder writes out as a NEW image,
//! read back and checked like any Convert output; the image it came from is only ever read.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::source::{SourceFile, SourceTree};
use crate::{format_err, Error, Result};

/// The filesystem an image holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filesystem {
    Exfat,
    /// `.ffpkg`: UFS2.
    Ufs2,
    /// `.ffpfs`: PFS.
    Pfs,
    /// A debug-key `.pkg`.
    Pkg,
}

impl Filesystem {
    pub fn name(self) -> &'static str {
        match self {
            Self::Exfat => "exfat",
            Self::Ufs2 => "ufs2",
            Self::Pfs => "pfs",
            Self::Pkg => "pkg",
        }
    }

    /// An edited copy can be written: an image writer exists for it. A package is not rebuilt
    /// from here (that is Convert's job, with its own checks), so it is read-only.
    pub fn editable(self) -> bool {
        !matches!(self, Self::Pkg)
    }
}

/// An image opened for browsing.
pub struct Opened {
    pub tree: Box<dyn SourceTree>,
    pub filesystem: Filesystem,
    /// A `.ffpfsc`: the image is inside a compressed PFS container.
    pub compressed: bool,
}

/// Opens a game image or debug package on this computer, by its extension (as
/// [`crate::source::open`] does), saying what it holds.
pub fn open(path: &Path) -> Result<Opened> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let plain = |filesystem| -> Result<Opened> {
        Ok(Opened {
            tree: crate::source::open(path)?,
            filesystem,
            compressed: false,
        })
    };
    match ext.as_str() {
        "exfat" => plain(Filesystem::Exfat),
        "ffpkg" | "ufs2" => plain(Filesystem::Ufs2),
        "ffpfs" => plain(Filesystem::Pfs),
        "pkg" => plain(Filesystem::Pkg),
        "ffpfsc" => {
            let open =
                || -> Result<Box<dyn crate::ReadSeek>> { Ok(Box::new(std::fs::File::open(path)?)) };
            let (tree, filesystem) =
                crate::source::ffpfsc_tree_of(&open, &format!("ffpfsc {}", path.display()))?;
            Ok(Opened {
                tree,
                filesystem,
                compressed: true,
            })
        }
        _ => format_err(format!(
            "{} is not a game image (.exfat, .ffpkg, .ffpfs, .ffpfsc) or debug package (.pkg)",
            path.display()
        )),
    }
}

/// A path inside an image as the tree spells it: `/`-separated, no leading or trailing `/`,
/// the root as "". `None` for one that names no place in a tree (`..`, `.`, a `\`, a NUL).
pub fn normalize(raw: &str) -> Option<String> {
    let mut parts = Vec::new();
    for part in raw.split('/') {
        if part.is_empty() {
            continue;
        }
        if part == "." || part == ".." || part.contains('\\') || part.contains('\0') {
            return None;
        }
        parts.push(part);
    }
    Some(parts.join("/"))
}

/// The folder `path` is in ("" at the root).
fn parent(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

/// `path` and every folder above it, innermost first.
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    let mut at = Some(path);
    std::iter::from_fn(move || {
        let here = at?;
        at = here.rfind('/').map(|i| &here[..i]);
        Some(here)
    })
}

/// One thing in a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    /// A file's size; a folder's is the sum of every file under it.
    pub size: u64,
    /// Files under a folder (1 for a file).
    pub files: u64,
}

/// What a path in an image is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File(u64),
    Dir,
}

/// An image's file list, kept to answer folder listings without the image open.
#[derive(Debug, Clone, Default)]
pub struct Index {
    files: Vec<SourceFile>,
    empty_dirs: Vec<String>,
    dirs: BTreeSet<String>,
}

impl Index {
    pub fn new(tree: &dyn SourceTree) -> Self {
        let files = tree.files().to_vec();
        let empty_dirs: Vec<String> = tree
            .empty_dirs()
            .iter()
            .filter_map(|d| normalize(d))
            .filter(|d| !d.is_empty())
            .collect();
        let mut dirs = BTreeSet::new();
        for f in &files {
            for a in ancestors(parent(&f.path)).filter(|a| !a.is_empty()) {
                if !dirs.insert(a.to_string()) {
                    break;
                }
            }
        }
        for d in &empty_dirs {
            for a in ancestors(d) {
                dirs.insert(a.to_string());
            }
        }
        Self {
            files,
            empty_dirs,
            dirs,
        }
    }

    pub fn file_count(&self) -> u64 {
        self.files.len() as u64
    }

    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// What `path` (normalized) is, or `None` when the image has nothing there. The root is a
    /// folder.
    pub fn kind(&self, path: &str) -> Option<Kind> {
        if path.is_empty() || self.dirs.contains(path) {
            return Some(Kind::Dir);
        }
        self.files
            .iter()
            .find(|f| f.path == path)
            .map(|f| Kind::File(f.size))
    }

    /// What folder `dir` (normalized) holds, folders first, each group by name ignoring case.
    /// `None` when there is no such folder.
    pub fn list(&self, dir: &str) -> Option<Vec<Entry>> {
        if self.kind(dir) != Some(Kind::Dir) {
            return None;
        }
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let mut folders: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
        let mut files = Vec::new();
        for f in &self.files {
            let Some(rest) = f.path.strip_prefix(&prefix) else {
                continue;
            };
            match rest.split_once('/') {
                Some((name, _)) => {
                    let e = folders.entry(name).or_default();
                    e.0 += f.size;
                    e.1 += 1;
                }
                None => files.push(Entry {
                    name: rest.to_string(),
                    is_dir: false,
                    size: f.size,
                    files: 1,
                }),
            }
        }
        for d in &self.empty_dirs {
            if let Some(rest) = d.strip_prefix(&prefix) {
                let name = rest.split('/').next().unwrap_or(rest);
                folders.entry(name).or_default();
            }
        }
        let mut out: Vec<Entry> = folders
            .into_iter()
            .map(|(name, (size, files))| Entry {
                name: name.to_string(),
                is_dir: true,
                size,
                files,
            })
            .collect();
        let by_name = |a: &Entry, b: &Entry| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.name.cmp(&b.name))
        };
        out.sort_by(by_name);
        files.sort_by(by_name);
        out.extend(files);
        Some(out)
    }
}

/// One folder of a tree as a tree of its own: its files with the folder's path taken off, so
/// saving it writes the folder's contents and nothing above it.
pub struct Subtree {
    inner: Box<dyn SourceTree>,
    prefix: String,
    files: Vec<SourceFile>,
    empty_dirs: Vec<String>,
}

impl Subtree {
    /// The folder `dir` (normalized; "" is the whole tree) of `inner`. An error when `inner`
    /// has no such folder.
    pub fn new(inner: Box<dyn SourceTree>, dir: &str) -> Result<Self> {
        let index = Index::new(&*inner);
        if index.kind(dir) != Some(Kind::Dir) {
            return format_err(format!("{dir} is not a folder in {}", inner.describe()));
        }
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let files = inner
            .files()
            .iter()
            .filter_map(|f| {
                f.path.strip_prefix(&prefix).map(|p| SourceFile {
                    path: p.to_string(),
                    size: f.size,
                })
            })
            .collect();
        let empty_dirs = index
            .empty_dirs
            .iter()
            .filter_map(|d| d.strip_prefix(&prefix).map(str::to_string))
            .collect();
        Ok(Self {
            inner,
            prefix,
            files,
            empty_dirs,
        })
    }
}

impl SourceTree for Subtree {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }

    fn empty_dirs(&self) -> &[String] {
        &self.empty_dirs
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let full = format!("{}{path}", self.prefix);
        self.inner.read(&full)
    }

    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let full = format!("{}{path}", self.prefix);
        self.inner.read_range(&full, offset, len)
    }

    fn describe(&self) -> String {
        match self.prefix.strip_suffix('/') {
            Some(dir) => format!("{dir} of {}", self.inner.describe()),
            None => self.inner.describe(),
        }
    }

    fn notes(&self) -> Vec<String> {
        self.inner.notes()
    }
}

/// One change to an image's tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Remove the file or the whole folder at `path`. The folder it was in stays, empty or not.
    Delete { path: String },
    /// Put the file `from` (on this computer) at `path`, replacing a file there; or, when
    /// `from` is a folder, everything in it under the folder `path`, merged with what is there.
    Put { path: String, from: PathBuf },
}

/// Where a file of an edited tree comes from.
#[derive(Debug, Clone)]
enum Origin {
    Image,
    Local(PathBuf),
}

/// An image's tree with [`Edit`]s applied: what an edited copy is written from.
pub struct Edited {
    base: Box<dyn SourceTree>,
    files: Vec<SourceFile>,
    empty_dirs: Vec<String>,
    local: BTreeMap<String, PathBuf>,
    open: Option<(String, std::fs::File)>,
    changes: usize,
}

impl Edited {
    /// `base` with `edits` applied in order. Each edit is checked against the tree as the edits
    /// before it left it: a delete must name something there, a file cannot go where a folder
    /// is (or under a file), and every path must be a plain relative one.
    pub fn new(base: Box<dyn SourceTree>, edits: &[Edit]) -> Result<Self> {
        let mut files: BTreeMap<String, (u64, Origin)> = base
            .files()
            .iter()
            .map(|f| (f.path.clone(), (f.size, Origin::Image)))
            .collect();
        let mut dirs: BTreeSet<String> = base
            .empty_dirs()
            .iter()
            .filter_map(|d| normalize(d))
            .filter(|d| !d.is_empty())
            .collect();
        let under = |prefix: &str, path: &str| {
            path.len() > prefix.len() + 1
                && path.starts_with(prefix)
                && path.as_bytes()[prefix.len()] == b'/'
        };
        // Sorted keys: everything under `p/` follows `p/` directly.
        let is_dir = |files: &BTreeMap<String, (u64, Origin)>, dirs: &BTreeSet<String>, p: &str| {
            let slash = format!("{p}/");
            dirs.contains(p)
                || dirs
                    .range(slash.clone()..)
                    .next()
                    .is_some_and(|d| d.starts_with(&slash))
                || files
                    .range(slash.clone()..)
                    .next()
                    .is_some_and(|(f, _)| f.starts_with(&slash))
        };
        let checked = |raw: &str| -> Result<String> {
            match normalize(raw) {
                Some(p) if !p.is_empty() => Ok(p),
                _ => format_err(format!("{raw:?} is not a path inside the image")),
            }
        };
        for edit in edits {
            match edit {
                Edit::Delete { path } => {
                    let path = checked(path)?;
                    if files.remove(&path).is_none() {
                        if !is_dir(&files, &dirs, &path) {
                            return format_err(format!("{path} is not in the image"));
                        }
                        files.retain(|f, _| !under(&path, f));
                        dirs.retain(|d| *d != path && !under(&path, d));
                    }
                    // The folder it was in stays, as on a drive.
                    let up = parent(&path);
                    if !up.is_empty() {
                        dirs.insert(up.to_string());
                    }
                }
                Edit::Put { path, from } => {
                    let path = checked(path)?;
                    let meta = std::fs::metadata(from)
                        .map_err(|e| Error::Format(format!("{}: {e}", from.display())))?;
                    let mut put: Vec<(String, u64, PathBuf)> = Vec::new();
                    let mut put_dirs: Vec<String> = Vec::new();
                    if meta.is_dir() {
                        if files.contains_key(&path) {
                            return format_err(format!("{path} is a file in the image"));
                        }
                        let (found, empty) = crate::source::scan_tree(from)?;
                        if found.is_empty() && empty.is_empty() {
                            put_dirs.push(path.clone());
                        }
                        for f in found {
                            put.push((format!("{path}/{}", f.path), f.size, from.join(&f.path)));
                        }
                        put_dirs.extend(empty.iter().map(|d| format!("{path}/{d}")));
                    } else {
                        if is_dir(&files, &dirs, &path) {
                            return format_err(format!("{path} is a folder in the image"));
                        }
                        put.push((path.clone(), meta.len(), from.clone()));
                    }
                    for (p, _, _) in &put {
                        if normalize(p).as_deref() != Some(p.as_str()) {
                            return format_err(format!("{p:?} is not a path inside the image"));
                        }
                        if let Some(file) =
                            ancestors(parent(p)).find(|a| !a.is_empty() && files.contains_key(*a))
                        {
                            return format_err(format!("{file} is a file in the image"));
                        }
                        if is_dir(&files, &dirs, p) {
                            return format_err(format!("{p} is a folder in the image"));
                        }
                    }
                    for (p, size, local) in put {
                        files.insert(p, (size, Origin::Local(local)));
                    }
                    dirs.extend(put_dirs);
                }
            }
        }
        // A folder is listed as empty only when nothing at all is under it.
        let empty_dirs: Vec<String> = dirs
            .iter()
            .filter(|d| {
                let slash = format!("{d}/");
                !files
                    .range(slash.clone()..)
                    .next()
                    .is_some_and(|(f, _)| f.starts_with(&slash))
                    && !dirs
                        .range(slash.clone()..)
                        .next()
                        .is_some_and(|o| o.starts_with(&slash))
            })
            .cloned()
            .collect();
        let mut local = BTreeMap::new();
        let files = files
            .into_iter()
            .map(|(path, (size, origin))| {
                if let Origin::Local(from) = origin {
                    local.insert(path.clone(), from);
                }
                SourceFile { path, size }
            })
            .collect();
        Ok(Self {
            base,
            files,
            empty_dirs,
            local,
            open: None,
            changes: edits.len(),
        })
    }

    fn size_of(&self, path: &str) -> Result<u64> {
        self.files
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.size)
            .ok_or_else(|| Error::Format(format!("{path} is not in the edited image")))
    }
}

impl SourceTree for Edited {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }

    fn empty_dirs(&self) -> &[String] {
        &self.empty_dirs
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let size = self.size_of(path)?;
        self.read_range(path, 0, usize::try_from(size).unwrap_or(usize::MAX))
    }

    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let Some(from) = self.local.get(path) else {
            return self.base.read_range(path, offset, len);
        };
        if self.open.as_ref().is_none_or(|(p, _)| p != path) {
            let f = std::fs::File::open(from)
                .map_err(|e| Error::Format(format!("{}: {e}", from.display())))?;
            self.open = Some((path.to_string(), f));
        }
        // A file that changed size since the edit was planned would leave the image's sizes
        // wrong: refuse it rather than write it.
        let size = self.size_of(path)?;
        let Some((_, f)) = self.open.as_mut() else {
            unreachable!("opened above")
        };
        if f.metadata()?.len() != size {
            return format_err(format!(
                "{} changed while the image was being written",
                from.display()
            ));
        }
        let end = offset.saturating_add(len as u64).min(size);
        let mut buf = Vec::with_capacity(end.saturating_sub(offset) as usize);
        if offset < end {
            f.seek(SeekFrom::Start(offset))?;
            f.by_ref().take(end - offset).read_to_end(&mut buf)?;
        }
        Ok(buf)
    }

    fn describe(&self) -> String {
        format!("{} with {} change(s)", self.base.describe(), self.changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mem {
        files: Vec<SourceFile>,
        data: BTreeMap<String, Vec<u8>>,
        empty: Vec<String>,
    }

    impl Mem {
        fn new(items: &[(&str, &[u8])], empty: &[&str]) -> Box<Self> {
            Box::new(Self {
                files: items
                    .iter()
                    .map(|(p, d)| SourceFile {
                        path: (*p).into(),
                        size: d.len() as u64,
                    })
                    .collect(),
                data: items
                    .iter()
                    .map(|(p, d)| ((*p).to_string(), d.to_vec()))
                    .collect(),
                empty: empty.iter().map(|s| (*s).to_string()).collect(),
            })
        }
    }

    impl SourceTree for Mem {
        fn files(&self) -> &[SourceFile] {
            &self.files
        }
        fn read(&mut self, path: &str) -> Result<Vec<u8>> {
            self.data
                .get(path)
                .cloned()
                .ok_or_else(|| Error::Format(format!("no {path}")))
        }
        fn empty_dirs(&self) -> &[String] {
            &self.empty
        }
        fn describe(&self) -> String {
            "memory".into()
        }
    }

    fn game() -> Box<Mem> {
        Mem::new(
            &[
                ("eboot.bin", b"EBOOT"),
                ("sce_sys/param.json", b"{}"),
                ("data/a.bin", b"aaaa"),
                ("data/sub/b.bin", b"bb"),
                ("Data2/c.bin", b"c"),
            ],
            &["data/empty", "hollow/deeper"],
        )
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fpkg-browse-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| {
                if e.is_dir {
                    format!("{}/ {} {}", e.name, e.size, e.files)
                } else {
                    format!("{} {}", e.name, e.size)
                }
            })
            .collect()
    }

    fn all_files(tree: &mut dyn SourceTree) -> Vec<(String, Vec<u8>)> {
        let paths: Vec<String> = tree.files().iter().map(|f| f.path.clone()).collect();
        paths
            .into_iter()
            .map(|p| {
                let bytes = tree.read(&p).unwrap();
                (p, bytes)
            })
            .collect()
    }

    #[test]
    fn paths_normalize_and_escapes_are_refused() {
        assert_eq!(normalize("/data//sub/").as_deref(), Some("data/sub"));
        assert_eq!(normalize("").as_deref(), Some(""));
        assert_eq!(normalize("/").as_deref(), Some(""));
        assert_eq!(normalize("data/../x"), None);
        assert_eq!(normalize("./x"), None);
        assert_eq!(normalize("a\\b"), None);
    }

    #[test]
    fn a_folder_lists_folders_first_with_their_totals() {
        let index = Index::new(&*game());
        assert_eq!(
            names(&index.list("").unwrap()),
            [
                "data/ 6 2",
                "Data2/ 1 1",
                "hollow/ 0 0",
                "sce_sys/ 2 1",
                "eboot.bin 5"
            ]
        );
        assert_eq!(
            names(&index.list("data").unwrap()),
            ["empty/ 0 0", "sub/ 2 1", "a.bin 4"]
        );
        assert_eq!(names(&index.list("hollow").unwrap()), ["deeper/ 0 0"]);
        assert!(index.list("hollow/deeper").unwrap().is_empty());
        assert_eq!(index.list("nope"), None);
        assert_eq!(index.list("eboot.bin"), None, "a file is not a folder");
        assert_eq!(index.kind("data/a.bin"), Some(Kind::File(4)));
        assert_eq!(index.kind("data/sub"), Some(Kind::Dir));
        assert_eq!(index.file_count(), 5);
        assert_eq!(index.total_bytes(), 14);
    }

    #[test]
    fn a_subtree_is_the_folder_without_its_path() {
        let mut sub = Subtree::new(game(), "data").unwrap();
        assert_eq!(
            all_files(&mut sub),
            [
                ("a.bin".to_string(), b"aaaa".to_vec()),
                ("sub/b.bin".to_string(), b"bb".to_vec())
            ]
        );
        assert_eq!(sub.empty_dirs(), ["empty"]);
        assert_eq!(sub.read_range("a.bin", 1, 2).unwrap(), b"aa");
        assert!(Subtree::new(game(), "eboot.bin").is_err());
        assert!(Subtree::new(game(), "missing").is_err());
        // "Data2" is not under "data", whatever a prefix match would say.
        let sub = Subtree::new(game(), "Data2").unwrap();
        assert_eq!(sub.files().len(), 1);
    }

    #[test]
    fn edits_add_replace_and_delete() {
        let dir = scratch("edits");
        std::fs::write(dir.join("new.bin"), b"NEW!").unwrap();
        std::fs::write(dir.join("eboot.bin"), b"PATCHED").unwrap();
        std::fs::create_dir_all(dir.join("patch/inner/void")).unwrap();
        std::fs::write(dir.join("patch/x.bin"), b"x").unwrap();
        std::fs::write(dir.join("patch/inner/y.bin"), b"yy").unwrap();
        let edits = [
            Edit::Put {
                path: "eboot.bin".into(),
                from: dir.join("eboot.bin"),
            },
            Edit::Put {
                path: "data/new.bin".into(),
                from: dir.join("new.bin"),
            },
            Edit::Put {
                path: "data/sub".into(),
                from: dir.join("patch"),
            },
            Edit::Delete {
                path: "sce_sys/param.json".into(),
            },
            Edit::Delete {
                path: "hollow".into(),
            },
            Edit::Delete {
                path: "Data2".into(),
            },
        ];
        let mut edited = Edited::new(game(), &edits).unwrap();
        assert_eq!(
            all_files(&mut edited),
            [
                ("data/a.bin".to_string(), b"aaaa".to_vec()),
                ("data/new.bin".to_string(), b"NEW!".to_vec()),
                ("data/sub/b.bin".to_string(), b"bb".to_vec()),
                ("data/sub/inner/y.bin".to_string(), b"yy".to_vec()),
                ("data/sub/x.bin".to_string(), b"x".to_vec()),
                ("eboot.bin".to_string(), b"PATCHED".to_vec()),
            ]
        );
        // The emptied sce_sys stays; the deleted folders go; the added folder's empty one comes.
        assert_eq!(
            edited.empty_dirs(),
            ["data/empty", "data/sub/inner/void", "sce_sys"]
        );
        assert_eq!(edited.read_range("eboot.bin", 2, 100).unwrap(), b"TCHED");
        assert_eq!(edited.read_range("eboot.bin", 50, 10).unwrap(), b"");

        // A local file that changes size after planning is refused, not written short.
        std::fs::write(dir.join("new.bin"), b"longer now").unwrap();
        let err = edited.read("data/new.bin").unwrap_err().to_string();
        assert!(err.contains("changed"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_edits_are_refused() {
        let dir = scratch("bad");
        std::fs::write(dir.join("f.bin"), b"f").unwrap();
        std::fs::create_dir_all(dir.join("folder")).unwrap();
        let put = |path: &str, from: &str| Edit::Put {
            path: path.into(),
            from: dir.join(from),
        };
        let del = |path: &str| Edit::Delete { path: path.into() };
        for (edit, want) in [
            (del("missing.bin"), "not in the image"),
            (del(""), "not a path"),
            (del("../x"), "not a path"),
            (put("data", "f.bin"), "is a folder"),
            (put("eboot.bin/x.bin", "f.bin"), "eboot.bin is a file"),
            (put("eboot.bin", "folder"), "is a file"),
            (put("x.bin", "absent"), "absent"),
        ] {
            let err = match Edited::new(game(), std::slice::from_ref(&edit)) {
                Ok(_) => panic!("{edit:?} must be refused"),
                Err(e) => e.to_string(),
            };
            assert!(err.contains(want), "{edit:?}: {err}");
        }
        // A delete sees what the edits before it did.
        assert!(Edited::new(game(), &[del("data"), del("data/a.bin")]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A real (small) exFAT image opens by extension, says what it is, lists and reads back.
    #[test]
    fn an_exfat_image_opens_and_lists() {
        let dir = scratch("exfat");
        let mut src = game();
        let plan = crate::exfat_write::plan_exfat(&*src, None, 0).unwrap();
        let image = dir.join("game.exfat");
        let mut out = std::fs::File::create(&image).unwrap();
        crate::exfat_write::write_exfat(&plan, &mut *src, &mut out, &mut |_, _| {}, &|| false)
            .unwrap();
        drop(out);
        let mut opened = open(&image).unwrap();
        assert_eq!(opened.filesystem, Filesystem::Exfat);
        assert!(!opened.compressed && opened.filesystem.editable());
        let index = Index::new(&*opened.tree);
        assert_eq!(
            names(&index.list("data").unwrap()),
            ["empty/ 0 0", "sub/ 2 1", "a.bin 4"]
        );
        assert_eq!(opened.tree.read("data/sub/b.bin").unwrap(), b"bb");
        let mut sub = Subtree::new(opened.tree, "sce_sys").unwrap();
        assert_eq!(sub.read("param.json").unwrap(), b"{}");
        assert!(open(&dir.join("game.txt")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
