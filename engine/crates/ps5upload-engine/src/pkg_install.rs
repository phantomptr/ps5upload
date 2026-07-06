//! Engine-side .pkg install plumbing.
//!
//! Three responsibilities:
//!   1. Parse a `.pkg` file (single or split-part set) and surface
//!      metadata for the UI — `parse_handler`.
//!   2. Host the `.pkg` bytes over HTTP with Range support so Sony's
//!      BGFT service on the PS5 can pull them — `serve_handler`.
//!   3. Drive the install: tell the payload to call BGFT, poll status,
//!      surface progress + final outcome — `install_start_handler` /
//!      `install_status_handler` / `install_cancel_handler`.
//!
//! Sessions are keyed by a random UUID v4 in the URL path so anyone
//! else on the LAN can't enumerate or hijack a different user's PKG.
//! Standard LAN-trust model for local PS5-side HTTP fetches.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderMap, Response, StatusCode},
    routing::{get, post},
    Json, Router,
};
use ps5upload_core::pkg_install::{
    err_code_message, pkg_install, pkg_install_status, InstallPhase, PkgInstallRequest,
    PkgInstallResponse, PkgInstallStatus,
};
use ps5upload_pkg::{
    extract_from_ffpkg, inspect_ffpkg, parse_pkg, parse_split_pkg, PkgKind, PkgMetadata,
    SplitPkgMetadata,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One in-flight install. The session lives from `install/start` until
/// the user dismisses the result or cancels. The HTTP-host listener
/// uses `parts` to satisfy Range requests.
///
/// Several fields are recorded for diagnostics / future introspection
/// endpoints (e.g. listing active sessions in the engine logs) even
/// though no current handler reads them — `#[allow(dead_code)]` documents
/// this intentional surplus rather than churn the struct each release.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct InstallSession {
    pub id: String,
    pub parts: Vec<PathBuf>,
    pub part_sizes: Vec<u64>,
    pub total_size: u64,
    pub content_id: String,
    pub title: String,
    pub package_type: String,
    /// PS5 mgmt-port address for status polling.
    pub ps5_mgmt_addr: String,
    /// BGFT task_id assigned by the payload after install/start.
    pub task_id: Option<i32>,
    /// Latest BGFT err_code surfaced to the client.
    pub err_code: u32,
    /// Latest detail string from payload / engine.
    pub detail: String,
    /// Whether the user has requested cancel (host-side; we stop
    /// serving HTTP when this is set).
    pub cancelled: bool,
    pub created_at_unix: u64,
    /// PS5-side absolute path of the Tier-1 staging file. Set on
    /// install_start when `local_ps5_path` was provided; the
    /// status handler best-effort fs_delete's it when the install
    /// terminates (phase = Done | Error). Then `take()`'s the field
    /// to None to prevent double-delete on subsequent polls.
    pub staging_path: Option<String>,
    /// Cached terminal status. Once an install reaches Done/Error the
    /// payload may reap the BGFT task_id, so re-polling it would 502 and
    /// flip a finished install to a spurious error on the client's next
    /// poll. We snapshot the terminal status here and replay it for any
    /// later poll instead of hitting the (now-gone) task.
    pub terminal_status: Option<PkgInstallStatus>,
    /// Resolved launchability once verification concludes: `Some(true)` =
    /// the title_id appeared in app.db (definitively launchable),
    /// `Some(false)` = it never appeared within the verification window,
    /// `None` = verification wasn't applicable (sqlite unavailable on this
    /// FW, or no real title_id) — the legacy optimistic behavior. Cached
    /// alongside `terminal_status` so replayed polls stay consistent.
    pub launchable: Option<bool>,
    /// Free bytes on the data volume at the first status poll — the baseline
    /// the progress tracker measures "bytes consumed" against. `None` until the
    /// first poll captures it (or if volumes couldn't be listed). See
    /// `observe_consumed` / `install_verdict`.
    pub install_start_free_bytes: Option<u64>,
    /// Max bytes the install has consumed so far (monotonic) — `max(free-space
    /// drop, title-dir size)`. Drives the live progress % and the stall clock.
    pub progress_consumed_bytes: u64,
    /// Unix time `progress_consumed_bytes` last increased. Resets the stall
    /// clock on every observed bit of progress, so a slow-but-advancing install
    /// never trips the adaptive stall deadline. `None` until the first poll.
    pub last_progress_unix: Option<u64>,
    /// Cached "the install stalled (no disk progress)" verdict, so replayed
    /// terminal polls keep reporting the stall (and the UI keeps the pkg).
    pub stalled: bool,
}

#[derive(Default)]
pub struct PkgInstallState {
    /// Active install sessions keyed by UUIDv4. Every route that
    /// touches this map locks via `.lock().unwrap_or_else(|e|
    /// e.into_inner())` rather than a bare `.unwrap()`: a panic in
    /// any handler that holds this lock would otherwise poison the
    /// mutex and wedge every subsequent install request for the
    /// engine's lifetime. The `into_inner()` recovery is safe here
    /// because the map's invariant is per-entry self-contained — a
    /// partially-mutated session row is no worse than a stale row,
    /// and the next status poll / GC pass cleans it up.
    pub sessions: Mutex<HashMap<String, InstallSession>>,
}

pub type PkgInstallStateHandle = Arc<PkgInstallState>;

pub fn router(state: PkgInstallStateHandle) -> Router {
    Router::new()
        .route("/api/pkg/parse", post(parse_handler))
        .route("/api/pkg/parse-split", post(parse_split_handler))
        // Read-only UFS2 image inspector for .ffpkg / .ufs files.
        // Lets the renderer surface "what's in this image?" before
        // a multi-GB upload. See ps5upload_pkg::ufs2 for the parser.
        .route("/api/ffpkg/inspect", post(inspect_handler))
        // Extract a file or subtree from a local .ffpkg to a local
        // dir. Useful for grabbing a single asset without uploading
        // the whole image to the PS5 first.
        .route("/api/ffpkg/extract", post(extract_handler))
        .route("/api/pkg/install/start", post(install_start_handler))
        .route("/api/pkg/install/status", get(install_status_handler))
        .route("/api/pkg/install/cancel", post(install_cancel_handler))
        // Install a staged .pkg through the standalone DPI daemon (:9040).
        // The daemon runs sceAppInstUtilAppInstallPkg from a clean loader
        // process — installs without the PlayGo gate. Caller stages the
        // pkg first and passes the bare PS5 path. See payload/dpi/.
        .route("/api/pkg/dpi-install", post(dpi_install_handler))
        // Direct/streaming install (beta, #81): skip the staging upload
        // entirely — the engine serves the pkg at /pkg-host/ and the DPI
        // daemon pulls it straight over HTTP. Useful when PS5 disk space
        // is tight or for a quick one-shot install from a machine that
        // already has the pkg mounted.
        .route(
            "/api/pkg/dpi-direct-install",
            post(dpi_direct_install_handler),
        )
        // Filename component is informational only — the session UUID
        // is the actual lookup key. We allow ANY {filename} so the URL
        // can carry the pkg's canonical `<ContentID>.pkg` name that
        // Sony's installer cross-checks against the pkg header. Without
        // this Sony rejects with 0x80B21106 on user-renamed pkgs (file
        // header says "FOO" but URL ends in "bar.pkg" — installer treats
        // them as inconsistent).
        .route("/pkg-host/{session}/{filename}", get(serve_handler))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
pub struct ExtractRequest {
    /// Local path to the `.ffpkg` image.
    pub ffpkg_path: String,
    /// Slash-separated path inside the image. Empty string = whole
    /// image (the root directory).
    #[serde(default)]
    pub inner_path: String,
    /// Local directory to write into. Created if missing.
    pub dest_dir: String,
}

async fn extract_handler(Json(req): Json<ExtractRequest>) -> Response<Body> {
    let ffpkg = req.ffpkg_path.clone();
    let inner = req.inner_path.clone();
    let dest = req.dest_dir.clone();
    let res = tokio::task::spawn_blocking(move || {
        extract_from_ffpkg(
            std::path::Path::new(&ffpkg),
            &inner,
            std::path::Path::new(&dest),
        )
    })
    .await;
    match res {
        Ok(Ok(meta)) => json_ok(&meta),
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, &format!("extract: {e}")),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("task: {e}")),
    }
}

async fn inspect_handler(Json(req): Json<ParseRequest>) -> Response<Body> {
    // spawn_blocking because the parser is sync I/O on a potentially
    // multi-GB image. With ~10 root entries (one inode read each) the
    // call typically finishes in a few hundred ms; without spawn_blocking
    // a slow disk could stall the axum reactor.
    let path = req.path.clone();
    let res = tokio::task::spawn_blocking(move || inspect_ffpkg(std::path::Path::new(&path))).await;
    match res {
        Ok(Ok(meta)) => json_ok(&meta),
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, &format!("inspect: {e}")),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("task: {e}")),
    }
}

// ─── /api/pkg/parse ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ParseRequest {
    pub path: String,
}

async fn parse_handler(Json(req): Json<ParseRequest>) -> Response<Body> {
    // spawn_blocking: parse_pkg is synchronous disk I/O (reads the pkg
    // header); a slow/remote path would otherwise stall the async reactor.
    // Mirrors inspect_handler / extract_handler below — parse_handler was
    // the lone holdout still blocking inline.
    let res = tokio::task::spawn_blocking(move || parse_pkg(std::path::Path::new(&req.path))).await;
    match res {
        Ok(Ok(meta)) => json_ok(&meta),
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, &format!("parse failed: {e}")),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("parse task panicked: {e}"),
        ),
    }
}

async fn parse_split_handler(Json(req): Json<ParseRequest>) -> Response<Body> {
    let res =
        tokio::task::spawn_blocking(move || parse_split_pkg(std::path::Path::new(&req.path))).await;
    match res {
        Ok(Ok(meta)) => json_ok(&meta),
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, &format!("split parse failed: {e}")),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("split parse task panicked: {e}"),
        ),
    }
}

// ─── /api/pkg/install/start ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct InstallStartRequest {
    /// PS5 mgmt-port address, e.g. "192.168.1.42:9114".
    pub ps5_addr: String,
    /// Either `path` (single .pkg) or `split_root` (lead `.pkg` of a
    /// split set) must be set. `split_root` triggers split-pkg
    /// detection — we look for `<root>.0`, `<root>.1`, ... siblings.
    pub path: Option<String>,
    pub split_root: Option<String>,
    /// Optional override for the package_type passed to BGFT. When
    /// unset we use whatever `derive_package_type(category)` returns
    /// or fall back to "PS4GD". Useful for unknown-magic PKGs where
    /// the user picks the type manually in the UI.
    pub package_type_override: Option<String>,
    /// PS5-side absolute path to a pkg already on the console's disk.
    /// When `Some`, the install URL is built as `file://{local_ps5_path}`
    /// and the HTTP-host serve_handler is not used. This is the
    /// "Tier-1" path: bytes already on PS5-local storage + ShellUI-RPC
    /// install + file:// URL = exactly what Settings → Debug Settings
    /// → Game → Package Installer does internally. Bypasses both the
    /// PlayGo HTTP-fetch authid reject (0x80B22404) and the
    /// engine-side network plumbing entirely.
    ///
    /// When `None`, falls back to the http://{lan-ip}:{port} flow —
    /// the engine hosts bytes for Sony's BGFT downloader.
    pub local_ps5_path: Option<String>,
    /// Caller-supplied content id for the staged-pkg case. When the pkg
    /// is already on the PS5 (`local_ps5_path` set) there's no PC-side
    /// file to parse, so the client passes the content id it parsed at
    /// upload time. Optional — the payload re-parses it from the staged
    /// pkg itself, so an empty value still installs.
    #[serde(default)]
    pub content_id: Option<String>,
    /// Whether to DELETE the staged `local_ps5_path` pkg after the install
    /// reaches a terminal phase (the "Tier-1 staging cleanup"). This is the
    /// user's "Auto Delete after installation" preference. Defaults TRUE for
    /// backward-compatibility (older clients that don't send it keep the old
    /// always-clean behaviour), but the current client always sends the real
    /// setting — so "Auto Delete off" now actually KEEPS the uploaded pkg
    /// instead of the engine silently deleting it regardless.
    #[serde(default = "default_true")]
    pub delete_staging: bool,
}

fn default_true() -> bool {
    true
}

/// Decide the session's `staging_path` — the file the terminal-phase handler
/// deletes after install. Returns `Some(path)` ONLY when the caller asked us to
/// clean up (`delete_staging`) AND a non-empty local path was supplied; `None`
/// otherwise, which KEEPS the uploaded pkg. Pulled out as a pure fn so the
/// "Auto Delete off ⇒ pkg kept" guarantee is unit-tested rather than buried in
/// the install_start flow (it was the root of a reported data-loss bug).
fn staging_path_for(local_ps5_path: &Option<String>, delete_staging: bool) -> Option<String> {
    if delete_staging {
        local_ps5_path.clone().filter(|s| !s.is_empty())
    } else {
        None
    }
}

/// Maximum number of attempts when retrying a staging cleanup that the
/// payload rejected with `fs_delete_failed`. Sony's in-process installer
/// briefly holds the staged `.pkg` file open (EBUSY / EBUSY-equivalent)
/// right after `terminal_complete` and on a register-reject; a single-shot
/// `fs_delete` races that window and surfaces as a scary "staging cleanup
/// failed" log even though the file would vanish a second later. We retry
/// a bounded number of times with a short backoff so the common case
/// (installer releasing the handle) succeeds without burning a slot.
const STAGING_DELETE_MAX_ATTEMPTS: u32 = 3;

/// Per-attempt sleep between staging-delete retries. Long enough for
/// Sony's installer to release its file handle on the staged pkg, short
/// enough that the spawn_blocking worker doesn't park the pool.
const STAGING_DELETE_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether a staging-delete error is worth retrying. Only the bare
/// `fs_delete_failed` token (Sony's installer still holding the file
/// open) qualifies — path-not-allowed, too-many-inflight, socket
/// timeout, or cancellation won't resolve on retry and should surface
/// immediately so the user sees the real cause. Exported as a pure fn
/// so the decision can be unit-tested without a live PS5 socket.
fn is_retryable_delete_error(err_str: &str) -> bool {
    err_str.contains("fs_delete_failed")
}

/// Delete a staged `.pkg` with a bounded retry on `fs_delete_failed`.
/// The payload sends that bare token when `rm_rf` returns non-zero — on
/// FW 10.40+ this is almost always Sony's installer still holding the
/// file open moments after the install completed (or was rejected), not
/// a genuine filesystem error. Retrying mirrors elf-arsenal's
/// `wait_for_install_row` settle window.
///
/// Returns Ok(()) if the file is gone (either deleted or already absent)
/// or the last error if all attempts failed. Logs each retry at warn so
/// a wedged console is still visible. The `label` is included in logs to
/// distinguish the three call sites (register-reject / terminal / cancel).
fn delete_staging_with_retry(addr: &str, path: &str, label: &str) -> Result<(), String> {
    let mut last_err: Option<String> = None;
    for attempt in 1..=STAGING_DELETE_MAX_ATTEMPTS {
        match ps5upload_core::fs_ops::fs_delete_with_timeout(
            addr,
            path,
            Some(std::time::Duration::from_secs(10)),
        ) {
            Ok(()) => {
                if attempt > 1 {
                    crate::log_info!(
                        "staging cleaned after retry: label={} addr={} path={} attempts={}",
                        label,
                        addr,
                        path,
                        attempt
                    );
                }
                return Ok(());
            }
            Err(e) => {
                let err_str = format!("{e:#}");
                // Only retry on the bare `fs_delete_failed` token — a
                // genuine path-not-allowed, too-many-inflight, or socket
                // timeout won't resolve on retry and should surface
                // immediately so the user sees the real cause.
                let retryable = is_retryable_delete_error(&err_str);
                last_err = Some(err_str);
                if !retryable || attempt == STAGING_DELETE_MAX_ATTEMPTS {
                    crate::log_warn!(
                        "staging cleanup failed: label={} addr={} path={} attempt={}/{} err={}",
                        label,
                        addr,
                        path,
                        attempt,
                        STAGING_DELETE_MAX_ATTEMPTS,
                        e
                    );
                    break;
                }
                crate::log_warn!(
                    "staging cleanup retrying: label={} addr={} path={} attempt={}/{} (installer may still hold the file) err={}",
                    label,
                    addr,
                    path,
                    attempt,
                    STAGING_DELETE_MAX_ATTEMPTS,
                    e
                );
                std::thread::sleep(STAGING_DELETE_BACKOFF);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "unknown error".to_string()))
}

#[derive(Debug, Serialize)]
pub struct InstallStartResponse {
    pub session_id: String,
    pub url: String,
    pub task_id: i32,
    pub err_code: u32,
    pub err_message: Option<String>,
    pub detail: String,
    /// 2.2.52 diagnostics — surfaced to the UI's "Advanced details"
    /// expander for failed installs. Empty / false on older payloads.
    /// `register_path` reports which BGFT Register variant ran
    /// ("intdebug" / "regular" / "none"); `intdebug_avail` is whether
    /// the IntDebug symbol resolved at all (false = fakepkg installs
    /// effectively unsupported on this firmware regardless of cred);
    /// `kernel_rw` mirrors the payload's process-wide cred-elevation
    /// state.
    pub register_path: String,
    pub intdebug_avail: bool,
    pub kernel_rw: bool,
    /// Per-tier err codes — null when tier wasn't attempted, 0 when
    /// it completed cleanly, otherwise the tier's err_code. Lets the
    /// host UI distinguish "Tier 1 silently bailed out" from "Tier 1
    /// reached Sony, who returned X". See `PkgInstallResponse`.
    pub shellui_err: Option<u32>,
    pub appinst_err: Option<u32>,
    /// Which install tier accepted this task — derived from the task_id
    /// bits set by the payload's bgft.c. Surfaced to the desktop's
    /// "Why?" diagnostic disclosure so the user (and us, during
    /// bug reports) sees whether the in-process appinst path took it,
    /// the SceShellUI RPC fallback did, or the legacy direct-BGFT
    /// path. See `ps5upload_core::pkg_install::via_tier`.
    pub via: String,
    /// True when the install was accepted via the unlaunchable last-resort
    /// path (`register_path == "appinst-local"`). The title installs but may
    /// fail to start ("can't start the game or app") on some firmwares —
    /// notably FW 12.xx. The UI shows a warning and points the user at the
    /// PS5's Settings → Package Installer to re-install if it won't boot.
    /// See `ps5upload_core::pkg_install::install_may_not_launch`.
    pub may_not_launch: bool,
    /// The package_type the install actually ran with, AFTER the engine's
    /// staged-pkg category parse (so a "…DP" here means "this was treated as a
    /// patch"). Lets the client recognise a guarded-patch rejection even on the
    /// USB/queue/File-System paths where it sent no type — and skip the pointless
    /// DPI fallback (DPI can't rescue a patch the same InstallByPackage rejected).
    #[serde(default)]
    pub package_type: String,
}

async fn install_start_handler(
    State(state): State<PkgInstallStateHandle>,
    Json(req): Json<InstallStartRequest>,
) -> Response<Body> {
    let (parts, part_sizes, total_size, head_meta) = match resolve_parts_and_meta(&req).await {
        Ok(t) => t,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, &e),
    };

    let mut package_type = req
        .package_type_override
        .clone()
        .or_else(|| head_meta.package_type.clone())
        .unwrap_or_else(|| "PS4GD".to_string());

    // ── Data-loss guard (engine layer) ──────────────────────────────────
    // The payload refuses to fall back to a DESTRUCTIVE install tier
    // (shellui-rpc / BGFT) for a patch — but it only recognises a patch by
    // package_type ending in "DP". The Library path carries the type the
    // client parsed at upload, but the USB-scan / File-System / upload-queue
    // paths have NO PARAM.SFO category, so they default to "PS4GD": a PS4
    // patch (which shares its base game's content_id) then looked like a full
    // game and slipped past the guard, re-registering the shared content_id
    // and WIPING the installed base (hardware-confirmed: a Jak X patch deleted
    // its 3.8 GB base). Fix: when the caller didn't declare a type, read the
    // category straight from the STAGED pkg's PARAM.SFO (three small ranged
    // reads) and derive the real type. This arms the guard for an ACTUAL patch
    // (`gp` → PS4DP) while leaving a full-game re-install (`gd` → PS4GD)
    // alone — an earlier "is it already installed?" heuristic wrongly blocked
    // legitimate base re-installs, which this avoids. Bounded + fail-soft: a
    // slow/unreadable pkg just leaves the default type, never hangs the start.
    let is_local = req
        .local_ps5_path
        .as_deref()
        .map(|p| !p.is_empty())
        .unwrap_or(false);
    let type_declared = req.package_type_override.is_some() || head_meta.package_type.is_some();
    if is_local && !type_declared {
        if let Some(local_path) = req.local_ps5_path.clone() {
            let addr = req.ps5_addr.clone();
            let parsed = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                tokio::task::spawn_blocking(move || {
                    ps5upload_pkg::category_from_reader(|off, len| {
                        ps5upload_core::fs_ops::fs_read(&addr, &local_path, off, len).ok()
                    })
                    .and_then(|cat| {
                        ps5upload_pkg::package_type_for_category(&cat).map(|pt| (cat, pt))
                    })
                }),
            )
            .await;
            if let Ok(Ok(Some((cat, pt)))) = parsed {
                if pt != package_type {
                    crate::log_info!(
                        "install guard: staged pkg category '{}' → package_type {} \
                         (was {}); {}",
                        cat,
                        pt,
                        package_type,
                        if pt.ends_with("DP") {
                            "arms the patch guard so a fallback tier can't wipe the base"
                        } else {
                            "full game — normal install cascade"
                        }
                    );
                    package_type = pt;
                }
            }
        }
    }

    // URL strategy: raw path when caller staged the pkg on PS5 disk
    // (Tier 1). On FW 9.60+ Sony's installer accepts a bare absolute
    // path WITHOUT the `file://` prefix; with the prefix it returns
    // 0x80B21106 (rejected) even on a freshly-rebooted PS5. The 6.xx
    // branch uses an HTTP loopback proxy; non-6.xx (incl. 9.60) uses
    // the bare path. Switched from `file://{path}` to raw `{path}` in
    // 2.2.54-fix-round-14 after direct hardware verification.
    let session_id = Uuid::new_v4().to_string();
    let url = match req.local_ps5_path.as_deref() {
        Some(p) if !p.is_empty() => {
            // Raw path. Sony's installer reads bytes off PS5
            // local disk; no engine-side HTTP listener needed.
            p.to_string()
        }
        _ => {
            // Pick the LAN IP this host presents to the PS5 and build the
            // /pkg-host/ URL Sony's installer will fetch. Centralised in
            // pkg_host_url_for so the direct-install (DPI) path constructs
            // an identical URL — a divergence would silently break the
            // installer's header cross-check (0x80B21106).
            let url = match pkg_host_url_for(&req.ps5_addr, &session_id, &head_meta.content_id) {
                Ok(u) => u,
                Err(e) => {
                    let ps5_host_only = strip_host_port(&req.ps5_addr);
                    return json_err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("could not determine local LAN IP for PS5 {ps5_host_only}: {e}"),
                    );
                }
            };
            url
        }
    };

    let session = InstallSession {
        id: session_id.clone(),
        parts,
        part_sizes,
        total_size,
        content_id: head_meta.content_id.clone(),
        title: if head_meta.title.is_empty() {
            head_meta
                .path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("(unknown)")
                .to_string()
        } else {
            head_meta.title.clone()
        },
        package_type: package_type.clone(),
        ps5_mgmt_addr: req.ps5_addr.clone(),
        task_id: None,
        err_code: 0,
        detail: String::new(),
        cancelled: false,
        created_at_unix: now_unix(),
        // staging_path drives the terminal-phase cleanup. None ⇒ pkg KEPT,
        // honouring "Auto Delete after installation" = off. See staging_path_for.
        staging_path: staging_path_for(&req.local_ps5_path, req.delete_staging),
        terminal_status: None,
        launchable: None,
        install_start_free_bytes: None,
        progress_consumed_bytes: 0,
        last_progress_unix: None,
        stalled: false,
    };

    // Insert *before* sending the install frame so the HTTP listener
    // is ready to serve when BGFT starts pulling immediately on its end.
    //
    // 2.2.55: opportunistic GC pass under the same lock to bound the
    // sessions map across a long-running engine.
    //
    // Two-policy GC, by design — see `gc_old_sessions` for the
    // status-poll-time fallback. Policies:
    //   1. (here, install_start) Drop sessions whose staging_path is
    //      already None AND that are older than ~half the
    //      configured max-age. staging_path == None is a strong
    //      "we already cleaned up" signal — set by the terminal,
    //      register-reject, and cancel paths. Aggressive prune of
    //      definitely-done sessions, while still keeping recent
    //      rows the UI may poll.
    //   2. (gc_old_sessions, status_handler) Pure age-based prune at
    //      full max-age; runs on every status poll. Catches
    //      sessions that never reached terminal (e.g. user closed
    //      the app mid-install, no cancel ever fired).
    //
    // Without this insert-time pass, a sequence of register-reject
    // failures (no status polls fire because the UI sees the
    // immediate error) would bloat the map unbounded — gc_old_sessions
    // alone wouldn't help because nothing calls status.
    {
        let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_unix();
        let max_age = pkg_session_max_age_sec();
        let full_cutoff = now.saturating_sub(max_age);
        let aggressive_cutoff = now.saturating_sub(max_age / 2);
        // Aggressive prune: drop sessions that are clearly "done with
        // host-side work AND old enough." `staging_path == None`
        // *alone* doesn't qualify any more — HTTP-mode (streaming)
        // installs always have staging_path == None for their entire
        // lifetime, so the v2.16.0 gate `staging_path.is_some()` was
        // evicting in-flight multi-GB downloads at the half-max-age
        // mark.
        //
        // The correct "definitely done" signal is `terminal_status.is_
        // some()` — set by the status handler the moment BGFT reaches
        // Done or Error. Sessions in either of those states have no
        // further polls to serve and can safely be reaped early.
        //
        // ALSO apply a pure age prune at full max-age regardless of state.
        // Register-reject installs (err_code != 0) never reach the status
        // poll's Done/Error path, so their terminal_status stays None forever
        // — the `terminal_status.is_none()` arm would otherwise retain them
        // indefinitely (gc_old_sessions only runs from the status handler,
        // which a register-reject never calls). The full-age sweep here is the
        // only thing that reaps them, bounding the sessions map.
        sessions.retain(|_, s| {
            if s.created_at_unix <= full_cutoff {
                return false;
            }
            s.created_at_unix > aggressive_cutoff || s.terminal_status.is_none()
        });
        sessions.insert(session_id.clone(), session.clone());
    }

    let install_req = PkgInstallRequest {
        url: url.clone(),
        content_id: session.content_id.clone(),
        size: session.total_size,
        title: session.title.clone(),
        package_type: package_type.clone(),
        method: None,
    };

    crate::log_info!(
        "pkg_install: addr={} session={} url={} content_id={} title={:?} package_type={} parts={} total={} bytes delete_staging={} staging_path={:?}",
        req.ps5_addr,
        session_id,
        url,
        session.content_id,
        session.title,
        install_req.package_type,
        session.parts.len(),
        total_size,
        req.delete_staging,
        session.staging_path,
    );

    // Run the blocking PS5 frame exchange OFF the async reactor. `pkg_install`
    // does synchronous TCP I/O (connect backoff + up-to-30s read timeout); the
    // bare `#[tokio::main]` runtime has only num_cpus worker threads, so calling
    // it inline would park a reactor thread for the whole RPC. Against a wedged
    // or unreachable console a few concurrent installs would occupy every
    // worker thread and stall the ENTIRE engine — SSE, /pkg-host serving, and
    // every OTHER console's requests. spawn_blocking keeps the reactor free so
    // 12 consoles stay independent. (Mirrors dpi_install_handler.)
    let resp: PkgInstallResponse = {
        let addr = req.ps5_addr.clone();
        let rollback = |e: String| {
            state
                .sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&session_id);
            crate::log_warn!(
                "pkg_install RPC failed: session={} addr={} err={}",
                session_id,
                req.ps5_addr,
                e,
            );
            json_err(
                StatusCode::BAD_GATEWAY,
                &format!("payload PKG_INSTALL failed: {e}"),
            )
        };
        match tokio::task::spawn_blocking(move || pkg_install(&addr, &install_req)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return rollback(e.to_string()),
            Err(e) => return rollback(format!("install task panicked/cancelled: {e}")),
        }
    };

    {
        let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(sess) = sessions.get_mut(&session_id) {
            sess.task_id = Some(resp.task_id);
            sess.err_code = resp.err_code;
            sess.detail = resp.detail.clone();
        }
    }

    let err_message = err_code_message(resp.err_code).map(|s| s.to_string());

    if resp.err_code == 0 {
        crate::log_info!(
            "pkg_install ok: session={} task_id={} register_path={} intdebug_avail={} kernel_rw={}",
            session_id,
            resp.task_id,
            resp.register_path,
            resp.intdebug_avail,
            resp.kernel_rw,
        );
    } else {
        // Sony rejected the register call. Log enough context to
        // diagnose post-mortem without ssh — the diagnostic disclosure
        // in the UI shows the same fields, but engine.log gives an
        // append-only history per attempt.
        crate::log_warn!(
            "pkg_install rejected: session={} err_code=0x{:08x} detail={:?} register_path={} intdebug_avail={} kernel_rw={} shellui_err={} appinst_err={}",
            session_id,
            resp.err_code,
            resp.detail,
            resp.register_path,
            resp.intdebug_avail,
            resp.kernel_rw,
            resp.shellui_err.map_or("null".to_string(), |e| format!("0x{e:08x}")),
            resp.appinst_err.map_or("null".to_string(), |e| format!("0x{e:08x}")),
        );
        // 2.2.55: best-effort staging cleanup on register-reject.
        // If Sony rejected the register call (e.g. duplicate
        // content_id, bad authid, FW gate), the install never
        // entered the BGFT phase machine, so the terminal-phase
        // cleanup at status-poll time will never fire — and the
        // staging file would otherwise leak on PS5 disk until the
        // payload's 24h sweep, polluting Sony's installer queue
        // and surfacing as 0x80B21106 on the user's NEXT attempt
        // with the same content_id. Cleaning here makes a failed
        // register idempotent: retry-after-fix works without
        // residue. Take() so we don't double-delete if status
        // somehow runs later. Spawn-blocking because fs_delete
        // does sync I/O over the mgmt socket.
        //
        // EXCEPTION — a guarded patch ("…DP"). The in-process
        // InstallByPackage hitting 0x80B21106 is the EXPECTED first
        // step for a PS4 update on FW 11/12: the client then retries
        // through the standalone DPI daemon (a separate, properly-
        // authid'd loader process — HW-proven to land patches the
        // in-process path can't). That fallback installs the SAME
        // staged pkg by its on-disk path, so deleting it here pulls
        // the file out from under DPI and the update can never apply
        // (HW bug bundle, FW 12.70: "register-reject staging cleaned"
        // fired right after 0x80B21106, leaving DPI nothing to install).
        // The client owns cleanup of its transient copy after the FULL
        // cascade (the USB path fs_deletes it unconditionally; the
        // queue keeps it for retry on failure / deletes on success),
        // so skipping the auto-clean for a patch doesn't leak.
        let is_guarded_patch =
            ps5upload_core::pkg_install::preserve_staging_on_reject(&package_type);
        let path_to_clean = if is_guarded_patch {
            // Leave staging_path in the session ref untouched — the client's
            // DPI fallback needs the file. Don't auto-delete.
            None
        } else {
            let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions
                .get_mut(&session_id)
                .and_then(|s| s.staging_path.take())
        };
        if is_guarded_patch {
            crate::log_info!(
                "register-reject staging PRESERVED for DPI fallback: session={} package_type={}",
                session_id,
                package_type,
            );
        }
        if let Some(path) = path_to_clean {
            let addr = req.ps5_addr.clone();
            let sid = session_id.clone();
            tokio::task::spawn_blocking(move || {
                // Retry on `fs_delete_failed` — Sony's installer briefly
                // holds the staged pkg open after a register-reject on
                // FW 10.40+; see delete_staging_with_retry.
                match delete_staging_with_retry(&addr, &path, "register-reject") {
                    Ok(()) => crate::log_info!(
                        "register-reject staging cleaned: session={} addr={} path={}",
                        sid,
                        addr,
                        path
                    ),
                    Err(e) => crate::log_warn!(
                        "register-reject staging cleanup failed: session={} addr={} path={} err={}",
                        sid,
                        addr,
                        path,
                        e
                    ),
                }
            });
        }
    }

    json_ok(&InstallStartResponse {
        session_id,
        url,
        task_id: resp.task_id,
        err_code: resp.err_code,
        err_message,
        detail: resp.detail,
        // Borrow register_path for may_not_launch BEFORE the move below —
        // struct fields evaluate in source order.
        may_not_launch: ps5upload_core::pkg_install::install_may_not_launch(&resp.register_path),
        register_path: resp.register_path,
        intdebug_avail: resp.intdebug_avail,
        kernel_rw: resp.kernel_rw,
        shellui_err: resp.shellui_err,
        appinst_err: resp.appinst_err,
        via: ps5upload_core::pkg_install::via_tier(resp.task_id).to_string(),
        package_type,
    })
}

// ─── /api/pkg/install/status ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct StatusQuery {
    pub session: String,
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub session_id: String,
    pub phase: InstallPhase,
    pub downloaded: u64,
    pub total: u64,
    pub err_code: u32,
    pub err_message: Option<String>,
    pub detail: String,
    pub cancelled: bool,
    /// Live diagnostic snapshot (matches the install/start ack shape).
    /// Pre-2.2.52-fix-round-2 the host only saw these from install/start;
    /// if BGFT transitioned to phase=error mid-install the user's
    /// "Why?" disclosure showed start-time values that said everything
    /// was fine. Now they refresh on every status poll. Empty / false
    /// for installs against pre-2.2.52 payloads (serde defaults).
    pub register_path: String,
    pub intdebug_avail: bool,
    pub kernel_rw: bool,
    /// Live per-tier err breakdown — same semantic as the install/start
    /// response, refreshed every status poll. See `InstallStartResponse`.
    pub shellui_err: Option<u32>,
    pub appinst_err: Option<u32>,
    /// Same tier identifier surfaced from install/start — re-derived
    /// here so the status poll's response is self-contained (the UI
    /// can read it without correlating against the start ack). See
    /// `ps5upload_core::pkg_install::via_tier`.
    pub via: String,
    /// Bytes the progress tracker has observed the install consume so far
    /// (`max(free-space drop, title-dir size)`). Drives the client's live
    /// install % (`installed_bytes / total`) for large titles where Sony's
    /// BGFT progress isn't meaningful (the file:// staging path). 0 until the
    /// first post-accept poll. See `install_verdict` / `observe_consumed`.
    #[serde(default)]
    pub installed_bytes: u64,
    /// True when the install was declared *stalled* — no disk progress past the
    /// adaptive deadline. Terminal like an error, but the staged pkg is KEPT so
    /// the user can retry. The client shows a "stalled — package kept" message
    /// instead of a generic failure, and must NOT delete the pkg. See
    /// `InstallVerdict::Stalled`.
    #[serde(default)]
    pub stalled: bool,
    /// path (`register_path == "appinst-local"`) — re-derived every poll so
    /// the status response is self-contained. The title installs but may not
    /// start on some firmwares (notably FW 12.xx). See
    /// `ps5upload_core::pkg_install::install_may_not_launch`.
    pub may_not_launch: bool,
    /// Definitive launchability from the engine's app.db verification
    /// (elf-arsenal `wait_for_install_row` analogue), once the install
    /// reaches Done:
    ///   `Some(true)`  — the title appeared in the PS5's app.db; it is
    ///                   launchable (overrides the `may_not_launch`
    ///                   heuristic — even an `appinst-local` install shows
    ///                   a clean success once verified).
    ///   `Some(false)` — the title never registered within the verification
    ///                   window; treat as "installed but won't launch".
    ///   `None`        — verification not applicable (app.db unreadable on
    ///                   this firmware, or no real title_id) — fall back to
    ///                   the `may_not_launch` heuristic. Pre-verification
    ///                   payloads/clients see this and behave as before.
    #[serde(default)]
    pub launchable: Option<bool>,
}

/// Default maximum age (seconds) of an install session before the
/// engine GCs it. 2 hours covers the practical worst case: a large
/// game (~50 GB) over weak WiFi (~10 Mbps sustained) takes ~70 min;
/// add Sony's BGFT install phase (decrypt + write, ~5-15 min for a
/// 50 GB title) and the upper bound is ~90 min real-world. The 2h
/// ceiling adds buffer without growing the sessions map unbounded.
///
/// Pre-2.2.32 was 30 min — too aggressive. A user with a slow PS5
/// network would see the session GC'd while polling was still active,
/// surfacing a 404 in the UI even though BGFT was still running on
/// the PS5. The new default avoids that bite.
///
/// Override at runtime via `PS5UPLOAD_PKG_SESSION_MAX_AGE_SEC` env
/// var — power users with extreme installs (huge games + cellular
/// hotspot) can extend further; sandboxed test environments can
/// shrink to seconds.
const PKG_SESSION_MAX_AGE_SEC_DEFAULT: u64 = 2 * 60 * 60;

fn pkg_session_max_age_sec() -> u64 {
    std::env::var("PS5UPLOAD_PKG_SESSION_MAX_AGE_SEC")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n >= 60) // sanity floor: <1min would race normal polling
        .unwrap_or(PKG_SESSION_MAX_AGE_SEC_DEFAULT)
}

// ─── progress-driven install tracker ──────────────────────────────────
//
// The fixed `pkg_verify_window_sec` window is *size-blind*: it gives up
// after ~90s and (historically) let the staging cleanup delete the uploaded
// pkg — fatal for a large title, because Sony's installer reads the pkg from
// that staged file for the *entire* install (a 25 GB game takes minutes, a
// 200 GB game far longer). Deleting it mid-install kills the install and
// leaves no game and no pkg (the reported Bloodborne data-loss).
//
// Instead of *assuming* completion after a timer, we *observe* it. Two
// physical signals the console already reports (no new payload frames):
//   • DONE  — `/user/app/<title_id>/app.pkg` appears (LaunchCheck::Registered).
//             Sony renames it into place at completion, so this is authoritative.
//   • ALIVE — bytes are landing: free space on the data volume drops
//             (FS_LIST_VOLUMES) and/or the title dir grows (FS_LIST_DIR sizes).
// We know the *expected* size up front (the pkg we just uploaded), so every
// decision is evidence-based against the real target instead of a guess.
//
// The stall deadline is *adaptive* and resets on any progress, so a slow but
// advancing install never false-fails; only a genuine flatline trips it, and
// only when both signals are flat (we track `max(free-drop, dir-size)`).

/// Seconds of zero disk progress before a *not-yet-writing* install (no bytes
/// consumed at all) is declared stalled — gives Sony time to even begin.
///
/// Generous (10 min) because on newer firmware (FW 12.x, hardware-observed) an
/// install registers and then the PS5 downloads/extracts the content in the
/// BACKGROUND — it can sit queued for minutes before it begins writing where we
/// can see it (free-space drop / title-dir growth). Giving up at the old 120s
/// false-stalled a perfectly good install while the console's own "Downloading…"
/// tile was still ticking. This only delays a *stall* verdict: completion is
/// still driven by the on-disk launch check (`registered`), never a timer, so a
/// longer wait can't manufacture a false success. Env-tunable.
const INSTALL_STALL_STARTUP_SEC_DEFAULT: u64 = 600;
/// Seconds of zero progress before a *mid-install* flatline (some bytes landed,
/// but well short of the target) is declared stalled — abnormal, so stricter.
const INSTALL_STALL_MID_SEC_DEFAULT: u64 = 240;
/// Seconds of zero progress tolerated once *near done* — the final commit /
/// registration phase writes almost nothing, so we wait patiently for the
/// title to register rather than crying stall.
const INSTALL_STALL_NEARDONE_SEC_DEFAULT: u64 = 600;
/// consumed/expected past which we consider the install "near done" and switch
/// to the patient deadline.
const INSTALL_NEARDONE_FRACTION: f64 = 0.90;
/// On firmware where launchability can't be verified (no derivable title_id,
/// both app.db and the /user/app scan unreadable), byte-accounting IS the
/// completion signal: this fraction of expected bytes consumed …
const INSTALL_SETTLE_FRACTION: f64 = 0.97;
/// … AND this many seconds of no further writes ⇒ treat as complete.
const INSTALL_SETTLE_SEC_DEFAULT: u64 = 60;

fn env_sec_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(default)
}
fn install_stall_startup_sec() -> u64 {
    env_sec_or(
        "PS5UPLOAD_INSTALL_STALL_STARTUP_SEC",
        INSTALL_STALL_STARTUP_SEC_DEFAULT,
    )
}
fn install_stall_mid_sec() -> u64 {
    env_sec_or(
        "PS5UPLOAD_INSTALL_STALL_MID_SEC",
        INSTALL_STALL_MID_SEC_DEFAULT,
    )
}
fn install_stall_neardone_sec() -> u64 {
    env_sec_or(
        "PS5UPLOAD_INSTALL_STALL_NEARDONE_SEC",
        INSTALL_STALL_NEARDONE_SEC_DEFAULT,
    )
}
fn install_settle_sec() -> u64 {
    env_sec_or("PS5UPLOAD_INSTALL_SETTLE_SEC", INSTALL_SETTLE_SEC_DEFAULT)
}

/// Launchability check, normalized for the tracker:
/// `Some(true)` = Registered (title on disk, definitively done),
/// `Some(false)` = Absent (reachable, title not yet there),
/// `None` = Unsupported (can't verify on this firmware).
type RegisteredObs = Option<bool>;

/// One poll's worth of observations, fed to the pure [`install_verdict`].
#[derive(Debug, Clone, Copy, PartialEq)]
struct TrackerObs {
    registered: RegisteredObs,
    /// Max bytes observed consumed so far (monotonic) — `max(free-space drop,
    /// title-dir size)`. Monotonic so a noisy free-space blip up can't look
    /// like a regression.
    consumed: u64,
    /// Expected install size (the pkg size we uploaded). 0 ⇒ unknown.
    expected: u64,
    /// Seconds since `consumed` last increased (the stall clock).
    idle_sec: u64,
    /// Tuning, pulled from env once by the caller so the function stays pure.
    startup_sec: u64,
    mid_sec: u64,
    neardone_sec: u64,
    settle_sec: u64,
}

/// What the tracker decides on a single poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallVerdict {
    /// Confirmed complete — the ONLY state in which the staging pkg may be
    /// deleted. (`registered == Some(true)`, or unverifiable-FW byte-settle.)
    Complete,
    /// Still progressing, or in the final commit window — keep polling, KEEP
    /// the staging pkg (the install may still be reading from it).
    Installing,
    /// No disk progress past the adaptive deadline — terminal, but KEEP the
    /// staging pkg so the user can retry.
    Stalled,
}

/// Pure completion/stall decision — no I/O, fully unit-testable. This is the
/// brain of the tracker; the handler only feeds it observations and acts on
/// the verdict.
fn install_verdict(obs: &TrackerObs) -> InstallVerdict {
    // Authoritative: the title's app.pkg landed on disk. Always wins, instantly
    // — independent of the byte math, which can only ever be an estimate.
    if obs.registered == Some(true) {
        return InstallVerdict::Complete;
    }
    let fraction = if obs.expected > 0 {
        obs.consumed as f64 / obs.expected as f64
    } else {
        0.0
    };
    // Byte-accounting completion — the authoritative fallback when the on-disk
    // launch check did NOT confirm `Registered`. This covers two real cases:
    //   • Unverifiable firmware (registered == None): app.db + /user/app both
    //     unreadable, byte-accounting is all we have.
    //   • EXTENDED-STORAGE installs (registered == Some(false)/Absent): the
    //     title's app.pkg lands on `/mnt/ext*`, which the payload's FS_LIST_DIR
    //     sees only through a stale/namespaced mount — so the filesystem check
    //     false-reports Absent for a perfectly good install (HW-confirmed: a
    //     game that plays, with app.pkg on /mnt/ext1, probes as Absent).
    // In BOTH, "essentially all expected bytes landed AND writing settled" means
    // the content really copied — which also cleanly rejects a "dead tile" (it
    // registers appmeta but writes ~no content, so it never settles near 100%).
    if obs.registered != Some(true)
        && fraction >= INSTALL_SETTLE_FRACTION
        && obs.idle_sec >= obs.settle_sec
    {
        return InstallVerdict::Complete;
    }
    // Adaptive stall deadline by how far along we are. Near the end the install
    // writes little (commit/register), so we're patient; an early/mid flatline
    // is abnormal, so we're stricter; before any byte lands we allow startup.
    let deadline = if fraction >= INSTALL_NEARDONE_FRACTION {
        obs.neardone_sec
    } else if obs.consumed == 0 {
        obs.startup_sec
    } else {
        obs.mid_sec
    };
    if obs.idle_sec >= deadline {
        InstallVerdict::Stalled
    } else {
        InstallVerdict::Installing
    }
}

/// Observe how many bytes the install has consumed so far, from the two
/// physical signals. Returns the larger of (free-space drop on the data
/// volume since the baseline) and (sum of the title dir's file sizes). Both
/// are best-effort: an unreadable signal contributes 0, not an error — the
/// tracker degrades to whichever signal is available.
fn observe_consumed(addr: &str, title_id: &str, baseline_free: Option<u64>) -> u64 {
    // Signal A — global free-space drop on the volume hosting /user/app. Noisy
    // (other writes move it) but available from the first poll, before Sony
    // even creates the title dir.
    let free_drop = match (baseline_free, current_free_bytes(addr)) {
        (Some(base), Some(now)) => base.saturating_sub(now),
        _ => 0,
    };
    // Signal B — the title dir's own size. Clean (only THIS install writes
    // there) but only exists once Sony creates /user/app/<title_id>/.
    let dir_size = title_dir_size(addr, title_id);
    free_drop.max(dir_size)
}

/// Free bytes across the possible install-target volumes: the volume hosting
/// `/user/app` (internal) PLUS every extended-storage drive (`/mnt/ext*`). A
/// title installs to whichever the console's storage setting selects, so we sum
/// them — the free-space drop then shows up wherever the content actually lands
/// (HW-confirmed: a Pro installs to `/mnt/ext1`, where an internal-only baseline
/// would never move and the tracker would false-stall a large install).
/// `None` if volumes can't be listed.
fn current_free_bytes(addr: &str) -> Option<u64> {
    let vols = ps5upload_core::volumes::list_volumes(addr).ok()?;
    let mut total = 0u64;
    let mut found = false;
    if let Some(v) = vols
        .find_for_path("/user/app")
        .or_else(|| vols.find_for_path("/user"))
    {
        total = total.saturating_add(v.free_bytes);
        found = true;
    }
    for v in &vols.volumes {
        if v.path.starts_with("/mnt/ext") {
            total = total.saturating_add(v.free_bytes);
            found = true;
        }
    }
    if found {
        Some(total)
    } else {
        None
    }
}

/// Sum of immediate file sizes under `…/user/app/<title_id>/` (the dominant
/// being `app.pkg`), across internal `/user/app` AND every extended-storage
/// mount (`/mnt/ext*/user/app`) — the install lands on whichever drive the
/// console targets. 0 if the dir doesn't exist yet or can't be read.
fn title_dir_size(addr: &str, title_id: &str) -> u64 {
    if title_id.is_empty() {
        return 0;
    }
    let mut dirs = vec![format!("/user/app/{title_id}")];
    if let Ok(vols) = ps5upload_core::volumes::list_volumes(addr) {
        for v in &vols.volumes {
            if v.path.starts_with("/mnt/ext") {
                dirs.push(format!("{}/user/app/{title_id}", v.path));
            }
        }
    }
    let mut total = 0u64;
    for dir in dirs {
        if let Ok(listing) = ps5upload_core::fs_ops::list_dir(
            addr,
            &dir,
            ps5upload_core::fs_ops::ListDirOptions::default(),
        ) {
            total = total.saturating_add(
                listing
                    .entries
                    .iter()
                    .filter(|e| e.kind == "file")
                    .map(|e| e.size)
                    .sum(),
            );
        }
    }
    total
}

/// Drop sessions older than the configured GC threshold. Called as a
/// best-effort sweep at the start of every status handler invocation.
/// Cheap (linear in active session count, which is bounded by the
/// queue UI to <100 in practice).
fn gc_old_sessions(state: &PkgInstallStateHandle) {
    let now = now_unix();
    let max_age = pkg_session_max_age_sec();
    let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
    sessions.retain(|_, s| {
        // Always keep sessions younger than the GC threshold; drop
        // older ones regardless of state. A session that's still
        // actively polling but past the threshold is treated as
        // orphaned (queue UI's worker loop terminates at done/error/
        // cancelled or after pollErrors >= 5, so it shouldn't be
        // legitimately polling a 2h-old session anyway).
        now.saturating_sub(s.created_at_unix) < max_age
    });
}

async fn install_status_handler(
    State(state): State<PkgInstallStateHandle>,
    Query(q): Query<StatusQuery>,
) -> Response<Body> {
    gc_old_sessions(&state);
    let (
        ps5_addr,
        task_id,
        total,
        cancelled,
        terminal,
        content_id,
        cached_launchable,
        cached_consumed,
        cached_stalled,
    ) = {
        let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        match sessions.get(&q.session) {
            None => {
                return json_err(
                    StatusCode::NOT_FOUND,
                    &format!("no install session {}", q.session),
                )
            }
            Some(s) => (
                s.ps5_mgmt_addr.clone(),
                s.task_id,
                s.total_size,
                s.cancelled,
                s.terminal_status.clone(),
                s.content_id.clone(),
                s.launchable,
                s.progress_consumed_bytes,
                s.stalled,
            ),
        }
    };
    // Once the install has finished, replay the cached terminal status and
    // do NOT re-poll: the payload may have reaped the BGFT task_id, so a
    // fresh PKG_INSTALL_STATUS would fail and turn a succeeded install into
    // a spurious 502 on the client's next poll.
    if let Some(status) = terminal {
        // For terminal responses we may not have a task_id (cancelled
        // before BGFT register, or a Tier-3 reject). Pass 0 — via_tier()
        // returns "direct-bgft" for 0, which is the most accurate
        // fallback (no synthetic flags = whatever Sony BGFT returned
        // raw, including "never got one").
        let tid = task_id.unwrap_or(0);
        return json_ok(&build_status_response(
            q.session,
            status,
            total,
            cancelled,
            tid,
            cached_launchable,
            cached_consumed,
            cached_stalled,
        ));
    }
    let task_id = match task_id {
        Some(t) => t,
        None => return json_err(StatusCode::CONFLICT, "session has no BGFT task_id yet"),
    };
    // Off the reactor: this handler is polled ~1/s per active install, and the
    // blocking STATUS frame exchange against a slow/wedged console would
    // otherwise park a worker thread per poll — with several installs that
    // starves the whole engine. (See install_start_handler.)
    let mut status: PkgInstallStatus = {
        let addr = ps5_addr.clone();
        match tokio::task::spawn_blocking(move || pkg_install_status(&addr, task_id)).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                return json_err(
                    StatusCode::BAD_GATEWAY,
                    &format!("payload PKG_INSTALL_STATUS failed: {e}"),
                )
            }
            Err(e) => {
                return json_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("status task panicked/cancelled: {e}"),
                )
            }
        }
    };

    // (`total` from the session is the fallback for build_status_response,
    // which prefers the BGFT-reported size when non-zero — BGFT reports 0
    // before the download starts.)

    // ── progress-driven completion tracking ────────────────────────────
    // The payload reports a *synthetic* Done the instant Sony *accepts* the
    // task (shellui-rpc / appinst-local) — long before a large title is
    // actually written. So on Done we don't trust the timer; we OBSERVE the
    // install to completion: poll the on-disk launch check (authoritative
    // "done") AND the bytes landing (free-space drop / title-dir growth), and
    // only declare the install *complete* — the sole state that lets the
    // staging cleanup below delete the uploaded pkg — when the title genuinely
    // registered (or, on unverifiable FW, when ~all expected bytes settled).
    // A flatline past the adaptive deadline is a *stall*: terminal, but the
    // pkg is KEPT so the user can retry. See `install_verdict`.
    let mut launchable: Option<bool> = None;
    // True only on confirmed completion — gates BOTH the staging cleanup and
    // the terminal-status cache. Stalls/errors are terminal but NOT complete.
    let mut terminal_complete = false;
    let mut stalled = false;
    if matches!(status.phase, InstallPhase::Done) {
        let addr = ps5_addr.clone();
        let cid = content_id.clone();
        let check = tokio::task::spawn_blocking(move || {
            ps5upload_core::pkg_install::verify_launchable(&addr, &cid)
        })
        .await
        .unwrap_or(ps5upload_core::pkg_install::LaunchCheck::Unsupported);

        // Normalize to the tracker's registered-observation.
        let registered: RegisteredObs = match check {
            ps5upload_core::pkg_install::LaunchCheck::Registered => Some(true),
            ps5upload_core::pkg_install::LaunchCheck::Absent => Some(false),
            ps5upload_core::pkg_install::LaunchCheck::Unsupported => None,
        };

        // Observe bytes consumed this poll (off-reactor: two blocking FS
        // frames). Best-effort — an unreadable signal contributes 0.
        let title_id =
            ps5upload_core::pkg_install::title_id_from_content_id(&content_id).unwrap_or_default();
        let baseline = {
            let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions
                .get(&q.session)
                .and_then(|s| s.install_start_free_bytes)
        };
        let (consumed_now, baseline_free) = {
            let addr = ps5_addr.clone();
            let tid = title_id.clone();
            tokio::task::spawn_blocking(move || {
                // Capture the free-space baseline on the first poll, then
                // measure drop against it on every subsequent poll.
                let base = baseline.or_else(|| current_free_bytes(&addr));
                let consumed = observe_consumed(&addr, &tid, base);
                (consumed, base)
            })
            .await
            .unwrap_or((0, baseline))
        };

        let now = now_unix();
        // Update the session's monotonic progress + stall clock, and read back
        // the values the verdict needs.
        let (consumed, idle_sec) = {
            let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
            match sessions.get_mut(&q.session) {
                Some(s) => {
                    if s.install_start_free_bytes.is_none() {
                        s.install_start_free_bytes = baseline_free;
                    }
                    // Monotonic: a noisy free-space blip up can't look like a
                    // regression and falsely advance/reset anything.
                    if consumed_now > s.progress_consumed_bytes {
                        s.progress_consumed_bytes = consumed_now;
                        s.last_progress_unix = Some(now);
                    }
                    let started = *s.last_progress_unix.get_or_insert(now);
                    (s.progress_consumed_bytes, now.saturating_sub(started))
                }
                None => (consumed_now, 0),
            }
        };

        let obs = TrackerObs {
            registered,
            consumed,
            expected: total,
            idle_sec,
            startup_sec: install_stall_startup_sec(),
            mid_sec: install_stall_mid_sec(),
            neardone_sec: install_stall_neardone_sec(),
            settle_sec: install_settle_sec(),
        };
        match install_verdict(&obs) {
            InstallVerdict::Complete => {
                // Confirmed done. `Some(true)` when the title registered;
                // `None` on unverifiable FW that settled by byte-accounting
                // (fall back to the may_not_launch heuristic, as before).
                launchable = if registered == Some(true) {
                    Some(true)
                } else {
                    None
                };
                terminal_complete = true;
                // Always log completion (even when auto-delete is off and the
                // "staging cleaned" line won't fire) so a bug bundle shows the
                // install ran to genuine completion, by which signal, and how
                // many bytes it took — the timeline the old code never recorded.
                crate::log_info!(
                    "install complete: session={} content_id={} via={} consumed={} expected={} idle_sec={}",
                    q.session,
                    content_id,
                    if registered == Some(true) { "registered" } else { "byte-settle" },
                    consumed,
                    total,
                    idle_sec
                );
            }
            InstallVerdict::Installing => {
                // Downgrade to Install: keeps the UI in "installing", surfaces
                // the live % (installed_bytes/total), and skips the terminal/
                // cleanup blocks so the next poll re-observes. The staging pkg
                // is left in place — Sony's installer is still reading it.
                status.phase = InstallPhase::Install;
            }
            InstallVerdict::Stalled => {
                // No disk progress past the adaptive deadline. Terminal, but
                // NOT complete: report an error AND KEEP the pkg (retry path).
                status.phase = InstallPhase::Error;
                stalled = true;
                launchable = Some(false);
                if status.detail.is_empty() {
                    status.detail = format!(
                        "install stalled: no disk progress for {}s ({} of {} bytes written)",
                        idle_sec, consumed, total
                    );
                }
                crate::log_warn!(
                    "install stalled (pkg KEPT): session={} content_id={} consumed={} expected={} idle_sec={}",
                    q.session,
                    content_id,
                    consumed,
                    total,
                    idle_sec
                );
            }
        }
    }

    // Tier-1 staging cleanup — delete the uploaded pkg ONLY on confirmed
    // completion (`terminal_complete`). Previously this fired on any terminal
    // phase (Done|Error), which deleted the pkg mid-install for large titles
    // (Sony reads it for the whole install) AND deleted it on a failed install
    // — the reported data-loss. Now a stall / error / not-yet-confirmed Done
    // KEEPS the pkg; only a genuine "the title registered" deletes it. We
    // `take()` the path so we never re-issue a delete on later polls.
    if terminal_complete {
        let path_to_clean = {
            let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions
                .get_mut(&q.session)
                .and_then(|s| s.staging_path.take())
        };
        if let Some(path) = path_to_clean {
            let addr = ps5_addr.clone();
            tokio::task::spawn_blocking(move || {
                // Retry on `fs_delete_failed` — Sony's installer briefly
                // holds the staged pkg open right after terminal_complete
                // on FW 10.40+; see delete_staging_with_retry.
                match delete_staging_with_retry(&addr, &path, "terminal") {
                    Ok(()) => crate::log_info!("staging cleaned: addr={} path={}", addr, path),
                    Err(e) => crate::log_warn!(
                        "staging cleanup failed: addr={} path={} err={}",
                        addr,
                        path,
                        e
                    ),
                }
            });
        }
    }

    // Snapshot the terminal status so later polls replay it instead of
    // re-hitting the (soon-to-be-reaped) BGFT task — see the short-circuit
    // at the top of this handler. Terminal = a confirmed-complete Done, a
    // stall (phase forced to Error above), or a genuine BGFT error. A Done
    // that the tracker downgraded to Install is NOT terminal — keep polling.
    let installed_bytes = if matches!(status.phase, InstallPhase::Done | InstallPhase::Error) {
        let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = sessions.get_mut(&q.session) {
            s.terminal_status = Some(status.clone());
            s.launchable = launchable;
            s.stalled = stalled;
            s.progress_consumed_bytes
        } else {
            cached_consumed
        }
    } else {
        // Live in-progress poll: surface the running consumed total for the
        // client's live % (installed_bytes / total).
        let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions
            .get(&q.session)
            .map(|s| s.progress_consumed_bytes)
            .unwrap_or(cached_consumed)
    };

    json_ok(&build_status_response(
        q.session,
        status,
        total,
        cancelled,
        task_id,
        launchable,
        installed_bytes,
        stalled,
    ))
}

/// Build the wire `StatusResponse` from a payload `PkgInstallStatus`.
/// `fallback_total` is our own known size, used when BGFT reports 0
/// (which it does before the download starts). Shared by the live path
/// and the cached-terminal replay so both render identically.
#[allow(clippy::too_many_arguments)] // flat builder for the wire struct's fields
fn build_status_response(
    session_id: String,
    status: PkgInstallStatus,
    fallback_total: u64,
    cancelled: bool,
    task_id: i32,
    launchable: Option<bool>,
    installed_bytes: u64,
    stalled: bool,
) -> StatusResponse {
    let total = if status.total > 0 {
        status.total
    } else {
        fallback_total
    };
    StatusResponse {
        session_id,
        phase: status.phase,
        downloaded: status.downloaded,
        total,
        err_code: status.err_code,
        err_message: err_code_message(status.err_code).map(|s| s.to_string()),
        detail: status.detail,
        cancelled,
        may_not_launch: ps5upload_core::pkg_install::install_may_not_launch(&status.register_path),
        launchable,
        register_path: status.register_path,
        intdebug_avail: status.intdebug_avail,
        kernel_rw: status.kernel_rw,
        shellui_err: status.shellui_err,
        appinst_err: status.appinst_err,
        via: ps5upload_core::pkg_install::via_tier(task_id).to_string(),
        installed_bytes,
        stalled,
    }
}

// ─── /api/pkg/install/cancel ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CancelRequest {
    pub session: String,
}

#[derive(Debug, Serialize)]
pub struct CancelResponse {
    pub session_id: String,
    /// True if the cancel reached the host-side serving listener.
    /// BGFT continues running on the PS5; once it sees the HTTP stream
    /// drop it surfaces a download error in PS5 notifications.
    pub host_stopped: bool,
}

async fn install_cancel_handler(
    State(state): State<PkgInstallStateHandle>,
    Json(req): Json<CancelRequest>,
) -> Response<Body> {
    // 2.2.55: also take() the staging path so we can delete it after
    // releasing the lock. Pre-fix the cancel path left the file on
    // PS5 disk forever — same Sony-queue-pollution failure mode as
    // the register-reject leak. Pull both fields under a single
    // lock acquisition so we never race with status_handler taking
    // the path first.
    let (cancel_ack, path_to_clean, ps5_addr) = {
        let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        match sessions.get_mut(&req.session) {
            Some(s) => {
                s.cancelled = true;
                let path = s.staging_path.take();
                let addr = s.ps5_mgmt_addr.clone();
                (true, path, addr)
            }
            None => {
                return json_err(
                    StatusCode::NOT_FOUND,
                    &format!("no install session {}", req.session),
                )
            }
        }
    };
    if let Some(path) = path_to_clean {
        let sid = req.session.clone();
        tokio::task::spawn_blocking(move || {
            // Retry on `fs_delete_failed` — Sony's installer may briefly
            // hold the staged pkg open when a cancel lands mid-install;
            // see delete_staging_with_retry.
            match delete_staging_with_retry(&ps5_addr, &path, "cancel") {
                Ok(()) => crate::log_info!(
                    "cancel staging cleaned: session={} addr={} path={}",
                    sid,
                    ps5_addr,
                    path
                ),
                Err(e) => crate::log_warn!(
                    "cancel staging cleanup failed: session={} addr={} path={} err={}",
                    sid,
                    ps5_addr,
                    path,
                    e
                ),
            }
        });
    }
    let _ = cancel_ack;
    json_ok(&CancelResponse {
        session_id: req.session,
        host_stopped: true,
    })
}

// ─── /api/pkg/dpi-install ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DpiInstallRequest {
    /// Any PS5 address we hold (`ip:9114` etc.) or a bare IP — we use
    /// only the host part and talk to the DPI daemon on `:9040`.
    pub ps5_addr: String,
    /// Absolute PS5-side path to the staged `.pkg` (under /user/data).
    pub local_ps5_path: String,
}

#[derive(Debug, Serialize)]
pub struct DpiInstallResponse {
    /// True when the daemon accepted the install (`ok` reply).
    pub ok: bool,
    /// The daemon's `sceAppInstUtilInstallByPackage` return code, or -1
    /// when the daemon never reached the install call (init/recv/badpath).
    pub rc: i32,
    /// True when the daemon could not initialize AppInstUtil. Distinct
    /// from a Sony-side reject: an init failure means the daemon is in
    /// fallback mode and retrying will likely fail the same way until
    /// the underlying IPMI/kstuff issue resolves.
    pub init_failed: bool,
    pub err_message: Option<String>,
}

/// One parsed reply from the DPI daemon. The daemon replies in the
/// reference's ok/error form (elf-arsenal payloads-src/dpi/main.c):
///   "ok"                  — InstallByPackage accepted
///   "error:0x%08X"        — InstallByPackage rejected with rc
///   "error:init:0x%08X"   — sceAppInstUtilInitialize failed with rc
///   "error:init:timeout"  — sceAppInstUtilInitialize timed out
///   "error:badpath"       — path rejected by the daemon's safety check
///   "error:recv"          — daemon saw no valid input on the socket
/// The old decimal-only form ("0", "-2147003130") is still accepted for
/// backward compatibility with older daemons still deployed on a console.
enum DpiReply {
    Ok,
    InstallReject(i32),
    InitFailed(Option<i32>), // None = timeout
    BadPath,
    RecvError,
    Unknown(String),
}

fn parse_dpi_reply(s: &str) -> DpiReply {
    let t = s.trim();
    if t == "ok" || t == "0" {
        return DpiReply::Ok;
    }
    if let Some(rest) = t.strip_prefix("error:init:") {
        if rest == "timeout" {
            return DpiReply::InitFailed(None);
        }
        // Sony error codes have the high bit set (e.g. 0x80B21106) and
        // overflow i32 — parse as u32 then cast so the negative i32
        // representation matches what InstallByPackage actually returns.
        if let Ok(rc) = u32::from_str_radix(rest.trim_start_matches("0x"), 16) {
            return DpiReply::InitFailed(Some(rc as i32));
        }
        return DpiReply::Unknown(t.to_string());
    }
    if let Some(rest) = t.strip_prefix("error:0x") {
        if let Ok(rc) = u32::from_str_radix(rest, 16) {
            return DpiReply::InstallReject(rc as i32);
        }
        return DpiReply::Unknown(t.to_string());
    }
    if t == "error:badpath" {
        return DpiReply::BadPath;
    }
    if t == "error:recv" {
        return DpiReply::RecvError;
    }
    // Backward-compat: old daemon replied with a bare decimal rc.
    if let Ok(rc) = t.parse::<i32>() {
        return if rc == 0 {
            DpiReply::Ok
        } else {
            DpiReply::InstallReject(rc)
        };
    }
    DpiReply::Unknown(t.to_string())
}

/// Connect to the PS5 DPI daemon on `:9040`, send one line (the staged
/// local path or an http(s):// URL), and read back the daemon's reply.
/// The daemon runs `sceAppInstUtilInstallByPackage(uri)` from its own
/// clean loader process, with a timed sceAppInstUtilInitialize + retry
/// so a cold install can never wedge IPMI (issue #152 root cause).
fn dpi_send(ps5_ip: &str, line: &str) -> std::io::Result<DpiReply> {
    use std::io::{Read, Write};
    use std::net::ToSocketAddrs;
    let sa = format!("{ps5_ip}:9040")
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "resolve :9040 failed"))?;
    let mut s = std::net::TcpStream::connect_timeout(&sa, std::time::Duration::from_secs(5))?;
    s.set_write_timeout(Some(std::time::Duration::from_secs(10)))?;
    // InstallByPackage can take a moment to ingest the pkg before replying.
    s.set_read_timeout(Some(std::time::Duration::from_secs(120)))?;
    s.write_all(line.as_bytes())?;
    s.write_all(b"\n")?;
    let mut buf = String::new();
    s.read_to_string(&mut buf)?;
    Ok(parse_dpi_reply(&buf))
}

async fn dpi_install_handler(Json(req): Json<DpiInstallRequest>) -> Response<Body> {
    if !req.local_ps5_path.starts_with('/') {
        return json_err(
            StatusCode::BAD_REQUEST,
            "local_ps5_path must be an absolute PS5 path (stage the pkg first)",
        );
    }
    let ps5_ip = strip_host_port(&req.ps5_addr);
    if ps5_ip.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "ps5_addr is required");
    }
    let path = req.local_ps5_path.clone();
    crate::log_info!("dpi-install: ps5={} path={}", ps5_ip, path);
    let res = tokio::task::spawn_blocking(move || dpi_send(&ps5_ip, &path)).await;
    match res {
        Ok(Ok(reply)) => {
            let (ok, rc, init_failed, err_message) = match reply {
                DpiReply::Ok => (true, 0, false, None),
                DpiReply::InstallReject(rc) => {
                    crate::log_warn!("dpi-install rejected rc=0x{:08x}", rc as u32);
                    (
                        false,
                        rc,
                        false,
                        err_code_message(rc as u32).map(|s| s.to_string()),
                    )
                }
                DpiReply::InitFailed(Some(rc)) => {
                    crate::log_warn!("dpi-install init failed rc=0x{:08x}", rc as u32);
                    (
                        false,
                        -1,
                        true,
                        Some(format!(
                            "sceAppInstUtilInitialize failed: 0x{:08X}",
                            rc as u32
                        )),
                    )
                }
                DpiReply::InitFailed(None) => {
                    crate::log_warn!("dpi-install init timed out");
                    (
                        false,
                        -1,
                        true,
                        Some(
                            "sceAppInstUtilInitialize timed out (IPMI backend not ready)"
                                .to_string(),
                        ),
                    )
                }
                DpiReply::BadPath => {
                    crate::log_warn!("dpi-install daemon rejected path");
                    (
                        false,
                        -1,
                        false,
                        Some("daemon rejected the path (unsafe)".to_string()),
                    )
                }
                DpiReply::RecvError => {
                    crate::log_warn!("dpi-install daemon saw no valid input");
                    (
                        false,
                        -1,
                        false,
                        Some("daemon received no valid input".to_string()),
                    )
                }
                DpiReply::Unknown(s) => {
                    crate::log_warn!("dpi-install unknown reply: {:?}", s);
                    (
                        false,
                        -1,
                        false,
                        Some(format!("unexpected daemon reply: {s}")),
                    )
                }
            };
            if ok {
                crate::log_info!("dpi-install ok");
            }
            json_ok(&DpiInstallResponse {
                ok,
                rc,
                init_failed,
                err_message,
            })
        }
        Ok(Err(e)) => json_err(
            StatusCode::BAD_GATEWAY,
            &format!("DPI daemon (:9040) not reachable / errored: {e}"),
        ),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("task: {e}")),
    }
}

// ─── /api/pkg/dpi-direct-install (streaming install beta, #81) ───────

/// Request body for `/api/pkg/dpi-direct-install`. Unlike
/// [`DpiInstallRequest`], this takes a `session_id` (from a prior
/// `/api/pkg/install/start` or the parsed-parts session) instead of a
/// staged PS5 path. The engine serves the pkg at `/pkg-host/{session}/`
/// and hands the DPI daemon that HTTP URL — the daemon pulls the bytes
/// straight off the engine, so no staging copy is uploaded to the PS5's
/// disk first. Saves disk space and one full transfer for the
/// quick-install case.
#[derive(Debug, Deserialize)]
pub struct DpiDirectInstallRequest {
    pub ps5_addr: String,
    pub session_id: String,
}

/// Send the pkg-host URL to the DPI daemon, returning the same
/// [`DpiInstallResponse`] shape as the staged-path route so the client
/// can handle both with identical logic. The daemon pulls the pkg over
/// HTTP; the engine's `/pkg-host/` handler satisfies Range requests so
/// Sony's PlayGo HTTP client works unchanged.
async fn dpi_direct_install_handler(
    State(state): State<PkgInstallStateHandle>,
    Json(req): Json<DpiDirectInstallRequest>,
) -> Response<Body> {
    let ps5_ip = strip_host_port(&req.ps5_addr);
    if ps5_ip.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "ps5_addr is required");
    }

    // Look up the session to get the content_id (for the canonical
    // pkg-host filename). Hold the lock only long enough to clone what
    // we need — the DPI send below is blocking and must not hold the
    // sessions mutex.
    let content_id = {
        let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        match sessions.get(&req.session_id) {
            Some(s) => s.content_id.clone(),
            None => {
                return json_err(
                    StatusCode::NOT_FOUND,
                    &format!("no pkg-host session {}", req.session_id),
                )
            }
        }
    };

    let url = match pkg_host_url_for(&req.ps5_addr, &req.session_id, &content_id) {
        Ok(u) => u,
        Err(e) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("could not build pkg-host URL for PS5 {ps5_ip}: {e}"),
            )
        }
    };

    crate::log_info!(
        "dpi-direct-install: ps5={} session={} url={}",
        ps5_ip,
        req.session_id,
        url
    );
    let res = tokio::task::spawn_blocking(move || dpi_send(&ps5_ip, &url)).await;
    match res {
        Ok(Ok(reply)) => {
            let (ok, rc, init_failed, err_message) = match reply {
                DpiReply::Ok => (true, 0, false, None),
                DpiReply::InstallReject(rc) => {
                    crate::log_warn!("dpi-direct-install rejected rc=0x{:08x}", rc as u32);
                    (
                        false,
                        rc,
                        false,
                        err_code_message(rc as u32).map(|s| s.to_string()),
                    )
                }
                DpiReply::InitFailed(Some(rc)) => {
                    crate::log_warn!("dpi-direct-install init failed rc=0x{:08x}", rc as u32);
                    (
                        false,
                        -1,
                        true,
                        Some(format!(
                            "sceAppInstUtilInitialize failed: 0x{:08X}",
                            rc as u32
                        )),
                    )
                }
                DpiReply::InitFailed(None) => {
                    crate::log_warn!("dpi-direct-install init timed out");
                    (
                        false,
                        -1,
                        true,
                        Some(
                            "sceAppInstUtilInitialize timed out (IPMI backend not ready)"
                                .to_string(),
                        ),
                    )
                }
                DpiReply::BadPath => {
                    crate::log_warn!("dpi-direct-install daemon rejected URL");
                    (
                        false,
                        -1,
                        false,
                        Some("daemon rejected the URL (unsafe)".to_string()),
                    )
                }
                DpiReply::RecvError => {
                    crate::log_warn!("dpi-direct-install daemon saw no valid input");
                    (
                        false,
                        -1,
                        false,
                        Some("daemon received no valid input".to_string()),
                    )
                }
                DpiReply::Unknown(s) => {
                    crate::log_warn!("dpi-direct-install unknown reply: {:?}", s);
                    (
                        false,
                        -1,
                        false,
                        Some(format!("unexpected daemon reply: {s}")),
                    )
                }
            };
            if ok {
                crate::log_info!("dpi-direct-install ok");
            }
            json_ok(&DpiInstallResponse {
                ok,
                rc,
                init_failed,
                err_message,
            })
        }
        Ok(Err(e)) => json_err(
            StatusCode::BAD_GATEWAY,
            &format!("DPI daemon (:9040) not reachable / errored: {e}"),
        ),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("task: {e}")),
    }
}

async fn serve_handler(
    State(state): State<PkgInstallStateHandle>,
    // Two path params for the `{session}/{filename}` pattern. The
    // `filename` is informational only — content_id canonicalisation
    // for Sony's installer header cross-check — and never participates
    // in authentication (session UUID is the only auth signal). It
    // MUST be extracted here even though we discard it, or axum returns
    // 500 ErrorMissingPathParams on every fetch and BGFT sees
    // 0x80B22404 PlayGo HTTP 404 (caught in Round-1 v2.16.1 audit).
    AxumPath((session, _filename)): AxumPath<(String, String)>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
) -> Response<Body> {
    // Log every PS5-side fetch attempt. Critical for diagnosing the
    // SCE_PLAYGO_ERROR_CORE_HTTP_STATUS_CODE_404_NOT_FOUND (0x80B22404)
    // class of failures: Sony's PlayGo HTTP client got a 404 from us
    // and we need to see exactly what URL/method/range it asked for
    // to figure out why. Captures the Range header and the User-Agent
    // (to identify which Sony component is making the request).
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("(none)");
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("(none)");
    // 2.2.55: single lock acquisition (was two — one for `contains_key`
    // logging, one for the actual `.get`). Cuts mutex pressure under
    // BGFT's parallel range fetches and removes the small TOCTOU window
    // where the session could be cancelled between the two acquisitions.
    let session_lookup = {
        let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.get(&session).cloned()
    };
    let session_known = session_lookup.is_some();
    crate::log_info!(
        "pkg-host fetch: session={} known={} peer={} range={:?} user-agent={:?}",
        session,
        session_known,
        peer.ip(),
        range,
        ua,
    );
    let session = match session_lookup {
        Some(s) if !s.cancelled => s,
        Some(_) => return plain_response(StatusCode::GONE, "install session was cancelled"),
        None => return plain_response(StatusCode::NOT_FOUND, "no such install session"),
    };

    // Source-address gate (2.9.0). The session UUID is high-entropy and
    // gated everywhere else, but the URL the engine emits to the PS5
    // (`http://{lan-ip}:{port}/pkg-host/{uuid}/file.pkg`) flows over
    // plaintext HTTP. Any host on the LAN that can passively observe
    // the PS5↔engine TCP stream (promiscuous WiFi, ARP-spoof, SOHO
    // router admin) recovers the UUID from the first GET and can then
    // hammer the URL with Range requests to drive a 16 MiB allocation
    // per call — DoS the engine, possibly OOM the whole Tauri shell.
    //
    // Defense: refuse any source IP that isn't the PS5 the session
    // belongs to. The session records `ps5_mgmt_addr` as `ip:port` at
    // install_start time; we compare the bare IP. Loopback callers are
    // allowed because dev workflows (curl against localhost, MITM
    // proxies running on the same box) need to work for debugging.
    // Strip the port + IPv6 brackets so the bare-IP compare against
    // `peer.ip().to_string()` works for both IPv4 and IPv6. See
    // strip_host_port for details — extracted so the URL-builder above
    // and this gate stay in sync (Round 1 fixed only this site; Round 2
    // caught the URL-builder mirror bug at the same site of truth).
    let expected_ip = strip_host_port(&session.ps5_mgmt_addr);
    let peer_ip = peer.ip().to_string();
    if !peer.ip().is_loopback() && !expected_ip.is_empty() && peer_ip != expected_ip {
        crate::log_warn!(
            "pkg-host fetch REJECTED: peer={} expected={} session={}",
            peer_ip,
            expected_ip,
            session.id,
        );
        return plain_response(
            StatusCode::FORBIDDEN,
            "pkg-host URL is bound to the PS5 it was issued for",
        );
    }

    let total = session.total_size;
    let (start, end) = match parse_range_header(&headers, total) {
        Ok(r) => r,
        Err(_) => return plain_response(StatusCode::RANGE_NOT_SATISFIABLE, "invalid Range"),
    };

    // Read the ≤16 MiB split off the async reactor. Sony's BGFT issues
    // parallel range fetches, and across up to 12 concurrently-installing
    // consoles these synchronous disk reads would otherwise park reactor
    // worker threads, stalling every console's serving. `session` isn't used
    // past this point, so move it into the blocking task.
    let chunk =
        match tokio::task::spawn_blocking(move || read_split_range(&session, start, end)).await {
            Ok(Ok(b)) => b,
            Ok(Err(e)) => {
                return plain_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("read failed: {e}"),
                )
            }
            Err(e) => {
                return plain_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("read task panicked/cancelled: {e}"),
                )
            }
        };

    let len = chunk.len() as u64;
    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, len.to_string());

    // Respond 206 + Content-Range whenever the body is a *subset* of the
    // file — i.e. there was a Range header, OR a bare GET whose body the
    // 16 MiB cap trimmed below `total` (`end + 1 < total`). The bare-GET
    // case is the important one: without Content-Range, a plain
    // `GET` on a >16 MiB pkg returned `200 OK` + `Content-Length: 16 MiB`
    // and no signal that more bytes existed — any non-Range consumer would
    // treat a truncated package as complete. Only a body that covers the
    // whole file gets a bare `200 OK`.
    let has_range = headers.contains_key(header::RANGE);
    let is_partial = serve_is_partial(has_range, end, total);
    if is_partial {
        builder = builder.status(StatusCode::PARTIAL_CONTENT).header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{total}"),
        );
    } else {
        builder = builder.status(StatusCode::OK);
    }

    builder
        .body(Body::from(chunk))
        .unwrap_or_else(builder_failed_response)
}

// ─── helpers ─────────────────────────────────────────────────────────

async fn resolve_parts_and_meta(
    req: &InstallStartRequest,
) -> Result<(Vec<PathBuf>, Vec<u64>, u64, PkgMetadata), String> {
    // The two `parse_*` calls open and read .pkg / split-part headers (and stat
    // each split part) from disk. On a cold or network-hosted pkg that's
    // blocking I/O; run it OFF the async reactor so concurrent install-starts
    // across consoles can't park reactor worker threads. (Mirrors parse_handler.)
    if let Some(p) = &req.split_root {
        let p = p.clone();
        let m: SplitPkgMetadata =
            tokio::task::spawn_blocking(move || parse_split_pkg(std::path::Path::new(&p)))
                .await
                .map_err(|e| format!("split pkg parse task panicked/cancelled: {e}"))?
                .map_err(|e| format!("{e}"))?;
        Ok((m.parts, m.part_sizes, m.total_size, m.head))
    } else if let Some(p) = &req.path {
        let p = p.clone();
        let meta = tokio::task::spawn_blocking(move || parse_pkg(std::path::Path::new(&p)))
            .await
            .map_err(|e| format!("pkg parse task panicked/cancelled: {e}"))?
            .map_err(|e| format!("{e}"))?;
        let size = meta.size;
        Ok((vec![meta.path.clone()], vec![size], size, meta))
    } else if req
        .local_ps5_path
        .as_deref()
        .map(|p| !p.is_empty())
        .unwrap_or(false)
    {
        // Staged-pkg install: the .pkg is already on the PS5's disk and
        // there's no PC-side file to parse. Build minimal metadata from the
        // caller-provided fields (the client parsed the header at upload
        // time). The install URL is just the raw PS5 path and the payload
        // re-parses the content id from the staged pkg itself, so empty
        // values here are fine. parts/size are unused for a local install
        // (nothing is HTTP-served). This is what lets the Install Package
        // page route a staged pkg through the main payload's
        // InstallByPackage (which installs launchable content) instead of
        // the metadata-only DPI daemon.
        let lp = req.local_ps5_path.clone().unwrap_or_default();
        let meta = PkgMetadata {
            path: PathBuf::from(&lp),
            size: 0,
            kind: PkgKind::Standard,
            content_id: req.content_id.clone().unwrap_or_default(),
            title: String::new(),
            title_id: String::new(),
            category: String::new(),
            app_ver: String::new(),
            package_type: req.package_type_override.clone(),
            platform: ps5upload_pkg::derive_platform(
                ps5upload_pkg::PKG_MAGIC,
                req.content_id.as_deref().unwrap_or(""),
                "",
            ),
            icon_png_base64: None,
            warnings: vec![],
        };
        Ok((vec![PathBuf::from(lp)], vec![0], 0, meta))
    } else {
        Err("either `path`, `split_root`, or `local_ps5_path` is required".into())
    }
}

/// Per-response byte cap. A LAN client (or a misbehaving BGFT) that
/// requests `bytes=0-{total-1}` on a 50 GB pkg would otherwise force the
/// engine to allocate ~50 GB and OOM. Sony's real BGFT fetches in
/// MB-sized chunks, so this cap is only ever hit by abuse cases. When a
/// requested range exceeds the cap, we trim `end` to `start + CAP - 1`
/// and return that prefix; HTTP Range semantics let the client follow
/// up with a `bytes=(end+1)-...` request, which is what BGFT already
/// does for legitimate chunked fetches.
const PKG_HOST_RESPONSE_BYTES_CAP: u64 = 16 * 1024 * 1024;

/// Decide whether a pkg-host response must be `206 Partial Content` (with a
/// `Content-Range`) rather than a bare `200 OK`. True when the client sent a
/// Range header, OR when the served body covers less than the whole file
/// (`end + 1 < total`) — the latter happens on a bare GET whose body the
/// 16 MiB cap trimmed. Without the second case a plain GET on a >16 MiB pkg
/// returned `200 OK` for a truncated body, so any non-Range consumer treated
/// an incomplete package as complete.
fn serve_is_partial(has_range: bool, end: u64, total: u64) -> bool {
    has_range || end + 1 < total
}

/// Map a Range request to (start, end) inclusive over the total size.
/// We support `bytes=N-M` and `bytes=N-` only — Sony BGFT only sends
/// those forms in practice. Out-of-bounds and inverted ranges are
/// rejected; over-large ranges are trimmed to the per-response cap so
/// a malicious large-range request can't OOM the engine.
fn parse_range_header(headers: &HeaderMap, total: u64) -> Result<(u64, u64), ()> {
    let h = match headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        Some(s) => s,
        None => {
            // No Range header — serve the prefix up to the cap.
            let cap_end = PKG_HOST_RESPONSE_BYTES_CAP
                .saturating_sub(1)
                .min(total.saturating_sub(1));
            return Ok((0, cap_end));
        }
    };
    let after = h.strip_prefix("bytes=").ok_or(())?;
    let (s, e) = after.split_once('-').ok_or(())?;
    let start: u64 = s.parse().map_err(|_| ())?;
    let end: u64 = if e.is_empty() {
        total.saturating_sub(1)
    } else {
        e.parse().map_err(|_| ())?
    };
    if start > end || end >= total {
        return Err(());
    }
    // Trim ranges that exceed the per-response byte cap. The client
    // sees a smaller-than-asked PARTIAL_CONTENT and follows up with
    // another Range request for the rest — same shape as if we'd been
    // serving from a stream with a small read buffer.
    let len = end - start + 1;
    if len > PKG_HOST_RESPONSE_BYTES_CAP {
        return Ok((start, start + PKG_HOST_RESPONSE_BYTES_CAP - 1));
    }
    Ok((start, end))
}

/// Read a byte range `[start, end]` (inclusive) from the split-pkg
/// part list, crossing part boundaries as needed.
fn read_split_range(s: &InstallSession, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity((end - start + 1) as usize);
    let mut cursor = start;

    // Find the part containing `cursor` and stream until we've covered
    // the requested range. Read in capped chunks to avoid huge buffers
    // on a single call.
    let mut prefix = 0u64;
    for (i, part_size) in s.part_sizes.iter().enumerate() {
        let part_end = prefix + part_size;
        if cursor < part_end {
            let local_start = cursor - prefix;
            let want_end_global = end.min(part_end - 1);
            let local_end = want_end_global - prefix;
            let take = local_end - local_start + 1;

            let mut f = std::fs::File::open(&s.parts[i])?;
            f.seek(SeekFrom::Start(local_start))?;
            let mut chunk = vec![0u8; take as usize];
            f.read_exact(&mut chunk)?;
            out.extend_from_slice(&chunk);

            cursor = want_end_global + 1;
            if cursor > end {
                break;
            }
        }
        prefix = part_end;
    }
    Ok(out)
}

/// Pick the LAN IP this host presents to the given PS5 host. Works
/// across multi-NIC machines by asking the OS what local IP it would
/// use to send a packet to the PS5 — that's the right one to give to
/// BGFT in the install URL.
pub fn lan_ip_for_ps5(ps5_host: &str) -> std::io::Result<IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0")?;
    // UDP "connect" doesn't actually send anything — it just sets the
    // peer for routing-table lookup, so local_addr() returns the IP
    // the OS would use. Port number is arbitrary.
    sock.connect(format!("{ps5_host}:1"))?;
    Ok(sock.local_addr()?.ip())
}

/// Build the engine's `/pkg-host/{session}/{filename}` URL as the PS5
/// will fetch it. Picks the LAN IP this host presents to the PS5 (multi-
/// NIC safe), stamps the engine port, and canonicalises the filename
/// from the pkg's content_id so Sony's installer header cross-check
/// passes. Returns an Err with a human-readable cause when the LAN IP
/// can't be determined (e.g. PS5 host unresolvable).
///
/// Shared by the regular install-start flow (which embeds the URL in
/// the BGFT register request) and the direct/streaming install flow
/// (which hands the URL to the DPI daemon instead of a local path).
fn pkg_host_url_for(ps5_addr: &str, session_id: &str, content_id: &str) -> std::io::Result<String> {
    let ps5_host_only = strip_host_port(ps5_addr);
    let local_ip = lan_ip_for_ps5(&ps5_host_only)?;
    let host_port = std::env::var("PS5UPLOAD_ENGINE_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(19113);
    let url_filename = pkg_url_filename(content_id);
    Ok(format!(
        "http://{local_ip}:{host_port}/pkg-host/{session_id}/{url_filename}"
    ))
}

/// Last-ditch fallback when a `Response::builder()` chain fails. The
/// builders in this file only set statically valid headers, so this is
/// unreachable in practice — but one engine process serves every
/// console, and a panic in a response builder would kill all of their
/// transfers, so fail soft with a bare 500 instead of unwrapping.
fn builder_failed_response(e: axum::http::Error) -> Response<Body> {
    let mut resp = Response::new(Body::from(format!("response build failed: {e}")));
    *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    resp
}

fn json_ok<T: Serialize>(v: &T) -> Response<Body> {
    let body = serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap_or_else(builder_failed_response)
}

fn json_err(status: StatusCode, msg: &str) -> Response<Body> {
    let body = serde_json::json!({ "error": msg }).to_string();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap_or_else(builder_failed_response)
}

fn plain_response(status: StatusCode, msg: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(msg.to_string()))
        .unwrap_or_else(builder_failed_response)
}

/// Strip port + IPv6 brackets from a `host:port` (or `[ipv6]:port`)
/// string, returning just the bare host or IP. Centralised so the URL
/// builder (where we feed the IP to `lan_ip_for_ps5`) and the source-IP
/// gate (where we compare against `peer.ip().to_string()`) stay in
/// lock-step — the v2.16.1 Round 1 audit caught one site and Round 2
/// caught the other, with the same root cause: `split(':').next()`
/// truncates IPv6 to `[` because IPv6 addresses contain colons.
///
/// rsplit_once on the LAST `:` correctly cuts off the port for both
/// `1.2.3.4:9114` → `1.2.3.4` and `[2001:db8::1]:9114` → `[2001:db8::1]`.
/// We then strip surrounding brackets to normalise to the bare form
/// `peer.ip().to_string()` emits.
///
/// Edge cases:
///   - input with no port (`1.2.3.4`, `::1`) → returned as-is (without
///     brackets if present)
///   - empty input → empty string (caller is expected to handle)
fn strip_host_port(host_port: &str) -> String {
    // Bracketed IPv6 with port: `[2001:db8::1]:9114` →
    // rsplit_once on `]:` gives `[2001:db8::1` (with leading bracket).
    if let Some((host, port)) = host_port.rsplit_once("]:") {
        // host has leading `[` from the original; port is just digits.
        if port.parse::<u16>().is_ok() {
            return host.trim_start_matches('[').to_string();
        }
        // Empty port like `[::1]:` or non-numeric like `[::1]:foo` —
        // strip the host's leading `[` plus the trailing `]:port`
        // fragment, leaving the bare IPv6. Without this branch the
        // string falls through to the multi-colon catch-all and
        // returns unchanged, breaking the source-IP gate.
        return host.trim_start_matches('[').to_string();
    }
    // Bracketed IPv6 without port: `[2001:db8::1]` → strip brackets.
    if host_port.starts_with('[') && host_port.ends_with(']') {
        return host_port
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string();
    }
    // Bare IPv6 (no brackets, no port): contains multiple colons —
    // can't reliably distinguish host from port. Assume no port and
    // return the whole string. Callers feeding bare IPv6 with a
    // port suffix MUST bracket the address — that's the only
    // unambiguous form.
    if host_port.matches(':').count() > 1 {
        return host_port.to_string();
    }
    // Single colon: IPv4-with-port or hostname-with-port — split.
    match host_port.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => host.to_string(),
        _ => host_port.to_string(),
    }
}

/// Derive the URL filename component for a hosted pkg, preferring the
/// pkg's canonical content_id (Sony cross-checks the URL filename
/// against the pkg header on register). Falls back to "file.pkg" when
/// the header didn't carry a content_id (corrupt pkg, parse failure,
/// or a non-Sony header). Sanitises against URL-meta chars even on
/// the happy path — content_id is supposed to be `[A-Z0-9-_]+` per
/// Sony's spec, but a malformed pkg could carry slashes / spaces that
/// would re-route the request or break axum's path matcher.
fn pkg_url_filename(content_id: &str) -> String {
    let trimmed = content_id.trim();
    if trimmed.is_empty() {
        return "file.pkg".to_string();
    }
    // Keep only ASCII printable, no path/URL-meta. Anything else is
    // replaced with '_' so a hostile or corrupt pkg can't escape the
    // URL pattern. Length-cap at 64 chars (content_id spec is 36;
    // 64 is a generous safety ceiling).
    let safe: String = trimmed
        .chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        return "file.pkg".to_string();
    }
    format!("{safe}.pkg")
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── progress-driven install tracker (the large-pkg data-loss fix) ──
    //
    // These pin the brain of the tracker — the pure `install_verdict`. The
    // failure they guard against: a 25 GB / 200 GB install reported "done"
    // after a fixed timer, deleting the staged pkg WHILE Sony was still
    // installing from it. The cure is "Complete only on observed completion".

    /// Build a TrackerObs with sane defaults (1 GB GB expected, idle 0), so
    /// each test sets only the dimension it exercises.
    fn obs(registered: RegisteredObs, consumed: u64, expected: u64, idle_sec: u64) -> TrackerObs {
        TrackerObs {
            registered,
            consumed,
            expected,
            idle_sec,
            startup_sec: 120,
            mid_sec: 240,
            neardone_sec: 600,
            settle_sec: 60,
        }
    }

    #[test]
    fn verdict_registered_is_complete_regardless_of_bytes_or_idle() {
        // The authoritative signal wins instantly — even with zero observed
        // bytes (the dir/free-space signals can lag the rename-into-place) and
        // a long idle. This is the ONLY path that deletes a verified pkg.
        assert_eq!(
            install_verdict(&obs(Some(true), 0, 25_000_000_000, 9_999)),
            InstallVerdict::Complete
        );
    }

    #[test]
    fn verdict_large_install_in_progress_is_never_complete() {
        // The exact Bloodborne shape: 25 GB expected, title not yet registered,
        // a few GB written, progress recent (idle 8s). MUST be Installing — the
        // old code returned "done" here and deleted the pkg mid-install.
        let v = install_verdict(&obs(Some(false), 3_000_000_000, 25_000_000_000, 8));
        assert_eq!(v, InstallVerdict::Installing);
    }

    #[test]
    fn verdict_progress_resets_stall_for_slow_but_advancing_install() {
        // Even a 200 GB install that's only 10% done stays Installing as long
        // as bytes keep landing (idle below the mid deadline). Size-agnostic.
        let v = install_verdict(&obs(Some(false), 20_000_000_000, 200_000_000_000, 200));
        assert_eq!(v, InstallVerdict::Installing);
    }

    #[test]
    fn verdict_mid_install_flatline_stalls() {
        // Some bytes landed but well short of target, and NO progress past the
        // mid deadline → genuinely stuck → Stalled (terminal, but pkg kept).
        let v = install_verdict(&obs(Some(false), 5_000_000_000, 25_000_000_000, 241));
        assert_eq!(v, InstallVerdict::Stalled);
        // One second earlier it must still be Installing (boundary).
        let v = install_verdict(&obs(Some(false), 5_000_000_000, 25_000_000_000, 239));
        assert_eq!(v, InstallVerdict::Installing);
    }

    #[test]
    fn verdict_near_done_is_patient_past_the_mid_deadline() {
        // ≥90% consumed → the final commit/register phase writes ~nothing, so a
        // flatline that WOULD trip the mid deadline (241s) must NOT stall yet —
        // we wait out the longer near-done window for the title to register.
        let v = install_verdict(&obs(Some(false), 24_000_000_000, 25_000_000_000, 300));
        assert_eq!(v, InstallVerdict::Installing);
        // …but a flatline past the near-done window is still a stall.
        let v = install_verdict(&obs(Some(false), 24_000_000_000, 25_000_000_000, 601));
        assert_eq!(v, InstallVerdict::Stalled);
    }

    #[test]
    fn verdict_startup_grace_before_any_bytes() {
        // Zero bytes consumed (Sony hasn't begun writing) gets the startup
        // grace, not the stricter mid deadline.
        assert_eq!(
            install_verdict(&obs(Some(false), 0, 25_000_000_000, 119)),
            InstallVerdict::Installing
        );
        assert_eq!(
            install_verdict(&obs(Some(false), 0, 25_000_000_000, 121)),
            InstallVerdict::Stalled
        );
    }

    #[test]
    fn verdict_unsupported_fw_completes_on_byte_settle() {
        // No launch verification possible (None). Byte-accounting is the signal:
        // ~all expected bytes landed AND writing settled ⇒ Complete (delete ok).
        let v = install_verdict(&obs(None, 24_500_000_000, 25_000_000_000, 60));
        assert_eq!(v, InstallVerdict::Complete);
        // Settled but well short of expected ⇒ NOT complete (don't delete).
        let v = install_verdict(&obs(None, 10_000_000_000, 25_000_000_000, 300));
        assert_eq!(v, InstallVerdict::Stalled);
        // ~all bytes but not yet settled ⇒ still Installing (let it settle).
        let v = install_verdict(&obs(None, 24_800_000_000, 25_000_000_000, 30));
        assert_eq!(v, InstallVerdict::Installing);
    }

    #[test]
    fn verdict_ext_storage_install_completes_on_byte_settle() {
        // EXTENDED-STORAGE install: the title's app.pkg lands on /mnt/ext*,
        // which the payload's FS_LIST_DIR sees only through a stale mount, so the
        // launch check FALSE-reports Absent (Some(false)). Byte-accounting must
        // still confirm Complete — else a game that installed and PLAYS gets
        // reported as failed (the real bug this fixes). HW-confirmed on a Pro.
        let v = install_verdict(&obs(Some(false), 24_500_000_000, 25_000_000_000, 60));
        assert_eq!(v, InstallVerdict::Complete);
        // A "dead tile" (registers appmeta but writes ~no content) has Absent +
        // ~zero bytes ⇒ must NOT complete — byte-accounting cleanly rejects it.
        let v = install_verdict(&obs(Some(false), 80_000_000, 25_000_000_000, 300));
        assert_eq!(v, InstallVerdict::Stalled);
        // Absent + still writing (not settled) ⇒ keep installing, don't claim done.
        let v = install_verdict(&obs(Some(false), 24_900_000_000, 25_000_000_000, 20));
        assert_eq!(v, InstallVerdict::Installing);
    }

    #[test]
    fn verdict_unknown_size_never_false_completes() {
        // expected == 0 (size unknown) ⇒ fraction 0, so the settle/near-done
        // shortcuts can't fire. It can only ever be Installing or, on a true
        // flatline, Stalled — never a spurious Complete that deletes the pkg.
        assert_eq!(
            install_verdict(&obs(None, 9_999, 0, 10)),
            InstallVerdict::Installing
        );
        assert_eq!(
            install_verdict(&obs(None, 9_999, 0, 999)),
            InstallVerdict::Stalled
        );
        // …unless the title actually registers.
        assert_eq!(
            install_verdict(&obs(Some(true), 0, 0, 0)),
            InstallVerdict::Complete
        );
    }

    // ── delete_staging / staging cleanup (the Auto-Delete data-loss fix) ──

    #[test]
    fn staging_path_kept_when_delete_disabled() {
        // Auto Delete OFF → no staging_path → the uploaded pkg is KEPT, even
        // though a real local path was supplied. This is the core guarantee.
        let path = Some("/user/data/ps5upload/pkg_library/game.pkg".to_string());
        assert_eq!(staging_path_for(&path, false), None);
    }

    #[test]
    fn staging_path_cleaned_when_delete_enabled() {
        let path = Some("/user/data/ps5upload/pkg_library/game.pkg".to_string());
        assert_eq!(
            staging_path_for(&path, true),
            Some("/user/data/ps5upload/pkg_library/game.pkg".to_string())
        );
    }

    #[test]
    fn staging_path_none_for_empty_or_missing_path() {
        // Empty string and None both yield None regardless of the flag (there's
        // nothing to clean — e.g. the http-host flow with no local pkg).
        assert_eq!(staging_path_for(&Some(String::new()), true), None);
        assert_eq!(staging_path_for(&None, true), None);
        assert_eq!(staging_path_for(&None, false), None);
    }

    // ── is_retryable_delete_error (the fs_delete_failed retry decision) ──

    #[test]
    fn retryable_delete_error_on_fs_delete_failed_token() {
        // The bare token the payload sends when rm_rf returns non-zero
        // (Sony's installer still holding the staged pkg open). This is
        // the ONLY case we retry — it resolves on its own in ~1-2s.
        assert!(is_retryable_delete_error(
            "payload rejected FS_DELETE: fs_delete_failed"
        ));
    }

    #[test]
    fn retryable_delete_error_not_on_path_not_allowed() {
        // A genuine allowlist rejection — won't resolve on retry.
        assert!(!is_retryable_delete_error(
            "payload rejected FS_DELETE: fs_delete_path_not_allowed"
        ));
    }

    #[test]
    fn retryable_delete_error_not_on_too_many_inflight() {
        // All MAX_FS_OPS slots busy — retrying immediately won't help.
        assert!(!is_retryable_delete_error(
            "payload rejected FS_DELETE: fs_delete_too_many_inflight"
        ));
    }

    #[test]
    fn retryable_delete_error_not_on_socket_timeout() {
        // A wedged console / network error — surfacing immediately is
        // more useful than silently retrying for 6s.
        assert!(!is_retryable_delete_error(
            "read frame header: Resource temporarily unavailable (os error 11)"
        ));
        assert!(!is_retryable_delete_error("connection reset by peer"));
    }

    #[test]
    fn retryable_delete_error_not_on_cancellation() {
        // User hit Stop — cancellation is intentional, not retryable.
        assert!(!is_retryable_delete_error("cancelled"));
        assert!(!is_retryable_delete_error(
            "payload rejected FS_DELETE: fs_delete_cancelled"
        ));
    }

    #[test]
    fn install_start_request_delete_staging_defaults_true() {
        // Back-compat: an older client that omits delete_staging must keep the
        // historical always-clean behaviour (true), not silently flip to keep.
        let json = r#"{"ps5_addr":"1.2.3.4:9114","local_ps5_path":"/x.pkg"}"#;
        let req: InstallStartRequest = serde_json::from_str(json).unwrap();
        assert!(
            req.delete_staging,
            "omitted delete_staging must default true"
        );
    }

    #[test]
    fn install_start_request_delete_staging_false_round_trips() {
        // The current client sends the real preference; false must be honoured.
        let json =
            r#"{"ps5_addr":"1.2.3.4:9114","local_ps5_path":"/x.pkg","delete_staging":false}"#;
        let req: InstallStartRequest = serde_json::from_str(json).unwrap();
        assert!(!req.delete_staging);
        // And it must flow through to a kept pkg.
        assert_eq!(
            staging_path_for(&req.local_ps5_path, req.delete_staging),
            None
        );
    }

    #[test]
    fn strip_host_port_handles_ipv4_and_ipv6() {
        // IPv4 with port — the common case.
        assert_eq!(strip_host_port("192.168.1.42:9114"), "192.168.1.42");
        // IPv6 bracketed with port — the SocketAddr-emitted form.
        assert_eq!(strip_host_port("[2001:db8::1]:9114"), "2001:db8::1");
        assert_eq!(strip_host_port("[::1]:9114"), "::1");
        // No port — should pass through unchanged.
        assert_eq!(strip_host_port("192.168.1.42"), "192.168.1.42");
        // Bare bracketless IPv6 without port — the disambiguation
        // case. Naïve rsplit_once would split "::1" into "::" / "1"
        // (wrong); the port-must-parse-as-u16 check rejects that.
        assert_eq!(strip_host_port("::1"), "::1");
        assert_eq!(strip_host_port("2001:db8::1"), "2001:db8::1");
        // Hostname with port.
        assert_eq!(strip_host_port("my-ps5.local:9114"), "my-ps5.local");
        // Empty input.
        assert_eq!(strip_host_port(""), "");
        // Edge: bracketed IPv6 with empty/invalid port — Round 4 found
        // these fell through unhandled. Should still strip the host.
        assert_eq!(strip_host_port("[::1]:"), "::1");
        assert_eq!(strip_host_port("[2001:db8::1]:foo"), "2001:db8::1");
    }

    #[test]
    fn pkg_url_filename_uses_content_id() {
        assert_eq!(
            pkg_url_filename("IV0002-NPXS39041_00-STOREUPD00000000"),
            "IV0002-NPXS39041_00-STOREUPD00000000.pkg"
        );
        assert_eq!(
            pkg_url_filename("UP9000-CUSA12345_00-GAMECONTENT12345"),
            "UP9000-CUSA12345_00-GAMECONTENT12345.pkg"
        );
    }

    #[test]
    fn pkg_host_url_for_builds_canonical_url() {
        // Loopback always resolves — exercises the full URL assembly
        // (LAN IP lookup, port stamping, filename canonicalisation, path
        // pattern) that both install-start and dpi-direct-install share.
        // Pin the shape so a divergence between the two routes would
        // break Sony's installer header cross-check visibly here.
        let url = pkg_host_url_for(
            "127.0.0.1:9114",
            "abc-123",
            "UP9000-CUSA12345_00-GAMECONTENT12345",
        )
        .expect("loopback should resolve");
        assert!(
            url.starts_with("http://127.0.0.1:"),
            "URL should target loopback: {url}"
        );
        assert!(
            url.ends_with("/pkg-host/abc-123/UP9000-CUSA12345_00-GAMECONTENT12345.pkg"),
            "URL should carry session + canonical filename: {url}"
        );
    }

    #[test]
    fn pkg_host_url_for_uses_env_port_when_set() {
        // The engine port is overridable via PS5UPLOAD_ENGINE_PORT; the
        // direct-install URL must honour it so the daemon fetches from
        // the same port the engine is actually listening on.
        std::env::set_var("PS5UPLOAD_ENGINE_PORT", "29113");
        let url = pkg_host_url_for("127.0.0.1:9114", "s", "IV0001-X").expect("loopback");
        std::env::remove_var("PS5UPLOAD_ENGINE_PORT");
        assert!(
            url.starts_with("http://127.0.0.1:29113/"),
            "URL should use env-overridden port: {url}"
        );
    }

    #[test]
    fn pkg_url_filename_fallback_when_empty() {
        assert_eq!(pkg_url_filename(""), "file.pkg");
        assert_eq!(pkg_url_filename("   "), "file.pkg");
    }

    #[test]
    fn pkg_url_filename_sanitises_meta_chars() {
        // Path-traversal / URL-escape attempts in a corrupt or hostile
        // header. Must not produce slashes, dots (other than the .pkg
        // suffix we add), or query/fragment chars.
        assert_eq!(pkg_url_filename("../etc/passwd"), "___etc_passwd.pkg");
        assert_eq!(pkg_url_filename("foo bar?baz=1"), "foo_bar_baz_1.pkg");
        assert_eq!(pkg_url_filename("a.b.c"), "a_b_c.pkg");
    }

    #[test]
    fn pkg_url_filename_caps_length() {
        // 200-char content_id (impossible per Sony spec but defensive)
        // is truncated to 64 before the .pkg suffix.
        let long = "A".repeat(200);
        let out = pkg_url_filename(&long);
        assert_eq!(out.len(), 64 + 4); // 64 chars + ".pkg"
        assert!(out.ends_with(".pkg"));
    }

    #[test]
    fn pkg_url_filename_handles_all_invalid_chars() {
        // String that sanitises to all-underscores still produces a
        // valid filename (not an empty one that would re-route the
        // request).
        let out = pkg_url_filename("...");
        assert!(out.ends_with(".pkg"));
        assert!(!out.starts_with('.'));
    }

    fn dummy_session(parts: Vec<(PathBuf, u64)>) -> InstallSession {
        let total: u64 = parts.iter().map(|(_, s)| s).sum();
        InstallSession {
            id: "test".into(),
            parts: parts.iter().map(|(p, _)| p.clone()).collect(),
            part_sizes: parts.iter().map(|(_, s)| *s).collect(),
            total_size: total,
            content_id: "TEST".into(),
            title: "Test".into(),
            package_type: "PS4GD".into(),
            ps5_mgmt_addr: "127.0.0.1:0".into(),
            task_id: None,
            err_code: 0,
            detail: String::new(),
            cancelled: false,
            created_at_unix: 0,
            staging_path: None,
            terminal_status: None,
            launchable: None,
            install_start_free_bytes: None,
            progress_consumed_bytes: 0,
            last_progress_unix: None,
            stalled: false,
        }
    }

    #[test]
    fn range_header_full_when_absent() {
        let h = HeaderMap::new();
        assert_eq!(parse_range_header(&h, 100).unwrap(), (0, 99));
    }

    #[test]
    fn range_header_explicit() {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=10-20".parse().unwrap());
        assert_eq!(parse_range_header(&h, 100).unwrap(), (10, 20));
    }

    #[test]
    fn range_header_open_end() {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=50-".parse().unwrap());
        assert_eq!(parse_range_header(&h, 100).unwrap(), (50, 99));
    }

    #[test]
    fn range_header_invalid() {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=200-300".parse().unwrap());
        assert!(parse_range_header(&h, 100).is_err());
    }

    #[test]
    fn over_cap_range_is_trimmed() {
        // A request larger than PKG_HOST_RESPONSE_BYTES_CAP must come
        // back trimmed to the cap, NOT errored. HTTP Range semantics
        // let the client follow up for the rest. Without this, a
        // malicious LAN client requesting `bytes=0-{total-1}` on a
        // big pkg would force a multi-GB Vec allocation and OOM the
        // engine.
        let mut h = HeaderMap::new();
        // 50 GB total, request the full thing.
        let total: u64 = 50 * 1024 * 1024 * 1024;
        h.insert(
            header::RANGE,
            format!("bytes=0-{}", total - 1).parse().unwrap(),
        );
        let (start, end) = parse_range_header(&h, total).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, PKG_HOST_RESPONSE_BYTES_CAP - 1);
        // `end - start < CAP` is the post-trim invariant. Avoiding
        // `+ 1 <= CAP` keeps clippy::int_plus_one quiet without
        // changing the assertion's semantics.
        assert!(end - start < PKG_HOST_RESPONSE_BYTES_CAP);
    }

    #[test]
    fn no_range_header_is_capped() {
        // GET with no Range header on a giant pkg used to return the
        // whole thing (and OOM). Now serves only the first cap-sized
        // window; client must follow up with explicit ranges.
        let h = HeaderMap::new();
        let total: u64 = 50 * 1024 * 1024 * 1024;
        let (start, end) = parse_range_header(&h, total).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, PKG_HOST_RESPONSE_BYTES_CAP - 1);
    }

    #[test]
    fn small_total_no_range_returns_full_file() {
        // Tiny pkg with no Range header: cap doesn't kick in, full
        // file is returned.
        let h = HeaderMap::new();
        let (start, end) = parse_range_header(&h, 100).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 99);
    }

    #[test]
    fn serve_partial_status_matches_body_coverage() {
        let total: u64 = 50 * 1024 * 1024 * 1024;
        // Bare GET trimmed by the cap → must be 206 (body < total).
        let (s, e) = parse_range_header(&HeaderMap::new(), total).unwrap();
        assert_eq!(s, 0);
        assert!(
            serve_is_partial(false, e, total),
            "trimmed bare GET must be 206"
        );
        // Bare GET on a small pkg that fits → bare 200.
        let (_s2, e2) = parse_range_header(&HeaderMap::new(), 100).unwrap();
        assert!(
            !serve_is_partial(false, e2, 100),
            "whole-file bare GET must be 200"
        );
        // Any explicit Range → 206, even if it happens to cover the file.
        assert!(serve_is_partial(true, 99, 100));
    }

    #[test]
    fn under_cap_range_passes_through_unchanged() {
        // A reasonable BGFT fetch (a few MB) is unaffected by the cap.
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=1000-2000".parse().unwrap());
        let (start, end) = parse_range_header(&h, 10_000_000).unwrap();
        assert_eq!(start, 1000);
        assert_eq!(end, 2000);
    }

    #[test]
    fn split_range_reads_within_one_part() {
        let dir = std::env::temp_dir().join(format!("pkg-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p1 = dir.join("a.pkg");
        std::fs::write(&p1, b"abcdefghij").unwrap();
        let s = dummy_session(vec![(p1, 10)]);
        let chunk = read_split_range(&s, 2, 5).unwrap();
        assert_eq!(chunk, b"cdef");
    }

    #[test]
    fn split_range_crosses_parts() {
        let dir = std::env::temp_dir().join(format!("pkg-test2-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p1 = dir.join("a.pkg");
        let p2 = dir.join("a.pkg.0");
        let p3 = dir.join("a.pkg.1");
        std::fs::write(&p1, b"AAAA").unwrap();
        std::fs::write(&p2, b"BBBB").unwrap();
        std::fs::write(&p3, b"CCCC").unwrap();
        let s = dummy_session(vec![(p1, 4), (p2, 4), (p3, 4)]);
        // Take the last byte of part0, all of part1, first byte of part2.
        let chunk = read_split_range(&s, 3, 8).unwrap();
        assert_eq!(chunk, b"ABBBBC");
    }

    #[test]
    fn sessions_lock_recovers_from_poison() {
        // Pin the poison-recovery contract for the sessions Mutex.
        // Every route handler now uses `.lock().unwrap_or_else(|e|
        // e.into_inner())` so a panic that propagates while the lock
        // is held doesn't permanently wedge the install API. This test
        // simulates that exact failure mode: hold the lock, panic
        // (mutex becomes poisoned), then verify a fresh `.lock()`
        // call still recovers the inner data via the recovery
        // pattern.
        let state = PkgInstallState::default();

        // Insert one session before the simulated panic so we can
        // assert the data isn't lost on recovery.
        {
            let mut sessions = state.sessions.lock().unwrap();
            sessions.insert(
                "before-panic".to_string(),
                dummy_session(vec![(PathBuf::from("/tmp/x"), 1)]),
            );
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Hold the lock, panic — this is what poisons the mutex.
            let _guard = state.sessions.lock().unwrap();
            panic!("simulated route-handler panic");
        }));
        assert!(result.is_err(), "the panic should propagate");
        assert!(
            state.sessions.is_poisoned(),
            "mutex must be poisoned after a panic-while-holding"
        );

        // The recovery pattern used at every call site in the engine:
        let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            sessions.len(),
            1,
            "data inserted before the panic must still be reachable"
        );
        assert!(sessions.contains_key("before-panic"));
    }

    // ── DPI daemon reply parser (the FW-10.40 helper-death fix, #152) ──
    //
    // The daemon now replies in the reference's ok/error form so the
    // engine can tell accept from reject from init-failure. These pin
    // every branch of the parser so a future daemon change can't
    // silently regress to "treat init-failure as install-success".

    #[test]
    fn dpi_parse_ok() {
        assert!(matches!(parse_dpi_reply("ok"), DpiReply::Ok));
        // trailing whitespace / newline tolerated
        assert!(matches!(parse_dpi_reply("ok\n"), DpiReply::Ok));
        assert!(matches!(parse_dpi_reply(" ok\r\n"), DpiReply::Ok));
    }

    #[test]
    fn dpi_parse_install_reject() {
        // 0x80B21106 — the FW-11/12 authid gate (the expected first-attempt
        // rejection that triggers the DPI fallback in the first place).
        assert!(matches!(
            parse_dpi_reply("error:0x80B21106"),
            DpiReply::InstallReject(rc) if rc as u32 == 0x80B21106
        ));
        assert!(matches!(
            parse_dpi_reply("error:0x80b21106\n"),
            DpiReply::InstallReject(rc) if rc as u32 == 0x80B21106
        ));
    }

    #[test]
    fn dpi_parse_init_failed_with_rc() {
        // sceAppInstUtilInitialize returned a Sony error — daemon is in
        // fallback mode and retrying will likely fail the same way.
        assert!(matches!(
            parse_dpi_reply("error:init:0x80B21106"),
            DpiReply::InitFailed(Some(rc)) if rc as u32 == 0x80B21106
        ));
    }

    #[test]
    fn dpi_parse_init_timeout() {
        // timed_init returned the -0xDEAD sentinel. Distinct from a Sony
        // error code — IPMI backend never came up.
        assert!(matches!(
            parse_dpi_reply("error:init:timeout"),
            DpiReply::InitFailed(None)
        ));
    }

    #[test]
    fn dpi_parse_badpath_and_recv() {
        assert!(matches!(
            parse_dpi_reply("error:badpath"),
            DpiReply::BadPath
        ));
        assert!(matches!(parse_dpi_reply("error:recv"), DpiReply::RecvError));
    }

    #[test]
    fn dpi_parse_legacy_decimal_ok() {
        // Backward compat: an older daemon still deployed on a console
        // replies with a bare decimal rc. "0" must map to Ok.
        assert!(matches!(parse_dpi_reply("0"), DpiReply::Ok));
        assert!(matches!(parse_dpi_reply("0\n"), DpiReply::Ok));
    }

    #[test]
    fn dpi_parse_legacy_decimal_reject() {
        // Legacy decimal reject: 0x80B21106 reinterpreted as i32 is
        // -2135813882 (i32::MIN + 0x7F8AAFAE + ... — two's complement).
        let expected: i32 = 0x80B21106_u32 as i32;
        assert!(matches!(
            parse_dpi_reply(&expected.to_string()),
            DpiReply::InstallReject(rc) if rc as u32 == 0x80B21106
        ));
    }

    #[test]
    fn dpi_parse_unknown_is_not_ok() {
        // An unrecognised reply must NEVER parse as Ok (would mask a
        // real failure as success) — it falls through to Unknown.
        assert!(matches!(parse_dpi_reply("error:???"), DpiReply::Unknown(_)));
        assert!(matches!(parse_dpi_reply(""), DpiReply::Unknown(_)));
    }
}
