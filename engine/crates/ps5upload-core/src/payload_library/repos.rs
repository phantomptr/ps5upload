//! User-added payload repos (#93).
//!
//! The curated catalogue is a `const`. Users also track their own repos (a fork, a niche payload),
//! which live in `custom-repos.json` and flow through the same release and download path, so
//! every download guard applies to them. Custom ids are always `custom-<hex>`, so they can never
//! collide with a catalogue id nor leave the library's folder.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::catalogue::CATALOGUE;
use super::{atomic_write, Library, LibraryError, PayloadInfo};

/// A custom repo loads after every curated entry in an autoload list.
pub(super) const CUSTOM_AUTOLOAD_PRIORITY: u8 = u8::MAX;
pub(super) const CUSTOM_AUTOLOAD_DELAY_MS: u32 = 200;

const DISPLAY_NAME_MAX_CHARS: usize = 80;
const ASSET_HINT_MAX_CHARS: usize = 100;

/// A user-added repo, as stored. `id` is minted when it is added; the rest is what the user
/// supplied.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct CustomRepo {
    pub(crate) id: String,
    pub(crate) display_name: String,
    pub(crate) repo_host: String,
    pub(crate) repo_owner: String,
    pub(crate) repo_name: String,
    #[serde(default)]
    pub(crate) asset_name_hint: String,
    /// The release's `sha256sum`-style list (`SHA256SUMS`), when the repo publishes one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) checksum_file: String,
}

impl From<&CustomRepo> for PayloadInfo {
    fn from(r: &CustomRepo) -> Self {
        let homepage = if r.repo_host == "github.com" {
            format!("https://github.com/{}/{}", r.repo_owner, r.repo_name)
        } else {
            format!("https://{}/{}/{}", r.repo_host, r.repo_owner, r.repo_name)
        };
        Self {
            id: r.id.clone(),
            display_name: r.display_name.clone(),
            role: "Custom repo".to_string(),
            description: format!(
                "User-added repo. Releases are fetched from {}/{}/{} and sent like any \
                 other payload — the same ELF/zip checks apply. ps5upload doesn't vet \
                 what this repo ships; only add repos you trust.",
                r.repo_host, r.repo_owner, r.repo_name
            ),
            repo_owner: r.repo_owner.clone(),
            repo_name: r.repo_name.clone(),
            on_console_marker_path: None,
            process_name_hint: None,
            ports: Vec::new(),
            autoload_priority: CUSTOM_AUTOLOAD_PRIORITY,
            autoload_delay_ms: CUSTOM_AUTOLOAD_DELAY_MS,
            homepage,
            is_custom: true,
            file_name: String::new(),
        }
    }
}

/// What a repo is added with.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AddRepo {
    pub host: String,
    pub owner: String,
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default, alias = "asset_name_hint")]
    pub asset_hint: Option<String>,
    #[serde(default)]
    pub checksum_file: Option<String>,
}

/// The list as read: the entries in use, and any entry this build cannot use, kept as they were
/// and written back. One bad entry (a hand edit, a field from a newer version) used to read as an
/// empty list, and the next add or remove then wrote that empty list over the file.
#[derive(Default)]
pub(super) struct RepoFile {
    pub(super) repos: Vec<CustomRepo>,
    kept: Vec<Value>,
}

/// A `custom-<id>` is well-formed iff it's exactly "custom-" + 1..=32 lowercase hex chars.
/// Enforced everywhere a custom id is used as a folder name.
pub(super) fn is_valid_custom_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("custom-") else {
        return false;
    };
    !rest.is_empty()
        && rest.len() <= 32
        && rest
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn parse_repo_file(bytes: &[u8]) -> Result<RepoFile, String> {
    let entries: Vec<Value> = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let mut file = RepoFile::default();
    for v in entries {
        match serde_json::from_value::<CustomRepo>(v.clone()) {
            // A corrupt id must never reach a cache-folder path.
            Ok(r) if is_valid_custom_id(&r.id) => file.repos.push(r),
            _ => {
                crate::log::log(
                    "[payloads] custom-repos.json: keeping an entry this build can't use",
                );
                file.kept.push(v);
            }
        }
    }
    Ok(file)
}

/// The host as the app stores it, or why it can't be one.
fn normalize_host(raw: &str) -> Result<String, LibraryError> {
    let lower = raw.trim().to_ascii_lowercase();
    let host = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .unwrap_or(&lower)
        .trim_end_matches('/');
    // A pasted www.github.com/owner/repo is GitHub; its API is not at www.
    let host = if host == "www.github.com" {
        "github.com"
    } else {
        host
    };
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.parse::<IpAddr>().is_ok() {
        return Err(LibraryError::new(
            "repo_host_is_ip",
            format!("\"{host}\" is an IP address. Use github.com or the domain of a Gitea or Forgejo server."),
        ));
    }
    if host == "gitlab.com" || host.starts_with("gitlab.") {
        return Err(LibraryError::new(
            "repo_host_gitlab",
            "GitLab isn't supported. Use github.com or a Gitea or Forgejo server.",
        ));
    }
    if !is_plausible_repo_host(host) {
        return Err(LibraryError::new(
            "repo_host_invalid",
            format!("\"{host}\" doesn't look like a repo host — use github.com or a Gitea/Forgejo domain."),
        ));
    }
    Ok(host.to_string())
}

/// A real hostname: at least one dot, labels of `[a-z0-9-]`, no scheme, path, port or credentials.
/// Not a list of servers (that is the point of "custom"), just a sanity gate.
pub(super) fn is_plausible_repo_host(host: &str) -> bool {
    if host == "github.com" {
        return true;
    }
    if host.len() > 253
        || host.contains('/')
        || host.contains(':')
        || host.contains('@')
        || host.contains(' ')
    {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    labels.iter().all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    })
}

/// One path component of a repo: non-empty, ≤100 chars, `[A-Za-z0-9._-]`, not dots alone. Keeps a
/// typo from adding path segments to the API URL.
pub(super) fn is_valid_repo_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

fn capped(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// `custom-<12 hex>`: FNV-1a over the lowercased identity, so the same repo typed in another
/// case gets the same id and the same cache folder. An entry added before this was lowercased
/// keeps the id it has; duplicates are found by comparing the repo, not the id.
fn mint_id(host: &str, owner: &str, name: &str) -> String {
    let key = format!("{host}/{owner}/{name}").to_ascii_lowercase();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("custom-{:012x}", h & 0xffff_ffff_ffff)
}

impl Library {
    fn repo_file_path(&self) -> std::path::PathBuf {
        self.dir.join("custom-repos.json")
    }

    /// The list on disk. A file that exists but can't be parsed is an error, never an empty list.
    pub(super) fn read_repo_file(&self) -> Result<RepoFile, LibraryError> {
        let path = self.repo_file_path();
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(RepoFile::default()),
            Err(e) => {
                return Err(LibraryError::new(
                    "custom_repos_unreadable",
                    format!("{}: {e}", path.display()),
                ))
            }
        };
        parse_repo_file(&bytes).map_err(|e| {
            LibraryError::new(
                "custom_repos_unreadable",
                format!("{}: {e}", path.display()),
            )
        })
    }

    /// The repos in use. A problem with the file is reported by [`Library::custom_repos_problem`]
    /// and by any add or remove; a listing just shows the curated entries.
    pub(super) fn load_custom_repos(&self) -> Vec<CustomRepo> {
        match self.read_repo_file() {
            Ok(f) => f.repos,
            Err(e) => {
                crate::log::log(&format!("[payloads] {e}"));
                Vec::new()
            }
        }
    }

    fn save_repo_file(&self, file: &RepoFile) -> Result<(), LibraryError> {
        let mut out: Vec<Value> = Vec::with_capacity(file.repos.len() + file.kept.len());
        for r in &file.repos {
            out.push(
                serde_json::to_value(r)
                    .map_err(|e| LibraryError::new("io_failed", format!("serialize: {e}")))?,
            );
        }
        out.extend(file.kept.iter().cloned());
        let body = serde_json::to_vec_pretty(&out)
            .map_err(|e| LibraryError::new("io_failed", format!("serialize: {e}")))?;
        std::fs::create_dir_all(&self.dir).map_err(|e| {
            LibraryError::new("io_failed", format!("mkdir {}: {e}", self.dir.display()))
        })?;
        atomic_write(&self.repo_file_path(), &body)
            .map_err(|e| LibraryError::new("io_failed", format!("write custom repos: {e}")))
    }

    /// Add a user's repo. Adding one again is how its name or hint gets corrected. A repo the
    /// catalogue already has answers with the curated entry and adds nothing.
    pub fn add_custom_repo(&self, req: &AddRepo) -> Result<PayloadInfo, LibraryError> {
        let host = normalize_host(&req.host)?;
        let owner = req.owner.trim().to_string();
        let name = req.name.trim().trim_end_matches(".git").to_string();
        if !is_valid_repo_segment(&owner) {
            return Err(LibraryError::new(
                "repo_owner_invalid",
                "The owner/org name has invalid characters.",
            ));
        }
        if !is_valid_repo_segment(&name) {
            return Err(LibraryError::new(
                "repo_name_invalid",
                "The repository name has invalid characters.",
            ));
        }
        let checksum_file = req
            .checksum_file
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        if !checksum_file.is_empty() && !is_valid_repo_segment(&checksum_file) {
            return Err(LibraryError::new(
                "invalid_checksum_file",
                "The checksum list must be a file name, like SHA256SUMS.",
            ));
        }

        if let Some(curated) = CATALOGUE.iter().find(|e| {
            e.repo_host == host
                && e.repo_owner.eq_ignore_ascii_case(&owner)
                && e.repo_name.eq_ignore_ascii_case(&name)
        }) {
            return Ok(PayloadInfo::from(curated));
        }

        let display_name = req
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| capped(s, DISPLAY_NAME_MAX_CHARS))
            .unwrap_or_else(|| capped(&format!("{owner}/{name}"), DISPLAY_NAME_MAX_CHARS));
        let asset_name_hint = capped(
            req.asset_hint.as_deref().map(str::trim).unwrap_or_default(),
            ASSET_HINT_MAX_CHARS,
        );

        let mut file = self.read_repo_file()?;
        if let Some(existing) = file.repos.iter_mut().find(|r| {
            r.repo_host == host
                && r.repo_owner.eq_ignore_ascii_case(&owner)
                && r.repo_name.eq_ignore_ascii_case(&name)
        }) {
            if existing.display_name != display_name
                || existing.asset_name_hint != asset_name_hint
                || existing.checksum_file != checksum_file
            {
                existing.display_name = display_name;
                existing.asset_name_hint = asset_name_hint;
                existing.checksum_file = checksum_file;
                let info = PayloadInfo::from(&*existing);
                self.save_repo_file(&file)?;
                return Ok(info);
            }
            return Ok(PayloadInfo::from(&*existing));
        }
        let repo = CustomRepo {
            id: mint_id(&host, &owner, &name),
            display_name,
            repo_host: host,
            repo_owner: owner,
            repo_name: name,
            asset_name_hint,
            checksum_file,
        };
        debug_assert!(is_valid_custom_id(&repo.id));
        let info = PayloadInfo::from(&repo);
        file.repos.push(repo);
        self.save_repo_file(&file)?;
        Ok(info)
    }

    /// Remove a user's repo and everything cached for it. `false` for an id that is not one (a
    /// catalogue id can't be removed).
    pub fn remove_custom_repo(&self, id: &str) -> Result<bool, LibraryError> {
        if !is_valid_custom_id(id) {
            return Ok(false);
        }
        let mut file = self.read_repo_file()?;
        let before = file.repos.len();
        file.repos.retain(|r| r.id != id);
        if file.repos.len() == before {
            return Ok(false);
        }
        self.save_repo_file(&file)?;
        // Named by a validated id, so this can't leave the library's folder.
        let _ = std::fs::remove_dir_all(self.dir.join(id));
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::library;
    use super::*;

    fn add(owner: &str, name: &str) -> AddRepo {
        AddRepo {
            host: "github.com".into(),
            owner: owner.into(),
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn custom_id_validation_blocks_traversal() {
        assert!(is_valid_custom_id("custom-0"));
        assert!(is_valid_custom_id("custom-deadbeef1234"));
        assert!(is_valid_custom_id(&format!("custom-{:012x}", 0xabcu64)));
        assert!(!is_valid_custom_id("custom-"));
        assert!(!is_valid_custom_id("kstuff-echostretch"));
        assert!(!is_valid_custom_id("custom-../etc"));
        assert!(!is_valid_custom_id("custom-ab/cd"));
        assert!(!is_valid_custom_id("custom-DEADBEEF"));
        assert!(!is_valid_custom_id("custom-xyz"));
        assert!(!is_valid_custom_id("../custom-ab"));
        assert!(!is_valid_custom_id(&format!("custom-{}", "a".repeat(33))));
    }

    #[test]
    fn plausible_repo_host_gate() {
        assert!(is_plausible_repo_host("github.com"));
        assert!(is_plausible_repo_host("git.etawen.dev"));
        assert!(is_plausible_repo_host("codeberg.org"));
        assert!(is_plausible_repo_host("gitea.example.co.uk"));
        assert!(!is_plausible_repo_host("localhost"));
        assert!(!is_plausible_repo_host("github"));
        assert!(!is_plausible_repo_host("evil.com/path"));
        assert!(!is_plausible_repo_host("evil.com:22"));
        assert!(!is_plausible_repo_host("user@evil.com"));
        assert!(!is_plausible_repo_host("has space.com"));
        assert!(!is_plausible_repo_host("-bad.com"));
        assert!(!is_plausible_repo_host(""));
    }

    #[test]
    fn repo_segment_validation() {
        assert!(is_valid_repo_segment("EchoStretch"));
        assert!(is_valid_repo_segment("kstuff-lite"));
        assert!(is_valid_repo_segment("my.repo_2"));
        assert!(!is_valid_repo_segment(""));
        assert!(!is_valid_repo_segment("."));
        assert!(!is_valid_repo_segment(".."));
        assert!(!is_valid_repo_segment("has/slash"));
        assert!(!is_valid_repo_segment("has space"));
        assert!(!is_valid_repo_segment(&"a".repeat(101)));
    }

    #[test]
    fn hosts_are_normalized_and_the_unusable_ones_named() {
        assert_eq!(normalize_host("www.github.com").unwrap(), "github.com");
        assert_eq!(normalize_host("https://GitHub.com/").unwrap(), "github.com");
        assert_eq!(normalize_host("codeberg.org").unwrap(), "codeberg.org");
        assert_eq!(
            normalize_host("192.168.1.1").unwrap_err().code,
            "repo_host_is_ip"
        );
        assert_eq!(normalize_host("[::1]").unwrap_err().code, "repo_host_is_ip");
        assert_eq!(
            normalize_host("gitlab.com").unwrap_err().code,
            "repo_host_gitlab"
        );
        assert_eq!(
            normalize_host("gitlab.example.org").unwrap_err().code,
            "repo_host_gitlab"
        );
        assert_eq!(
            normalize_host("localhost").unwrap_err().code,
            "repo_host_invalid"
        );
    }

    #[test]
    fn a_custom_repo_maps_to_payload_info() {
        let (_d, _h, lib) = library("info");
        let info = lib
            .add_custom_repo(&AddRepo {
                display_name: Some("My Fork".into()),
                ..add("me", "my-payload")
            })
            .unwrap();
        assert!(is_valid_custom_id(&info.id));
        assert_eq!(info.display_name, "My Fork");
        assert!(info.is_custom);
        assert_eq!(info.homepage, "https://github.com/me/my-payload");
        assert_eq!(info.autoload_priority, u8::MAX);
        let resolved = lib.resolve_entry(&info.id).unwrap();
        assert!(resolved.is_custom);
        assert_eq!(resolved.repo_host, "github.com");
        assert!(lib.catalog().iter().any(|p| p.id == info.id));
    }

    #[test]
    fn the_same_repo_in_another_case_is_one_entry_with_one_id() {
        let (_d, _h, lib) = library("case");
        let a = lib.add_custom_repo(&add("SomeOne", "Thing")).unwrap();
        let b = lib.add_custom_repo(&add("someone", "thing")).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(lib.load_custom_repos().len(), 1);
        // The case the user typed first is what the URLs use.
        assert_eq!(lib.load_custom_repos()[0].repo_owner, "SomeOne");
    }

    #[test]
    fn an_entry_saved_with_an_older_id_keeps_it_and_is_not_duplicated() {
        let (_d, _h, lib) = library("oldid");
        std::fs::create_dir_all(lib.dir()).unwrap();
        std::fs::write(
            lib.dir().join("custom-repos.json"),
            r#"[{"id":"custom-0a","display_name":"x","repo_host":"github.com","repo_owner":"Me","repo_name":"R"}]"#,
        )
        .unwrap();
        let info = lib.add_custom_repo(&add("me", "r")).unwrap();
        assert_eq!(info.id, "custom-0a");
        assert_eq!(lib.load_custom_repos().len(), 1);
    }

    #[test]
    fn a_curated_repo_returns_the_curated_row_and_adds_nothing() {
        let (_d, _h, lib) = library("curated");
        let row = CATALOGUE
            .iter()
            .find(|e| e.repo_host == "github.com")
            .unwrap();
        let info = lib
            .add_custom_repo(&add(
                &row.repo_owner.to_uppercase(),
                &row.repo_name.to_uppercase(),
            ))
            .unwrap();
        assert_eq!(info.id, row.id);
        assert!(!info.is_custom);
        assert!(lib.load_custom_repos().is_empty());
        assert!(!lib.dir().join("custom-repos.json").exists());
    }

    #[test]
    fn names_are_capped() {
        let (_d, _h, lib) = library("cap");
        let info = lib
            .add_custom_repo(&AddRepo {
                display_name: Some("é".repeat(500)),
                asset_hint: Some("x".repeat(500)),
                ..add("me", "r")
            })
            .unwrap();
        assert_eq!(info.display_name.chars().count(), DISPLAY_NAME_MAX_CHARS);
        assert_eq!(
            lib.load_custom_repos()[0].asset_name_hint.len(),
            ASSET_HINT_MAX_CHARS
        );
    }

    #[test]
    fn one_bad_entry_loses_nothing_on_the_next_save() {
        let (_d, _h, lib) = library("bad-entry");
        std::fs::create_dir_all(lib.dir()).unwrap();
        std::fs::write(
            lib.dir().join("custom-repos.json"),
            r#"[
              {"id":"custom-0a","display_name":"good","repo_host":"github.com","repo_owner":"a","repo_name":"b"},
              {"id":"custom-0b","display_name":"missing fields"},
              {"id":"../evil","display_name":"x","repo_host":"github.com","repo_owner":"c","repo_name":"d"}
            ]"#,
        )
        .unwrap();
        assert_eq!(lib.load_custom_repos().len(), 1);
        lib.add_custom_repo(&add("new", "repo")).unwrap();
        // Written back: the good one, the new one, and both it could not use.
        let saved: Vec<Value> =
            serde_json::from_slice(&std::fs::read(lib.dir().join("custom-repos.json")).unwrap())
                .unwrap();
        assert_eq!(saved.len(), 4);
        assert!(saved.iter().any(|v| v["id"] == "custom-0b"));
        assert!(saved.iter().any(|v| v["id"] == "../evil"));
        // The unusable ones never reach a cache path.
        assert_eq!(lib.load_custom_repos().len(), 2);
    }

    #[test]
    fn a_file_that_does_not_parse_refuses_every_save() {
        let (_d, _h, lib) = library("unparsable");
        std::fs::create_dir_all(lib.dir()).unwrap();
        let path = lib.dir().join("custom-repos.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(lib.load_custom_repos().is_empty());
        assert!(lib.custom_repos_problem().is_some());
        let err = lib.add_custom_repo(&add("a", "b")).unwrap_err();
        assert_eq!(err.code, "custom_repos_unreadable");
        let err = lib.remove_custom_repo("custom-0a").unwrap_err();
        assert_eq!(err.code, "custom_repos_unreadable");
        assert_eq!(std::fs::read(&path).unwrap(), b"{ not json");
    }

    #[test]
    fn remove_takes_the_cache_with_it_and_ignores_catalogue_ids() {
        let (_d, _h, lib) = library("remove");
        let info = lib.add_custom_repo(&add("me", "r")).unwrap();
        let dir = lib.dir().join(&info.id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.elf"), b"x").unwrap();
        assert!(lib.remove_custom_repo(&info.id).unwrap());
        assert!(!dir.exists());
        assert!(!lib.remove_custom_repo(&info.id).unwrap());
        assert!(!lib.remove_custom_repo("kstuff-echostretch").unwrap());
    }

    #[test]
    fn a_checksum_file_must_be_a_plain_name() {
        let (_d, _h, lib) = library("sums");
        let err = lib
            .add_custom_repo(&AddRepo {
                checksum_file: Some("../SHA256SUMS".into()),
                ..add("me", "r")
            })
            .unwrap_err();
        assert_eq!(err.code, "invalid_checksum_file");
        let info = lib
            .add_custom_repo(&AddRepo {
                checksum_file: Some("SHA256SUMS".into()),
                ..add("me", "r")
            })
            .unwrap();
        assert_eq!(
            lib.resolve_entry(&info.id).unwrap().checksum_file,
            "SHA256SUMS"
        );
    }

    #[test]
    fn autoload_meta_covers_custom_repos() {
        let (_d, _h, lib) = library("meta");
        let info = lib.add_custom_repo(&add("me", "r")).unwrap();
        let meta = lib.autoload_meta(&info.id).unwrap();
        assert_eq!(meta.priority, u8::MAX);
        assert_eq!(meta.delay_ms, 200);
        assert_eq!(lib.autoload_meta("kstuff-echostretch").unwrap().priority, 0);
        assert!(lib.autoload_meta("nope").is_none());
    }
}
