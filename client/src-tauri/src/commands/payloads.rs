//! Payload Library — curated catalogue of third-party PS5 payloads,
//! release fetching from GitHub, on-disk caching, and one-click send.
//!
//! Why this exists:
//!   - Today the desktop app ships only `ps5upload.elf`. Every other
//!     payload (kstuff, ShadowMountPlus, etaHEN, …) is something
//!     the user has to download manually, drag into the Send-payload
//!     screen, and hope they got the right asset. This module turns
//!     that into one click.
//!   - The catalogue is also reused by Phase 3 (SMP integration —
//!     "is SMP installed/running?") and Phase 4 (USB autoloader
//!     wizard — "which payloads should I bundle?"), so it's the
//!     load-bearing data structure for the whole homebrew-ecosystem
//!     surface area.
//!
//! Design choices:
//!   - **Curated catalogue, not auto-discovery.** A free-form GitHub
//!     search would surface dozens of forks and variants of every
//!     project. The user wants "the canonical kstuff for current
//!     firmware", not "every kstuff fork ever uploaded." Each entry
//!     is hand-picked.
//!   - **Static const list + runtime release fetch.** The catalogue
//!     itself (which repos to track, where the asset lives) ships
//!     baked into the binary. Versions are fetched on-demand from
//!     GitHub Releases. Release responses cached on disk for 1 hour
//!     to stay under the 60 req/hour unauthenticated rate limit.
//!   - **Asset pattern is a substring**, not a full regex. Most repos
//!     ship one ELF per release; the substring picks the canonical
//!     one when there are multiple (e.g. `lowfw` variants).
//!   - **Marker fields** (`process_name`, `on_console_marker_path`,
//!     `ports`) feed Phase 3 detection without needing a separate
//!     fingerprint table.
//!   - **`autoload_priority` + `autoload_delay_ms`** feed Phase 4's
//!     autoload.txt writer. Lower priority = earlier in the file.
//!     Delays are post-payload (i.e., "wait this long before loading
//!     the next entry").
//!
//! Read-only scope: we never modify a third-party tool's on-console
//! state. We only fetch its releases and (when the user clicks Send)
//! stream the ELF to :9021 the same way our own payload does.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// HTTP user-agent. GitHub requires a non-empty UA on every request
/// (returns 403 otherwise) and uses it for abuse pattern detection.
/// Suffix the running app version so a misbehaving release can be
/// pinpointed in GitHub's logs.
const HTTP_USER_AGENT: &str = concat!("ps5upload/", env!("CARGO_PKG_VERSION"), " (payloads)");

/// Cache TTL for GitHub release responses. 1 hour balances "user
/// notices new releases promptly" against "unauthenticated rate
/// limit is 60 req/hour" — for ~9 catalogue entries that means at
/// most 9 calls per hour, well under the cap.
const RELEASE_CACHE_TTL: Duration = Duration::from_secs(3600);

/// Max bytes accepted from a GitHub asset download. Mirrors the
/// embedded payload cap in `probes.rs`. 128 MiB covers the largest
/// known PS5 homebrew (etaHEN bundle ~5 MiB, kstuff ~1.5 MiB) with
/// generous headroom; anything above is a malformed asset or a
/// supply-chain issue.
const PAYLOAD_DOWNLOAD_MAX_BYTES: u64 = 128 * 1024 * 1024;

/// Network timeouts. Release listing is metadata only (small JSON);
/// the binary fetch streams a real ELF so it gets a longer ceiling.
const RELEASE_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const ASSET_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// One row in the catalogue. Every field is `&'static` so the table
/// is a `const` — no allocation, no runtime initialisation, ships
/// directly in the .rodata segment.
///
/// Adding a new payload: append a row to `CATALOGUE` below. No code
/// changes elsewhere are required — Phase 3 and Phase 4 read the same
/// rows for detection and autoloader writes.
#[derive(Clone, Copy)]
struct CatalogueEntry {
    /// Stable identifier, kebab-case. Used as the on-disk cache
    /// directory name and as the lookup key from the renderer.
    /// Must remain stable across releases — rename → cache miss for
    /// every user.
    id: &'static str,
    /// User-facing display name. Shown in the Payloads grid.
    display_name: &'static str,
    /// One-line role blurb. Shown under the name.
    role: &'static str,
    /// Two-sentence description for the detail card.
    description: &'static str,
    /// Where the releases live. We currently support GitHub only;
    /// Gitea/forgejo support would be a follow-up (a few payloads
    /// in the homebrew ecosystem are hosted on self-run Gitea
    /// instances — same JSON shape, different base URL).
    repo_owner: &'static str,
    repo_name: &'static str,
    /// Substring matched against each release asset's `name` field
    /// to pick the canonical ELF when multiple are uploaded.
    /// Empty string = "first asset whose name ends in .elf".
    asset_name_hint: &'static str,
    /// File the payload writes to /data on first run, used by
    /// `payloads_check_running` for "installed but not yet loaded"
    /// detection. None = no canonical marker, fall back to ports.
    on_console_marker_path: Option<&'static str>,
    /// Process name in PROC_LIST output, for "is it running" probe.
    /// None = no reliable process name (some payloads spawn under
    /// different names per build).
    process_name_hint: Option<&'static str>,
    /// TCP ports this payload binds when alive. Used for live-probe
    /// detection (Phase 3).
    ports: &'static [u16],
    /// Suggested autoload-order index for Phase 4's autoload.txt.
    /// kstuff = 0 (must run first to enable the others), SMP = 1,
    /// ps5upload = 2, ftpsrv etc = 3+.
    autoload_priority: u8,
    /// Milliseconds to pause AFTER this payload before loading the
    /// next entry in autoload.txt. kstuff needs ~3s to settle the
    /// kernel patches; userland payloads need much less.
    autoload_delay_ms: u32,
    /// Project homepage (UI links to this for "learn more").
    homepage: &'static str,
}

/// The curated catalogue. Order here is also the default UI display
/// order — keep the most-load-bearing payloads (kstuff, SMP) at the
/// top so they're visible without scrolling.
///
/// Sources for the entries:
///   - kstuff-echostretch: covers FW 1.00 → 12.x via runtime NID
///     resolution; the same binary works on every supported firmware
///     revision. Best default when firmware coverage matters most.
///   - kstuff-lite-drakmor: fork of EchoStretch/kstuff-lite tuned for
///     3-4× faster .ffpkg (UFS) mounting and lower per-mount overhead.
///     Narrower FW range (3.00 → 10.01). Better choice for users whose
///     primary workflow is ShadowMount+ with .ffpkg / .exfat images.
///   - shadowmountplus: the homebrew mount layer most users pair with
///     kstuff for image-based game launching.
///   - ftpsrv / websrv / ps5-app-dumper: utility payloads in common
///     use on the platform.
///   - etahen / itemzflow: best-effort entries; users can edit this
///     table to point at the variant they prefer.
const CATALOGUE: &[CatalogueEntry] = &[
    CatalogueEntry {
        id: "kstuff-echostretch",
        display_name: "kstuff-lite (EchoStretch)",
        role: "Kernel exploit + R/W primitive",
        description: "Kernel patcher for the full PS5 firmware range. Resolves kernel symbols at runtime via the SDK's NID table, so the same binary covers FW 1.00 → 12.x. Required by ShadowMountPlus and most other privileged payloads. Load this first.",
        repo_owner: "EchoStretch",
        repo_name: "kstuff",
        asset_name_hint: "kstuff",
        on_console_marker_path: Some("/data/kstuff.elf"),
        process_name_hint: None,
        ports: &[],
        autoload_priority: 0,
        autoload_delay_ms: 3000,
        homepage: "https://github.com/EchoStretch/kstuff",
    },
    CatalogueEntry {
        // Same role as kstuff-echostretch (kernel R/W + ucred elevation
        // + ShadowMount+ enablement) — they're alternatives, not
        // companions. Install only one. Picked over the EchoStretch
        // build when the user's main workflow is mounting .ffpkg /
        // .exfat images: drakmor's tree includes the per-mount fast
        // path and the repeated-operation overhead reduction
        // (autoload pause/resume + remount cycles) that ShadowMount+
        // exercises heavily. Marker path is shared (both drop
        // /data/kstuff.elf) so detection-of-presence is a single
        // probe — but the *active* variant has to be inferred from
        // user choice in the autoload list, not the marker.
        id: "kstuff-lite-drakmor",
        display_name: "kstuff-lite (drakmor — fpkg-optimized)",
        role: "Kernel exploit + R/W primitive — 3-4× faster .ffpkg mounting",
        description: "Fork of EchoStretch/kstuff-lite with a hot path for .ffpkg (UFS) mounts and lower overhead in repeated mount/unmount cycles — measured 3-4× faster end-to-end than the upstream lite build. Supports FW 3.00 → 10.01 (narrower than the EchoStretch full kstuff; pick this only if your firmware is in range). Recommended when your primary workflow is ShadowMount+ with .ffpkg or .exfat images. Same load-first ordering as any other kstuff: must boot before ShadowMount+ or ps5upload.",
        repo_owner: "drakmor",
        repo_name: "kstuff-lite",
        asset_name_hint: "kstuff",
        on_console_marker_path: Some("/data/kstuff.elf"),
        process_name_hint: None,
        ports: &[],
        autoload_priority: 0,
        autoload_delay_ms: 3000,
        homepage: "https://github.com/drakmor/kstuff-lite",
    },
    CatalogueEntry {
        id: "shadowmountplus",
        display_name: "ShadowMount+",
        role: "Auto-mount daemon for game backups",
        description: "Watches /mnt/usb*, /mnt/ext*, /data for .ffpkg/.exfat/.ffpfs game images and auto-mounts + registers them. Includes fakelib (backports) overlay and kstuff autopause.",
        repo_owner: "drakmor",
        repo_name: "shadowMountPlus",
        asset_name_hint: "shadowmountplus",
        on_console_marker_path: Some("/data/shadowmount/debug.log"),
        process_name_hint: Some("shadowmountplus"),
        ports: &[],
        autoload_priority: 1,
        autoload_delay_ms: 1000,
        homepage: "https://github.com/drakmor/shadowMountPlus",
    },
    CatalogueEntry {
        id: "etahen",
        display_name: "etaHEN",
        role: "Homebrew enabler + jailbreak helper",
        description: "Long-running homebrew enabler with toolbox features. Faster app jailbreak than the on-the-fly path; provides the HijackerCommand IPC many homebrew apps expect on :9028.",
        repo_owner: "LightningMods",
        repo_name: "etaHEN",
        asset_name_hint: "etaHEN",
        on_console_marker_path: None,
        process_name_hint: Some("etaHEN"),
        ports: &[9028, 2323],
        autoload_priority: 2,
        autoload_delay_ms: 500,
        homepage: "https://github.com/LightningMods/etaHEN",
    },
    CatalogueEntry {
        id: "ftpsrv",
        display_name: "ftpsrv",
        role: "FTP server payload",
        description: "Lightweight FTP server on :2121 with SELF↔ELF auto-decryption and remount-RW SITE commands. Lets you browse the PS5's filesystem from any FTP client.",
        repo_owner: "ps5-payload-dev",
        repo_name: "ftpsrv",
        asset_name_hint: "ftpsrv",
        on_console_marker_path: None,
        process_name_hint: Some("ftpsrv"),
        ports: &[2121],
        autoload_priority: 3,
        autoload_delay_ms: 200,
        homepage: "https://github.com/ps5-payload-dev/ftpsrv",
    },
    CatalogueEntry {
        id: "websrv",
        display_name: "websrv",
        role: "Web-based homebrew launcher",
        description: "HTTP server on :8080 serving a homebrew launcher page. Pairs with the homebrew bundles distributed by ps5-payload-dev.",
        repo_owner: "ps5-payload-dev",
        repo_name: "websrv",
        asset_name_hint: "websrv",
        on_console_marker_path: None,
        process_name_hint: Some("websrv"),
        ports: &[8080],
        autoload_priority: 3,
        autoload_delay_ms: 200,
        homepage: "https://github.com/ps5-payload-dev/websrv",
    },
    CatalogueEntry {
        id: "ps5-app-dumper",
        display_name: "ps5-app-dumper",
        role: "Dump installed apps to USB",
        description: "Dumps installed PS5 apps to USB or internal storage in fakepkg/folder format. Reads config from /data/ps5-app-dumper/config.ini.",
        repo_owner: "ps5-payload-dev",
        repo_name: "ps5-app-dumper",
        asset_name_hint: "dumper",
        on_console_marker_path: None,
        process_name_hint: Some("dumper"),
        ports: &[],
        autoload_priority: 4,
        autoload_delay_ms: 200,
        homepage: "https://github.com/ps5-payload-dev/ps5-app-dumper",
    },
    CatalogueEntry {
        id: "itemzflow",
        display_name: "Itemzflow",
        role: "PS5 native homebrew launcher UI",
        description: "Full-screen native PS5 launcher for homebrew, fpkg games, and FTP browsing. Heavyweight (~50 MB) but the most polished launcher in the scene.",
        repo_owner: "LightningMods",
        repo_name: "itemzflow_PS5",
        asset_name_hint: "itemzflow",
        on_console_marker_path: None,
        process_name_hint: Some("itemzflow"),
        ports: &[],
        autoload_priority: 5,
        autoload_delay_ms: 200,
        homepage: "https://github.com/LightningMods/itemzflow_PS5",
    },
];

/// Serializable mirror of `CatalogueEntry`. Has owned `String` fields
/// because `&'static str` doesn't survive serde→JSON without explicit
/// borrowed-types-in-output dance, and the per-call cost is negligible.
#[derive(Serialize)]
pub struct PayloadInfo {
    id: String,
    display_name: String,
    role: String,
    description: String,
    repo_owner: String,
    repo_name: String,
    on_console_marker_path: Option<String>,
    process_name_hint: Option<String>,
    ports: Vec<u16>,
    autoload_priority: u8,
    autoload_delay_ms: u32,
    homepage: String,
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
        }
    }
}

/// Look up a catalogue entry by id. None = unknown payload (renderer
/// passed an id that was deleted from the catalogue between releases).
fn find_entry(id: &str) -> Option<&'static CatalogueEntry> {
    CATALOGUE.iter().find(|e| e.id == id)
}

/// Cross-module accessor so `usb_autoloader.rs` can resolve a payload
/// id to its `(id, autoload_priority, autoload_delay_ms)` tuple
/// without re-parsing the catalogue. Returns None for unknown ids.
pub(crate) fn autoload_meta_for(id: &str) -> Option<(&'static str, u8, u32)> {
    find_entry(id).map(|e| (e.id, e.autoload_priority, e.autoload_delay_ms))
}

/// List the full catalogue. Tauri command — invoked by the Payloads
/// screen on mount.
#[tauri::command]
pub async fn payloads_catalog() -> Vec<PayloadInfo> {
    CATALOGUE.iter().map(PayloadInfo::from).collect()
}

// ─── GitHub releases ────────────────────────────────────────────────

/// Subset of GitHub's release JSON we care about. Trailing fields are
/// dropped silently by serde-derive's default.
#[derive(Deserialize, Serialize, Clone)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Deserialize, Serialize, Clone)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    published_at: String,
    #[serde(default)]
    assets: Vec<GithubAsset>,
    #[serde(default)]
    html_url: String,
}

/// What the renderer actually consumes — flat, no surprise asset
/// arrays. `picked_asset_*` fields are pre-filtered using the
/// catalogue's `asset_name_hint` so the UI doesn't have to know about
/// asset disambiguation.
#[derive(Serialize, Clone)]
pub struct ReleaseInfo {
    payload_id: String,
    /// Latest release tag (e.g. "v1.6.0"). Empty string if the repo
    /// has no releases yet.
    tag: String,
    /// Friendly name (often the same as tag).
    name: String,
    /// Release notes as published. Markdown — renderer should render
    /// or display verbatim.
    body: String,
    published_at: String,
    /// URL on github.com pointing at the release page.
    html_url: String,
    /// The asset URL we'd download for this payload. Empty when no
    /// asset matched the catalogue's `asset_name_hint`.
    picked_asset_url: String,
    picked_asset_name: String,
    picked_asset_size: u64,
    /// Cache freshness in seconds. UI surfaces this as "checked Ns
    /// ago" so users know whether to hit Refresh.
    cached_age_secs: u64,
}

/// Cache file path for a payload's release manifest. Per-payload so
/// one stale cache doesn't poison the rest, and so cache eviction
/// can be granular.
fn cache_release_path(app: &AppHandle, payload_id: &str) -> Result<PathBuf, String> {
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?;
    let dir = root.join("payloads").join(payload_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    Ok(dir.join("release.json"))
}

/// Read the cached release JSON. Returns `Some((release, age_secs))`
/// when the cache is fresh enough; `None` when missing, expired, or
/// unreadable.
fn read_cached_release(path: &std::path::Path) -> Option<(GithubRelease, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(mtime).ok()?;
    if age >= RELEASE_CACHE_TTL {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let release: GithubRelease = serde_json::from_slice(&bytes).ok()?;
    Some((release, age.as_secs()))
}

/// Pick the canonical asset from a release, using the catalogue hint.
/// Returns `(name, url, size)` or zeros when no asset matched.
/// Extension priority for the asset picker. The downloader streams
/// the chosen asset straight to the loader port, so a non-payload
/// file (a source-bundle .zip, a sha256 .txt, a release-notes .md)
/// would land on disk and immediately fail the ELF magic check.
///
/// We prioritise the executable shapes the loader actually accepts:
/// `.elf` first, then `.bin`/`.lua`/`.js`/`.jar` for non-ELF loaders
/// some payloads use. `.jar` is last because BD-JB-style JAR payloads
/// need a JAR-aware loader on a non-9021 port — the :9021 elfldr
/// rejects them. Everything else is last-resort and almost always a
/// mistake (e.g. `shadowmountplus-1.6beta10.zip` is the source
/// bundle, not the runnable payload — bug observed in the field).
const PAYLOAD_EXT_PRIORITY: &[&str] = &[".elf", ".bin", ".lua", ".js", ".jar"];

fn ext_priority(name: &str) -> usize {
    let lower = name.to_ascii_lowercase();
    for (i, ext) in PAYLOAD_EXT_PRIORITY.iter().enumerate() {
        if lower.ends_with(ext) {
            return i;
        }
    }
    PAYLOAD_EXT_PRIORITY.len()
}

fn pick_asset(release: &GithubRelease, hint: &str) -> (String, String, u64) {
    if release.assets.is_empty() {
        return (String::new(), String::new(), 0);
    }
    let lower_hint = hint.to_ascii_lowercase();
    // First pass: hint substring match — but require a payload-shaped
    // extension so a source `.zip` never wins over the runnable `.elf`.
    // Within payload-shaped matches, prefer .elf > .bin > .lua > .js.
    if !hint.is_empty() {
        let mut best: Option<(usize, &GithubAsset)> = None;
        for a in &release.assets {
            if !a.name.to_ascii_lowercase().contains(&lower_hint) {
                continue;
            }
            let prio = ext_priority(&a.name);
            // Skip non-payload extensions in this pass; the second
            // pass below handles "no payload-shaped match" gracefully.
            if prio >= PAYLOAD_EXT_PRIORITY.len() {
                continue;
            }
            if best.map(|(p, _)| prio < p).unwrap_or(true) {
                best = Some((prio, a));
            }
        }
        if let Some((_, a)) = best {
            return (a.name.clone(), a.browser_download_url.clone(), a.size);
        }
    }
    // Fallback: first .elf-named asset (any name).
    for a in &release.assets {
        if a.name.to_ascii_lowercase().ends_with(".elf") {
            return (a.name.clone(), a.browser_download_url.clone(), a.size);
        }
    }
    // Last resort: first asset. Almost always wrong if we get here —
    // the ELF-magic check downstream will catch it before write.
    let a = &release.assets[0];
    (a.name.clone(), a.browser_download_url.clone(), a.size)
}

/// Fetch the latest release for a payload, with cache. `force_refresh`
/// bypasses the cache regardless of age.
///
/// On network failure with a stale-but-readable cache, returns the
/// cached value with the real age — UI sees "old data" rather than
/// "no data" and renders a banner.
#[tauri::command]
pub async fn payloads_release(
    app: AppHandle,
    id: String,
    force_refresh: Option<bool>,
) -> Result<ReleaseInfo, String> {
    let entry = find_entry(&id).ok_or_else(|| format!("unknown payload id: {id}"))?;
    let cache_path = cache_release_path(&app, &id)?;
    let force = force_refresh.unwrap_or(false);

    // Cache-hit fast path.
    if !force {
        if let Some((release, age)) = read_cached_release(&cache_path) {
            return Ok(release_to_info(entry, &release, age));
        }
    }

    let url = format!(
        "https://api.github.com/repos/{}/{}/releases/latest",
        entry.repo_owner, entry.repo_name
    );
    let client = reqwest::Client::builder()
        .timeout(RELEASE_FETCH_TIMEOUT)
        .user_agent(HTTP_USER_AGENT)
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let resp = match client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            // Network failure: fall back to whatever cache we have,
            // even if expired. Better stale than nothing.
            if let Ok(bytes) = std::fs::read(&cache_path) {
                if let Ok(release) = serde_json::from_slice::<GithubRelease>(&bytes) {
                    let age = std::fs::metadata(&cache_path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| SystemTime::now().duration_since(t).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    return Ok(release_to_info(entry, &release, age));
                }
            }
            return Err(format!("fetch {url}: {e}"));
        }
    };
    if !resp.status().is_success() {
        return Err(format!(
            "fetch {url}: HTTP {} ({})",
            resp.status().as_u16(),
            resp.status().canonical_reason().unwrap_or("unknown status")
        ));
    }
    let body = resp.text().await.map_err(|e| format!("read body: {e}"))?;
    let release: GithubRelease =
        serde_json::from_str(&body).map_err(|e| format!("parse release JSON: {e}"))?;

    // Persist before returning so a successful fetch warms the cache
    // even if the renderer ignores the response.
    let tmp = cache_path.with_extension("json.tmp");
    std::fs::write(&tmp, &body).map_err(|e| format!("write cache: {e}"))?;
    super::replace_file(&tmp, &cache_path).map_err(|e| format!("rename cache: {e}"))?;

    Ok(release_to_info(entry, &release, 0))
}

fn release_to_info(
    entry: &CatalogueEntry,
    release: &GithubRelease,
    cached_age_secs: u64,
) -> ReleaseInfo {
    let (name, url, size) = pick_asset(release, entry.asset_name_hint);
    ReleaseInfo {
        payload_id: entry.id.to_string(),
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
        cached_age_secs,
    }
}

// ─── On-disk inventory & download ───────────────────────────────────

#[derive(Serialize)]
pub struct LocalInventoryEntry {
    payload_id: String,
    /// Tag/version of the cached binary. Read from a sibling
    /// `.version` sidecar file we write alongside the .elf.
    version: String,
    /// Path of the cached .elf — what `payload_send` consumes.
    path: String,
    size: u64,
    /// File mtime as a Unix timestamp (seconds). Renderer formats it.
    mtime: i64,
}

/// Per-payload cache root. `~/.../payloads/<id>/` holds the latest
/// downloaded binary (we only keep one version at a time — older
/// ones would just bloat the cache).
fn payload_cache_dir(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?;
    let dir = root.join("payloads").join(id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    Ok(dir)
}

/// List all locally-cached payload binaries. The Payloads screen uses
/// this to decide whether to show a "Download" or "Send" affordance.
#[tauri::command]
pub async fn payloads_local_inventory(app: AppHandle) -> Vec<LocalInventoryEntry> {
    let mut out = Vec::new();
    for entry in CATALOGUE {
        let dir = match payload_cache_dir(&app, entry.id) {
            Ok(d) => d,
            Err(_) => continue,
        };
        // Find the first .elf in the dir.
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for f in read.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("elf") {
                continue;
            }
            let meta = match std::fs::metadata(&p) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let version = std::fs::read_to_string(dir.join("VERSION"))
                .unwrap_or_default()
                .trim()
                .to_string();
            out.push(LocalInventoryEntry {
                payload_id: entry.id.to_string(),
                version,
                path: p.to_string_lossy().to_string(),
                size: meta.len(),
                mtime,
            });
            break; // one per payload
        }
    }
    out
}

/// Download a payload asset to the local cache. Validates ELF magic
/// before persisting, so we never cache a bad asset (e.g. a 404 HTML
/// page or a corrupted partial response).
///
/// The renderer typically calls `payloads_release(id)` first, then
/// hands `picked_asset_url` and `tag` here.
#[tauri::command]
pub async fn payloads_download(
    app: AppHandle,
    id: String,
    asset_url: String,
    version: String,
) -> Result<LocalInventoryEntry, String> {
    let entry = find_entry(&id).ok_or_else(|| format!("unknown payload id: {id}"))?;
    if asset_url.is_empty() {
        return Err("no asset url".to_string());
    }
    let dir = payload_cache_dir(&app, entry.id)?;

    let client = reqwest::Client::builder()
        .timeout(ASSET_DOWNLOAD_TIMEOUT)
        .user_agent(HTTP_USER_AGENT)
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut resp = client
        .get(&asset_url)
        .send()
        .await
        .map_err(|e| format!("fetch {asset_url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "fetch {asset_url}: HTTP {}",
            resp.status().as_u16()
        ));
    }
    let total = resp.content_length().unwrap_or(0);
    if total > PAYLOAD_DOWNLOAD_MAX_BYTES {
        return Err(format!(
            "asset is too large ({total} bytes > {PAYLOAD_DOWNLOAD_MAX_BYTES} cap)"
        ));
    }

    // Stream to a tmp file, validate ELF magic on first chunk, atomic
    // rename when done.
    let asset_basename = entry
        .asset_name_hint
        .trim_matches(|c: char| !c.is_alphanumeric());
    let final_name = if asset_basename.is_empty() {
        format!("{}.elf", entry.id)
    } else {
        format!("{asset_basename}.elf")
    };
    let final_path = dir.join(&final_name);
    let tmp_path = final_path.with_extension("elf.tmp");

    {
        let mut tmp = std::fs::File::create(&tmp_path)
            .map_err(|e| format!("create {}: {e}", tmp_path.display()))?;
        let mut total_written: u64 = 0;
        let mut magic_buf: Vec<u8> = Vec::with_capacity(4);
        while let Some(chunk) = resp.chunk().await.map_err(|e| format!("read chunk: {e}"))? {
            if magic_buf.len() < 4 {
                let take = (4 - magic_buf.len()).min(chunk.len());
                magic_buf.extend_from_slice(&chunk[..take]);
                if magic_buf.len() == 4 && magic_buf.as_slice() != b"\x7FELF" {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(format!(
                        "downloaded asset is not an ELF (first bytes {:02x?})",
                        magic_buf
                    ));
                }
            }
            total_written = total_written.saturating_add(chunk.len() as u64);
            if total_written > PAYLOAD_DOWNLOAD_MAX_BYTES {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(format!(
                    "downloaded {total_written} bytes exceeds {PAYLOAD_DOWNLOAD_MAX_BYTES} cap"
                ));
            }
            tmp.write_all(&chunk)
                .map_err(|e| format!("write {}: {e}", tmp_path.display()))?;
        }
        // Final tail-check: a response that delivers a total of <4 bytes
        // (truncated download, malformed asset) would slip past the
        // in-loop magic check because the `magic_buf.len() == 4` branch
        // never fires. Reject the file here instead of saving a 3-byte
        // "ELF" that fails much later when someone tries to send it.
        if magic_buf.len() < 4 {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(format!(
                "downloaded asset is too short to be an ELF ({} bytes total)",
                magic_buf.len()
            ));
        }
        tmp.sync_all()
            .map_err(|e| format!("fsync {}: {e}", tmp_path.display()))?;
    }
    super::replace_file(&tmp_path, &final_path).map_err(|e| {
        format!(
            "rename {} -> {}: {e}",
            tmp_path.display(),
            final_path.display()
        )
    })?;

    // Stash the version alongside the binary for inventory.
    let _ = std::fs::write(dir.join("VERSION"), version.as_bytes());

    let meta = std::fs::metadata(&final_path).map_err(|e| format!("stat: {e}"))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Ok(LocalInventoryEntry {
        payload_id: entry.id.to_string(),
        version,
        path: final_path.to_string_lossy().to_string(),
        size: meta.len(),
        mtime,
    })
}

/// Resolve the locally-cached binary path for a payload, if any. Used
/// by Phase 4 (USB autoloader writer) so it can copy from cache
/// without re-downloading.
///
/// Validates `id` against the static catalogue first — without this,
/// a renderer-supplied `id = "../foo"` would let `payload_cache_dir`
/// (which calls `create_dir_all`) materialise attacker-named
/// directories anywhere under `app_local_data_dir`.
#[tauri::command]
pub async fn payloads_local_path(app: AppHandle, id: String) -> Option<String> {
    let entry = find_entry(&id)?;
    let dir = payload_cache_dir(&app, entry.id).ok()?;
    let read = std::fs::read_dir(&dir).ok()?;
    for f in read.flatten() {
        let p = f.path();
        if p.extension().and_then(|e| e.to_str()) == Some("elf") {
            return Some(p.to_string_lossy().to_string());
        }
    }
    None
}

use std::io::Write;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_ids_unique() {
        let mut ids: Vec<&str> = CATALOGUE.iter().map(|e| e.id).collect();
        ids.sort();
        let dup = ids.windows(2).find(|w| w[0] == w[1]);
        assert!(dup.is_none(), "duplicate catalogue id: {dup:?}");
    }

    #[test]
    fn catalogue_ids_kebab_case() {
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
    fn pick_asset_prefers_hint_match() {
        let r = GithubRelease {
            tag_name: "v1.0".into(),
            name: "".into(),
            body: "".into(),
            published_at: "".into(),
            html_url: "".into(),
            assets: vec![
                GithubAsset {
                    name: "kstuff-lowfw.elf".into(),
                    browser_download_url: "https://example/lowfw".into(),
                    size: 1,
                },
                GithubAsset {
                    name: "kstuff.elf".into(),
                    browser_download_url: "https://example/main".into(),
                    size: 2,
                },
            ],
        };
        // Hint "kstuff" matches both, but first-substring-match wins
        // (lowfw has "kstuff" in it). For the EchoStretch entry we'd
        // want a more specific hint — this test pins the current
        // behaviour so we notice if matching changes.
        let (n, _, _) = pick_asset(&r, "kstuff");
        assert_eq!(n, "kstuff-lowfw.elf");
    }

    #[test]
    fn pick_asset_falls_back_to_elf() {
        let r = GithubRelease {
            tag_name: "v1.0".into(),
            name: "".into(),
            body: "".into(),
            published_at: "".into(),
            html_url: "".into(),
            assets: vec![
                GithubAsset {
                    name: "README.md".into(),
                    browser_download_url: "https://example/readme".into(),
                    size: 1,
                },
                GithubAsset {
                    name: "main.elf".into(),
                    browser_download_url: "https://example/main".into(),
                    size: 2,
                },
            ],
        };
        let (n, _, _) = pick_asset(&r, "nothing-matches");
        assert_eq!(n, "main.elf");
    }

    #[test]
    fn pick_asset_prefers_elf_over_source_zip() {
        // Real-world regression: ShadowMount+ releases ship both
        // `shadowmountplus-1.6beta10.zip` (source bundle) AND
        // `shadowmountplus-1.6beta10.elf` (runnable). The pre-fix
        // matcher took the first substring hit which was the .zip,
        // causing the loader to receive PK\x03\x04 bytes and reject.
        let r = GithubRelease {
            tag_name: "1.6beta10".into(),
            name: "".into(),
            body: "".into(),
            published_at: "".into(),
            html_url: "".into(),
            assets: vec![
                GithubAsset {
                    name: "shadowmountplus-1.6beta10.zip".into(),
                    browser_download_url: "https://example/zip".into(),
                    size: 1_000_000,
                },
                GithubAsset {
                    name: "shadowmountplus-1.6beta10.elf".into(),
                    browser_download_url: "https://example/elf".into(),
                    size: 750_000,
                },
            ],
        };
        let (n, _, _) = pick_asset(&r, "shadowmountplus");
        assert_eq!(n, "shadowmountplus-1.6beta10.elf");
    }

    #[test]
    fn pick_asset_skips_unrelated_extensions() {
        // sha256 / .txt sidecars (some releases bundle them) must
        // not be picked over a payload-shaped asset.
        let r = GithubRelease {
            tag_name: "v1".into(),
            name: "".into(),
            body: "".into(),
            published_at: "".into(),
            html_url: "".into(),
            assets: vec![
                GithubAsset {
                    name: "kstuff.sha256".into(),
                    browser_download_url: "https://example/hash".into(),
                    size: 64,
                },
                GithubAsset {
                    name: "kstuff.elf".into(),
                    browser_download_url: "https://example/elf".into(),
                    size: 100_000,
                },
            ],
        };
        let (n, _, _) = pick_asset(&r, "kstuff");
        assert_eq!(n, "kstuff.elf");
    }

    #[test]
    fn pick_asset_empty_assets() {
        let r = GithubRelease {
            tag_name: "v1.0".into(),
            name: "".into(),
            body: "".into(),
            published_at: "".into(),
            html_url: "".into(),
            assets: vec![],
        };
        let (n, u, s) = pick_asset(&r, "");
        assert_eq!(n, "");
        assert_eq!(u, "");
        assert_eq!(s, 0);
    }

    #[test]
    fn find_entry_lookup() {
        assert!(find_entry("shadowmountplus").is_some());
        assert!(find_entry("nope").is_none());
    }

    #[test]
    fn autoload_priorities_form_groups() {
        // Sanity check: kstuff entries are the only priority-0 ones,
        // SMP is alone at priority 1, runtime payloads are 2+.
        let p0: Vec<_> = CATALOGUE
            .iter()
            .filter(|e| e.autoload_priority == 0)
            .collect();
        let p1: Vec<_> = CATALOGUE
            .iter()
            .filter(|e| e.autoload_priority == 1)
            .collect();
        assert!(p0.iter().all(|e| e.id.starts_with("kstuff-")));
        assert!(p1.iter().all(|e| e.id == "shadowmountplus"));
    }
}
