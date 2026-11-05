//! Walking the roots: discovery, sizes (cached by a folder's depth-1 signature), identity
//! (cached by size and mtime), then grouping and covers.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use super::{
    game_id_in, group, identify, iso_utc, split, FolderCacheEntry, Library, Location,
    PkgCacheEntry, PkgInfo, INDEX_VERSION, ITEM_EXTS,
};

/// Deep enough for any sane arrangement, shallow enough that an extracted game tree cannot
/// take the scan on a long walk. The organizer walks as far, so it never moves what the
/// Collection cannot see.
pub const MAX_DEPTH: usize = 6;

/// A later file of an item that is more than one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub rel: String,
    pub abs: String,
    pub size: u64,
    pub mtime: i64,
    /// A `_N.pkg` whose first bytes were read and held no package header. (Unread, it counts as
    /// a part for this scan and is looked at again the next.)
    pub read: bool,
}

/// One thing discovery found: (path relative to its root, absolute path, type). For a root on a
/// saved server the absolute path is a `remote://` path, which every reader here accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub rel: String,
    pub abs: String,
    pub kind: String,
    /// (size, mtime, created) from the listing that found it, so no second look is needed. For
    /// an item of several files: their sizes added up and the newest time.
    pub stat: (u64, i64, i64),
    /// The later parts of a split package, or the later volumes of a `.rar` set.
    pub parts: Vec<Part>,
    /// A split package with a part missing from its run.
    pub gap: bool,
    /// A game folder's depth-1 signature, from the listing that found it.
    pub sig: Option<String>,
}

/// One walk of a root.
#[derive(Debug, Default)]
pub struct Discovery {
    pub items: Vec<Found>,
    /// Folders that could not be read (relative to the root, "" for the root itself; absolute;
    /// why): what is under them is unknown, not gone.
    pub unreadable: Vec<(String, String, String)>,
}

fn visible(name: &str) -> bool {
    !name.starts_with('.')
}

/// The item type a file name denotes.
pub fn file_type(name: &str) -> Option<&'static str> {
    let low = name.to_ascii_lowercase();
    ITEM_EXTS
        .iter()
        .find(|(ext, _)| low.ends_with(ext))
        .map(|(_, kind)| *kind)
}

/// One directory entry, with what a listing says about it.
#[derive(Debug, Clone)]
pub struct Ent {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub mtime: i64,
    /// Birth time where the file system keeps one; 0 otherwise.
    pub created: i64,
}

/// Where a root's files are: this computer's disk, or a saved server. A server root's listings
/// carry sizes and times, so walking it costs one round trip per folder.
pub enum Fsys {
    Local,
    Remote {
        fs: std::sync::Arc<crate::remote::source_fs::RemoteSourceFs>,
    },
}

impl Fsys {
    /// The backend for `root`. A server root needs the runtime to sign in (the scan thread
    /// passes the engine's).
    pub fn for_root(root: &str, rt: Option<&tokio::runtime::Handle>) -> Result<Self, String> {
        if !crate::remote::path::is_remote(root) {
            return if Path::new(root).is_dir() {
                Ok(Fsys::Local)
            } else {
                Err(format!("{root} is not a folder this engine can read"))
            };
        }
        let rt = rt.ok_or_else(|| format!("{root}: no runtime to reach the server from"))?;
        crate::remote::path::parse(root).map_err(|e| e.to_string())?;
        let (fs, _) = rt
            .block_on(crate::remote::source_fs::RemoteSourceFs::for_path(root))
            .map_err(|e| format!("{root}: {e}"))?;
        let f = Fsys::Remote { fs };
        if f.try_list(root).is_err() {
            return Err(format!("{root} could not be listed on the server"));
        }
        Ok(f)
    }

    fn try_list(&self, dir: &str) -> std::io::Result<Vec<Ent>> {
        match self {
            Fsys::Local => {
                let rd = std::fs::read_dir(dir)?;
                Ok(rd
                    .flatten()
                    .map(|e| {
                        let m = std::fs::symlink_metadata(e.path()).ok();
                        let ft = m.as_ref().map(|m| m.file_type());
                        let mtime = m
                            .as_ref()
                            .and_then(|m| m.modified().ok())
                            .map(secs)
                            .unwrap_or(0);
                        let created = m
                            .as_ref()
                            .and_then(|m| m.created().ok())
                            .map(secs)
                            .unwrap_or(0);
                        Ent {
                            name: e.file_name().to_string_lossy().into_owned(),
                            path: e.path().to_string_lossy().into_owned(),
                            is_dir: ft.is_some_and(|t| t.is_dir()),
                            is_symlink: ft.is_some_and(|t| t.is_symlink()),
                            size: m.as_ref().map(|m| m.len()).unwrap_or(0),
                            mtime,
                            created,
                        }
                    })
                    .collect())
            }
            Fsys::Remote { fs, .. } => {
                let server = crate::remote::path::parse(dir)
                    .map_err(|e| std::io::Error::other(e.to_string()))?
                    .path;
                let children = fs.entries(&server)?;
                // Each name encoded into the `remote://` path, so that reading it back gives
                // this name ("a%20b.pkg" stays itself, a backslash stays a name character).
                let at = dir.trim_end_matches('/');
                Ok(children
                    .into_iter()
                    .map(|e| Ent {
                        path: format!("{at}/{}", crate::remote::path::encode_segment(&e.name)),
                        name: e.name,
                        is_dir: e.is_dir,
                        is_symlink: false,
                        size: e.size,
                        mtime: e.mtime.filter(|t| *t > 0).unwrap_or(0),
                        created: 0,
                    })
                    .collect())
            }
        }
    }

    /// The visible entries of `dir`, sorted by name.
    pub fn list(&self, dir: &str) -> std::io::Result<Vec<Ent>> {
        let mut v: Vec<Ent> = self
            .try_list(dir)?
            .into_iter()
            .filter(|e| visible(&e.name))
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    /// A file's first four bytes, or `None` when they could not be read.
    fn head(&self, path: &str) -> Option<[u8; 4]> {
        use std::io::Read;
        let mut head = [0u8; 4];
        match self {
            Fsys::Local => std::fs::File::open(path).ok()?.read_exact(&mut head).ok()?,
            Fsys::Remote { fs, .. } => {
                use ps5upload_core::source_fs::SourceFs;
                let server = crate::remote::path::parse(path).ok()?.path;
                fs.open(Path::new(&server))
                    .ok()?
                    .read_exact(&mut head)
                    .ok()?
            }
        }
        Some(head)
    }
}

/// An extracted game keeps its metadata at `sce_sys/param.json` (PS5) or `param.sfo` (PS4).
/// `Err` when its `sce_sys` could not be read: then what it is is unknown.
fn is_app_folder(fs: &Fsys, entries: &[Ent]) -> std::io::Result<bool> {
    let Some(sys) = entries.iter().find(|e| e.is_dir && e.name == "sce_sys") else {
        return Ok(false);
    };
    match fs.list(&sys.path) {
        Ok(sys) => Ok(sys
            .iter()
            .any(|e| !e.is_dir && (e.name == "param.json" || e.name == "param.sfo"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// The `.rar` sets among a folder's files, each from its first volume in volume order:
/// `game.part1.rar`, `game.part2.rar`…; `game.rar`, `game.r00`, `game.r01`…
fn rar_sets(entries: &[Ent]) -> Vec<Vec<&Ent>> {
    let files = || entries.iter().filter(|e| !e.is_dir && !e.is_symlink);
    let mut sets: BTreeMap<(bool, String), Vec<(u32, &Ent)>> = BTreeMap::new();
    for e in files() {
        let low = e.name.to_ascii_lowercase();
        let Some(base) = low.strip_suffix(".rar") else {
            continue;
        };
        let numbered = base.rfind(".part").and_then(|at| {
            let n = base.get(at + 5..)?.parse::<u32>().ok()?;
            Some((base.get(..at)?, n))
        });
        match numbered {
            Some((stem, n)) => sets
                .entry((true, stem.to_string()))
                .or_default()
                .push((n, e)),
            None => sets
                .entry((false, base.to_string()))
                .or_default()
                .push((0, e)),
        }
    }
    // An old-style volume belongs to the `.rar` it continues; without one it is nothing.
    for e in files() {
        if let Some((stem, i)) = ps5upload_core::archive_extract::rar_old_volume_index(&e.name) {
            if let Some(set) = sets.get_mut(&(false, stem.to_ascii_lowercase())) {
                set.push((i + 1, e));
            }
        }
    }
    sets.into_values()
        .map(|mut set| {
            set.sort_by_key(|(n, _)| *n);
            set.into_iter().map(|(_, e)| e).collect()
        })
        .collect()
}

/// Whether a `_N.pkg` with a part 0 beside it is a package of its own. The index remembers the
/// answer while the file is unchanged, so a folder of parts on a server costs no reads per scan.
/// `Err` when the file could not be read.
fn is_own_package(fs: &Fsys, prev: &Library, key: &str, e: &Ent) -> Result<bool, ()> {
    let known = prev
        .pkg_cache
        .get(key)
        .filter(|c| c.size == e.size && c.mtime == e.mtime && c.v == super::IDENTIFY_VERSION);
    if let Some(c) = known {
        if c.pkg.error.as_deref() == Some(identify::SPLIT_PART) {
            return Ok(false);
        }
        if !c.pkg.title_id.is_empty() || !c.pkg.content_id.is_empty() {
            return Ok(true);
        }
    }
    match fs.head(&e.path) {
        Some(head) => Ok(split::is_package_head(&head)),
        None => Err(()),
    }
}

/// Every game item under `root`. A directory is an item when it is an extracted game; otherwise
/// it is only a place to look further, whatever else lies in it. Files are items by what they
/// are, and the files of a split package or a `.rar` set are one item. Symlinks are not followed.
/// `prev` is the last scan, for what it already knows about split parts.
pub fn discover(fs: &Fsys, root: &str, prev: &Library, cancel: &AtomicBool) -> Discovery {
    struct Walk<'a> {
        fs: &'a Fsys,
        root: &'a str,
        prev: &'a Library,
        cancel: &'a AtomicBool,
        out: Discovery,
    }
    impl Walk<'_> {
        fn rel_of(&self, full: &str) -> String {
            let rest = full.strip_prefix(self.root).unwrap_or(full);
            match self.fs {
                // Server paths are `/`-separated, their names encoded: the relative path is names.
                Fsys::Remote { .. } => {
                    crate::remote::path::decode_rel(rest.trim_start_matches('/'))
                }
                Fsys::Local => rest.trim_start_matches(['/', '\\']).replace('\\', "/"),
            }
        }

        fn unreadable(&mut self, dir: &str, why: &std::io::Error) {
            crate::log_warn!("collection: could not read {dir}: {why}");
            let rel = self.rel_of(dir);
            self.out
                .unreadable
                .push((rel, dir.to_string(), why.to_string()));
        }

        /// `dir`'s entries. A folder that went away since its parent was listed is empty; one
        /// that "went away" along with the root is a drive unplugged mid-walk, which is not.
        fn entries(&mut self, dir: &str) -> Option<Vec<Ent>> {
            match self.fs.list(dir) {
                Ok(v) => Some(v),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if dir != self.root && self.fs.try_list(self.root).is_ok() {
                        Some(Vec::new())
                    } else {
                        self.unreadable(dir, &e);
                        None
                    }
                }
                Err(e) => {
                    self.unreadable(dir, &e);
                    None
                }
            }
        }

        fn part(&self, e: &Ent, read: bool) -> Part {
            Part {
                rel: self.rel_of(&e.path),
                abs: e.path.clone(),
                size: e.size,
                mtime: e.mtime,
                read,
            }
        }

        /// One item of several files, in order.
        fn set(&self, kind: &str, files: &[&Ent], unread: &HashSet<String>, gap: bool) -> Found {
            let lead = files[0];
            let parts: Vec<Part> = files[1..]
                .iter()
                .map(|e| self.part(e, !unread.contains(&e.name)))
                .collect();
            Found {
                rel: self.rel_of(&lead.path),
                abs: lead.path.clone(),
                kind: kind.into(),
                stat: (
                    files.iter().map(|e| e.size).sum(),
                    files.iter().map(|e| e.mtime).max().unwrap_or(0),
                    lead.created,
                ),
                parts,
                gap,
                sig: None,
            }
        }

        fn folder(&mut self, entries: &[Ent], depth: usize) {
            let files: Vec<&Ent> = entries
                .iter()
                .filter(|e| !e.is_dir && !e.is_symlink)
                .collect();
            let names: Vec<&str> = files.iter().map(|e| e.name.as_str()).collect();
            let by_name: BTreeMap<&str, &Ent> =
                files.iter().map(|e| (e.name.as_str(), *e)).collect();
            let mut unread: HashSet<String> = HashSet::new();
            let sets = split::split_sets(&names, &mut |n| {
                let Some(e) = by_name.get(n) else {
                    return false;
                };
                let key = format!("{}|{}", self.root, self.rel_of(&e.path));
                is_own_package(self.fs, self.prev, &key, e).unwrap_or_else(|()| {
                    unread.insert(n.to_string());
                    false
                })
            });
            let in_a_set: HashSet<&str> = sets
                .iter()
                .flat_map(|s| s.parts.iter().map(|(n, _)| n.as_str()))
                .collect();
            let rars = rar_sets(entries);
            for e in entries {
                if self.cancel.load(Ordering::Relaxed) {
                    return;
                }
                if e.is_symlink {
                    continue;
                }
                if e.is_dir {
                    let Some(inner) = self.entries(&e.path) else {
                        continue;
                    };
                    match is_app_folder(self.fs, &inner) {
                        Err(why) => self.unreadable(&e.path, &why),
                        Ok(true) => self.out.items.push(Found {
                            rel: self.rel_of(&e.path),
                            abs: e.path.clone(),
                            kind: "folder".into(),
                            stat: (e.size, e.mtime, e.created),
                            parts: Vec::new(),
                            gap: false,
                            sig: Some(folder_signature(&inner)),
                        }),
                        Ok(false) if depth < MAX_DEPTH => self.folder(&inner, depth + 1),
                        Ok(false) => {}
                    }
                } else if in_a_set.contains(e.name.as_str()) {
                    // Counted with its set's first file.
                } else if let Some(set) = sets.iter().find(|s| s.lead == e.name) {
                    let mut files = vec![e];
                    files.extend(
                        set.parts
                            .iter()
                            .filter_map(|(n, _)| by_name.get(n.as_str())),
                    );
                    let found = self.set("pkg", &files, &unread, set.has_gap());
                    self.out.items.push(found);
                } else if let Some(set) = rars.iter().find(|s| s[0].name == e.name) {
                    let found = self.set("rar", set, &unread, false);
                    self.out.items.push(found);
                } else if let Some(kind) = file_type(&e.name) {
                    self.out.items.push(Found {
                        rel: self.rel_of(&e.path),
                        abs: e.path.clone(),
                        kind: kind.into(),
                        stat: (e.size, e.mtime, e.created),
                        parts: Vec::new(),
                        gap: false,
                        sig: None,
                    });
                }
            }
        }
    }
    let mut walk = Walk {
        fs,
        root,
        prev,
        cancel,
        out: Discovery::default(),
    };
    if let Some(entries) = walk.entries(root) {
        walk.folder(&entries, 0);
    }
    walk.out
}

/// A cheap depth-1 fingerprint of a folder: name, size and mtime of its immediate children.
fn folder_signature(entries: &[Ent]) -> String {
    let mut parts: Vec<String> = entries
        .iter()
        .map(|e| format!("{}:{}:{}", e.name, e.size, e.mtime))
        .collect();
    parts.sort();
    blake3::hash(parts.join("\n").as_bytes()).to_hex()[..16].to_string()
}

/// The bytes under a folder, and whether every folder in it could be read (otherwise the total
/// is short).
fn deep_size(fs: &Fsys, path: &str) -> (u64, bool) {
    let mut total = 0u64;
    let mut whole = true;
    let mut stack = vec![path.to_string()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs.try_list(&dir) else {
            whole = false;
            continue;
        };
        for e in entries {
            if e.is_symlink {
                continue;
            }
            if e.is_dir {
                stack.push(e.path);
            } else {
                total += e.size;
            }
        }
    }
    (total, whole)
}

/// Whether the relative path `rel` is `dir` or below it ("" is the whole root).
fn under(rel: &str, dir: &str) -> bool {
    dir.is_empty()
        || rel
            .strip_prefix(dir)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// What the last scan knew below folders this one could not read stays: a listing that failed
/// says nothing about what is there, and reading again later costs a read of every package.
fn keep_unread(
    prev: &Library,
    root: &str,
    unread: &[String],
    found: &HashSet<String>,
    lib: &mut Library,
    games: &mut BTreeMap<String, super::Game>,
) {
    if unread.is_empty() {
        return;
    }
    for g in prev.games.values() {
        for l in g.locations.iter().filter(|l| l.root == root) {
            if !found.contains(&l.path) && unread.iter().any(|d| under(&l.path, d)) {
                group::place(games, &g.game_id, l.clone());
            }
        }
    }
    let mine = format!("{root}|");
    let kept = |key: &str| {
        key.strip_prefix(&mine)
            .is_some_and(|rel| unread.iter().any(|d| under(rel, d)))
    };
    for (k, v) in prev.folder_cache.range(mine.clone()..) {
        if !k.starts_with(&mine) {
            break;
        }
        if kept(k) {
            lib.folder_cache
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
    }
    for (k, v) in prev.pkg_cache.range(mine.clone()..) {
        if !k.starts_with(&mine) {
            break;
        }
        if kept(k) {
            lib.pkg_cache.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
}

fn secs(t: std::time::SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// When an item arrived (its birth time, where the file system keeps one) and when it last
/// changed.
fn dates(stat: Option<(u64, i64, i64)>) -> (i64, i64, &'static str) {
    let Some((_, mtime, created)) = stat else {
        return (0, 0, "modified");
    };
    if created > 0 {
        (created, mtime, "created")
    } else {
        (mtime, mtime, "modified")
    }
}

/// Live progress of a scan, read by the API while it runs.
#[derive(Default)]
pub struct Progress {
    /// Items found so far (each root's are added once that root has been walked).
    pub found: AtomicU64,
    /// Items read and identified.
    pub done: AtomicU64,
    /// What the scan is doing: one of the `PHASE_*` values.
    pub phase: AtomicU8,
    pub roots_total: AtomicU64,
    pub roots_done: AtomicU64,
    /// Games given their name and cover at the end (looked up online when they can't name
    /// themselves, cover read out of the game otherwise).
    pub games_total: AtomicU64,
    pub games_done: AtomicU64,
    /// The folder being searched, or the item being read.
    current: Mutex<String>,
}

pub const PHASE_FINDING: u8 = 0;
pub const PHASE_READING: u8 = 1;
pub const PHASE_NAMING: u8 = 2;
pub const PHASE_SAVING: u8 = 3;

impl Progress {
    pub fn set_current(&self, what: &str) {
        *self.current.lock().unwrap_or_else(|e| e.into_inner()) = what.to_string();
    }

    pub fn current(&self) -> String {
        self.current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn phase_name(&self) -> &'static str {
        match self.phase.load(Ordering::Relaxed) {
            PHASE_READING => "reading",
            PHASE_NAMING => "naming",
            PHASE_SAVING => "saving",
            _ => "finding",
        }
    }

    /// Back to the start, for a new scan.
    pub fn reset(&self) {
        for c in [
            &self.found,
            &self.done,
            &self.roots_total,
            &self.roots_done,
            &self.games_total,
            &self.games_done,
        ] {
            c.store(0, Ordering::Relaxed);
        }
        self.phase.store(PHASE_FINDING, Ordering::Relaxed);
        self.set_current("");
    }
}

/// Scans `roots` into a new library. `previous` provides the caches (and titles/covers already
/// known); `deep` ignores the caches. `covers_dir` receives `<GAMEID>.png` read out of games.
#[allow(clippy::too_many_arguments)]
pub fn scan(
    roots: &[String],
    previous: Option<&Library>,
    deep: bool,
    covers_dir: Option<&Path>,
    online: bool,
    cancel: &AtomicBool,
    progress: &Progress,
    rt: Option<&tokio::runtime::Handle>,
) -> Result<Library, String> {
    let empty = Library::default();
    let prev = previous.unwrap_or(&empty);
    let mut lib = Library {
        version: INDEX_VERSION,
        roots: roots.to_vec(),
        library_root: roots.first().cloned().unwrap_or_default(),
        ..Library::default()
    };
    let mut games = BTreeMap::new();
    progress
        .roots_total
        .store(roots.len() as u64, Ordering::Relaxed);
    for root in roots {
        progress.phase.store(PHASE_FINDING, Ordering::Relaxed);
        progress.set_current(root);
        let fs = match Fsys::for_root(root, rt) {
            Ok(fs) => fs,
            Err(e) => {
                crate::log_warn!("collection: skipping {root}: {e}");
                let mine = format!("{root}|");
                for (k, v) in prev.folder_cache.range(mine.clone()..) {
                    if !k.starts_with(&mine) {
                        break;
                    }
                    lib.folder_cache.insert(k.clone(), v.clone());
                }
                for (k, v) in prev.pkg_cache.range(mine.clone()..) {
                    if !k.starts_with(&mine) {
                        break;
                    }
                    lib.pkg_cache.insert(k.clone(), v.clone());
                }
                lib.missing_roots.push(root.clone());
                continue;
            }
        };
        let Discovery { items, unreadable } = discover(&fs, root, prev, cancel);
        // A cancel during the walk cut it short: what it found so far is not the library, and
        // saving it would drop every game and cached identity it did not reach.
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let found_rels: HashSet<String> = items.iter().map(|i| i.rel.clone()).collect();
        let unread: Vec<String> = unreadable.iter().map(|(rel, _, _)| rel.clone()).collect();
        lib.unreadable
            .extend(unreadable.into_iter().map(|(_, abs, _)| abs));
        keep_unread(prev, root, &unread, &found_rels, &mut lib, &mut games);
        progress
            .found
            .fetch_add(items.len() as u64, Ordering::Relaxed);
        progress.phase.store(PHASE_READING, Ordering::Relaxed);
        for it in items {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let key = format!("{root}|{}", it.rel);
            let name = it.rel.rsplit('/').next().unwrap_or(&it.rel).to_string();
            progress.set_current(&name);
            let stat = Some(it.stat);
            let size = match it.kind.as_str() {
                "folder" => {
                    let sig = it.sig.clone().unwrap_or_default();
                    let known = prev.folder_cache.get(&key);
                    match known {
                        Some(c) if !deep && c.sig == sig => {
                            lib.folder_cache.insert(key.clone(), c.clone());
                            c.size
                        }
                        _ => {
                            let (size, whole) = deep_size(&fs, &it.abs);
                            if whole {
                                lib.folder_cache
                                    .insert(key.clone(), FolderCacheEntry { sig, size });
                                size
                            } else {
                                // Short by what could not be read: not remembered, read again
                                // next scan, and until then the last whole count stands.
                                crate::log_warn!(
                                    "collection: {name}: part of it could not be read"
                                );
                                known.map_or(size, |c| c.size)
                            }
                        }
                    }
                }
                _ => it.stat.0,
            };
            // A set's later parts are remembered as what they are, so the next scan does not
            // read their first bytes again.
            for p in it.parts.iter().filter(|p| it.kind == "pkg" && p.read) {
                lib.pkg_cache.insert(
                    format!("{root}|{}", p.rel),
                    PkgCacheEntry {
                        size: p.size,
                        mtime: p.mtime,
                        pkg: PkgInfo {
                            error: Some(identify::SPLIT_PART.into()),
                            complete: true,
                            ..PkgInfo::default()
                        },
                        v: super::IDENTIFY_VERSION,
                    },
                );
            }
            let (added, modified, date_source) = dates(stat);
            // Identity, reused while the item's size and mtime hold.
            let pkg = match prev.pkg_cache.get(&key) {
                Some(c)
                    if !deep
                        && c.size == size
                        && c.mtime == modified
                        && c.v == super::IDENTIFY_VERSION =>
                {
                    Some(c.pkg.clone())
                }
                _ if it.kind == "pkg" && !it.parts.is_empty() => {
                    Some(identify::identify_set(Path::new(&it.abs), size, it.gap))
                }
                _ => identify::identify(Path::new(&it.abs), &it.kind),
            };
            // A file that could not be read at all (a dropped connection) is read again next
            // time; everything else it said holds while it is unchanged.
            if let Some(p) = pkg
                .as_ref()
                .filter(|p| p.error.as_deref() != Some(identify::READ_FAILED))
            {
                lib.pkg_cache.insert(
                    key.clone(),
                    PkgCacheEntry {
                        size,
                        mtime: modified,
                        pkg: p.clone(),
                        v: super::IDENTIFY_VERSION,
                    },
                );
            }
            let gid = pkg
                .as_ref()
                .map(|p| p.title_id.to_ascii_uppercase())
                .filter(|t| super::is_game_id(t))
                .or_else(|| game_id_in(&name))
                .or_else(|| game_id_in(&it.rel));
            progress.done.fetch_add(1, Ordering::Relaxed);
            let Some(gid) = gid else { continue };
            let loc = Location {
                root: root.clone(),
                container: it
                    .rel
                    .split_once('/')
                    .map(|(c, _)| c.to_string())
                    .unwrap_or_default(),
                name,
                kind: it.kind.clone(),
                path: it.rel.clone(),
                absolute_path: it.abs.clone(),
                size_bytes: size,
                added_at: (added > 0).then(|| iso_utc(added)),
                modified_at: (modified > 0).then(|| iso_utc(modified)),
                date_source: Some(date_source.into()),
                added_ts: added,
                pkg,
                parts: it.parts.into_iter().map(|p| p.abs).collect(),
            };
            group::place(&mut games, &gid, loc);
        }
        progress.roots_done.fetch_add(1, Ordering::Relaxed);
    }
    progress.phase.store(PHASE_NAMING, Ordering::Relaxed);
    progress
        .games_total
        .store(games.len() as u64, Ordering::Relaxed);
    let fetch = super::online::fetch;
    let mut lookups = online.then(|| {
        super::online::Lookups::new(
            prev.online.clone(),
            secs(std::time::SystemTime::now()),
            &fetch,
        )
    });
    for g in games.values_mut() {
        progress.set_current(&g.title);
        if let Some(old) = prev.games.get(&g.game_id) {
            g.cover_url = old.cover_url.clone();
            // A title looked up before is reused rather than fetched again, and a game that
            // lost its own title keeps the one it had.
            let looked_up = g.title_rank < 2 && old.title_source == "online";
            if looked_up || (g.title == g.game_id && old.title != old.game_id) {
                g.title = old.title.clone();
                g.title_source = old.title_source.clone();
            }
        }
        let cancelled = cancel.load(Ordering::Relaxed);
        // Only a game that cannot name itself (DLC only, or an archive's ID) is looked up.
        if let Some(lookups) = lookups
            .as_mut()
            .filter(|_| g.title_rank < 2 && g.title_source != "online" && !cancelled)
        {
            if let Some((title, cover)) = lookups.ask(&g.game_id) {
                if let Some(t) = title {
                    g.title = t;
                    g.title_source = "online".into();
                }
                if cover.is_some() {
                    g.cover_url = cover;
                }
            }
        }
        group::finish(g);
        if let Some(dir) = covers_dir {
            // After a cancel, the covers already saved: what is left is only the slow reads.
            g.local_cover = if cancelled {
                saved_cover(g, dir)
            } else {
                cover_for(g, dir, deep)
            };
            g.cover_stamp = g
                .local_cover
                .as_ref()
                .and_then(|n| super::store::cover_stamp(&dir.join(n)));
        }
        progress.games_done.fetch_add(1, Ordering::Relaxed);
    }
    progress.phase.store(PHASE_SAVING, Ordering::Relaxed);
    progress.set_current("");
    lib.summary = group::summarize(&games);
    lib.online = match lookups {
        Some(l) => l.finish(|id| games.contains_key(id)),
        None => prev.online.clone(),
    };
    lib.games = games;
    lib.generated_at = iso_utc(secs(std::time::SystemTime::now()));
    Ok(lib)
}

/// The cover already saved for a game, if any.
fn saved_cover(g: &super::Game, dir: &Path) -> Option<String> {
    let name = format!("{}.png", g.game_id);
    (super::is_bare_file_name(&name)
        && std::fs::metadata(dir.join(&name)).is_ok_and(|m| m.len() > 0))
    .then_some(name)
}

/// Saves the game's own `icon0.png` as `<GAMEID>.png`; an already-saved cover is reused unless
/// `force`.
fn cover_for(g: &super::Game, dir: &Path, force: bool) -> Option<String> {
    if !force {
        if let Some(saved) = saved_cover(g, dir) {
            return Some(saved);
        }
    }
    let name = format!("{}.png", g.game_id);
    if !super::is_bare_file_name(&name) {
        return None;
    }
    let out = dir.join(&name);
    let mut order: Vec<&Location> = g.locations.iter().collect();
    order.sort_by_key(|l| group::cover_rank(l));
    for l in order {
        if group::cover_rank(l).0 == 9 {
            continue;
        }
        if let Some(bytes) = identify::cover(Path::new(&l.absolute_path), &l.kind) {
            let _ = std::fs::create_dir_all(dir);
            let tmp = dir.join(format!("{name}.tmp"));
            if std::fs::write(&tmp, &bytes).is_ok() && std::fs::rename(&tmp, &out).is_ok() {
                return Some(name);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ps5upload-coll-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn app(dir: &Path, id: &str, title: &str, ver: &str) {
        std::fs::create_dir_all(dir.join("sce_sys")).unwrap();
        std::fs::write(
            dir.join("sce_sys/param.json"),
            format!(
                r#"{{"titleId":"{id}","contentId":"UP0000-{id}_00-TEST000000000000","contentVersion":"{ver}","applicationCategoryType":0,"localizedParameters":{{"defaultLanguage":"en-US","en-US":{{"titleName":"{title}"}}}}}}"#
            ),
        )
        .unwrap();
        std::fs::write(dir.join("eboot.bin"), vec![1u8; 1000]).unwrap();
    }

    #[test]
    fn items_are_found_by_what_they_are_wherever_they_sit() {
        let root = tmp("disc");
        app(
            &root.join("anything/deep/Game-app"),
            "PPSA01234",
            "Game",
            "01.000.000",
        );
        std::fs::create_dir_all(root.join("x")).unwrap();
        std::fs::write(root.join("x/PPSA01234.exfat"), b"img").unwrap();
        std::fs::create_dir_all(root.join("sets/Big")).unwrap();
        std::fs::write(root.join("sets/Big/CUSA00001.part1.rar"), b"r1").unwrap();
        std::fs::write(root.join("sets/Big/CUSA00001.part2.rar"), b"r22").unwrap();
        std::fs::write(root.join(".hidden.pkg"), b"x").unwrap();
        std::fs::write(root.join("notes.txt"), b"x").unwrap();
        let found = discover(
            &Fsys::Local,
            &s(&root),
            &Library::default(),
            &AtomicBool::new(false),
        )
        .items;
        let kinds: Vec<(String, String)> = found
            .iter()
            .map(|f| (f.rel.clone(), f.kind.clone()))
            .collect();
        assert!(
            kinds.contains(&("anything/deep/Game-app".into(), "folder".into())),
            "{kinds:?}"
        );
        assert!(kinds.contains(&("x/PPSA01234.exfat".into(), "mount.exfat".into())));
        assert!(
            kinds.contains(&("sets/Big/CUSA00001.part1.rar".into(), "rar".into())),
            "the volumes of a .rar set are one item, from the first: {kinds:?}"
        );
        assert_eq!(
            found.len(),
            3,
            "dot-files and other files are not items: {kinds:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Cancel while the first folders are being listed: nothing was found at root level (the
    /// organizer's own layout), so nothing in the reading loop ever saw the cancel and an empty
    /// library replaced the whole index.
    #[test]
    fn a_scan_cancelled_while_walking_is_a_cancelled_scan_not_an_empty_library() {
        let root = tmp("cancel-walk");
        app(
            &root.join("PS5/PPSA01234 - Game/PPSA01234-app"),
            "PPSA01234",
            "Game",
            "01.000.000",
        );
        let cancel = AtomicBool::new(true);
        let r = scan(
            &[s(&root)],
            None,
            false,
            None,
            false,
            &cancel,
            &Progress::default(),
            None,
        );
        assert_eq!(r, Err("cancelled".to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn scan_of(root: &Path, prev: Option<&Library>) -> Library {
        scan(
            &[s(root)],
            prev,
            false,
            None,
            false,
            &AtomicBool::new(false),
            &Progress::default(),
            None,
        )
        .unwrap()
    }

    use crate::inspect::test_fixtures::write_cnt_pkg;

    /// The package header's id in the fixture.
    const FIXTURE_ID: &str = "CUSA00001";

    #[test]
    fn a_split_package_is_one_copy_never_a_duplicate_of_itself() {
        let root = tmp("split");
        let dir = root.join("PS4/CUSA00001 - Game");
        std::fs::create_dir_all(&dir).unwrap();
        let lead = write_cnt_pkg(dir.join("Game_0.pkg"), "gd");
        std::fs::write(dir.join("Game_1.pkg"), vec![7u8; 1000]).unwrap();
        std::fs::write(dir.join("Game_2.pkg"), vec![9u8; 500]).unwrap();
        let lead_size = std::fs::metadata(&lead).unwrap().len();

        let lib = scan_of(&root, None);
        let g = &lib.games[FIXTURE_ID];
        assert_eq!(g.locations.len(), 1, "{:?}", g.locations);
        assert_eq!((g.copies, g.is_duplicate), (1, false));
        assert_eq!(lib.summary.reclaimable_bytes, 0);
        let l = &g.locations[0];
        assert_eq!(l.name, "Game_0.pkg");
        assert_eq!(l.size_bytes, lead_size + 1500);
        assert_eq!(
            l.parts,
            vec![s(&dir.join("Game_1.pkg")), s(&dir.join("Game_2.pkg"))]
        );
        assert!(l.pkg.as_ref().unwrap().complete);

        // Nothing changed: the same answer, from what the index remembers. The parts are not
        // read again: with their bytes changed but their size and time kept, they stay parts.
        let mut bytes = std::fs::read(dir.join("Game_1.pkg")).unwrap();
        let stamp = std::fs::metadata(dir.join("Game_1.pkg"))
            .unwrap()
            .modified();
        bytes[..4].copy_from_slice(&ps5upload_pkg::PKG_MAGIC.to_be_bytes());
        std::fs::write(dir.join("Game_1.pkg"), &bytes).unwrap();
        std::fs::File::options()
            .write(true)
            .open(dir.join("Game_1.pkg"))
            .unwrap()
            .set_modified(stamp.unwrap())
            .unwrap();
        let again = scan_of(&root, Some(&lib));
        assert_eq!(again.games, lib.games);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn packages_that_only_look_like_parts_stay_their_own_copies() {
        let root = tmp("split-not");
        let dir = root.join("fpkg");
        std::fs::create_dir_all(&dir).unwrap();
        // A part 0 and a later file with a header of its own: two packages.
        write_cnt_pkg(dir.join("Game_0.pkg"), "gd");
        write_cnt_pkg(dir.join("Game_1.pkg"), "gd");
        // No part 0 at all: nothing joins these either.
        write_cnt_pkg(dir.join("Other_1.pkg"), "gd");
        write_cnt_pkg(dir.join("Other_2.pkg"), "gd");
        let lib = scan_of(&root, None);
        let g = &lib.games[FIXTURE_ID];
        assert_eq!(g.locations.len(), 4, "{:?}", g.locations);
        assert!(g.locations.iter().all(|l| l.parts.is_empty()));
        assert!(g.is_duplicate, "the same release four times");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_split_package_with_a_part_missing_is_incomplete() {
        let root = tmp("split-gap");
        let dir = root.join("x");
        std::fs::create_dir_all(&dir).unwrap();
        write_cnt_pkg(dir.join("Game_0.pkg"), "gd");
        std::fs::write(dir.join("Game_2.pkg"), vec![7u8; 100]).unwrap();
        let lib = scan_of(&root, None);
        let l = &lib.games[FIXTURE_ID].locations[0];
        assert_eq!(l.parts.len(), 1);
        assert!(!l.pkg.as_ref().unwrap().complete);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn numbered_suffix_parts_count_with_their_package() {
        let root = tmp("split-suffix");
        let dir = root.join("x");
        std::fs::create_dir_all(&dir).unwrap();
        let lead = write_cnt_pkg(dir.join("Game.pkg"), "gd");
        std::fs::write(dir.join("Game.pkg.0"), vec![7u8; 100]).unwrap();
        std::fs::write(dir.join("Game.pkg.1"), vec![7u8; 50]).unwrap();
        let lib = scan_of(&root, None);
        let g = &lib.games[FIXTURE_ID];
        assert_eq!(g.locations.len(), 1);
        let l = &g.locations[0];
        assert_eq!(l.size_bytes, std::fs::metadata(&lead).unwrap().len() + 150);
        assert_eq!(l.parts.len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rar_never_hides_what_lies_beside_it() {
        let root = tmp("rar-mixed");
        let mixed = root.join("Downloads");
        std::fs::create_dir_all(mixed.join("deeper")).unwrap();
        write_cnt_pkg(mixed.join("game.pkg"), "gd");
        std::fs::write(mixed.join("deeper/PPSA00002.exfat"), b"img").unwrap();
        app(
            &mixed.join("PPSA00003-app"),
            "PPSA00003",
            "App",
            "01.000.000",
        );
        std::fs::write(mixed.join("PPSA00004.part1.rar"), b"r1").unwrap();
        std::fs::write(mixed.join("PPSA00004.part2.rar"), b"r22").unwrap();
        std::fs::write(mixed.join("PPSA00005.rar"), b"old").unwrap();
        std::fs::write(mixed.join("PPSA00005.r00"), b"vol0").unwrap();
        std::fs::write(mixed.join("readme.txt"), b"text").unwrap();
        let lib = scan_of(&root, None);
        for id in [
            FIXTURE_ID,
            "PPSA00002",
            "PPSA00003",
            "PPSA00004",
            "PPSA00005",
        ] {
            assert!(lib.games.contains_key(id), "{id}: {:?}", lib.games.keys());
        }
        let rar = &lib.games["PPSA00004"].locations[0];
        assert_eq!(rar.kind, "rar");
        assert_eq!(rar.path, "Downloads/PPSA00004.part1.rar");
        assert_eq!(rar.absolute_path, s(&mixed.join("PPSA00004.part1.rar")));
        assert_eq!(rar.size_bytes, 5, "the volumes, and nothing else");
        assert_eq!(rar.parts, vec![s(&mixed.join("PPSA00004.part2.rar"))]);
        let old = &lib.games["PPSA00005"].locations[0];
        assert_eq!(old.size_bytes, 7);
        assert_eq!(old.parts, vec![s(&mixed.join("PPSA00005.r00"))]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A folder this user may not read, for as long as the guard lives. `None` when the test
    /// runs as a user who reads everything anyway (root in a container).
    #[cfg(unix)]
    struct Unreadable(PathBuf);

    #[cfg(unix)]
    impl Unreadable {
        fn new(dir: &Path) -> Option<Self> {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();
            let guard = Self(dir.to_path_buf());
            std::fs::read_dir(dir).is_err().then_some(guard)
        }
    }

    #[cfg(unix)]
    impl Drop for Unreadable {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_read_keeps_what_it_had() {
        let root = tmp("unread");
        let shelf = root.join("shelf");
        std::fs::create_dir_all(&shelf).unwrap();
        write_cnt_pkg(shelf.join("game.pkg"), "gd");
        std::fs::write(shelf.join("PPSA00002.exfat"), b"img").unwrap();
        app(
            &root.join("other/PPSA00003-app"),
            "PPSA00003",
            "App",
            "01.000.000",
        );
        let first = scan_of(&root, None);
        assert_eq!(first.summary.total_games, 3);
        assert!(first.unreadable.is_empty());
        let key = format!("{}|shelf/game.pkg", s(&root));
        assert!(first.pkg_cache.contains_key(&key));

        let second = {
            let Some(_locked) = Unreadable::new(&shelf) else {
                let _ = std::fs::remove_dir_all(&root);
                return;
            };
            scan_of(&root, Some(&first))
        };
        assert_eq!(second.summary.total_games, 3, "{:?}", second.games.keys());
        assert_eq!(second.unreadable, vec![s(&shelf)]);
        assert_eq!(second.pkg_cache.get(&key), first.pkg_cache.get(&key));
        assert_eq!(
            second.games[FIXTURE_ID].locations,
            first.games[FIXTURE_ID].locations
        );

        // Readable again: nothing is left to say, and what it holds now is what counts.
        std::fs::remove_file(shelf.join("PPSA00002.exfat")).unwrap();
        let third = scan_of(&root, Some(&second));
        assert!(third.unreadable.is_empty());
        assert_eq!(third.summary.total_games, 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_that_is_really_gone_takes_its_games_with_it() {
        let root = tmp("really-gone");
        std::fs::create_dir_all(root.join("shelf")).unwrap();
        std::fs::write(root.join("shelf/PPSA00002.exfat"), b"img").unwrap();
        let first = scan_of(&root, None);
        assert_eq!(first.summary.total_games, 1);
        std::fs::remove_dir_all(root.join("shelf")).unwrap();
        let second = scan_of(&root, Some(&first));
        assert_eq!(second.summary.total_games, 0);
        assert!(second.unreadable.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_saved_cover_is_one_that_has_something_in_it() {
        let dir = tmp("saved-cover");
        let g = crate::collection::Game {
            game_id: "PPSA05555".into(),
            ..Default::default()
        };
        assert_eq!(saved_cover(&g, &dir), None);
        std::fs::write(dir.join("PPSA05555.png"), b"").unwrap();
        assert_eq!(saved_cover(&g, &dir), None);
        std::fs::write(dir.join("PPSA05555.png"), b"x").unwrap();
        assert_eq!(saved_cover(&g, &dir).as_deref(), Some("PPSA05555.png"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every folder is listed once: each listing is a round trip on a server, every minute.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_walk_lists_each_folder_once() {
        use crate::remote::pool::testing::remote_with_shared;
        use crate::remote::store::{conn, Protocol, Secret};
        use crate::remote::MemFs;
        let mem = std::sync::Arc::new(MemFs::new(&[
            ("/games/a/x.pkg", b"pkg"),
            ("/games/a/b/y.pkg", b"pkg"),
            ("/games/c/PPSA01234-app/sce_sys/param.json", b"{}"),
            ("/games/c/PPSA01234-app/eboot.bin", b"e"),
        ]));
        let r = remote_with_shared(std::sync::Arc::clone(&mem), None);
        let id = r
            .store
            .add(conn("NAS", Protocol::Smb), Secret::None)
            .unwrap()
            .conn
            .id;
        let fs = std::sync::Arc::new(
            crate::remote::source_fs::RemoteSourceFs::new(
                std::sync::Arc::clone(&r.pool),
                std::sync::Arc::clone(&r.store),
                &id,
            )
            .await
            .unwrap(),
        );
        let root = format!("remote://{id}/games");
        mem.reset_list_count();
        let found = tokio::task::spawn_blocking(move || {
            discover(
                &Fsys::Remote { fs },
                &root,
                &Library::default(),
                &AtomicBool::new(false),
            )
            .items
        })
        .await
        .unwrap();
        assert_eq!(found.len(), 3);
        // games, a, a/b, c, the game folder, its sce_sys: six folders, six listings.
        assert_eq!(mem.list_count(), 6);
    }

    #[test]
    fn under_goes_by_whole_names() {
        assert!(under("a/b.pkg", "a"));
        assert!(under("a", "a"));
        assert!(under("anything", ""));
        assert!(!under("ab/c.pkg", "a"));
        assert!(!under("a", "a/b"));
    }

    /// The same on a saved server: a folder that will not list (here, one SMB would hand back
    /// with made-up names) keeps what it had, and the rest of the root is read as usual.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_server_folder_that_will_not_list_keeps_what_it_had() {
        use crate::remote::pool::testing::remote_with_shared;
        use crate::remote::store::{conn, Protocol, Secret};
        use crate::remote::MemFs;
        let mem = std::sync::Arc::new(MemFs::new(&[
            ("/games/shelf/PPSA00002.exfat", b"img"),
            ("/games/other/PPSA00003.exfat", b"img"),
        ]));
        let r = remote_with_shared(std::sync::Arc::clone(&mem), None);
        let id = r
            .store
            .add(conn("NAS", Protocol::Smb), Secret::None)
            .unwrap()
            .conn
            .id;
        let fs = std::sync::Arc::new(
            crate::remote::source_fs::RemoteSourceFs::new(
                std::sync::Arc::clone(&r.pool),
                std::sync::Arc::clone(&r.store),
                &id,
            )
            .await
            .unwrap(),
        );
        let root = format!("remote://{id}/games");
        let walk = move |prev: Library| {
            let (fs, root) = (std::sync::Arc::clone(&fs), root.clone());
            tokio::task::spawn_blocking(move || {
                discover(&Fsys::Remote { fs }, &root, &prev, &AtomicBool::new(false))
            })
        };
        let first = walk(Library::default()).await.unwrap();
        assert_eq!(first.items.len(), 2);
        assert!(first.unreadable.is_empty());
        mem.add(&[("/games/shelf/C69YSY~1.TXT", b"mangled")]);
        let second = walk(Library::default()).await.unwrap();
        let rels: Vec<&str> = second.items.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, ["other/PPSA00003.exfat"]);
        assert_eq!(second.unreadable.len(), 1);
        assert_eq!(second.unreadable[0].0, "shelf");
    }

    #[test]
    fn a_root_that_is_gone_is_skipped_and_its_caches_kept() {
        // #461: a deleted folder failed every scan, so the others were never read either.
        let root = tmp("gone-live");
        app(
            &root.join("PPSA01234-app"),
            "PPSA01234",
            "Game",
            "01.000.000",
        );
        let gone = s(&std::env::temp_dir().join("ps5upload-coll-never-there-461"));
        let mut prev = Library::default();
        let kept = PkgCacheEntry::default();
        prev.pkg_cache.insert(format!("{gone}|a.pkg"), kept.clone());
        let (cancel, p) = (AtomicBool::new(false), Progress::default());
        let lib = scan(
            &[gone.clone(), s(&root)],
            Some(&prev),
            false,
            None,
            false,
            &cancel,
            &p,
            None,
        )
        .unwrap();
        assert!(
            lib.games.contains_key("PPSA01234"),
            "the other root is read"
        );
        assert_eq!(lib.missing_roots, vec![gone.clone()]);
        assert_eq!(lib.pkg_cache.get(&format!("{gone}|a.pkg")), Some(&kept));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_scan_groups_copies_reads_identity_and_reuses_its_caches() {
        let root = tmp("scan");
        app(
            &root.join("app/PPSA01234-app"),
            "PPSA01234",
            "Test Game",
            "01.020.000",
        );
        std::fs::create_dir_all(root.join("zip")).unwrap();
        std::fs::write(root.join("zip/Test Game PPSA01234.zip"), vec![0u8; 50]).unwrap();
        let cancel = AtomicBool::new(false);
        let p = Progress::default();
        let lib = scan(&[s(&root)], None, false, None, false, &cancel, &p, None).unwrap();
        assert_eq!(lib.summary.total_games, 1);
        // What the screen shows while it runs, as it stands at the end.
        assert_eq!(p.roots_total.load(Ordering::Relaxed), 1);
        assert_eq!(p.roots_done.load(Ordering::Relaxed), 1);
        assert_eq!(p.found.load(Ordering::Relaxed), 2);
        assert_eq!(p.done.load(Ordering::Relaxed), 2);
        assert_eq!(p.games_total.load(Ordering::Relaxed), 1);
        assert_eq!(p.games_done.load(Ordering::Relaxed), 1);
        assert_eq!(p.phase_name(), "saving");
        let g = &lib.games["PPSA01234"];
        assert_eq!(g.title, "Test Game");
        assert_eq!(g.platform, "PS5");
        assert_eq!((g.copies, g.is_duplicate), (2, true));
        assert_eq!(lib.summary.reclaimable_bytes, 50);
        let folder = g.locations.iter().find(|l| l.kind == "folder").unwrap();
        let pkg = folder.pkg.as_ref().unwrap();
        assert_eq!(pkg.version, "1.20.0");
        assert_eq!((pkg.kind.as_str(), pkg.kind_confident), ("base", true));
        assert_eq!(folder.container, "app");
        assert!(folder.size_bytes >= 1000);
        assert_eq!(p.done.load(Ordering::Relaxed), 2);
        // A second scan reuses the folder size and identity while nothing changed.
        let again = scan(
            &[s(&root)],
            Some(&lib),
            false,
            None,
            false,
            &cancel,
            &Progress::default(),
            None,
        )
        .unwrap();
        assert_eq!(again.games, lib.games);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_title_id_that_is_not_a_game_id_never_names_a_file() {
        let root = tmp("evilid");
        let covers = root.join("covers");
        let game = root.join("lib/PPSA06666-app");
        app(&game, "../../x", "Evil", "01.000.000");
        std::fs::write(game.join("sce_sys/icon0.png"), b"\x89PNG fake").unwrap();
        let lib = scan(
            &[s(&root.join("lib"))],
            None,
            false,
            Some(&covers),
            false,
            &AtomicBool::new(false),
            &Progress::default(),
            None,
        )
        .unwrap();
        // The bad id is ignored and the name's id is used instead.
        assert_eq!(lib.games.keys().collect::<Vec<_>>(), vec!["PPSA06666"]);
        assert!(!root.join("x.png").exists());
        assert!(!covers.join("../../x.png").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn covers_are_read_out_of_the_game() {
        let root = tmp("cover");
        let covers = root.join("covers");
        let game = root.join("lib/PPSA05555-app");
        app(&game, "PPSA05555", "Covered", "01.000.000");
        std::fs::write(game.join("sce_sys/icon0.png"), b"\x89PNG fake").unwrap();
        let lib = scan(
            &[s(&root.join("lib"))],
            None,
            false,
            Some(&covers),
            false,
            &AtomicBool::new(false),
            &Progress::default(),
            None,
        )
        .unwrap();
        assert_eq!(
            lib.games["PPSA05555"].local_cover.as_deref(),
            Some("PPSA05555.png")
        );
        assert_eq!(
            std::fs::read(covers.join("PPSA05555.png")).unwrap(),
            b"\x89PNG fake"
        );
        // A deep rescan reads the cover out again: its stamp changes, so the UI's URL does.
        let first = lib.games["PPSA05555"].cover_stamp.clone();
        assert!(first.is_some());
        std::fs::write(game.join("sce_sys/icon0.png"), b"\x89PNG a new cover").unwrap();
        let deep = scan(
            &[s(&root.join("lib"))],
            Some(&lib),
            true,
            Some(&covers),
            false,
            &AtomicBool::new(false),
            &Progress::default(),
            None,
        )
        .unwrap();
        assert_ne!(deep.games["PPSA05555"].cover_stamp, first);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A root on a saved server is walked by its listings: items found by what they are,
    /// sizes and times from the listing, paths kept as `remote://` paths.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_server_root_is_discovered_through_its_listings() {
        use crate::remote::pool::testing::remote_with_shared;
        use crate::remote::store::{conn, Protocol, Secret};
        use crate::remote::MemFs;
        let mem = MemFs::new(&[
            ("/games/fpkg/Game.pkg", b"pkg bytes"),
            ("/games/app/PPSA01234-app/sce_sys/param.json", b"{}"),
            ("/games/app/PPSA01234-app/eboot.bin", &[1u8; 100]),
            ("/games/sets/Big/x.part1.rar", b"r1"),
            ("/games/sets/Big/x.part2.rar", b"r22"),
            ("/games/.hidden/a.pkg", b"x"),
            ("/games/notes.txt", b"x"),
        ]);
        let r = remote_with_shared(std::sync::Arc::new(mem), None);
        let id = r
            .store
            .add(conn("NAS", Protocol::Smb), Secret::None)
            .unwrap()
            .conn
            .id;
        let fs = crate::remote::source_fs::RemoteSourceFs::new(
            std::sync::Arc::clone(&r.pool),
            std::sync::Arc::clone(&r.store),
            &id,
        )
        .await
        .unwrap();
        let root = format!("remote://{id}/games");
        let fsys = Fsys::Remote {
            fs: std::sync::Arc::new(fs),
        };
        let (found, sizes) = tokio::task::spawn_blocking(move || {
            let found = discover(&fsys, &root, &Library::default(), &AtomicBool::new(false)).items;
            let folder = found
                .iter()
                .find(|f| f.kind == "folder")
                .unwrap()
                .abs
                .clone();
            let rar = found.iter().find(|f| f.kind == "rar").unwrap().stat.0;
            let sizes = (deep_size(&fsys, &folder).0, rar);
            (found, sizes)
        })
        .await
        .unwrap();
        let kinds: Vec<(&str, &str)> = found
            .iter()
            .map(|f| (f.rel.as_str(), f.kind.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("app/PPSA01234-app", "folder"),
                ("fpkg/Game.pkg", "pkg"),
                ("sets/Big/x.part1.rar", "rar")
            ]
        );
        assert!(found
            .iter()
            .all(|f| f.abs.starts_with(&format!("remote://{id}/games/"))));
        assert_eq!(sizes, (102, 5));
    }

    /// Server names that look like escapes or separators are found, and read back, as
    /// themselves: "a%20b.pkg" is not "a b.pkg", and a backslash is part of the name.
    #[tokio::test(flavor = "multi_thread")]
    async fn odd_server_names_resolve_to_themselves() {
        use crate::remote::pool::testing::remote_with_shared;
        use crate::remote::store::{conn, Protocol, Secret};
        use crate::remote::MemFs;
        use ps5upload_core::source_fs::SourceFs;
        let files: [(&str, &[u8]); 4] = [
            ("/games/a%20b.pkg", b"percent"),
            ("/games/a b.pkg", b"space"),
            ("/games/100% Orange.pkg", b"orange"),
            ("/games/a\\b.pkg", b"backslash"),
        ];
        let r = remote_with_shared(std::sync::Arc::new(MemFs::new(&files)), None);
        let id = r
            .store
            .add(conn("NAS", Protocol::Smb), Secret::None)
            .unwrap()
            .conn
            .id;
        let fs = std::sync::Arc::new(
            crate::remote::source_fs::RemoteSourceFs::new(
                std::sync::Arc::clone(&r.pool),
                std::sync::Arc::clone(&r.store),
                &id,
            )
            .await
            .unwrap(),
        );
        let root = format!("remote://{id}/games");
        let fsys = Fsys::Remote {
            fs: std::sync::Arc::clone(&fs),
        };
        let found = tokio::task::spawn_blocking(move || {
            let found = discover(&fsys, &root, &Library::default(), &AtomicBool::new(false)).items;
            found
                .into_iter()
                .map(|f| {
                    let server = crate::remote::path::parse(&f.abs).unwrap().path;
                    let mut bytes = Vec::new();
                    std::io::Read::read_to_end(
                        &mut fs.open(Path::new(&server)).unwrap(),
                        &mut bytes,
                    )
                    .unwrap();
                    (f.rel, server, bytes)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap();
        assert_eq!(found.len(), 4);
        for (name, bytes) in files {
            let rel = name.trim_start_matches("/games/");
            let hit = found.iter().find(|(r, _, _)| r == rel).unwrap_or_else(|| {
                panic!("{rel} not found as itself: {found:?}");
            });
            assert_eq!(hit.1, name, "{rel} read back as another path");
            assert_eq!(hit.2, bytes, "{rel} read another file");
        }
    }

    #[test]
    fn a_new_scan_starts_its_progress_from_nothing() {
        let p = Progress::default();
        p.done.store(5, Ordering::Relaxed);
        p.phase.store(PHASE_SAVING, Ordering::Relaxed);
        p.set_current("Some Game");
        p.reset();
        assert_eq!(p.phase_name(), "finding");
        assert_eq!(p.done.load(Ordering::Relaxed), 0);
        assert_eq!(p.current(), "");
    }
}
