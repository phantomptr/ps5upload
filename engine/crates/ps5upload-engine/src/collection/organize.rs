//! A root-relative layout for collection packages, mount images and extracted games.
//!
//! Every package is renamed `Title - TitleID - vVersion - Region - KIND.pkg` and filed under
//! `<root>/<Platform>/<TitleID - Title>/`, with images retaining their extension and game
//! folders moved intact. A source never moves between configured collection roots. Planning is pure:
//! the preview and the run come from the same code, and the run re-plans and drops any move
//! that changed since the preview. Nothing is overwritten, nothing is deleted, and every move
//! is a rename on one volume (never a copy). Each run writes its undo log before its first move.
//!
//! The folder can be on this computer or on a saved server: everything here goes through a
//! [`Tree`], and a server's paths are its `remote://` paths. On a server a rename never moves
//! between servers (a move stays inside its container), and a dropped connection stops the run
//! where it is, with the log saying what happened so far.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use super::junk;
use super::organize_fs::{path_string, OpError, PlanTree, Tree};
use super::scan::MAX_DEPTH;
use super::split;
use super::PkgInfo;

pub const DUPLICATE_TAG: &str = " - DUPLICATE";

pub const SKIP_INCOMPLETE: &str = "incomplete";
pub const SKIP_UNREADABLE: &str = "unreadable";
pub const SKIP_NO_ID: &str = "no title id";
pub const SKIP_IN_PLACE: &str = "already correct";

/// What one package (or split set, read from its first part) says, plus where it is.
#[derive(Debug, Clone)]
pub struct Facts {
    pub path: PathBuf,
    /// Every part of a split set in order; just `[path]` otherwise.
    pub parts: Vec<PathBuf>,
    /// The whole set's size.
    pub size: u64,
    pub info: PkgInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Move {
    /// The collection folder this move is inside (absolute).
    #[serde(default)]
    pub container: String,
    /// Relative to `container`, `/`-separated.
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub title_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub confident: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub content_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    /// A part of a split package: its set, named by the set's first file as it is now. A set
    /// moves as one: all of it, or none of it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub set: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub duplicate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Skipped {
    #[serde(default)]
    pub container: String,
    pub path: String,
    pub reason: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Plan {
    pub moves: Vec<Move>,
    pub skipped: Vec<Skipped>,
    /// Packages whose kind was inferred rather than read (`_UNSURE` in their name).
    pub unsure: Vec<Move>,
}

/// What a running plan is doing, and whether to stop: a NAS with hundreds of packages keeps a plan
/// busy for minutes.
#[derive(Default)]
pub struct PlanWatch {
    pub cancel: AtomicBool,
    /// It has walked the folders and is reading packages.
    pub reading: AtomicBool,
    /// Items to look at, and looked at.
    pub total: AtomicU64,
    pub done: AtomicU64,
    current: Mutex<String>,
}

impl PlanWatch {
    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn set_current(&self, what: String) {
        *self.current.lock().unwrap_or_else(|e| e.into_inner()) = what;
    }

    pub fn current(&self) -> String {
        self.current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Comparison key for a path, insensitive to case and Unicode form. macOS stores an accented
/// name decomposed while a package's param.json composes it; without this a title with a
/// macron would look renamed on every run.
fn key(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
        .nfc()
        .collect::<String>()
        .to_lowercase()
}

fn rel(container: &Path, p: &Path) -> String {
    p.strip_prefix(container)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Safe for a file or folder name on every OS: reserved characters and control codes become
/// spaces, runs of space collapse, and it is cut at 120 characters.
pub fn sanitize(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| {
            if "<>:\"/\\|?*".contains(c) || (c as u32) < 32 {
                ' '
            } else {
                c
            }
        })
        .collect();
    let joined = mapped.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = joined.trim_matches(|c| c == ' ' || c == '.').to_string();
    if trimmed.chars().count() > 120 {
        let cut: String = trimmed.chars().take(120).collect();
        cut.trim_end_matches([' ', '.']).to_string()
    } else {
        trimmed
    }
}

/// The third content-ID segment: `UP1001-PPSA14251_00-DONPACK…` → `DONPACK…`.
/// Only letters, digits and `_` are kept as a suffix: the segment comes from the package and
/// ends up in a file name.
pub fn content_suffix(cid: &str) -> &str {
    let seg = cid.split('-').nth(2).unwrap_or("");
    if seg.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        seg
    } else {
        ""
    }
}

/// A move's `from`/`to` must stay inside its container: relative, ordinary names only.
fn contained(rel: &str) -> bool {
    let p = Path::new(rel);
    p.components().next().is_some()
        && p.components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

fn region_short(region: &str) -> &str {
    match region {
        "Europe" => "EU",
        "Americas" => "US",
        "Japan" => "JP",
        "Asia" => "AS",
        r => r,
    }
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "base" => "BASE",
        "patch" => "PATCH",
        "dlc" => "DLC",
        _ => "",
    }
}

/// `Title - TitleID - vVersion - Region - KIND`. DLC also carries its content-ID segment:
/// sibling DLC for one title share title, ID and version and would otherwise collide.
pub fn clean_name(info: &PkgInfo, title: &str) -> String {
    let tid = info.title_id.trim();
    let title = title.trim();
    let mut parts: Vec<String> = Vec::new();
    if !title.is_empty() && title != tid {
        parts.push(title.into());
    }
    if !tid.is_empty() {
        parts.push(tid.into());
    }
    if !info.version.trim().is_empty() {
        parts.push(format!("v{}", info.version.trim()));
    }
    let region = region_short(&info.region);
    if !region.is_empty() {
        parts.push(region.into());
    }
    let kind = kind_label(&info.kind);
    if !kind.is_empty() {
        parts.push(if info.kind_confident {
            kind.into()
        } else {
            format!("{kind}_UNSURE")
        });
    }
    let name = sanitize(&parts.join(" - "));
    if info.kind == "dlc" {
        let suffix = content_suffix(&info.content_id);
        if !suffix.is_empty() {
            return sanitize(&format!("{name} ({suffix})"));
        }
    }
    name
}

/// `CUSA03041 - Red Dead Redemption 2`, or the ID alone when untitled.
pub fn title_folder(title_id: &str, title: &str) -> String {
    let (tid, title) = (title_id.trim(), title.trim());
    if !tid.is_empty() && !title.is_empty() && title != tid {
        sanitize(&format!("{tid} - {title}"))
    } else {
        sanitize(if tid.is_empty() { title } else { tid })
    }
}

/// The folder title for one Title ID: a base names the game, a patch is next best; DLC alone
/// share the game's name as a prefix ("Mafia The Old Country - Padrino Pack").
fn group_title(members: &[&Facts]) -> String {
    for kind in ["base", "patch"] {
        let mut titles: Vec<&str> = members
            .iter()
            .filter(|f| f.info.kind == kind && !f.info.title.is_empty())
            .map(|f| f.info.title.as_str())
            .collect();
        titles.sort_by_key(|t| (t.chars().count(), *t));
        if let Some(t) = titles.first() {
            return (*t).to_string();
        }
    }
    let mut titles: Vec<&str> = members
        .iter()
        .filter(|f| !f.info.title.is_empty())
        .map(|f| f.info.title.as_str())
        .collect();
    titles.sort();
    titles.dedup();
    match titles.len() {
        0 => return String::new(),
        1 => return titles[0].to_string(),
        _ => {}
    }
    let first: Vec<char> = titles[0].chars().collect();
    let mut n = first.len();
    for t in &titles[1..] {
        n = n.min(
            first
                .iter()
                .zip(t.chars())
                .take_while(|(a, b)| **a == *b)
                .count(),
        );
    }
    let prefix: String = first[..n].iter().collect();
    for sep in [" - ", ": ", " ("] {
        if let Some(i) = prefix.rfind(sep) {
            return prefix[..i].trim().to_string();
        }
    }
    let stripped = prefix.trim_matches([' ', '-', ':', '(']);
    if stripped.is_empty() {
        titles[0].to_string()
    } else {
        stripped.to_string()
    }
}

/// Every part of the split set `path` belongs to, from its first file; just `[path]` for a
/// package on its own. See [`split`]: only files with no package header of their own join a set,
/// so `Bloodborne_1.pkg` and `Bloodborne_2.pkg` keep their own names and identities.
pub fn split_siblings(tree: &dyn Tree, path: &Path) -> Vec<PathBuf> {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return vec![path.to_path_buf()];
    };
    if !name.to_ascii_lowercase().ends_with(".pkg") && split::suffix_part(name).is_none() {
        return vec![path.to_path_buf()];
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let entries = tree.list(dir).unwrap_or_default();
    let names: Vec<&str> = entries
        .iter()
        .filter(|e| e.is_file && !e.name.starts_with('.') && !junk::is_junk_name(&e.name))
        .map(|e| e.name.as_str())
        .collect();
    let sets = split::split_sets(&names, &mut |n| {
        tree.head(&dir.join(n))
            .is_some_and(|h| split::is_package_head(&h))
    });
    let set = sets
        .iter()
        .find(|s| s.lead == name || s.parts.iter().any(|(part, _)| part == name));
    match set {
        Some(s) => std::iter::once(s.lead.as_str())
            .chain(s.parts.iter().map(|(part, _)| part.as_str()))
            .map(|n| dir.join(n))
            .collect(),
        None => vec![path.to_path_buf()],
    }
}

/// Where one file of a package goes under a name: `{stem}.pkg`, or its part's slot in a set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Whole,
    /// `{stem}_{n}.pkg`.
    Named(u32),
    /// `{stem}.pkg.{n}`.
    Suffix(u32),
}

/// The slots of a package's files, keeping its parts' numbering and its layout.
fn slots_for(parts: &[PathBuf]) -> Vec<Slot> {
    let name = |p: &PathBuf| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    if parts.len() < 2 {
        return vec![Slot::Whole];
    }
    if parts[1..]
        .iter()
        .any(|p| split::suffix_part(&name(p)).is_some())
    {
        return parts
            .iter()
            .map(|p| split::suffix_part(&name(p)).map_or(Slot::Whole, |(_, n)| Slot::Suffix(n)))
            .collect();
    }
    parts
        .iter()
        .map(|p| split::named_part(&name(p)).map_or(Slot::Whole, |(_, n)| Slot::Named(n)))
        .collect()
}

/// A part is missing from the run of a set's slots.
fn set_has_gap(slots: &[Slot]) -> bool {
    let numbers = slots.iter().filter_map(|s| match s {
        Slot::Named(n) | Slot::Suffix(n) => Some(*n),
        Slot::Whole => None,
    });
    numbers.enumerate().any(|(i, n)| u64::from(n) != i as u64)
}

/// Discovery types understood by the existing Collection metadata readers.
fn source_kind(tree: &dyn Tree, path: &Path) -> String {
    if path.parent().and_then(|p| tree.list(p)).is_some_and(|es| {
        es.iter()
            .any(|e| e.is_dir && path.file_name().is_some_and(|n| n == e.name.as_str()))
    }) {
        "folder".into()
    } else {
        super::scan::file_type(&path.file_name().unwrap_or_default().to_string_lossy())
            .unwrap_or("unknown")
            .into()
    }
}

fn is_game_folder(tree: &dyn Tree, dir: &Path, entries: &[super::organize_fs::DirEnt]) -> bool {
    entries.iter().any(|e| e.is_dir && e.name == "sce_sys")
        && tree.list(&dir.join("sce_sys")).is_some_and(|es| {
            es.iter()
                .any(|e| e.is_file && matches!(e.name.as_str(), "param.json" | "param.sfo"))
        })
}

/// One candidate per complete game source. Never walk inside an extracted game.
#[cfg(test)]
fn discover(
    tree: &dyn Tree,
    container: &Path,
    excluded: &[PathBuf],
) -> (Vec<PathBuf>, Vec<Skipped>) {
    discover_watched(tree, container, excluded, &PlanWatch::default())
}

fn discover_watched(
    tree: &dyn Tree,
    container: &Path,
    excluded: &[PathBuf],
    watch: &PlanWatch,
) -> (Vec<PathBuf>, Vec<Skipped>) {
    let mut found = Vec::new();
    let mut skipped = Vec::new();
    let mut stack = vec![(container.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if watch.stopped() {
            break;
        }
        watch.set_current(rel(container, &dir));
        let Some(mut entries) = tree.list(&dir) else {
            skipped.push(Skipped {
                container: path_string(container),
                path: rel(container, &dir),
                reason: SKIP_UNREADABLE.into(),
                detail: "directory could not be listed".into(),
            });
            continue;
        };
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        if is_game_folder(tree, &dir, &entries) {
            if dir == container {
                skipped.push(Skipped {
                    container: path_string(container),
                    path: String::new(),
                    reason: "game is the collection root".into(),
                    detail: "Choose its parent folder to organize this game.".into(),
                });
            } else {
                found.push(dir);
            }
            continue;
        }
        for e in entries {
            if e.name.starts_with('.') || junk::is_junk_name(&e.name) {
                continue;
            }
            let path = dir.join(&e.name);
            if excluded.iter().any(|r| path.starts_with(r)) {
                continue;
            }
            if e.is_dir {
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                } else {
                    skipped.push(Skipped {
                        container: path_string(container),
                        path: rel(container, &path),
                        reason: "depth limit".into(),
                        detail: "Directory exceeds the organization scan depth.".into(),
                    });
                }
            } else if e.is_file && super::scan::file_type(&e.name).is_some() {
                found.push(path);
            }
        }
    }
    found.sort();
    (found, skipped)
}

#[cfg(test)]
pub fn scan(tree: &dyn Tree, container: &Path) -> Vec<PathBuf> {
    discover(tree, container, &[]).0
}

/// A configured root is the organization boundary, regardless of its child folder names.
#[cfg(test)]
pub fn containers(tree: &dyn Tree, root: &Path) -> Vec<PathBuf> {
    if tree.list(root).is_some() {
        vec![root.to_path_buf()]
    } else {
        Vec::new()
    }
}

/// Reads a package's facts, taking them from `cached(path, size, mtime)` when it knows the
/// file as it is now (the Collection's index), and from the file otherwise.
/// On a server the index's facts save reading each package's header over the network.
pub fn read_facts_with(
    tree: &dyn Tree,
    path: &Path,
    cached: &dyn Fn(&Path, u64, i64) -> Option<PkgInfo>,
) -> Facts {
    let kind = source_kind(tree, path);
    let parts = if kind == "pkg" {
        split_siblings(tree, path)
    } else {
        vec![path.to_path_buf()]
    };
    let size = if kind == "folder" {
        0
    } else {
        parts
            .iter()
            .map(|p| tree.meta(p).map(|m| m.size).unwrap_or(0))
            .sum()
    };
    // The newest of a set's files, as the Collection's index keeps it.
    let mtime = parts
        .iter()
        .filter_map(|p| tree.meta(p))
        .map(|m| m.mtime)
        .max()
        .unwrap_or(0);
    // Tree paths use literal server names; metadata readers consume encoded remote URLs.
    let identity_path = match super::organize_fs::split_raw(&path_string(&parts[0])) {
        Some((id, path)) => PathBuf::from(format!(
            "remote://{}/{}",
            id,
            path.split('/')
                .filter(|s| !s.is_empty())
                .map(crate::remote::path::encode_segment)
                .collect::<Vec<_>>()
                .join("/")
        )),
        None => parts[0].clone(),
    };
    let info = (kind != "folder")
        .then(|| cached(&parts[0], size, mtime))
        .flatten()
        .or_else(|| {
            if parts.len() > 1 {
                let gap = set_has_gap(&slots_for(&parts));
                Some(super::identify::identify_set(&identity_path, size, gap))
            } else {
                super::identify::identify(&identity_path, &kind)
            }
        })
        .unwrap_or_default();
    Facts {
        path: path.to_path_buf(),
        parts,
        size,
        info,
    }
}

/// Every file name in the folder that a move must not land on: everything that stays.
fn taken_names(
    tree: &dyn Tree,
    container: &Path,
    moving: &HashSet<String>,
    watch: &PlanWatch,
) -> HashSet<String> {
    let mut taken = HashSet::new();
    let mut stack = vec![(container.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if watch.stopped() {
            break;
        }
        for e in tree.list(&dir).unwrap_or_default() {
            let path = dir.join(&e.name);
            if e.is_dir {
                // A folder's name is taken too: no file may land on it.
                taken.insert(key(&path));
                if !e.name.starts_with('.') && depth < MAX_DEPTH && !moving.contains(&key(&path)) {
                    if let Some(children) = tree.list(&path) {
                        if !is_game_folder(tree, &path, &children) {
                            stack.push((path, depth + 1));
                        }
                    }
                }
            } else if !junk::is_junk_name(&e.name) {
                let k = key(&path);
                if !moving.contains(&k) {
                    taken.insert(k);
                }
            }
        }
    }
    taken
}

/// Names for one package, or for every part of a split set. A set is placed only where all of
/// its slots are free, keeping its parts' numbering (a set missing part 1 stays 0 and 2, never
/// closed up into a fake complete set). A file already at one of its own candidate names keeps
/// it, so re-planning an organized library moves nothing and two packages sharing a computed
/// name never trade places.
fn unique_names(
    dest_dir: &Path,
    base: &str,
    content_id: &str,
    taken: &HashSet<String>,
    file_slots: &[Slot],
    current: Option<&[String]>,
    extension: &str,
) -> Vec<String> {
    let slots = |stem: &str| -> Vec<String> {
        file_slots
            .iter()
            .map(|s| match s {
                Slot::Whole => format!("{stem}{extension}"),
                Slot::Named(n) => format!("{stem}_{n}.pkg"),
                Slot::Suffix(n) => format!("{stem}{extension}.{n}"),
            })
            .collect()
    };
    let mut stems = vec![base.to_string()];
    let suffix = content_suffix(content_id);
    if !suffix.is_empty() {
        stems.push(format!("{base} ({suffix})"));
    }
    stems.extend((2..66).map(|n| format!("{base} ({n})")));
    if let Some(current) = current {
        let want: Vec<String> = current.iter().map(|n| key(&dest_dir.join(n))).collect();
        for stem in &stems {
            let names = slots(stem);
            if names
                .iter()
                .map(|n| key(&dest_dir.join(n)))
                .collect::<Vec<_>>()
                == want
            {
                return names;
            }
        }
    }
    for stem in &stems {
        let names = slots(stem);
        if names
            .iter()
            .all(|n| !taken.contains(&key(&dest_dir.join(n))))
        {
            return names;
        }
    }
    // More collisions than the list allows: unique rather than overwriting anything.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    slots(&format!("{base} ({stamp})"))
}

/// Pure: what organizing `container` would do. `facts` reads one package; nothing is touched.
#[cfg(test)]
pub fn plan_with(tree: &dyn Tree, container: &Path, facts: &dyn Fn(&Path) -> Facts) -> Plan {
    plan_files(tree, container, scan(tree, container), facts)
}

/// `plan_with` over the given packages of `container` (all of them, or a root's loose ones).
#[cfg(test)]
pub fn plan_files(
    tree: &dyn Tree,
    container: &Path,
    files: Vec<PathBuf>,
    facts: &dyn Fn(&Path) -> Facts,
) -> Plan {
    plan_files_watched(tree, container, files, facts, &PlanWatch::default())
}

fn plan_files_watched(
    tree: &dyn Tree,
    container: &Path,
    files: Vec<PathBuf>,
    facts: &dyn Fn(&Path) -> Facts,
    watch: &PlanWatch,
) -> Plan {
    let cname = path_string(container);
    let mut out = Plan::default();
    let mut parsed: Vec<Facts> = Vec::new();
    watch.total.fetch_add(files.len() as u64, Ordering::Relaxed);
    watch.reading.store(true, Ordering::Relaxed);
    for path in files {
        if watch.stopped() {
            break;
        }
        watch.set_current(rel(container, &path));
        watch.done.fetch_add(1, Ordering::Relaxed);
        let kind = source_kind(tree, &path);
        if !matches!(
            kind.as_str(),
            "pkg" | "folder" | "mount.exfat" | "mount.ffpkg" | "mount.ffpfs" | "mount.ffpfsc"
        ) {
            out.skipped.push(Skipped {
                container: cname.clone(),
                path: rel(container, &path),
                reason: "unsupported format".into(),
                detail: "Extract archives before organizing their game contents.".into(),
            });
            continue;
        }
        // Only part 0 of a split set carries a header: plan the set from it.
        let parts = if kind == "pkg" {
            split_siblings(tree, &path)
        } else {
            vec![path.clone()]
        };
        if parts.len() > 1 && parts[0] != path {
            continue;
        }
        let f = facts(&path);
        let r = rel(container, &path);
        let skip = |reason: &str, detail: String| Skipped {
            container: cname.clone(),
            path: r.clone(),
            reason: reason.into(),
            detail,
        };
        if let Some(e) = &f.info.error {
            out.skipped.push(skip(SKIP_UNREADABLE, e.clone()));
        } else if !f.info.complete {
            out.skipped
                .push(skip(SKIP_INCOMPLETE, f.info.kind_reason.clone()));
        } else if !super::is_game_id(&f.info.title_id)
            || !matches!(f.info.platform.as_str(), "PS3" | "PS4" | "PS5")
        {
            out.skipped.push(skip(SKIP_NO_ID, String::new()));
        } else {
            parsed.push(f);
        }
    }

    let mut groups: BTreeMap<String, Vec<&Facts>> = BTreeMap::new();
    for f in &parsed {
        groups.entry(f.info.title_id.clone()).or_default().push(f);
    }
    let moving: HashSet<String> = parsed
        .iter()
        .flat_map(|f| f.parts.iter().map(|p| key(p)))
        .collect();
    let mut taken = taken_names(tree, container, &moving, watch);

    for (title_id, members) in groups {
        let title = group_title(&members);
        let folder = title_folder(&title_id, &title);
        // One platform per Title ID: the most common reading, ties by name.
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for f in &members {
            *counts.entry(f.info.platform.as_str()).or_default() += 1;
        }
        let platform = counts
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
            .map(|(p, _)| p.to_string())
            .unwrap_or_default();
        let dest_dir = container.join(&platform).join(&folder);

        // The same release stored twice is filed beside its twin and marked DUPLICATE. Which
        // copy is canonical must not depend on the marker, or the two would swap forever.
        let mut ordered = members.clone();
        ordered.sort_by_key(|f| {
            let stem = f
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            (
                f.info.kind.clone(),
                stem.contains(DUPLICATE_TAG),
                f.path.clone(),
            )
        });
        let mut seen: HashMap<(String, String, String, String, u64), String> = HashMap::new();
        for f in ordered {
            let kind = source_kind(tree, &f.path);
            let extension = if kind == "folder" {
                String::new()
            } else {
                format!(
                    ".{}",
                    f.path
                        .extension()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_ascii_lowercase()
                )
            };
            let identity = (
                kind,
                f.info.content_id.clone(),
                f.info.kind.clone(),
                f.info.version.clone(),
                f.size,
            );
            let duplicate_of = seen.get(&identity).cloned();
            if duplicate_of.is_none() {
                seen.insert(identity, rel(container, &f.path));
            }
            let mut base = clean_name(
                &f.info,
                if title.is_empty() {
                    &f.info.title
                } else {
                    &title
                },
            );
            if base.is_empty() {
                out.skipped.push(Skipped {
                    container: cname.clone(),
                    path: rel(container, &f.path),
                    reason: SKIP_NO_ID.into(),
                    detail: "no usable name".into(),
                });
                continue;
            }
            if duplicate_of.is_some() {
                base.push_str(DUPLICATE_TAG);
            }
            let file_slots = slots_for(&f.parts);
            let here: Option<Vec<String>> = f
                .parts
                .iter()
                .all(|p| p.parent().map(key) == Some(key(&dest_dir)))
                .then(|| {
                    f.parts
                        .iter()
                        .map(|p| {
                            p.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned()
                        })
                        .collect()
                });
            let names = unique_names(
                &dest_dir,
                &base,
                &f.info.content_id,
                &taken,
                &file_slots,
                here.as_deref(),
                &extension,
            );
            for n in &names {
                taken.insert(key(&dest_dir.join(n)));
            }
            for (i, (part, name)) in f.parts.iter().zip(&names).enumerate() {
                let dest = dest_dir.join(name);
                if key(&dest) == key(part) {
                    out.skipped.push(Skipped {
                        container: cname.clone(),
                        path: rel(container, part),
                        reason: SKIP_IN_PLACE.into(),
                        detail: String::new(),
                    });
                    continue;
                }
                let mv = Move {
                    container: cname.clone(),
                    from: rel(container, part),
                    to: rel(container, &dest),
                    platform: platform.clone(),
                    title_id: title_id.clone(),
                    title: title.clone(),
                    kind: f.info.kind.clone(),
                    confident: f.info.kind_confident,
                    reason: f.info.kind_reason.clone(),
                    version: f.info.version.clone(),
                    region: f.info.region.clone(),
                    size: if f.parts.len() == 1 {
                        f.size
                    } else {
                        tree.meta(part).map(|m| m.size).unwrap_or(0)
                    },
                    content_id: f.info.content_id.clone(),
                    part: (names.len() > 1).then(|| format!("{} of {}", i + 1, names.len())),
                    set: if names.len() > 1 {
                        rel(container, &f.parts[0])
                    } else {
                        String::new()
                    },
                    duplicate: duplicate_of.is_some(),
                    duplicate_of: duplicate_of.clone(),
                };
                if !mv.confident {
                    out.unsure.push(mv.clone());
                }
                out.moves.push(mv);
            }
        }
    }
    out
}

/// A plan whose package facts come from the Collection's index where it is current. `roots`
/// are (the root as a tree path, the root as the index keys it).
#[cfg(test)]
pub fn plan_cached(
    tree: &dyn Tree,
    container: &Path,
    lib: Option<&super::Library>,
    roots: &[(PathBuf, String)],
) -> Plan {
    plan_watched(tree, container, lib, roots, &PlanWatch::default())
}

/// [`plan_cached`], telling `watch` how far it is and stopping when it says so (the plan it then
/// returns is cut short: the caller knows, it set the flag).
pub fn plan_watched(
    tree: &dyn Tree,
    container: &Path,
    lib: Option<&super::Library>,
    roots: &[(PathBuf, String)],
    watch: &PlanWatch,
) -> Plan {
    let tree = &PlanTree::new(tree);
    let cached = |p: &Path, size: u64, mtime: i64| -> Option<PkgInfo> {
        let lib = lib?;
        roots.iter().find_map(|(root, key_root)| {
            let rel = p.strip_prefix(root).ok()?;
            let rel = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let c = lib.pkg_cache.get(&format!("{key_root}|{rel}"))?;
            (c.size == size && c.mtime == mtime && c.v == super::IDENTIFY_VERSION)
                .then(|| c.pkg.clone())
        })
    };
    let excluded: Vec<PathBuf> = roots
        .iter()
        .map(|(r, _)| r)
        .filter(|r| r.as_path() != container && r.starts_with(container))
        .cloned()
        .collect();
    let (files, skipped) = discover_watched(tree, container, &excluded, watch);
    let facts = |p: &Path| {
        let mut f = read_facts_with(tree, p, &cached);
        if source_kind(tree, p) == "folder" {
            if let Some(location) = lib.and_then(|lib| {
                lib.games.values().flat_map(|g| &g.locations).find(|l| {
                    roots
                        .iter()
                        .any(|(r, key)| key == &l.root && r.join(&l.path) == p)
                })
            }) {
                f.size = location.size_bytes;
            }
        }
        f
    };
    let mut plan = plan_files_watched(tree, container, files, &facts, watch);
    plan.skipped.extend(skipped);
    if let Some(trouble) = tree.trouble() {
        // A folder the server would not list: its packages are missing from this preview.
        plan.skipped.push(Skipped {
            container: path_string(container),
            path: String::new(),
            reason: SKIP_UNREADABLE.into(),
            detail: trouble,
        });
    }
    plan
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoLog {
    pub version: u32,
    pub container_root: String,
    /// The saved server's name when the folder is on one, for the list of runs.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub server: String,
    /// Which server the folder was on. Undo refuses when the saved connection (same id) now
    /// points somewhere else. Logs from before it was recorded have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    pub created_at: String,
    pub moves: Vec<Move>,
    #[serde(default)]
    pub completed: Vec<Move>,
    #[serde(default)]
    pub failed: Vec<FailedMove>,
    #[serde(default)]
    pub removed_dirs: Vec<String>,
    #[serde(default)]
    pub kept_dirs: Vec<String>,
    /// Set once the run was undone, so it cannot be undone twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverted_at: Option<String>,
}

/// A server folder's whereabouts, as the saved connection said when the run was made.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Origin {
    pub protocol: String,
    /// Lowercase.
    pub host: String,
    pub port: u16,
    /// The SMB share, lowercase ("" for other protocols).
    #[serde(default)]
    pub share: String,
    /// The folder's server path.
    pub path: String,
}

impl Origin {
    /// The same server: protocol, host, port and share.
    pub fn same_server(&self, other: &Origin) -> bool {
        self.protocol == other.protocol
            && self.host == other.host
            && self.port == other.port
            && self.share == other.share
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FailedMove {
    #[serde(flatten)]
    pub mv: Move,
    pub error: String,
    /// The connection dropped during this move: it may have happened. Undo looks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub uncertain: bool,
}

impl FailedMove {
    fn new(mv: Move, error: impl Into<String>) -> Self {
        Self {
            mv,
            error: error.into(),
            uncertain: false,
        }
    }
}

fn now_iso() -> String {
    chrono_like_now()
}

/// RFC 3339 in UTC without a date crate.
fn chrono_like_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn write_log(path: &Path, log: &UndoLog) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(log).map_err(std::io::Error::other)?;
    // On the disk before it takes the log's name: a power cut right after a run must leave a log
    // Undo can read, not a name with nothing behind it.
    {
        let mut f = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut f, &bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    // The rename itself. A folder cannot be opened for this on Windows, which journals it anyway.
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// Removes a folder holding nothing but Finder's files. True when it is gone afterwards.
/// Finder can rewrite `.DS_Store` the moment it notices a folder, so it retries briefly.
fn try_remove_dir(tree: &dyn Tree, dir: &Path) -> bool {
    for _ in 0..3 {
        let Some(entries) = tree.list(dir) else {
            return tree.exists(dir) == Ok(false);
        };
        let entries: Vec<PathBuf> = entries.iter().map(|e| dir.join(&e.name)).collect();
        if entries.iter().any(|p| !tree.is_removable_junk(p)) {
            return false;
        }
        for p in &entries {
            tree.remove_junk(p);
        }
        if tree.remove_dir(dir).is_ok() || tree.exists(dir) == Ok(false) {
            return true;
        }
    }
    false
}

/// Source folders the moves emptied, removed up to the container. Returns (removed, kept):
/// kept names one that held only Finder's files and still could not be removed.
fn prune_empty(
    tree: &dyn Tree,
    container: &Path,
    started_in: &HashSet<PathBuf>,
) -> (Vec<String>, Vec<String>) {
    let (mut removed, mut kept) = (Vec::new(), Vec::new());
    let mut dirs: Vec<&PathBuf> = started_in.iter().collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.as_os_str().len()));
    for start in dirs {
        let mut dir = start.clone();
        while dir != container && dir.starts_with(container) {
            let Some(entries) = tree.list(&dir) else {
                break;
            };
            if entries
                .iter()
                .any(|e| !tree.is_removable_junk(&dir.join(&e.name)))
            {
                break;
            }
            if try_remove_dir(tree, &dir) {
                removed.push(rel(container, &dir));
            } else {
                kept.push(rel(container, &dir));
                break;
            }
            match dir.parent() {
                Some(p) => dir = p.to_path_buf(),
                None => break,
            }
        }
    }
    (removed, kept)
}

/// Makes `dir` and the folders above it, down from `container` (which exists). Folders known
/// to exist are skipped, so a run of moves into one folder makes it once.
fn make_dirs(
    tree: &dyn Tree,
    container: &Path,
    dir: &Path,
    known: &mut HashSet<PathBuf>,
) -> Result<(), OpError> {
    let Ok(below) = dir.strip_prefix(container) else {
        return Err(OpError::Other("path leaves the folder".into()));
    };
    let mut at = container.to_path_buf();
    for c in below.components() {
        at.push(c);
        if known.contains(&at) {
            continue;
        }
        tree.create_dir(&at)?;
        known.insert(at.clone());
    }
    Ok(())
}

/// How many moves run between rewrites of the log.
const LOG_EVERY: usize = 16;

/// Moves in order, the parts of one split set together.
fn by_set(moves: Vec<Move>) -> Vec<Vec<Move>> {
    let mut groups: Vec<Vec<Move>> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for mv in moves {
        if mv.set.is_empty() {
            groups.push(vec![mv]);
            continue;
        }
        match at.get(&mv.set) {
            Some(i) => groups[*i].push(mv),
            None => {
                at.insert(mv.set.clone(), groups.len());
                groups.push(vec![mv]);
            }
        }
    }
    groups
}

/// What one group of moves came to.
#[derive(Default)]
struct GroupRun {
    moved: Vec<Move>,
    failed: Vec<FailedMove>,
    /// Folders a move that was put back may have left empty.
    emptied: Vec<PathBuf>,
    lost: Option<String>,
}

fn move_one(
    tree: &dyn Tree,
    container: &Path,
    mv: &Move,
    made: &mut HashSet<PathBuf>,
) -> Result<(), OpError> {
    if !contained(&mv.from) || !contained(&mv.to) {
        return Err(OpError::Other("path leaves the folder".into()));
    }
    let (src, dst) = (container.join(&mv.from), container.join(&mv.to));
    if tree.exists(&dst)? {
        return Err(OpError::Exists("destination appeared".into()));
    }
    dst.parent()
        .map_or(Ok(()), |d| make_dirs(tree, container, d, made))
        .and_then(|()| tree.rename_new(&src, &dst))
}

/// Why a set cannot start: a part it needs is gone, or a name it would take is there now. Asked
/// before the first rename, so the common failures never split a set across two folders.
fn set_cannot_start(
    tree: &dyn Tree,
    container: &Path,
    group: &[Move],
) -> Result<Option<String>, OpError> {
    for mv in group {
        if !contained(&mv.from) || !contained(&mv.to) {
            return Ok(Some("path leaves the folder".into()));
        }
        if !tree.exists(&container.join(&mv.from))? {
            return Ok(Some(format!("{} is not there any more", mv.from)));
        }
        if tree.exists(&container.join(&mv.to))? {
            return Ok(Some("destination appeared".into()));
        }
    }
    Ok(None)
}

/// Runs one move, or all the moves of one split set. A set is one unit: when a part cannot move,
/// the parts already moved go back, because a set across two folders reads as a broken package
/// (its first part incomplete, the others unreadable) until someone finds the failure and undoes
/// the run. Only a server that stops answering can leave it part way; the log then says what
/// happened and Undo finishes the job.
fn run_group(
    tree: &dyn Tree,
    container: &Path,
    group: Vec<Move>,
    made: &mut HashSet<PathBuf>,
) -> GroupRun {
    let mut run = GroupRun::default();
    let whole_set = group.len() > 1;
    if whole_set {
        match set_cannot_start(tree, container, &group) {
            Ok(None) => {}
            Ok(Some(why)) => {
                run.failed
                    .extend(group.into_iter().map(|mv| FailedMove::new(mv, why.clone())));
                return run;
            }
            Err(OpError::Lost(why)) => {
                run.failed.extend(group.into_iter().map(|mv| {
                    FailedMove::new(
                        mv,
                        format!("not tried: the server stopped answering ({why})"),
                    )
                }));
                run.lost = Some(why);
                return run;
            }
            Err(e) => {
                let why = e.to_string();
                run.failed
                    .extend(group.into_iter().map(|mv| FailedMove::new(mv, why.clone())));
                return run;
            }
        }
    }
    let mut rest = group.into_iter();
    while let Some(mv) = rest.next() {
        let failure = match move_one(tree, container, &mv, made) {
            Ok(()) => {
                run.moved.push(mv);
                continue;
            }
            Err(OpError::Lost(e)) => {
                run.failed.push(FailedMove {
                    mv,
                    error: format!("the server stopped answering during this move ({e})"),
                    uncertain: true,
                });
                run.failed.extend(rest.map(|mv| {
                    FailedMove::new(mv, format!("not tried: the server stopped answering ({e})"))
                }));
                run.lost = Some(e);
                return run;
            }
            Err(OpError::Exists(_)) => "destination appeared".to_string(),
            Err(OpError::Other(e)) => e,
        };
        run.failed.push(FailedMove::new(mv, failure.clone()));
        run.failed.extend(rest.map(|mv| {
            FailedMove::new(
                mv,
                format!("not tried: another part of this set could not move ({failure})"),
            )
        }));
        // The parts that went, back. One that cannot stays recorded as moved, for Undo.
        let done = std::mem::take(&mut run.moved);
        for mv in done.into_iter().rev() {
            let (src, dst) = (container.join(&mv.from), container.join(&mv.to));
            match tree.rename_new(&dst, &src) {
                Ok(()) => {
                    run.emptied.extend(dst.parent().map(Path::to_path_buf));
                    run.failed.push(FailedMove::new(
                        mv,
                        format!("put back: another part of this set could not move ({failure})"),
                    ));
                }
                Err(OpError::Lost(e)) => {
                    run.lost = Some(e);
                    run.moved.push(mv);
                }
                Err(_) => run.moved.push(mv),
            }
        }
        return run;
    }
    run
}

/// Runs `moves` (all in `container`), the log written before the first one. The parts of a split
/// set move as one (see [`run_group`]). A dropped
/// connection (one a fresh sign-in did not cure) stops the run: the move in flight is logged as
/// uncertain, the rest as not tried, and the log stays good for Undo.
pub fn apply(
    tree: &dyn Tree,
    container: &Path,
    moves: Vec<Move>,
    log_path: &Path,
    server: &str,
    origin: Option<Origin>,
) -> std::io::Result<UndoLog> {
    let mut log = UndoLog {
        version: 1,
        container_root: path_string(container),
        server: server.to_string(),
        origin,
        created_at: now_iso(),
        moves: moves.clone(),
        completed: Vec::new(),
        failed: Vec::new(),
        removed_dirs: Vec::new(),
        kept_dirs: Vec::new(),
        reverted_at: None,
    };
    write_log(log_path, &log)?;
    let mut started_in = HashSet::new();
    let mut made = HashSet::new();
    let mut lost: Option<String> = None;
    for group in by_set(moves) {
        if let Some(why) = &lost {
            for mv in group {
                log.failed.push(FailedMove::new(
                    mv,
                    format!("not tried: the server stopped answering ({why})"),
                ));
            }
            continue;
        }
        let run = run_group(tree, container, group, &mut made);
        if let Some(why) = run.lost {
            lost = Some(why);
        }
        started_in.extend(run.emptied);
        for mv in run.moved {
            let src = container.join(&mv.from);
            if let (Some(dir), Some(name)) = (src.parent(), src.file_name()) {
                tree.remove_junk(&dir.join(format!("._{}", name.to_string_lossy())));
                started_in.insert(dir.to_path_buf());
            }
            log.completed.push(mv);
            // Kept current as it goes, so a run that dies part way can still be undone.
            if log.completed.len().is_multiple_of(LOG_EVERY) {
                let _ = write_log(log_path, &log);
            }
        }
        let uncertain = run.failed.iter().any(|f| f.uncertain);
        log.failed.extend(run.failed);
        if uncertain {
            let _ = write_log(log_path, &log);
        }
    }
    if lost.is_none() {
        let (removed, kept) = prune_empty(tree, container, &started_in);
        log.removed_dirs = removed;
        log.kept_dirs = kept;
    }
    write_log(log_path, &log)?;
    Ok(log)
}

/// Gives the tree a path is in.
pub type OpenTree = dyn Fn(&Path) -> Result<Box<dyn Tree>, String>;

#[derive(Debug, Clone, Default, Serialize)]
pub struct RevertResult {
    pub reverted: Vec<Move>,
    pub problems: Vec<FailedMove>,
    /// Something the user should know about how it was undone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Undoes a run from its log: only the moves that happened, newest first. `open` gives the
/// tree the run's folder is in (this computer, or its server).
pub fn revert(log_path: &Path, open: &OpenTree) -> std::io::Result<RevertResult> {
    let bytes = std::fs::read(log_path)?;
    let mut log: UndoLog = serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if log.reverted_at.is_some() {
        return Err(std::io::Error::other("this run was already undone"));
    }
    let root = PathBuf::from(&log.container_root);
    let tree = open(&root).map_err(std::io::Error::other)?;
    let tree = tree.as_ref();
    // The server must answer before anything is judged "gone".
    if !tree
        .exists(&root)
        .map_err(|e| std::io::Error::other(e.to_string()))?
    {
        return Err(std::io::Error::other(format!(
            "{} is not there any more",
            log.container_root
        )));
    }
    let mut out = RevertResult::default();
    let here = |p: &Path| tree.exists(p);
    // A run that died after its last log write, or lost its server mid-move, left moves that
    // may have happened but are not in `completed`; those whose file sits at the new name and
    // not the old one are undone too.
    let mut done = log.completed.clone();
    for mv in &log.moves {
        if done.contains(mv) || log.failed.iter().any(|f| &f.mv == mv && !f.uncertain) {
            continue;
        }
        if contained(&mv.from)
            && contained(&mv.to)
            && here(&root.join(&mv.to)) == Ok(true)
            && here(&root.join(&mv.from)) == Ok(false)
        {
            done.push(mv.clone());
        }
    }
    let mut made = HashSet::new();
    let mut lost = None;
    for mv in done.iter().rev() {
        let src = root.join(&mv.to);
        let dst = root.join(&mv.from);
        let problem = if let Some(why) = &lost {
            Some(format!("not tried: the server stopped answering ({why})"))
        } else if !contained(&mv.from) || !contained(&mv.to) {
            Some("path leaves the folder".to_string())
        } else {
            match (here(&src), here(&dst)) {
                // Already back under its old name (an earlier, partly finished undo): nothing
                // to do, and not a problem that should keep the run open.
                (Ok(false), Ok(true)) => None,
                (Ok(false), _) => Some("moved file is gone".to_string()),
                (_, Ok(true)) => Some("original name is taken".to_string()),
                (Ok(true), Ok(false)) => dst
                    .parent()
                    .map_or(Ok(()), |d| make_dirs(tree, &root, d, &mut made))
                    .and_then(|()| tree.rename_new(&src, &dst))
                    .err()
                    .map(|e| {
                        if let OpError::Lost(m) = &e {
                            lost = Some(m.clone());
                        }
                        e.to_string()
                    }),
                (Err(e), _) | (_, Err(e)) => {
                    if let OpError::Lost(m) = &e {
                        lost = Some(m.clone());
                    }
                    Some(e.to_string())
                }
            }
        };
        match problem {
            Some(error) => out.problems.push(FailedMove::new(mv.clone(), error)),
            None => out.reverted.push(mv.clone()),
        }
    }
    if let Some(why) = lost {
        // Left undoable: the moves already put back now sit at their old names, so the next
        // Undo skips them ("moved file is gone") and finishes the rest.
        return Err(std::io::Error::other(format!(
            "the server stopped answering part way ({} put back); undo again once it is back: {why}",
            out.reverted.len()
        )));
    }
    // Folders the run created and the undo emptied.
    let made: HashSet<PathBuf> = out
        .reverted
        .iter()
        .filter_map(|m| root.join(&m.to).parent().map(Path::to_path_buf))
        .collect();
    prune_empty(tree, &root, &made);
    // Only a complete undo is final. A move that could not go back (its old name taken, the
    // file gone) stays listed, and the run stays undoable so the person can fix the clash
    // and undo again; the moves already put back are then skipped as "moved file is gone".
    if out.problems.is_empty() {
        log.reverted_at = Some(now_iso());
        write_log(log_path, &log)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::organize_fs::LocalTree;
    use super::*;

    fn local(_: &Path) -> Result<Box<dyn Tree>, String> {
        Ok(Box::new(LocalTree))
    }

    struct Lib(PathBuf);
    impl Drop for Lib {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn lib(name: &str) -> Lib {
        let d = std::env::temp_dir().join(format!("ps5u-org-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("fpkg")).unwrap();
        Lib(d)
    }

    /// Facts by file name, the way the tests of the source app build packages: the file's
    /// contents only give it a size.
    fn table<'a>(entries: &'a [(&'a str, PkgInfo)]) -> impl Fn(&Path) -> Facts + 'a {
        move |p: &Path| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let parts = split_siblings(&LocalTree, p);
            let size = parts
                .iter()
                .map(|q| std::fs::metadata(q).map(|m| m.len()).unwrap_or(0))
                .sum();
            let info = entries
                .iter()
                .find(|(n, _)| name.starts_with(n))
                .map(|(_, i)| i.clone())
                .unwrap_or_default();
            Facts {
                path: p.to_path_buf(),
                parts,
                size,
                info,
            }
        }
    }

    fn info(title: &str, tid: &str, cid: &str, kind: &str, ver: &str) -> PkgInfo {
        PkgInfo {
            platform: "PS4".into(),
            title: title.into(),
            title_id: tid.into(),
            content_id: cid.into(),
            version: ver.into(),
            region: "Americas".into(),
            kind: kind.into(),
            kind_confident: true,
            complete: true,
            ..PkgInfo::default()
        }
    }

    fn put(dir: &Path, name: &str, bytes: usize) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), vec![7u8; bytes]).unwrap();
    }

    #[test]
    fn current_folder_metadata_and_unsupported_sources_are_visible() {
        let l = lib("folder-metadata");
        let root = &l.0;
        let sys = root.join("dump/sce_sys");
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::write(sys.join("param.json"), br#"{"titleId":"PPSA01234","contentVersion":"01.000.000","localizedParameters":{"defaultLanguage":"en-US","en-US":{"titleName":"Example"}}}"#).unwrap();
        put(root, "archive.zip", 10);
        put(root, "broken.ffpfs", 10);
        let roots = [(root.clone(), "root".into())];
        let plan = plan_cached(&LocalTree, root, None, &roots);
        assert_eq!(plan.moves.len(), 1, "{plan:?}");
        assert_eq!(plan.moves[0].from, "dump");
        assert!(plan.moves[0].to.starts_with("PS5/PPSA01234 - Example/"));
        assert!(plan
            .skipped
            .iter()
            .any(|s| s.path == "archive.zip" && s.reason == "unsupported format"));
        assert!(plan
            .skipped
            .iter()
            .any(|s| s.path == "broken.ffpfs" && s.reason == SKIP_UNREADABLE));
        std::fs::write(sys.join("param.json"), b"invalid JSON").unwrap();
        let fresh = plan_cached(&LocalTree, root, None, &roots);
        assert!(fresh.moves.is_empty());
        assert!(fresh
            .skipped
            .iter()
            .any(|s| s.path == "dump" && s.reason == SKIP_UNREADABLE));
    }

    #[test]
    fn discovery_reports_depth_limit_without_entering_game_assets() {
        let l = lib("depth-limit");
        let mut deep = l.0.clone();
        for _ in 0..=MAX_DEPTH {
            deep.push("nested");
        }
        put(&deep, "hidden.pkg", 1);
        let (found, skipped) = discover(&LocalTree, &l.0, &[]);
        assert!(found.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].reason, "depth limit");
    }

    #[cfg(unix)]
    #[test]
    fn discovery_does_not_follow_symlinks_outside_the_root() {
        let l = lib("symlink-root");
        let outside = lib("symlink-outside");
        put(&outside.0, "external.pkg", 1);
        std::os::unix::fs::symlink(&outside.0, l.0.join("linked")).unwrap();
        std::os::unix::fs::symlink(outside.0.join("external.pkg"), l.0.join("linked.pkg")).unwrap();
        assert!(scan(&LocalTree, &l.0).is_empty());
    }

    #[test]
    fn mixed_collection_is_discovered_at_the_configured_root() {
        let l = lib("mixed-root");
        let root = &l.0;
        put(&root.join("CUSA00900 - Bloodborne"), "base.pkg", 10);
        for ext in ["exfat", "ffpkg", "ffpfs", "FFPFSC"] {
            put(root, &format!("image.{ext}"), 12);
        }
        put(&root.join("PPSA01234-app/sce_sys"), "param.json", 2);
        put(&root.join("PPSA01234-app/Media"), "internal.pkg", 3);
        let found = scan(&LocalTree, root);
        assert_eq!(
            found.len(),
            6,
            "one package, four images and one intact folder: {found:?}"
        );
        assert!(found.contains(&root.join("PPSA01234-app")));
        assert!(!found.iter().any(|p| p.ends_with("internal.pkg")));
        assert_eq!(containers(&LocalTree, root), vec![root.clone()]);
    }

    #[test]
    fn mixed_formats_keep_extensions_and_folder_contents_through_undo() {
        let l = lib("mixed-undo");
        let root = &l.0;
        for ext in ["exfat", "ffpkg", "ffpfs", "ffpfsc"] {
            put(root, &format!("image.{ext}"), 12);
        }
        put(&root.join("dump/sce_sys"), "param.json", 2);
        put(&root.join("dump/Media"), "asset.bin", 23);
        let metadata = info(
            "Example",
            "PPSA01234",
            "UP0000-PPSA01234_00-EXAMPLE",
            "base",
            "1.00",
        );
        let facts = |p: &Path| Facts {
            path: p.to_path_buf(),
            parts: vec![p.to_path_buf()],
            size: 12,
            info: PkgInfo {
                platform: "PS5".into(),
                ..metadata.clone()
            },
        };
        let sources = [
            "image.exfat",
            "image.ffpkg",
            "image.ffpfs",
            "image.ffpfsc",
            "dump",
        ]
        .iter()
        .map(|p| root.join(p))
        .collect();
        let plan = plan_files(&LocalTree, root, sources, &facts);
        assert_eq!(plan.moves.len(), 5);
        for ext in ["exfat", "ffpkg", "ffpfs", "ffpfsc"] {
            let mv = plan
                .moves
                .iter()
                .find(|m| m.from == format!("image.{ext}"))
                .unwrap();
            assert!(mv.to.ends_with(&format!(".{ext}")), "{mv:?}");
            assert!(
                !mv.duplicate,
                "different representations are not identical files"
            );
        }
        let folder = plan
            .moves
            .iter()
            .find(|m| m.from == "dump")
            .unwrap()
            .to
            .clone();
        let log = root.join("undo.json");
        let run = apply(&LocalTree, root, plan.moves, &log, "", None).unwrap();
        assert!(run.failed.is_empty(), "{:?}", run.failed);
        assert_eq!(
            std::fs::read(root.join(&folder).join("Media/asset.bin")).unwrap(),
            vec![7; 23]
        );
        let again = plan_with(&LocalTree, root, &facts);
        assert!(again.moves.is_empty(), "{:?}", again.moves);
        assert_eq!(
            again
                .skipped
                .iter()
                .filter(|s| s.reason == SKIP_IN_PLACE)
                .count(),
            5
        );
        let undone = revert(&log, &local).unwrap();
        assert!(undone.problems.is_empty(), "{:?}", undone.problems);
        assert_eq!(undone.reverted.len(), 5);
        assert_eq!(
            std::fs::read(root.join("dump/Media/asset.bin")).unwrap(),
            vec![7; 23]
        );
    }

    #[test]
    fn extracted_folder_never_merges_with_an_existing_directory() {
        let l = lib("folder-collision");
        let root = &l.0;
        let dest = "PS5/PPSA01234 - Example/Example - PPSA01234 - v1.00 - US - BASE";
        put(&root.join("dump/sce_sys"), "param.json", 2);
        put(&root.join(dest), "keep.bin", 7);
        let facts = |p: &Path| Facts {
            path: p.to_path_buf(),
            parts: vec![p.to_path_buf()],
            size: 2,
            info: PkgInfo {
                platform: "PS5".into(),
                ..info("Example", "PPSA01234", "", "base", "1.00")
            },
        };
        let plan = plan_files(&LocalTree, root, vec![root.join("dump")], &facts);
        assert_eq!(plan.moves.len(), 1);
        assert_ne!(plan.moves[0].to, dest);
        assert!(!plan.moves[0].to.ends_with(".pkg"));
        let log = root.join("undo.json");
        let run = apply(&LocalTree, root, plan.moves, &log, "", None).unwrap();
        assert!(run.failed.is_empty());
        assert_eq!(
            std::fs::read(root.join(dest).join("keep.bin")).unwrap(),
            vec![7; 7]
        );
        assert!(!root.join(dest).join("sce_sys").exists());
    }

    #[test]
    fn configured_child_root_is_excluded_from_the_parent_plan() {
        let l = lib("overlap");
        put(&l.0, "outside.exfat", 2);
        put(&l.0.join("separate"), "owned.exfat", 2);
        let roots = vec![
            (l.0.clone(), "parent".into()),
            (l.0.join("separate"), "child".into()),
        ];
        let plan = plan_cached(&LocalTree, &l.0, None, &roots);
        assert_eq!(
            plan.skipped.len(),
            1,
            "only the parent's invalid image should be reported"
        );
        assert_eq!(plan.skipped[0].path, "outside.exfat");
    }

    #[test]
    fn dlc_is_filed_under_the_base_game_folder() {
        let l = lib("dlc");
        let c = l.0.join("fpkg");
        put(&c, "base.pkg", 10);
        put(&c, "dlc.pkg", 10);
        let t = [
            (
                "base",
                info(
                    "Bloodborne",
                    "CUSA00900",
                    "UP9000-CUSA00900_00-BLOODBORNE000000",
                    "base",
                    "1.00",
                ),
            ),
            (
                "dlc",
                info(
                    "Bloodborne The Old Hunters",
                    "CUSA00900",
                    "UP9000-CUSA00900_00-SPEXPANSIONDLC03",
                    "dlc",
                    "1.00",
                ),
            ),
        ];
        let p = plan_with(&LocalTree, &c, &table(&t));
        assert_eq!(p.moves.len(), 2);
        for m in &p.moves {
            assert!(m.to.starts_with("PS4/CUSA00900 - Bloodborne/"), "{}", m.to);
        }
        let dlc = p.moves.iter().find(|m| m.kind == "dlc").unwrap();
        assert_eq!(
            dlc.to,
            "PS4/CUSA00900 - Bloodborne/Bloodborne - CUSA00900 - v1.00 - US - DLC (SPEXPANSIONDLC03).pkg"
        );
    }

    #[test]
    fn a_dlc_only_group_recovers_the_game_name() {
        let l = lib("dlconly");
        let c = l.0.join("fpkg");
        put(&c, "a.pkg", 10);
        put(&c, "b.pkg", 10);
        let t = [
            (
                "a",
                info(
                    "Mafia The Old Country - Padrino Pack",
                    "PPSA14251",
                    "UP1001-PPSA14251_00-DONPACK000000000",
                    "dlc",
                    "1.00",
                ),
            ),
            (
                "b",
                info(
                    "Mafia The Old Country - Gatto Nero Pack",
                    "PPSA14251",
                    "UP1001-PPSA14251_00-GATTONEROPACK000",
                    "dlc",
                    "1.00",
                ),
            ),
        ];
        let p = plan_with(&LocalTree, &c, &table(&t));
        for m in &p.moves {
            assert!(
                m.to.starts_with("PS4/PPSA14251 - Mafia The Old Country/"),
                "{}",
                m.to
            );
        }
    }

    #[test]
    fn the_same_release_twice_is_filed_beside_it_and_marked() {
        let l = lib("dup");
        let c = l.0.join("fpkg");
        for n in ["one.pkg", "two.pkg", "three.pkg"] {
            put(&c, n, 10);
        }
        let i = info(
            "RDR2",
            "CUSA03041",
            "UP1004-CUSA03041_00-REDEMPTION000002",
            "patch",
            "1.32",
        );
        let t = [("one", i.clone()), ("two", i.clone()), ("three", i)];
        let p = plan_with(&LocalTree, &c, &table(&t));
        assert_eq!(p.moves.len(), 3);
        assert_eq!(p.moves.iter().filter(|m| !m.duplicate).count(), 1);
        let tos: HashSet<&str> = p.moves.iter().map(|m| m.to.as_str()).collect();
        assert_eq!(tos.len(), 3);
        assert!(p
            .moves
            .iter()
            .filter(|m| m.duplicate)
            .all(|m| m.to.contains("DUPLICATE") && m.duplicate_of.is_some()));
    }

    #[test]
    fn different_builds_sharing_a_name_take_distinct_names() {
        let l = lib("builds");
        let c = l.0.join("fpkg");
        put(&c, "one.pkg", 10);
        put(&c, "two.pkg", 74);
        let i = info(
            "RDR2",
            "CUSA03041",
            "UP1004-CUSA03041_00-REDEMPTION000002",
            "patch",
            "1.32",
        );
        let p = plan_with(&LocalTree, &c, &table(&[("one", i.clone()), ("two", i)]));
        assert_eq!(p.moves.len(), 2);
        assert!(p.moves.iter().all(|m| !m.duplicate));
        assert_ne!(p.moves[0].to, p.moves[1].to);
    }

    #[test]
    fn incomplete_unreadable_and_unidentified_packages_are_left_alone() {
        let l = lib("skip");
        let c = l.0.join("fpkg");
        put(&c, "partial.pkg", 10);
        put(&c, "broken.pkg", 10);
        put(&c, "noid.pkg", 10);
        let mut partial = info("X", "PPSA03016", "UP9000-PPSA03016_00-X", "base", "1.00");
        partial.complete = false;
        let broken = PkgInfo {
            error: Some("bad magic".into()),
            ..PkgInfo::default()
        };
        let noid = PkgInfo {
            complete: true,
            ..PkgInfo::default()
        };
        let p = plan_with(
            &LocalTree,
            &c,
            &table(&[("partial", partial), ("broken", broken), ("noid", noid)]),
        );
        assert!(p.moves.is_empty());
        let reasons: HashSet<&str> = p.skipped.iter().map(|s| s.reason.as_str()).collect();
        assert_eq!(
            reasons,
            HashSet::from([SKIP_INCOMPLETE, SKIP_UNREADABLE, SKIP_NO_ID])
        );
    }

    #[test]
    fn packages_loose_in_a_root_are_organized_too() {
        let l = lib("loose");
        let root = l.0.clone();
        put(&root, "game.pkg", 10);
        put(&root.join("fpkg"), "other.pkg", 10);
        let found = containers(&LocalTree, &root);
        assert!(found.contains(&root), "{found:?}");
        assert_eq!(found, vec![root.clone()]);
        // Both loose and nested packages belong to the configured root.
        assert_eq!(scan(&LocalTree, &root).len(), 2);
        let p = plan_files(
            &LocalTree,
            &root,
            scan(&LocalTree, &root),
            &table(&[(
                "game",
                info(
                    "Game",
                    "PPSA03016",
                    "UP9000-PPSA03016_00-GAME",
                    "base",
                    "1.00",
                ),
            )]),
        );
        assert_eq!(p.moves.len(), 1, "{:?}", p.moves);
        assert!(p.moves[0].from == "game.pkg", "{:?}", p.moves[0]);
    }

    #[test]
    fn an_uncertain_kind_is_marked_unsure_in_its_name() {
        let l = lib("unsure");
        let c = l.0.join("fpkg");
        put(&c, "u.pkg", 10);
        let mut i = info(
            "Game",
            "CUSA00001",
            "UP0000-CUSA00001_00-AAAAAAAAAAAAAAAA",
            "patch",
            "1.02",
        );
        i.kind_confident = false;
        let p = plan_with(&LocalTree, &c, &table(&[("u", i)]));
        assert!(
            p.moves[0].to.ends_with("PATCH_UNSURE.pkg"),
            "{}",
            p.moves[0].to
        );
        assert_eq!(p.unsure.len(), 1);
    }

    #[test]
    fn a_split_set_moves_together_and_keeps_its_numbering() {
        let l = lib("split");
        let c = l.0.join("fpkg");
        put(&c, "Big_0.pkg", 10);
        put(&c, "Big_2.pkg", 10);
        let i = info(
            "Big Game",
            "CUSA03041",
            "UP1004-CUSA03041_00-REDEMPTION000002",
            "base",
            "1.00",
        );
        let p = plan_with(&LocalTree, &c, &table(&[("Big", i)]));
        let names: Vec<&str> = p
            .moves
            .iter()
            .map(|m| m.to.rsplit('/').next().unwrap())
            .collect();
        assert_eq!(p.moves.len(), 2);
        assert!(
            names[0].ends_with("_0.pkg") && names[1].ends_with("_2.pkg"),
            "{names:?}"
        );
        assert_eq!(p.moves[0].part.as_deref(), Some("1 of 2"));
    }

    /// A file with a package's first bytes (it needs nothing else here: `table` names it).
    fn put_package(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let mut bytes = ps5upload_pkg::PKG_MAGIC.to_be_bytes().to_vec();
        bytes.resize(64, 0);
        std::fs::write(dir.join(name), bytes).unwrap();
    }

    #[test]
    fn packages_that_only_look_like_parts_are_planned_one_by_one() {
        let l = lib("split-not");
        let c = l.0.join("fpkg");
        // Two packages with a name that looks like parts, no part 0.
        put_package(&c, "Bloodborne_1.pkg");
        put_package(&c, "Bloodborne_2.pkg");
        // A part 0 and a package that happens to be numbered after it.
        put_package(&c, "Dup_0.pkg");
        put_package(&c, "Dup_1.pkg");
        let t = [
            (
                "Bloodborne_1",
                info(
                    "Bloodborne",
                    "CUSA00900",
                    "UP9000-CUSA00900_00-A",
                    "base",
                    "1.00",
                ),
            ),
            (
                "Bloodborne_2",
                info(
                    "Bloodborne",
                    "CUSA00900",
                    "UP9000-CUSA00900_00-A",
                    "patch",
                    "1.09",
                ),
            ),
            (
                "Dup_0",
                info("Dup", "CUSA00901", "UP9000-CUSA00901_00-A", "base", "1.00"),
            ),
            (
                "Dup_1",
                info("Dup", "CUSA00901", "UP9000-CUSA00901_00-A", "patch", "1.02"),
            ),
        ];
        let p = plan_with(&LocalTree, &c, &table(&t));
        assert_eq!(p.moves.len(), 4, "{:?}", p.moves);
        assert!(p.moves.iter().all(|m| m.part.is_none() && m.set.is_empty()));
        let to = |from: &str| &p.moves.iter().find(|m| m.from == from).unwrap().to;
        assert!(
            to("Bloodborne_1.pkg").ends_with("- BASE.pkg"),
            "{}",
            to("Bloodborne_1.pkg")
        );
        assert!(
            to("Bloodborne_2.pkg").ends_with("- PATCH.pkg"),
            "{}",
            to("Bloodborne_2.pkg")
        );
        assert!(
            to("Dup_1.pkg").ends_with("- PATCH.pkg"),
            "{}",
            to("Dup_1.pkg")
        );
    }

    #[test]
    fn a_set_with_numbered_suffixes_moves_together_in_its_own_layout() {
        let l = lib("split-suffix");
        let c = l.0.join("fpkg");
        put(&c, "Big.pkg", 10);
        put(&c, "Big.pkg.0", 10);
        put(&c, "Big.pkg.1", 10);
        let i = info(
            "Big Game",
            "CUSA03041",
            "UP1004-CUSA03041_00-X",
            "base",
            "1.00",
        );
        let p = plan_with(&LocalTree, &c, &table(&[("Big", i)]));
        assert_eq!(p.moves.len(), 3, "{:?}", p.moves);
        let names: Vec<&str> = p
            .moves
            .iter()
            .map(|m| m.to.rsplit('/').next().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "Big Game - CUSA03041 - v1.00 - US - BASE.pkg",
                "Big Game - CUSA03041 - v1.00 - US - BASE.pkg.0",
                "Big Game - CUSA03041 - v1.00 - US - BASE.pkg.1",
            ]
        );
        assert!(p.moves.iter().all(|m| m.set == "Big.pkg"));
    }

    fn big_set(c: &Path) -> Plan {
        put(c, "Big_0.pkg", 10);
        put(c, "Big_1.pkg", 10);
        put(c, "Big_2.pkg", 10);
        let i = info(
            "Big Game",
            "CUSA03041",
            "UP1004-CUSA03041_00-X",
            "base",
            "1.00",
        );
        plan_with(&LocalTree, c, &table(&[("Big", i)]))
    }

    #[test]
    fn a_set_whose_name_is_taken_since_the_preview_does_not_start() {
        let l = lib("set-precheck");
        let c = l.0.join("fpkg");
        let p = big_set(&c);
        assert_eq!(p.moves.len(), 3);
        // The last part's name appears after the preview.
        let last = c.join(&p.moves[2].to);
        put(
            last.parent().unwrap(),
            last.file_name().unwrap().to_str().unwrap(),
            3,
        );
        let log = apply(
            &LocalTree,
            &c,
            p.moves.clone(),
            &l.0.join("log.json"),
            "",
            None,
        )
        .unwrap();
        assert!(log.completed.is_empty(), "{:?}", log.completed);
        assert_eq!(log.failed.len(), 3);
        for part in ["Big_0.pkg", "Big_1.pkg", "Big_2.pkg"] {
            assert!(c.join(part).is_file(), "{part} stayed");
        }
    }

    /// A tree whose renames to a name ending in `fail_on` fail, with nothing done; with
    /// `no_way_back`, so do renames back to the set's first part's old name.
    struct FailOn {
        fail_on: &'static str,
        no_way_back: bool,
    }

    impl Tree for FailOn {
        fn list(&self, dir: &Path) -> Option<Vec<super::super::organize_fs::DirEnt>> {
            LocalTree.list(dir)
        }
        fn meta(&self, p: &Path) -> Option<super::super::organize_fs::Meta> {
            LocalTree.meta(p)
        }
        fn exists(&self, p: &Path) -> Result<bool, OpError> {
            LocalTree.exists(p)
        }
        fn create_dir(&self, p: &Path) -> Result<(), OpError> {
            LocalTree.create_dir(p)
        }
        fn rename_new(&self, from: &Path, to: &Path) -> Result<(), OpError> {
            if to.to_string_lossy().ends_with(self.fail_on)
                || (self.no_way_back && to.to_string_lossy().ends_with("Big_0.pkg"))
            {
                return Err(OpError::Other("no".into()));
            }
            LocalTree.rename_new(from, to)
        }
        fn remove_dir(&self, p: &Path) -> Result<(), OpError> {
            LocalTree.remove_dir(p)
        }
        fn is_removable_junk(&self, p: &Path) -> bool {
            LocalTree.is_removable_junk(p)
        }
        fn remove_junk(&self, p: &Path) {
            LocalTree.remove_junk(p);
        }
    }

    #[test]
    fn a_part_that_cannot_move_brings_the_set_back_together() {
        let l = lib("set-rollback");
        let c = l.0.join("fpkg");
        let p = big_set(&c);
        let tree = FailOn {
            fail_on: "_1.pkg",
            no_way_back: false,
        };
        let log = apply(&tree, &c, p.moves.clone(), &l.0.join("log.json"), "", None).unwrap();
        assert!(log.completed.is_empty(), "{:?}", log.completed);
        assert_eq!(log.failed.len(), 3, "{:?}", log.failed);
        for part in ["Big_0.pkg", "Big_1.pkg", "Big_2.pkg"] {
            assert!(c.join(part).is_file(), "{part} is back");
        }
        assert!(!c.join("PS4").exists(), "the empty folders went too");
        // Nothing to undo.
        let undone = revert(&l.0.join("log.json"), &local).unwrap();
        assert!(undone.reverted.is_empty(), "{:?}", undone.reverted);
    }

    #[test]
    fn a_part_that_cannot_go_back_stays_on_the_log_for_undo() {
        let l = lib("set-rollback-stuck");
        let c = l.0.join("fpkg");
        let p = big_set(&c);
        let tree = FailOn {
            fail_on: "_1.pkg",
            no_way_back: true,
        };
        let log_path = l.0.join("log.json");
        let log = apply(&tree, &c, p.moves.clone(), &log_path, "", None).unwrap();
        assert_eq!(
            log.completed.len(),
            1,
            "part 0 went and could not come back"
        );
        assert_eq!(log.completed[0].from, "Big_0.pkg");
        assert_eq!(log.failed.len(), 2);
        assert!(!c.join("Big_0.pkg").exists());
        // Undo, once it can, finishes the job.
        let undone = revert(&log_path, &local).unwrap();
        assert_eq!(undone.reverted.len(), 1);
        assert!(c.join("Big_0.pkg").is_file());
        assert!(c.join("Big_1.pkg").is_file());
    }

    #[test]
    fn untouched_moves_beside_a_failed_set_still_run() {
        let l = lib("set-beside");
        let c = l.0.join("fpkg");
        put(&c, "Solo.pkg", 10);
        put(&c, "Big_0.pkg", 10);
        put(&c, "Big_1.pkg", 10);
        let t = [
            (
                "Big",
                info("Big", "CUSA03041", "UP1004-CUSA03041_00-X", "base", "1.00"),
            ),
            (
                "Solo",
                info("Solo", "CUSA03042", "UP1004-CUSA03042_00-X", "base", "1.00"),
            ),
        ];
        let p = plan_with(&LocalTree, &c, &table(&t));
        assert_eq!(p.moves.len(), 3);
        let tree = FailOn {
            fail_on: "_1.pkg",
            no_way_back: false,
        };
        let log = apply(&tree, &c, p.moves, &l.0.join("log.json"), "", None).unwrap();
        assert_eq!(log.completed.len(), 1);
        assert_eq!(log.completed[0].from, "Solo.pkg");
        assert_eq!(log.failed.len(), 2);
    }

    #[test]
    fn a_stopped_plan_reads_no_further() {
        let l = lib("watch");
        let c = l.0.join("fpkg");
        for n in ["a.pkg", "b.pkg", "c.pkg"] {
            put(&c, n, 10);
        }
        let watch = PlanWatch::default();
        let seen = std::cell::Cell::new(0);
        let facts = |p: &Path| {
            seen.set(seen.get() + 1);
            watch.cancel.store(true, Ordering::Relaxed);
            Facts {
                path: p.to_path_buf(),
                parts: vec![p.to_path_buf()],
                size: 1,
                info: info("A", "CUSA00001", "UP0000-CUSA00001_00-A", "base", "1.00"),
            }
        };
        let (files, _) = discover_watched(&LocalTree, &c, &[], &watch);
        assert_eq!(files.len(), 3);
        plan_files_watched(&LocalTree, &c, files, &facts, &watch);
        assert_eq!(seen.get(), 1, "the first read asked it to stop");
        // A plan told to stop before it starts lists nothing either.
        let (none, _) = discover_watched(&LocalTree, &c, &[], &watch);
        assert!(none.is_empty());
        assert_eq!(watch.total.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn a_plan_lists_each_folder_once() {
        use std::sync::atomic::AtomicUsize;
        struct Counting(AtomicUsize);
        impl Tree for Counting {
            fn list(&self, dir: &Path) -> Option<Vec<super::super::organize_fs::DirEnt>> {
                self.0.fetch_add(1, Ordering::Relaxed);
                LocalTree.list(dir)
            }
            fn meta(&self, p: &Path) -> Option<super::super::organize_fs::Meta> {
                LocalTree.meta(p)
            }
            fn exists(&self, p: &Path) -> Result<bool, OpError> {
                LocalTree.exists(p)
            }
            fn create_dir(&self, p: &Path) -> Result<(), OpError> {
                LocalTree.create_dir(p)
            }
            fn rename_new(&self, from: &Path, to: &Path) -> Result<(), OpError> {
                LocalTree.rename_new(from, to)
            }
            fn remove_dir(&self, p: &Path) -> Result<(), OpError> {
                LocalTree.remove_dir(p)
            }
            fn is_removable_junk(&self, p: &Path) -> bool {
                LocalTree.is_removable_junk(p)
            }
            fn remove_junk(&self, p: &Path) {
                LocalTree.remove_junk(p);
            }
        }
        let l = lib("list-once");
        let c = l.0.join("fpkg");
        for i in 0..20 {
            put(&c, &format!("g{i}.pkg"), 10);
        }
        let tree = Counting(AtomicUsize::new(0));
        let roots = vec![(c.clone(), "c".to_string())];
        plan_cached(&tree, &c, None, &roots);
        assert_eq!(tree.0.load(Ordering::Relaxed), 1, "one folder, one listing");
    }

    #[test]
    fn an_undo_log_is_whole_on_disk_or_not_there() {
        let l = lib("log");
        let path = l.0.join("logs/run.json");
        let log = UndoLog {
            version: 1,
            container_root: "/x".into(),
            server: String::new(),
            origin: None,
            created_at: "t".into(),
            moves: Vec::new(),
            completed: Vec::new(),
            failed: Vec::new(),
            removed_dirs: Vec::new(),
            kept_dirs: Vec::new(),
            reverted_at: None,
        };
        write_log(&path, &log).unwrap();
        let back: UndoLog = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back.container_root, "/x");
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn apply_then_replan_is_a_no_op_and_undo_restores_everything() {
        let l = lib("apply");
        let c = l.0.join("fpkg");
        put(&c.join("loose/inner"), "x.pkg", 10);
        std::fs::write(c.join("loose/inner/._x.pkg"), [0x00, 0x05, 0x16, 0x07]).unwrap();
        std::fs::write(c.join("loose/.DS_Store"), b"x").unwrap();
        put(&c, "Note.txt", 3);
        let rdr2 = info("RDR2", "CUSA03041", "UP1004-CUSA03041_00-X", "base", "1.00");
        // Found by its old name before the move and its new one after.
        let t = [("x", rdr2.clone()), ("RDR2", rdr2)];
        let first = plan_with(&LocalTree, &c, &table(&t));
        assert_eq!(first.moves.len(), 1);
        let log_path = l.0.join("logs/run.json");
        let log = apply(&LocalTree, &c, first.moves.clone(), &log_path, "", None).unwrap();
        assert_eq!(log.completed.len(), 1);
        assert!(c.join(&first.moves[0].to).is_file());
        // The emptied folders (only Finder's files left) are gone; other files stay.
        assert!(!c.join("loose").exists(), "{:?}", log.kept_dirs);
        assert!(c.join("Note.txt").is_file());

        let again = plan_with(&LocalTree, &c, &table(&t));
        assert!(again.moves.is_empty());
        assert_eq!(again.skipped[0].reason, SKIP_IN_PLACE);

        let undone = revert(&log_path, &local).unwrap();
        assert_eq!(undone.reverted.len(), 1);
        assert!(c.join("loose/inner/x.pkg").is_file());
        assert!(!c.join("PS4").exists());
        assert!(revert(&log_path, &local).is_err(), "a run is undone once");
    }

    #[test]
    fn moves_that_leave_the_folder_are_refused_both_ways() {
        let l = lib("escape");
        let c = l.0.join("fpkg");
        put(&c, "x.pkg", 10);
        let mv = |from: &str, to: &str| -> Move {
            serde_json::from_value(serde_json::json!({ "from": from, "to": to })).unwrap()
        };
        let log = apply(
            &LocalTree,
            &c,
            vec![mv("x.pkg", "../out.pkg"), mv("/etc/hosts", "y.pkg")],
            &l.0.join("log.json"),
            "",
            None,
        )
        .unwrap();
        assert_eq!(log.failed.len(), 2);
        assert!(c.join("x.pkg").is_file() && !l.0.join("out.pkg").exists());
        assert_eq!(content_suffix("UP1001-PPSA14251_00-../../x"), "");
        assert_eq!(
            content_suffix("UP1001-PPSA14251_00-DONPACK0001"),
            "DONPACK0001"
        );
    }

    #[test]
    fn a_run_that_died_part_way_is_still_undone() {
        let l = lib("crash");
        let c = l.0.join("fpkg");
        put(&c, "x.pkg", 10);
        let t = [(
            "x",
            info("RDR2", "CUSA03041", "UP1004-CUSA03041_00-X", "base", "1.00"),
        )];
        let p = plan_with(&LocalTree, &c, &table(&t));
        let log_path = l.0.join("log.json");
        let log = apply(&LocalTree, &c, p.moves.clone(), &log_path, "", None).unwrap();
        assert_eq!(log.completed.len(), 1);
        // As if the engine died before the final write: the log knows only the plan.
        let mut stale = log.clone();
        stale.completed.clear();
        write_log(&log_path, &stale).unwrap();
        let undone = revert(&log_path, &local).unwrap();
        assert_eq!(undone.reverted.len(), 1);
        assert!(c.join("x.pkg").is_file());
    }

    #[test]
    fn a_destination_that_appears_is_never_overwritten() {
        let l = lib("race");
        let c = l.0.join("fpkg");
        put(&c, "x.pkg", 10);
        let t = [(
            "x",
            info("RDR2", "CUSA03041", "UP1004-CUSA03041_00-X", "base", "1.00"),
        )];
        let p = plan_with(&LocalTree, &c, &table(&t));
        put(
            &c.join(Path::new(&p.moves[0].to).parent().unwrap()),
            Path::new(&p.moves[0].to)
                .file_name()
                .unwrap()
                .to_str()
                .unwrap(),
            99,
        );
        let log = apply(&LocalTree, &c, p.moves, &l.0.join("log.json"), "", None).unwrap();
        assert_eq!(log.failed.len(), 1);
        assert!(c.join("x.pkg").is_file());
    }

    #[test]
    fn a_decomposed_name_on_disk_counts_as_in_place() {
        let l = lib("nfd");
        let c = l.0.join("fpkg");
        let title = "Ghost of Y\u{014d}tei";
        let i = info(
            title,
            "PPSA00001",
            "UP0000-PPSA00001_00-AAAAAAAAAAAAAAAA",
            "base",
            "1.00",
        );
        let p = plan_with(&LocalTree, &c, &table(&[]));
        assert!(p.moves.is_empty());
        let name = format!("{} - PPSA00001 - v1.00 - US - BASE.pkg", title)
            .nfd()
            .collect::<String>();
        let folder = format!("PPSA00001 - {title}").nfd().collect::<String>();
        put(&c.join("PS4").join(&folder), &name, 10);
        let p = plan_with(&LocalTree, &c, &table(&[("Ghost", i)]));
        assert!(
            p.moves.is_empty(),
            "{:?}",
            p.moves.iter().map(|m| &m.to).collect::<Vec<_>>()
        );
    }

    #[test]
    fn names_are_safe_on_every_os() {
        assert_eq!(sanitize("  A:B/C?  . "), "A B C");
        assert_eq!(title_folder("CUSA1", ""), "CUSA1");
        assert_eq!(title_folder("CUSA1", "CUSA1"), "CUSA1");
        assert_eq!(sanitize(&"x".repeat(130)).chars().count(), 120);
    }
}
