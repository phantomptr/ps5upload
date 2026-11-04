//! Walking the roots: discovery, sizes (cached by a folder's depth-1 signature), identity
//! (cached by size and mtime), then grouping and covers.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use super::{
    game_id_in, group, identify, iso_utc, FolderCacheEntry, Library, Location, PkgCacheEntry,
    INDEX_VERSION, ITEM_EXTS,
};

/// Deep enough for any sane arrangement, shallow enough that an extracted game tree cannot
/// take the scan on a long walk.
pub const MAX_DEPTH: usize = 6;

/// One thing discovery found: (path relative to its root, absolute path, type). For a root on a
/// saved server the absolute path is a `remote://` path, which every reader here accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub rel: String,
    pub abs: String,
    pub kind: String,
    /// (size, mtime, created) from the listing that found it, so no second look is needed.
    pub stat: (u64, i64, i64),
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
        if f.try_list(root).is_none() {
            return Err(format!("{root} could not be listed on the server"));
        }
        Ok(f)
    }

    fn try_list(&self, dir: &str) -> Option<Vec<Ent>> {
        match self {
            Fsys::Local => {
                let rd = std::fs::read_dir(dir).ok()?;
                Some(
                    rd.flatten()
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
                        .collect(),
                )
            }
            Fsys::Remote { fs, .. } => {
                let server = crate::remote::path::parse(dir).ok()?.path;
                let children = fs.entries(&server).ok()?;
                // Each name encoded into the `remote://` path, so that reading it back gives
                // this name ("a%20b.pkg" stays itself, a backslash stays a name character).
                let at = dir.trim_end_matches('/');
                Some(
                    children
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
                        .collect(),
                )
            }
        }
    }

    /// The visible entries of `dir`, sorted by name; empty when it cannot be read.
    pub fn list(&self, dir: &str) -> Vec<Ent> {
        let mut v: Vec<Ent> = self
            .try_list(dir)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| visible(&e.name))
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }
}

/// An extracted game keeps its metadata at `sce_sys/param.json` (PS5) or `param.sfo` (PS4).
fn is_app_folder(fs: &Fsys, entries: &[Ent]) -> bool {
    let Some(sys) = entries.iter().find(|e| e.is_dir && e.name == "sce_sys") else {
        return false;
    };
    fs.list(&sys.path)
        .iter()
        .any(|e| !e.is_dir && (e.name == "param.json" || e.name == "param.sfo"))
}

/// The immediate `.rar` parts of a folder (from its listing), which make it one multipart set.
fn rar_parts(entries: &[Ent]) -> Vec<&Ent> {
    entries
        .iter()
        .filter(|e| !e.is_dir && !e.is_symlink && e.name.to_ascii_lowercase().ends_with(".rar"))
        .collect()
}

/// Every game item under `root`. A directory is an item when it is an extracted game or a
/// multipart RAR set; otherwise it is only a place to look further. Symlinks are not followed.
pub fn discover(fs: &Fsys, root: &str, cancel: &AtomicBool) -> Vec<Found> {
    let mut found = Vec::new();
    fn rel_of(fs: &Fsys, root: &str, full: &str) -> String {
        let rest = full.strip_prefix(root).unwrap_or(full);
        match fs {
            // Server paths are `/`-separated, their names encoded: the relative path is names.
            Fsys::Remote { .. } => crate::remote::path::decode_rel(rest.trim_start_matches('/')),
            Fsys::Local => rest.trim_start_matches(['/', '\\']).replace('\\', "/"),
        }
    }
    fn walk(
        fs: &Fsys,
        root: &str,
        dir: &str,
        depth: usize,
        found: &mut Vec<Found>,
        cancel: &AtomicBool,
    ) {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        for e in fs.list(dir) {
            if e.is_symlink {
                continue;
            }
            let rel = rel_of(fs, root, &e.path);
            if e.is_dir {
                let inner = fs.list(&e.path);
                let stat = (e.size, e.mtime, e.created);
                if is_app_folder(fs, &inner) {
                    found.push(Found {
                        rel,
                        abs: e.path,
                        kind: "folder".into(),
                        stat,
                    });
                } else if !rar_parts(&inner).is_empty() {
                    found.push(Found {
                        rel,
                        abs: e.path,
                        kind: "rar".into(),
                        stat,
                    });
                } else if depth < MAX_DEPTH {
                    walk(fs, root, &e.path, depth + 1, found, cancel);
                }
            } else if let Some(kind) = file_type(&e.name) {
                found.push(Found {
                    rel,
                    stat: (e.size, e.mtime, e.created),
                    abs: e.path,
                    kind: kind.into(),
                });
            }
        }
    }
    walk(fs, root, root, 0, &mut found, cancel);
    found
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

fn deep_size(fs: &Fsys, path: &str) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_string()];
    while let Some(dir) = stack.pop() {
        for e in fs.try_list(&dir).unwrap_or_default() {
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
    total
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
        let items = discover(&fs, root, cancel);
        // A cancel during the walk cut it short: what it found so far is not the library, and
        // saving it would drop every game and cached identity it did not reach.
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
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
                "rar" => rar_parts(&fs.list(&it.abs)).iter().map(|e| e.size).sum(),
                "folder" => {
                    let sig = folder_signature(&fs.list(&it.abs));
                    let size = match prev.folder_cache.get(&key) {
                        Some(c) if !deep && c.sig == sig => c.size,
                        _ => deep_size(&fs, &it.abs),
                    };
                    lib.folder_cache
                        .insert(key.clone(), FolderCacheEntry { sig, size });
                    size
                }
                _ => stat.map(|s| s.0).unwrap_or(0),
            };
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
                _ => identify::identify(Path::new(&it.abs), &it.kind),
            };
            if let Some(p) = &pkg {
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
            };
            group::place(&mut games, &gid, loc);
        }
        progress.roots_done.fetch_add(1, Ordering::Relaxed);
    }
    progress.phase.store(PHASE_NAMING, Ordering::Relaxed);
    progress
        .games_total
        .store(games.len() as u64, Ordering::Relaxed);
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
        // Only a game that cannot name itself (DLC only, or an archive's ID) is looked up.
        if online
            && g.title_rank < 2
            && g.title_source != "online"
            && !cancel.load(Ordering::Relaxed)
        {
            if let Ok((title, cover)) = super::online::fetch(&g.game_id) {
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
            g.local_cover = cover_for(g, dir, deep);
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
    lib.games = games;
    lib.generated_at = iso_utc(secs(std::time::SystemTime::now()));
    Ok(lib)
}

/// Saves the game's own `icon0.png` as `<GAMEID>.png`; an already-saved cover is reused unless
/// `force`.
fn cover_for(g: &super::Game, dir: &Path, force: bool) -> Option<String> {
    let name = format!("{}.png", g.game_id);
    if !super::is_bare_file_name(&name) {
        return None;
    }
    let out = dir.join(&name);
    if !force && std::fs::metadata(&out).is_ok_and(|m| m.len() > 0) {
        return Some(name);
    }
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
        let found = discover(&Fsys::Local, &s(&root), &AtomicBool::new(false));
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
            kinds.contains(&("sets/Big".into(), "rar".into())),
            "a folder of .rar parts is one set"
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
            let found = discover(&fsys, &root, &AtomicBool::new(false));
            let folder = found
                .iter()
                .find(|f| f.kind == "folder")
                .unwrap()
                .abs
                .clone();
            let rar = found.iter().find(|f| f.kind == "rar").unwrap().abs.clone();
            let sizes = (
                deep_size(&fsys, &folder),
                rar_parts(&fsys.list(&rar))
                    .iter()
                    .map(|e| e.size)
                    .sum::<u64>(),
            );
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
                ("sets/Big", "rar")
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
            let found = discover(&fsys, &root, &AtomicBool::new(false));
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
