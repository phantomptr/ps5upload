//! Unified install module (spec 2): one endpoint + orchestrator, status,
//! delivery decision, and per-console history. See
//! ps5upload-docs/superpowers/specs/2026-09-26-unified-install-module-design.md

pub mod status;

pub mod history;

pub mod deliver;

use std::collections::HashMap;
use std::sync::Mutex;

use ps5upload_core::installer_client as ic;

use status::{FailReason, InstallStatus, Phase, Verdict};

/// One install job per console at a time. `begin` reserves the console;
/// `finish` releases it. Statuses live here until the process restarts (the
/// durable record is the history log).
pub struct JobStore {
    jobs: Mutex<HashMap<String, InstallStatus>>,
    active: Mutex<HashMap<String, String>>, // console_id -> job_id
    seq: Mutex<u64>,
}

impl Default for JobStore {
    fn default() -> Self {
        Self::new()
    }
}

impl JobStore {
    pub fn new() -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            active: Mutex::new(HashMap::new()),
            seq: Mutex::new(0),
        }
    }

    fn new_job_id(&self) -> String {
        let mut s = self.seq.lock().unwrap();
        *s += 1;
        format!("{}-{}", status::now_unix(), *s)
    }

    /// Reserve the console. `Ok(job_id)` if free; `Err(active_job_id)` if an
    /// install is already running for that console.
    pub fn begin(&self, ps5_addr: &str) -> Result<String, String> {
        let cid = console_id(ps5_addr);
        let mut active = self.active.lock().unwrap();
        if let Some(j) = active.get(&cid) {
            return Err(j.clone());
        }
        let job = self.new_job_id();
        active.insert(cid, job.clone());
        self.jobs
            .lock()
            .unwrap()
            .insert(job.clone(), InstallStatus::new(&job, ps5_addr, ""));
        Ok(job)
    }

    pub fn finish(&self, job: &str) {
        let cid = self
            .jobs
            .lock()
            .unwrap()
            .get(job)
            .map(|st| console_id(&st.ps5_addr));
        if let Some(cid) = cid {
            let mut active = self.active.lock().unwrap();
            // only clear if this job still owns the slot
            if active.get(&cid).map(|j| j.as_str()) == Some(job) {
                active.remove(&cid);
            }
        }
    }

    pub fn get(&self, job: &str) -> Option<InstallStatus> {
        self.jobs.lock().unwrap().get(job).cloned()
    }

    pub fn update(&self, job: &str, f: impl FnOnce(&mut InstallStatus)) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(st) = jobs.get_mut(job) {
            f(st);
            st.updated_at = status::now_unix();
        }
    }
}

/// The per-console key: the host part of the address, port-stripped, so
/// `ip:9114` and `ip:9113` and a bare `ip` all map to one console.
pub fn console_id(ps5_addr: &str) -> String {
    match ps5_addr.rsplit_once(':') {
        Some((host, _)) => host.to_string(),
        None => ps5_addr.to_string(),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum GuardDecision {
    Allow,
    Refuse,
}

/// A base-game reinstall over an already-installed title wipes it before
/// writing the new copy; a patch/DLC shares the base's content_id but does
/// not wipe it. Category suffixes: base games end `gd`/`GD` (PS4) or are the
/// default; patches end `gp`/`DP`; DLC ends `ac`.
pub fn is_destructive_reinstall(category: &str, already_installed: bool) -> bool {
    if !already_installed {
        return false;
    }
    let c = category.to_ascii_lowercase();
    let is_patch_or_dlc = c.ends_with("gp") || c.ends_with("dp") || c.ends_with("ac");
    !is_patch_or_dlc
}

pub fn guard_decision(category: &str, already_installed: bool, allow: bool) -> GuardDecision {
    if is_destructive_reinstall(category, already_installed) && !allow {
        GuardDecision::Refuse
    } else {
        GuardDecision::Allow
    }
}

/// Map an `installer_client::ensure` result to a status. A listening daemon
/// advances to Install; otherwise it is a terminal failure carrying the
/// machine reason (never a silent fallback).
pub fn status_from_ensure(e: &ic::Ensure) -> InstallStatus {
    let mut s = InstallStatus::new("", "", "");
    if e.listening {
        s.phase = Phase::Install;
        return s;
    }
    s.phase = Phase::Failed;
    s.verdict = Some(Verdict::Failed);
    s.reason = Some(match e.reason {
        Some("loader_unreachable") => FailReason::LoaderUnreachable,
        Some("no_image") => FailReason::NoImage,
        _ => FailReason::NoBringup,
    });
    s.hint = e.error.clone();
    s
}

/// The verdict comes from the artifact check (verify), not the socket
/// outcome — an ambiguous/accepted daemon reply defers to this.
pub fn verdict_from_verify(
    pv: ps5upload_core::patch_verify::PatchVerdict,
    launchable: bool,
) -> Verdict {
    use ps5upload_core::patch_verify::PatchVerdict::*;
    match pv {
        DidNotApply | Regressed => Verdict::Failed,
        Applied | Inconclusive => {
            if launchable {
                Verdict::Installed
            } else {
                Verdict::MayNotLaunch
            }
        }
    }
}

/// When the caller supplied no identity to verify against, trust the daemon's
/// accept.
pub fn verdict_no_identity(accepted: bool) -> Verdict {
    if accepted {
        Verdict::Installed
    } else {
        Verdict::Failed
    }
}

/// Fill identity fields the caller left empty from the package header.
/// Anything the caller sent wins; an empty header field fills nothing.
pub fn fill_from_header(
    req: &mut InstallRequest,
    content_id: &str,
    title_id: &str,
    category: &str,
    app_ver: &str,
) {
    let some = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
    if req.content_id.trim().is_empty() {
        if let Some(c) = some(content_id) {
            req.content_id = c;
        }
    }
    if req.title_id.as_deref().is_none_or(|t| t.trim().is_empty()) {
        req.title_id = some(title_id).or(req.title_id.take());
    }
    if req.category.as_deref().is_none_or(|c| c.trim().is_empty()) {
        req.category = some(category).or(req.category.take());
    }
    if req
        .package_app_ver
        .as_deref()
        .is_none_or(|v| v.trim().is_empty())
    {
        req.package_app_ver = some(app_ver).or(req.package_app_ver.take());
    }
}

/// Whether a failed start should recycle the installer daemon: a stream
/// install whose URL the console never fetched, refused with a network-class
/// Sony code (0x8043xxxx — the same class the daemon itself treats as
/// "network"). Measured on FW 5.10: after one such failure every later URL
/// install fails instantly (0x80431064) until the daemon process restarts;
/// Sony's install library keeps the bad state for the life of the process.
pub fn should_recycle_daemon(stream: bool, never_fetched: bool, code: u32) -> bool {
    stream && never_fetched && (code & 0xFFFF_0000) == 0x8043_0000
}

/// Whether a stalled delivery should recycle the installer daemon: only when
/// the daemon served it (a console-path install). The daemon keeps a loopback
/// job "serving" until Sony has read every byte, which a stalled install never
/// does, so without a restart every later install is refused as busy.
pub fn should_recycle_after_stall(daemon_served: bool) -> bool {
    daemon_served
}

/// Stop the installer daemon and wait (bounded) until :9115 has actually
/// closed, so the next install's ensure sends a fresh one. A daemon still
/// answering while it exits makes that ensure skip sending a new one and then
/// hit a refused connect.
async fn recycle_daemon(ip: &str) {
    let ip_s = ip.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        let _ = ic::stop(&ip_s);
        use ps5upload_core::payload_lifecycle as pl;
        let addr = pl::join_host_port(&ip_s, pl::INSTALLER_PORT);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline
            && pl::port_is_open(&addr, std::time::Duration::from_millis(500))
        {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    })
    .await;
}

/// Sony's HTTP-proxy rejection of the install URL.
const SCE_HTTP_ERROR_PROXY: u32 = 0x8043_1084;

/// Guidance for a stream install the console never fetched a byte of. With
/// no proxy error this is the console failing to reach this computer at all
/// (usually a firewall), so saying "the PS5 declined the install" sends the
/// user looking in the wrong place.
/// Why Sony refused an install: a stream the console never fetched from gets
/// its own reason, so the UI can explain it in the user's language.
pub fn refusal_reason(never_fetched: bool, code: u32, route: Route) -> FailReason {
    match (never_fetched && code != 0, code) {
        (true, SCE_HTTP_ERROR_PROXY) => FailReason::StreamProxy,
        (true, _) => FailReason::StreamUnreachable,
        _ if route == Route::Loopback && STAGED_ROUTE_REFUSALS.contains(&code) => {
            FailReason::StagedRefused
        }
        _ => FailReason::SonyRefused,
    }
}

/// Sony codes measured refusing the staged (Loopback) route while the same
/// package streamed from a computer installed: 0x80B2116F on FW 9.60 and
/// 13.60 (the latter from a user whose every install came from a phone, which
/// can only stage), 0x80B2150F on FW 5.10.
const STAGED_ROUTE_REFUSALS: &[u32] = &[0x80B2_116F, 0x80B2_150F];

pub fn stream_unreachable_hint(served_from: Option<&str>, code: u32) -> String {
    stream_unreachable_hint_for(
        served_from,
        code,
        crate::pkg_install::bridged_container_without_pkg_host_ip(),
    )
}

/// `stream_unreachable_hint` with the container check passed in, so both
/// wordings are testable. In a bridged container the firewall advice is
/// wrong: the engine told the PS5 to fetch from the container's internal
/// address, which nothing outside the Docker host can reach.
fn stream_unreachable_hint_for(
    served_from: Option<&str>,
    code: u32,
    bridged_container: bool,
) -> String {
    let rc = format!("0x{code:08x}");
    if bridged_container {
        let at = served_from.map(|o| format!(" at {o}")).unwrap_or_default();
        return format!(
            "The PS5 never reached the engine{at} to fetch the package ({rc}). The engine is running in a container, so that is the container's internal address, which the PS5 cannot reach. Run the container with host networking (`--network host`, or `network_mode: host` in Compose), or set PS5UPLOAD_PKG_HOST_IP to the Docker host's LAN IP and publish port 19113."
        );
    }
    if code == SCE_HTTP_ERROR_PROXY {
        return format!(
            "The PS5's proxy setting blocked the stream ({rc}). In the PS5's network Advanced Settings set Proxy Server to \u{201c}Do Not Use\u{201d}, or use Upload & install, which reads the package from PS5-local storage."
        );
    }
    let at = served_from.map(|o| format!(" at {o}")).unwrap_or_default();
    format!(
        "The PS5 never reached this computer{at} to fetch the package ({rc}). Allow ps5upload through this computer's firewall (on Windows, for both Private and Public networks), keep the computer and the PS5 on the same network with any VPN off, and set the PS5's Proxy Server to \u{201c}Do Not Use\u{201d}. Upload & install works without this connection."
    )
}

/// Ask the helper whether the console can open a connection to the engine's
/// pkg-host origin in `url`. `Some(message)` only when it definitely cannot;
/// `None` when it can, or when the check could not run (an older helper, an
/// unparseable URL) — that must never block an install that might work.
async fn reach_block(ip: &str, url: &str) -> Option<String> {
    let origin = origin_of(url)?;
    let authority = origin.split_once("://")?.1.to_string();
    let (host, port) = authority.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let (mgmt, h) = (
        crate::mgmt_addr_for(ip),
        host.trim_matches(|c| c == '[' || c == ']').to_string(),
    );
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::diagnostics::net_reach(&mgmt, &h, port, 4000)
    })
    .await
    .ok()?
    .ok()?;
    (!r.ok).then(|| reach_block_message(&origin, &r))
}

/// The user-facing reason a reach check failed. A timeout means packets are
/// being dropped (a firewall on this computer, or client isolation on the
/// network); a refusal means nothing accepted the connection on that port.
fn reach_block_message(origin: &str, r: &ps5upload_core::diagnostics::NetReach) -> String {
    let cause = if r.timed_out {
        "the connection timed out, which usually means a firewall on this computer is silently dropping it (on Windows, allow ps5upload for both Private and Public networks; a PS5 connected through Internet Connection Sharing sits on a Public network), or the Wi-Fi isolates devices from each other"
    } else if r.errno == 61 || r.errno == 111 || r.err.to_ascii_lowercase().contains("refused") {
        "the connection was refused, so nothing is accepting connections on that port for the PS5 — a firewall rule that rejects it, or the engine listening on another address"
    } else {
        "the network would not let it through"
    };
    format!(
        "The PS5 cannot connect to this computer at {origin} ({}), so a stream install cannot start: {cause}. Upload & install copies the package to the PS5 instead and does not need this connection.",
        if r.err.is_empty() { format!("after {} ms", r.ms) } else { format!("{} after {} ms", r.err, r.ms) }
    )
}

/// True when `path` is a game's own installed file on the console —
/// `…/user/app/<id>/…`, `…/user/patch/<id>/…`, `…/user/addcont/<id>/…`, on
/// internal storage or extended storage (`/mnt/ext*/user/…`). Installing one
/// reinstalls the game from itself, and an update reinstall removes the old
/// update before applying the new one — the very file being read.
pub fn is_installed_content_path(path: &str) -> bool {
    let p = path.trim_end_matches('/');
    let rest = if let Some(r) = p.strip_prefix("/user/") {
        r
    } else if let Some(r) = p.strip_prefix("/mnt/ext") {
        match r.split_once("/user/") {
            Some((n, r)) if !n.contains('/') => r,
            _ => return false,
        }
    } else {
        return false;
    };
    ["app/", "patch/", "addcont/"]
        .iter()
        .any(|d| rest.starts_with(d))
}

/// `http://host:port` of a URL, for naming where the console was sent.
fn origin_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?;
    let host = rest.1.split('/').next()?;
    (!host.is_empty()).then(|| format!("{}://{host}", rest.0))
}

/// The version to check after install, or `None` when the "did the installed
/// APP_VER rise?" check does not apply. Only a patch (`…DP` / `gp`) bumps an
/// installed title's version; a base game or DLC carries its own `APP_VER`,
/// and comparing a DLC's 01.00 against a base at 01.09 would read as
/// "regressed" and fail a good install.
pub fn patch_check_version(category: &str, package_app_ver: Option<&str>) -> Option<String> {
    let c = category.to_ascii_lowercase();
    let is_patch = c.ends_with("dp") || c.ends_with("gp");
    match package_app_ver.map(str::trim) {
        Some(v) if is_patch && !v.is_empty() => Some(v.to_string()),
        _ => None,
    }
}

/// How long delivery may make no progress before the install is judged
/// stalled. Generous: a healthy console pulls continuously, and a large title
/// must never be written off for a pause.
pub const DELIVERY_STALL_MS: u64 = 5 * 60 * 1000;

/// Where the console is in pulling the package after Sony accepted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryProgress {
    /// Still pulling (or no total to compare against yet).
    Pending,
    /// Every byte has been pulled; the source is no longer needed.
    Complete,
    /// No progress for the whole stall window.
    Stalled,
}

/// Pure: decide delivery progress from bytes served so far, the package
/// total, and how long since the byte count last moved. Sony *accepting* an
/// install is not completion — PS5 installs pull asynchronously — so the
/// orchestrator waits on this before it reports a verdict or lets a caller
/// delete the source.
pub fn delivery_progress(served: u64, total: u64, idle_ms: u64, stall_ms: u64) -> DeliveryProgress {
    if total > 0 && served >= total {
        DeliveryProgress::Complete
    } else if idle_ms >= stall_ms {
        DeliveryProgress::Stalled
    } else {
        DeliveryProgress::Pending
    }
}

/// The daemon's own job phase, when it settles delivery by itself: "done"
/// (the loopback source was fully read) or "accepted" (the path was handed to
/// Sony directly, so there is nothing for the daemon to serve).
pub fn daemon_phase_progress(phase: &str) -> Option<DeliveryProgress> {
    matches!(phase, "done" | "accepted").then_some(DeliveryProgress::Complete)
}

use crate::install::deliver::{decide_delivery, needs_short_alias, Delivery, Source};
use crate::install::history::HistoryEntry;
use crate::install::status::Route;
use crate::pkg_install::PkgInstallStateHandle;

use axum::{
    extract::{Json, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct InstallOptions {
    #[serde(default)]
    pub delete_source_copy_after: bool,
    #[serde(default)]
    pub allow_destructive_reinstall: bool,
}

#[derive(Debug, Deserialize)]
pub struct InstallRequest {
    pub ps5_addr: String,
    pub source: Source,
    #[serde(default)]
    pub content_id: String,
    #[serde(default)]
    pub title_id: Option<String>,
    #[serde(default)]
    pub package_app_ver: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub options: InstallOptions,
}

/// Where the install history log lives: `<data>/install-history/`
/// (`PS5UPLOAD_DATA_DIR`, else `~/.ps5upload`, else a relative dir).
fn history_dir() -> std::path::PathBuf {
    crate::remote::store::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from(".ps5upload"))
        .join("install-history")
}

/// Correlation tag for an install's log lines, so a past failure's engine
/// lines and the daemon `stderr.log` span can be lined up in a bug bundle.
pub fn correlation_tag(job: &str) -> String {
    let short: String = job.chars().take(8).collect();
    format!("install[{short}]")
}

/// Kind-only content name for the daemon (never title-bearing).
fn name_hint(title_id: Option<&str>) -> String {
    match title_id {
        Some(t) if !t.trim().is_empty() => format!("{t} (Base)"),
        _ => String::new(),
    }
}

/// `POST /api/pkg/install` — reserve the console, start the async state
/// machine, return the job id. `busy` (with the active job) if an install is
/// already running for that console.
pub async fn install_handler(
    State(state): State<PkgInstallStateHandle>,
    Json(req): Json<InstallRequest>,
) -> Response {
    let ip = console_id(&req.ps5_addr);
    if ip.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"ok":false,"error":"ps5_addr is required"})),
        )
            .into_response();
    }
    if let Source::ConsolePath(path) = &req.source {
        if is_installed_content_path(path) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"ok":false,"error":format!(
                    "{path} is the console's own copy of an installed game, update or \
                     add-on, not a package to install. Install from the original .pkg instead."
                )})),
            )
                .into_response();
        }
    }
    let job = match state.jobs.begin(&req.ps5_addr) {
        Ok(j) => j,
        Err(active) => {
            return Json(serde_json::json!({"ok":false,"error":"busy","job":active}))
                .into_response()
        }
    };
    let st = state.clone();
    let job2 = job.clone();
    tokio::spawn(async move {
        run_install(st, job2, req).await;
    });
    Json(serde_json::json!({"ok":true,"job":job})).into_response()
}

#[derive(Debug, Deserialize)]
pub struct JobQuery {
    pub job: String,
}

pub async fn install_status_handler(
    State(state): State<PkgInstallStateHandle>,
    Query(q): Query<JobQuery>,
) -> Response {
    match state.jobs.get(&q.job) {
        Some(st) => Json(st).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"ok":false,"error":"no such job"})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct AddrQuery {
    pub ps5_addr: String,
}

pub async fn install_history_handler(Query(q): Query<AddrQuery>) -> Response {
    let entries = history::read_recent(&history_dir(), &q.ps5_addr, 50);
    Json(entries).into_response()
}

/// The async state machine: resolve → deliver → install → verify → done/failed.
/// Always clears the active-job guard and records history at the end.
async fn run_install(state: PkgInstallStateHandle, job: String, mut req: InstallRequest) {
    let started = std::time::Instant::now();
    let tag = correlation_tag(&job);
    let ip = console_id(&req.ps5_addr);
    let mgmt = crate::pkg_install::normalize_mgmt_addr(&req.ps5_addr);
    // A console-side package (upload queue, USB, File System) often arrives
    // without its category/version: read them off the package's own header
    // so the destructive-reinstall guard and the patch version check work.
    if let Source::ConsolePath(path) = &req.source {
        if req.category.is_none() || req.package_app_ver.is_none() || req.title_id.is_none() {
            let (m, p) = (mgmt.clone(), path.clone());
            let meta = tokio::task::spawn_blocking(move || {
                ps5upload_pkg::metadata_from_reader(|off, len| {
                    ps5upload_core::fs_ops::fs_read(&m, &p, off, len).ok()
                })
            })
            .await
            .ok()
            .flatten();
            if let Some(h) = meta {
                fill_from_header(
                    &mut req,
                    &h.content_id,
                    &h.title_id,
                    &h.category,
                    &h.app_ver,
                );
            }
        }
    }
    crate::log_info!(
        "{tag}: install start ps5={ip} source={} content_id={} category={} app_ver={}",
        req.source.kind(),
        req.content_id,
        req.category.as_deref().unwrap_or("?"),
        req.package_app_ver.as_deref().unwrap_or("?")
    );
    let category = req.category.clone().unwrap_or_default();
    let patch_ver = patch_check_version(&category, req.package_app_ver.as_deref());
    let title_id = req.title_id.clone().filter(|t| !t.trim().is_empty());
    let mut route = match decide_delivery(&req.source) {
        Delivery::Loopback => Route::Loopback,
        Delivery::Stream => Route::Stream,
    };
    state.jobs.update(&job, |s| {
        s.content_id = req.content_id.clone();
        s.title_id = title_id.clone();
        s.route = Some(route);
        s.phase = Phase::Resolve;
    });

    // resolve: destructive-reinstall guard.
    let already_installed = match &title_id {
        Some(t) => {
            let (m, t) = (mgmt.clone(), t.clone());
            tokio::task::spawn_blocking(move || {
                crate::pkg_install::read_installed_app_ver(&m, &t).is_some()
            })
            .await
            .unwrap_or(false)
        }
        None => false,
    };
    if let GuardDecision::Refuse = guard_decision(
        &category,
        already_installed,
        req.options.allow_destructive_reinstall,
    ) {
        state.jobs.update(&job, |s| {
            s.phase = Phase::Failed;
            s.verdict = Some(Verdict::Failed);
            s.reason = Some(FailReason::DestructiveGuard);
            s.hint = Some(
                "this would erase the installed game first; re-run with allow_destructive_reinstall"
                    .into(),
            );
        });
        finalize(&state, &job, &req, started);
        return;
    }

    // deliver: for a stream source, create a serve-only pkg-host session and
    // get its URL by reusing the existing start handler internally.
    let app_ver_before = match (&title_id, &patch_ver) {
        (Some(t), Some(_)) => {
            let (m, t) = (mgmt.clone(), t.clone());
            tokio::task::spawn_blocking(move || crate::pkg_install::read_installed_app_ver(&m, &t))
                .await
                .ok()
                .flatten()
        }
        _ => None,
    };
    state.jobs.update(&job, |s| {
        s.phase = Phase::Deliver;
        s.app_ver_before = app_ver_before.clone();
    });
    let deliver_started = std::time::Instant::now();

    // ensure the daemon. It is sent as a companion image, so it runs alongside
    // the helper and never displaces it.
    let elf = crate::bundled_payload::image_bytes(crate::bundled_payload::Image::Installer).ok();
    let ip_for_ensure = ip.clone();
    let ens =
        tokio::task::spawn_blocking(move || ic::ensure(&ip_for_ensure, elf.as_deref(), false))
            .await
            .unwrap_or(ic::Ensure {
                listening: false,
                sent: false,
                state: None,
                reason: Some("no_bringup"),
                error: Some("ensure task failed".into()),
            });
    if !ens.listening {
        let mapped = status_from_ensure(&ens);
        state.jobs.update(&job, |s| {
            s.phase = Phase::Failed;
            s.verdict = mapped.verdict;
            s.reason = mapped.reason;
            s.hint = mapped.hint.clone();
        });
        finalize(&state, &job, &req, started);
        return;
    }

    // build the install call per source.
    let hint_name = name_hint(title_id.as_deref());
    state.jobs.update(&job, |s| s.phase = Phase::Install);
    let mut served_from: Option<String> = None;
    let (reply, session_id, shortened): (Result<ic::InstallReply, String>, Option<String>, bool) =
        match &req.source {
            Source::ConsolePath(path) => {
                let (i, p, h) = (ip.clone(), path.clone(), hint_name.clone());
                let r = tokio::task::spawn_blocking(move || ic::install_path(&i, &p, &h))
                    .await
                    .unwrap_or_else(|e| Err(format!("install task failed: {e}")));
                // The console refused the copy it serves itself (0x80B2116F on
                // FW 9.60/13.60, 0x80B2150F on 5.10: Sony's overwrite/patch
                // check). The same bytes served from this engine install, so
                // do that — read back through the helper, one job, one result.
                match &r {
                    Ok(ic::InstallReply::Sony { code, .. })
                        if STAGED_ROUTE_REFUSALS.contains(code) =>
                    {
                        crate::log_info!(
                            "{tag}: the console refused its own copy (0x{code:08X}); serving it from this engine instead"
                        );
                        let console_url = format!("ps5://{ip}{path}");
                        match create_serve_session_for(&state, &req, &console_url).await {
                            Ok((sid, url)) => {
                                served_from = origin_of(&url);
                                if let Some(block) = reach_block(&ip, &url).await {
                                    crate::pkg_install::release_serve_session(
                                        &state.sessions,
                                        &sid,
                                    );
                                    crate::log_warn!("{tag}: {block}");
                                    (r, None, false)
                                } else {
                                    route = Route::Stream;
                                    state.jobs.update(&job, |s| s.route = Some(Route::Stream));
                                    let (i, u, h) = (ip.clone(), url, hint_name.clone());
                                    let r2 = tokio::task::spawn_blocking(move || {
                                        ic::install_url(&i, &u, &h)
                                    })
                                    .await
                                    .unwrap_or_else(|e| Err(format!("install task failed: {e}")));
                                    (r2, Some(sid), false)
                                }
                            }
                            Err(e) => {
                                crate::log_warn!("{tag}: could not serve the console's copy: {e}");
                                (r, None, false)
                            }
                        }
                    }
                    _ => (r, None, false),
                }
            }
            Source::Url(url) => {
                // The daemon installs the URL directly; alias it if too long.
                let (i, u) = (ip.clone(), url.clone());
                let short = if needs_short_alias(url) {
                    tokio::task::spawn_blocking({
                        let (a, u) = (req.ps5_addr.clone(), url.clone());
                        move || crate::pkg_install::shorten_for_installer(&a, &u)
                    })
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .flatten()
                } else {
                    None
                };
                let was_short = short.is_some();
                let final_url = short.unwrap_or(u);
                let h = hint_name.clone();
                let r = tokio::task::spawn_blocking(move || ic::install_url(&i, &final_url, &h))
                    .await
                    .unwrap_or_else(|e| Err(format!("install task failed: {e}")));
                // The console refused the link as served by its own server. On
                // FW 13.60 that is what a server without a usable
                // Last-Modified gets (0x80B2116F; the same link with one
                // installs — measured back to back). This engine's link proxy
                // always sends one, so serve the link through it instead.
                match &r {
                    Ok(ic::InstallReply::Sony { code, .. })
                        if STAGED_ROUTE_REFUSALS.contains(code) =>
                    {
                        crate::log_info!(
                            "{tag}: the console refused the link (0x{code:08X}); proxying it through this engine instead"
                        );
                        match create_serve_session_for(&state, &req, url).await {
                            Ok((sid, purl)) => {
                                served_from = origin_of(&purl);
                                if let Some(block) = reach_block(&ip, &purl).await {
                                    crate::pkg_install::release_serve_session(
                                        &state.sessions,
                                        &sid,
                                    );
                                    crate::log_warn!("{tag}: {block}");
                                    (r, None, was_short)
                                } else {
                                    route = Route::Stream;
                                    state.jobs.update(&job, |s| s.route = Some(Route::Stream));
                                    let (i, u, h) = (ip.clone(), purl, hint_name.clone());
                                    let r2 = tokio::task::spawn_blocking(move || {
                                        ic::install_url(&i, &u, &h)
                                    })
                                    .await
                                    .unwrap_or_else(|e| Err(format!("install task failed: {e}")));
                                    (r2, Some(sid), false)
                                }
                            }
                            Err(e) => {
                                crate::log_warn!("{tag}: could not proxy the link: {e}");
                                (r, None, was_short)
                            }
                        }
                    }
                    _ => (r, None, was_short),
                }
            }
            Source::HostFile(_) | Source::Remote { .. } => {
                match create_serve_session(&state, &req).await {
                    Ok((sid, url)) => {
                        served_from = origin_of(&url);
                        // Can the console reach us at all? A firewall that
                        // drops the connection otherwise shows up only as
                        // Sony's 0x80431068 after a 30 s wait (#342).
                        if let Some(block) = reach_block(&ip, &url).await {
                            crate::pkg_install::release_serve_session(&state.sessions, &sid);
                            crate::log_warn!("{tag}: {block}");
                            state.jobs.update(&job, |s| {
                                s.phase = Phase::Failed;
                                s.verdict = Some(Verdict::Failed);
                                s.reason = Some(FailReason::StreamUnreachable);
                                s.hint = Some(block.clone());
                            });
                            finalize(&state, &job, &req, started);
                            return;
                        }
                        let (i, u, h) = (ip.clone(), url, hint_name.clone());
                        let r = tokio::task::spawn_blocking(move || ic::install_url(&i, &u, &h))
                            .await
                            .unwrap_or_else(|e| Err(format!("install task failed: {e}")));
                        (r, Some(sid), false)
                    }
                    Err(e) => (Err(e), None, false),
                }
            }
        };
    let deliver_ms = deliver_started.elapsed().as_millis() as u64;

    // interpret the daemon reply.
    let (accepted, code, hint) = match &reply {
        Ok(ic::InstallReply::Accepted { .. }) => (true, 0u32, None),
        Ok(ic::InstallReply::Busy { job: j }) => (
            false,
            0,
            Some(format!("an install is already in progress (job {j})")),
        ),
        Ok(ic::InstallReply::NotReady { init_rc }) => (
            false,
            *init_rc,
            Some(format!("installer not ready (init 0x{init_rc:08X})")),
        ),
        Ok(ic::InstallReply::BadPath) => (
            false,
            0,
            Some("the installer rejected the package path".into()),
        ),
        Ok(ic::InstallReply::BadRequest) => {
            (false, 0, Some("the installer rejected the request".into()))
        }
        Ok(ic::InstallReply::UnknownJob) => (false, 0, Some("no such install job".into())),
        Ok(ic::InstallReply::Sony { code, hint }) => (false, *code, hint.clone()),
        Ok(ic::InstallReply::Unknown(s)) => (
            false,
            0,
            Some(format!("installer reply not understood: {s}")),
        ),
        Err(e) => (false, 0, Some(e.clone())),
    };
    let install_job_id = match &reply {
        Ok(ic::InstallReply::Accepted { job, .. }) => Some(job.clone()),
        _ => None,
    };

    if !accepted {
        // Sony (or the daemon) refused the start, so the console never pulled
        // from the serving session: release it, or a retry of the same package
        // is refused as "already running".
        // A Sony error on a stream the console never fetched from means it
        // could not reach this computer (or its proxy blocked it) — say so.
        let never_fetched = session_id.as_ref().is_some_and(|sid| {
            let s = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
            s.get(sid).is_some_and(|x| x.bytes_served == 0)
        });
        let hint = if never_fetched && code != 0 {
            Some(stream_unreachable_hint(served_from.as_deref(), code))
        } else {
            hint
        };
        if let Some(sid) = &session_id {
            crate::pkg_install::release_serve_session(&state.sessions, sid);
        }
        if should_recycle_daemon(route == Route::Stream, never_fetched, code) {
            crate::log_warn!(
                "{tag}: network-class refusal 0x{code:08X} before any fetch — recycling the installer daemon so the next install starts clean"
            );
            recycle_daemon(&ip).await;
        }
        state.jobs.update(&job, |s| {
            s.phase = Phase::Failed;
            s.verdict = Some(Verdict::Failed);
            s.reason = Some(refusal_reason(never_fetched, code, route));
            s.code = code;
            s.hint = hint.clone();
            s.shortened = shortened;
            s.metrics.sony_rc = code;
            s.metrics.phase_ms.insert("deliver".into(), deliver_ms);
        });
        finalize(&state, &job, &req, started);
        return;
    }

    // wait for delivery. Sony ACCEPTING an install is not completion: PS5
    // installs pull the package asynchronously (measured: an 820 MB stream
    // install was accepted at 13 MB). Track what the console has pulled until
    // the whole package is read — only then is the source safe to delete and a
    // verdict honest. Live metrics are published while waiting so the client's
    // progress bar moves. A bare URL is fetched by the console itself, so there
    // is nothing to observe and it proceeds straight to verify.
    let observable = session_id.is_some()
        || (install_job_id.is_some() && matches!(req.source, Source::ConsolePath(_)));
    let mut last_served: u64 = 0;
    let mut last_move = std::time::Instant::now();
    let delivery = if !observable {
        DeliveryProgress::Complete
    } else {
        loop {
            // (served, total, daemon job phase)
            let obs: Option<(u64, u64, Option<String>)> = if let Some(sid) = &session_id {
                let s = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
                s.get(sid).map(|x| (x.bytes_served, x.total_size, None))
            } else if let Some(jid) = &install_job_id {
                let (i, j) = (ip.clone(), jid.clone());
                tokio::task::spawn_blocking(move || ic::job(&i, &j))
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .map(|j| (j.bytes_served, j.total, Some(j.phase)))
            } else {
                None
            };
            // An unreadable observation counts as "no movement": it can only
            // run the stall clock, never declare success.
            let (served, total, dphase) = obs.unwrap_or((last_served, 0, None));
            if served > last_served {
                last_served = served;
                last_move = std::time::Instant::now();
            }
            state.jobs.update(&job, |s| {
                s.metrics.served_bytes = served;
                if total > 0 {
                    s.metrics.total_bytes = total;
                }
            });
            let p = if let Some(p) = dphase.as_deref().and_then(daemon_phase_progress) {
                p
            } else {
                delivery_progress(
                    served,
                    total,
                    last_move.elapsed().as_millis() as u64,
                    DELIVERY_STALL_MS,
                )
            };
            if p != DeliveryProgress::Pending {
                break p;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    };
    // The console has finished with the stream source (pulled it all, or
    // stalled): release the pkg-host session so the same package can be
    // installed again. Its counters stay readable for the metrics below.
    if let Some(sid) = &session_id {
        crate::pkg_install::release_serve_session(&state.sessions, sid);
    }
    let deliver_ms = deliver_started.elapsed().as_millis() as u64;
    if delivery == DeliveryProgress::Stalled {
        crate::log_warn!(
            "{tag}: delivery stalled at {last_served} bytes — source kept, reporting failed"
        );
        if should_recycle_after_stall(session_id.is_none() && install_job_id.is_some()) {
            crate::log_warn!(
                "{tag}: recycling the installer daemon so a retry is not refused as busy"
            );
            recycle_daemon(&ip).await;
        }
        state.jobs.update(&job, |s| {
            s.phase = Phase::Failed;
            s.verdict = Some(Verdict::Failed);
            s.reason = Some(FailReason::Stalled);
            s.hint = Some(
                "the console stopped fetching the package before it finished; the package was kept so you can retry"
                    .into(),
            );
            s.shortened = shortened;
            s.metrics.phase_ms.insert("deliver".into(), deliver_ms);
        });
        finalize(&state, &job, &req, started);
        return;
    }

    // verify.
    state.jobs.update(&job, |s| s.phase = Phase::Verify);
    let verify_started = std::time::Instant::now();
    let (verdict, patch_verdict, app_ver_after) = match (&title_id, &patch_ver) {
        (Some(t), Some(pv)) => {
            let (m, t, pv, before) = (mgmt.clone(), t.clone(), pv.clone(), app_ver_before.clone());
            let (cv, after) = tokio::task::spawn_blocking(move || {
                crate::pkg_install::verify_patch_after_install(&m, &t, before.as_deref(), &pv)
            })
            .await
            .unwrap_or((
                ps5upload_core::patch_verify::PatchVerdict::Inconclusive,
                None,
            ));
            let launchable = !matches!(
                cv,
                ps5upload_core::patch_verify::PatchVerdict::DidNotApply
                    | ps5upload_core::patch_verify::PatchVerdict::Regressed
            );
            (
                verdict_from_verify(cv, launchable),
                Some(status::PatchVerdict::from(cv)),
                after,
            )
        }
        _ => (verdict_no_identity(true), None, None),
    };
    let verify_ms = verify_started.elapsed().as_millis() as u64;

    // metrics: served bytes + throughput.
    let (served, total) = match (&route, &session_id, &install_job_id) {
        (Route::Stream, Some(sid), _) => {
            let s = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
            s.get(sid)
                .map(|x| (x.bytes_served, x.total_size))
                .unwrap_or((0, 0))
        }
        (Route::Loopback, _, Some(jid)) => {
            let (i, jid) = (ip.clone(), jid.clone());
            tokio::task::spawn_blocking(move || ic::job(&i, &jid))
                .await
                .ok()
                .and_then(|r| r.ok())
                .map(|j| (j.bytes_served, j.total))
                .unwrap_or((0, 0))
        }
        _ => (0, 0),
    };

    state.jobs.update(&job, |s| {
        s.phase = Phase::Done;
        s.verdict = Some(verdict);
        s.patch_verdict = patch_verdict;
        s.app_ver_after = app_ver_after.clone();
        s.shortened = shortened;
        s.metrics.total_bytes = total;
        s.metrics.served_bytes = served;
        s.metrics.throughput_mbps = status::throughput_mbps(served, deliver_ms);
        s.metrics.phase_ms.insert("deliver".into(), deliver_ms);
        s.metrics.phase_ms.insert("verify".into(), verify_ms);
    });
    finalize(&state, &job, &req, started);
}

/// Reuse the existing start handler in serve-only mode to create a pkg-host
/// session for a host/remote source, returning (session_id, url).
async fn create_serve_session(
    state: &PkgInstallStateHandle,
    req: &InstallRequest,
) -> Result<(String, String), String> {
    let path = match &req.source {
        Source::HostFile(p) => p.clone(),
        Source::Remote { connection, path } => format!("remote://{connection}/{path}"),
        _ => return Err("create_serve_session called for a non-stream source".into()),
    };
    create_serve_session_for(state, req, &path).await
}

/// A serve session for an explicit source path — a local file, `remote://…`
/// on a saved server, or `ps5://<console>/<path>` read back through the helper.
async fn create_serve_session_for(
    state: &PkgInstallStateHandle,
    req: &InstallRequest,
    path: &str,
) -> Result<(String, String), String> {
    // A link is proxied (its origin read over many connections); anything
    // else is a path the engine reads — local, remote:// or ps5://.
    let is_link = path.starts_with("http://") || path.starts_with("https://");
    let start_req = if is_link {
        serde_json::json!({
            "ps5_addr": req.ps5_addr,
            "serve_only": true,
            "remote_url": path,
            "content_id": req.content_id,
        })
    } else {
        serde_json::json!({
            "ps5_addr": req.ps5_addr,
            "serve_only": true,
            "path": path,
            "content_id": req.content_id,
        })
    };
    let start_req: crate::pkg_install::InstallStartRequest =
        serde_json::from_value(start_req).map_err(|e| format!("build start request: {e}"))?;
    let resp =
        crate::pkg_install::install_start_handler(State(state.clone()), Json(start_req)).await;
    let (parts, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|e| format!("read start response: {e}"))?;
    if !parts.status.is_success() {
        return Err(format!(
            "serve session failed: {}",
            String::from_utf8_lossy(&bytes)
        ));
    }
    let start: crate::pkg_install::InstallStartResponse =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse start response: {e}"))?;
    Ok((start.session_id, start.url))
}

/// Record history and release the console's active-job guard. Always runs.
fn finalize(
    state: &PkgInstallStateHandle,
    job: &str,
    req: &InstallRequest,
    _started: std::time::Instant,
) {
    if let Some(st) = state.jobs.get(job) {
        crate::log_info!(
            "{}: done phase={:?} verdict={:?} route={:?} code=0x{:08X} served={} MB/s={:.1}",
            correlation_tag(job),
            st.phase,
            st.verdict,
            st.route,
            st.code,
            st.metrics.served_bytes,
            st.metrics.throughput_mbps
        );
        let entry = HistoryEntry {
            job: st.job.clone(),
            at: status::now_unix(),
            source_kind: req.source.kind().to_string(),
            content_id: st.content_id.clone(),
            title_id: st.title_id.clone(),
            route: st.route,
            verdict: st.verdict,
            code: st.code,
            metrics: st.metrics.clone(),
        };
        let _ = history::append(&history_dir(), &req.ps5_addr, &entry, history::HISTORY_CAP);

        // Optional cleanup of an engine-side working copy (e.g. a Convert
        // output) after a successful install. Never touches a file the user
        // placed on the console themselves (console_path / remote / url).
        if req.options.delete_source_copy_after && st.verdict == Some(Verdict::Installed) {
            if let Source::HostFile(path) = &req.source {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    state.jobs.finish(job);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::deliver::{decide_delivery, Delivery, Source};

    // ── console-path installs learn their identity from the package header ──

    fn bare_req(source: Source) -> InstallRequest {
        InstallRequest {
            ps5_addr: "192.168.86.99:9114".into(),
            source,
            content_id: String::new(),
            title_id: None,
            package_app_ver: None,
            category: None,
            options: InstallOptions::default(),
        }
    }

    #[test]
    fn an_upload_queue_patch_gets_its_category_and_version_from_the_header() {
        // Measured on the Phat: the Upload screen sends no category/app_ver, so
        // a Star Wars 1.02 patch installed with no version check at all.
        let mut r = bare_req(Source::ConsolePath(
            "/user/data/ps5upload/pkg_library/updates/x.pkg".into(),
        ));
        fill_from_header(
            &mut r,
            "UP1082-CUSA03474_00-SLUS202680000001",
            "CUSA03474",
            "gp",
            "01.02",
        );
        assert_eq!(r.category.as_deref(), Some("gp"));
        assert_eq!(r.package_app_ver.as_deref(), Some("01.02"));
        assert_eq!(r.title_id.as_deref(), Some("CUSA03474"));
        assert_eq!(r.content_id, "UP1082-CUSA03474_00-SLUS202680000001");
        assert_eq!(
            patch_check_version(r.category.as_deref().unwrap(), r.package_app_ver.as_deref()),
            Some("01.02".to_string())
        );
    }

    #[test]
    fn values_the_caller_sent_always_win_over_the_header() {
        let mut r = bare_req(Source::ConsolePath("/x.pkg".into()));
        r.category = Some("PS4DP".into());
        r.package_app_ver = Some("01.09".into());
        r.title_id = Some("CUSA00900".into());
        r.content_id = "UP9000-CUSA00900_00-BLOODBORNE000000".into();
        fill_from_header(&mut r, "OTHER", "CUSA99999", "gd", "01.00");
        assert_eq!(r.category.as_deref(), Some("PS4DP"));
        assert_eq!(r.package_app_ver.as_deref(), Some("01.09"));
        assert_eq!(r.title_id.as_deref(), Some("CUSA00900"));
        assert_eq!(r.content_id, "UP9000-CUSA00900_00-BLOODBORNE000000");
    }

    #[test]
    fn empty_header_fields_fill_nothing() {
        let mut r = bare_req(Source::ConsolePath("/x.pkg".into()));
        fill_from_header(&mut r, "", "", "", "");
        assert!(r.category.is_none() && r.package_app_ver.is_none() && r.title_id.is_none());
        assert!(r.content_id.is_empty());
    }

    // ── recycle a daemon wedged by a failed network install ──

    #[test]
    fn a_never_fetched_network_failure_recycles_the_daemon() {
        // Measured: after this failure every later URL install failed
        // instantly until the daemon process restarted.
        assert!(should_recycle_daemon(true, true, 0x8043_1064));
        assert!(should_recycle_daemon(true, true, 0x8043_1068));
    }

    #[test]
    fn other_failures_leave_the_daemon_alone() {
        // Not a stream (loopback), bytes already flowed, or not network-class.
        assert!(!should_recycle_daemon(false, true, 0x8043_1064));
        assert!(!should_recycle_daemon(true, false, 0x8043_1064));
        assert!(!should_recycle_daemon(true, true, 0x80B2_116F));
        assert!(!should_recycle_daemon(true, true, 0));
    }

    #[test]
    fn a_stalled_daemon_served_install_recycles_the_daemon() {
        // Measured on FW 5.10: the daemon keeps a stalled loopback job as
        // "serving" until Sony reads every byte, which never happens, so every
        // later install was refused as busy until the daemon restarted.
        assert!(should_recycle_after_stall(true));
        // A stream stall is served by this engine; the daemon holds nothing.
        assert!(!should_recycle_after_stall(false));
    }

    // ── a stream the console never fetched from: say why, not "declined" ──

    #[test]
    fn unreachable_stream_names_the_address_and_the_firewall() {
        // Measured on the Phat: a firewall-blocked engine gave 0x80431064 with
        // 0 bytes served, and the UI said only "The PS5 declined the install."
        let h = stream_unreachable_hint(Some("http://192.168.86.199:19200"), 0x80431064);
        assert!(
            h.contains("never reached this computer at http://192.168.86.199:19200"),
            "{h}"
        );
        assert!(h.contains("0x80431064"), "{h}");
        assert!(h.contains("firewall"), "{h}");
        assert!(h.contains("Upload & install"), "{h}");
    }

    #[test]
    fn a_failed_reach_check_names_the_likely_cause() {
        use ps5upload_core::diagnostics::NetReach;
        // #342: a PS5 on Windows Internet Connection Sharing could never
        // reach the PC; the dropped connection timed out.
        let dropped = NetReach {
            ok: false,
            timed_out: true,
            errno: 0,
            err: "timed out".into(),
            ms: 4000,
        };
        let m = reach_block_message("http://192.168.137.1:19113", &dropped);
        assert!(m.contains("192.168.137.1:19113"), "{m}");
        assert!(m.contains("firewall"), "{m}");
        assert!(m.contains("Public"), "{m}");
        assert!(m.contains("Upload & install"), "{m}");
        let refused = NetReach {
            ok: false,
            timed_out: false,
            errno: 61,
            err: "Connection refused".into(),
            ms: 3,
        };
        assert!(reach_block_message("http://x:1", &refused).contains("refused"));
    }

    #[test]
    fn a_games_own_installed_files_are_never_an_install_source() {
        for p in [
            "/mnt/ext0/user/patch/CUSA02092/patch.pkg",
            "/mnt/ext1/user/app/PPSA01234/app.pkg",
            "/user/app/CUSA00001/app.pkg",
            "/user/addcont/CUSA00001/X/ac.pkg",
        ] {
            assert!(is_installed_content_path(p), "{p}");
        }
        for p in [
            "/user/data/ps5upload/pkg_library/X.pkg",
            "/mnt/usb0/user/patch/X/patch.pkg",
            "/mnt/ext0/games/patch.pkg",
            "/data/pkgs/app.pkg",
        ] {
            assert!(!is_installed_content_path(p), "{p}");
        }
    }

    #[test]
    fn a_bridged_container_is_told_about_host_networking_not_firewalls() {
        // A homelab engine in Docker with default bridge networking hands the
        // PS5 its 172.17.x address; "allow it through the Windows firewall"
        // sent people chasing the wrong thing.
        let h = stream_unreachable_hint_for(Some("http://172.17.0.2:19113"), 0x80431064, true);
        assert!(h.contains("container"), "{h}");
        assert!(h.contains("--network host"), "{h}");
        assert!(h.contains("PS5UPLOAD_PKG_HOST_IP"), "{h}");
        assert!(!h.contains("firewall"), "{h}");
        let plain = stream_unreachable_hint_for(None, 0x80431064, false);
        assert!(plain.contains("firewall"), "{plain}");
    }

    #[test]
    fn a_refusal_names_why_so_the_ui_can_explain_it_in_its_language() {
        assert_eq!(
            refusal_reason(true, 0x80431064, Route::Stream),
            FailReason::StreamUnreachable
        );
        assert_eq!(
            refusal_reason(true, 0x80431084, Route::Stream),
            FailReason::StreamProxy
        );
        // Fetched, or no Sony code: an ordinary refusal.
        assert_eq!(
            refusal_reason(false, 0x80431064, Route::Stream),
            FailReason::SonyRefused
        );
        assert_eq!(
            refusal_reason(true, 0, Route::Stream),
            FailReason::SonyRefused
        );
        // The staged route's measured refusals get their own reason, so the
        // UI can send the user to Stream instead of "the PS5 declined".
        assert_eq!(
            refusal_reason(false, 0x80B2116F, Route::Loopback),
            FailReason::StagedRefused
        );
        assert_eq!(
            refusal_reason(false, 0x80B2150F, Route::Loopback),
            FailReason::StagedRefused
        );
        // Same code on a stream is not the staged limitation.
        assert_eq!(
            refusal_reason(false, 0x80B2116F, Route::Stream),
            FailReason::SonyRefused
        );
    }

    #[test]
    fn a_daemon_that_handed_the_path_to_sony_has_nothing_left_to_serve() {
        // The loopback route was refused and the daemon fell back to a bare
        // path: the job is "accepted" with no bytes to watch. Waiting on it
        // ran the 5-minute stall clock and reported a working install as
        // stalled.
        assert_eq!(
            daemon_phase_progress("accepted"),
            Some(DeliveryProgress::Complete)
        );
        assert_eq!(
            daemon_phase_progress("done"),
            Some(DeliveryProgress::Complete)
        );
        assert_eq!(daemon_phase_progress("serving"), None);
    }

    #[test]
    fn a_proxy_reject_gets_the_proxy_guidance() {
        let h = stream_unreachable_hint(None, 0x80431084);
        assert!(h.contains("proxy"), "{h}");
        assert!(h.contains("Do Not Use"), "{h}");
    }

    // ── the "did the version rise?" check is for patches only ──

    #[test]
    fn version_check_runs_only_for_a_patch_with_a_version() {
        assert_eq!(
            patch_check_version("PS4DP", Some("01.09")),
            Some("01.09".to_string())
        );
        assert_eq!(
            patch_check_version("gp", Some("01.02")),
            Some("01.02".to_string())
        );
        // Base games and DLC are not version bumps of an installed title: a DLC
        // at 01.00 over a base at 01.09 must never read as "regressed".
        assert_eq!(patch_check_version("PS4GD", Some("01.00")), None);
        assert_eq!(patch_check_version("PS4AC", Some("01.00")), None);
        assert_eq!(patch_check_version("PS5GD", Some("01.044.000")), None);
        // A patch with no usable version has nothing to compare.
        assert_eq!(patch_check_version("PS4DP", Some("  ")), None);
        assert_eq!(patch_check_version("PS4DP", None), None);
    }

    // ── delivery wait: "Sony accepted" is not "installed" ──
    // Measured on the Phat (FW 5.10): a stream install of an 820 MB package
    // was reported done at 13 MB served; the console pulled the rest over the
    // next minute. Loopback + Auto-Delete would have deleted the pkg mid-read.

    #[test]
    fn delivery_is_pending_until_every_byte_is_pulled() {
        assert_eq!(
            delivery_progress(13_108_926, 819_791_550, 0, DELIVERY_STALL_MS),
            DeliveryProgress::Pending
        );
    }

    #[test]
    fn delivery_completes_when_served_reaches_total() {
        assert_eq!(
            delivery_progress(819_791_550, 819_791_550, 0, DELIVERY_STALL_MS),
            DeliveryProgress::Complete
        );
        // pkg-host counts a few header re-reads, so served can pass total.
        assert_eq!(
            delivery_progress(820_100_000, 819_791_550, 0, DELIVERY_STALL_MS),
            DeliveryProgress::Complete
        );
    }

    #[test]
    fn delivery_stalls_only_after_the_idle_window() {
        let t = DELIVERY_STALL_MS;
        assert_eq!(
            delivery_progress(1, 10, t - 1, t),
            DeliveryProgress::Pending
        );
        assert_eq!(delivery_progress(1, 10, t, t), DeliveryProgress::Stalled);
    }

    #[test]
    fn unknown_total_cannot_complete_by_bytes() {
        // Without a total there is nothing to compare against; only the stall
        // window can end the wait (and the caller then checks registration).
        assert_eq!(
            delivery_progress(5, 0, 0, DELIVERY_STALL_MS),
            DeliveryProgress::Pending
        );
    }

    #[test]
    fn concurrent_install_for_same_console_is_refused_with_active_job() {
        let store = JobStore::new();
        let id = store.begin("192.168.1.5:9114").expect("first admitted");
        match store.begin("192.168.1.5:9113") {
            // same console, different port
            Err(active) => assert_eq!(active, id),
            Ok(_) => panic!("second concurrent install must be refused"),
        }
        store.finish(&id);
        assert!(
            store.begin("192.168.1.5:9114").is_ok(),
            "after finish, admitted again"
        );
    }

    #[test]
    fn destructive_full_game_reinstall_refused_unless_opted_in() {
        assert!(is_destructive_reinstall("gd", true));
        assert!(!is_destructive_reinstall("gp", true)); // a patch is not a base wipe
        assert!(!is_destructive_reinstall("ac", true)); // DLC is not a base wipe
        assert!(!is_destructive_reinstall("gd", false)); // fresh install is fine
        assert_eq!(guard_decision("gd", true, false), GuardDecision::Refuse);
        assert_eq!(guard_decision("gd", true, true), GuardDecision::Allow);
        assert_eq!(guard_decision("gd", false, false), GuardDecision::Allow);
        assert_eq!(guard_decision("gp", true, false), GuardDecision::Allow);
    }

    #[test]
    fn delivery_matches_source() {
        assert!(matches!(
            decide_delivery(&Source::ConsolePath("/user/data/x.pkg".into())),
            Delivery::Loopback
        ));
        assert!(matches!(
            decide_delivery(&Source::Url("http://h/x.pkg".into())),
            Delivery::Stream
        ));
        assert!(matches!(
            decide_delivery(&Source::HostFile("/tmp/x.pkg".into())),
            Delivery::Stream
        ));
        assert!(matches!(
            decide_delivery(&Source::Remote {
                connection: "c".into(),
                path: "p".into()
            }),
            Delivery::Stream
        ));
    }

    #[test]
    fn ensure_failure_is_a_terminal_failed_status_with_reason() {
        let st = status_from_ensure(&ic::Ensure {
            listening: false,
            sent: false,
            state: None,
            reason: Some("loader_unreachable"),
            error: Some("nothing on :9021".into()),
        });
        assert!(matches!(st.phase, Phase::Failed));
        assert_eq!(st.reason, Some(FailReason::LoaderUnreachable));
        assert_eq!(st.verdict, Some(Verdict::Failed));
    }

    #[test]
    fn accepted_install_maps_to_installed_or_may_not_launch_from_verify_not_socket() {
        use ps5upload_core::patch_verify::PatchVerdict as PV;
        assert_eq!(verdict_from_verify(PV::Applied, true), Verdict::Installed);
        assert_eq!(
            verdict_from_verify(PV::Applied, false),
            Verdict::MayNotLaunch
        );
        assert_eq!(verdict_from_verify(PV::DidNotApply, true), Verdict::Failed);
        assert_eq!(verdict_no_identity(true), Verdict::Installed);
        assert_eq!(verdict_no_identity(false), Verdict::Failed);
    }

    #[test]
    fn correlation_tag_is_install_bracket_short_job() {
        assert_eq!(correlation_tag("1758-42"), "install[1758-42]");
        assert_eq!(correlation_tag("1790000000-3"), "install[17900000]"); // first 8
    }

    #[test]
    fn status_serializes_the_unified_shape() {
        let s = InstallStatus::new("job1", "1.2.3.4:9114", "CID");
        let j = serde_json::to_value(&s).unwrap();
        assert_eq!(j["phase"], "resolve");
        assert!(j.get("route").is_some() && j["route"].is_null());
        assert!(j.get("metrics").is_some());
    }
}
