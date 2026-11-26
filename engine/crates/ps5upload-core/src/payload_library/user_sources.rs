//! Payload sources the user adds: a GitHub project, or a Gitea or Forgejo one (on the internet or
//! on the home network), that publishes payload files in its releases.
//!
//! The user picks the file once. The app reads the project's releases only when asked (opening
//! Payloads, or Check), keeps what it read for ten minutes, downloads only on Download or Update,
//! and sends only on Send (through the usual send path). Nothing runs in the background. The
//! built-in catalogue stays a list of links: only a project the user added is ever downloaded.
//!
//! Stored with the repos earlier versions added (`custom-repos.json`, ids `custom-<hex>`): an entry
//! with a `file_name` is complete; one without (added before 12.6.0) asks for the file to be
//! picked.
//!
//! **Which file a newer release uses.** Two names match when they are the same apart from their
//! version parts. A version part is a number that does not follow a letter (a `v` before it
//! excepted), with the dots and letters joined to it: `1.6.0`, `2.4b`, `20240101`. So
//! `kstuff-v1.6.0.elf` matches `kstuff-v1.7.elf`, while `ftpsrv-ps5.elf` (the 5 follows a letter)
//! matches only itself and never `ftpsrv-ps4.elf`. Case does not matter. In a release, a file with
//! the exact name comes first.
//!
//! **Addresses.** https only, no user name or password in the address. github.com projects use
//! GitHub's API and its release-file hosts. Any other host is a Gitea or Forgejo server, which may
//! be on the home network (a private address or a LAN name) and on its own port; never this
//! computer (loopback, `localhost`) or a link-local address. Every redirect is checked again
//! (`http::hop_allowed`).
//!
//! **Files per source**, in `<dir>/<id>/`: the payload, `download.json` (which release it is),
//! `VERSION` (the tag, as older versions kept it) and `source-releases.json` (the last release
//! list read, used for ten minutes).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::cache::sha256_of_file;
use super::http::{fetch, host_class, FetchKind, HostClass};
use super::releases::{GithubAsset, GithubRelease, JSON_ACCEPT, RELEASE_JSON_MAX_BYTES};
use super::repos::{is_valid_custom_id, CustomRepo};
use super::{
    atomic_write, elf_in_dir, is_newer, replace_file, unique_suffix, Library, LibraryError,
    ResolvedEntry, TmpFile,
};

/// How long a release list read from a project is used before it is read again (unless the user
/// presses Check). GitHub allows 60 anonymous requests an hour.
const SOURCE_CHECK_TTL: Duration = Duration::from_secs(600);
/// The newest releases read from a project.
const SOURCE_RELEASE_COUNT: usize = 10;
/// The largest payload file downloaded.
pub(crate) const SOURCE_DOWNLOAD_MAX_BYTES: u64 = 128 * 1024 * 1024;
/// The largest checksum file read.
const CHECKSUM_FILE_MAX_BYTES: u64 = 64 * 1024;
/// A partial download older than this was left by a crash.
const STALE_PART_AGE: Duration = Duration::from_secs(3600);
const RELEASES_FILE: &str = "source-releases.json";
const DOWNLOAD_FILE: &str = "download.json";
const URL_MAX_CHARS: usize = 500;
const FILE_NAME_MAX_CHARS: usize = 200;
const SEGMENT_MAX_CHARS: usize = 100;

/// Why a source call failed: a stable `code` the app words itself, an English `message`, and for
/// a host's rate limit, when to try again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceError {
    pub code: String,
    pub message: String,
    /// Seconds since the Unix epoch: when the host said to ask again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<u64>,
}

impl SourceError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            retry_at: None,
        }
    }
}

impl From<LibraryError> for SourceError {
    fn from(e: LibraryError) -> Self {
        Self::new(e.code, e.message)
    }
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SourceError {}

// ── Addresses ────────────────────────────────────────────────────────────────────────────────────

/// A project as the user's address names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRef {
    /// `github.com`, or a Gitea or Forgejo server's host as a URL names it (lowercase).
    pub host: String,
    /// A Gitea or Forgejo server's port, when it is not 443.
    pub port: Option<u16>,
    pub owner: String,
    pub name: String,
}

impl ProjectRef {
    pub fn is_github(&self) -> bool {
        self.host == "github.com"
    }

    fn host_and_port(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{p}", self.host),
            None => self.host.clone(),
        }
    }

    pub fn project_url(&self) -> String {
        format!(
            "https://{}/{}/{}",
            self.host_and_port(),
            self.owner,
            self.name
        )
    }

    /// `custom-<12 hex>`: FNV-1a over the lowercased `host[:port]/owner/name`, as earlier versions
    /// minted it, so adding a project again finds the same entry and folder.
    pub fn id(&self) -> String {
        let key =
            format!("{}/{}/{}", self.host_and_port(), self.owner, self.name).to_ascii_lowercase();
        let mut h: u64 = 0xcbf29ce484222325;
        for b in key.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        format!("custom-{:012x}", h & 0xffff_ffff_ffff)
    }

    fn entry(&self) -> ResolvedEntry {
        ResolvedEntry {
            id: self.id(),
            repo_host: self.host.clone(),
            repo_owner: self.owner.clone(),
            repo_name: self.name.clone(),
            asset_name_hint: String::new(),
            is_custom: true,
            list_payload: None,
            repo_port: self.port,
        }
    }

    fn of_repo(r: &CustomRepo) -> Self {
        Self {
            host: r.repo_host.clone(),
            port: r.repo_port,
            owner: r.repo_owner.clone(),
            name: r.repo_name.clone(),
        }
    }
}

/// One path part of a project: an owner or a project name.
fn is_valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= SEGMENT_MAX_CHARS
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// A host name or address, as a URL names it: letters, digits, dots and dashes, or an IPv6
/// address in brackets.
fn is_plausible_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return inner.parse::<std::net::Ipv6Addr>().is_ok();
    }
    host.split('.').all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    })
}

/// Read a project's address: `https://github.com/owner/project` (anything after the project, like
/// `/releases`, is ignored), or a Gitea or Forgejo server's `https://host[:port]/owner/project`.
/// An address without a scheme is read as https.
pub fn parse_project_url(raw: &str) -> Result<ProjectRef, SourceError> {
    let raw = raw.trim();
    let not_a_project = || {
        SourceError::new(
            "url_not_a_project",
            "That isn't a project address. Paste the project's page, like \
             https://github.com/owner/project.",
        )
    };
    if raw.is_empty() || raw.chars().count() > URL_MAX_CHARS || raw.chars().any(char::is_whitespace)
    {
        return Err(not_a_project());
    }
    let lower = raw.to_ascii_lowercase();
    let with_scheme = if lower.starts_with("https://") {
        raw.to_string()
    } else if lower.contains("://") {
        return Err(SourceError::new(
            "url_not_https",
            "Only https:// addresses can be added.",
        ));
    } else {
        format!("https://{raw}")
    };
    let uri: ureq::http::Uri = with_scheme.parse().map_err(|_| not_a_project())?;
    let authority = uri.authority().ok_or_else(not_a_project)?;
    if authority.as_str().contains('@') {
        return Err(SourceError::new(
            "url_has_credentials",
            "Remove the user name or password from the address.",
        ));
    }
    let host = uri
        .host()
        .ok_or_else(not_a_project)?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let port = uri.port_u16().filter(|p| *p != 443);
    let mut parts = uri.path().split('/').filter(|s| !s.is_empty());
    let owner = parts.next().unwrap_or_default().to_string();
    let name = parts
        .next()
        .unwrap_or_default()
        .trim_end_matches(".git")
        .to_string();
    if !is_valid_segment(&owner) || !is_valid_segment(&name) || !is_plausible_host(&host) {
        return Err(not_a_project());
    }
    if host == "github.com" || host == "www.github.com" {
        if port.is_some() {
            return Err(not_a_project());
        }
        return Ok(ProjectRef {
            host: "github.com".into(),
            port: None,
            owner,
            name,
        });
    }
    if host.ends_with(".github.com")
        || host.ends_with("githubusercontent.com")
        || host == "github.io"
        || host.ends_with(".github.io")
    {
        return Err(not_a_project());
    }
    if host == "gitlab.com" || host.starts_with("gitlab.") {
        return Err(SourceError::new(
            "url_gitlab",
            "GitLab projects aren't supported. Use a GitHub, Gitea or Forgejo project.",
        ));
    }
    if host_class(&host) == HostClass::Forbidden {
        return Err(SourceError::new(
            "url_host_not_allowed",
            format!(
                "{host} can't be used: it is this computer or a link-local address. Use the \
                 server's address on your network."
            ),
        ));
    }
    Ok(ProjectRef {
        host,
        port,
        owner,
        name,
    })
}

// ── File names ───────────────────────────────────────────────────────────────────────────────────

/// A file name a source can use: a plain `.elf` or `.bin` name.
pub fn is_payload_file_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !name.is_empty()
        && name.chars().count() <= FILE_NAME_MAX_CHARS
        && (lower.ends_with(".elf") || lower.ends_with(".bin"))
        && lower.len() > 4
        && !name.starts_with('.')
        && !name.contains("..")
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '%' | '*' | '?' | '"'))
}

#[derive(Debug, PartialEq, Eq)]
enum Part {
    Text(String),
    Version,
}

/// A name cut into text and version parts (see the module's notes). Text is kept as written.
fn name_parts(name: &str) -> Vec<Part> {
    let chars: Vec<char> = name.chars().collect();
    let mut out = Vec::new();
    let mut text = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let starts_part = c.is_ascii_digit()
            && match i {
                0 => true,
                _ => {
                    let prev = chars[i - 1];
                    !prev.is_alphanumeric()
                        || (matches!(prev, 'v' | 'V') && (i < 2 || !chars[i - 2].is_alphanumeric()))
                }
            };
        if !starts_part {
            text.push(c);
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() {
            let d = chars[j];
            if d.is_ascii_alphanumeric()
                || (d == '.' && chars.get(j + 1).is_some_and(|n| n.is_ascii_digit()))
            {
                j += 1;
            } else {
                break;
            }
        }
        if !text.is_empty() {
            out.push(Part::Text(std::mem::take(&mut text)));
        }
        out.push(Part::Version);
        i = j;
    }
    if !text.is_empty() {
        out.push(Part::Text(text));
    }
    out
}

/// The picked name as it shows which names newer releases may use: `*` for each version part
/// (`kstuff-v1.6.0.elf` → `kstuff-v*.elf`). A name without one shows as itself.
pub fn name_pattern(name: &str) -> String {
    name_parts(name)
        .iter()
        .map(|p| match p {
            Part::Text(t) => t.as_str(),
            Part::Version => "*",
        })
        .collect()
}

/// Whether `candidate` is the picked `name` or the same apart from its version parts.
pub fn names_match(name: &str, candidate: &str) -> bool {
    let (a, b) = (name_parts(name), name_parts(candidate));
    a.len() == b.len()
        && a.iter().zip(&b).all(|pair| match pair {
            (Part::Version, Part::Version) => true,
            (Part::Text(x), Part::Text(y)) => x.eq_ignore_ascii_case(y),
            _ => false,
        })
}

/// The file a release has for the picked name: the exact name first, else a match.
fn file_in_release<'a>(release: &'a GithubRelease, picked: &str) -> Option<&'a GithubAsset> {
    let usable = |a: &&GithubAsset| is_payload_file_name(&a.name);
    release
        .assets
        .iter()
        .filter(usable)
        .find(|a| a.name.eq_ignore_ascii_case(picked))
        .or_else(|| {
            release
                .assets
                .iter()
                .filter(usable)
                .find(|a| names_match(picked, &a.name))
        })
}

/// The newest release with a file for the picked name, and that file.
fn newest_match<'a>(
    list: &'a [GithubRelease],
    picked: &str,
) -> Option<(&'a GithubRelease, &'a GithubAsset)> {
    list.iter()
        .find_map(|r| file_in_release(r, picked).map(|a| (r, a)))
}

// ── Checksums ────────────────────────────────────────────────────────────────────────────────────

/// Checksum files a release may carry for its files: one per file, or a list.
const CHECKSUM_LISTS: &[&str] = &[
    "sha256sums",
    "sha256sums.txt",
    "sha256sum.txt",
    "sha256.txt",
    "checksums.txt",
    "checksums.sha256",
    "checksum.txt",
];

/// The release's checksum file for `file`, and whether it is for that one file alone.
fn checksum_asset<'a>(release: &'a GithubRelease, file: &str) -> Option<(&'a GithubAsset, bool)> {
    let lower = file.to_ascii_lowercase();
    let own = [
        format!("{lower}.sha256"),
        format!("{lower}.sha256sum"),
        format!("{lower}.sha256.txt"),
    ];
    if let Some(a) = release
        .assets
        .iter()
        .find(|a| own.contains(&a.name.to_ascii_lowercase()))
    {
        return Some((a, true));
    }
    release
        .assets
        .iter()
        .find(|a| CHECKSUM_LISTS.contains(&a.name.to_ascii_lowercase().as_str()))
        .map(|a| (a, false))
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// The SHA-256 a checksum file gives `file`: `<hex>  <name>` (or `<hex> *<name>`), BSD's
/// `SHA256 (<name>) = <hex>`, or for a file's own checksum file the first hash in it. `None` when
/// it has none for `file`.
fn expected_sha256(text: &str, file: &str, own_file: bool) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("SHA256 (") {
            if let Some((name, hex)) = rest.split_once(") = ") {
                let base = name.rsplit('/').next().unwrap_or(name);
                if base.eq_ignore_ascii_case(file) && is_sha256_hex(hex.trim()) {
                    return Some(hex.trim().to_ascii_lowercase());
                }
            }
            continue;
        }
        let mut words = line.split_whitespace();
        let Some(hex) = words.next().filter(|h| is_sha256_hex(h)) else {
            continue;
        };
        match words.next() {
            Some(name) => {
                let name = name.trim_start_matches('*');
                let base = name.rsplit('/').next().unwrap_or(name);
                if base.eq_ignore_ascii_case(file) {
                    return Some(hex.to_ascii_lowercase());
                }
            }
            None if own_file => return Some(hex.to_ascii_lowercase()),
            None => {}
        }
    }
    None
}

// ── What the screens show ────────────────────────────────────────────────────────────────────────

/// The copy of a source's file on this computer.
#[derive(Debug, Clone, Serialize)]
pub struct HaveCopy {
    /// The release it came from; empty for a copy an old version saved without one.
    pub tag: String,
    pub file_name: String,
    pub size: u64,
    /// SHA-256 of the file as it is on disk now: what a send gives the console.
    pub sha256: String,
    pub published_at: String,
    /// Seconds since the Unix epoch; 0 when unknown.
    pub downloaded_at: i64,
    /// `matched` (the release's checksum file named this file and it matched), `none` (the
    /// release had none for it), or empty for a copy from an older version.
    pub checksum: String,
    /// Where it is: what a send uses.
    pub path: String,
}

/// The newest release's file for the picked name.
#[derive(Debug, Clone, Serialize)]
pub struct LatestFile {
    pub tag: String,
    pub release_name: String,
    pub file_name: String,
    pub size: u64,
    pub published_at: String,
    pub prerelease: bool,
    pub html_url: String,
}

/// A source as the screens show it.
#[derive(Debug, Clone, Serialize)]
pub struct UserSource {
    pub id: String,
    pub name: String,
    /// `github` or `gitea` (Gitea and Forgejo).
    pub kind: &'static str,
    pub project_url: String,
    pub owner: String,
    pub repo: String,
    /// The picked file. Empty for an entry added before 12.6.0: it asks for the file.
    pub file_name: String,
    /// Which names newer releases may use: the picked name with `*` for each version part.
    pub file_pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub have: Option<HaveCopy>,
    /// Known after a check: the newest release's file for the picked name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<LatestFile>,
    /// The newest release's file is newer than the copy here.
    pub update_available: bool,
    /// Age of the release list shown, seconds. Absent before any check.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_age_secs: Option<u64>,
    /// The last check failed (the list shown, if any, is older), or no release has the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<SourceError>,
}

/// One file of a release, as the add step lists it.
#[derive(Debug, Clone, Serialize)]
pub struct PreviewFile {
    pub name: String,
    pub size: u64,
    /// What newer releases may name it (see [`name_pattern`]).
    pub pattern: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreviewRelease {
    pub tag: String,
    pub name: String,
    pub published_at: String,
    pub prerelease: bool,
    pub html_url: String,
    /// Its `.elf` and `.bin` files.
    pub files: Vec<PreviewFile>,
}

/// A project's newest releases with payload files, before (or instead of) adding it.
#[derive(Debug, Clone, Serialize)]
pub struct SourcePreview {
    /// The id it has once added.
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub project_url: String,
    /// It is added already: adding again changes its file.
    pub already_added: bool,
    /// Its picked file, when added already.
    pub current_file: String,
    /// Newest first; only releases with a `.elf` or `.bin` file.
    pub releases: Vec<PreviewRelease>,
}

/// What `download.json` keeps about a downloaded file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DownloadRecord {
    tag: String,
    file_name: String,
    #[serde(default)]
    published_at: String,
    #[serde(default)]
    checksum: String,
    #[serde(default)]
    downloaded_at: i64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn kind_of(p: &ProjectRef) -> &'static str {
    if p.is_github() {
        "github"
    } else {
        "gitea"
    }
}

/// The release list's address: GitHub's API, or the Gitea/Forgejo API on the server itself.
fn releases_api_url(p: &ProjectRef) -> String {
    if p.is_github() {
        format!(
            "https://api.github.com/repos/{}/{}/releases?per_page={SOURCE_RELEASE_COUNT}",
            p.owner, p.name
        )
    } else {
        format!(
            "https://{}/api/v1/repos/{}/{}/releases?limit={SOURCE_RELEASE_COUNT}",
            p.host_and_port(),
            p.owner,
            p.name
        )
    }
}

fn host_label(p: &ProjectRef) -> String {
    if p.is_github() {
        "GitHub".to_string()
    } else {
        p.host_and_port()
    }
}

/// Removes every `.elf` and `.bin` in a source's folder except `keep`.
fn remove_other_payload_files(dir: &Path, keep: &str) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for f in read.flatten() {
        let p = f.path();
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        if p.is_file()
            && matches!(ext.as_deref(), Some("elf") | Some("bin"))
            && f.file_name().to_str() != Some(keep)
        {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Removes partial downloads a crash left (older than an hour; a newer one may be a download in
/// progress).
fn sweep_stale_parts(dir: &Path) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for f in read.flatten() {
        let name = f.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".part") {
            continue;
        }
        let old = f
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age >= STALE_PART_AGE);
        if old {
            let _ = std::fs::remove_file(f.path());
        }
    }
}

/// A release list as a host answered it: the published releases, newest first.
fn parse_release_list(body: &str) -> Result<Vec<GithubRelease>, SourceError> {
    let list: Vec<GithubRelease> = serde_json::from_str(body).map_err(|e| {
        SourceError::new(
            "fetch_failed",
            format!("The answer wasn't a list of releases ({e})."),
        )
    })?;
    Ok(list.into_iter().filter(|r| !r.draft).collect())
}

impl Library {
    fn find_source(&self, id: &str) -> Result<CustomRepo, SourceError> {
        let unknown = || SourceError::new("unknown_source", format!("no payload source {id}"));
        if !is_valid_custom_id(id) {
            return Err(unknown());
        }
        self.read_repo_file()?
            .repos
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(unknown)
    }

    /// Ask a project for its newest releases.
    fn fetch_release_list(&self, p: &ProjectRef) -> Result<Vec<GithubRelease>, SourceError> {
        let entry = p.entry();
        let url = releases_api_url(p);
        let resp = fetch(
            self.http.as_ref(),
            &entry,
            &url,
            Some(JSON_ACCEPT),
            FetchKind::Metadata,
        )
        .map_err(|e| {
            if e.code == "fetch_failed" {
                SourceError::new(
                    "unreachable",
                    format!("Couldn't reach {}: {}", host_label(p), e.message),
                )
            } else {
                e.into()
            }
        })?;
        let status = resp.status;
        if status == 429 || (status == 403 && resp.retry_at.is_some()) {
            return Err(SourceError {
                code: "rate_limited".into(),
                message: format!(
                    "{} is limiting how often it can be asked. Try again later.",
                    host_label(p)
                ),
                retry_at: resp.retry_at,
            });
        }
        if status == 404 {
            return Err(SourceError::new(
                "project_not_found",
                format!(
                    "{} has no public project {}/{}.",
                    host_label(p),
                    p.owner,
                    p.name
                ),
            ));
        }
        if !(200..300).contains(&status) {
            return Err(SourceError::new(
                "fetch_failed",
                format!("{} answered HTTP {status}.", host_label(p)),
            ));
        }
        let mut body = String::new();
        resp.body
            .take(RELEASE_JSON_MAX_BYTES)
            .read_to_string(&mut body)
            .map_err(|e| {
                SourceError::new(
                    "unreachable",
                    format!("Couldn't read {}'s answer: {e}", host_label(p)),
                )
            })?;
        parse_release_list(&body)
    }

    fn releases_cache(&self, id: &str) -> PathBuf {
        self.dir.join(id).join(RELEASES_FILE)
    }

    /// The release list saved for a source, and its age. `fresh_only`: only one from the last ten
    /// minutes.
    fn cached_release_list(&self, id: &str, fresh_only: bool) -> Option<(Vec<GithubRelease>, u64)> {
        let path = self.releases_cache(id);
        let age = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .unwrap_or_default();
        if fresh_only && age >= SOURCE_CHECK_TTL {
            return None;
        }
        let list = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
        Some((list, age.as_secs()))
    }

    fn save_release_list(&self, id: &str, list: &[GithubRelease]) {
        let dir = self.dir.join(id);
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        if let Ok(body) = serde_json::to_vec(list) {
            // A saving, not the answer: a full or read-only folder must not fail a check.
            let _ = atomic_write(&dir.join(RELEASES_FILE), &body);
        }
    }

    /// A source's release list: the one from the last ten minutes (unless `force`), else the
    /// project's. A failed read answers the older list, if any, with the error. `skip` is an error
    /// this pass already had from the same host (its rate limit, or no network): the host is not
    /// asked again.
    fn source_release_list(
        &self,
        repo: &CustomRepo,
        force: bool,
        skip: Option<&SourceError>,
    ) -> (Option<(Vec<GithubRelease>, u64)>, Option<SourceError>) {
        if !force {
            if let Some(fresh) = self.cached_release_list(&repo.id, true) {
                return (Some(fresh), None);
            }
        }
        let fetched = match skip {
            Some(e) => Err(e.clone()),
            None => self.fetch_release_list(&ProjectRef::of_repo(repo)),
        };
        match fetched {
            Ok(list) => {
                self.save_release_list(&repo.id, &list);
                (Some((list, 0)), None)
            }
            Err(e) => (self.cached_release_list(&repo.id, false), Some(e)),
        }
    }

    /// The copy of a source's file here, if any.
    fn have_copy(&self, repo: &CustomRepo) -> Option<HaveCopy> {
        let dir = self.dir.join(&repo.id);
        if !dir.is_dir() {
            return None;
        }
        let record: Option<DownloadRecord> = std::fs::read(dir.join(DOWNLOAD_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .filter(|r: &DownloadRecord| is_payload_file_name(&r.file_name));
        let (path, record) = match record {
            Some(r) if dir.join(&r.file_name).is_file() => (dir.join(&r.file_name), r),
            _ => {
                // A copy an older version saved: its `.elf` and `VERSION`.
                let path = elf_in_dir(&dir)?;
                let tag = std::fs::read_to_string(dir.join("VERSION"))
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let file_name = path.file_name()?.to_string_lossy().into_owned();
                (
                    path,
                    DownloadRecord {
                        tag,
                        file_name,
                        ..DownloadRecord::default()
                    },
                )
            }
        };
        let size = std::fs::metadata(&path).ok()?.len();
        let sha256 = sha256_of_file(&path).ok()?;
        Some(HaveCopy {
            tag: record.tag,
            file_name: record.file_name,
            size,
            sha256,
            published_at: record.published_at,
            downloaded_at: record.downloaded_at,
            checksum: record.checksum,
            path: path.to_string_lossy().into_owned(),
        })
    }

    fn source_view(
        &self,
        repo: &CustomRepo,
        list: Option<(Vec<GithubRelease>, u64)>,
        error: Option<SourceError>,
    ) -> UserSource {
        let p = ProjectRef::of_repo(repo);
        let have = self.have_copy(repo);
        let mut error = error;
        let mut latest = None;
        let mut checked_age_secs = None;
        if let Some((list, age)) = &list {
            checked_age_secs = Some(*age);
            if !repo.file_name.is_empty() {
                match newest_match(list, &repo.file_name) {
                    Some((r, a)) => {
                        latest = Some(LatestFile {
                            tag: r.tag_name.clone(),
                            release_name: if r.name.is_empty() {
                                r.tag_name.clone()
                            } else {
                                r.name.clone()
                            },
                            file_name: a.name.clone(),
                            size: a.size,
                            published_at: r.published_at.clone(),
                            prerelease: r.prerelease,
                            html_url: r.html_url.clone(),
                        })
                    }
                    None if error.is_none() => {
                        error = Some(SourceError::new(
                            "no_matching_file",
                            format!(
                                "None of the newest releases has a file named like {}.",
                                name_pattern(&repo.file_name)
                            ),
                        ))
                    }
                    None => {}
                }
            }
        }
        let update_available = match (&have, &latest) {
            (Some(h), Some(l)) => {
                is_newer(&l.tag, &h.tag)
                    || (l.tag == h.tag
                        && (!l.file_name.eq_ignore_ascii_case(&h.file_name)
                            || (l.size > 0 && l.size != h.size)))
            }
            _ => false,
        };
        UserSource {
            id: repo.id.clone(),
            name: repo.display_name.clone(),
            kind: kind_of(&p),
            project_url: p.project_url(),
            owner: repo.repo_owner.clone(),
            repo: repo.repo_name.clone(),
            file_pattern: if repo.file_name.is_empty() {
                String::new()
            } else {
                name_pattern(&repo.file_name)
            },
            file_name: repo.file_name.clone(),
            have,
            latest,
            update_available,
            checked_age_secs,
            error,
        }
    }

    /// The sources the user added. `check`: read each one's releases (a list from the last ten
    /// minutes is used as it is, unless `force`). Without `check` nothing asks the network: the
    /// last list read is shown when it is from the last ten minutes. Once a host answers with its
    /// rate limit or cannot be reached, the other sources on it are not asked in this pass.
    pub fn user_sources(&self, check: bool, force: bool) -> Result<Vec<UserSource>, SourceError> {
        let repos = self.read_repo_file()?.repos;
        let mut host_errors: HashMap<String, SourceError> = HashMap::new();
        let mut out = Vec::with_capacity(repos.len());
        for repo in &repos {
            if repo.file_name.is_empty() {
                out.push(self.source_view(repo, None, None));
                continue;
            }
            if !check {
                let list = self.cached_release_list(&repo.id, true);
                out.push(self.source_view(repo, list, None));
                continue;
            }
            let host = repo.host_and_port();
            let (list, error) = self.source_release_list(repo, force, host_errors.get(&host));
            if let Some(e) = &error {
                if matches!(e.code.as_str(), "rate_limited" | "unreachable") {
                    host_errors.entry(host).or_insert_with(|| e.clone());
                }
            }
            out.push(self.source_view(repo, list, error));
        }
        Ok(out)
    }

    /// One source, without asking the network.
    pub fn user_source(&self, id: &str) -> Result<UserSource, SourceError> {
        let repo = self.find_source(id)?;
        let list = self.cached_release_list(&repo.id, true);
        Ok(self.source_view(&repo, list, None))
    }

    /// A project's newest releases with `.elf` and `.bin` files, for picking the file. A project
    /// already added uses its list from the last ten minutes.
    pub fn preview_user_source(&self, url: &str) -> Result<SourcePreview, SourceError> {
        let p = parse_project_url(url)?;
        let id = p.id();
        let existing = self
            .read_repo_file()?
            .repos
            .into_iter()
            .find(|r| r.id == id);
        let list = match &existing {
            Some(repo) => match self.source_release_list(repo, false, None) {
                (_, Some(e)) => return Err(e),
                (Some((list, _)), None) => list,
                (None, None) => Vec::new(),
            },
            None => self.fetch_release_list(&p)?,
        };
        let releases = list
            .iter()
            .filter_map(|r| {
                let files: Vec<PreviewFile> = r
                    .assets
                    .iter()
                    .filter(|a| is_payload_file_name(&a.name))
                    .map(|a| PreviewFile {
                        name: a.name.clone(),
                        size: a.size,
                        pattern: name_pattern(&a.name),
                    })
                    .collect();
                (!files.is_empty()).then(|| PreviewRelease {
                    tag: r.tag_name.clone(),
                    name: if r.name.is_empty() {
                        r.tag_name.clone()
                    } else {
                        r.name.clone()
                    },
                    published_at: r.published_at.clone(),
                    prerelease: r.prerelease,
                    html_url: r.html_url.clone(),
                    files,
                })
            })
            .collect();
        Ok(SourcePreview {
            id,
            name: existing
                .as_ref()
                .map(|r| r.display_name.clone())
                .unwrap_or_else(|| p.name.clone()),
            kind: kind_of(&p),
            project_url: p.project_url(),
            already_added: existing.is_some(),
            current_file: existing.map(|r| r.file_name).unwrap_or_default(),
            releases,
        })
    }

    /// Add a project as a payload source with the file the user picked, or change the file of one
    /// added already. Asks no network: the next check reads its releases.
    pub fn add_user_source(&self, url: &str, file_name: &str) -> Result<UserSource, SourceError> {
        let p = parse_project_url(url)?;
        let file_name = checked_file_name(file_name)?;
        let id = p.id();
        let mut file = self.read_repo_file()?;
        let same = |r: &CustomRepo| {
            r.id == id
                || (r.repo_host == p.host
                    && r.repo_port == p.port
                    && r.repo_owner.eq_ignore_ascii_case(&p.owner)
                    && r.repo_name.eq_ignore_ascii_case(&p.name))
        };
        let repo = match file.repos.iter_mut().find(|r| same(r)) {
            Some(r) => {
                r.file_name = file_name;
                r.clone()
            }
            None => {
                let r = CustomRepo {
                    id,
                    display_name: p.name.clone(),
                    repo_host: p.host.clone(),
                    repo_owner: p.owner.clone(),
                    repo_name: p.name.clone(),
                    asset_name_hint: String::new(),
                    repo_port: p.port,
                    file_name,
                };
                file.repos.push(r.clone());
                r
            }
        };
        self.save_repo_file(&file)?;
        let list = self.cached_release_list(&repo.id, true);
        Ok(self.source_view(&repo, list, None))
    }

    /// Change the file a source downloads (or pick it, for an entry from before 12.6.0). The copy
    /// here stays until the next download replaces it.
    pub fn set_user_source_file(
        &self,
        id: &str,
        file_name: &str,
    ) -> Result<UserSource, SourceError> {
        let file_name = checked_file_name(file_name)?;
        self.find_source(id)?;
        let mut file = self.read_repo_file()?;
        let Some(repo) = file.repos.iter_mut().find(|r| r.id == id) else {
            return Err(SourceError::new(
                "unknown_source",
                format!("no payload source {id}"),
            ));
        };
        repo.file_name = file_name;
        let repo = repo.clone();
        self.save_repo_file(&file)?;
        let list = self.cached_release_list(&repo.id, true);
        Ok(self.source_view(&repo, list, None))
    }

    /// Download the newest release's file for a source into its folder: through a temporary file,
    /// checked against the size the release gives and against the release's checksum file when it
    /// has one for it, then renamed into place. The copy it replaces stays if anything fails.
    pub fn download_user_source(&self, id: &str) -> Result<UserSource, SourceError> {
        let repo = self.find_source(id)?;
        if repo.file_name.is_empty() {
            return Err(SourceError::new(
                "needs_file",
                "Pick the file this source downloads first.",
            ));
        }
        let p = ProjectRef::of_repo(&repo);
        let list = match self.source_release_list(&repo, false, None) {
            (Some((list, _)), _) => list,
            (None, Some(e)) => return Err(e),
            (None, None) => Vec::new(),
        };
        let Some((release, asset)) = newest_match(&list, &repo.file_name) else {
            return Err(SourceError::new(
                "no_matching_file",
                format!(
                    "None of the newest releases has a file named like {}.",
                    name_pattern(&repo.file_name)
                ),
            ));
        };
        // Only a plain .elf or .bin name is ever picked (`file_in_release`), so it is safe as a
        // file name in the source's folder; checked again here, where it becomes one.
        if !is_payload_file_name(&asset.name) {
            return Err(SourceError::new(
                "asset_not_payload",
                format!("{} isn't a .elf or .bin file name.", asset.name),
            ));
        }
        let entry = p.entry();
        let dir = self.dir.join(&repo.id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| SourceError::new("io_failed", format!("mkdir {}: {e}", dir.display())))?;
        sweep_stale_parts(&dir);

        // The checksum first: a file that lists it but can't be read fails before the download.
        let expected = match checksum_asset(release, &asset.name) {
            Some((sums, own)) => {
                let text = self.read_checksum_file(&entry, &sums.browser_download_url)?;
                expected_sha256(&text, &asset.name, own)
            }
            None => None,
        };

        let resp = fetch(
            self.http.as_ref(),
            &entry,
            &asset.browser_download_url,
            None,
            FetchKind::Download,
        )?;
        if !(200..300).contains(&resp.status) {
            return Err(SourceError::new(
                "fetch_failed",
                format!(
                    "{} answered HTTP {} for {}.",
                    host_label(&p),
                    resp.status,
                    asset.name
                ),
            ));
        }
        let too_large = |n: u64| {
            SourceError::new(
                "asset_too_large",
                format!(
                    "{} is too large ({n} bytes; the most is {SOURCE_DOWNLOAD_MAX_BYTES}).",
                    asset.name
                ),
            )
        };
        if let Some(n) = resp
            .content_length
            .filter(|n| *n > SOURCE_DOWNLOAD_MAX_BYTES)
        {
            return Err(too_large(n));
        }
        if asset.size > SOURCE_DOWNLOAD_MAX_BYTES {
            return Err(too_large(asset.size));
        }

        let part = TmpFile(dir.join(format!(".{}.{}.part", asset.name, unique_suffix())));
        let mut hasher = Sha256::new();
        let mut written: u64 = 0;
        let mut head = [0u8; 4];
        let mut head_len = 0usize;
        {
            let mut out = std::fs::File::create(&part.0).map_err(|e| {
                SourceError::new("io_failed", format!("create {}: {e}", part.0.display()))
            })?;
            let mut body = resp.body;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = body.read(&mut buf).map_err(|e| {
                    SourceError::new(
                        "download_cut_short",
                        format!(
                            "The download of {} stopped after {written} bytes ({e}). Try again.",
                            asset.name
                        ),
                    )
                })?;
                if n == 0 {
                    break;
                }
                written = written.saturating_add(n as u64);
                if written > SOURCE_DOWNLOAD_MAX_BYTES {
                    return Err(too_large(written));
                }
                hasher.update(&buf[..n]);
                let take = (head.len() - head_len).min(n);
                head[head_len..head_len + take].copy_from_slice(&buf[..take]);
                head_len += take;
                out.write_all(&buf[..n]).map_err(|e| {
                    SourceError::new("io_failed", format!("write {}: {e}", part.0.display()))
                })?;
            }
            out.sync_all().map_err(|e| {
                SourceError::new("io_failed", format!("fsync {}: {e}", part.0.display()))
            })?;
        }
        let want = resp
            .content_length
            .or((asset.size > 0).then_some(asset.size));
        if let Some(total) = want.filter(|t| *t != written) {
            return Err(SourceError::new(
                "download_cut_short",
                format!(
                    "The download of {} stopped after {written} of {total} bytes. Try again.",
                    asset.name
                ),
            ));
        }
        if asset.size > 0 && asset.size != written {
            return Err(SourceError::new(
                "download_cut_short",
                format!(
                    "{} is {written} bytes, but the release lists {} bytes. Try again.",
                    asset.name, asset.size
                ),
            ));
        }
        let got: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let checksum = match &expected {
            Some(want) if *want != got => {
                return Err(SourceError::new(
                    "checksum_mismatch",
                    format!(
                        "{} doesn't match the checksum its release lists (got {got}, expected \
                         {want}). It was not kept.",
                        asset.name
                    ),
                ))
            }
            Some(_) => "matched",
            None => "none",
        };
        // The same rule a send applies: an .elf must be one (not an error page saved under its
        // name), a .bin must not be empty.
        let is_elf_name = asset.name.to_ascii_lowercase().ends_with(".elf");
        if written == 0 || (is_elf_name && head[..head_len] != *b"\x7FELF") {
            return Err(SourceError::new(
                "asset_not_payload",
                format!(
                    "{} isn't a payload ({}). It was not kept.",
                    asset.name,
                    if written == 0 {
                        "it is empty"
                    } else {
                        "not an ELF file"
                    }
                ),
            ));
        }
        let final_path = dir.join(&asset.name);
        replace_file(&part.0, &final_path).map_err(|e| {
            SourceError::new(
                "io_failed",
                format!("rename into {}: {e}", final_path.display()),
            )
        })?;
        remove_other_payload_files(&dir, &asset.name);
        let record = DownloadRecord {
            tag: release.tag_name.clone(),
            file_name: asset.name.clone(),
            published_at: release.published_at.clone(),
            checksum: checksum.to_string(),
            downloaded_at: now_secs() as i64,
        };
        if let Ok(body) = serde_json::to_vec_pretty(&record) {
            atomic_write(&dir.join(DOWNLOAD_FILE), &body)
                .map_err(|e| SourceError::new("io_failed", format!("write download.json: {e}")))?;
        }
        let _ = atomic_write(&dir.join("VERSION"), release.tag_name.as_bytes());
        crate::log::log(&format!(
            "[payloads] source {}: downloaded {} {} ({written} bytes, checksum {checksum})",
            repo.id, asset.name, release.tag_name
        ));
        let list = self.cached_release_list(&repo.id, false);
        Ok(self.source_view(&repo, list, None))
    }

    fn read_checksum_file(&self, entry: &ResolvedEntry, url: &str) -> Result<String, SourceError> {
        let unreadable = |why: String| {
            SourceError::new(
                "checksum_unreadable",
                format!("The release's checksum file couldn't be read ({why})."),
            )
        };
        let resp = fetch(self.http.as_ref(), entry, url, None, FetchKind::Metadata)
            .map_err(|e| unreadable(e.message))?;
        if !(200..300).contains(&resp.status) {
            return Err(unreadable(format!("HTTP {}", resp.status)));
        }
        if resp
            .content_length
            .is_some_and(|n| n > CHECKSUM_FILE_MAX_BYTES)
        {
            return Err(unreadable("too large".into()));
        }
        let mut bytes = Vec::new();
        resp.body
            .take(CHECKSUM_FILE_MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| unreadable(e.to_string()))?;
        if bytes.len() as u64 > CHECKSUM_FILE_MAX_BYTES {
            return Err(unreadable("too large".into()));
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

fn checked_file_name(name: &str) -> Result<String, SourceError> {
    let name = name.trim();
    if !is_payload_file_name(name) {
        return Err(SourceError::new(
            "invalid_file_name",
            "Pick a .elf or .bin file from the project's releases.",
        ));
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::super::http::hop_allowed;
    use super::super::test_support::{library, Scripted};
    use super::*;

    const GH_API: &str = "https://api.github.com/repos/me/kstuff/releases?per_page=10";
    const GH_URL: &str = "https://github.com/me/kstuff";

    fn elf(fill: u8, len: usize) -> Vec<u8> {
        let mut v = b"\x7FELF".to_vec();
        v.resize(len, fill);
        v
    }

    fn sha_hex(b: &[u8]) -> String {
        Sha256::digest(b)
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect()
    }

    fn asset_json(name: &str, url: &str, size: usize) -> serde_json::Value {
        serde_json::json!({ "name": name, "browser_download_url": url, "size": size })
    }

    fn release_json(tag: &str, assets: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "tag_name": tag, "name": format!("kstuff {tag}"), "body": null,
            "published_at": "2026-11-01T00:00:00Z", "prerelease": false, "draft": false,
            "html_url": format!("{GH_URL}/releases/tag/{tag}"), "assets": assets,
        })
    }

    fn dl(tag: &str, name: &str) -> String {
        format!("{GH_URL}/releases/download/{tag}/{name}")
    }

    /// A release list with one release carrying `name` (and the body served at its URL).
    fn serve_one(http: &Scripted, tag: &str, name: &str, body: &[u8]) {
        let list = serde_json::json!([release_json(
            tag,
            vec![asset_json(name, &dl(tag, name), body.len())]
        )]);
        http.ok(GH_API, list.to_string().as_bytes());
        http.ok(&dl(tag, name), body);
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|r| {
                r.flatten()
                    .map(|f| f.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    #[test]
    fn github_addresses_are_read_in_their_usual_forms() {
        for url in [
            "https://github.com/me/kstuff",
            "https://github.com/me/kstuff/",
            "https://www.github.com/me/kstuff",
            "https://github.com/me/kstuff.git",
            "https://github.com/me/kstuff/releases/tag/v1.0",
            "github.com/me/kstuff",
            "  https://GitHub.com/me/kstuff?tab=readme#top  ",
        ] {
            let p = parse_project_url(url).unwrap_or_else(|e| panic!("{url}: {e}"));
            assert_eq!(
                (p.host.as_str(), p.port, p.owner.as_str(), p.name.as_str()),
                ("github.com", None, "me", "kstuff"),
                "{url}"
            );
            assert_eq!(p.project_url(), "https://github.com/me/kstuff");
        }
    }

    #[test]
    fn the_id_is_the_one_earlier_versions_minted() {
        let (_d, _h, lib) = library("same-id");
        let old = super::super::repos::test_repos::save_custom_repo(&lib, "me", "kstuff", "old");
        let p = parse_project_url("https://github.com/Me/KStuff").unwrap();
        assert_eq!(p.id(), old.id);
        // Adding it again completes the old entry instead of making a second one.
        let s = lib.add_user_source(GH_URL, "kstuff.elf").unwrap();
        assert_eq!(s.id, old.id);
        assert_eq!(s.name, "old");
        assert_eq!(lib.user_sources(false, false).unwrap().len(), 1);
    }

    #[test]
    fn address_rules() {
        let code = |url: &str| parse_project_url(url).err().map(|e| e.code);
        assert_eq!(
            code("http://github.com/me/kstuff").as_deref(),
            Some("url_not_https")
        );
        assert_eq!(
            code("ftp://example.org/me/k").as_deref(),
            Some("url_not_https")
        );
        assert_eq!(
            code("https://user:pw@github.com/me/kstuff").as_deref(),
            Some("url_has_credentials")
        );
        assert_eq!(
            code("https://token@git.example.org/me/k").as_deref(),
            Some("url_has_credentials")
        );
        for bad in [
            "https://github.com/me",
            "https://github.com/",
            "https://github.com:8443/me/kstuff",
            "https://api.github.com/repos/me/kstuff",
            "https://raw.githubusercontent.com/me/kstuff/main/x.elf",
            "https://me.github.io/kstuff",
            "https://github.com/me/../x",
            "https://github.com/me/kst uff",
            "",
        ] {
            assert_eq!(code(bad).as_deref(), Some("url_not_a_project"), "{bad}");
        }
        assert_eq!(
            code("https://gitlab.com/me/k").as_deref(),
            Some("url_gitlab")
        );
        // Never this computer or a link-local address, whatever the kind.
        for bad in [
            "https://127.0.0.1/me/k",
            "https://127.0.0.1:19113/me/k",
            "https://localhost:3000/me/k",
            "https://gitea.localhost/me/k",
            "https://[::1]:3000/me/k",
            "https://169.254.169.254/me/k",
            "https://[fe80::1]/me/k",
            "https://0.0.0.0/me/k",
            "https://[::ffff:127.0.0.1]/me/k",
        ] {
            assert_eq!(code(bad).as_deref(), Some("url_host_not_allowed"), "{bad}");
        }
    }

    #[test]
    fn a_gitea_server_may_be_on_the_home_network_and_its_own_port() {
        let p = parse_project_url("https://192.168.1.5:3000/me/kstuff").unwrap();
        assert_eq!((p.host.as_str(), p.port), ("192.168.1.5", Some(3000)));
        assert!(!p.is_github());
        assert_eq!(p.project_url(), "https://192.168.1.5:3000/me/kstuff");
        assert_eq!(
            releases_api_url(&p),
            "https://192.168.1.5:3000/api/v1/repos/me/kstuff/releases?limit=10"
        );
        for ok in [
            "https://nas.local/me/k",
            "https://gitea/me/k",
            "https://10.0.0.2/me/k",
            "https://[fd00::5]:3000/me/k",
            "https://codeberg.org/me/k",
            "https://git.example.org:443/me/k",
        ] {
            assert!(parse_project_url(ok).is_ok(), "{ok}");
        }
        assert_eq!(
            parse_project_url("https://git.example.org:443/me/k")
                .unwrap()
                .port,
            None
        );
    }

    #[test]
    fn redirects_follow_the_same_rules() {
        let gh = parse_project_url(GH_URL).unwrap().entry();
        assert!(hop_allowed(
            &gh,
            "https://github.com/me/kstuff/releases/download/v1/k.elf"
        ));
        assert!(hop_allowed(
            &gh,
            "https://release-assets.githubusercontent.com/x?sig=1"
        ));
        assert!(hop_allowed(&gh, "https://objects.githubusercontent.com/x"));
        assert!(!hop_allowed(&gh, "https://evil.example/x"));
        assert!(!hop_allowed(&gh, "https://192.168.1.5/x"));
        assert!(!hop_allowed(&gh, "https://user@github.com/x"));

        let lan = parse_project_url("https://192.168.1.5:3000/me/k")
            .unwrap()
            .entry();
        assert!(hop_allowed(
            &lan,
            "https://192.168.1.5:3000/attachments/abc"
        ));
        assert!(hop_allowed(&lan, "https://s3.example.net/bucket/a.elf"));
        for bad in [
            "http://192.168.1.5:3000/attachments/abc",
            "https://192.168.1.5:22/x",
            "https://192.168.1.6:3000/x",
            "https://127.0.0.1:3000/x",
            "https://localhost:3000/x",
            "https://169.254.169.254/latest/meta-data",
            "https://nas.local/x",
        ] {
            assert!(!hop_allowed(&lan, bad), "{bad}");
        }
    }

    #[test]
    fn the_file_name_rule() {
        assert_eq!(name_pattern("kstuff-v1.6.0.elf"), "kstuff-v*.elf");
        assert_eq!(name_pattern("kstuff.elf"), "kstuff.elf");
        assert_eq!(name_pattern("ftpsrv-ps5.elf"), "ftpsrv-ps5.elf");
        assert_eq!(
            name_pattern("ps5-kstuff-1.13-fpkg.elf"),
            "ps5-kstuff-*-fpkg.elf"
        );
        assert_eq!(name_pattern("etaHEN-2.4b.bin"), "etaHEN-*.bin");
        assert_eq!(name_pattern("payload_20240101.elf"), "payload_*.elf");
        assert_eq!(name_pattern("1.0-thing.elf"), "*-thing.elf");

        assert!(names_match("kstuff-v1.6.0.elf", "kstuff-v1.7.elf"));
        assert!(names_match("kstuff-v1.6.0.elf", "KSTUFF-v2.0.0.ELF"));
        assert!(names_match("etaHEN-2.4b.bin", "etaHEN-2.5.bin"));
        assert!(names_match("kstuff.elf", "kstuff.elf"));
        assert!(!names_match("kstuff.elf", "kstuff-1.0.elf"));
        assert!(!names_match("kstuff-v1.6.0.elf", "kstuff-v1.6.0-debug.elf"));
        assert!(!names_match("kstuff-v1.6.0.elf", "kstuff-v1.6.0.bin"));
        assert!(!names_match("ftpsrv-ps5.elf", "ftpsrv-ps4.elf"));
        assert!(!names_match("kstuff-1.0.elf", "kstuff-x.elf"));

        assert!(is_payload_file_name("kstuff.elf"));
        assert!(is_payload_file_name("etaHEN.BIN"));
        for bad in [
            "kstuff.zip",
            ".elf",
            "../x.elf",
            "a/b.elf",
            "a\\b.elf",
            "C:x.elf",
            "x%2f.elf",
            "",
        ] {
            assert!(!is_payload_file_name(bad), "{bad}");
        }
    }

    #[test]
    fn checksum_files_in_their_common_forms() {
        let h = "a".repeat(64);
        let want = Some(h.clone());
        assert_eq!(
            expected_sha256(&format!("{h}  kstuff.elf\n"), "kstuff.elf", false),
            want
        );
        assert_eq!(
            expected_sha256(&format!("{h} *dist/kstuff.elf"), "kstuff.elf", false),
            want
        );
        assert_eq!(
            expected_sha256(
                &format!("SHA256 (kstuff.elf) = {}", h.to_uppercase()),
                "kstuff.elf",
                false
            ),
            want
        );
        assert_eq!(expected_sha256(&format!("{h}\n"), "kstuff.elf", true), want);
        assert_eq!(
            expected_sha256(&format!("{h}  other.elf"), "kstuff.elf", false),
            None
        );
        assert_eq!(
            expected_sha256(&format!("{h}\n"), "kstuff.elf", false),
            None
        );
        assert_eq!(expected_sha256("nothing here", "kstuff.elf", true), None);
    }

    #[test]
    fn add_check_and_download_from_a_github_project() {
        let (_d, http, lib) = library("src-flow");
        let body = elf(1, 300);
        serve_one(&http, "v1.6.0", "kstuff-v1.6.0.elf", &body);

        let preview = lib.preview_user_source(GH_URL).unwrap();
        assert!(!preview.already_added);
        assert_eq!(preview.kind, "github");
        assert_eq!(preview.releases.len(), 1);
        assert_eq!(preview.releases[0].files[0].pattern, "kstuff-v*.elf");
        assert!(
            !lib.dir().join(&preview.id).exists(),
            "a preview saves nothing"
        );

        let added = lib.add_user_source(GH_URL, "kstuff-v1.6.0.elf").unwrap();
        assert_eq!(added.file_pattern, "kstuff-v*.elf");
        assert!(added.have.is_none() && added.latest.is_none());

        let listed = lib.user_sources(true, false).unwrap();
        let latest = listed[0].latest.as_ref().unwrap();
        assert_eq!((latest.tag.as_str(), latest.size), ("v1.6.0", 300));
        assert!(!listed[0].update_available);

        let got = lib.download_user_source(&added.id).unwrap();
        let have = got.have.unwrap();
        assert_eq!(have.tag, "v1.6.0");
        assert_eq!(have.file_name, "kstuff-v1.6.0.elf");
        assert_eq!(have.size, 300);
        assert_eq!(have.sha256, sha_hex(&body));
        assert_eq!(have.checksum, "none");
        assert_eq!(std::fs::read(&have.path).unwrap(), body);
        assert_eq!(
            files_in(&lib.dir().join(&added.id)),
            [
                "VERSION",
                "download.json",
                "kstuff-v1.6.0.elf",
                "source-releases.json"
            ]
        );
        // The library's own lookups see it too: a send by id uses it.
        assert_eq!(
            lib.local_path(&added.id).unwrap().to_string_lossy(),
            have.path
        );
    }

    #[test]
    fn checks_use_the_last_ten_minutes_unless_forced() {
        let (_d, http, lib) = library("src-cache");
        serve_one(&http, "v1", "k.elf", &elf(1, 10));
        lib.add_user_source(GH_URL, "k.elf").unwrap();
        let asked = || {
            http.asked
                .lock()
                .unwrap()
                .iter()
                .filter(|u| u.as_str() == GH_API)
                .count()
        };
        lib.user_sources(true, false).unwrap();
        lib.user_sources(true, false).unwrap();
        assert_eq!(asked(), 1);
        // Listing without a check never asks, and still shows the fresh list.
        let quiet = lib.user_sources(false, false).unwrap();
        assert!(quiet[0].latest.is_some());
        assert_eq!(asked(), 1);
        lib.user_sources(true, true).unwrap();
        assert_eq!(asked(), 2);
    }

    #[test]
    fn a_rate_limit_says_when_and_the_host_is_not_asked_again() {
        let (_d, http, lib) = library("src-limit");
        http.limited(GH_API, 403, 1_900_000_000);
        let other = "https://api.github.com/repos/me/other/releases?per_page=10";
        http.ok(other, b"[]");
        lib.add_user_source(GH_URL, "k.elf").unwrap();
        lib.add_user_source("https://github.com/me/other", "o.elf")
            .unwrap();
        let listed = lib.user_sources(true, true).unwrap();
        for s in &listed {
            let e = s.error.as_ref().unwrap();
            assert_eq!(e.code, "rate_limited", "{}", s.id);
            assert_eq!(e.retry_at, Some(1_900_000_000));
        }
        assert!(!http.asked.lock().unwrap().iter().any(|u| u == other));

        // The add step says the same.
        let e = lib.preview_user_source(GH_URL).unwrap_err();
        assert_eq!(
            (e.code.as_str(), e.retry_at),
            ("rate_limited", Some(1_900_000_000))
        );

        // A 429 without a time is a rate limit too; a plain 403 is not.
        http.status(GH_API, 429);
        http.retry_at.lock().unwrap().clear();
        let e = lib.preview_user_source(GH_URL).unwrap_err();
        assert_eq!((e.code.as_str(), e.retry_at), ("rate_limited", None));
        http.status(GH_API, 403);
        assert_eq!(
            lib.preview_user_source(GH_URL).unwrap_err().code,
            "fetch_failed"
        );
        http.status(GH_API, 404);
        assert_eq!(
            lib.preview_user_source(GH_URL).unwrap_err().code,
            "project_not_found"
        );
        http.unreachable(GH_API);
        assert_eq!(
            lib.preview_user_source(GH_URL).unwrap_err().code,
            "unreachable"
        );
    }

    #[test]
    fn a_failed_check_keeps_showing_the_last_list() {
        let (_d, http, lib) = library("src-stale");
        serve_one(&http, "v1", "k.elf", &elf(1, 10));
        lib.add_user_source(GH_URL, "k.elf").unwrap();
        lib.user_sources(true, false).unwrap();
        http.limited(GH_API, 429, 1_900_000_000);
        let s = &lib.user_sources(true, true).unwrap()[0];
        assert_eq!(s.latest.as_ref().unwrap().tag, "v1");
        assert_eq!(s.error.as_ref().unwrap().code, "rate_limited");
    }

    #[test]
    fn a_release_without_the_file_is_passed_over() {
        let (_d, http, lib) = library("src-missing");
        let body = elf(2, 64);
        let list = serde_json::json!([
            // Newest: only a debug build and the source.
            release_json(
                "v2",
                vec![
                    asset_json("kstuff-v2-debug.elf", &dl("v2", "kstuff-v2-debug.elf"), 64),
                    asset_json("source.zip", &dl("v2", "source.zip"), 64),
                ]
            ),
            serde_json::json!({ "tag_name": "v3-draft", "draft": true, "assets": [
                asset_json("kstuff-v3.elf", &dl("v3", "kstuff-v3.elf"), 64)] }),
            release_json(
                "v1",
                vec![asset_json("kstuff-v1.elf", &dl("v1", "kstuff-v1.elf"), 64)]
            ),
        ]);
        http.ok(GH_API, list.to_string().as_bytes());
        http.ok(&dl("v1", "kstuff-v1.elf"), &body);
        let s = lib.add_user_source(GH_URL, "kstuff-v1.elf").unwrap();
        let listed = lib.user_sources(true, false).unwrap();
        assert_eq!(listed[0].latest.as_ref().unwrap().tag, "v1");
        assert!(listed[0].error.is_none());
        assert_eq!(
            lib.download_user_source(&s.id).unwrap().have.unwrap().tag,
            "v1"
        );

        // Picked a name no release has: said plainly, nothing downloaded.
        let s = lib.set_user_source_file(&s.id, "other.elf").unwrap();
        let listed = lib.user_sources(true, false).unwrap();
        let e = listed[0].error.as_ref().unwrap();
        assert_eq!(e.code, "no_matching_file");
        assert!(e.message.contains("other.elf"), "{}", e.message);
        assert_eq!(
            lib.download_user_source(&s.id).unwrap_err().code,
            "no_matching_file"
        );
    }

    #[test]
    fn a_renamed_file_that_matches_is_the_update() {
        let (_d, http, lib) = library("src-renamed");
        let v1 = elf(1, 40);
        serve_one(&http, "v1.6.0", "kstuff-v1.6.0.elf", &v1);
        let s = lib.add_user_source(GH_URL, "kstuff-v1.6.0.elf").unwrap();
        lib.download_user_source(&s.id).unwrap();

        let v2 = elf(2, 50);
        serve_one(&http, "v1.7.0", "kstuff-v1.7.0.elf", &v2);
        let listed = lib.user_sources(true, true).unwrap();
        assert!(listed[0].update_available);
        assert_eq!(
            listed[0].latest.as_ref().unwrap().file_name,
            "kstuff-v1.7.0.elf"
        );

        let got = lib.download_user_source(&s.id).unwrap();
        let have = got.have.unwrap();
        assert_eq!(
            (have.tag.as_str(), have.file_name.as_str()),
            ("v1.7.0", "kstuff-v1.7.0.elf")
        );
        assert!(!got.update_available);
        // The old file is gone: one payload per source.
        let files = files_in(&lib.dir().join(&s.id));
        assert!(files.contains(&"kstuff-v1.7.0.elf".to_string()));
        assert!(!files.contains(&"kstuff-v1.6.0.elf".to_string()));
    }

    #[test]
    fn a_checksum_file_is_checked() {
        let (_d, http, lib) = library("src-sums");
        let body = elf(3, 77);
        let list = serde_json::json!([release_json(
            "v1",
            vec![
                asset_json("k.elf", &dl("v1", "k.elf"), body.len()),
                asset_json("SHA256SUMS", &dl("v1", "SHA256SUMS"), 100),
            ]
        )]);
        http.ok(GH_API, list.to_string().as_bytes());
        http.ok(&dl("v1", "k.elf"), &body);
        http.ok(
            &dl("v1", "SHA256SUMS"),
            format!("{}  k.elf\n", sha_hex(&body)).as_bytes(),
        );
        let s = lib.add_user_source(GH_URL, "k.elf").unwrap();
        assert_eq!(
            lib.download_user_source(&s.id)
                .unwrap()
                .have
                .unwrap()
                .checksum,
            "matched"
        );

        // A file that doesn't match its checksum is not kept, and the copy here stays.
        let mut tampered = body.clone();
        tampered[10] ^= 0xff;
        http.ok(&dl("v1", "k.elf"), &tampered);
        let e = lib.download_user_source(&s.id).unwrap_err();
        assert_eq!(e.code, "checksum_mismatch");
        let have = lib.user_source(&s.id).unwrap().have.unwrap();
        assert_eq!(have.sha256, sha_hex(&body));
        assert!(!files_in(&lib.dir().join(&s.id))
            .iter()
            .any(|f| f.ends_with(".part")));

        // A checksum file that can't be read stops the download before it starts.
        http.status(&dl("v1", "SHA256SUMS"), 500);
        assert_eq!(
            lib.download_user_source(&s.id).unwrap_err().code,
            "checksum_unreadable"
        );
    }

    #[test]
    fn a_files_own_checksum_file_counts() {
        let (_d, http, lib) = library("src-own-sum");
        let body = elf(4, 33);
        let list = serde_json::json!([release_json(
            "v1",
            vec![
                asset_json("k.elf", &dl("v1", "k.elf"), body.len()),
                asset_json("k.elf.sha256", &dl("v1", "k.elf.sha256"), 64),
            ]
        )]);
        http.ok(GH_API, list.to_string().as_bytes());
        http.ok(&dl("v1", "k.elf"), &body);
        http.ok(&dl("v1", "k.elf.sha256"), sha_hex(&body).as_bytes());
        let s = lib.add_user_source(GH_URL, "k.elf").unwrap();
        assert_eq!(
            lib.download_user_source(&s.id)
                .unwrap()
                .have
                .unwrap()
                .checksum,
            "matched"
        );
    }

    #[test]
    fn a_download_cut_short_leaves_nothing_and_keeps_the_old_copy() {
        let (_d, http, lib) = library("src-short");
        let v1 = elf(1, 40);
        serve_one(&http, "v1", "k.elf", &v1);
        let s = lib.add_user_source(GH_URL, "k.elf").unwrap();
        lib.download_user_source(&s.id).unwrap();

        // The answer claims 100 bytes and ends after 60.
        let list = serde_json::json!([release_json(
            "v2",
            vec![asset_json("k.elf", &dl("v2", "k.elf"), 100)]
        )]);
        http.ok(GH_API, list.to_string().as_bytes());
        http.cut_short(&dl("v2", "k.elf"), &elf(2, 60), 100);
        lib.user_sources(true, true).unwrap();
        let e = lib.download_user_source(&s.id).unwrap_err();
        assert_eq!(e.code, "download_cut_short");
        assert!(e.message.contains("60 of 100"), "{}", e.message);
        // The release's own size counts when the answer agrees with the short body.
        http.cut_short(&dl("v2", "k.elf"), &elf(2, 60), 60);
        assert_eq!(
            lib.download_user_source(&s.id).unwrap_err().code,
            "download_cut_short"
        );

        let dir = lib.dir().join(&s.id);
        assert!(
            !files_in(&dir).iter().any(|f| f.ends_with(".part")),
            "{:?}",
            files_in(&dir)
        );
        let have = lib.user_source(&s.id).unwrap().have.unwrap();
        assert_eq!((have.tag.as_str(), have.sha256), ("v1", sha_hex(&v1)));
    }

    #[test]
    fn an_error_page_is_not_kept_as_a_payload() {
        let (_d, http, lib) = library("src-html");
        serve_one(&http, "v1", "k.elf", b"<html>Not Found</html>");
        let s = lib.add_user_source(GH_URL, "k.elf").unwrap();
        assert_eq!(
            lib.download_user_source(&s.id).unwrap_err().code,
            "asset_not_payload"
        );
        assert!(lib.user_source(&s.id).unwrap().have.is_none());
        // A .bin only has to have something in it.
        serve_one(&http, "v1", "k.bin", b"\x01\x02");
        lib.user_sources(true, true).unwrap();
        let s = lib.set_user_source_file(&s.id, "k.bin").unwrap();
        let have = lib.download_user_source(&s.id).unwrap().have.unwrap();
        assert_eq!(have.file_name, "k.bin");
        assert_eq!(lib.local_path(&s.id).unwrap().to_string_lossy(), have.path);
    }

    #[test]
    fn a_download_through_githubs_file_host() {
        let (_d, http, lib) = library("src-cdn");
        let body = elf(5, 20);
        let list = serde_json::json!([release_json(
            "v1",
            vec![asset_json("k.elf", &dl("v1", "k.elf"), body.len())]
        )]);
        http.ok(GH_API, list.to_string().as_bytes());
        let cdn =
            "https://release-assets.githubusercontent.com/github-production-release-asset/1?sig=x";
        http.redirect(&dl("v1", "k.elf"), cdn);
        http.ok(cdn, &body);
        let s = lib.add_user_source(GH_URL, "k.elf").unwrap();
        assert_eq!(
            lib.download_user_source(&s.id).unwrap().have.unwrap().size,
            20
        );

        // A redirect anywhere else is refused before it is asked.
        http.redirect(&dl("v1", "k.elf"), "https://192.168.1.1/x.elf");
        lib.user_sources(true, true).unwrap();
        assert_eq!(
            lib.download_user_source(&s.id).unwrap_err().code,
            "redirect_refused"
        );
        assert!(!http
            .asked
            .lock()
            .unwrap()
            .iter()
            .any(|u| u.contains("192.168.1.1")));
    }

    #[test]
    fn a_gitea_server_on_the_lan() {
        let (_d, http, lib) = library("src-gitea");
        let base = "https://192.168.1.5:3000";
        let api = format!("{base}/api/v1/repos/me/k/releases?limit=10");
        let file = format!("{base}/me/k/releases/download/v1/k.elf");
        let body = elf(6, 12);
        let list = serde_json::json!([{
            "tag_name": "v1", "name": "", "published_at": "2026-11-01T00:00:00Z",
            "prerelease": true, "draft": false, "html_url": format!("{base}/me/k/releases/tag/v1"),
            "assets": [ { "name": "k.elf", "browser_download_url": file, "size": 12 } ],
        }]);
        http.ok(&api, list.to_string().as_bytes());
        http.ok(&file, &body);
        let url = format!("{base}/me/k");
        let preview = lib.preview_user_source(&url).unwrap();
        assert_eq!(preview.kind, "gitea");
        assert!(preview.releases[0].prerelease);
        let s = lib.add_user_source(&url, "k.elf").unwrap();
        assert_eq!(s.project_url, url);
        assert_eq!(
            lib.download_user_source(&s.id)
                .unwrap()
                .have
                .unwrap()
                .sha256,
            sha_hex(&body)
        );
    }

    #[test]
    fn an_entry_from_before_asks_for_the_file_and_keeps_its_copy() {
        let (_d, http, lib) = library("src-old");
        let info =
            super::super::repos::test_repos::save_custom_repo(&lib, "me", "kstuff", "My kstuff");
        let dir = lib.dir().join(&info.id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("kstuff.elf"), elf(7, 9)).unwrap();
        std::fs::write(dir.join("VERSION"), "v0.9\n").unwrap();

        let s = &lib.user_sources(true, true).unwrap()[0];
        assert_eq!(s.file_name, "");
        assert!(s.latest.is_none() && s.error.is_none());
        let have = s.have.as_ref().unwrap();
        assert_eq!(
            (
                have.tag.as_str(),
                have.file_name.as_str(),
                have.checksum.as_str()
            ),
            ("v0.9", "kstuff.elf", "")
        );
        assert!(
            http.asked.lock().unwrap().is_empty(),
            "nothing to check yet"
        );
        assert_eq!(
            lib.download_user_source(&info.id).unwrap_err().code,
            "needs_file"
        );

        serve_one(&http, "v1.0", "kstuff.elf", &elf(8, 11));
        let s = lib.set_user_source_file(&info.id, "kstuff.elf").unwrap();
        assert_eq!(s.file_name, "kstuff.elf");
        let s = &lib.user_sources(true, false).unwrap()[0];
        assert!(s.update_available);
        assert!(lib.preview_user_source(GH_URL).unwrap().already_added);
    }

    #[test]
    fn bad_input_is_refused_before_anything_is_written() {
        let (_d, http, lib) = library("src-bad");
        assert_eq!(
            lib.add_user_source(GH_URL, "k.zip").unwrap_err().code,
            "invalid_file_name"
        );
        assert_eq!(
            lib.add_user_source(GH_URL, "../k.elf").unwrap_err().code,
            "invalid_file_name"
        );
        assert_eq!(
            lib.add_user_source("http://github.com/me/k", "k.elf")
                .unwrap_err()
                .code,
            "url_not_https"
        );
        assert_eq!(
            lib.download_user_source("custom-../x").unwrap_err().code,
            "unknown_source"
        );
        assert_eq!(
            lib.set_user_source_file("custom-0b", "k.elf")
                .unwrap_err()
                .code,
            "unknown_source"
        );
        assert!(!lib.dir().exists());
        assert!(http.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn a_source_is_not_part_of_the_update_check() {
        let (_d, http, lib) = library("src-not-updates");
        serve_one(&http, "v1", "k.elf", &elf(1, 10));
        let s = lib.add_user_source(GH_URL, "k.elf").unwrap();
        lib.download_user_source(&s.id).unwrap();
        let state = lib.check_updates(true, &[]).unwrap();
        assert!(state.rows.iter().all(|r| r.payload_id != s.id));
        // Removing it takes its folder.
        assert!(lib.remove_custom_repo(&s.id).unwrap());
        assert!(!lib.dir().join(&s.id).exists());
    }
}
