//! Cheat engine proxy: list titles, list mods, toggle, delete, reload,
//! status, and enable/disable the engine master flag.
//!
//! Also includes a community cheat download system that fetches cheat
//! files from GitHub repositories (etaHEN/PS5_Cheats, GoldHEN, etc.)
//! and installs them to the PS5 filesystem.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::mgmt::{self, m, Method};

use std::io::Read;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatTitle {
    #[serde(default)]
    pub title_id: String,
    #[serde(default)]
    pub name: String,
    /// Game version the on-console cheat targets, parsed from its filename
    /// by the payload. Without this field serde dropped it and the client
    /// never saw it.
    #[serde(default)]
    pub version: String,
    /// Formats present on the console for this title (`json`, `shn`, `mc4`).
    #[serde(default)]
    pub formats: Vec<String>,
    /// How many of its cheats are switched on (saved state).
    #[serde(default)]
    pub enabled: i32,
    #[serde(default)]
    pub running: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatsListResponse {
    #[serde(default)]
    pub titles: Vec<CheatTitle>,
    #[serde(default)]
    pub game_running: bool,
    #[serde(default)]
    pub game_title_id: String,
    /// The console stopped adding titles at its reply ceiling: the list is valid but partial.
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatMod {
    #[serde(default)]
    pub index: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub desc: String,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub on: bool,
    /// "xml" for an XML game patch entry; absent for the other formats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// An XML game patch entry made for another game version than the installed one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub other_version: bool,
    /// What happened to this switched-on cheat in the running game (#627): "applied",
    /// "waiting" (the game is not ready yet), "failed" or "changed" (the game changed it, so it
    /// was left alone). Absent when the game is not running or the cheat is off, and from helpers
    /// before 10.7.2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied: Option<String>,
    /// Why, for "waiting", "failed" and "changed".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatsGetResponse {
    #[serde(default)]
    pub mods: Vec<CheatMod>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatsToggleRequest {
    pub title_id: String,
    pub index: i32,
    pub on: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatsToggleResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub err: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatsStatusResponse {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub patches_last: i64,
    #[serde(default)]
    pub patches_total: i64,
    #[serde(default)]
    pub game_running: bool,
    #[serde(default)]
    pub game_title_id: String,
    #[serde(default)]
    pub game_pid: i32,
    /// The helper paused the running game's cheats: it closed right after they were applied
    /// (#550). Helpers before 9.5.0 never send it.
    #[serde(default)]
    pub held: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatsEngineSetRequest {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatsEngineSetResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub enabled: bool,
}

fn send_recv(addr: &str, method: Method, label: &str, body: Option<&[u8]>) -> Result<Vec<u8>> {
    // The handler's `{"ok":false,...}` bodies carry data the callers read; call_keep gives them back
    // as before, and leaves a plain refusal an error ("payload rejected <label>: <cause>").
    mgmt::call_keep(addr, method, label, body.unwrap_or(&[]))
}

pub fn cheats_list(addr: &str) -> Result<CheatsListResponse> {
    let resp = send_recv(addr, m::CHEATS_LIST, "CheatsList", None)?;
    Ok(serde_json::from_slice(&resp)?)
}

pub fn cheats_get(addr: &str, title_id: &str) -> Result<CheatsGetResponse> {
    let body = serde_json::json!({ "title_id": title_id });
    let resp = send_recv(
        addr,
        m::CHEATS_GET,
        "CheatsGet",
        Some(&serde_json::to_vec(&body)?),
    )?;
    Ok(serde_json::from_slice(&resp)?)
}

pub fn cheats_toggle(
    addr: &str,
    title_id: &str,
    index: i32,
    on: bool,
) -> Result<CheatsToggleResponse> {
    let req = CheatsToggleRequest {
        title_id: title_id.to_string(),
        index,
        on,
    };
    let resp = send_recv(
        addr,
        m::CHEATS_TOGGLE,
        "CheatsToggle",
        Some(&serde_json::to_vec(&req)?),
    )?;
    Ok(serde_json::from_slice(&resp)?)
}

pub fn cheats_delete(addr: &str, title_id: &str) -> Result<bool> {
    let body = serde_json::json!({ "title_id": title_id });
    let resp = send_recv(
        addr,
        m::CHEATS_DELETE,
        "CheatsDelete",
        Some(&serde_json::to_vec(&body)?),
    )?;
    let v: serde_json::Value = serde_json::from_slice(&resp)?;
    Ok(v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false))
}

pub fn cheats_reload(addr: &str) -> Result<bool> {
    let resp = send_recv(addr, m::CHEATS_RELOAD, "CheatsReload", None)?;
    let v: serde_json::Value = serde_json::from_slice(&resp)?;
    Ok(v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false))
}

pub fn cheats_status(addr: &str) -> Result<CheatsStatusResponse> {
    let resp = send_recv(addr, m::CHEATS_STATUS, "CheatsStatus", None)?;
    Ok(serde_json::from_slice(&resp)?)
}

pub fn cheats_engine_set(addr: &str, enabled: bool) -> Result<CheatsEngineSetResponse> {
    let req = CheatsEngineSetRequest { enabled };
    let resp = send_recv(
        addr,
        m::CHEATS_ENGINE_SET,
        "CheatsEngineSet",
        Some(&serde_json::to_vec(&req)?),
    )?;
    Ok(serde_json::from_slice(&resp)?)
}

// ─── Community cheat download system ─────────────────────────────────
//
// Fetches cheat files from well-known GitHub repositories and installs
// them onto the PS5 filesystem under /data/cheats/<title_id>/.
//
// HTTP goes through `ureq` on every target, Android included.

/// Description of a community GitHub cheat repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatRepo {
    pub id: String,
    pub name: String,
    /// Base URL for raw file access, e.g.
    /// `https://raw.githubusercontent.com/etaHEN/PS5_Cheats/main/`
    pub raw_base: String,
    /// Index files in the repo root (e.g. `json.txt`, `shn.txt`).
    /// Empty when the repo publishes no index and must be enumerated
    /// through `tree_api` instead.
    pub index_files: Vec<String>,
    /// Base URL holding the `json/`, `shn/` and `mc4/` subdirectories.
    /// Usually equal to `raw_base`, but repos that nest their cheats
    /// (e.g. under `cheats/`) point this deeper.
    #[serde(default)]
    pub content_base: String,
    /// GitHub git-trees API URL used to enumerate repos that ship no
    /// index files. Empty means "use `index_files`".
    #[serde(default)]
    pub tree_api: String,
    /// Path prefix inside the tree that contains the format
    /// subdirectories, e.g. `cheats/`. Empty means the tree root.
    #[serde(default)]
    pub tree_prefix: String,
}

/// One entry parsed from a repo index file (`filename=game_title`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatRepoEntry {
    pub filename: String,
    pub game_title: String,
    /// Cheat format: `json`, `shn`, or `mc4`.
    pub format: String,
    /// Repo ID this entry came from.
    pub repo_id: String,
    /// Title id parsed out of the filename (`CUSA09193_01.05_2.json` →
    /// `CUSA09193`). Empty when the name does not follow the convention.
    #[serde(default)]
    pub title_id: String,
    /// Game version the cheat was authored against (`01.05`), parsed from the
    /// same filename. Empty when absent. Cheats are version-specific, so this
    /// is the difference between "a cheat for this game" and "a cheat that
    /// will work".
    #[serde(default)]
    pub game_version: String,
}

/// Split a repo filename into `(title_id, game_version)`.
///
/// Every published index uses `<TITLE_ID>_<VERSION>[_<variant>].<ext>`, e.g.
/// `CUSA15438_01.00.json`, `CUSA09193_01.05_2.json` (a second cheat for the
/// same build) and `CUSA34394_05.01.json`. Returning empty strings rather than
/// an Option keeps the caller honest: a name that does not follow the
/// convention still lists, it just cannot be filtered.
pub fn parse_cheat_filename(filename: &str) -> (String, String) {
    let stem = filename.rsplit_once('.').map_or(filename, |(head, _)| head);
    let mut parts = stem.split('_');
    let Some(title) = parts.next() else {
        return (String::new(), String::new());
    };
    // A title id is four letters then five digits (CUSA12345 / PPSA12345).
    let looks_like_title = title.len() == 9
        && title[..4].chars().all(|c| c.is_ascii_alphabetic())
        && title[4..].chars().all(|c| c.is_ascii_digit());
    if !looks_like_title {
        return (String::new(), String::new());
    }
    // The version is the next segment when it looks like one; the `_2` that
    // sometimes follows is a variant counter, not a version.
    let version = parts
        .next()
        .filter(|v| {
            !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.') && v.contains('.')
        })
        .unwrap_or("")
        .to_string();
    (title.to_ascii_uppercase(), version)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CheatRepoSearchResponse {
    #[serde(default)]
    pub entries: Vec<CheatRepoEntry>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CheatDownloadResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Return the hard-coded list of known community cheat repositories.
pub fn cheat_repos() -> Vec<CheatRepo> {
    vec![
        CheatRepo {
            id: "etahen".into(),
            name: "etaHEN/PS5_Cheats".into(),
            raw_base: "https://raw.githubusercontent.com/etaHEN/PS5_Cheats/main/".into(),
            index_files: vec!["json.txt".into(), "shn.txt".into(), "mc4.txt".into()],
            content_base: "https://raw.githubusercontent.com/etaHEN/PS5_Cheats/main/".into(),
            tree_api: String::new(),
            tree_prefix: String::new(),
        },
        CheatRepo {
            id: "goldhen".into(),
            name: "GoldHEN/GoldHEN_Cheat_Repository".into(),
            raw_base: "https://raw.githubusercontent.com/GoldHEN/GoldHEN_Cheat_Repository/main/"
                .into(),
            index_files: vec!["json.txt".into(), "shn.txt".into(), "mc4.txt".into()],
            content_base:
                "https://raw.githubusercontent.com/GoldHEN/GoldHEN_Cheat_Repository/main/".into(),
            tree_api: String::new(),
            tree_prefix: String::new(),
        },
        // This repo publishes no `*.txt` index and lives on `master`
        // with its cheats nested under `cheats/`, so it is enumerated
        // through the git-trees API instead.
        CheatRepo {
            id: "henmix".into(),
            name: "TeeKay87/HEN-Cheats-Collection".into(),
            raw_base: "https://raw.githubusercontent.com/TeeKay87/HEN-Cheats-Collection/master/"
                .into(),
            index_files: Vec::new(),
            content_base:
                "https://raw.githubusercontent.com/TeeKay87/HEN-Cheats-Collection/master/cheats/"
                    .into(),
            tree_api:
                "https://api.github.com/repos/TeeKay87/HEN-Cheats-Collection/git/trees/master?recursive=1"
                    .into(),
            tree_prefix: "cheats/".into(),
        },
        // XML game patches (#527): the PS-Game-Patch release zip, which holds one
        // `<TITLE_ID>.xml` per game (PS4 under xml/, PS5 under xml_prospero/).
        CheatRepo {
            id: PS_GAME_PATCH_REPO.into(),
            name: "illusionyy/PS-Game-Patch".into(),
            raw_base: PS_GAME_PATCH_ZIP.into(),
            index_files: Vec::new(),
            content_base: String::new(),
            tree_api: String::new(),
            tree_prefix: String::new(),
        },
    ]
}

/// The repo id of the PS-Game-Patch XML game patches.
pub const PS_GAME_PATCH_REPO: &str = "psgamepatch";
/// The latest PS-Game-Patch release: every patch, already split into one file per title id.
const PS_GAME_PATCH_ZIP: &str =
    "https://github.com/illusion0001/PS4-PS5-Game-Patch/releases/latest/download/patch1.zip";

/// One file of the patch archive: `<TITLE_ID>.xml`, the game's title, the file's bytes.
type PatchFile = (String, String, Vec<u8>);

/// `patches/xml/CUSA03173.xml` or `patches/xml_prospero/PPSA01339.xml` -> `CUSA03173.xml`.
/// Anything else in the archive (build notes, folders, other names) is `None`.
fn patch_archive_name(path: &str) -> Option<String> {
    let rest = path
        .strip_prefix("patches/xml/")
        .or_else(|| path.strip_prefix("patches/xml_prospero/"))?;
    let stem = rest.strip_suffix(".xml")?;
    let (title, _) = parse_cheat_filename(rest);
    (!title.is_empty() && title.eq_ignore_ascii_case(stem)).then(|| format!("{title}.xml"))
}

/// The game title of a patch file: its first `Title="..."` attribute.
fn patch_xml_title(xml: &str) -> String {
    let Some(at) = xml.find("Title=\"") else {
        return String::new();
    };
    let rest = &xml[at + 7..];
    let end = rest.find('"').unwrap_or(0);
    rest[..end]
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Every patch file of the release archive.
fn parse_patch_archive(zip_bytes: &[u8]) -> Result<Vec<PatchFile>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))?;
    let mut out = Vec::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let Some(name) = file.name().ok().and_then(|n| patch_archive_name(&n)) else {
            continue;
        };
        if file.size() > 1024 * 1024 {
            continue;
        }
        let mut bytes = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut bytes)?;
        let title = patch_xml_title(&String::from_utf8_lossy(&bytes));
        out.push((name, title, bytes));
    }
    Ok(out)
}

type PatchArchiveCache =
    std::sync::Mutex<Option<(std::time::Instant, std::sync::Arc<Vec<PatchFile>>)>>;

/// The release archive, fetched at most once an hour (it is under 1 MiB).
fn patch_archive() -> Result<std::sync::Arc<Vec<PatchFile>>> {
    static CACHE: std::sync::OnceLock<PatchArchiveCache> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(None));
    if let Some((at, files)) = cache.lock().ok().and_then(|c| c.clone()) {
        if at.elapsed() < TREE_CACHE_TTL {
            return Ok(files);
        }
    }
    let files = std::sync::Arc::new(parse_patch_archive(&repo_fetch(PS_GAME_PATCH_ZIP)?)?);
    if let Ok(mut c) = cache.lock() {
        *c = Some((std::time::Instant::now(), files.clone()));
    }
    Ok(files)
}

/// Infer cheat format from the index file that contained the entry.
fn format_from_index(index_name: &str) -> &'static str {
    match index_name {
        "json.txt" => "json",
        "shn.txt" => "shn",
        "mc4.txt" => "mc4",
        _ => "json",
    }
}

/// Split a git-tree path into `(filename, format)`.
///
/// Accepts exactly `<prefix><format>/<filename>` where `<format>` is one
/// of `json`, `shn` or `mc4` and the file's extension agrees with it.
/// Anything else — a nested path, an unknown directory, a README —
/// returns `None`.
fn tree_entry_from_path(path: &str, prefix: &str) -> Option<(String, &'static str)> {
    let rest = path.strip_prefix(prefix)?;
    let (dir, filename) = rest.split_once('/')?;
    // Reject nested paths: the filename must be a leaf.
    if filename.contains('/') || filename.is_empty() {
        return None;
    }
    let format = match dir {
        "json" => "json",
        "shn" => "shn",
        "mc4" => "mc4",
        _ => return None,
    };
    // The extension must agree with the directory, so stray files
    // (READMEs, images) inside a format directory are ignored.
    let ext = filename.rsplit_once('.')?.1;
    if !ext.eq_ignore_ascii_case(format) {
        return None;
    }
    Some((filename.to_string(), format))
}

fn repo_fetch(url: &str) -> Result<Vec<u8>> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let resp = agent.get(url).header("User-Agent", "ps5upload").call()?;
    let mut buf = Vec::with_capacity(32 * 1024);
    resp.into_body().into_reader().read_to_end(&mut buf)?;
    Ok(buf)
}

/// Parse one line of `filename=game_title` format.
fn parse_index_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let eq = line.find('=')?;
    let filename = line[..eq].trim().to_string();
    let game_title = line[eq + 1..].trim().to_string();
    if filename.is_empty() {
        return None;
    }
    Some((filename, game_title))
}

/// Cached git-tree listings, keyed by repo id. The trees API is rate
/// limited (60 requests/hour unauthenticated) and each response covers
/// thousands of files, so a listing is reused for an hour rather than
/// refetched on every search.
/// `(filename, format)` pairs for one repo, with the time they were
/// fetched.
type TreeListing = (std::time::Instant, Vec<(String, String)>);

type TreeCache = std::sync::Mutex<std::collections::HashMap<String, TreeListing>>;

fn tree_cache() -> &'static TreeCache {
    static CACHE: std::sync::OnceLock<TreeCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

const TREE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

/// Enumerate a repo that ships no index files by reading its git tree.
/// Returns `(filename, format)` pairs.
fn repo_tree_listing(repo: &CheatRepo) -> Result<Vec<(String, String)>> {
    if let Some((fetched, cached)) = tree_cache()
        .lock()
        .ok()
        .and_then(|c| c.get(&repo.id).cloned())
    {
        if fetched.elapsed() < TREE_CACHE_TTL {
            return Ok(cached);
        }
    }

    let bytes = repo_fetch(&repo.tree_api)?;
    let parsed: serde_json::Value = serde_json::from_slice(&bytes)?;
    let nodes = parsed
        .get("tree")
        .and_then(|t| t.as_array())
        .ok_or_else(|| anyhow::anyhow!("no tree in response for {}", repo.id))?;

    let mut out = Vec::new();
    for node in nodes {
        if node.get("type").and_then(|t| t.as_str()) != Some("blob") {
            continue;
        }
        let Some(path) = node.get("path").and_then(|p| p.as_str()) else {
            continue;
        };
        if let Some((filename, format)) = tree_entry_from_path(path, &repo.tree_prefix) {
            out.push((filename, format.to_string()));
        }
    }

    if let Ok(mut cache) = tree_cache().lock() {
        cache.insert(repo.id.clone(), (std::time::Instant::now(), out.clone()));
    }
    Ok(out)
}

/// Search all community repos for cheat entries matching `query`.
/// `query` may match either the filename (e.g. a CUSA ID) or the game
/// title. Matching is case-insensitive substring.
pub fn cheats_repo_search(query: &str) -> Result<CheatRepoSearchResponse> {
    let mut entries: Vec<CheatRepoEntry> = Vec::new();
    // De-duplicate by filename across every repo. `Vec::dedup_by` only
    // collapses *adjacent* duplicates, so it never caught the case it
    // was written for: the same file listed by two different repos,
    // whose entries are never adjacent because repos are walked in turn.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for repo in cheat_repos() {
        let q = query.to_lowercase();
        let repo_id = repo.id.clone();
        let mut push = |filename: String, game_title: String, format: String| {
            let matches = q.is_empty()
                || filename.to_lowercase().contains(&q)
                || game_title.to_lowercase().contains(&q);
            if !matches || !seen.insert(filename.clone()) {
                return;
            }
            let (title_id, game_version) = parse_cheat_filename(&filename);
            entries.push(CheatRepoEntry {
                filename,
                game_title,
                format,
                repo_id: repo_id.clone(),
                title_id,
                game_version,
            });
        };

        if repo.id == PS_GAME_PATCH_REPO {
            match patch_archive() {
                Ok(files) => {
                    for (filename, title, _) in files.iter() {
                        push(filename.clone(), title.clone(), "xml".into());
                    }
                }
                Err(e) => eprintln!("[cheats] patch archive failed: {e}"),
            }
        } else if repo.index_files.is_empty() {
            // No index published — enumerate through the git tree.
            // Game titles are not available this way, so these
            // entries match on filename (the title id) only.
            match repo_tree_listing(&repo) {
                Ok(listing) => {
                    for (filename, format) in listing {
                        push(filename, String::new(), format);
                    }
                }
                Err(e) => {
                    eprintln!("[cheats] tree listing failed for {}: {}", repo.id, e);
                }
            }
        } else {
            for idx in &repo.index_files {
                let url = format!("{}{}", repo.raw_base, idx);
                match repo_fetch(&url) {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        for line in text.lines() {
                            if let Some((filename, game_title)) = parse_index_line(line) {
                                push(filename, game_title, format_from_index(idx).into());
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[cheats] fetch failed for {}/{}: {}", repo.id, idx, e);
                    }
                }
            }
        }
    }

    Ok(CheatRepoSearchResponse {
        entries,
        error: None,
    })
}

/// Determine the on-PS5 install path for a cheat file.
/// The payload's cheat engine scans `/data/ps5upload/cheats/{json,shn,mc4}/`
/// and matches files by filename prefix (title_id). So we install into
/// the correct format subdirectory based on the cheat format.
fn cheat_install_path(filename: &str, format: &str) -> String {
    let safe = filename.replace("..", "");
    let subdir = match format {
        "shn" => "shn",
        "mc4" => "mc4",
        "xml" => "xml",
        _ => "json",
    };
    format!("/data/ps5upload/cheats/{}/{}", subdir, safe)
}

/// Download a cheat file from a community repo and install it to the
/// PS5. The file lands in `/data/ps5upload/cheats/{json,shn,mc4}/`
/// so the payload's cheat engine finds it on the next reload.
/// Creates the target directory if needed, then writes the file via
/// `fs_write_bytes` (≤256 KB). For larger files this errors — the
/// caller should use the transfer pipeline instead.
pub fn cheats_repo_download(
    addr: &str,
    repo_id: &str,
    filename: &str,
    title_id: &str,
) -> Result<CheatDownloadResponse> {
    let _ = title_id; // title_id is extracted from filename by the payload

    let repo = cheat_repos()
        .into_iter()
        .find(|r| r.id == repo_id)
        .ok_or_else(|| anyhow::anyhow!("unknown repo: {repo_id}"))?;

    if repo.id == PS_GAME_PATCH_REPO {
        let files = patch_archive()?;
        let Some((name, _, bytes)) = files
            .iter()
            .find(|(n, _, _)| n.eq_ignore_ascii_case(filename))
        else {
            return Ok(CheatDownloadResponse {
                ok: false,
                error: Some(format!("{filename} is not in the PS-Game-Patch release")),
                ..Default::default()
            });
        };
        return write_cheat_file(
            addr,
            &cheat_install_path(name, "xml"),
            "/data/ps5upload/cheats/xml",
            bytes,
        );
    }

    // Try each format subdirectory until one succeeds.
    let mut bytes = None;
    // `content_base` is where the format directories actually live,
    // which is not always the repo root (see the henmix entry).
    let base = if repo.content_base.is_empty() {
        repo.raw_base.clone()
    } else {
        repo.content_base.clone()
    };
    for subdir in &["json", "shn", "mc4", "misc"] {
        let try_url = format!("{}{}/{}", base, subdir, filename);
        match repo_fetch(&try_url) {
            Ok(b) => {
                bytes = Some(b);
                break;
            }
            Err(_) => continue,
        }
    }
    let bytes = match bytes {
        Some(b) => b,
        None => {
            return Ok(CheatDownloadResponse {
                ok: false,
                error: Some(format!(
                    "file not found in any subdirectory of {}",
                    repo.name
                )),
                ..Default::default()
            });
        }
    };

    // Determine format from filename extension.
    let format = if filename.ends_with(".shn") {
        "shn"
    } else if filename.ends_with(".mc4") {
        "mc4"
    } else {
        "json"
    };

    let path = cheat_install_path(filename, format);

    // Ensure the target directory exists (idempotent).
    let dir = match format {
        "shn" => "/data/ps5upload/cheats/shn",
        "mc4" => "/data/ps5upload/cheats/mc4",
        _ => "/data/ps5upload/cheats/json",
    };
    write_cheat_file(addr, &path, dir, &bytes)
}

/// Write a downloaded cheat or patch file into its folder on the PS5 (≤ 256 KiB).
fn write_cheat_file(
    addr: &str,
    path: &str,
    dir: &str,
    bytes: &[u8],
) -> Result<CheatDownloadResponse> {
    if let Err(e) = crate::fs_ops::fs_mkdir(addr, dir) {
        eprintln!("[cheats] mkdir {} failed (may already exist): {}", dir, e);
    }

    if bytes.len() > 256 * 1024 {
        return Ok(CheatDownloadResponse {
            ok: false,
            error: Some(format!(
                "file too large for fast write ({} bytes > 256 KiB); use the transfer pipeline",
                bytes.len()
            )),
            ..Default::default()
        });
    }

    match crate::diagnostics::fs_write_bytes(addr, path, bytes, false) {
        Ok(r) => Ok(CheatDownloadResponse {
            ok: true,
            path: Some(path.to_string()),
            size: r.size.or(Some(bytes.len() as u64)),
            ..Default::default()
        }),
        Err(e) => Ok(CheatDownloadResponse {
            ok: false,
            error: Some(e.to_string()),
            ..Default::default()
        }),
    }
}

/// Validate a local JSON trainer and strip persisted activation before writing it.
/// Filename selects the title on the helper; an embedded ID must agree.
pub fn prepare_local_cheat(filename: &str, bytes: &[u8]) -> Result<(String, Vec<u8>)> {
    use anyhow::ensure;
    ensure!(
        bytes.len() <= 256 * 1024,
        "Cheat files must be at most 256 KiB"
    );
    ensure!(
        filename.len() <= 160
            && filename
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            && !filename.contains(".."),
        "Use a plain cheat filename without directories"
    );
    if filename.ends_with(".xml") {
        return prepare_local_patch(filename, bytes);
    }
    ensure!(
        filename.ends_with(".json"),
        "Local import supports JSON trainers and XML game patches; convert SHN/MC4 to JSON first"
    );
    let title = filename.get(..9).unwrap_or("");
    ensure!(
        (title.starts_with("PPSA") || title.starts_with("CUSA"))
            && title[4..].bytes().all(|b| b.is_ascii_digit())
            && matches!(filename.as_bytes().get(9), Some(b'.' | b'_')),
        "Filename must start with its title ID, e.g. PPSA13197_01.000.json"
    );
    let mut v: serde_json::Value = serde_json::from_slice(bytes)?;
    if let Some(id) = v.get("id") {
        ensure!(id.as_str() == Some(title), "Embedded title ID must match the filename; renaming does not make a trainer compatible");
    }
    let mods = v
        .get_mut("mods")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("JSON trainer needs a mods array"))?;
    ensure!(
        !mods.is_empty() && mods.len() <= 64,
        "Trainer must contain 1–64 mods"
    );
    for m in mods {
        ensure!(
            m.get("name")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty() && s.len() < 128),
            "Each mod needs a name under 128 bytes"
        );
        let memory = m
            .get("memory")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("Each mod needs a memory array"))?;
        ensure!(
            !memory.is_empty() && memory.len() <= 32,
            "Each mod must contain 1–32 memory patches"
        );
        for patch in memory {
            let offset = patch
                .get("offset")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            let offset = offset
                .strip_prefix("0x")
                .or_else(|| offset.strip_prefix("0X"))
                .unwrap_or(offset);
            ensure!(
                !offset.is_empty() && offset.len() <= 16 && u64::from_str_radix(offset, 16).is_ok(),
                "Invalid patch offset"
            );
            let mut lengths = Vec::new();
            for field in ["on", "off"] {
                let text = patch.get(field).and_then(|v| v.as_str()).unwrap_or("");
                ensure!(text.len() < 256, "Patch bytes exceed helper limit");
                let hex: String = text
                    .chars()
                    .filter(|c| !" , -:\t\r\n".contains(*c))
                    .collect();
                ensure!(
                    !hex.is_empty()
                        && hex.len().is_multiple_of(2)
                        && hex.bytes().all(|b| b.is_ascii_hexdigit()),
                    "Invalid {field} patch bytes"
                );
                lengths.push(hex.len());
            }
            ensure!(
                lengths[0] == lengths[1],
                "On/off patches must be the same length"
            );
        }
        m["_sonic_enabled"] = false.into();
        // The helper's scanner uses a bounded temporary buffer for each mod.
        ensure!(
            serde_json::to_vec(m)?.len() < 8192,
            "A mod exceeds the helper's parser limit"
        );
    }
    let bytes = serde_json::to_vec(&v)?;
    ensure!(
        bytes.len() <= 256 * 1024,
        "Cheat files must be at most 256 KiB"
    );
    Ok((format!("/data/ps5upload/cheats/json/{filename}"), bytes))
}

/// A PS-Game-Patch XML file (#527): named `<TITLE_ID>.xml` like the published ones, with at least
/// one `<Metadata>` entry. The helper reads it as is (every entry starts off).
fn prepare_local_patch(filename: &str, bytes: &[u8]) -> Result<(String, Vec<u8>)> {
    use anyhow::ensure;
    let (title, _) = parse_cheat_filename(filename);
    ensure!(
        !title.is_empty() && filename.eq_ignore_ascii_case(&format!("{title}.xml")),
        "Name a game patch after its title ID, e.g. PPSA01339.xml"
    );
    let text = String::from_utf8_lossy(bytes);
    ensure!(
        text.contains("<Metadata") && text.contains("<PatchList") && text.contains("<Line"),
        "Not a PS-Game-Patch XML file (no <Metadata> entry with a <PatchList>)"
    );
    Ok((
        format!("/data/ps5upload/cheats/xml/{title}.xml"),
        bytes.to_vec(),
    ))
}

/// Import without replacing files or inheriting index-based toggle state.
pub fn cheats_import_local(addr: &str, source: &std::path::Path) -> Result<CheatDownloadResponse> {
    use anyhow::ensure;
    let filename = source
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid filename"))?;
    let mut bytes = Vec::new();
    std::fs::File::open(source)?
        .take(256 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    let (path, bytes) = prepare_local_cheat(filename, &bytes)?;
    let title = &filename[..9];
    ensure!(
        !cheats_status(addr)?.enabled,
        "Turn off the cheat engine before importing"
    );
    let list = cheats_list(addr)?;
    ensure!(
        !list.truncated && !list.titles.iter().any(|t| t.title_id == title),
        "Remove existing cheats for this title before importing (this clears saved toggle state)"
    );
    let state = format!("/data/ps5upload/cheats/state/{title}.json");
    match crate::fs_ops::fs_stat(addr, &state) {
        Err(e) if crate::fs_ops::is_not_found(&format!("{e:#}")) => {}
        Err(e) => return Err(e),
        Ok(_) => anyhow::bail!(
            "Saved toggle state exists for this title. Remove its cheats before importing"
        ),
    }
    // Directory normally exists after cheat-engine initialization; don't suppress unexpected errors.
    let dir = path
        .rsplit_once('/')
        .map_or("/data/ps5upload/cheats/json", |(d, _)| d);
    if let Err(e) = crate::fs_ops::fs_stat(addr, dir) {
        if crate::fs_ops::is_not_found(&format!("{e:#}")) {
            crate::fs_ops::fs_mkdir(addr, dir)?;
        } else {
            return Err(e);
        }
    }
    let r = crate::diagnostics::fs_write_bytes(addr, &path, &bytes, true)?;
    Ok(CheatDownloadResponse {
        ok: true,
        path: Some(path),
        size: r.size.or(Some(bytes.len() as u64)),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_import_never_overwrites_or_enables_and_refuses_saved_state() {
        use crate::mgmt::{MgmtError, MgmtTransport};
        use std::sync::{Arc, Mutex};
        use std::time::Duration;
        struct Fake {
            active: bool,
            existing: bool,
            state: bool,
            writes: Mutex<Vec<serde_json::Value>>,
        }
        impl MgmtTransport for Fake {
            fn call(
                &self,
                _: &str,
                method: Method,
                _: &str,
                body: &[u8],
                _: Duration,
            ) -> Result<Option<Vec<u8>>> {
                let v = if method == m::CHEATS_STATUS {
                    serde_json::json!({"enabled":self.active})
                } else if method == m::CHEATS_LIST {
                    serde_json::json!({"titles": if self.existing { vec![serde_json::json!({"title_id":"PPSA13197"})] } else { vec![] }})
                } else if method == m::FS_STAT {
                    let b: serde_json::Value = serde_json::from_slice(body)?;
                    if b["path"].as_str().unwrap().contains("/state/") && !self.state {
                        return Err(MgmtError {
                            label: "FS_STAT".into(),
                            status: 1,
                            cause: "fs_stat_failed_errno_2".into(),
                        }
                        .into());
                    }
                    serde_json::json!({"kind":"dir","size":0,"mtime":0,"mode":493})
                } else if method == m::FS_WRITE {
                    self.writes
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(body)?);
                    serde_json::json!({"ok":true,"size":100})
                } else {
                    panic!("unexpected mutation or RPC: {}", method.label);
                };
                Ok(Some(serde_json::to_vec(&v)?))
            }
        }
        let dir =
            std::env::temp_dir().join(format!("ps5upload-local-cheat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("PPSA13197.json");
        std::fs::write(
            &source,
            br#"{"mods":[{"name":"HP","memory":[{"offset":"A0","on":"90","off":"80"}]}]}"#,
        )
        .unwrap();
        for (active, existing, state, allowed) in [
            (false, false, false, true),
            (true, false, false, false),
            (false, true, false, false),
            (false, false, true, false),
        ] {
            let f = Arc::new(Fake {
                active,
                existing,
                state,
                writes: Mutex::new(vec![]),
            });
            let _g = mgmt::scoped_transport(f.clone());
            let result = cheats_import_local("h", &source);
            assert_eq!(result.is_ok(), allowed, "{result:?}");
            let writes = f.writes.lock().unwrap();
            assert_eq!(writes.len(), usize::from(allowed));
            if allowed {
                assert_eq!(writes[0]["mode"], "create");
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn local_import_validates_title_and_disables_persisted_toggles() {
        let src = br#"{"id":"PPSA13197","mods":[{"name":"HP","_sonic_enabled":true,"memory":[{"offset":"A0","on":"90","off":"80"}]}]}"#;
        let (path, bytes) = prepare_local_cheat("PPSA13197_01.000.json", src).unwrap();
        assert_eq!(path, "/data/ps5upload/cheats/json/PPSA13197_01.000.json");
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["mods"][0]["_sonic_enabled"], false);
        assert!(prepare_local_cheat("PPSA13195.json", src).is_err());
        assert!(prepare_local_cheat("../PPSA13197.json", src).is_err());
        assert!(prepare_local_cheat("PPSA13197.json", b"{}").is_err());
        assert!(prepare_local_cheat("PPSA13197.json", &vec![b' '; 256 * 1024 + 1]).is_err());
    }

    #[test]
    fn local_import_takes_xml_game_patches_named_by_title() {
        let src = b"<Patch><Metadata Title=\"D\" Name=\"x\" AppVer=\"01.000.000\"><PatchList>\
                    <Line Type=\"bytes\" Address=\"0x400000\" Value=\"90\"/></PatchList></Metadata></Patch>";
        let (path, bytes) = prepare_local_cheat("PPSA01339.xml", src).unwrap();
        assert_eq!(path, "/data/ps5upload/cheats/xml/PPSA01339.xml");
        assert_eq!(bytes, src.to_vec());
        assert!(prepare_local_cheat("DemonSouls.xml", src).is_err());
        assert!(prepare_local_cheat("PPSA01339_01.000.xml", src).is_err());
        assert!(prepare_local_cheat("PPSA01339.xml", b"<Trainer/>").is_err());
    }

    #[test]
    fn local_import_refuses_patch_strings_that_the_helper_would_truncate() {
        let src = serde_json::json!({"mods":[{"name":"HP","memory":[{"offset":"A0","on":"90".repeat(128),"off":"80".repeat(128)}]}]});
        assert!(prepare_local_cheat("PPSA13197.json", &serde_json::to_vec(&src).unwrap()).is_err());
    }

    #[test]
    fn local_import_rejects_unsupported_formats_and_broken_patch_bytes() {
        assert!(prepare_local_cheat("PPSA13197.mc4", b"encrypted").is_err());
        assert!(prepare_local_cheat(
            "PPSA13197.json",
            br#"{"mods":[{"name":"HP","memory":[{"offset":"A0","on":"ZZ"}]}]}"#
        )
        .is_err());
        assert!(prepare_local_cheat(
            "CUSA00001.json",
            br#"{"mods":[{"name":"HP","memory":[{"offset":"A0","on":"90","off":"80"}]}]}"#
        )
        .is_ok());
    }

    #[test]
    fn deserialize_cheats_list_response() {
        let json = r#"{
            "titles": [
                {"title_id":"CUSA00001","name":"Game A","running":false},
                {"title_id":"CUSA00002","name":"Game B","running":true}
            ],
            "game_running": true,
            "game_title_id": "CUSA00002"
        }"#;
        let resp: CheatsListResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.titles.len(), 2);
        assert_eq!(resp.titles[0].title_id, "CUSA00001");
        assert!(!resp.titles[0].running);
        assert!(resp.titles[1].running);
        assert!(resp.game_running);
        assert_eq!(resp.game_title_id, "CUSA00002");
    }

    #[test]
    fn cheats_list_keeps_version_and_formats() {
        let json = r#"{"titles":[{"title_id":"CUSA00002","name":"Killzone","version":"01.00","formats":["shn","mc4"],"running":false}]}"#;
        let resp: CheatsListResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.titles[0].version, "01.00");
        assert_eq!(resp.titles[0].formats, vec!["shn", "mc4"]);
        let back = serde_json::to_value(&resp).unwrap();
        assert_eq!(back["titles"][0]["formats"][1], "mc4");
        // An older payload without the fields still parses.
        let old: CheatsListResponse =
            serde_json::from_str(r#"{"titles":[{"title_id":"X","name":"X"}]}"#).unwrap();
        assert!(old.titles[0].formats.is_empty());
    }

    #[test]
    fn deserialize_cheats_list_empty() {
        let json = r#"{}"#;
        let resp: CheatsListResponse = serde_json::from_str(json).unwrap();
        assert!(resp.titles.is_empty());
        assert!(!resp.game_running);
    }

    #[test]
    fn deserialize_cheats_get_response() {
        let json = r#"{
            "mods": [
                {"index":0,"name":"Inf HP","desc":"God mode","type":"json","on":true},
                {"index":1,"name":"Inf Ammo","desc":"","type":"shn","on":false}
            ]
        }"#;
        let resp: CheatsGetResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.mods.len(), 2);
        assert_eq!(resp.mods[0].name, "Inf HP");
        assert!(resp.mods[0].on);
        assert!(!resp.mods[1].on);
        assert!(resp.error.is_none());
    }

    #[test]
    fn cheats_get_passes_on_the_running_game_status() {
        let json = r#"{"title_id":"CUSA04402","mods":[
            {"index":8,"name":"60 FPS (01.18)","desc":"","type":"checkbox","on":true,"format":"xml",
             "applied":"failed","applied_reason":"this patch is for game version 01.17; the game is 01.18"},
            {"index":9,"name":"B","desc":"","type":"checkbox","on":true,"applied":"waiting"},
            {"index":10,"name":"C","desc":"","type":"checkbox","on":false}
        ]}"#;
        let resp: CheatsGetResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.mods[0].applied.as_deref(), Some("failed"));
        assert!(resp.mods[0]
            .applied_reason
            .as_deref()
            .unwrap()
            .contains("01.17"));
        assert_eq!(resp.mods[1].applied.as_deref(), Some("waiting"));
        assert!(resp.mods[1].applied_reason.is_none());
        assert!(resp.mods[2].applied.is_none());
        // what the engine hands the apps keeps it, and leaves it out when there is none
        let out = serde_json::to_string(&resp.mods).unwrap();
        assert!(out.contains(r#""applied":"failed","applied_reason":"this patch"#));
        assert!(out.contains(r#""applied":"waiting"}"#));
        assert_eq!(out.matches("applied\"").count(), 2);
    }

    #[test]
    fn deserialize_cheats_toggle_response_ok() {
        let json = r#"{"ok":true}"#;
        let resp: CheatsToggleResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert!(resp.err.is_none());
    }

    #[test]
    fn deserialize_cheats_toggle_response_err() {
        let json = r#"{"ok":false,"err":"process not running"}"#;
        let resp: CheatsToggleResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.err.as_deref(), Some("process not running"));
    }

    #[test]
    fn deserialize_cheats_status_response() {
        let json = r#"{
            "enabled": true,
            "patches_last": 3,
            "patches_total": 12,
            "game_running": true,
            "game_title_id": "CUSA12345",
            "game_pid": 98765
        }"#;
        let resp: CheatsStatusResponse = serde_json::from_str(json).unwrap();
        assert!(resp.enabled);
        assert_eq!(resp.patches_last, 3);
        assert_eq!(resp.patches_total, 12);
        assert!(resp.game_running);
        assert_eq!(resp.game_title_id, "CUSA12345");
        assert_eq!(resp.game_pid, 98765);
        // A helper before 9.5.0 sends no "held".
        assert!(!resp.held);
        let held: CheatsStatusResponse = serde_json::from_str(
            r#"{"enabled":true,"game_running":true,"game_title_id":"CUSA04402","held":true}"#,
        )
        .unwrap();
        assert!(held.held);
    }

    #[test]
    fn deserialize_cheats_engine_set_response() {
        let json = r#"{"ok":true,"enabled":false}"#;
        let resp: CheatsEngineSetResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert!(!resp.enabled);
    }

    #[test]
    fn tree_entry_from_path_json() {
        assert_eq!(
            tree_entry_from_path("cheats/json/CUSA00002_01.00_c20ae0e8.json", "cheats/"),
            Some(("CUSA00002_01.00_c20ae0e8.json".to_string(), "json"))
        );
    }

    #[test]
    fn tree_entry_from_path_shn_and_mc4() {
        assert_eq!(
            tree_entry_from_path("cheats/shn/CUSA123.shn", "cheats/"),
            Some(("CUSA123.shn".to_string(), "shn"))
        );
        assert_eq!(
            tree_entry_from_path("cheats/mc4/CUSA123.mc4", "cheats/"),
            Some(("CUSA123.mc4".to_string(), "mc4"))
        );
    }

    #[test]
    fn tree_entry_from_path_empty_prefix() {
        assert_eq!(
            tree_entry_from_path("json/CUSA1.json", ""),
            Some(("CUSA1.json".to_string(), "json"))
        );
    }

    #[test]
    fn tree_entry_from_path_rejects_outside_prefix() {
        assert_eq!(tree_entry_from_path("docs/json/a.json", "cheats/"), None);
    }

    #[test]
    fn tree_entry_from_path_rejects_unknown_dir() {
        assert_eq!(tree_entry_from_path("cheats/misc/a.json", "cheats/"), None);
    }

    #[test]
    fn tree_entry_from_path_rejects_nested() {
        assert_eq!(
            tree_entry_from_path("cheats/json/sub/a.json", "cheats/"),
            None
        );
    }

    #[test]
    fn tree_entry_from_path_rejects_mismatched_extension() {
        // A README or image sitting inside a format directory.
        assert_eq!(
            tree_entry_from_path("cheats/json/README.md", "cheats/"),
            None
        );
        assert_eq!(tree_entry_from_path("cheats/json/noext", "cheats/"), None);
    }

    #[test]
    fn tree_entry_from_path_rejects_directory_itself() {
        assert_eq!(tree_entry_from_path("cheats/json/", "cheats/"), None);
    }

    #[test]
    fn every_repo_is_enumerable() {
        // A repo must be reachable one way or the other: either it
        // publishes index files, or it declares a tree API. An entry
        // with neither contributes nothing and fails silently.
        for r in cheat_repos() {
            if r.id == PS_GAME_PATCH_REPO {
                // enumerated from its release archive instead
                assert!(r.raw_base.ends_with("/patch1.zip"), "{}", r.raw_base);
                continue;
            }
            assert!(
                !r.index_files.is_empty() || !r.tree_api.is_empty(),
                "repo {} has no enumeration strategy",
                r.id
            );
            assert!(
                !r.content_base.is_empty(),
                "repo {} has no content_base",
                r.id
            );
            assert!(
                r.content_base.ends_with('/'),
                "repo {} content_base must end in / so URLs join correctly",
                r.id
            );
        }
    }

    #[test]
    fn henmix_points_at_master_branch_and_cheats_root() {
        // Regression: this entry used to point at `main/` (the repo is
        // on `master`) and at the repo root (cheats live under
        // `cheats/`), so every lookup 404'd.
        let r = cheat_repos()
            .into_iter()
            .find(|r| r.id == "henmix")
            .unwrap();
        assert!(
            r.content_base.ends_with("/master/cheats/"),
            "{}",
            r.content_base
        );
        assert!(r.tree_api.contains("/git/trees/master"), "{}", r.tree_api);
        assert!(r.index_files.is_empty());
    }

    #[test]
    fn cheat_repos_list_nonempty() {
        let repos = cheat_repos();
        assert!(repos.len() >= 2);
        for r in &repos {
            assert!(!r.id.is_empty());
            assert!(!r.raw_base.is_empty());
            // Enumeration strategy is asserted by
            // `every_repo_is_enumerable`; a repo may legitimately have
            // no index files if it declares a tree API instead.
        }
    }

    #[test]
    fn parse_index_line_basic() {
        let (f, t) = parse_index_line("CUSA00001_cheats.json=Spider-Man").unwrap();
        assert_eq!(f, "CUSA00001_cheats.json");
        assert_eq!(t, "Spider-Man");
    }

    #[test]
    fn parse_index_line_empty_filename() {
        assert!(parse_index_line("=No Filename").is_none());
    }

    #[test]
    fn parse_index_line_comment() {
        assert!(parse_index_line("# this is a comment").is_none());
    }

    #[test]
    fn parse_index_line_blank() {
        assert!(parse_index_line("").is_none());
        assert!(parse_index_line("   ").is_none());
    }

    #[test]
    fn parse_index_line_no_equals() {
        assert!(parse_index_line("just a filename").is_none());
    }

    #[test]
    fn parse_index_line_trims_whitespace() {
        let (f, t) = parse_index_line("  file.json  =  My Game  ").unwrap();
        assert_eq!(f, "file.json");
        assert_eq!(t, "My Game");
    }

    #[test]
    fn format_from_index_mappings() {
        assert_eq!(format_from_index("json.txt"), "json");
        assert_eq!(format_from_index("shn.txt"), "shn");
        assert_eq!(format_from_index("mc4.txt"), "mc4");
        assert_eq!(format_from_index("unknown"), "json");
    }

    #[test]
    fn cheat_install_path_basic() {
        let p = cheat_install_path("cheats.json", "json");
        assert_eq!(p, "/data/ps5upload/cheats/json/cheats.json");
    }

    #[test]
    fn cheat_install_path_shn() {
        let p = cheat_install_path("game.shn", "shn");
        assert_eq!(p, "/data/ps5upload/cheats/shn/game.shn");
    }

    #[test]
    fn cheat_install_path_mc4() {
        let p = cheat_install_path("game.mc4", "mc4");
        assert_eq!(p, "/data/ps5upload/cheats/mc4/game.mc4");
    }

    #[test]
    fn patch_archive_names_are_title_id_files() {
        assert_eq!(
            patch_archive_name("patches/xml/CUSA03173.xml").as_deref(),
            Some("CUSA03173.xml")
        );
        assert_eq!(
            patch_archive_name("patches/xml_prospero/PPSA01339.xml").as_deref(),
            Some("PPSA01339.xml")
        );
        assert_eq!(patch_archive_name("patches/xml/"), None);
        assert_eq!(patch_archive_name("patches/build.txt"), None);
        assert_eq!(patch_archive_name("patches/xml/Bloodborne.xml"), None);
        assert_eq!(patch_archive_name("patches/xml/CUSA03173_01.09.xml"), None);
        assert_eq!(patch_archive_name("other/xml/CUSA03173.xml"), None);
    }

    #[test]
    fn patch_archive_lists_each_title_with_its_game_title() {
        use std::io::{Cursor, Write};
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zw.start_file("patches/build.txt", opts).unwrap();
            zw.write_all(b"Patch database version").unwrap();
            zw.add_directory("patches/xml/", opts).unwrap();
            zw.start_file("patches/xml/CUSA03173.xml", opts).unwrap();
            zw.write_all(
                b"<Patch><Metadata Title=\"Bloodborne &amp; Co\" Name=\"60 FPS\"/></Patch>",
            )
            .unwrap();
            zw.start_file("patches/xml_prospero/PPSA01339.xml", opts)
                .unwrap();
            zw.write_all(
                b"<Patch>\n  <Metadata Title=\"Demons Souls\" Name=\"Debug Menu\"/></Patch>",
            )
            .unwrap();
            zw.finish().unwrap();
        }
        let files = parse_patch_archive(&buf).unwrap();
        assert_eq!(files.len(), 2);
        let bb = files.iter().find(|f| f.0 == "CUSA03173.xml").unwrap();
        assert_eq!(bb.1, "Bloodborne & Co");
        assert!(bb.2.starts_with(b"<Patch>"));
        let ds = files.iter().find(|f| f.0 == "PPSA01339.xml").unwrap();
        assert_eq!(ds.1, "Demons Souls");
        assert_eq!(patch_xml_title("<Patch/>"), "");
    }

    #[test]
    fn xml_patches_install_into_the_xml_folder() {
        assert_eq!(
            cheat_install_path("PPSA01339.xml", "xml"),
            "/data/ps5upload/cheats/xml/PPSA01339.xml"
        );
    }

    #[test]
    fn cheat_install_path_strips_traversal() {
        let p = cheat_install_path("../../../etc/passwd", "json");
        assert!(!p.contains(".."));
        assert!(p.starts_with("/data/ps5upload/cheats/json/"));
    }
}

#[cfg(test)]
mod cheat_filename_tests {
    use super::parse_cheat_filename;

    #[test]
    fn reads_the_title_and_version_from_a_published_name() {
        // Shapes taken verbatim from the etaHEN and GoldHEN indexes.
        assert_eq!(
            parse_cheat_filename("CUSA15438_01.00.json"),
            ("CUSA15438".into(), "01.00".into())
        );
        assert_eq!(
            parse_cheat_filename("CUSA34394_05.01.json"),
            ("CUSA34394".into(), "05.01".into())
        );
        assert_eq!(
            parse_cheat_filename("PPSA01234_02.10.mc4"),
            ("PPSA01234".into(), "02.10".into())
        );
    }

    #[test]
    fn a_variant_counter_is_not_a_version() {
        // `_2` marks a second cheat for the SAME build; reading it as the
        // version would split one game's cheats across two filter values.
        assert_eq!(
            parse_cheat_filename("CUSA09193_01.05_2.json"),
            ("CUSA09193".into(), "01.05".into())
        );
    }

    #[test]
    fn a_name_that_breaks_the_convention_still_lists() {
        // Empty rather than an error: such an entry must still appear in the
        // browser, it just cannot be filtered on.
        assert_eq!(
            parse_cheat_filename("readme.md"),
            (String::new(), String::new())
        );
        assert_eq!(
            parse_cheat_filename("CUSA1.json"),
            (String::new(), String::new())
        );
        assert_eq!(parse_cheat_filename(""), (String::new(), String::new()));
        // Title id but no version segment.
        assert_eq!(
            parse_cheat_filename("CUSA15438.json"),
            ("CUSA15438".into(), String::new())
        );
    }

    #[test]
    fn the_title_id_is_normalised_for_matching() {
        // The installed-apps list reports upper case; a lower-case filename
        // must still match it.
        assert_eq!(parse_cheat_filename("cusa15438_01.00.json").0, "CUSA15438");
    }
}
