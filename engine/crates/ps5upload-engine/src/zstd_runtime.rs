//! The ampr-emu-ng a zstd-packed game gets (`fakelib/libSceAmpr.sprx`): quer3q/ampr-emu-ng's
//! newest GitHub release, or the tested one this app pins.
//!
//! The newest is taken only when GitHub publishes a SHA-256 for its `libSceAmpr.sprx` asset
//! (the asset's `digest`, `sha256:<hex>`) and the download matches it. Anything else (GitHub
//! out of reach, no such asset, no digest, a mismatch, a download from somewhere else) falls
//! back to the pinned release, which is checked against the digest this app ships with. Each
//! version is cached in the data dir under its own name, and a cached file is used only while it
//! still matches its digest.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::zstd_packs::{NG_SHA256, NG_URL, NG_VERSION};

/// GitHub's answer for the newest release.
const LATEST_API: &str = "https://api.github.com/repos/quer3q/ampr-emu-ng/releases/latest";
/// Where the release's assets download from: nothing is fetched from anywhere else.
const DOWNLOAD_PREFIX: &str = "https://github.com/quer3q/ampr-emu-ng/releases/download/";
const ASSET: &str = "libSceAmpr.sprx";
/// Largest the emulator (and GitHub's answer) may be.
const LIMIT: u64 = 16 << 20;
/// How long GitHub's answer is reused: a queue of games asks once, not once a game (GitHub
/// allows 60 unauthenticated requests an hour).
const REMEMBER_LATEST: Duration = Duration::from_secs(30 * 60);

/// Which ampr-emu-ng goes into a packed game.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeChoice {
    /// The newest release, verified against GitHub's digest; the pinned one when it can't be.
    #[default]
    Latest,
    /// The tested release this app pins.
    Pinned,
}

/// A release that can be fetched and checked.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pinned<'a> {
    pub version: &'a str,
    pub url: &'a str,
    pub sha256: &'a str,
}

/// The tested release.
pub(crate) const PINNED: Pinned<'static> = Pinned {
    version: NG_VERSION,
    url: NG_URL,
    sha256: NG_SHA256,
};

/// The emulator a game got, and why it is that one.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct PackRuntime {
    pub name: &'static str,
    pub version: String,
    /// `latest` (the newest release, digest checked) or `pinned` (the tested release).
    pub source: &'static str,
    /// Why the newest was asked for and not used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    #[serde(skip)]
    pub bytes: Vec<u8>,
}

impl PackRuntime {
    /// One line for the log: `ampr-emu-ng 0.3.2 (pinned: <why>)`.
    pub(crate) fn describe(&self) -> String {
        match &self.fallback_reason {
            Some(why) => format!(
                "{} {} ({}; the newest release was not used: {why})",
                self.name, self.version, self.source
            ),
            None => format!("{} {} ({})", self.name, self.version, self.source),
        }
    }
}

/// An HTTP GET: the body, or why there is none.
pub(crate) type Getter<'a> = dyn Fn(&str) -> Result<Vec<u8>, String> + 'a;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `path` from the cache when it matches `sha`, else `url` fetched through `get`, used and
/// cached only when it matches: what goes into a game is exactly what was published.
pub(crate) fn fetch_checked(
    get: &Getter,
    url: &str,
    sha: &str,
    path: &Path,
    what: &str,
) -> Result<Vec<u8>, String> {
    if let Ok(bytes) = std::fs::read(path) {
        if sha256_hex(&bytes) == sha {
            return Ok(bytes);
        }
    }
    let bytes = get(url).map_err(|e| format!("fetching {what}: {e}"))?;
    if sha256_hex(&bytes) != sha {
        return Err(format!(
            "{what} downloaded does not match the digest this app expects; not used"
        ));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, &bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// A version as a release tag spells it, made safe for a file name: `v0.4.0` is `0.4.0`.
fn version_of(tag: &str) -> Option<String> {
    let v = tag.trim().trim_start_matches(['v', 'V']);
    (!v.is_empty()
        && v.len() <= 64
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+')))
    .then(|| v.to_string())
}

fn cache_name(version: &str) -> String {
    format!("ampr-emu-ng-{version}-libSceAmpr.sprx")
}

/// The newest release, checked against the digest GitHub publishes for its asset.
fn latest(get: &Getter, cache: &Path) -> Result<PackRuntime, String> {
    let body = get(LATEST_API).map_err(|e| format!("asking GitHub for the newest release: {e}"))?;
    let release: Release = serde_json::from_slice(&body)
        .map_err(|e| format!("reading GitHub's answer for the newest release: {e}"))?;
    let version = version_of(&release.tag_name).ok_or_else(|| {
        format!(
            "the newest release's tag {:?} is not a version",
            release.tag_name
        )
    })?;
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == ASSET)
        .ok_or_else(|| format!("release {version} has no {ASSET}"))?;
    let digest = asset
        .digest
        .as_deref()
        .filter(|d| !d.is_empty())
        .ok_or_else(|| format!("GitHub publishes no digest for {ASSET} of release {version}"))?;
    let sha = digest
        .strip_prefix("sha256:")
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| format!("{ASSET} of release {version} has no SHA-256 digest ({digest})"))?;
    if !asset.browser_download_url.starts_with(DOWNLOAD_PREFIX) {
        return Err(format!(
            "{ASSET} of release {version} downloads from {}, not from the project's releases",
            asset.browser_download_url
        ));
    }
    let bytes = fetch_checked(
        get,
        &asset.browser_download_url,
        &sha,
        &cache.join(cache_name(&version)),
        &format!("ampr-emu-ng {version}"),
    )?;
    Ok(PackRuntime {
        name: "ampr-emu-ng",
        version,
        source: "latest",
        fallback_reason: None,
        bytes,
    })
}

/// The emulator `choice` asks for, `get` doing every request and `cache` keeping each version:
/// the newest when it checks out, else `pinned`.
pub(crate) fn resolve(
    choice: RuntimeChoice,
    get: &Getter,
    cache: &Path,
    pinned: Pinned,
) -> Result<PackRuntime, String> {
    let mut fallback_reason = None;
    if choice == RuntimeChoice::Latest {
        match latest(get, cache) {
            Ok(r) => return Ok(r),
            Err(why) => {
                crate::log_warn!(
                    "zstd packs: the newest ampr-emu-ng is not used, {} is: {why}",
                    pinned.version
                );
                fallback_reason = Some(why);
            }
        }
    }
    let bytes = fetch_checked(
        get,
        pinned.url,
        pinned.sha256,
        &cache.join(cache_name(pinned.version)),
        &format!("ampr-emu-ng {}", pinned.version),
    )?;
    Ok(PackRuntime {
        name: "ampr-emu-ng",
        version: pinned.version.to_string(),
        source: "pinned",
        fallback_reason,
        bytes,
    })
}

/// A GET with a time limit and a size cap, as the engine's other downloads do.
pub(crate) fn http_get(url: &str) -> Result<Vec<u8>, String> {
    let mut req = ureq::get(url)
        .header(
            "User-Agent",
            concat!("ps5upload/", env!("CARGO_PKG_VERSION")),
        )
        .config()
        .timeout_global(Some(Duration::from_secs(60)))
        .build();
    if url.starts_with("https://api.github.com/") {
        req = req.header("Accept", "application/vnd.github+json");
    }
    req.call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(LIMIT)
        .read_to_vec()
        .map_err(|e| e.to_string())
}

/// GitHub's answer for the newest release, reused for [`REMEMBER_LATEST`].
static LATEST_ANSWER: Mutex<Option<(Instant, Vec<u8>)>> = Mutex::new(None);

/// [`http_get`], GitHub's release answer reused for a while.
fn http_get_remembered(url: &str) -> Result<Vec<u8>, String> {
    if url != LATEST_API {
        return http_get(url);
    }
    if let Some((at, body)) = LATEST_ANSWER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        if at.elapsed() < REMEMBER_LATEST {
            return Ok(body);
        }
    }
    let body = http_get(url)?;
    *LATEST_ANSWER.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), body.clone()));
    Ok(body)
}

/// The emulator for a packed game, from the network and the data dir's cache.
pub(crate) fn pack_runtime(choice: RuntimeChoice) -> Result<PackRuntime, String> {
    let cache = ps5upload_ava1::pool::data_dir()
        .ok_or_else(|| "no data directory (no HOME)".to_string())?
        .join("cache");
    resolve(choice, &http_get_remembered, &cache, PINNED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    const PIN_URL: &str =
        "https://github.com/quer3q/ampr-emu-ng/releases/download/v0.3.2/libSceAmpr.sprx";
    const NEW_URL: &str =
        "https://github.com/quer3q/ampr-emu-ng/releases/download/v0.4.0/libSceAmpr.sprx";

    fn cache(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ps5upload-zstd-runtime-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn pinned(sha: &str) -> Pinned<'_> {
        Pinned {
            version: "0.3.2",
            url: PIN_URL,
            sha256: sha,
        }
    }

    fn release(digest: Option<&str>, url: &str) -> Vec<u8> {
        let mut asset =
            serde_json::json!({ "name": "libSceAmpr.sprx", "browser_download_url": url });
        if let Some(d) = digest {
            asset["digest"] = d.into();
        }
        serde_json::to_vec(&serde_json::json!({
            "tag_name": "v0.4.0",
            "assets": [{ "name": "README.md", "browser_download_url": url }, asset],
        }))
        .unwrap()
    }

    /// Serves `answers` by URL; anything else is unreachable.
    fn web(answers: Vec<(&str, Vec<u8>)>) -> impl Fn(&str) -> Result<Vec<u8>, String> {
        let map: HashMap<String, Vec<u8>> = answers
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        move |url: &str| {
            map.get(url)
                .cloned()
                .ok_or_else(|| format!("{url}: offline"))
        }
    }

    #[test]
    fn the_newest_release_is_used_when_its_digest_matches() {
        let dir = cache("match");
        let new = b"NEW EMULATOR".to_vec();
        let get = web(vec![
            (
                LATEST_API,
                release(
                    Some(&format!("sha256:{}", sha256_hex(&new).to_uppercase())),
                    NEW_URL,
                ),
            ),
            (NEW_URL, new.clone()),
        ]);
        let pin = sha256_hex(b"PINNED");
        let r = resolve(RuntimeChoice::Latest, &get, &dir, pinned(&pin)).unwrap();
        assert_eq!(
            (r.version.as_str(), r.source, r.bytes),
            ("0.4.0", "latest", new.clone())
        );
        assert_eq!(r.fallback_reason, None);
        // Kept per version, and used from there next time.
        assert_eq!(std::fs::read(dir.join(cache_name("0.4.0"))).unwrap(), new);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_mismatched_digest_falls_back_to_the_pinned_release() {
        let dir = cache("mismatch");
        let pin_bytes = b"PINNED".to_vec();
        let get = web(vec![
            (
                LATEST_API,
                release(Some(&format!("sha256:{}", sha256_hex(b"other"))), NEW_URL),
            ),
            (NEW_URL, b"TAMPERED".to_vec()),
            (PIN_URL, pin_bytes.clone()),
        ]);
        let pin = sha256_hex(&pin_bytes);
        let r = resolve(RuntimeChoice::Latest, &get, &dir, pinned(&pin)).unwrap();
        assert_eq!((r.version.as_str(), r.source), ("0.3.2", "pinned"));
        assert_eq!(r.bytes, pin_bytes);
        assert!(r.fallback_reason.unwrap().contains("does not match"));
        assert!(
            !dir.join(cache_name("0.4.0")).exists(),
            "a mismatch is never kept"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_unusable_digest_falls_back_to_the_pinned_release() {
        let pin_bytes = b"PINNED".to_vec();
        let pin = sha256_hex(&pin_bytes);
        for (n, digest) in [None, Some(""), Some("md5:abcd"), Some("sha256:12")]
            .into_iter()
            .enumerate()
        {
            let dir = cache(&format!("nodigest{n}"));
            let get = web(vec![
                (LATEST_API, release(digest, NEW_URL)),
                (NEW_URL, b"NEW".to_vec()),
                (PIN_URL, pin_bytes.clone()),
            ]);
            let r = resolve(RuntimeChoice::Latest, &get, &dir, pinned(&pin)).unwrap();
            assert_eq!(r.source, "pinned", "{digest:?}");
            assert!(r.fallback_reason.unwrap().contains("digest"), "{digest:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn an_asset_from_elsewhere_or_no_asset_falls_back() {
        let pin_bytes = b"PINNED".to_vec();
        let pin = sha256_hex(&pin_bytes);
        let elsewhere = "https://example.com/libSceAmpr.sprx";
        let no_asset =
            serde_json::to_vec(&serde_json::json!({ "tag_name": "v0.4.0", "assets": [] })).unwrap();
        for (n, answer) in [
            release(Some(&format!("sha256:{}", sha256_hex(b"NEW"))), elsewhere),
            no_asset,
            b"<html>rate limited</html>".to_vec(),
        ]
        .into_iter()
        .enumerate()
        {
            let dir = cache(&format!("elsewhere{n}"));
            let get = web(vec![
                (LATEST_API, answer),
                (elsewhere, b"NEW".to_vec()),
                (PIN_URL, pin_bytes.clone()),
            ]);
            let r = resolve(RuntimeChoice::Latest, &get, &dir, pinned(&pin)).unwrap();
            assert_eq!(r.source, "pinned", "case {n}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn offline_the_cached_pinned_release_is_used_and_without_it_the_build_fails() {
        let dir = cache("offline");
        let pin_bytes = b"PINNED".to_vec();
        let pin = sha256_hex(&pin_bytes);
        let get = web(vec![]);
        let err = resolve(RuntimeChoice::Latest, &get, &dir, pinned(&pin)).unwrap_err();
        assert!(err.contains("offline"), "{err}");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(cache_name("0.3.2")), &pin_bytes).unwrap();
        let r = resolve(RuntimeChoice::Latest, &get, &dir, pinned(&pin)).unwrap();
        assert_eq!((r.source, r.bytes), ("pinned", pin_bytes));
        assert!(r.fallback_reason.unwrap().contains("offline"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tested_choice_never_asks_github() {
        let dir = cache("pinned");
        let pin_bytes = b"PINNED".to_vec();
        let pin = sha256_hex(&pin_bytes);
        let get = |url: &str| {
            assert_ne!(url, LATEST_API, "asked GitHub for the newest release");
            if url == PIN_URL {
                Ok(b"PINNED".to_vec())
            } else {
                Err("offline".to_string())
            }
        };
        let r = resolve(RuntimeChoice::Pinned, &get, &dir, pinned(&pin)).unwrap();
        assert_eq!((r.source, r.fallback_reason), ("pinned", None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tag_is_a_version_only_when_it_is_safe_as_a_file_name() {
        assert_eq!(version_of("v0.4.0").as_deref(), Some("0.4.0"));
        assert_eq!(version_of("0.5.0-rc1").as_deref(), Some("0.5.0-rc1"));
        assert_eq!(version_of("../x"), None);
        assert_eq!(version_of("v"), None);
        assert_eq!(version_of("a/b"), None);
    }

    #[test]
    fn the_choice_reads_as_the_request_spells_it() {
        let c: RuntimeChoice = serde_json::from_str("\"pinned\"").unwrap();
        assert_eq!(c, RuntimeChoice::Pinned);
        let c: RuntimeChoice = serde_json::from_str("\"latest\"").unwrap();
        assert_eq!(c, RuntimeChoice::Latest);
        assert!(serde_json::from_str::<RuntimeChoice>("\"other\"").is_err());
    }
}
