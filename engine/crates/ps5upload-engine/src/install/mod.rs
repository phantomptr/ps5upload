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
async fn run_install(state: PkgInstallStateHandle, job: String, req: InstallRequest) {
    let started = std::time::Instant::now();
    let tag = correlation_tag(&job);
    let ip = console_id(&req.ps5_addr);
    crate::log_info!(
        "{tag}: install start ps5={ip} source={} content_id={}",
        req.source.kind(),
        req.content_id
    );
    let mgmt = crate::pkg_install::normalize_mgmt_addr(&req.ps5_addr);
    let category = req.category.clone().unwrap_or_default();
    let title_id = req.title_id.clone().filter(|t| !t.trim().is_empty());
    let route = match decide_delivery(&req.source) {
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
    let app_ver_before = match (&title_id, &req.package_app_ver) {
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

    // ensure the daemon (restore the helper afterwards if it was displaced).
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
    let displaced = ens.sent;

    // build the install call per source.
    let hint_name = name_hint(title_id.as_deref());
    state.jobs.update(&job, |s| s.phase = Phase::Install);
    let (reply, session_id, shortened): (Result<ic::InstallReply, String>, Option<String>, bool) =
        match &req.source {
            Source::ConsolePath(path) => {
                let (i, p, h) = (ip.clone(), path.clone(), hint_name.clone());
                let r = tokio::task::spawn_blocking(move || ic::install_path(&i, &p, &h))
                    .await
                    .unwrap_or_else(|e| Err(format!("install task failed: {e}")));
                (r, None, false)
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
                (r, None, was_short)
            }
            Source::HostFile(_) | Source::Remote { .. } => {
                match create_serve_session(&state, &req).await {
                    Ok((sid, url)) => {
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

    // restore the main payload if the ensure displaced it.
    if displaced {
        let ip_r = ip.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(bytes) =
                crate::bundled_payload::image_bytes(crate::bundled_payload::Image::Payload)
            {
                let _ = ps5upload_core::payload_lifecycle::send_elf_to_loader(
                    &ip_r,
                    ps5upload_core::payload_lifecycle::PS5_LOADER_PORT,
                    &bytes,
                    ps5upload_core::payload_lifecycle::LoaderImage::Ps5Upload,
                );
            }
        })
        .await;
    }

    if !accepted {
        // Sony (or the daemon) refused the start, so the console never pulled
        // from the serving session: release it, or a retry of the same package
        // is refused as "already running".
        if let Some(sid) = &session_id {
            crate::pkg_install::release_serve_session(&state.sessions, sid);
        }
        state.jobs.update(&job, |s| {
            s.phase = Phase::Failed;
            s.verdict = Some(Verdict::Failed);
            s.reason = Some(FailReason::SonyRefused);
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
            let p = if dphase.as_deref() == Some("done") {
                DeliveryProgress::Complete
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
    let (verdict, patch_verdict, app_ver_after) = match (&title_id, &req.package_app_ver) {
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
    let start_req = serde_json::json!({
        "ps5_addr": req.ps5_addr,
        "serve_only": true,
        "path": path,
        "content_id": req.content_id,
    });
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
