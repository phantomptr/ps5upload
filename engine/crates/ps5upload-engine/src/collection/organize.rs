//! A tidy layout for the packages in a collection folder: PS Game Library's `pkgorganize.py`,
//! ported rule for rule.
//!
//! Every package is renamed `Title - TitleID - vVersion - Region - KIND.pkg` and filed under
//! `<folder>/<Platform>/<TitleID - Title>/`, where `<folder>` is the top-level folder of the
//! collection root it already sits in (a package never moves between them). Planning is pure:
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

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use super::junk;
use super::organize_fs::{path_string, OpError, Tree};
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

/// Deeper than any library arrangement; a bound for a walk over a server.
const MAX_DEPTH: usize = 12;

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

/// `name_N.pkg`: the stem and the part number.
fn split_part(name: &str) -> Option<(&str, u32)> {
    let lower = name.to_ascii_lowercase();
    if !lower.ends_with(".pkg") {
        return None;
    }
    let stem = &name[..name.len() - 4];
    let (head, num) = stem.rsplit_once('_')?;
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((head, num.parse().ok()?))
}

/// Every part of a split set (`name_0.pkg`, `name_1.pkg`, …) in order, or just `[path]`.
pub fn split_siblings(tree: &dyn Tree, path: &Path) -> Vec<PathBuf> {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return vec![path.to_path_buf()];
    };
    let Some((stem, _)) = split_part(name) else {
        return vec![path.to_path_buf()];
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut sibs: Vec<(u32, PathBuf)> = tree
        .list(dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.is_file)
        .filter_map(|e| {
            let (s, k) = split_part(&e.name)?;
            (s == stem && !junk::is_junk_name(&e.name)).then(|| (k, dir.join(&e.name)))
        })
        .collect();
    sibs.sort_by_key(|(k, _)| *k);
    if sibs.len() > 1 && sibs.iter().any(|(_, p)| p == path) {
        sibs.into_iter().map(|(_, p)| p).collect()
    } else {
        vec![path.to_path_buf()]
    }
}

/// Every package below a folder, Finder's sidecars and dot-folders ignored.
pub fn scan(tree: &dyn Tree, container: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![(container.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Some(mut entries) = tree.list(&dir) else {
            continue;
        };
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        for e in entries {
            if e.is_dir {
                if !e.name.starts_with('.') && depth < MAX_DEPTH {
                    stack.push((dir.join(&e.name), depth + 1));
                }
            } else if e.is_file
                && !junk::is_junk_name(&e.name)
                && e.name.to_ascii_lowercase().ends_with(".pkg")
            {
                found.push(dir.join(&e.name));
            }
        }
    }
    found.sort();
    found
}

/// The packages lying loose in a folder itself, not in its subfolders. A collection root is
/// organized this way: its subfolders are containers of their own (planned separately), so a
/// root plans only the packages at its top level.
pub fn scan_top(tree: &dyn Tree, dir: &Path) -> Vec<PathBuf> {
    let Some(entries) = tree.list(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .into_iter()
        .filter(|e| {
            e.is_file
                && !e.name.starts_with('.')
                && !junk::is_junk_name(&e.name)
                && e.name.to_ascii_lowercase().ends_with(".pkg")
        })
        .map(|e| dir.join(&e.name))
        .collect();
    found.sort();
    found
}

/// The top-level folders of a collection root that hold packages, whatever they are called,
/// and the root itself when packages lie loose in it.
pub fn containers(tree: &dyn Tree, root: &Path) -> Vec<PathBuf> {
    let Some(entries) = tree.list(root) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .into_iter()
        .filter(|e| !e.name.starts_with('.') && e.is_dir)
        .map(|e| root.join(&e.name))
        .filter(|p| !scan(tree, p).is_empty())
        .collect();
    if !scan_top(tree, root).is_empty() {
        out.push(root.to_path_buf());
    }
    out.sort();
    out
}

/// Reads a package's facts, taking them from `cached(path, size, mtime)` when it knows the
/// file as it is now (the Collection's index), and from the file otherwise.
/// On a server the index's facts save reading each package's header over the network.
pub fn read_facts_with(
    tree: &dyn Tree,
    path: &Path,
    cached: &dyn Fn(&Path, u64, i64) -> Option<PkgInfo>,
) -> Facts {
    let parts = split_siblings(tree, path);
    let size = parts
        .iter()
        .map(|p| tree.meta(p).map(|m| m.size).unwrap_or(0))
        .sum();
    let mtime = tree.meta(&parts[0]).map(|m| m.mtime).unwrap_or(0);
    let info = (parts.len() == 1)
        .then(|| cached(&parts[0], size, mtime))
        .flatten()
        .or_else(|| super::identify::identify(&parts[0], "pkg"))
        .unwrap_or_default();
    Facts {
        path: path.to_path_buf(),
        parts,
        size,
        info,
    }
}

/// Every file name in the folder that a move must not land on: everything that stays.
fn taken_names(tree: &dyn Tree, container: &Path, moving: &HashSet<String>) -> HashSet<String> {
    let mut taken = HashSet::new();
    let mut stack = vec![(container.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        for e in tree.list(&dir).unwrap_or_default() {
            let path = dir.join(&e.name);
            if e.is_dir {
                // A folder's name is taken too: no file may land on it.
                taken.insert(key(&path));
                if !e.name.starts_with('.') && depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
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
    part_numbers: Option<&[u32]>,
    current: Option<&[String]>,
) -> Vec<String> {
    let slots = |stem: &str| -> Vec<String> {
        match part_numbers {
            None => vec![format!("{stem}.pkg")],
            Some(ns) => ns.iter().map(|n| format!("{stem}_{n}.pkg")).collect(),
        }
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
pub fn plan_files(
    tree: &dyn Tree,
    container: &Path,
    files: Vec<PathBuf>,
    facts: &dyn Fn(&Path) -> Facts,
) -> Plan {
    let cname = path_string(container);
    let mut out = Plan::default();
    let mut parsed: Vec<Facts> = Vec::new();
    for path in files {
        // Only part 0 of a split set carries a header: plan the set from it.
        let parts = split_siblings(tree, &path);
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
        } else if f.info.title_id.is_empty() {
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
    let mut taken = taken_names(tree, container, &moving);

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
        let mut seen: HashMap<(String, String, String, u64), String> = HashMap::new();
        for f in ordered {
            let identity = (
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
            let numbers: Option<Vec<u32>> = (f.parts.len() > 1).then(|| {
                f.parts
                    .iter()
                    .filter_map(|p| split_part(&p.file_name()?.to_string_lossy()).map(|(_, n)| n))
                    .collect()
            });
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
                numbers.as_deref(),
                here.as_deref(),
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
                    size: tree.meta(part).map(|m| m.size).unwrap_or(0),
                    content_id: f.info.content_id.clone(),
                    part: (names.len() > 1).then(|| format!("{} of {}", i + 1, names.len())),
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
pub fn plan_cached(
    tree: &dyn Tree,
    container: &Path,
    lib: Option<&super::Library>,
    roots: &[(PathBuf, String)],
) -> Plan {
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
    // A root is planned for its loose packages only: its subfolders are containers of their own.
    let files = if roots.iter().any(|(r, _)| r == container) {
        scan_top(tree, container)
    } else {
        scan(tree, container)
    };
    let mut plan = plan_files(tree, container, files, &|p: &Path| {
        read_facts_with(tree, p, &cached)
    });
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
    std::fs::write(&tmp, serde_json::to_vec_pretty(log).unwrap_or_default())?;
    std::fs::rename(&tmp, path)
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

/// Runs `moves` (all in `container`), the log written before the first one. A dropped
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
    for mv in moves {
        if let Some(why) = &lost {
            log.failed.push(FailedMove::new(
                mv,
                format!("not tried: the server stopped answering ({why})"),
            ));
            continue;
        }
        if !contained(&mv.from) || !contained(&mv.to) {
            log.failed
                .push(FailedMove::new(mv, "path leaves the folder"));
            continue;
        }
        let src = container.join(&mv.from);
        let dst = container.join(&mv.to);
        let res = match tree.exists(&dst) {
            Ok(true) => Err(OpError::Exists("destination appeared".into())),
            Ok(false) => dst
                .parent()
                .map_or(Ok(()), |d| make_dirs(tree, container, d, &mut made))
                .and_then(|()| tree.rename_new(&src, &dst)),
            Err(e) => Err(e),
        };
        match res {
            Ok(()) => {}
            Err(OpError::Lost(e)) => {
                lost = Some(e.clone());
                log.failed.push(FailedMove {
                    mv,
                    error: format!("the server stopped answering during this move ({e})"),
                    uncertain: true,
                });
                let _ = write_log(log_path, &log);
                continue;
            }
            Err(OpError::Exists(_)) => {
                log.failed.push(FailedMove::new(mv, "destination appeared"));
                continue;
            }
            Err(OpError::Other(e)) => {
                log.failed.push(FailedMove::new(mv, e));
                continue;
            }
        }
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
    log.reverted_at = Some(now_iso());
    write_log(log_path, &log)?;
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
        assert_eq!(found.len(), 6, "one package, four images and one intact folder: {found:?}");
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
        let metadata = info("Example", "PPSA01234", "UP0000-PPSA01234_00-EXAMPLE", "base", "1.00");
        let facts = |p: &Path| Facts {
            path: p.to_path_buf(), parts: vec![p.to_path_buf()], size: 12,
            info: PkgInfo { platform: "PS5".into(), ..metadata.clone() },
        };
        let sources = ["image.exfat", "image.ffpkg", "image.ffpfs", "image.ffpfsc", "dump"]
            .iter().map(|p| root.join(p)).collect();
        let plan = plan_files(&LocalTree, root, sources, &facts);
        assert_eq!(plan.moves.len(), 5);
        for ext in ["exfat", "ffpkg", "ffpfs", "ffpfsc"] {
            let mv = plan.moves.iter().find(|m| m.from == format!("image.{ext}")).unwrap();
            assert!(mv.to.ends_with(&format!(".{ext}")), "{mv:?}");
            assert!(!mv.duplicate, "different representations are not identical files");
        }
        let folder = plan.moves.iter().find(|m| m.from == "dump").unwrap().to.clone();
        let log = root.join("undo.json");
        let run = apply(&LocalTree, root, plan.moves, &log, "", None).unwrap();
        assert!(run.failed.is_empty(), "{:?}", run.failed);
        assert_eq!(std::fs::read(root.join(&folder).join("Media/asset.bin")).unwrap(), vec![7; 23]);
        let again = plan_with(&LocalTree, root, &facts);
        assert!(again.moves.is_empty(), "{:?}", again.moves);
        assert_eq!(again.skipped.iter().filter(|s| s.reason == SKIP_IN_PLACE).count(), 5);
        let undone = revert(&log, &local).unwrap();
        assert!(undone.problems.is_empty(), "{:?}", undone.problems);
        assert_eq!(undone.reverted.len(), 5);
        assert_eq!(std::fs::read(root.join("dump/Media/asset.bin")).unwrap(), vec![7; 23]);
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
        assert!(found.contains(&root.join("fpkg")), "{found:?}");
        // The root plans only its loose package; the subfolder is a container of its own.
        assert_eq!(scan_top(&LocalTree, &root), vec![root.join("game.pkg")]);
        let p = plan_files(
            &LocalTree,
            &root,
            scan_top(&LocalTree, &root),
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
