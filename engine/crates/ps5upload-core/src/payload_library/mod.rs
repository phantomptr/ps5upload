//! Payload Library: the curated catalogue of third-party PS5 payloads, user-added repos,
//! release fetching (GitHub, Gitea, Forgejo), and the on-disk cache of downloaded ELFs.
//!
//! One implementation for every front end: the desktop app's commands and the engine's
//! `/api/payloads` routes both call [`Library`], over the same folder when the engine is the
//! desktop app's own sidecar (`PS5UPLOAD_PAYLOADS_DIR`).
//!
//! Design choices:
//!   - **Curated catalogue, not auto-discovery.** A free-form GitHub search would surface dozens
//!     of forks of every project. Each entry is hand-picked; users add their own repos on top.
//!   - **Static table + runtime release fetch.** The table ships in the binary; versions are
//!     fetched on demand and cached for an hour, to stay under GitHub's 60 requests an hour.
//!   - **Read-only towards the console.** This module fetches and caches. Sending is
//!     `payload_lifecycle::send_payload_file`.
//!
//! On-disk layout (kept from when this lived in the desktop app, so existing caches work):
//! `<dir>/custom-repos.json`, `<dir>/<id>/<name>.elf`, `<dir>/<id>/VERSION`,
//! `<dir>/<id>/release.json`, `<dir>/<id>/releases.json`.

mod catalogue;
mod download;
mod http;
mod pm_sync;
mod releases;
mod repos;
mod updates;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use serde::Serialize;

use catalogue::{CatalogueEntry, CATALOGUE};
pub use http::{FetchKind, HttpClient, HttpResponse, UreqClient};
pub use pm_sync::{HttpPm, PmChange, PmConsole, PmStoredRecord, PmSyncChoice, PmSyncReport};
pub use releases::ReleaseInfo;
pub use repos::AddRepo;
use repos::CustomRepo;
pub use updates::{
    is_newer, AppliedUpdate, UpdateOutcome, UpdateRow, UpdateSettings, UpdateState, PREVIOUS_DIR,
};

/// Overrides where the library lives. The desktop app sets it for its sidecar, so the engine's
/// routes and the app's screens share one cache.
pub const PAYLOADS_DIR_ENV: &str = "PS5UPLOAD_PAYLOADS_DIR";

/// An error a caller can branch on. `message` is English text for a log or an API client;
/// `code` is stable. See [`LibraryError::ui_text`] for what the app's screens receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryError {
    pub code: &'static str,
    pub message: String,
}

/// Codes the app words itself, in the user's language (`lib/payloadErrors.ts`).
const WORDED_BY_THE_APP: &[&str] = &[
    "repo_host_is_ip",
    "repo_host_gitlab",
    "custom_repos_unreadable",
    "download_cancelled",
    "redirect_refused",
    "invalid_checksum_file",
];

impl LibraryError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The text a screen receives through a desktop command: `code: message` for a code the app
    /// words itself (the renderer looks the code up), else the message alone.
    pub fn ui_text(&self) -> String {
        if WORDED_BY_THE_APP.contains(&self.code) {
            format!("{}: {}", self.code, self.message)
        } else {
            self.message.clone()
        }
    }
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LibraryError {}

/// Serializable form of one catalogue row or custom repo.
#[derive(Debug, Clone, Serialize)]
pub struct PayloadInfo {
    pub id: String,
    pub display_name: String,
    pub role: String,
    pub description: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub on_console_marker_path: Option<String>,
    pub process_name_hint: Option<String>,
    pub ports: Vec<u16>,
    pub autoload_priority: u8,
    pub autoload_delay_ms: u32,
    pub homepage: String,
    /// True for a user-added repo (#93): the UI badges it and offers Remove.
    pub is_custom: bool,
    /// The file name a download is cached and stored in Payload Manager under
    /// (`shadowmountplus.elf`), so a copy stored there by hand can be told apart. Empty for a
    /// custom repo, whose name comes from each release.
    pub file_name: String,
}

impl From<&CatalogueEntry> for PayloadInfo {
    fn from(e: &CatalogueEntry) -> Self {
        Self {
            id: e.id.to_string(),
            display_name: e.display_name.to_string(),
            role: e.role.to_string(),
            description: e.description.to_string(),
            repo_owner: e.repo_owner.to_string(),
            repo_name: e.repo_name.to_string(),
            on_console_marker_path: e.on_console_marker_path.map(String::from),
            process_name_hint: e.process_name_hint.map(String::from),
            ports: e.ports.to_vec(),
            autoload_priority: e.autoload_priority,
            autoload_delay_ms: e.autoload_delay_ms,
            homepage: e.homepage.to_string(),
            is_custom: false,
            file_name: download::cached_elf_name(e.id, e.asset_name_hint, ""),
        }
    }
}

/// A file in the local cache.
#[derive(Debug, Clone, Serialize)]
pub struct LocalInventoryEntry {
    pub payload_id: String,
    /// Tag of the cached binary, from the `VERSION` file beside it.
    pub version: String,
    /// Path of the cached `.elf`: what a send consumes.
    pub path: String,
    pub size: u64,
    /// File mtime, seconds since the Unix epoch.
    pub mtime: i64,
}

/// Where a payload loads in a generated `autoload.txt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoloadMeta {
    pub id: String,
    pub priority: u8,
    pub delay_ms: u32,
}

/// An id resolved for the network path: a catalogue row or a custom repo.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedEntry {
    pub(crate) id: String,
    pub(crate) repo_host: String,
    pub(crate) repo_owner: String,
    pub(crate) repo_name: String,
    pub(crate) asset_name_hint: String,
    /// The release's `sha256sum`-style list, when it publishes one: a download is refused unless
    /// its SHA-256 matches. Empty for most payloads.
    pub(crate) checksum_file: String,
    /// A user-added repo: its host was approved at add time, so the catalogue's fixed host list
    /// does not apply to it.
    pub(crate) is_custom: bool,
}

impl From<&CatalogueEntry> for ResolvedEntry {
    fn from(e: &CatalogueEntry) -> Self {
        Self {
            id: e.id.to_string(),
            repo_host: e.repo_host.to_string(),
            repo_owner: e.repo_owner.to_string(),
            repo_name: e.repo_name.to_string(),
            asset_name_hint: e.asset_name_hint.to_string(),
            checksum_file: download::checksum_file_for(e.id)
                .unwrap_or_default()
                .to_string(),
            is_custom: false,
        }
    }
}

impl From<&CustomRepo> for ResolvedEntry {
    fn from(r: &CustomRepo) -> Self {
        Self {
            id: r.id.clone(),
            repo_host: r.repo_host.clone(),
            repo_owner: r.repo_owner.clone(),
            repo_name: r.repo_name.clone(),
            asset_name_hint: r.asset_name_hint.clone(),
            checksum_file: r.checksum_file.clone(),
            is_custom: true,
        }
    }
}

/// The library at one folder. Cheap to create: it holds a path and an HTTP client handle.
#[derive(Clone)]
pub struct Library {
    dir: PathBuf,
    http: Arc<dyn HttpClient>,
}

impl fmt::Debug for Library {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Library").field("dir", &self.dir).finish()
    }
}

impl Library {
    /// The library at `dir`, the folder that holds `custom-repos.json` and one folder per payload.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self::with_http(dir, Arc::new(UreqClient::new()))
    }

    /// As [`Library::new`], with the HTTP client given: the seam tests use.
    pub fn with_http(dir: impl Into<PathBuf>, http: Arc<dyn HttpClient>) -> Self {
        Self {
            dir: dir.into(),
            http,
        }
    }

    /// The library an engine uses: [`PAYLOADS_DIR_ENV`] when set, else `payloads` under its data
    /// folder.
    pub fn from_env(data_dir: Option<PathBuf>) -> Option<Self> {
        if let Some(v) = std::env::var_os(PAYLOADS_DIR_ENV).filter(|v| !v.is_empty()) {
            return Some(Self::new(PathBuf::from(v)));
        }
        data_dir.map(|d| Self::new(d.join("payloads")))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Curated entries first, then the user's own repos.
    pub fn catalog(&self) -> Vec<PayloadInfo> {
        let mut out: Vec<PayloadInfo> = CATALOGUE.iter().map(PayloadInfo::from).collect();
        out.extend(self.load_custom_repos().iter().map(PayloadInfo::from));
        out
    }

    /// Why the custom repo list cannot be read, when it cannot. An add or remove is refused
    /// then, so the file stays for the user to repair.
    pub fn custom_repos_problem(&self) -> Option<String> {
        self.read_repo_file().err().map(|e| e.message)
    }

    /// Resolve an id for the network path: user repos first, then the catalogue.
    pub(crate) fn resolve_entry(&self, id: &str) -> Result<ResolvedEntry, LibraryError> {
        if let Some(c) = self.load_custom_repos().iter().find(|r| r.id == id) {
            return Ok(c.into());
        }
        find_entry(id).map(ResolvedEntry::from).ok_or_else(|| {
            LibraryError::new("unknown_payload", format!("unknown payload id: {id}"))
        })
    }

    /// Where a payload loads in an autoload list: a catalogue row's own slot, or after every
    /// curated entry for a user's repo. `None` for an id the library does not know.
    pub fn autoload_meta(&self, id: &str) -> Option<AutoloadMeta> {
        if let Some(e) = find_entry(id) {
            return Some(AutoloadMeta {
                id: e.id.to_string(),
                priority: e.autoload_priority,
                delay_ms: e.autoload_delay_ms,
            });
        }
        self.load_custom_repos()
            .iter()
            .find(|r| r.id == id)
            .map(|r| AutoloadMeta {
                id: r.id.clone(),
                priority: repos::CUSTOM_AUTOLOAD_PRIORITY,
                delay_ms: repos::CUSTOM_AUTOLOAD_DELAY_MS,
            })
    }

    /// The folder of a payload's cache, if the id is safe as a path component and the folder is
    /// there. Never creates it: listing the inventory left 25+ empty folders behind.
    fn cache_dir_if_exists(&self, id: &str) -> Option<PathBuf> {
        if !is_safe_payload_id(id) {
            return None;
        }
        let dir = self.dir.join(id);
        dir.is_dir().then_some(dir)
    }

    /// The cached `.elf` of a payload, if any.
    pub fn local_path(&self, id: &str) -> Option<PathBuf> {
        elf_in_dir(&self.cache_dir_if_exists(id)?)
    }

    fn local_entry(&self, id: &str) -> Option<LocalInventoryEntry> {
        let path = self.local_path(id)?;
        let meta = std::fs::metadata(&path).ok()?;
        let version = path
            .parent()
            .and_then(|d| std::fs::read_to_string(d.join("VERSION")).ok())
            .unwrap_or_default()
            .trim()
            .to_string();
        Some(LocalInventoryEntry {
            payload_id: id.to_string(),
            version,
            path: path.to_string_lossy().to_string(),
            size: meta.len(),
            mtime: mtime_secs(&meta),
        })
    }

    /// Every cached binary, catalogue entries and custom repos.
    pub fn local_inventory(&self) -> Vec<LocalInventoryEntry> {
        let mut out = Vec::new();
        for entry in CATALOGUE {
            out.extend(self.local_entry(entry.id));
        }
        for repo in self.load_custom_repos() {
            out.extend(self.local_entry(&repo.id));
        }
        out
    }
}

/// Look up a catalogue row by id.
fn find_entry(id: &str) -> Option<&'static CatalogueEntry> {
    CATALOGUE.iter().find(|e| e.id == id)
}

/// A catalogue id or a well-formed custom id: the only names ever used as a cache folder, so
/// an id from a request can never leave the library's folder.
fn is_safe_payload_id(id: &str) -> bool {
    find_entry(id).is_some() || repos::is_valid_custom_id(id)
}

/// The `.elf` in a payload's cache folder (there is one per payload; see `remove_other_elfs`).
fn elf_in_dir(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|f| {
        let p = f.path();
        (p.extension().and_then(|e| e.to_str()) == Some("elf")).then_some(p)
    })
}

fn mtime_secs(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A name no other writer of the folder uses at the same time, this process's or another's.
fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{}.{nanos}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// Move `from` over `to`. Windows refuses a rename over an open or read-only target more often
/// than POSIX, hence the second try after removing it.
pub(crate) fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        match std::fs::rename(from, to) {
            Ok(()) => Ok(()),
            Err(first) if to.exists() => {
                std::fs::remove_file(to)?;
                std::fs::rename(from, to).map_err(|retry| {
                    std::io::Error::new(
                        retry.kind(),
                        format!(
                            "replace failed after removing the target: {retry}; first: {first}"
                        ),
                    )
                })
            }
            Err(e) => Err(e),
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(from, to)
    }
}

/// Write a file through a private temporary name, so a reader never sees half of it and two
/// writers never truncate each other's temporary file.
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = PathBuf::from(format!("{}.{}.tmp", path.display(), unique_suffix()));
    let guard = TmpFile(tmp.clone());
    std::fs::write(&tmp, bytes)?;
    replace_file(&tmp, path)?;
    drop(guard);
    Ok(())
}

/// Removes a temporary file when the step that owns it ends, whichever way it ends. A file that
/// was renamed away is simply no longer there.
struct TmpFile(PathBuf);

impl Drop for TmpFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::collections::HashMap;
    use std::io::Cursor;
    use std::sync::Mutex;

    type Route = (u16, Option<String>, Vec<u8>);

    /// A scripted server: url → (status, location, body). Unknown urls answer 404.
    #[derive(Default)]
    pub struct Scripted {
        pub routes: Mutex<HashMap<String, Route>>,
        pub asked: Mutex<Vec<String>>,
    }

    impl Scripted {
        pub fn ok(&self, url: &str, body: &[u8]) {
            self.routes
                .lock()
                .unwrap()
                .insert(url.to_string(), (200, None, body.to_vec()));
        }
        pub fn status(&self, url: &str, status: u16) {
            self.routes
                .lock()
                .unwrap()
                .insert(url.to_string(), (status, None, Vec::new()));
        }
        pub fn redirect(&self, url: &str, to: &str) {
            self.routes
                .lock()
                .unwrap()
                .insert(url.to_string(), (302, Some(to.to_string()), Vec::new()));
        }
    }

    impl HttpClient for Scripted {
        fn get(
            &self,
            url: &str,
            _accept: Option<&str>,
            _kind: FetchKind,
        ) -> Result<HttpResponse, String> {
            self.asked.lock().unwrap().push(url.to_string());
            let (status, location, body) = self
                .routes
                .lock()
                .unwrap()
                .get(url)
                .cloned()
                .unwrap_or((404, None, Vec::new()));
            Ok(HttpResponse {
                status,
                location,
                content_length: Some(body.len() as u64),
                body: Box::new(Cursor::new(body)),
            })
        }
    }

    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("ps5upload-library-{tag}-{}", unique_suffix()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub fn library(tag: &str) -> (TempDir, Arc<Scripted>, Library) {
        let dir = TempDir::new(tag);
        let http = Arc::new(Scripted::default());
        let lib = Library::with_http(dir.0.join("payloads"), http.clone());
        (dir, http, lib)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn listing_the_inventory_creates_no_folders() {
        let (_d, _h, lib) = library("list");
        assert!(lib.local_inventory().is_empty());
        assert!(lib.local_path("kstuff-echostretch").is_none());
        assert!(
            !lib.dir().exists(),
            "reads must not create the library folder"
        );
    }

    #[test]
    fn an_existing_cache_folder_is_listed_with_its_version() {
        let (_d, _h, lib) = library("cached");
        let dir = lib.dir().join("kstuff-echostretch");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("kstuff.elf"), b"\x7FELFx").unwrap();
        std::fs::write(dir.join("VERSION"), "v1.2\n").unwrap();
        let inv = lib.local_inventory();
        assert_eq!(inv.len(), 1);
        assert_eq!(inv[0].payload_id, "kstuff-echostretch");
        assert_eq!(inv[0].version, "v1.2");
        assert_eq!(inv[0].size, 5);
        assert!(inv[0].path.ends_with("kstuff.elf"));
    }

    #[test]
    fn a_path_like_id_never_reaches_the_file_system() {
        let (_d, _h, lib) = library("traverse");
        std::fs::create_dir_all(lib.dir().join("x")).unwrap();
        std::fs::write(lib.dir().join("x").join("a.elf"), b"\x7FELF").unwrap();
        assert!(lib.local_path("../payloads/x").is_none());
        assert!(lib.local_path("x").is_none(), "not a known id");
        assert_eq!(
            lib.resolve_entry("../etc").unwrap_err().code,
            "unknown_payload"
        );
    }

    #[test]
    fn the_app_words_only_the_codes_it_knows() {
        let worded = LibraryError::new("repo_host_is_ip", "x");
        assert_eq!(worded.ui_text(), "repo_host_is_ip: x");
        // An older message the renderer matches ("HTTP 404") stays as it was.
        let plain = LibraryError::new("fetch_failed", "fetch u: HTTP 404 (Not Found)");
        assert_eq!(plain.ui_text(), "fetch u: HTTP 404 (Not Found)");
    }

    #[test]
    fn catalogue_ids_unique_and_kebab_case() {
        let mut ids: Vec<&str> = CATALOGUE.iter().map(|e| e.id).collect();
        ids.sort();
        let dup = ids.windows(2).find(|w| w[0] == w[1]);
        assert!(dup.is_none(), "duplicate catalogue id: {dup:?}");
        for entry in CATALOGUE {
            assert!(
                entry
                    .id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "id must be kebab-case: {}",
                entry.id
            );
        }
    }

    #[test]
    fn catalogue_hosts_allowed_and_loader_asset_pinned() {
        for entry in CATALOGUE {
            assert!(
                catalogue::CATALOGUE_HOSTS.contains(&entry.repo_host),
                "{}",
                entry.id
            );
        }
        let lz4 = find_entry("lz4-compression").expect("LZ4 catalogue entry");
        assert_eq!(lz4.repo_host, "theghostonline.dev");
        let loader = find_entry("elfldr").unwrap();
        assert_eq!(loader.asset_name_hint, "elfldr-ps5.elf");
    }

    #[test]
    fn find_entry_lookup() {
        assert!(find_entry("shadowmountplus").is_some());
        assert!(find_entry("nope").is_none());
    }

    #[test]
    fn autoload_priorities_form_groups() {
        // kstuff entries are the only priority-0 ones, SMP is alone at priority 1.
        let p0: Vec<_> = CATALOGUE
            .iter()
            .filter(|e| e.autoload_priority == 0)
            .collect();
        let p1: Vec<_> = CATALOGUE
            .iter()
            .filter(|e| e.autoload_priority == 1)
            .collect();
        assert!(p0.iter().all(|e| e.id.starts_with("kstuff-")));
        assert!(p1
            .iter()
            .all(|e| matches!(e.id, "shadowmountplus" | "elfldr")));
    }
}

/// Real network, real TLS and real redirects: run by hand after changing the HTTP client.
/// `cargo test -p ps5upload-core payload_library::live -- --ignored --nocapture`
#[cfg(test)]
mod live {
    use super::test_support::TempDir;
    use super::*;

    fn fetch_and_cache(id: &str) {
        let d = TempDir::new("live");
        let lib = Library::new(d.0.join("payloads"));
        let list = lib.releases(id, false).expect("release list");
        println!("{id}: {} releases, newest {}", list.len(), list[0].tag);
        assert!(list[0].refresh_error.is_none());
        let local = lib
            .download_release(id, Some(&list[0].tag), false)
            .expect("download");
        println!("{id}: cached {} bytes at {}", local.size, local.path);
        let bytes = std::fs::read(&local.path).unwrap();
        assert_eq!(&bytes[..4], b"\x7FELF");
        assert_eq!(bytes.len() as u64, local.size);
        // The file, VERSION and the release list: no temporary left behind.
        assert_eq!(
            std::fs::read_dir(d.0.join("payloads").join(id))
                .unwrap()
                .count(),
            3
        );
    }

    #[test]
    #[ignore]
    fn github_through_its_asset_cdn() {
        fetch_and_cache("kstuff-echostretch");
    }

    #[test]
    #[ignore]
    fn a_zip_release_on_github() {
        fetch_and_cache("shadowmountplus");
    }

    #[test]
    #[ignore]
    fn a_gitea_host() {
        fetch_and_cache("np-fake-signin");
    }

    #[test]
    #[ignore]
    fn a_release_with_a_checksum_list() {
        fetch_and_cache("hush");
    }
}
