//! Lightweight self-update: check + download + reveal.
//!
//! Deliberately NOT using tauri-plugin-updater / NSIS / ed25519 signing.
//! The whole flow is:
//!
//!   1. `update_check` fetches `latest.json` from the GitHub Releases
//!      endpoint and reports version/notes/asset-url to the renderer.
//!   2. If the renderer decides to update, it calls `update_download`
//!      which streams the platform-appropriate archive into the user's
//!      Downloads folder and opens that folder in the OS file manager.
//!   3. The user manually closes the running app and replaces the
//!      existing binary/bundle with the fresh download.
//!
//! Why not auto-install:
//!   - Cross-platform auto-replace is a lot of platform-specific code
//!     (Windows file-locking dance, macOS .app bundle swapping, etc.)
//!     with real failure modes.
//!   - The user asked explicitly for "download + ask user to replace."
//!   - Without code-signing certs we can't provide the integrity
//!     guarantees that an auto-installer would imply anyway.
//!
//! Manifest shape (written by `scripts/gen-updater-manifest.py`):
//!
//!   {
//!     "version": "2.2.0",
//!     "notes": "…",
//!     "pub_date": "2026-05-01T00:00:00Z",
//!     "assets": {
//!       "darwin-aarch64": "https://.../PS5Upload-2.2.0-mac-arm64.dmg",
//!       "darwin-x86_64":  "https://.../PS5Upload-2.2.0-mac-x64.dmg",
//!       "windows-x86_64": "https://.../PS5Upload-2.2.0-win-x64.zip",
//!       "windows-aarch64":"https://.../PS5Upload-2.2.0-win-arm64.zip",
//!       "linux-x86_64":   "https://.../PS5Upload-2.2.0-linux-x64.zip",
//!       "linux-aarch64":  "https://.../PS5Upload-2.2.0-linux-arm64.zip",
//!       "android":        "https://.../PS5Upload-2.2.0-android.apk",
//!       "android-armv7":  "https://.../PS5Upload-2.2.0-android-armv7.apk"
//!     },
//!     "sha256": { "darwin-aarch64": "<64 hex>", … }
//!   }
//!
//! `sha256` (since 10.2) has the same keys as `assets`; `update_download` checks the file
//! against it before the update is offered for install, and deletes a file that does not
//! match. A manifest without it (an older release) is downloaded unchecked, as before.
//!
//! Missing platforms → the renderer reports "up to date" to users on
//! that arch (same as if no new version exists for them).

use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// Where to fetch the manifest. Stable URL — GitHub redirects
/// `/releases/latest/download/<asset>` to the actual release's asset.
/// Overridable via env var so we can point at a staging release for QA.
const DEFAULT_MANIFEST_URL: &str =
    "https://github.com/phantomptr/ps5upload/releases/latest/download/latest.json";

/// Newest release INCLUDING pre-releases. The API lists newest-first and omits
/// drafts, so the first entry is what the pre-release channel wants.
const PRERELEASE_LIST_URL: &str =
    "https://api.github.com/repos/phantomptr/ps5upload/releases?per_page=10";

/// Which releases the updater is willing to offer.
///
/// Every release is published as a pre-release and promoted to a full release
/// by hand once it has been checked on real hardware (see publish.yml), so
/// "stable" means "a human has signed this off" rather than merely "newest".
/// Stable is the default; opting into pre-releases is a deliberate choice made
/// in Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateChannel {
    Stable,
    Prerelease,
}

impl UpdateChannel {
    fn from_opt(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some("prerelease") | Some("pre") | Some("beta") => Self::Prerelease,
            // Anything else — absent, empty, unrecognised — is stable. An
            // unknown value must never silently opt someone into pre-releases.
            _ => Self::Stable,
        }
    }
}

/// Result of a check. Flat shape so the renderer doesn't have to
/// pattern-match a tagged union. `download_url` is non-empty only when
/// `available == true` AND a bundle was published for the caller's
/// platform — that distinguishes "no update" from "update exists but
/// not for your arch yet." The renderer hides the Download button when
/// `download_url` is empty.
#[derive(Serialize)]
pub struct UpdateCheck {
    pub available: bool,
    pub current_version: String,
    pub latest_version: String,
    pub notes: String,
    pub pub_date: String,
    /// Empty string when this platform isn't represented in the
    /// release manifest (e.g., we only shipped mac + windows today,
    /// the user's on linux-arm64). Non-empty when the Download button
    /// has a target.
    pub download_url: String,
    /// Suggested on-disk filename (the last path segment of
    /// `download_url`). Used by `update_download` to pick the local
    /// save path in ~/Downloads.
    pub download_filename: String,
    /// Lowercase hex SHA-256 the manifest publishes for `download_url`, or empty when it
    /// publishes none (releases before 10.2). Handed back to `update_download`.
    pub sha256: String,
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    pub_date: String,
    #[serde(default)]
    assets: std::collections::HashMap<String, String>,
    /// Same keys as `assets`. Absent before 10.2.
    #[serde(default)]
    sha256: std::collections::HashMap<String, String>,
}

/// The asset this copy should download: its URL and the manifest's SHA-256 for it (empty
/// when there is none). `keys` is `preferred_asset_keys()`, most specific first.
fn pick_asset(manifest: &Manifest, keys: &[String]) -> (String, String) {
    keys.iter()
        .find_map(|k| {
            manifest.assets.get(k).map(|url| {
                let sha = manifest.sha256.get(k).cloned().unwrap_or_default();
                (url.clone(), sha)
            })
        })
        .unwrap_or_default()
}

/// The checksum `update_download` was handed: None when the manifest had none, the
/// lowercase hex when it is well formed. A malformed one is an error, not "unchecked": a
/// manifest that tries to publish a hash and gets it wrong must not be waved through.
fn expected_sha256(raw: Option<&str>) -> Result<Option<String>, String> {
    let raw = raw.map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return Ok(None);
    }
    if raw.len() == 64 && raw.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(Some(raw.to_ascii_lowercase()))
    } else {
        Err(format!(
            "the release manifest's SHA-256 for this download is malformed: {raw:?}"
        ))
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Lowercase hex SHA-256 of a file on disk.
fn sha256_file(path: &std::path::Path) -> std::io::Result<String> {
    use sha2::Digest;
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex_lower(&h.finalize()))
}

/// Ok when `actual` matches `expected` (or nothing was published to check against).
fn check_sha256(expected: Option<&str>, actual: &str, filename: &str) -> Result<(), String> {
    match expected {
        Some(exp) if !exp.eq_ignore_ascii_case(actual) => Err(format!(
            "{filename} does not match the SHA-256 this release published (expected {exp}, got {actual}). \
             The file was deleted and nothing was installed; try the download again."
        )),
        _ => Ok(()),
    }
}

/// Writes one line to stderr without panicking when it is gone (a `println!` would).
fn log_line(args: std::fmt::Arguments<'_>) {
    let _ = std::io::Write::write_fmt(&mut std::io::stderr(), args);
}

/// Platform key used in the manifest. Matches the format tauri-plugin-
/// updater historically uses, which keeps the `scripts/gen-updater-
/// manifest.py` output compatible with either consumer. This function
/// is the single source of truth for which of our supported (os, arch)
/// combinations we're running under.
fn current_platform_key() -> &'static str {
    // The cfg-gated returns compile down to a single string constant
    // per target — zero runtime cost.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return "darwin-aarch64";
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        return "darwin-x86_64";
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return "windows-x86_64";
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        return "windows-aarch64";
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return "linux-x86_64";
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        return "linux-aarch64";
    }
    // Android ships one APK per ABI (publish.yml, --split-per-abi): arm64 as
    // `PS5Upload-<ver>-android.apk` under "android", 32-bit ARM as
    // `-android-armv7.apk` under "android-armv7". Keep in sync with
    // `scripts/gen-updater-manifest.py`.
    #[cfg(all(target_os = "android", target_arch = "arm"))]
    {
        return "android-armv7";
    }
    #[cfg(all(target_os = "android", not(target_arch = "arm")))]
    {
        return "android";
    }
    // iPhone: the unsigned .ipa (arm64 only), which the person sideloads again with their tool.
    // Keep in sync with the `-ios-unsigned.ipa` pattern in scripts/gen-updater-manifest.py.
    #[cfg(target_os = "ios")]
    {
        return "ios";
    }
    #[allow(unreachable_code)]
    {
        "unknown"
    }
}

/// How this Linux copy was installed, if we can tell: `"rpm"`, `"deb"`,
/// or `None` for a portable/unpacked copy.
///
/// We publish `.deb`, `.rpm` AND `.zip` for Linux, but the updater only
/// ever offered the `.zip`. Someone who installed the RPM was told to
/// download a tarball — they cannot `dnf update` it, and unpacking it
/// leaves a second copy alongside the packaged one. A user asked for this
/// directly.
///
/// The authoritative signal is the package database, not the distro: ask
/// which package owns our own executable. A user can perfectly well run
/// the portable zip on Fedora, so keying off `/etc/os-release` would
/// hand them an RPM they never installed. If neither package manager
/// claims the file, it is portable and the plain key is right.
#[cfg(target_os = "linux")]
fn linux_package_kind() -> Option<&'static str> {
    fn owns(tool: &str, args: &[&str]) -> bool {
        std::process::Command::new(tool)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|st| st.success())
            .unwrap_or(false)
    }
    // Resolve symlinks: /usr/bin/ps5upload may point into /opt, and the
    // package database knows the real path.
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let exe = exe.to_str()?;
    if owns("rpm", &["-qf", exe]) {
        return Some("rpm");
    }
    if owns("dpkg", &["-S", exe]) {
        return Some("deb");
    }
    None
}

/// Whether this Windows copy came from the setup installer. Tauri's NSIS
/// installer puts `uninstall.exe` beside the app; the portable zip has none.
/// An installed copy was being offered the portable zip, so "updating" left a
/// second, unmanaged copy in Downloads (a user report).
#[cfg(target_os = "windows")]
fn windows_installed() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("uninstall.exe")))
        .is_some_and(|u| u.is_file())
}

/// Manifest keys to try, most specific first.
///
/// On Linux a packaged install prefers its own format (`linux-x86_64-rpm`)
/// and falls back to the portable `.zip` key when the release predates
/// those entries — so an older manifest still updates rather than
/// reporting "no build for your platform".
fn preferred_asset_keys() -> Vec<String> {
    let base = current_platform_key();
    #[cfg(target_os = "linux")]
    {
        if let Some(kind) = linux_package_kind() {
            return vec![format!("{base}-{kind}"), base.to_string()];
        }
    }
    // An installed Windows copy wants the installer; a release that predates
    // the `-setup` entries still offers the zip rather than nothing.
    #[cfg(target_os = "windows")]
    {
        if windows_installed() {
            return vec![format!("{base}-setup"), base.to_string()];
        }
    }
    vec![base.to_string()]
}

/// Minimal semver-ish comparison. Splits MAJOR.MINOR.PATCH numerics and
/// honours pre-release suffix order per semver: a pre-release version
/// (`2.2.0-rc1`) is LESS THAN the GA of the same numeric (`2.2.0`).
/// Without that, a user on `2.2.0-rc1` would never see `2.2.0` GA as
/// "available", which silently strands them on the rc.
///
/// Build metadata after `+` is ignored entirely (semver rule).
/// Returns true iff `latest` is strictly greater than `current`.
fn is_newer(current: &str, latest: &str) -> bool {
    /// Returns (numeric_parts, has_prerelease). `2.2.0-rc1` →
    /// (`[2,2,0]`, true). `2.2.0+sha.abc` → (`[2,2,0]`, false).
    /// `2.2.0-rc1+sha.abc` → (`[2,2,0]`, true).
    fn parts(s: &str) -> (Vec<u64>, bool) {
        // Strip build-metadata first (`+...`), THEN check for `-`
        // pre-release. Order matters: `2.2.0+rc1` is build-metadata
        // (no pre-release) while `2.2.0-rc1` is a pre-release.
        let no_build = s.split('+').next().unwrap_or(s);
        let mut split = no_build.splitn(2, '-');
        let core = split.next().unwrap_or("");
        let has_pre = split.next().is_some();
        let nums: Vec<u64> = core
            .split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect();
        (nums, has_pre)
    }
    let (c, c_pre) = parts(current);
    let (l, l_pre) = parts(latest);
    let len = c.len().max(l.len());
    for i in 0..len {
        let cv = c.get(i).copied().unwrap_or(0);
        let lv = l.get(i).copied().unwrap_or(0);
        if lv > cv {
            return true;
        }
        if lv < cv {
            return false;
        }
    }
    // Numerics equal — pre-release ordering decides:
    //   current GA, latest GA   → equal, return false
    //   current GA, latest pre  → latest is OLDER (pre < GA), return false
    //   current pre, latest GA  → latest is NEWER, return true
    //   current pre, latest pre → equal-ish (we don't compare suffix
    //                              text), return false
    c_pre && !l_pre
}

#[cfg(test)]
mod is_newer_tests {
    use super::{
        install_command_for, is_newer, is_safe_update_url, UpdateChannel, DEFAULT_MANIFEST_URL,
        PRERELEASE_LIST_URL,
    };

    #[test]
    fn a_packaged_linux_install_gets_its_own_command() {
        let p = "/home/me/Downloads/PS5Upload-7.0.23-linux-x64.rpm";
        assert_eq!(
            install_command_for(p, Some("rpm"), true, Some("ps5-upload")).as_deref(),
            Some("rpm-ostree uninstall ps5-upload --install '/home/me/Downloads/PS5Upload-7.0.23-linux-x64.rpm' && systemctl reboot")
        );
        assert_eq!(
            install_command_for(p, Some("rpm"), false, None).as_deref(),
            Some("sudo dnf install '/home/me/Downloads/PS5Upload-7.0.23-linux-x64.rpm'")
        );
        assert_eq!(
            install_command_for("/tmp/it's.deb", Some("deb"), false, None).as_deref(),
            Some("sudo apt install '/tmp/it'\\''s.deb'")
        );
        assert_eq!(install_command_for(p, None, true, None), None);
    }

    #[test]
    fn higher_numeric_is_newer() {
        assert!(is_newer("2.2.0", "2.2.1"));
        assert!(is_newer("2.2.0", "2.3.0"));
        assert!(is_newer("2.2.0", "3.0.0"));
    }

    #[test]
    fn lower_numeric_is_not_newer() {
        assert!(!is_newer("2.2.1", "2.2.0"));
        assert!(!is_newer("3.0.0", "2.9.9"));
    }

    #[test]
    fn equal_numeric_is_not_newer() {
        assert!(!is_newer("2.2.0", "2.2.0"));
    }

    #[test]
    fn rc_user_sees_ga_as_newer() {
        // The Bug U fix: someone on the release candidate must see
        // the GA as available.
        assert!(is_newer("2.2.0-rc1", "2.2.0"));
        // Two pre-release versions with the same numeric triple are
        // treated as equal — we don't compare suffix text. Use `!`
        // form rather than `== false` to satisfy `clippy::bool_comparison`.
        assert!(!is_newer("2.2.0-rc1", "2.2.0-rc2"));
        assert!(is_newer("2.2.0-beta", "2.2.0"));
    }

    #[test]
    fn ga_user_does_not_see_rc_as_newer() {
        // Conversely, GA users shouldn't be downgraded to a pre-release.
        assert!(!is_newer("2.2.0", "2.2.0-rc1"));
        assert!(!is_newer("2.2.0", "2.2.0-beta"));
    }

    #[test]
    fn unknown_channel_values_stay_on_stable() {
        // Getting this backwards would silently move people onto pre-releases,
        // which is the one direction that must never happen by accident.
        assert_eq!(UpdateChannel::from_opt(None), UpdateChannel::Stable);
        assert_eq!(UpdateChannel::from_opt(Some("")), UpdateChannel::Stable);
        assert_eq!(
            UpdateChannel::from_opt(Some("stable")),
            UpdateChannel::Stable
        );
        assert_eq!(
            UpdateChannel::from_opt(Some("nonsense")),
            UpdateChannel::Stable
        );
        assert_eq!(
            UpdateChannel::from_opt(Some("prerelease")),
            UpdateChannel::Prerelease
        );
        assert_eq!(
            UpdateChannel::from_opt(Some("  prerelease  ")),
            UpdateChannel::Prerelease
        );
    }

    #[test]
    fn the_releases_api_host_is_pinned() {
        // The pre-release channel resolves through api.github.com, so it has to
        // pass the production pin or the channel is dead on arrival.
        assert!(is_safe_update_url(PRERELEASE_LIST_URL));
        assert!(is_safe_update_url(DEFAULT_MANIFEST_URL));
        assert!(!is_safe_update_url("https://evil.example.com/latest.json"));
    }

    #[test]
    fn build_metadata_ignored() {
        // Per semver, +build-metadata doesn't affect ordering.
        assert!(!is_newer("2.2.0", "2.2.0+sha.abc"));
        assert!(!is_newer("2.2.0+sha.def", "2.2.0+sha.abc"));
    }
}

/// Hard cap on the manifest body size. A well-formed latest.json for
/// our six supported platforms is under 2 KiB; 64 KiB leaves headroom
/// for notes and future expansion without letting a misconfigured or
/// hostile endpoint wedge the app by dripping gigabytes into reqwest's
/// default-unlimited body buffer.
const MANIFEST_MAX_BYTES: usize = 64 * 1024;

/// Ask the releases API for the newest release of any kind and return its
/// `latest.json` asset URL.
///
/// Only used by the pre-release channel. Any failure here is reported rather
/// than silently falling back to stable: a user who opted into pre-releases
/// and is quietly served a stable manifest has no way to tell.
async fn prerelease_manifest_url(client: &reqwest::Client) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
    }
    #[derive(serde::Deserialize)]
    struct Release {
        draft: bool,
        assets: Vec<Asset>,
    }
    let resp = client
        .get(PRERELEASE_LIST_URL)
        // The API rejects requests without one.
        .header("User-Agent", "ps5upload-updater")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("fetch release list: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("release list HTTP {}", resp.status()));
    }
    let releases: Vec<Release> = resp
        .json()
        .await
        .map_err(|e| format!("parse release list: {e}"))?;
    let url = releases
        .into_iter()
        .filter(|r| !r.draft)
        .find_map(|r| {
            r.assets
                .into_iter()
                .find(|a| a.name == "latest.json")
                .map(|a| a.browser_download_url)
        })
        .ok_or_else(|| "no release with a latest.json asset was found".to_string())?;
    Ok(url)
}

async fn fetch_manifest(channel: UpdateChannel) -> Result<Manifest, String> {
    let env_override = std::env::var("PS5UPLOAD_UPDATE_MANIFEST_URL").ok();
    let resolver = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("updater client init: {e}"))?;
    let url = match (&env_override, channel) {
        (Some(u), _) => u.clone(),
        (None, UpdateChannel::Prerelease) => prerelease_manifest_url(&resolver).await?,
        (None, UpdateChannel::Stable) => DEFAULT_MANIFEST_URL.to_string(),
    };
    // Production default URL requires HTTPS + a pinned GitHub host.
    // Env-overridden URL (dev/staging) gets a relaxed check that only
    // requires HTTPS or loopback HTTP — pinning would block a tester
    // from pointing at https://staging-internal.example.com/latest.json,
    // which the comment in this module's history has always promised
    // would work. The relaxation is acceptable because the env var is
    // set explicitly by the dev (not user input the way the manifest
    // contents are), so the threat model is different.
    let is_safe = if env_override.is_some() {
        is_safe_dev_url(&url)
    } else {
        is_safe_update_url(&url)
    };
    if !is_safe {
        return Err(format!(
            "refusing to fetch update manifest over insecure URL: {url}"
        ));
    }
    let resp = resolver
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("manifest fetch HTTP {}: {url}", resp.status()));
    }
    // Bound the body before JSON parsing. The Content-Length pre-check is
    // only an early-out: content_length() is None for chunked
    // transfer-encoding, so a Some-only guard is bypassable and
    // `resp.bytes()` would then buffer the entire body before any cap
    // fires. Stream with a running-total cap (mirroring update_download)
    // so an absent Content-Length cannot defeat the limit.
    if let Some(len) = resp.content_length() {
        if len as usize > MANIFEST_MAX_BYTES {
            return Err(format!(
                "manifest body too large ({len} bytes > {MANIFEST_MAX_BYTES} cap)"
            ));
        }
    }
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("read manifest body: {e}"))?;
        if body.len().saturating_add(chunk.len()) > MANIFEST_MAX_BYTES {
            return Err(format!(
                "manifest body too large (> {MANIFEST_MAX_BYTES} cap)"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice::<Manifest>(&body).map_err(|e| format!("parse manifest: {e}"))
}

#[tauri::command]
pub async fn update_check(app: AppHandle, channel: Option<String>) -> Result<UpdateCheck, String> {
    let current_version = app.package_info().version.to_string();
    let manifest = fetch_manifest(UpdateChannel::from_opt(channel.as_deref())).await?;
    let latest_version = manifest.version.clone();
    let available = is_newer(&current_version, &latest_version);
    // Most-specific-first: a packaged Linux install gets its own format,
    // everything else gets the single platform key.
    let (download_url, sha256) = pick_asset(&manifest, &preferred_asset_keys());
    let download_filename = download_url.rsplit('/').next().unwrap_or("").to_string();
    Ok(UpdateCheck {
        available,
        current_version,
        latest_version,
        notes: manifest.notes,
        pub_date: manifest.pub_date,
        download_url,
        download_filename,
        sha256,
    })
}

/// Resolve the best "Downloads" folder for the host. We try the
/// standard one first; if it's missing (CI / sandboxed accounts) we
/// fall back to the Tauri app's own config dir so we always have
/// somewhere writeable to land the file.
/// Where `update_download` saves updates (`update_install` installs only from here).
pub(crate) fn downloads_dir(app: &AppHandle) -> Result<PathBuf, String> {
    resolve_downloads_dir(app)
}

fn resolve_downloads_dir(app: &AppHandle) -> Result<PathBuf, String> {
    if let Ok(p) = app.path().download_dir() {
        if p.exists() || std::fs::create_dir_all(&p).is_ok() {
            return Ok(p);
        }
    }
    // Fallback: ~/.config/PS5Upload/downloads (or platform equivalent).
    let cfg = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("no writeable downloads path: {e}"))?;
    let out = cfg.join("downloads");
    std::fs::create_dir_all(&out).map_err(|e| format!("mkdir {out:?}: {e}"))?;
    Ok(out)
}

#[derive(Serialize)]
pub struct UpdateDownload {
    /// Absolute path to where the file was saved.
    pub path: String,
    /// Total bytes written to disk.
    pub bytes: u64,
    /// For a packaged Linux install: the one command that installs the download, to copy into
    /// a terminal (#456: on Bazzite, `rpm-ostree uninstall ps5-upload --install <rpm>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,
    /// `update_install` can install this download and restart into it (commands/self_install).
    pub can_install: bool,
    /// The file matched the SHA-256 the release manifest published. False only when the
    /// manifest published none (a release before 10.2); a mismatch is an error instead.
    pub verified: bool,
}

/// `path` quoted for a POSIX shell.
fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

/// The command that installs `path` over this packaged Linux copy, or None (portable copy,
/// another OS). An rpm-ostree system (Bazzite, Silverblue, Kinoite) cannot `dnf install`; its
/// layered package is swapped in one transaction that applies on the next boot.
fn install_command_for(
    path: &str,
    kind: Option<&str>,
    ostree: bool,
    package: Option<&str>,
) -> Option<String> {
    let q = shell_quote(path);
    match kind? {
        "rpm" if ostree => Some(format!(
            "rpm-ostree uninstall {} --install {q} && systemctl reboot",
            package.unwrap_or("ps5-upload")
        )),
        "rpm" => Some(format!("sudo dnf install {q}")),
        "deb" => Some(format!("sudo apt install {q}")),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn install_command(path: &str) -> Option<String> {
    let kind = linux_package_kind();
    let ostree = std::path::Path::new("/run/ostree-booted").exists();
    let package = (kind == Some("rpm"))
        .then(|| {
            let exe = std::env::current_exe().ok()?;
            let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
            let out = std::process::Command::new("rpm")
                .args(["-qf", "--qf", "%{NAME}"])
                .arg(&exe)
                .output()
                .ok()?;
            let name = String::from_utf8(out.stdout).ok()?;
            let name = name.trim();
            (out.status.success() && !name.is_empty() && !name.contains(char::is_whitespace))
                .then(|| name.to_string())
        })
        .flatten();
    install_command_for(path, kind, ostree, package.as_deref())
}

#[cfg(not(target_os = "linux"))]
fn install_command(path: &str) -> Option<String> {
    let _ = install_command_for;
    let _ = path;
    None
}

/// Download the platform-specific asset from the URL returned by
/// `update_check` into the user's Downloads folder, then reveal it in
/// the OS file manager. Skips re-downloading if the target file
/// already exists with the same size reported by Content-Length — a
/// user who closed the app mid-install shouldn't have to wait for the
/// whole thing again on the next click.
#[tauri::command]
pub async fn update_download(
    app: AppHandle,
    url: String,
    filename: String,
    sha256: Option<String>,
) -> Result<UpdateDownload, String> {
    let expected = expected_sha256(sha256.as_deref())?;
    if url.is_empty() {
        return Err("no download URL — this release has no bundle for your platform".into());
    }
    // Mirror the manifest-fetch scheme requirement. The download URL
    // comes from the remote manifest and we verify TLS via rustls, but
    // if a future misconfiguration ever let a plain-http URL slip into
    // `assets`, we'd happily download it over the clear. Refuse up
    // front. Honour the same env-override relaxation as fetch_manifest:
    // when the dev set PS5UPLOAD_UPDATE_MANIFEST_URL, asset URLs in the
    // resulting manifest may legitimately point at non-GitHub hosts
    // (the staging-internal CDN, etc.); pinning would block the
    // download even though the manifest fetch was allowed.
    let download_is_safe = if std::env::var("PS5UPLOAD_UPDATE_MANIFEST_URL").is_ok() {
        is_safe_dev_url(&url)
    } else {
        is_safe_update_url(&url)
    };
    if !download_is_safe {
        return Err(format!("refusing to download over insecure URL: {url}"));
    }
    // Tight basename validation. The filename comes from the update
    // manifest's URL-last-segment, which we trust to be well-formed —
    // but since it's ultimately a remote input parsed by `update_check`,
    // defense in depth:
    //   - `/` `\` rule out path separators so we can't drop into another
    //     directory.
    //   - `.` `..` rule out directory references.
    //   - NUL rules out C-string early-terminators (would still fail on
    //     create, but the error would be confusing).
    //   - leading dot rules out hidden-file shenanigans.
    if filename.is_empty()
        || filename == "."
        || filename == ".."
        || filename.starts_with('.')
        || filename.contains('/')
        || filename.contains('\\')
        || filename.contains('\0')
    {
        return Err(format!("invalid filename: {filename:?}"));
    }
    let dl_dir = resolve_downloads_dir(&app)?;
    let target = dl_dir.join(&filename);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60 * 10))
        .build()
        .map_err(|e| format!("download client init: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("download {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download HTTP {}: {url}", resp.status()));
    }
    let expected_len = resp.content_length();
    // Reject obviously-too-large downloads up front. The full app is
    // ~50 MB; a legitimate update would never approach 500 MB. A cap
    // here stops a misconfigured or hostile endpoint from dripping
    // gigabytes into the user's Downloads folder. The stream loop
    // below does its own running-total check so a Content-Length=0 /
    // unknown-length response is also bounded.
    const DOWNLOAD_MAX_BYTES: u64 = 500 * 1024 * 1024;
    if let Some(len) = expected_len {
        if len > DOWNLOAD_MAX_BYTES {
            return Err(format!(
                "download too large ({len} bytes > {DOWNLOAD_MAX_BYTES} cap) — refusing"
            ));
        }
    }

    // Skip if we already have a complete copy from a prior run. We
    // stream to `<target>.part` and atomic-rename only after the
    // stream succeeds (see PartialDownloadGuard below), so `target`
    // existing at all means a prior invocation finished end-to-end.
    // Two cases to handle:
    //   1. Server sent Content-Length → verify byte-exact match
    //      (catches the rare "same filename, different size release"
    //      edge case).
    //   2. Server omitted Content-Length (GitHub CDN redirects to
    //      release-assets.githubusercontent.com often drop it) → trust
    //      that the file is complete because only the atomic-rename
    //      path produces it. Without this branch the user re-downloads
    //      the same bundle on every click.
    if let Ok(meta) = std::fs::metadata(&target) {
        let skip = match expected_len {
            Some(expected) => meta.len() == expected,
            None => meta.len() > 0,
        };
        // A copy already here is only reused when it matches the published checksum; one
        // that does not (a different build under the same name, a damaged file) is
        // deleted and downloaded again.
        let matches = match &expected {
            _ if !skip => false,
            None => true,
            Some(exp) => {
                let t = target.clone();
                tokio::task::spawn_blocking(move || sha256_file(&t))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .is_some_and(|actual| actual == *exp)
            }
        };
        if skip && !matches {
            let _ = std::fs::remove_file(&target);
        }
        if skip && matches {
            // `reveal` is best-effort for the same reason as the fresh-download
            // path below: the bundle is already complete on disk, so a failure
            // to open the OS file manager must not be reported back to the
            // renderer as a download failure (a user re-clicking "Download"
            // would otherwise see a spurious error).
            let path = target.to_string_lossy().into_owned();
            let can_install = super::self_install::can_install(&path);
            if !can_install {
                reveal(&target);
            }
            return Ok(UpdateDownload {
                install_command: install_command(&path),
                can_install,
                verified: expected.is_some(),
                path,
                bytes: meta.len(),
            });
        }
    }

    // Stream to disk so a multi-hundred-MB bundle doesn't balloon
    // process RAM. `.chunk()` yields network-sized buffers that we
    // write straight through.
    let tmp = target.with_extension("part");
    // Cleanup guard: any early-return from here on removes the `.part`
    // file. Without this, a mid-stream network error or a disk-full
    // write failure would leak a partial file into ~/Downloads that
    // would confuse the next call's "skip if already fully downloaded"
    // optimization and waste user disk. The guard runs in its Drop
    // impl, so every `?`-return path triggers cleanup.
    struct PartialDownloadGuard<'a> {
        path: &'a std::path::Path,
        armed: bool,
    }
    impl Drop for PartialDownloadGuard<'_> {
        fn drop(&mut self) {
            if self.armed {
                let _ = std::fs::remove_file(self.path);
            }
        }
    }
    let mut cleanup = PartialDownloadGuard {
        path: &tmp,
        armed: true,
    };

    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("create {tmp:?}: {e}"))?;
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    let mut total: u64 = 0;
    let mut hasher = {
        use sha2::Digest;
        sha2::Sha256::new()
    };
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("chunk read: {e}"))?;
        total = total.saturating_add(chunk.len() as u64);
        if total > DOWNLOAD_MAX_BYTES {
            return Err(format!(
                "download exceeded {DOWNLOAD_MAX_BYTES} bytes mid-stream — aborting"
            ));
        }
        file.write_all(&chunk).map_err(|e| format!("write: {e}"))?;
        sha2::Digest::update(&mut hasher, &chunk);
    }
    file.sync_all().map_err(|e| format!("fsync: {e}"))?;
    drop(file);
    // Checked before the rename: a file that does not match never takes the update's name,
    // and the guard deletes the `.part`.
    let actual = hex_lower(&sha2::Digest::finalize(hasher));
    check_sha256(expected.as_deref(), &actual, &filename)?;
    if expected.is_none() {
        log_line(format_args!(
            "[updates] the manifest publishes no SHA-256 for {filename} (a release before 10.2): not verified, sha256={actual}\n"
        ));
    }
    super::replace_file(&tmp, &target).map_err(|e| format!("rename: {e}"))?;
    // Rename succeeded — disarm the guard so it doesn't try to remove
    // the already-moved file. (A Drop-safe `rename_if_exists` is
    // another option, but the guard pattern is simpler here.)
    cleanup.armed = false;

    // `reveal` is best-effort. If the OS file manager fails to open
    // (sandboxed session, display server unavailable, exotic DE), the
    // download itself succeeded — we shouldn't convert that into a
    // download-failed error for the user. Log + continue.
    let path = target.to_string_lossy().into_owned();
    let can_install = super::self_install::can_install(&path);
    // A copy that installs it itself shows "Install and restart": opening the folder (or, on
    // Windows, starting the setup, which then asks to close the running app) only gets in
    // the way.
    if !can_install {
        reveal(&target);
    }
    Ok(UpdateDownload {
        install_command: install_command(&path),
        can_install,
        verified: expected.is_some(),
        path,
        bytes: total,
    })
}

/// Shows the download where the person installs it by hand: the Windows setup is started, else
/// the folder opens in the file manager. Best effort, and never waited on: on macOS the `open`
/// crate waits for `/usr/bin/open`, which can sit for as long as Finder does (33 min with the
/// screen locked, measured) — awaited here it held the download, so the app showed
/// "Downloading…" with the file long saved.
fn reveal(path: &std::path::Path) {
    let setup = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().ends_with("-setup.exe"));
    let Some(target) = (if setup { Some(path) } else { path.parent() }) else {
        return;
    };
    let target = target.to_path_buf();
    let _ = std::thread::Builder::new()
        .name("reveal-download".into())
        .spawn(move || {
            if let Err(e) = open::that_detached(&target) {
                let _ = std::io::Write::write_fmt(
                    &mut std::io::stderr(),
                    format_args!("[updates] could not show {target:?}: {e}\n"),
                );
            }
        });
}

/// Hosts allowed for production-grade update HTTPS URLs. We pin to
/// GitHub-owned origins because all releases ship through
/// `github.com/phantomptr/ps5upload/releases/` (manifest URL,
/// `latest.json`) and `*.githubusercontent.com` (release-asset
/// redirect target — `release-assets.githubusercontent.com` and
/// `objects.githubusercontent.com` are the actual CDN hostnames the
/// download follows). Without this pin, TLS alone is the integrity
/// boundary: a poisoned manifest published via a compromised release
/// pipeline could redirect downloads to attacker-controlled HTTPS
/// without our shell noticing. Pinning closes that gap as long as
/// the manifest itself is hosted on github.com.
const PINNED_PRODUCTION_HOSTS: &[&str] = &[
    "github.com",
    // The pre-release channel resolves its manifest through the releases API:
    // github.com/releases/latest/download/... deliberately skips pre-releases,
    // which is exactly what makes it the right default for the stable channel
    // and useless for the other one.
    "api.github.com",
    "release-assets.githubusercontent.com",
    "objects.githubusercontent.com",
    "raw.githubusercontent.com",
];

fn is_safe_update_url(raw: &str) -> bool {
    is_safe_update_url_for_host(raw, PINNED_PRODUCTION_HOSTS)
}

fn is_safe_update_url_for_host(raw: &str, allowed_hosts: &[&str]) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    match url.scheme() {
        "https" => match url.host_str() {
            Some(host) => allowed_hosts.contains(&host),
            None => false,
        },
        "http" => matches!(
            url.host_str(),
            Some("127.0.0.1") | Some("localhost") | Some("::1") | Some("[::1]")
        ),
        _ => false,
    }
}

/// Relaxed safety check for the dev/staging env-override path. Allows
/// any HTTPS host (no pin) plus loopback HTTP. Used only when the
/// `PS5UPLOAD_UPDATE_MANIFEST_URL` env var is set explicitly — the
/// developer setting the variable is opting into a less-pinned
/// origin. The threat model here is "dev points at internal staging
/// server", which is fundamentally different from "user runs the
/// shipped binary against the GitHub default".
fn is_safe_dev_url(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    match url.scheme() {
        "https" => url.host_str().is_some(),
        "http" => matches!(
            url.host_str(),
            Some("127.0.0.1") | Some("localhost") | Some("::1") | Some("[::1]")
        ),
        _ => false,
    }
}

#[cfg(test)]
mod checksum_tests {
    use super::*;

    fn manifest(json: &str) -> Manifest {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_manifest_without_checksums_still_parses_and_picks_its_asset() {
        // The shape every release before 10.2 published.
        let m = manifest(
            r#"{"version":"10.1.0","assets":{"linux-x86_64":"https://github.com/a/b.zip"}}"#,
        );
        assert_eq!(
            pick_asset(&m, &["linux-x86_64-deb".into(), "linux-x86_64".into()]),
            ("https://github.com/a/b.zip".to_string(), String::new())
        );
        assert_eq!(
            pick_asset(&m, &["darwin-aarch64".into()]),
            Default::default()
        );
    }

    #[test]
    fn the_checksum_comes_from_the_same_key_as_the_url() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let m = manifest(&format!(
            r#"{{"version":"10.2.0",
                "assets":{{"linux-x86_64":"https://github.com/z.zip","linux-x86_64-deb":"https://github.com/z.deb"}},
                "sha256":{{"linux-x86_64":"{a}","linux-x86_64-deb":"{b}"}}}}"#
        ));
        assert_eq!(
            pick_asset(&m, &["linux-x86_64-deb".into(), "linux-x86_64".into()]),
            ("https://github.com/z.deb".to_string(), b.clone())
        );
        assert_eq!(
            pick_asset(&m, &["linux-x86_64".into()]),
            ("https://github.com/z.zip".to_string(), a)
        );
    }

    #[test]
    fn expected_checksums_are_normalised_or_refused() {
        assert_eq!(expected_sha256(None), Ok(None));
        assert_eq!(expected_sha256(Some("  ")), Ok(None));
        assert_eq!(
            expected_sha256(Some(&"AB".repeat(32))),
            Ok(Some("ab".repeat(32)))
        );
        assert!(expected_sha256(Some("abc")).is_err());
        assert!(expected_sha256(Some(&"g".repeat(64))).is_err());
    }

    #[test]
    fn a_file_is_checked_against_the_published_hash() {
        let dir = std::env::temp_dir().join(format!("ps5upload-sha-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("update.bin");
        std::fs::write(&f, b"abc").unwrap();
        let actual = sha256_file(&f).unwrap();
        // The well-known SHA-256 of "abc".
        assert_eq!(
            actual,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(check_sha256(Some(&actual), &actual, "update.bin").is_ok());
        assert!(check_sha256(Some(&actual.to_ascii_uppercase()), &actual, "update.bin").is_ok());
        assert!(check_sha256(None, &actual, "update.bin").is_ok());
        let err = check_sha256(Some(&"0".repeat(64)), &actual, "update.bin").unwrap_err();
        assert!(
            err.contains("update.bin") && err.contains("deleted"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::is_safe_update_url;

    #[test]
    fn update_urls_require_https_except_loopback_http() {
        // Pinned production hosts pass.
        assert!(is_safe_update_url(
            "https://github.com/phantomptr/ps5upload/latest.json"
        ));
        assert!(is_safe_update_url(
            "https://release-assets.githubusercontent.com/foo/bar"
        ));
        assert!(is_safe_update_url(
            "https://objects.githubusercontent.com/foo/bar"
        ));
        // Loopback http still passes (env-override staging tests).
        assert!(is_safe_update_url("http://127.0.0.1:8000/latest.json"));
        assert!(is_safe_update_url("http://localhost:8000/latest.json"));
        assert!(is_safe_update_url("http://[::1]:8000/latest.json"));

        // Plain http on github is rejected (TLS required for prod).
        assert!(!is_safe_update_url(
            "http://github.com/phantomptr/ps5upload/latest.json"
        ));
        // Hostnames that look-alike but aren't loopback are rejected.
        assert!(!is_safe_update_url(
            "http://127.0.0.1.evil.test/latest.json"
        ));
        // Non-HTTP(S) schemes rejected.
        assert!(!is_safe_update_url("ftp://github.com/latest.json"));
        assert!(!is_safe_update_url("not a url"));
        // HTTPS to a non-pinned host is rejected — this is the new
        // host-pinning protection: a poisoned manifest can't redirect
        // downloads to an attacker's HTTPS server.
        assert!(!is_safe_update_url(
            "https://attacker.example.com/PS5Upload-pwn.dmg"
        ));
        assert!(!is_safe_update_url(
            "https://github-attacker.com/latest.json"
        ));
    }
}
