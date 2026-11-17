//! User-added payload repos (#93).
//!
//! The curated catalogue is a `const`. Users also track their own repos (a fork, a niche payload),
//! which live in `custom-repos.json` and flow through the same release-information path, so
//! every fetch guard applies to them. New repos can no longer be added from the app (it never
//! downloads a payload); repos added earlier stay listed and can be removed. Custom ids are always
//! `custom-<hex>`, so they can never collide with a catalogue id nor leave the library's folder.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{atomic_write, Library, LibraryError, PayloadInfo};

/// A custom repo loads after every curated entry in an autoload list.
pub(super) const CUSTOM_AUTOLOAD_PRIORITY: u8 = u8::MAX;
pub(super) const CUSTOM_AUTOLOAD_DELAY_MS: u32 = 200;

/// A user-added repo, as stored by an earlier version. `id` was minted when it was added; the rest
/// is what the user supplied. Fields this build no longer reads (`checksum_file`) are ignored.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct CustomRepo {
    pub(crate) id: String,
    pub(crate) display_name: String,
    pub(crate) repo_host: String,
    pub(crate) repo_owner: String,
    pub(crate) repo_name: String,
    #[serde(default)]
    pub(crate) asset_name_hint: String,
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
            source_id: String::new(),
            source_name: String::new(),
        }
    }
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
    /// and by any remove; a listing just shows the curated entries.
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
pub(crate) mod test_repos {
    use super::*;

    /// Save a custom repo as an earlier version of the app did (repos can no longer be added).
    pub(crate) fn save_custom_repo(
        lib: &Library,
        owner: &str,
        name: &str,
        display_name: &str,
    ) -> PayloadInfo {
        let key = format!("github.com/{owner}/{name}").to_ascii_lowercase();
        let mut h: u64 = 0xcbf29ce484222325;
        for b in key.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let repo = CustomRepo {
            id: format!("custom-{:012x}", h & 0xffff_ffff_ffff),
            display_name: display_name.to_string(),
            repo_host: "github.com".into(),
            repo_owner: owner.into(),
            repo_name: name.into(),
            asset_name_hint: String::new(),
        };
        assert!(is_valid_custom_id(&repo.id));
        let mut file = lib.read_repo_file().unwrap();
        file.repos.push(repo.clone());
        lib.save_repo_file(&file).unwrap();
        PayloadInfo::from(&repo)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::library;
    use super::test_repos::save_custom_repo;
    use super::*;

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
    fn a_custom_repo_maps_to_payload_info() {
        let (_d, _h, lib) = library("info");
        let info = save_custom_repo(&lib, "me", "my-payload", "My Fork");
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
        assert!(lib.remove_custom_repo("custom-0a").unwrap());
        // Written back: both entries it could not use, as they were.
        let saved: Vec<Value> =
            serde_json::from_slice(&std::fs::read(lib.dir().join("custom-repos.json")).unwrap())
                .unwrap();
        assert_eq!(saved.len(), 2);
        assert!(saved.iter().any(|v| v["id"] == "custom-0b"));
        assert!(saved.iter().any(|v| v["id"] == "../evil"));
        // The unusable ones never reach a cache path.
        assert!(lib.load_custom_repos().is_empty());
    }

    #[test]
    fn a_file_that_does_not_parse_refuses_every_save() {
        let (_d, _h, lib) = library("unparsable");
        std::fs::create_dir_all(lib.dir()).unwrap();
        let path = lib.dir().join("custom-repos.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(lib.load_custom_repos().is_empty());
        assert!(lib.custom_repos_problem().is_some());
        let err = lib.remove_custom_repo("custom-0a").unwrap_err();
        assert_eq!(err.code, "custom_repos_unreadable");
        assert_eq!(std::fs::read(&path).unwrap(), b"{ not json");
    }

    #[test]
    fn remove_takes_the_cache_with_it_and_ignores_catalogue_ids() {
        let (_d, _h, lib) = library("remove");
        let info = save_custom_repo(&lib, "me", "r", "me/r");
        let dir = lib.dir().join(&info.id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.elf"), b"x").unwrap();
        assert!(lib.remove_custom_repo(&info.id).unwrap());
        assert!(!dir.exists());
        assert!(!lib.remove_custom_repo(&info.id).unwrap());
        assert!(!lib.remove_custom_repo("kstuff-echostretch").unwrap());
    }

    #[test]
    fn autoload_meta_covers_custom_repos() {
        let (_d, _h, lib) = library("meta");
        let info = save_custom_repo(&lib, "me", "r", "me/r");
        let meta = lib.autoload_meta(&info.id).unwrap();
        assert_eq!(meta.priority, u8::MAX);
        assert_eq!(meta.delay_ms, 200);
        assert_eq!(lib.autoload_meta("kstuff-echostretch").unwrap().priority, 0);
        assert!(lib.autoload_meta("nope").is_none());
    }
}
