//! What a share offers: its items, each a fixed list of files made when the share is made. The
//! friend can only ask for a file by an item's id and a path on that list; nothing else is ever
//! read. A path is checked again when it is served: relative, no `..`, no symbolic link anywhere
//! below the item's root, and still a plain file.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

/// Files one item may hold (an app folder is a few thousand at most).
pub(crate) const MAX_FILES: usize = 200_000;
/// Folders deep.
const MAX_DEPTH: usize = 64;
/// What an item made of data (controls, stream settings, a note, which cheats are on) may hold.
pub(crate) const MAX_INLINE: usize = 256 * 1024;

/// What an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ItemKind {
    /// The app itself (homebrew only: retail.rs).
    App,
    /// A save: the shared-save file save_copy.rs makes.
    Save,
    /// Screenshots and video clips.
    Media,
    /// The cheat file and which cheats are on.
    Cheats,
    /// Button remaps, per-game controls, keyboard and mouse, macros (the app's own data).
    Controls,
    /// Stream settings for the game.
    Stream,
    /// A short note.
    Note,
}

impl ItemKind {
    pub(crate) fn word(self) -> &'static str {
        match self {
            ItemKind::App => "app",
            ItemKind::Save => "save",
            ItemKind::Media => "media",
            ItemKind::Cheats => "cheats",
            ItemKind::Controls => "controls",
            ItemKind::Stream => "stream",
            ItemKind::Note => "note",
        }
    }
}

/// Where one file's bytes come from.
#[derive(Clone)]
pub(crate) enum Loc {
    /// A file on this computer, below the item's root.
    Local(PathBuf),
    /// A file on a console, read through the helper.
    Console { addr: String, path: String },
    /// Data held in memory.
    Inline(Arc<Vec<u8>>),
}

impl std::fmt::Debug for Loc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Loc::Local(p) => write!(f, "Local({})", p.display()),
            Loc::Console { addr, path } => write!(f, "Console({addr}:{path})"),
            Loc::Inline(b) => write!(f, "Inline({} bytes)", b.len()),
        }
    }
}

/// One file of an item: the path the friend sees and asks for, its size, where it is.
#[derive(Debug, Clone)]
pub(crate) struct FileEntry {
    pub path: String,
    pub size: u64,
    pub loc: Loc,
}

/// One item of a share.
#[derive(Debug, Clone)]
pub(crate) struct Item {
    pub id: String,
    pub kind: ItemKind,
    /// What the friend sees.
    pub name: String,
    pub title_id: Option<String>,
    /// The app's format (`folder`, `pkg`, `mount.exfat`, …), for the receiving side's choices.
    pub format: Option<String>,
    /// For a local item, the folder its files must stay below (the item's own folder, or the
    /// folder holding its one file).
    pub root: Option<PathBuf>,
    pub files: Vec<FileEntry>,
    /// Symbolic links found and left out.
    pub links_skipped: usize,
    /// For the app: how it was built (retail.rs), which the friend's side checks too.
    pub build: Option<super::retail::Build>,
}

impl Item {
    pub(crate) fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// The item as the friend sees it before accepting: names and sizes only.
    pub(crate) fn listing(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "id": self.id,
            "kind": self.kind.word(),
            "name": self.name,
            "title_id": self.title_id,
            "format": self.format,
            "size": self.size(),
            "files": self.files.len(),
        });
        if let Some(b) = &self.build {
            v["build"] = b.json();
        }
        v
    }
}

/// Why a path may not be served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathRefusal {
    /// Not relative and plain (`..`, a leading `/`, `\`, a drive, an empty part, NUL).
    Unsafe,
    /// Not on the accepted items' lists.
    NotInManifest,
    /// A symbolic link on the way, or no longer a plain file.
    Link,
    /// Gone or changed since the share was made.
    Changed,
}

impl PathRefusal {
    pub(crate) fn word(self) -> &'static str {
        match self {
            PathRefusal::Unsafe => "unsafe_path",
            PathRefusal::NotInManifest => "not_in_manifest",
            PathRefusal::Link => "symlink",
            PathRefusal::Changed => "changed",
        }
    }
}

/// A path the friend may name: relative, `/`-separated, no `.` or `..`, no empty part, no `\`,
/// `:` or NUL, at most 1024 bytes.
pub(crate) fn rel_ok(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 1024
        && !p.starts_with('/')
        && !p.contains(['\\', '\0', ':'])
        && p.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

/// The file `path` of item `item_id`, if the friend may have it: on an accepted item's list,
/// exactly. Nothing else is looked at.
pub(crate) fn lookup<'a>(
    items: &'a [Item],
    accepted: &[String],
    item_id: &str,
    path: &str,
) -> Result<(&'a Item, &'a FileEntry), PathRefusal> {
    if !rel_ok(path) {
        return Err(PathRefusal::Unsafe);
    }
    if !accepted.iter().any(|a| a == item_id) {
        return Err(PathRefusal::NotInManifest);
    }
    let item = items
        .iter()
        .find(|i| i.id == item_id)
        .ok_or(PathRefusal::NotInManifest)?;
    let f = item
        .files
        .iter()
        .find(|f| f.path == path)
        .ok_or(PathRefusal::NotInManifest)?;
    Ok((item, f))
}

/// A local file on the list, checked again before it is opened: below `root`, no symbolic link
/// from `root` down (the root itself included), a plain file of at least the listed size.
pub(crate) fn check_local(root: &Path, file: &Path, size: u64) -> Result<(), PathRefusal> {
    let rel = file.strip_prefix(root).map_err(|_| PathRefusal::Unsafe)?;
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(PathRefusal::Unsafe);
    }
    let mut at = root.to_path_buf();
    let meta = std::fs::symlink_metadata(&at).map_err(|_| PathRefusal::Changed)?;
    if meta.file_type().is_symlink() {
        return Err(PathRefusal::Link);
    }
    for c in rel.components() {
        at.push(c);
        let meta = std::fs::symlink_metadata(&at).map_err(|_| PathRefusal::Changed)?;
        if meta.file_type().is_symlink() {
            return Err(PathRefusal::Link);
        }
    }
    let meta = std::fs::symlink_metadata(file).map_err(|_| PathRefusal::Changed)?;
    if !meta.is_file() {
        return Err(PathRefusal::Link);
    }
    if meta.len() < size {
        return Err(PathRefusal::Changed);
    }
    Ok(())
}

/// The files below the local folder `root`, as `(relative path, size, absolute path)`: symbolic
/// links left out (and counted), nothing that is not a plain file or a folder. Err when `root`
/// itself is a link or not a folder, or when there are too many files.
pub(crate) fn walk_local(root: &Path) -> Result<(Vec<FileEntry>, usize), String> {
    let meta = std::fs::symlink_metadata(root).map_err(|e| format!("{}: {e}", root.display()))?;
    if meta.file_type().is_symlink() {
        return Err("a shortcut (symbolic link) can't be shared".into());
    }
    if !meta.is_dir() {
        return Err(format!("{} is not a folder", root.display()));
    }
    let mut out = Vec::new();
    let mut links = 0usize;
    let mut stack: Vec<(PathBuf, String, usize)> = vec![(root.to_path_buf(), String::new(), 0)];
    while let Some((dir, rel, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            return Err("folders too deep to share".into());
        }
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let Some(name) = e.file_name().to_str().map(str::to_string) else {
                continue; // a name that is not UTF-8 can't be named to the friend
            };
            let p = format!("{rel}{}{name}", if rel.is_empty() { "" } else { "/" });
            if !rel_ok(&p) {
                continue;
            }
            let Ok(m) = std::fs::symlink_metadata(e.path()) else {
                continue;
            };
            if m.file_type().is_symlink() {
                links += 1;
            } else if m.is_dir() {
                stack.push((e.path(), p, depth + 1));
            } else if m.is_file() {
                out.push(FileEntry {
                    path: p,
                    size: m.len(),
                    loc: Loc::Local(e.path()),
                });
                if out.len() > MAX_FILES {
                    return Err("too many files to share".into());
                }
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((out, links))
}

/// One local file as an item's only file (`name` is what the friend sees and asks for). Err
/// when it is a link or not a plain file.
pub(crate) fn local_file(path: &Path) -> Result<FileEntry, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.file_type().is_symlink() {
        return Err("a shortcut (symbolic link) can't be shared".into());
    }
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| rel_ok(n))
        .ok_or("a file name that can't be shared")?;
    Ok(FileEntry {
        path: name.to_string(),
        size: meta.len(),
        loc: Loc::Local(path.to_path_buf()),
    })
}

/// The files below the console folder `root` (through the helper), symbolic links left out.
pub(crate) fn walk_console(addr: &str, root: &str) -> Result<(Vec<FileEntry>, usize), String> {
    use ps5upload_core::fs_ops::{list_dir, ListDirOptions};
    let root = root.trim_end_matches('/');
    if !root.starts_with('/') || root.split('/').any(|c| c == "..") {
        return Err("not a console path".into());
    }
    let mut out = Vec::new();
    let mut links = 0usize;
    let mut stack: Vec<(String, String, usize)> = vec![(root.to_string(), String::new(), 0)];
    while let Some((dir, rel, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            return Err("folders too deep to share".into());
        }
        let mut offset = 0u64;
        loop {
            let page = list_dir(addr, &dir, ListDirOptions { offset, limit: 256 })
                .map_err(|e| format!("{dir}: {e:#}"))?;
            let n = page.entries.len() as u64;
            for e in page.entries {
                let p = format!("{rel}{}{}", if rel.is_empty() { "" } else { "/" }, e.name);
                if !rel_ok(&p) {
                    continue;
                }
                match e.kind.as_str() {
                    "dir" => stack.push((format!("{dir}/{}", e.name), p, depth + 1)),
                    "file" => {
                        out.push(FileEntry {
                            path: p,
                            size: e.size,
                            loc: Loc::Console {
                                addr: addr.to_string(),
                                path: format!("{dir}/{}", e.name),
                            },
                        });
                        if out.len() > MAX_FILES {
                            return Err("too many files to share".into());
                        }
                    }
                    "link" | "symlink" => links += 1,
                    _ => {}
                }
            }
            if n == 0 || page.total_scanned <= offset + n {
                break;
            }
            offset += n;
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((out, links))
}

/// One console file as an item's file, sized by `fs.stat` (a link is refused).
pub(crate) fn console_file(addr: &str, path: &str) -> Result<FileEntry, String> {
    if !path.starts_with('/') || path.split('/').any(|c| c == "..") {
        return Err("not a console path".into());
    }
    let st = ps5upload_core::fs_ops::fs_stat(addr, path).map_err(|e| format!("{path}: {e:#}"))?;
    if st.kind != "file" {
        return Err(format!("{path} is not a file"));
    }
    let name = path
        .rsplit('/')
        .next()
        .filter(|n| rel_ok(n))
        .ok_or("a file name that can't be shared")?;
    Ok(FileEntry {
        path: name.to_string(),
        size: st.size,
        loc: Loc::Console {
            addr: addr.to_string(),
            path: path.to_string(),
        },
    })
}

/// Reads `len` bytes of `f` from `offset` (fewer at its end). Blocking.
pub(crate) fn read_at(
    item: &Item,
    f: &FileEntry,
    offset: u64,
    len: usize,
) -> Result<Vec<u8>, String> {
    let len = (len as u64).min(f.size.saturating_sub(offset)) as usize;
    if len == 0 {
        return Ok(Vec::new());
    }
    match &f.loc {
        Loc::Inline(b) => Ok(b
            .get(offset as usize..offset as usize + len)
            .ok_or("past the end")?
            .to_vec()),
        Loc::Local(p) => {
            use std::io::{Read, Seek, SeekFrom};
            let root = item.root.as_deref().ok_or("no root")?;
            check_local(root, p, f.size).map_err(|r| r.word().to_string())?;
            let mut file = std::fs::File::open(p).map_err(|e| format!("open: {e}"))?;
            // The file opened is the one checked (not one swapped in for a link since).
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let a = file.metadata().map_err(|e| format!("open: {e}"))?;
                let b = std::fs::symlink_metadata(p).map_err(|e| format!("open: {e}"))?;
                if a.ino() != b.ino() || a.dev() != b.dev() {
                    return Err(PathRefusal::Link.word().into());
                }
            }
            file.seek(SeekFrom::Start(offset))
                .map_err(|e| format!("seek: {e}"))?;
            let mut buf = vec![0u8; len];
            let mut got = 0;
            while got < len {
                match file.read(&mut buf[got..]) {
                    Ok(0) => break,
                    Ok(n) => got += n,
                    Err(e) => return Err(format!("read: {e}")),
                }
            }
            buf.truncate(got);
            Ok(buf)
        }
        Loc::Console { addr, path } => {
            let mut buf = Vec::with_capacity(len);
            while buf.len() < len {
                let at = offset + buf.len() as u64;
                let want = (len - buf.len()).min(2 * 1024 * 1024) as u64;
                let got = ps5upload_core::fs_ops::fs_read(addr, path, at, want)
                    .map_err(|e| format!("read: {e:#}"))?;
                if got.is_empty() {
                    break;
                }
                buf.extend_from_slice(&got);
            }
            Ok(buf)
        }
    }
}

/// The discovery format of a local app at `path` (as the Collection names it), or None for what
/// can't be identified (an archive).
pub(crate) fn local_format(path: &Path) -> Option<&'static str> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.is_dir() {
        return Some("folder");
    }
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    [
        (".pkg", "pkg"),
        (".exfat", "mount.exfat"),
        (".ffpkg", "mount.ffpkg"),
        (".ffpfsc", "mount.ffpfsc"),
        (".ffpfs", "mount.ffpfs"),
    ]
    .iter()
    .find(|(ext, _)| name.ends_with(ext))
    .map(|(_, f)| *f)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ps5u-share-{name}-{}-{}",
            std::process::id(),
            crate::join_net::random_b64(6).unwrap()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn item(root: &Path) -> Item {
        let (files, links) = walk_local(root).unwrap();
        Item {
            id: "i1".into(),
            kind: ItemKind::App,
            name: "App".into(),
            title_id: Some("PSUP00001".into()),
            format: Some("folder".into()),
            root: Some(root.to_path_buf()),
            files,
            links_skipped: links,
            build: None,
        }
    }

    #[test]
    fn unsafe_paths_are_refused_before_anything_is_looked_up() {
        for p in [
            "",
            "/etc/passwd",
            "../secret",
            "a/../../b",
            "a/./b",
            "a//b",
            "a\\b",
            "C:/x",
            "a\0b",
            "a/",
            ".",
        ] {
            assert!(!rel_ok(p), "{p:?} passed");
        }
        assert!(rel_ok("sce_sys/param.json"));
        assert!(rel_ok("eboot.bin"));
        assert!(rel_ok("..hidden"), "a name that starts with dots is a name");
    }

    #[test]
    fn only_files_on_an_accepted_items_list_are_served() {
        let root = tmp("manifest");
        std::fs::create_dir_all(root.join("sce_sys")).unwrap();
        std::fs::write(root.join("eboot.bin"), b"code").unwrap();
        std::fs::write(root.join("sce_sys/param.json"), b"{}").unwrap();
        let items = vec![item(&root)];
        let yes = vec!["i1".to_string()];
        assert!(lookup(&items, &yes, "i1", "eboot.bin").is_ok());
        assert!(lookup(&items, &yes, "i1", "sce_sys/param.json").is_ok());
        assert_eq!(
            lookup(&items, &yes, "i1", "missing.bin").unwrap_err(),
            PathRefusal::NotInManifest
        );
        assert_eq!(
            lookup(&items, &[], "i1", "eboot.bin").unwrap_err(),
            PathRefusal::NotInManifest,
            "an item the friend did not accept"
        );
        assert_eq!(
            lookup(&items, &yes, "i2", "eboot.bin").unwrap_err(),
            PathRefusal::NotInManifest
        );
        assert_eq!(
            lookup(&items, &yes, "i1", "../eboot.bin").unwrap_err(),
            PathRefusal::Unsafe
        );
        assert_eq!(
            lookup(&items, &yes, "i1", "/etc/passwd").unwrap_err(),
            PathRefusal::Unsafe
        );
        assert_eq!(
            lookup(&items, &yes, "i1", "sce_sys/../eboot.bin").unwrap_err(),
            PathRefusal::Unsafe
        );
        // A file added after the share was made is not on the list.
        std::fs::write(root.join("later.bin"), b"x").unwrap();
        assert_eq!(
            lookup(&items, &yes, "i1", "later.bin").unwrap_err(),
            PathRefusal::NotInManifest
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_left_out_and_refused_when_served() {
        let root = tmp("links");
        let outside = tmp("outside");
        std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("data/ok.bin"), b"fine").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linkdir")).unwrap();
        let it = item(&root);
        assert_eq!(it.links_skipped, 2);
        assert!(it.files.iter().all(|f| !f.path.contains("link")));
        assert_eq!(it.files.len(), 1);

        // A file on the list turned into a link after the share was made: refused.
        let f = it.files[0].clone();
        std::fs::remove_file(root.join("data/ok.bin")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("data/ok.bin")).unwrap();
        assert_eq!(
            read_at(&it, &f, 0, 100).unwrap_err(),
            PathRefusal::Link.word()
        );
        // …or a folder on the way did.
        std::fs::remove_file(root.join("data/ok.bin")).unwrap();
        std::fs::remove_dir(root.join("data")).unwrap();
        std::fs::create_dir_all(outside.join("d")).unwrap();
        std::fs::write(outside.join("d/ok.bin"), b"other").unwrap();
        std::os::unix::fs::symlink(outside.join("d"), root.join("data")).unwrap();
        assert_eq!(
            read_at(&it, &f, 0, 100).unwrap_err(),
            PathRefusal::Link.word()
        );
        // A root that is itself a link can't be shared.
        let link_root = outside.join("rootlink");
        std::os::unix::fs::symlink(&root, &link_root).unwrap();
        assert!(walk_local(&link_root).is_err());
        assert!(local_file(&outside.join("rootlink")).is_err());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn a_file_is_read_in_pieces_and_never_past_its_listed_size() {
        let root = tmp("read");
        std::fs::write(root.join("a.bin"), b"0123456789").unwrap();
        let it = item(&root);
        let f = &it.files[0];
        assert_eq!(read_at(&it, f, 0, 4).unwrap(), b"0123");
        assert_eq!(read_at(&it, f, 8, 100).unwrap(), b"89");
        assert!(read_at(&it, f, 10, 100).unwrap().is_empty());
        // Grown since: still the listed size.
        std::fs::write(root.join("a.bin"), b"0123456789ABCDEF").unwrap();
        assert_eq!(read_at(&it, f, 8, 100).unwrap(), b"89");
        // Shrunk: changed.
        std::fs::write(root.join("a.bin"), b"01").unwrap();
        assert_eq!(read_at(&it, f, 0, 4).unwrap_err(), "changed");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn formats_by_name() {
        let root = tmp("fmt");
        assert_eq!(local_format(&root), Some("folder"));
        for (n, f) in [
            ("a.pkg", Some("mount.exfat")),
            ("b.ffpkg", Some("mount.ffpkg")),
            ("c.ffpfs", Some("mount.ffpfs")),
            ("d.zip", None),
        ] {
            let p = root.join(n);
            std::fs::write(&p, b"x").unwrap();
            let want = if n == "a.pkg" { Some("pkg") } else { f };
            assert_eq!(local_format(&p), want, "{n}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
