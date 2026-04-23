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
//!       "linux-aarch64":  "https://.../PS5Upload-2.2.0-linux-arm64.zip"
//!     }
//!   }
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
}

/// Platform key used in the manifest. Matches the format tauri-plugin-
/// updater historically uses, which keeps the `scripts/gen-updater-
/// manifest.py` output compatible with either consumer. This function
/// is the single source of truth for which of our six (os, arch)
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
    #[allow(unreachable_code)]
    {
        "unknown"
    }
}

/// Minimal semver-ish comparison: split on '.', compare numeric
/// components, ignore pre-release suffixes. Good enough for the
/// version shape we actually publish (MAJOR.MINOR.PATCH). Returns
/// true iff `latest` is strictly greater than `current`.
fn is_newer(current: &str, latest: &str) -> bool {
    fn parts(s: &str) -> Vec<u64> {
        // Drop pre-release tail at '-' or '+' before splitting so
        // "2.2.0-rc1" parses as [2, 2, 0].
        let core = s.split(['-', '+']).next().unwrap_or(s);
        core.split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    }
    let c = parts(current);
    let l = parts(latest);
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
    false
}

/// Hard cap on the manifest body size. A well-formed latest.json for
/// our six supported platforms is under 2 KiB; 64 KiB leaves headroom
/// for notes and future expansion without letting a misconfigured or
/// hostile endpoint wedge the app by dripping gigabytes into reqwest's
/// default-unlimited body buffer.
const MANIFEST_MAX_BYTES: usize = 64 * 1024;

async fn fetch_manifest() -> Result<Manifest, String> {
    let url = std::env::var("PS5UPLOAD_UPDATE_MANIFEST_URL")
        .unwrap_or_else(|_| DEFAULT_MANIFEST_URL.to_string());
    // Require HTTPS for the manifest source — the update flow trusts
    // the manifest's `assets` URLs to be safe (we eventually drop the
    // download into the user's Downloads folder), so letting a plain
    // `http://` origin poison those URLs is a real concern. The env
    // override is still honored because a dev tester pointing at a
    // local staging server is a reasonable use case; gate HTTPS only
    // on the production default.
    if !url.starts_with("https://") && !url.starts_with("http://127.0.0.1") {
        return Err(format!(
            "refusing to fetch update manifest over insecure URL: {url}"
        ));
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("updater client init: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("manifest fetch HTTP {}: {url}", resp.status()));
    }
    // Bound the body before JSON parsing. `resp.bytes()` would read
    // unbounded; clamp via content-length + a safety cap.
    if let Some(len) = resp.content_length() {
        if len as usize > MANIFEST_MAX_BYTES {
            return Err(format!(
                "manifest body too large ({len} bytes > {MANIFEST_MAX_BYTES} cap)"
            ));
        }
    }
    let body = resp
        .bytes()
        .await
        .map_err(|e| format!("read manifest body: {e}"))?;
    if body.len() > MANIFEST_MAX_BYTES {
        return Err(format!(
            "manifest body too large after read ({} bytes > {MANIFEST_MAX_BYTES} cap)",
            body.len()
        ));
    }
    serde_json::from_slice::<Manifest>(&body)
        .map_err(|e| format!("parse manifest: {e}"))
}

#[tauri::command]
pub async fn update_check(app: AppHandle) -> Result<UpdateCheck, String> {
    let current_version = app.package_info().version.to_string();
    let manifest = fetch_manifest().await?;
    let latest_version = manifest.version.clone();
    let available = is_newer(&current_version, &latest_version);
    let key = current_platform_key();
    let download_url = manifest.assets.get(key).cloned().unwrap_or_default();
    let download_filename = download_url
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    Ok(UpdateCheck {
        available,
        current_version,
        latest_version,
        notes: manifest.notes,
        pub_date: manifest.pub_date,
        download_url,
        download_filename,
    })
}

/// Resolve the best "Downloads" folder for the host. We try the
/// standard one first; if it's missing (CI / sandboxed accounts) we
/// fall back to the Tauri app's own config dir so we always have
/// somewhere writeable to land the file.
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
) -> Result<UpdateDownload, String> {
    if url.is_empty() {
        return Err("no download URL — this release has no bundle for your platform".into());
    }
    // Mirror the manifest-fetch scheme requirement. The download URL
    // comes from the remote manifest and we verify TLS via rustls, but
    // if a future misconfiguration ever let a plain-http URL slip into
    // `assets`, we'd happily download it over the clear. Refuse up
    // front. (Same 127.0.0.1 escape hatch for local staging tests.)
    if !url.starts_with("https://") && !url.starts_with("http://127.0.0.1") {
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

    // Skip if we already have a complete copy from a prior run.
    if let (Some(expected), Ok(meta)) = (expected_len, std::fs::metadata(&target)) {
        if meta.len() == expected {
            reveal(&app, &target).await?;
            return Ok(UpdateDownload {
                path: target.to_string_lossy().into_owned(),
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

    let mut file = std::fs::File::create(&tmp)
        .map_err(|e| format!("create {tmp:?}: {e}"))?;
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    let mut total: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("chunk read: {e}"))?;
        total = total.saturating_add(chunk.len() as u64);
        if total > DOWNLOAD_MAX_BYTES {
            return Err(format!(
                "download exceeded {DOWNLOAD_MAX_BYTES} bytes mid-stream — aborting"
            ));
        }
        file.write_all(&chunk).map_err(|e| format!("write: {e}"))?;
    }
    file.sync_all().map_err(|e| format!("fsync: {e}"))?;
    drop(file);
    std::fs::rename(&tmp, &target).map_err(|e| format!("rename: {e}"))?;
    // Rename succeeded — disarm the guard so it doesn't try to remove
    // the already-moved file. (A Drop-safe `rename_if_exists` is
    // another option, but the guard pattern is simpler here.)
    cleanup.armed = false;

    // `reveal` is best-effort. If the OS file manager fails to open
    // (sandboxed session, display server unavailable, exotic DE), the
    // download itself succeeded — we shouldn't convert that into a
    // download-failed error for the user. Log + continue.
    if let Err(e) = reveal(&app, &target).await {
        eprintln!(
            "[updates] download saved to {target:?} but reveal failed: {e}"
        );
    }

    Ok(UpdateDownload {
        path: target.to_string_lossy().into_owned(),
        bytes: total,
    })
}

/// Open the containing folder in the OS file manager. On macOS we can
/// open the .dmg directly (user double-clicks to mount), but on
/// Windows/Linux we just reveal the folder since zip-extraction is a
/// conscious user step.
async fn reveal(_app: &AppHandle, path: &std::path::Path) -> Result<(), String> {
    // Prefer opening the parent directory — works on all three OSes
    // and avoids auto-launching a .dmg, which we want the user to
    // decide to do.
    let parent = path
        .parent()
        .ok_or_else(|| "downloaded path has no parent dir".to_string())?;
    open::that_detached(parent)
        .map_err(|e| format!("open downloads folder: {e}"))
}
