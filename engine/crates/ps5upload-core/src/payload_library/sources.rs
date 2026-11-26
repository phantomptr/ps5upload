//! Payload sources (#829): a list of payloads someone publishes as one JSON file, added once
//! instead of repo by repo. The format is Payload Manager's "Payload Sources" list
//! (`payloads_v2.json`): `{"name": "...", "payloads": [{"name", "url", "version", "checksum",
//! "description", "category", "extract_file"?, "last_update"?, "source"?}, ...]}`.
//!
//! Each listed payload shows in the catalogue as `srcp-<hex>`, with two rules, because a list can
//! name any file:
//!   - the payload's file must be https on github.com (or the list's own site);
//!   - the list must give the file's SHA-256. A listed payload without one is left out (and
//!     counted).
//!
//! A payload the curated catalogue already has (same GitHub repo in its `source` field) is left
//! out too, so it is not shown twice.
//!
//! New lists can no longer be added, and the app never downloads a payload from one: lists added
//! by earlier versions still show their payloads, and a local copy got earlier stays usable. Lists
//! are never fetched again: a check for updates reads the copy saved on disk.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::catalogue::CATALOGUE;
use super::http::is_local_name;
use super::repos::{is_plausible_repo_host, CUSTOM_AUTOLOAD_DELAY_MS, CUSTOM_AUTOLOAD_PRIORITY};
use super::{atomic_write, Library, LibraryError, PayloadInfo, ResolvedEntry};

const SOURCES_FILE: &str = "payload-sources.json";
const SOURCES_CACHE_DIR: &str = "sources";
const URL_MAX_CHARS: usize = 2000;
const TEXT_MAX_CHARS: usize = 400;
const NAME_MAX_CHARS: usize = 80;

/// A list the user added.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct PayloadSource {
    pub(crate) id: String,
    pub(crate) url: String,
}

/// One usable payload of a list, as the catalogue needs it.
#[derive(Debug, Clone)]
pub(crate) struct ListPayload {
    pub(crate) id: String,
    pub(crate) source_id: String,
    pub(crate) source_name: String,
    pub(crate) name: String,
    pub(crate) url: String,
    pub(crate) host: String,
    pub(crate) version: String,
    pub(crate) description: String,
    pub(crate) category: String,
    pub(crate) extract_file: String,
    pub(crate) last_update: String,
    pub(crate) homepage: String,
}

/// A source as the screens show it.
#[derive(Debug, Clone, Serialize)]
pub struct SourceInfo {
    pub id: String,
    pub url: String,
    /// The list's own name, or its site when it gives none.
    pub name: String,
    /// Payloads shown from it.
    pub payloads: usize,
    /// Payloads left out: no SHA-256, a file off github.com and the list's site, or a malformed
    /// entry.
    pub skipped: usize,
    /// Payloads left out because the catalogue already has them.
    pub already_listed: usize,
    /// Age of the copy in use, seconds. `None` before the first successful fetch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_age_secs: Option<u64>,
    /// The last refresh failed: why. The previous copy (if any) is still used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn fnv12(key: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:012x}", h & 0xffff_ffff_ffff)
}

fn is_hex_id(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix).is_some_and(|rest| {
        rest.len() == 12
            && rest
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    })
}

pub(super) fn is_valid_source_id(id: &str) -> bool {
    is_hex_id(id, "src-")
}

/// A listed payload's id: used as its cache folder name.
pub(super) fn is_valid_list_payload_id(id: &str) -> bool {
    is_hex_id(id, "srcp-")
}

fn capped(s: &str, max: usize) -> String {
    s.trim().chars().take(max).collect()
}

/// The host of an https URL on the default port, lowercase, when it is a public name.
fn https_host(url: &str) -> Option<String> {
    let uri: ureq::http::Uri = url.parse().ok()?;
    if uri.scheme_str() != Some("https") || !matches!(uri.port_u16(), None | Some(443)) {
        return None;
    }
    if uri.authority()?.as_str().contains('@') {
        return None;
    }
    let host = uri.host()?.to_ascii_lowercase();
    (is_plausible_repo_host(&host) && !is_local_name(&host)).then_some(host)
}

/// The URL a user typed, checked: https, a public host name, no credentials.
fn normalize_url(raw: &str) -> Result<String, LibraryError> {
    let url = raw.trim();
    if url.len() > URL_MAX_CHARS || url.chars().any(char::is_whitespace) {
        return Err(LibraryError::new(
            "source_url_invalid",
            "That doesn't look like a web address.",
        ));
    }
    if https_host(url).is_none() {
        return Err(LibraryError::new(
            "source_url_invalid",
            "A source must be an https:// address on a public site.",
        ));
    }
    Ok(url.to_string())
}

/// `owner/name` of a GitHub repo page (`https://github.com/o/n/releases` → `o`, `n`).
fn github_repo_of(page: &str) -> Option<(String, String)> {
    let rest = page
        .trim()
        .strip_prefix("https://github.com/")
        .or_else(|| page.trim().strip_prefix("https://www.github.com/"))?;
    let mut parts = rest.split('/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let name = parts.next().filter(|s| !s.is_empty())?;
    Some((owner.to_string(), name.trim_end_matches(".git").to_string()))
}

/// A payload the curated catalogue has, or ps5upload's own helper (sent by the app itself).
fn already_listed(homepage: &str) -> bool {
    let Some((owner, name)) = github_repo_of(homepage) else {
        return false;
    };
    (owner.eq_ignore_ascii_case("phantomptr") && name.eq_ignore_ascii_case("ps5upload"))
        || CATALOGUE.iter().any(|e| {
            e.repo_host == "github.com"
                && e.repo_owner.eq_ignore_ascii_case(&owner)
                && e.repo_name.eq_ignore_ascii_case(&name)
        })
}

#[derive(Default)]
struct Parsed {
    name: String,
    payloads: Vec<ListPayload>,
    skipped: usize,
    already_listed: usize,
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Parse a list body. An error when it is not a list at all; bad entries are only counted.
fn parse_list(src: &PayloadSource, body: &[u8]) -> Result<Parsed, String> {
    let doc: Value =
        serde_json::from_slice(body).map_err(|e| format!("not a payload list: {e}"))?;
    let items = doc
        .get("payloads")
        .and_then(Value::as_array)
        .ok_or("not a payload list: it has no \"payloads\"")?;
    let list_host = https_host(&src.url).unwrap_or_default();
    let mut out = Parsed {
        name: capped(&str_field(&doc, "name"), NAME_MAX_CHARS),
        ..Parsed::default()
    };
    if out.name.is_empty() {
        out.name = list_host.clone();
    }
    for item in items {
        let name = capped(&str_field(item, "name"), NAME_MAX_CHARS);
        let url = str_field(item, "url");
        let sha256 = str_field(item, "checksum").to_ascii_lowercase();
        let host = https_host(&url);
        let usable = !name.is_empty()
            && url.len() <= URL_MAX_CHARS
            && host
                .as_deref()
                .is_some_and(|h| h == "github.com" || h == list_host)
            && sha256.len() == 64
            && sha256.chars().all(|c| c.is_ascii_hexdigit());
        if !usable {
            out.skipped += 1;
            continue;
        }
        let homepage = {
            let s = str_field(item, "source");
            if https_host(&s).is_some() && s.len() <= URL_MAX_CHARS {
                s
            } else {
                String::new()
            }
        };
        if already_listed(&homepage) {
            out.already_listed += 1;
            continue;
        }
        let id = format!(
            "srcp-{}",
            fnv12(&format!("{}\n{}", src.id, name.to_ascii_lowercase()))
        );
        if out.payloads.iter().any(|p| p.id == id) {
            out.skipped += 1;
            continue;
        }
        out.payloads.push(ListPayload {
            id,
            source_id: src.id.clone(),
            source_name: String::new(),
            name,
            url,
            host: host.unwrap_or_default(),
            version: capped(&str_field(item, "version"), NAME_MAX_CHARS),
            description: capped(&str_field(item, "description"), TEXT_MAX_CHARS),
            category: capped(&str_field(item, "category"), NAME_MAX_CHARS),
            extract_file: capped(&str_field(item, "extract_file"), NAME_MAX_CHARS),
            last_update: capped(&str_field(item, "last_update"), NAME_MAX_CHARS),
            homepage,
        });
    }
    let source_name = out.name.clone();
    for p in &mut out.payloads {
        p.source_name = source_name.clone();
    }
    Ok(out)
}

impl From<&ListPayload> for PayloadInfo {
    fn from(p: &ListPayload) -> Self {
        Self {
            id: p.id.clone(),
            display_name: p.name.clone(),
            role: p.category.clone(),
            description: p.description.clone(),
            repo_owner: String::new(),
            repo_name: String::new(),
            on_console_marker_path: None,
            process_name_hint: None,
            ports: Vec::new(),
            autoload_priority: CUSTOM_AUTOLOAD_PRIORITY,
            autoload_delay_ms: CUSTOM_AUTOLOAD_DELAY_MS,
            homepage: if p.homepage.is_empty() {
                p.url.clone()
            } else {
                p.homepage.clone()
            },
            is_custom: false,
            file_name: super::cache::cached_elf_name(&p.id, &p.file_hint(), &p.url),
            source_id: p.source_id.clone(),
            source_name: p.source_name.clone(),
        }
    }
}

impl ListPayload {
    /// The name the file is cached and stored under, and what picks the ELF inside a zip.
    pub(crate) fn file_hint(&self) -> String {
        if self.extract_file.is_empty() {
            self.name.clone()
        } else {
            self.extract_file.clone()
        }
    }

    pub(crate) fn resolved(&self) -> ResolvedEntry {
        ResolvedEntry {
            id: self.id.clone(),
            repo_host: self.host.clone(),
            repo_owner: String::new(),
            repo_name: String::new(),
            asset_name_hint: self.file_hint(),
            is_custom: false,
            list_payload: Some(Box::new(self.clone())),
            repo_port: None,
        }
    }
}

impl Library {
    fn sources_path(&self) -> std::path::PathBuf {
        self.dir.join(SOURCES_FILE)
    }

    fn list_cache_path(&self, id: &str) -> std::path::PathBuf {
        self.dir.join(SOURCES_CACHE_DIR).join(format!("{id}.json"))
    }

    fn list_error_path(&self, id: &str) -> std::path::PathBuf {
        self.dir.join(SOURCES_CACHE_DIR).join(format!("{id}.error"))
    }

    pub(crate) fn read_sources(&self) -> Vec<PayloadSource> {
        let Ok(bytes) = std::fs::read(self.sources_path()) else {
            return Vec::new();
        };
        let list: Vec<PayloadSource> = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            crate::log::log(&format!("[payloads] {SOURCES_FILE}: {e}"));
            Vec::new()
        });
        list.into_iter()
            .filter(|s| is_valid_source_id(&s.id) && normalize_url(&s.url).is_ok())
            .collect()
    }

    fn save_sources(&self, list: &[PayloadSource]) -> Result<(), LibraryError> {
        let body = serde_json::to_vec_pretty(list)
            .map_err(|e| LibraryError::new("io_failed", format!("serialize: {e}")))?;
        std::fs::create_dir_all(&self.dir).map_err(|e| {
            LibraryError::new("io_failed", format!("mkdir {}: {e}", self.dir.display()))
        })?;
        atomic_write(&self.sources_path(), &body)
            .map_err(|e| LibraryError::new("io_failed", format!("write sources: {e}")))
    }

    fn cached_list(&self, src: &PayloadSource) -> Option<(Parsed, u64)> {
        let path = self.list_cache_path(&src.id);
        let bytes = std::fs::read(&path).ok()?;
        let parsed = parse_list(src, &bytes).ok()?;
        let age = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Some((parsed, age))
    }

    fn source_info(&self, src: &PayloadSource) -> SourceInfo {
        let error = std::fs::read_to_string(self.list_error_path(&src.id))
            .ok()
            .filter(|s| !s.trim().is_empty());
        match self.cached_list(src) {
            Some((p, age)) => SourceInfo {
                id: src.id.clone(),
                url: src.url.clone(),
                name: p.name,
                payloads: p.payloads.len(),
                skipped: p.skipped,
                already_listed: p.already_listed,
                cached_age_secs: Some(age),
                error,
            },
            None => SourceInfo {
                id: src.id.clone(),
                url: src.url.clone(),
                name: https_host(&src.url).unwrap_or_default(),
                payloads: 0,
                skipped: 0,
                already_listed: 0,
                cached_age_secs: None,
                error,
            },
        }
    }

    /// The sources, from the copies on disk (no network).
    pub fn sources(&self) -> Vec<SourceInfo> {
        self.read_sources()
            .iter()
            .map(|s| self.source_info(s))
            .collect()
    }

    /// Remove a list, its copy and the local copies of its payloads. `false` for an unknown id.
    pub fn remove_source(&self, id: &str) -> Result<bool, LibraryError> {
        if !is_valid_source_id(id) {
            return Ok(false);
        }
        let mut list = self.read_sources();
        let Some(src) = list.iter().find(|s| s.id == id).cloned() else {
            return Ok(false);
        };
        let ids: Vec<String> = self
            .cached_list(&src)
            .map(|(p, _)| p.payloads.into_iter().map(|p| p.id).collect())
            .unwrap_or_default();
        list.retain(|s| s.id != id);
        self.save_sources(&list)?;
        for pid in ids.iter().filter(|p| is_valid_list_payload_id(p)) {
            let _ = std::fs::remove_dir_all(self.dir.join(pid));
        }
        let _ = std::fs::remove_file(self.list_cache_path(id));
        let _ = std::fs::remove_file(self.list_error_path(id));
        Ok(true)
    }

    /// Every usable payload of every list, from the copies on disk. A name two lists share shows
    /// once, from the first list.
    pub(crate) fn list_payloads(&self) -> Vec<ListPayload> {
        let mut out: Vec<ListPayload> = Vec::new();
        for src in self.read_sources() {
            if let Some((p, _)) = self.cached_list(&src) {
                for item in p.payloads {
                    if !out.iter().any(|o| o.name.eq_ignore_ascii_case(&item.name)) {
                        out.push(item);
                    }
                }
            }
        }
        out
    }

    pub(crate) fn list_payload(&self, id: &str) -> Option<ListPayload> {
        if !is_valid_list_payload_id(id) {
            return None;
        }
        self.list_payloads().into_iter().find(|p| p.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{library, seed};
    use super::*;

    const LIST_URL: &str = "https://lists.example.org/payloads.json";
    const ELF_URL: &str = "https://github.com/someone/tool/releases/download/v1/tool.elf";

    fn elf_bytes() -> Vec<u8> {
        let mut v = b"\x7FELF".to_vec();
        v.extend(std::iter::repeat_n(7u8, 300));
        v
    }

    fn sha(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn list_body(checksum: &str) -> String {
        serde_json::json!({
            "name": "Some suite",
            "payloads": [
                {"name": "tool", "url": ELF_URL, "version": "v1", "checksum": checksum,
                 "description": "A tool", "category": "Tools",
                 "source": "https://github.com/someone/tool"},
                {"name": "no-sum", "url": ELF_URL, "version": "v1"},
                {"name": "elsewhere", "url": "https://files.example.net/x.elf", "version": "v1",
                 "checksum": checksum},
                {"name": "plain-http", "url": "http://github.com/a/b/releases/download/v1/x.elf",
                 "checksum": checksum},
                {"name": "kstuff", "url": ELF_URL, "version": "v1", "checksum": checksum,
                 "source": "https://github.com/EchoStretch/kstuff-lite/releases"},
                {"name": "ps5upload", "url": ELF_URL, "checksum": checksum,
                 "source": "https://github.com/phantomptr/ps5upload/releases"}
            ]
        })
        .to_string()
    }

    /// A list as an earlier version of the app added it: saved, with its copy on disk.
    fn add(lib: &Library, url: &str, body: &[u8]) -> Result<SourceInfo, LibraryError> {
        let src = PayloadSource {
            id: format!("src-{}", fnv12(url)),
            url: normalize_url(url)?,
        };
        parse_list(&src, body).map_err(|e| LibraryError::new("source_not_a_list", e))?;
        std::fs::create_dir_all(lib.dir.join(SOURCES_CACHE_DIR)).unwrap();
        atomic_write(&lib.list_cache_path(&src.id), body).unwrap();
        lib.save_sources(std::slice::from_ref(&src))?;
        Ok(lib.source_info(&src))
    }

    fn tool_id(lib: &Library) -> String {
        lib.catalog()
            .into_iter()
            .find(|p| p.display_name == "tool")
            .unwrap()
            .id
    }

    #[test]
    fn a_list_shows_its_usable_payloads_in_the_catalogue() {
        let (_d, _http, lib) = library("src-add");
        let elf = elf_bytes();
        let info = add(&lib, LIST_URL, list_body(&sha(&elf)).as_bytes()).unwrap();
        assert_eq!(info.name, "Some suite");
        assert_eq!(info.payloads, 1);
        assert_eq!(info.skipped, 3, "no checksum, another host, plain http");
        assert_eq!(
            info.already_listed, 2,
            "kstuff is curated, ps5upload is the app's own"
        );
        let cat = lib.catalog();
        let p = cat.iter().find(|p| p.display_name == "tool").unwrap();
        assert!(is_valid_list_payload_id(&p.id));
        assert_eq!(p.source_name, "Some suite");
        assert_eq!(p.homepage, "https://github.com/someone/tool");
        assert_eq!(lib.sources().len(), 1);
    }

    #[test]
    fn a_listed_payload_is_sent_only_from_a_local_copy() {
        let (_d, http, lib) = library("src-local");
        let elf = elf_bytes();
        add(&lib, LIST_URL, list_body(&sha(&elf)).as_bytes()).unwrap();
        let id = tool_id(&lib);
        let rel = lib.release(&id, false).unwrap();
        assert_eq!(rel.tag, "v1");
        assert_eq!(rel.picked_asset_url, ELF_URL);

        let e = lib.local_copy(&id, None).unwrap_err();
        assert_eq!(e.code, super::super::NOT_ON_THIS_COMPUTER);
        assert!(e.message.contains("https://github.com/someone/tool"), "{e}");
        assert!(!http.asked.lock().unwrap().iter().any(|u| u == ELF_URL));

        let path = seed(&lib, &id, "tool.elf", &elf, "v1");
        assert_eq!(lib.local_copy(&id, None).unwrap(), path);
        assert!(lib.local_inventory().iter().any(|e| e.payload_id == id));
    }

    #[test]
    fn bad_addresses_and_non_lists_are_refused() {
        let (_d, _http, lib) = library("src-bad");
        let body = list_body(&sha(&elf_bytes()));
        for url in [
            "http://lists.example.org/p.json",
            "https://192.168.1.2/p.json",
            "https://user@lists.example.org/p.json",
            "not a url",
        ] {
            assert_eq!(
                add(&lib, url, body.as_bytes()).unwrap_err().code,
                "source_url_invalid"
            );
        }
        assert_eq!(
            add(&lib, LIST_URL, b"<html>hello</html>").unwrap_err().code,
            "source_not_a_list"
        );
        assert!(lib.sources().is_empty());
    }

    #[test]
    fn a_check_for_updates_reads_the_saved_list_and_never_fetches_it() {
        let (_d, http, lib) = library("src-refresh");
        let elf = elf_bytes();
        add(&lib, LIST_URL, list_body(&sha(&elf)).as_bytes()).unwrap();
        // Whatever the list's site would answer now is never asked.
        http.status(LIST_URL, 503);
        let id = tool_id(&lib);
        let rel = lib.release(&id, true).unwrap();
        assert_eq!(rel.tag, "v1");
        assert!(rel.refresh_error.is_none());
        assert!(http.asked.lock().unwrap().is_empty());
        assert_eq!(lib.sources()[0].payloads, 1);
    }

    #[test]
    fn removing_a_source_removes_its_payloads_and_their_files() {
        let (_d, _http, lib) = library("src-rm");
        let elf = elf_bytes();
        let src = add(&lib, LIST_URL, list_body(&sha(&elf)).as_bytes()).unwrap();
        let id = tool_id(&lib);
        seed(&lib, &id, "tool.elf", &elf, "v1");
        assert!(lib.remove_source(&src.id).unwrap());
        assert!(lib.local_path(&id).is_none());
        assert!(!lib.catalog().iter().any(|p| p.id == id));
        assert!(!lib.remove_source(&src.id).unwrap());
        assert!(!lib.remove_source("../x").unwrap());
    }
}
