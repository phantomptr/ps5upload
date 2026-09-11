//! Cheat engine proxy: list titles, list mods, toggle, delete, reload,
//! status, and enable/disable the engine master flag.
//!
//! Also includes a community cheat download system that fetches cheat
//! files from GitHub repositories (etaHEN/PS5_Cheats, GoldHEN, etc.)
//! and installs them to the PS5 filesystem.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

#[cfg(not(target_os = "android"))]
use std::io::Read;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatTitle {
    #[serde(default)]
    pub title_id: String,
    #[serde(default)]
    pub name: String,
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

/// Helper: send a frame and expect an ACK of the given type.
fn send_recv(
    addr: &str,
    req_type: FrameType,
    ack_type: FrameType,
    body: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let mut c = Connection::connect(addr)?;
    let empty: Vec<u8> = Vec::new();
    let body = body.unwrap_or(&empty);
    c.send_frame(req_type, body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected {:?}: {}",
            req_type,
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != ack_type {
        bail!("expected {:?}, got {ack_type:?}", ack_type);
    }
    Ok(resp)
}

pub fn cheats_list(addr: &str) -> Result<CheatsListResponse> {
    let resp = send_recv(addr, FrameType::CheatsList, FrameType::CheatsListAck, None)?;
    Ok(serde_json::from_slice(&resp)?)
}

pub fn cheats_get(addr: &str, title_id: &str) -> Result<CheatsGetResponse> {
    let body = serde_json::json!({ "title_id": title_id });
    let resp = send_recv(
        addr,
        FrameType::CheatsGet,
        FrameType::CheatsGetAck,
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
        FrameType::CheatsToggle,
        FrameType::CheatsToggleAck,
        Some(&serde_json::to_vec(&req)?),
    )?;
    Ok(serde_json::from_slice(&resp)?)
}

pub fn cheats_delete(addr: &str, title_id: &str) -> Result<bool> {
    let body = serde_json::json!({ "title_id": title_id });
    let resp = send_recv(
        addr,
        FrameType::CheatsDelete,
        FrameType::CheatsDeleteAck,
        Some(&serde_json::to_vec(&body)?),
    )?;
    let v: serde_json::Value = serde_json::from_slice(&resp)?;
    Ok(v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false))
}

pub fn cheats_reload(addr: &str) -> Result<bool> {
    let resp = send_recv(
        addr,
        FrameType::CheatsReload,
        FrameType::CheatsReloadAck,
        None,
    )?;
    let v: serde_json::Value = serde_json::from_slice(&resp)?;
    Ok(v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false))
}

pub fn cheats_status(addr: &str) -> Result<CheatsStatusResponse> {
    let resp = send_recv(
        addr,
        FrameType::CheatsStatus,
        FrameType::CheatsStatusAck,
        None,
    )?;
    Ok(serde_json::from_slice(&resp)?)
}

pub fn cheats_engine_set(addr: &str, enabled: bool) -> Result<CheatsEngineSetResponse> {
    let req = CheatsEngineSetRequest { enabled };
    let resp = send_recv(
        addr,
        FrameType::CheatsEngineSet,
        FrameType::CheatsEngineSetAck,
        Some(&serde_json::to_vec(&req)?),
    )?;
    Ok(serde_json::from_slice(&resp)?)
}

// ─── Community cheat download system ─────────────────────────────────
//
// Fetches cheat files from well-known GitHub repositories and installs
// them onto the PS5 filesystem under /data/cheats/<title_id>/.
//
// On non-Android targets the functions use `ureq` for HTTP. On Android
// they return an error (the Android app does its own HTTP via OkHttp).

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatRepoSearchRequest {
    pub addr: String,
    pub query: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CheatRepoSearchResponse {
    #[serde(default)]
    pub entries: Vec<CheatRepoEntry>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatDownloadRequest {
    pub addr: String,
    pub repo_id: String,
    pub filename: String,
    pub title_id: String,
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
    ]
}

/// Infer cheat format from the index file that contained the entry.
#[cfg(not(target_os = "android"))]
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
#[cfg(any(not(target_os = "android"), test))]
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

#[cfg(not(target_os = "android"))]
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
#[cfg(not(target_os = "android"))]
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
#[cfg(not(target_os = "android"))]
type TreeListing = (std::time::Instant, Vec<(String, String)>);

#[cfg(not(target_os = "android"))]
type TreeCache = std::sync::Mutex<std::collections::HashMap<String, TreeListing>>;

#[cfg(not(target_os = "android"))]
fn tree_cache() -> &'static TreeCache {
    static CACHE: std::sync::OnceLock<TreeCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(not(target_os = "android"))]
const TREE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

/// Enumerate a repo that ships no index files by reading its git tree.
/// Returns `(filename, format)` pairs.
#[cfg(not(target_os = "android"))]
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
    // Android does its own HTTP via OkHttp, so none of the fetch path
    // below is compiled there. Returning early keeps that a single
    // statement instead of leaving a body whose locals are never
    // mutated and whose helpers are dead -- which is what the Android
    // build was warning about.
    #[cfg(target_os = "android")]
    {
        let _ = query;
        eprintln!("[cheats] repo fetch not available on android");
        return Ok(CheatRepoSearchResponse {
            entries: Vec::new(),
            error: None,
        });
    }

    #[cfg(not(target_os = "android"))]
    {
        let mut entries: Vec<CheatRepoEntry> = Vec::new();
        // De-duplicate by filename across every repo. `Vec::dedup_by` only
        // collapses *adjacent* duplicates, so it never caught the case it
        // was written for: the same file listed by two different repos,
        // whose entries are never adjacent because repos are walked in turn.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for repo in cheat_repos() {
            #[cfg(not(target_os = "android"))]
            {
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

                if repo.index_files.is_empty() {
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
            #[cfg(target_os = "android")]
            {
                let _ = &repo;
                eprintln!("[cheats] repo fetch not available on android");
            }
        }

        Ok(CheatRepoSearchResponse {
            entries,
            error: None,
        })
    }
}

/// Determine the on-PS5 install path for a cheat file.
/// The payload's cheat engine scans `/data/ps5upload/cheats/{json,shn,mc4}/`
/// and matches files by filename prefix (title_id). So we install into
/// the correct format subdirectory based on the cheat format.
#[cfg(not(target_os = "android"))]
fn cheat_install_path(filename: &str, format: &str) -> String {
    let safe = filename.replace("..", "");
    let subdir = match format {
        "shn" => "shn",
        "mc4" => "mc4",
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

    #[cfg(not(target_os = "android"))]
    {
        let repo = cheat_repos()
            .into_iter()
            .find(|r| r.id == repo_id)
            .ok_or_else(|| anyhow::anyhow!("unknown repo: {repo_id}"))?;

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
        if let Err(e) = crate::fs_ops::fs_mkdir(addr, dir) {
            eprintln!("[cheats] mkdir {} failed (may already exist): {}", dir, e);
        }

        if bytes.len() > 256 * 1024 {
            return Ok(CheatDownloadResponse {
                ok: false,
                error: Some(format!(
                    "file too large for fast write ({} bytes > 256 KB); use the transfer pipeline",
                    bytes.len()
                )),
                ..Default::default()
            });
        }

        match crate::diagnostics::fs_write_bytes(addr, &path, &bytes, false) {
            Ok(r) => Ok(CheatDownloadResponse {
                ok: true,
                path: Some(path),
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

    #[cfg(target_os = "android")]
    {
        let _ = (addr, filename, repo_id);
        Ok(CheatDownloadResponse {
            ok: false,
            error: Some("repo download not available on android".into()),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
