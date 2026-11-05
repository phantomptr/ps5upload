//! Release listing: GitHub's and Gitea/Forgejo's release JSON, cached on disk for an hour, with
//! a stale cache served (and flagged) when a live fetch fails.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::catalogue::CATALOGUE_HOSTS;
use super::http::{fetch, FetchKind};
use super::{atomic_write, Library, LibraryError, ResolvedEntry};

/// One hour balances "a new release shows up promptly" against GitHub's 60 unauthenticated
/// requests an hour.
const RELEASE_CACHE_TTL: Duration = Duration::from_secs(3600);

/// The most release JSON accepted: 30 releases with long notes fit well under this.
const RELEASE_JSON_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// GitHub and Gitea both accept this; only GitHub looks at it.
const JSON_ACCEPT: &str = "application/vnd.github+json";

/// A text field GitHub sometimes sends as `null` instead of leaving it out (a release with no
/// notes has `"body": null`). `#[serde(default)]` alone only covers a missing field, so a null
/// failed the whole release list ("invalid type: null, expected a string").
fn null_as_empty<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize, Serialize, Clone)]
pub(super) struct GithubAsset {
    pub(super) name: String,
    pub(super) browser_download_url: String,
    pub(super) size: u64,
}

#[derive(Deserialize, Serialize, Clone)]
pub(super) struct GithubRelease {
    pub(super) tag_name: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub(super) name: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub(super) body: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub(super) published_at: String,
    #[serde(default)]
    pub(super) assets: Vec<GithubAsset>,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub(super) html_url: String,
    /// So the UI can warn that a version may be unstable (payload development moves fast).
    #[serde(default)]
    pub(super) prerelease: bool,
}

/// What a client consumes: flat, with the asset already picked.
#[derive(Debug, Serialize, Clone)]
pub struct ReleaseInfo {
    pub payload_id: String,
    /// Release tag (e.g. "v1.6.0").
    pub tag: String,
    pub name: String,
    /// Release notes as published, Markdown.
    pub body: String,
    pub published_at: String,
    pub html_url: String,
    /// The asset a download takes. Empty when nothing matched.
    pub picked_asset_url: String,
    pub picked_asset_name: String,
    pub picked_asset_size: u64,
    pub prerelease: bool,
    /// Cache age in seconds, so the UI can say how old this is.
    pub cached_age_secs: u64,
    /// Set when this came from the cache because the live fetch failed. Absent when the data is
    /// live, or when there was no cache either (the call then fails).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_error: Option<String>,
}

/// Extension priority for the asset picker: the executable shapes a loader accepts, `.elf` first.
/// `.jar` is last because BD-JB JARs need a JAR-aware loader, not :9021. Anything else is a last
/// resort and almost always a mistake.
const PAYLOAD_EXT_PRIORITY: &[&str] = &[".elf", ".bin", ".lua", ".js", ".jar"];

fn ext_priority(name: &str) -> usize {
    let lower = name.to_ascii_lowercase();
    PAYLOAD_EXT_PRIORITY
        .iter()
        .position(|ext| lower.ends_with(ext))
        .unwrap_or(PAYLOAD_EXT_PRIORITY.len())
}

/// Pick the canonical asset of a release using the catalogue hint: `(name, url, size)`, or empty
/// when the release has none.
pub(super) fn pick_asset(release: &GithubRelease, hint: &str) -> (String, String, u64) {
    let pack = |a: &GithubAsset| (a.name.clone(), a.browser_download_url.clone(), a.size);
    if release.assets.is_empty() {
        return (String::new(), String::new(), 0);
    }
    let lower_hint = hint.to_ascii_lowercase();
    // A hint match with a payload-shaped extension, so a source .zip never beats the runnable
    // .elf. Within those, .elf > .bin > .lua > .js.
    if !hint.is_empty() {
        let mut best: Option<(usize, &GithubAsset)> = None;
        for a in &release.assets {
            if !a.name.to_ascii_lowercase().contains(&lower_hint) {
                continue;
            }
            let prio = ext_priority(&a.name);
            if prio >= PAYLOAD_EXT_PRIORITY.len() {
                continue;
            }
            if best.map(|(p, _)| prio < p).unwrap_or(true) {
                best = Some((prio, a));
            }
        }
        if let Some((_, a)) = best {
            return pack(a);
        }
    }
    // Any .elf.
    if let Some(a) = release
        .assets
        .iter()
        .find(|a| a.name.to_ascii_lowercase().ends_with(".elf"))
    {
        return pack(a);
    }
    // Some payloads ship only a release .zip with the ELF inside (ShadowMount+). A hint match,
    // then any .zip; the downloader tells a zip by its magic and extracts the ELF.
    if !hint.is_empty() {
        if let Some(a) = release.assets.iter().find(|a| {
            let n = a.name.to_ascii_lowercase();
            n.ends_with(".zip") && n.contains(&lower_hint)
        }) {
            return pack(a);
        }
    }
    if let Some(a) = release
        .assets
        .iter()
        .find(|a| a.name.to_ascii_lowercase().ends_with(".zip"))
    {
        return pack(a);
    }
    // Almost always wrong; the magic check downstream catches it before anything is cached.
    pack(&release.assets[0])
}

fn release_to_info(
    entry: &ResolvedEntry,
    release: &GithubRelease,
    cached_age_secs: u64,
    refresh_error: Option<String>,
) -> ReleaseInfo {
    let (name, url, size) = pick_asset(release, &entry.asset_name_hint);
    ReleaseInfo {
        payload_id: entry.id.clone(),
        tag: release.tag_name.clone(),
        name: if release.name.is_empty() {
            release.tag_name.clone()
        } else {
            release.name.clone()
        },
        body: release.body.clone(),
        published_at: release.published_at.clone(),
        html_url: release.html_url.clone(),
        picked_asset_url: url,
        picked_asset_name: name,
        picked_asset_size: size,
        prerelease: release.prerelease,
        cached_age_secs,
        refresh_error,
    }
}

/// A cached file, parsed, with its age. `respect_ttl` is the fresh fast path; without it, stale
/// is better than nothing.
fn read_cached<T: serde::de::DeserializeOwned>(path: &Path, respect_ttl: bool) -> Option<(T, u64)> {
    let age = std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| SystemTime::now().duration_since(t).ok());
    if respect_ttl && age.is_none_or(|a| a >= RELEASE_CACHE_TTL) {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let parsed: T = serde_json::from_slice(&bytes).ok()?;
    Some((parsed, age.map(|a| a.as_secs()).unwrap_or(0)))
}

/// Parse a `/releases/latest` body (one object) or a releases-list body (an array, newest first:
/// the pre-release-only fallback). Returns the release and the single-object JSON to cache, so
/// the cache file keeps one shape whichever endpoint answered.
pub(super) fn parse_release_or_newest(body: &str) -> Result<(GithubRelease, String), String> {
    if body.trim_start().starts_with('[') {
        let mut list: Vec<serde_json::Value> =
            serde_json::from_str(body).map_err(|e| e.to_string())?;
        if list.is_empty() {
            return Err("the repository has no releases".into());
        }
        let first = list.swap_remove(0);
        let release: GithubRelease =
            serde_json::from_value(first.clone()).map_err(|e| e.to_string())?;
        return Ok((release, first.to_string()));
    }
    let release: GithubRelease = serde_json::from_str(body).map_err(|e| e.to_string())?;
    Ok((release, body.to_string()))
}

/// The repo's API root: GitHub has its own host; Gitea and Forgejo serve it under the web host.
fn api_url(entry: &ResolvedEntry, tail: &str) -> String {
    if entry.repo_host == "github.com" {
        format!(
            "https://api.github.com/repos/{}/{}/{tail}",
            entry.repo_owner, entry.repo_name
        )
    } else {
        format!(
            "https://{}/api/v1/repos/{}/{}/{tail}",
            entry.repo_host, entry.repo_owner, entry.repo_name
        )
    }
}

/// A catalogue row can only name a hostile host through a malicious change to the table, so its
/// host must be on the fixed list. A custom repo's host was approved when it was added.
fn check_catalogue_host(entry: &ResolvedEntry) -> Result<(), LibraryError> {
    if !entry.is_custom && !CATALOGUE_HOSTS.contains(&entry.repo_host.as_str()) {
        return Err(LibraryError::new(
            "fetch_failed",
            format!(
                "catalog entry {} declares repo_host={:?} which is not in the allowlist",
                entry.id, entry.repo_host
            ),
        ));
    }
    Ok(())
}

impl Library {
    fn cache_path(&self, id: &str, file: &str) -> PathBuf {
        self.dir.join(id).join(file)
    }

    fn write_cache(&self, path: &Path, body: &str) -> Result<(), LibraryError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                LibraryError::new("io_failed", format!("mkdir {}: {e}", dir.display()))
            })?;
        }
        atomic_write(path, body.as_bytes())
            .map_err(|e| LibraryError::new("io_failed", format!("write cache: {e}")))
    }

    /// One JSON document. `Err` carries the text for `refresh_error`.
    fn fetch_json(
        &self,
        entry: &ResolvedEntry,
        url: &str,
        fallback_url: Option<&str>,
    ) -> Result<String, LibraryError> {
        let mut resp = fetch(
            self.http.as_ref(),
            entry,
            url,
            Some(JSON_ACCEPT),
            FetchKind::Metadata,
        )?;
        if resp.status == 404 {
            if let Some(alt) = fallback_url {
                resp = fetch(
                    self.http.as_ref(),
                    entry,
                    alt,
                    Some(JSON_ACCEPT),
                    FetchKind::Metadata,
                )?;
            }
        }
        if !(200..300).contains(&resp.status) {
            // GitHub's unauthenticated limit answers 403, its abuse limit 429, an outage 5xx. All
            // of them are better served from a stale cache than as a hard error.
            let reason = ureq::http::StatusCode::from_u16(resp.status)
                .ok()
                .and_then(|s| s.canonical_reason())
                .unwrap_or("unknown status");
            return Err(LibraryError::new(
                "fetch_failed",
                format!("fetch {url}: HTTP {} ({reason})", resp.status),
            ));
        }
        let mut body = String::new();
        resp.body
            .take(RELEASE_JSON_MAX_BYTES)
            .read_to_string(&mut body)
            .map_err(|e| LibraryError::new("fetch_failed", format!("read body: {e}")))?;
        Ok(body)
    }

    /// The latest release of a payload. On a failed fetch with any cache, the cached release with
    /// `refresh_error` set, so the UI says the data may be old instead of showing nothing.
    pub fn release(&self, id: &str, force_refresh: bool) -> Result<ReleaseInfo, LibraryError> {
        let entry = self.resolve_entry(id)?;
        let cache_path = self.cache_path(&entry.id, "release.json");
        if !force_refresh {
            if let Some((release, age)) = read_cached::<GithubRelease>(&cache_path, true) {
                return Ok(release_to_info(&entry, &release, age, None));
            }
        }
        check_catalogue_host(&entry)?;

        let stale = |err: LibraryError| -> Result<ReleaseInfo, LibraryError> {
            match read_cached::<GithubRelease>(&cache_path, false) {
                Some((release, age)) => {
                    Ok(release_to_info(&entry, &release, age, Some(err.message)))
                }
                None => Err(err),
            }
        };

        // A repo that publishes only pre-releases (a rolling `latest` built by CI, like
        // remoteplay_unlock) has no `/releases/latest`: GitHub answers 404. The newest entry of
        // the list, which includes pre-releases, answers instead.
        let url = api_url(&entry, "releases/latest");
        let list_url = api_url(&entry, "releases?per_page=1");
        let body = match self.fetch_json(&entry, &url, Some(&list_url)) {
            Ok(b) => b,
            Err(e) => return stale(e),
        };
        let (release, body) = match parse_release_or_newest(&body) {
            Ok(r) => r,
            Err(e) => {
                return stale(LibraryError::new(
                    "fetch_failed",
                    format!("parse release JSON: {e}"),
                ))
            }
        };
        self.write_cache(&cache_path, &body)?;
        Ok(release_to_info(&entry, &release, 0, None))
    }

    /// Every release of a payload (the newest 30), newest first, so a user can pin a known-good
    /// build or go back when the latest is unstable.
    pub fn releases(
        &self,
        id: &str,
        force_refresh: bool,
    ) -> Result<Vec<ReleaseInfo>, LibraryError> {
        let entry = self.resolve_entry(id)?;
        let cache_path = self.cache_path(&entry.id, "releases.json");
        let infos = |list: &[GithubRelease], age: u64, why: Option<&str>| -> Vec<ReleaseInfo> {
            list.iter()
                .map(|r| release_to_info(&entry, r, age, why.map(str::to_string)))
                .collect()
        };
        if !force_refresh {
            if let Some((list, age)) = read_cached::<Vec<GithubRelease>>(&cache_path, true) {
                return Ok(infos(&list, age, None));
            }
        }
        check_catalogue_host(&entry)?;

        let stale = |err: LibraryError| -> Result<Vec<ReleaseInfo>, LibraryError> {
            match read_cached::<Vec<GithubRelease>>(&cache_path, false) {
                Some((list, age)) => Ok(infos(&list, age, Some(&err.message))),
                None => Err(err),
            }
        };

        // GitHub pages with per_page, Gitea with limit.
        let tail = if entry.repo_host == "github.com" {
            "releases?per_page=30"
        } else {
            "releases?limit=30"
        };
        let url = api_url(&entry, tail);
        let body = match self.fetch_json(&entry, &url, None) {
            Ok(b) => b,
            Err(e) => return stale(e),
        };
        let list: Vec<GithubRelease> = match serde_json::from_str(&body) {
            Ok(r) => r,
            Err(e) => {
                return stale(LibraryError::new(
                    "fetch_failed",
                    format!("parse releases JSON: {e}"),
                ))
            }
        };
        self.write_cache(&cache_path, &body)?;
        Ok(infos(&list, 0, None))
    }
}

#[cfg(test)]
mod tests {
    use super::super::catalogue::CATALOGUE;
    use super::super::test_support::library;
    use super::*;

    fn asset(name: &str, url: &str, size: u64) -> GithubAsset {
        GithubAsset {
            name: name.into(),
            browser_download_url: url.into(),
            size,
        }
    }

    fn release_with(tag: &str, assets: Vec<GithubAsset>) -> GithubRelease {
        GithubRelease {
            tag_name: tag.into(),
            name: "".into(),
            body: "".into(),
            published_at: "".into(),
            html_url: "".into(),
            prerelease: false,
            assets,
        }
    }

    fn row(id: &str) -> &'static super::super::catalogue::CatalogueEntry {
        CATALOGUE.iter().find(|e| e.id == id).unwrap()
    }

    #[test]
    fn a_release_with_null_notes_still_parses() {
        // websrv v0.35 shipped with `"body": null`, which failed the whole list.
        let json = r#"[{"tag_name":"v0.35","name":null,"body":null,"published_at":null,
            "html_url":null,"prerelease":false,"assets":[{"name":"websrv-ps5.elf",
            "browser_download_url":"https://example/websrv-ps5.elf","size":1,"label":null}]}]"#;
        let r: Vec<GithubRelease> = serde_json::from_str(json).unwrap();
        assert_eq!(r[0].tag_name, "v0.35");
        assert_eq!(r[0].body, "");
        assert_eq!(r[0].assets[0].name, "websrv-ps5.elf");
    }

    #[test]
    fn pick_asset_picks_the_webkit_autoloader_elf_not_the_host_tools() {
        // The real v0.3.1 release ships a Windows host .exe, a Python host .py and the on-console
        // installer .elf. The names carry the version, so the catalogue hint stops before it.
        let r = release_with(
            "v0.3.1",
            vec![
                asset(
                    "webkit-autoloader-host_v0.3.1.exe",
                    "https://example/exe",
                    11196317,
                ),
                asset(
                    "webkit-autoloader-host_v0.3.1.py",
                    "https://example/py",
                    3467510,
                ),
                asset(
                    "webkit-autoloader-installer_v0.3.1.elf",
                    "https://example/elf",
                    1983744,
                ),
            ],
        );
        let (n, _, _) = pick_asset(&r, "webkit-autoloader-installer");
        assert_eq!(n, "webkit-autoloader-installer_v0.3.1.elf");
        let r2 = release_with(
            "v9",
            vec![asset(
                "webkit-autoloader-installer_v9.9.9.elf",
                "https://example/elf",
                1,
            )],
        );
        let (n2, _, _) = pick_asset(&r2, "webkit-autoloader-installer");
        assert_eq!(n2, "webkit-autoloader-installer_v9.9.9.elf");
    }

    #[test]
    fn pick_asset_prefers_hint_match() {
        let r = release_with(
            "v1.0",
            vec![
                asset("kstuff-lowfw.elf", "https://example/lowfw", 1),
                asset("kstuff.elf", "https://example/main", 2),
            ],
        );
        // "kstuff" matches both and the first wins: pinned so a change is noticed.
        let (n, _, _) = pick_asset(&r, "kstuff");
        assert_eq!(n, "kstuff-lowfw.elf");
    }

    #[test]
    fn pick_asset_falls_back_to_elf() {
        let r = release_with(
            "v1.0",
            vec![
                asset("README.md", "https://example/readme", 1),
                asset("main.elf", "https://example/main", 2),
            ],
        );
        let (n, _, _) = pick_asset(&r, "nothing-matches");
        assert_eq!(n, "main.elf");
    }

    #[test]
    fn pick_asset_prefers_elf_over_source_zip() {
        // Field report: the first substring hit was the source .zip, so the loader got PK bytes.
        let r = release_with(
            "1.6beta10",
            vec![
                asset(
                    "shadowmountplus-1.6beta10.zip",
                    "https://example/zip",
                    1_000_000,
                ),
                asset(
                    "shadowmountplus-1.6beta10.elf",
                    "https://example/elf",
                    750_000,
                ),
            ],
        );
        let (n, _, _) = pick_asset(&r, "shadowmountplus");
        assert_eq!(n, "shadowmountplus-1.6beta10.elf");
    }

    #[test]
    fn pick_asset_skips_unrelated_extensions() {
        let r = release_with(
            "v1",
            vec![
                asset("kstuff.sha256", "https://example/hash", 64),
                asset("kstuff.elf", "https://example/elf", 100_000),
            ],
        );
        let (n, _, _) = pick_asset(&r, "kstuff");
        assert_eq!(n, "kstuff.elf");
    }

    #[test]
    fn pick_asset_empty_assets() {
        let (n, u, s) = pick_asset(&release_with("v1.0", vec![]), "");
        assert_eq!((n.as_str(), u.as_str(), s), ("", "", 0));
    }

    #[test]
    fn pick_asset_uses_zip_when_only_zip() {
        // ShadowMount+'s real releases ship only a .zip.
        let r = release_with(
            "1.6test11",
            vec![asset(
                "ShadowMountPlus_1.6test11.zip",
                "https://example/smp.zip",
                760_192,
            )],
        );
        let (n, _, _) = pick_asset(&r, "shadowmountplus");
        assert_eq!(n, "ShadowMountPlus_1.6test11.zip");
    }

    #[test]
    fn ps5debug_ng_entry_picks_the_versioned_release_elf() {
        let e = row("ps5debug-ng");
        assert_eq!(e.ports, &[744]);
        // It runs inside the console's system process, so there is no process to look for.
        assert!(e.process_name_hint.is_none());
        let r = release_with(
            "1.3.2",
            vec![asset(
                "ps5debug-NG_v1.3.2.elf",
                "https://example/ps5debug",
                1,
            )],
        );
        let (n, _, _) = pick_asset(&r, e.asset_name_hint);
        assert_eq!(n, "ps5debug-NG_v1.3.2.elf");
    }

    #[test]
    fn remoteplay_unlock_takes_the_rolling_prerelease() {
        let e = row("remoteplay-unlock");
        assert!(e.process_name_hint.is_none() && e.on_console_marker_path.is_none());
        assert!(e.ports.is_empty());
        let body = r#"[{"tag_name":"latest","name":"latest","body":null,"published_at":"2026-10-30T00:00:00Z",
            "html_url":"https://github.com/earthonion/remoteplay_unlock/releases/tag/latest","prerelease":true,
            "assets":[{"name":"remoteplay_unlock.elf","browser_download_url":"https://example/rp","size":9}]}]"#;
        let (r, cached) = parse_release_or_newest(body).expect("list parses");
        assert_eq!(r.tag_name, "latest");
        assert!(r.prerelease);
        assert!(cached.trim_start().starts_with('{'));
        assert_eq!(
            parse_release_or_newest(&cached).unwrap().0.tag_name,
            "latest"
        );
        let (n, _, _) = pick_asset(&r, e.asset_name_hint);
        assert_eq!(n, "remoteplay_unlock.elf");
        assert!(parse_release_or_newest("[]").is_err());
    }

    #[test]
    fn release_to_info_carries_tag_and_prerelease() {
        let entry: ResolvedEntry = row("shadowmountplus").into();
        let mk = |tag: &str, pre: bool| GithubRelease {
            prerelease: pre,
            ..release_with(tag, vec![])
        };
        let stable = release_to_info(&entry, &mk("v2.0", false), 0, None);
        let pre = release_to_info(&entry, &mk("v2.1-rc1", true), 0, None);
        assert_eq!(stable.tag, "v2.0");
        assert!(!stable.prerelease);
        assert_eq!(pre.tag, "v2.1-rc1");
        assert!(pre.prerelease);
    }

    const LIST: &str = r#"[{"tag_name":"v2","assets":[{"name":"k.elf","browser_download_url":"https://github.com/EchoStretch/kstuff-lite/releases/download/v2/k.elf","size":4}]},
        {"tag_name":"v1","assets":[]}]"#;

    #[test]
    fn a_list_is_fetched_cached_and_served_fresh() {
        let (_d, http, lib) = library("list-fresh");
        let url = "https://api.github.com/repos/EchoStretch/kstuff-lite/releases?per_page=30";
        http.ok(url, LIST.as_bytes());
        let list = lib.releases("kstuff-echostretch", false).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].picked_asset_name, "k.elf");
        assert!(list[0].refresh_error.is_none());
        let asked = http.asked.lock().unwrap().len();
        // Fresh cache: no second request.
        lib.releases("kstuff-echostretch", false).unwrap();
        assert_eq!(http.asked.lock().unwrap().len(), asked);
    }

    #[test]
    fn a_failed_refresh_serves_the_cache_and_says_why() {
        let (_d, http, lib) = library("list-stale");
        let url = "https://api.github.com/repos/EchoStretch/kstuff-lite/releases?per_page=30";
        http.ok(url, LIST.as_bytes());
        lib.releases("kstuff-echostretch", false).unwrap();
        http.status(url, 403);
        let list = lib.releases("kstuff-echostretch", true).unwrap();
        assert_eq!(list.len(), 2);
        let why = list[0].refresh_error.as_deref().unwrap();
        assert!(why.contains("HTTP 403 (Forbidden)"), "{why}");
    }

    #[test]
    fn no_cache_and_a_failed_fetch_is_an_error_and_leaves_no_folder() {
        let (_d, _http, lib) = library("list-none");
        let err = lib.releases("kstuff-echostretch", false).unwrap_err();
        assert!(err.message.contains("HTTP 404"), "{}", err.message);
        assert!(!lib.dir().join("kstuff-echostretch").exists());
    }

    #[test]
    fn a_prerelease_only_repo_falls_back_to_the_list() {
        let (_d, http, lib) = library("latest-404");
        let base = "https://api.github.com/repos/EchoStretch/kstuff-lite";
        http.status(&format!("{base}/releases/latest"), 404);
        http.ok(&format!("{base}/releases?per_page=1"), LIST.as_bytes());
        let r = lib.release("kstuff-echostretch", false).unwrap();
        assert_eq!(r.tag, "v2");
        // Cached as one object, and the fresh path reads it back.
        let again = lib.release("kstuff-echostretch", false).unwrap();
        assert_eq!(again.tag, "v2");
    }

    #[test]
    fn a_gitea_host_uses_its_own_api_root() {
        let (_d, http, lib) = library("gitea");
        http.ok(
            "https://git.etawen.dev/api/v1/repos/earthonion/np-fake-signin/releases?limit=30",
            b"[]",
        );
        assert!(lib.releases("np-fake-signin", false).unwrap().is_empty());
    }

    #[test]
    fn an_unknown_id_is_named() {
        let (_d, _h, lib) = library("unknown");
        assert_eq!(
            lib.releases("nope", false).unwrap_err().code,
            "unknown_payload"
        );
        assert_eq!(
            lib.release("nope", false).unwrap_err().code,
            "unknown_payload"
        );
    }
}
