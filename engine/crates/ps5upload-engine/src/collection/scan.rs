//! Walking the roots: discovery, sizes (cached by a folder's depth-1 signature), identity
//! (cached by size and mtime), then grouping and covers.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
        /// `remote://<connection-id>`.
        base: String,
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
        let p = crate::remote::path::parse(root).map_err(|e| e.to_string())?;
        let (fs, _) = rt
            .block_on(crate::remote::source_fs::RemoteSourceFs::for_path(root))
            .map_err(|e| format!("{root}: {e}"))?;
        let f = Fsys::Remote {
            fs,
            base: format!("remote://{}", p.connection_id),
        };
        if f.try_list(root).is_none() {
            return Err(format!("{root} could not be listed on the server"));
        }
        Ok(f)
    }

    fn try_list(&self, dir: &str) -> Option<Vec<Ent>> {
        use ps5upload_core::source_fs::SourceFs;
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
            Fsys::Remote { fs, base } => {
                let server = crate::remote::path::parse(dir).ok()?.path;
                let children = fs.read_dir(Path::new(&server)).ok()?;
                Some(
                    children
                        .into_iter()
                        .map(|(child, is_dir)| {
                            let size = fs.metadata(&child).map(|m| m.len).unwrap_or(0);
                            let mtime = fs.mtime(&child).unwrap_or(0) as i64;
                            let sp = child.to_string_lossy().replace('\\', "/");
                            Ent {
                                name: sp.rsplit('/').next().unwrap_or(&sp).to_string(),
                                path: format!("{base}{sp}"),
                                is_dir,
                                is_symlink: false,
                                size,
                                mtime,
                                created: 0,
                            }
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
    fn rel_of(root: &str, full: &str) -> String {
        full.strip_prefix(root)
            .unwrap_or(full)
            .trim_start_matches(['/', '\\'])
            .replace('\\', "/")
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
            let rel = rel_of(root, &e.path);
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
    pub found: AtomicU64,
    pub done: AtomicU64,
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
    for root in roots {
        let fs = Fsys::for_root(root, rt)?;
        let items = discover(&fs, root, cancel);
        progress
            .found
            .fetch_add(items.len() as u64, Ordering::Relaxed);
        for it in items {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let key = format!("{root}|{}", it.rel);
            let name = it.rel.rsplit('/').next().unwrap_or(&it.rel).to_string();
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
                .filter(|t| !t.is_empty())
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
    }
    for g in games.values_mut() {
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
        }
    }
    lib.summary = group::summarize(&games);
    lib.games = games;
    lib.generated_at = iso_utc(secs(std::time::SystemTime::now()));
    Ok(lib)
}

/// Saves the game's own `icon0.png` as `<GAMEID>.png`; an already-saved cover is reused unless
/// `force`.
fn cover_for(g: &super::Game, dir: &Path, force: bool) -> Option<String> {
    let name = format!("{}.png", g.game_id);
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
            base: format!("remote://{id}"),
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
}
