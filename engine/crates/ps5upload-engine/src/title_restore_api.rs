//! Restore to home screen (#961): a game installed from a package whose home-screen record is
//! gone (Settings › Rebuild Database drops fake packages; the files stay on the drive) is put
//! back by installing the package that is already on the console again, through the console's
//! own installer, without a download.
//!
//! The steps (each helper step is `app.restore`, `payload/src/title_restore.c`):
//!   1. check: read-only, the helper says whether the title is restorable and why not.
//!   2. this console's install slot is reserved (one install per console), then `prepare`: the
//!      helper backs up app.db and appinfo.db and moves the package (and its update) aside on the
//!      same drive.
//!   3. the copy aside is installed like any package on the console (`install::run_reserved`).
//!   4. `finish`: after a verdict of installed, the copy aside is deleted once app.db has the title
//!      and a package is in place again (the database can lag: it is asked again for a while);
//!      after a failure the console never took (refused, unreachable, bad request), the package
//!      and update go back where they were.
//!   5. `clear` (#961): an install the console took and then failed (stalled, stopped, nothing on
//!      the home screen after the wait, or a refusal that left the console's own copy behind)
//!      stays in the PS5's download queue and blocks every later install until it is deleted. The
//!      helper removes the console's unfinished copy with Sony's uninstall, the same as deleting
//!      it on the PS5, and then puts the package back. Only a lost installer reply leaves
//!      everything as it is (pending: the game page offers Finish or Put back later); Restore all
//!      stops there, since every later title would wait for the same installer.
//!
//! Nothing here writes app.db or appinfo.db; the console's installer does, as for any install.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use ps5upload_core::title_restore::{self as tr, RestoreReply};
use serde::{Deserialize, Serialize};

use crate::install::deliver::Source;
use crate::install::status::{FailReason, InstallStatus, Phase, Verdict};
use crate::install::{self, InstallOptions, InstallRequest};
use crate::pkg_install::PkgInstallStateHandle;

/// The most titles one request restores (each is one install, one after the other).
const MAX_TITLES: usize = 50;
/// How long a finished install waits for app.db to list the title before it is called failed and
/// cleared (the database lags the files, both ways; about a minute was measured).
const SETTLE_TRIES: u32 = 60;
const SETTLE_EVERY: Duration = Duration::from_secs(5);
/// How long a clear waits for Sony to finish removing its copy before the restore is left pending.
const CLEAR_TRIES: u32 = 24;

/// What to do once the install of the copy aside has ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterInstall {
    /// Installed: confirm with the helper and delete the copy aside.
    Finish,
    /// The console never took it: put the package back.
    Rollback,
    /// The console took it and it did not finish (stalled, stopped, nothing installed after the
    /// whole package was read): its unfinished copy, and the failed download in the PS5's queue
    /// that blocks every later install, go with Sony's uninstall, then the package goes back.
    Clear,
    /// Nobody can tell what the console is doing (the installer's answer never came): touch
    /// nothing.
    Leave,
}

/// The decision, from the install job's last status (None: the job is unknown).
pub fn after_install(st: Option<&InstallStatus>) -> AfterInstall {
    let Some(st) = st else {
        return AfterInstall::Leave;
    };
    match (st.phase, st.verdict) {
        (_, Some(Verdict::Installed)) | (_, Some(Verdict::MayNotLaunch)) => AfterInstall::Finish,
        // The console had the whole package and nothing of the game showed up in the wait.
        (_, Some(Verdict::Unverified)) => AfterInstall::Clear,
        (Phase::Failed, _) | (_, Some(Verdict::Failed)) => match st.reason {
            // Sony took it and then stopped, or the user stopped it: the uninstall ends whatever
            // Sony still has of it before the package moves back.
            Some(FailReason::Stalled) | Some(FailReason::Cancelled) => AfterInstall::Clear,
            Some(FailReason::ReplyLost) => AfterInstall::Leave,
            _ => AfterInstall::Rollback,
        },
        // Not ended.
        _ => AfterInstall::Leave,
    }
}

/// A rollback the console's own leftovers stood in the way of: a record of the title or a package
/// in its place, both written by the failed install (before the restore the title had neither).
pub fn rollback_needs_clear(r: &RestoreReply) -> bool {
    r.state == "unsettled"
        && matches!(
            r.reason.as_deref(),
            Some("recorded_without_package" | "on_home_screen" | "place_taken")
        )
}

/// The installer itself has stopped answering (a lost reply, or the daemon could not be reached):
/// every later install would wait for it too. A restart of the PS5 clears it.
pub fn installer_stuck(st: Option<&InstallStatus>) -> bool {
    st.is_some_and(|s| {
        matches!(
            s.reason,
            Some(FailReason::ReplyLost) | Some(FailReason::LoaderUnreachable)
        )
    })
}

/// What the user reads when the installer stopped answering.
pub const INSTALLER_STUCK: &str = "the PS5's installer stopped answering, so nothing more was tried; restart the PS5, then open each game's page to finish or put back its package";

/// One line for the log: how the install of a restore ended, with Sony's code and the phase, so a
/// bug report shows why a restore failed (#961: several stuck at 99.9%).
pub fn install_summary(st: Option<&InstallStatus>) -> String {
    let Some(s) = st else {
        return "install job unknown".to_string();
    };
    format!(
        "install phase={:?} verdict={:?} reason={:?} route={:?} sony_code=0x{:08X} served={}/{} bytes{}",
        s.phase,
        s.verdict,
        s.reason,
        s.route,
        s.code.max(s.metrics.sony_rc),
        s.metrics.served_bytes,
        s.metrics.total_bytes,
        s.hint
            .as_deref()
            .map(|h| format!(" hint={h:?}"))
            .unwrap_or_default()
    )
}

/// One title's restore as this engine ran it.
#[derive(Debug, Clone, Serialize)]
pub struct RestoreRun {
    pub title_id: String,
    pub addr: String,
    /// queued | installing | finishing | restored | rolled_back | pending | failed
    pub phase: String,
    /// The install job (`/api/pkg/install/status?job=`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepared: Option<RestoreReply>,
    /// The helper's last finish answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<RestoreReply>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_verdict: Option<Verdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The console's unfinished copy of a failed install was removed (Sony's uninstall) before
    /// the package went back.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cleared: bool,
    pub updated_at: u64,
}

impl RestoreRun {
    fn new(addr: &str, title_id: &str, phase: &str) -> Self {
        RestoreRun {
            title_id: title_id.to_string(),
            addr: addr.to_string(),
            phase: phase.to_string(),
            job: None,
            prepared: None,
            result: None,
            install_verdict: None,
            install_hint: None,
            error: None,
            cleared: false,
            updated_at: install::status::now_unix(),
        }
    }

    fn active(&self) -> bool {
        matches!(self.phase.as_str(), "queued" | "installing" | "finishing")
    }
}

static RUNS: Mutex<Option<HashMap<String, RestoreRun>>> = Mutex::new(None);

fn key(addr: &str, title_id: &str) -> String {
    format!("{}/{}", install::console_id(addr), title_id)
}

fn with_runs<T>(f: impl FnOnce(&mut HashMap<String, RestoreRun>) -> T) -> T {
    let mut g = RUNS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

fn set_run(run: RestoreRun) {
    let mut run = run;
    run.updated_at = install::status::now_unix();
    with_runs(|m| m.insert(key(&run.addr, &run.title_id), run));
}

fn update_run(addr: &str, title_id: &str, f: impl FnOnce(&mut RestoreRun)) {
    with_runs(|m| {
        if let Some(r) = m.get_mut(&key(addr, title_id)) {
            f(r);
            r.updated_at = install::status::now_unix();
        }
    });
}

fn get_run(addr: &str, title_id: &str) -> Option<RestoreRun> {
    with_runs(|m| m.get(&key(addr, title_id)).cloned())
}

/// Ends a run that is still active (a batch that stopped early, a step that failed without saying
/// so): it must never stay queued or installing, which would refuse every later restore on that
/// console until the engine restarts.
fn end_if_active(addr: &str, title_id: &str, why: &str) {
    update_run(addr, title_id, |r| {
        if r.active() {
            r.phase = "failed".into();
            r.error.get_or_insert_with(|| why.to_string());
        }
    });
}

/// Ends every run of a batch that is still active when the batch's task ends, however it ends
/// (a panic included).
struct BatchGuard {
    addr: String,
    titles: Vec<String>,
}

impl Drop for BatchGuard {
    fn drop(&mut self) {
        for t in &self.titles {
            end_if_active(&self.addr, t, "the restore stopped before this game");
        }
    }
}

/// A restore is active on the console (what [`try_reserve`] refuses).
#[cfg(test)]
fn console_busy(addr: &str) -> bool {
    let cid = install::console_id(addr);
    with_runs(|m| {
        m.values()
            .any(|r| r.active() && install::console_id(&r.addr) == cid)
    })
}

/// Marks `title_id` on `addr` as `phase` (an active one) unless a restore is active on that
/// console: the look and the mark under one lock, so two requests at once can't both start (one
/// would otherwise overwrite the other's run, or run Sony's uninstall beside its install). The
/// run's phase before (None: there was none); Err while the console is busy.
fn try_reserve(addr: &str, title_id: &str, phase: &str) -> Result<Option<String>, ()> {
    let cid = install::console_id(addr);
    with_runs(|m| {
        if m.values()
            .any(|r| r.active() && install::console_id(&r.addr) == cid)
        {
            return Err(());
        }
        let k = key(addr, title_id);
        let prev = m.get(&k).map(|r| r.phase.clone());
        let run = m
            .entry(k)
            .or_insert_with(|| RestoreRun::new(addr, title_id, phase));
        run.phase = phase.to_string();
        run.updated_at = install::status::now_unix();
        Ok(prev)
    })
}

/// A finish's mark ([`try_reserve`]): taken back when the finish ends without setting a phase of
/// its own (an error, nothing pending), so the console is never left looking busy.
struct Reserved {
    addr: String,
    title_id: String,
    prev: Option<String>,
}

impl Drop for Reserved {
    fn drop(&mut self) {
        let k = key(&self.addr, &self.title_id);
        with_runs(|m| match (&self.prev, m.get_mut(&k)) {
            (_, Some(r)) if r.phase != "finishing" => {}
            (Some(prev), Some(r)) => r.phase = prev.clone(),
            (None, Some(_)) => {
                m.remove(&k);
            }
            (_, None) => {}
        });
    }
}

/// The folders a restore step moved, installed or deleted in (the title's folder, the package's
/// place, the copy aside): their known sizes in Files are no longer right (#974).
fn sizes_changed(addr: &str, id: &str, r: &RestoreReply) {
    let mut paths = vec![format!("/user/app/{id}")];
    paths.extend(
        [&r.pkg, &r.aside, &r.install_path, &r.patch, &r.kept]
            .into_iter()
            .flatten()
            .cloned(),
    );
    crate::folder_sizes::changed(addr, &paths);
}

fn fail(status: StatusCode, error: impl Into<String>) -> Response {
    (
        status,
        Json(serde_json::json!({"ok": false, "error": error.into()})),
    )
        .into_response()
}

/// The HTTP status for a helper call that failed.
fn helper_failure(e: &anyhow::Error) -> Response {
    let text = format!("{e:#}");
    let status = if text.starts_with("restore_invalid_title") {
        StatusCode::BAD_REQUEST
    } else if text.starts_with(tr::UNSUPPORTED) {
        StatusCode::CONFLICT
    } else {
        StatusCode::BAD_GATEWAY
    };
    fail(status, text)
}

fn normalized_id(title_id: &str) -> Option<String> {
    let id = title_id.trim().to_ascii_uppercase();
    tr::valid_title_id(&id).then_some(id)
}

#[derive(Debug, Deserialize)]
pub struct RestoreQuery {
    #[serde(alias = "console", alias = "host", alias = "ps5_addr")]
    pub addr: String,
    pub title_id: String,
}

/// `GET /api/ps5/app/restore/check?addr=&title_id=`: whether a game installed from a package can
/// be put back on the home screen, read-only. The helper's answer: `state` is `restorable`, or
/// why not (`registered`: it is on the home screen; `record_elsewhere`: the console still has a
/// record of it; `not_installed`, `pending` (an earlier restore did not finish),
/// `folder_registration`, `ambiguous`, `retail`, `no_room`, `aside_cross_device`, `db_unreadable`,
/// `invalid_title`, `system_title`), with `pkg`, `size`, `where`, `kind`, `patch` and the free
/// space. A helper without the method answers 409 `restore_unsupported`.
pub async fn check_handler(Query(q): Query<RestoreQuery>) -> Response {
    let Some(id) = normalized_id(&q.title_id) else {
        return fail(StatusCode::BAD_REQUEST, "title_id is not a title id");
    };
    let mgmt = crate::pkg_install::normalize_mgmt_addr(&q.addr);
    match tokio::task::spawn_blocking(move || tr::check(&mgmt, &id)).await {
        Ok(Ok(r)) => Json(r).into_response(),
        Ok(Err(e)) => helper_failure(&e),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Debug, Deserialize)]
pub struct CandidatesQuery {
    #[serde(alias = "console", alias = "host", alias = "ps5_addr")]
    pub addr: String,
}

/// One game the console has a package of but no home-screen record for.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub title_id: String,
    pub title_name: String,
    pub check: RestoreReply,
}

/// `GET /api/ps5/app/restore/candidates?addr=`: every title with an app folder on the internal
/// drive, USB extended storage or M.2 that app.db does not list, with the helper's check of each
/// (`check.state` `restorable` can be put back; `pending` has a restore to finish). Read-only.
/// app.db must answer in full: a partial list could make a listed game look missing.
pub async fn candidates_handler(Query(q): Query<CandidatesQuery>) -> Response {
    let mgmt = crate::pkg_install::normalize_mgmt_addr(&q.addr);
    let r = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Candidate>> {
        let db = ps5upload_core::diagnostics::appdb_query(&mgmt)?;
        if db.truncated || db.err.is_some() {
            anyhow::bail!(
                "app.db could not be read in full{}",
                db.err.map(|e| format!(": {e}")).unwrap_or_default()
            );
        }
        let known: std::collections::HashSet<String> =
            db.apps.iter().map(|a| a.title_id.clone()).collect();
        let mut ids: Vec<String> = Vec::new();
        for root in crate::APP_FOLDER_ROOTS {
            // A drive that is not there lists nothing.
            if let Ok((entries, _)) = crate::list_dir_whole(&mgmt, root) {
                ids.extend(
                    entries
                        .into_iter()
                        .map(|e| e.name)
                        .filter(|n| crate::looks_like_title_id(n) && !n.starts_with("NPXS")),
                );
            }
        }
        ids.sort();
        ids.dedup();
        ids.retain(|id| !known.contains(id) && !crate::is_console_webui_tile(id));
        let mut out = Vec::new();
        let mut first_error: Option<anyhow::Error> = None;
        for id in ids.into_iter().take(MAX_TITLES * 2) {
            // One title the helper can't check is left out (and logged), not the whole list;
            // a helper without the method fails them all.
            let check = match tr::check(&mgmt, &id) {
                Ok(c) => c,
                Err(e) if format!("{e:#}").starts_with(tr::UNSUPPORTED) => return Err(e),
                Err(e) => {
                    crate::log_at!(
                        Warn,
                        "install",
                        "restore candidates on {mgmt}: {id} not checked: {e:#}"
                    );
                    first_error.get_or_insert(e);
                    continue;
                }
            };
            if matches!(check.state.as_str(), "not_installed" | "registered") {
                continue;
            }
            let title_name = crate::title_name(&mgmt, &id).unwrap_or_else(|| id.clone());
            out.push(Candidate {
                title_id: id,
                title_name,
                check,
            });
        }
        // Every check failed: that is the console's problem, not an empty list.
        match first_error {
            Some(e) if out.is_empty() => Err(e),
            _ => Ok(out),
        }
    })
    .await;
    match r {
        Ok(Ok(list)) => Json(serde_json::json!({"ok": true, "titles": list})).into_response(),
        Ok(Err(e)) => helper_failure(&e),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Debug, Deserialize)]
pub struct RestoreStart {
    #[serde(alias = "console", alias = "host", alias = "ps5_addr")]
    pub addr: String,
    /// One title.
    #[serde(default)]
    pub title_id: Option<String>,
    /// Several, restored one after the other ("Restore all").
    #[serde(default)]
    pub title_ids: Option<Vec<String>>,
}

/// The titles a start names, normalized and without repeats, or why not.
pub fn start_titles(req: &RestoreStart) -> Result<Vec<String>, String> {
    let mut raw: Vec<String> = req.title_ids.clone().unwrap_or_default();
    if let Some(t) = &req.title_id {
        raw.insert(0, t.clone());
    }
    if raw.is_empty() {
        return Err("title_id or title_ids is required".into());
    }
    let mut out: Vec<String> = Vec::new();
    for t in raw {
        let Some(id) = normalized_id(&t) else {
            return Err(format!("{t:?} is not a title id"));
        };
        if !out.contains(&id) {
            out.push(id);
        }
    }
    if out.len() > MAX_TITLES {
        return Err(format!("at most {MAX_TITLES} titles at a time"));
    }
    Ok(out)
}

/// `POST /api/ps5/app/restore {addr, title_id | title_ids}`: put games installed from a package
/// back on the home screen by installing the package already on the console again (no
/// download). The first title is checked and prepared before this answers: a refusal is 409 with
/// the helper's `restore` answer and nothing changed; another install on the console is 409
/// `busy`. Then `{ok, job, prepared, queued}`: `job` is the first title's install
/// (`/api/pkg/install/status`); the others follow one after the other. Each title's progress:
/// `GET /api/ps5/app/restore/status`.
pub async fn start_handler(
    State(state): State<PkgInstallStateHandle>,
    Json(req): Json<RestoreStart>,
) -> Response {
    if install::console_id(&req.addr).is_empty() {
        return fail(StatusCode::BAD_REQUEST, "addr is required");
    }
    let titles = match start_titles(&req) {
        Ok(t) => t,
        Err(e) => return fail(StatusCode::BAD_REQUEST, e),
    };
    {
        let ip = install::console_id(&req.addr);
        let away =
            tokio::task::spawn_blocking(move || ps5upload_core::route::via(&ip).is_tailnet())
                .await
                .unwrap_or(false);
        if away {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "ok": false,
                    "reason": "tailnet_home_only",
                    "error": ps5upload_core::route::home_only("Restoring a game"),
                })),
            )
                .into_response();
        }
    }
    let addr = req.addr.clone();
    let first = titles[0].clone();
    // The first title marks the console busy before anything awaits; begin_one replaces the
    // mark with what became of it.
    if try_reserve(&addr, &first, "queued").is_err() {
        return fail(
            StatusCode::CONFLICT,
            "busy: a restore is already running on this console",
        );
    }
    let (job, prepared) = match begin_one(&state, &addr, &first).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    for t in titles.iter().skip(1) {
        set_run(RestoreRun::new(&addr, t, "queued"));
    }
    let queued: Vec<String> = titles.iter().skip(1).cloned().collect();
    let st = state.clone();
    let (job2, prepared2, rest) = (job.clone(), prepared.clone(), queued.clone());
    let guard = BatchGuard {
        addr: addr.clone(),
        titles: titles.clone(),
    };
    tokio::spawn(async move {
        let _guard = guard;
        let mut stuck = run_one(st.clone(), &addr, &first, job2, prepared2).await;
        for t in rest {
            if stuck {
                // Every later install would wait for the same installer: say so, try nothing.
                end_if_active(&addr, &t, INSTALLER_STUCK);
                continue;
            }
            // A refusal or a failed restore is recorded for its title; the next title still goes.
            match begin_one(&st, &addr, &t).await {
                Ok((job, prepared)) => stuck = run_one(st.clone(), &addr, &t, job, prepared).await,
                Err(_) => end_if_active(&addr, &t, "the restore could not start"),
            }
        }
    });
    Json(serde_json::json!({"ok": true, "job": job, "prepared": prepared, "queued": queued}))
        .into_response()
}

/// Check, reserve the console's install slot, prepare. On a refusal the run records why and the
/// response to send is returned.
#[allow(clippy::result_large_err)]
async fn begin_one(
    state: &PkgInstallStateHandle,
    addr: &str,
    id: &str,
) -> Result<(String, RestoreReply), Response> {
    let mgmt = crate::pkg_install::normalize_mgmt_addr(addr);
    let refused = |run_phase: &str, error: String, reply: Option<RestoreReply>| {
        let mut run = RestoreRun::new(addr, id, run_phase);
        run.error = Some(error.clone());
        run.result = reply.clone();
        set_run(run);
        (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"ok": false, "error": error, "restore": reply})),
        )
            .into_response()
    };
    // Read-only first, so a refusal never takes the install slot.
    let (m, i) = (mgmt.clone(), id.to_string());
    let check = match tokio::task::spawn_blocking(move || tr::check(&m, &i)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            set_run({
                let mut r = RestoreRun::new(addr, id, "failed");
                r.error = Some(format!("{e:#}"));
                r
            });
            return Err(helper_failure(&e));
        }
        Err(e) => {
            end_if_active(addr, id, "the restore could not start");
            return Err(fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
        }
    };
    crate::log_at!(
        Info,
        "install",
        "restore {id} on {addr}: check {} ({}): {} bytes on {}; free there {} (needs {}), on the console's storage {} (needs {})",
        check.state,
        check.reason.as_deref().unwrap_or("-"),
        check.size.unwrap_or(0),
        check.where_.as_deref().unwrap_or("?"),
        check.free_volume.map_or("?".into(), |n| n.to_string()),
        check.need_volume.map_or("?".into(), |n| n.to_string()),
        check.free_internal.map_or("?".into(), |n| n.to_string()),
        check.need_internal.map_or("?".into(), |n| n.to_string()),
    );
    if !check.restorable() {
        let state_word = check.state.clone();
        return Err(refused(
            "failed",
            format!("restore_{state_word}"),
            Some(check),
        ));
    }
    let job = match state.jobs.begin(addr) {
        Ok(j) => j,
        Err(active) => {
            set_run({
                let mut r = RestoreRun::new(addr, id, "failed");
                r.error = Some(format!("busy: install {active} is running on this console"));
                r
            });
            return Err((
                StatusCode::CONFLICT,
                Json(serde_json::json!({"ok": false, "error": "busy", "job": active})),
            )
                .into_response());
        }
    };
    let (m, i) = (mgmt.clone(), id.to_string());
    let prepared = match tokio::task::spawn_blocking(move || tr::prepare(&m, &i)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            // The helper may or may not have moved it: the next check says (`pending`).
            state
                .jobs
                .release_unstarted(&job, "the restore could not be prepared");
            set_run({
                let mut r = RestoreRun::new(addr, id, "failed");
                r.error = Some(format!("{e:#}"));
                r
            });
            return Err(helper_failure(&e));
        }
        Err(e) => {
            state
                .jobs
                .release_unstarted(&job, "the restore could not be prepared");
            end_if_active(addr, id, "the restore could not be prepared");
            return Err(fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
        }
    };
    let Some(path) = prepared.install_path.clone().filter(|_| prepared.ok) else {
        state
            .jobs
            .release_unstarted(&job, "the console refused to prepare the restore");
        let err = prepared
            .err
            .clone()
            .unwrap_or_else(|| "restore_not_prepared".into());
        return Err(refused("failed", err, Some(prepared)));
    };
    crate::log_at!(
        Info,
        "install",
        "restore {id} on {addr}: {} moved aside to {path}; installing it (job {job})",
        prepared.pkg.as_deref().unwrap_or("?")
    );
    {
        // What the console's installer will find at the end of the package: a debug package's
        // install information (a ZIP with playgo-chunk.crc) that the console may ask for beside
        // the package. The installer daemon serves a .crc only as a file next to the package,
        // so a report that shows the console asking for one at 99.9% names that as the cause.
        let (m, p, size) = (mgmt.clone(), path.clone(), prepared.size.unwrap_or(0));
        let crc = tokio::task::spawn_blocking(move || {
            crate::pkg_sidecar::locate_playgo_crc(size, |off, n| console_read(&m, &p, off, n))
        })
        .await;
        crate::log_at!(
            Info,
            "install",
            "restore {id} on {addr}: package {} bytes, kind {}; install information (playgo-chunk.crc) {}",
            prepared.size.unwrap_or(0),
            prepared.kind.as_deref().unwrap_or("?"),
            match crc {
                Ok(Ok(Some((off, len)))) => format!("at {off}, {len} bytes"),
                Ok(Ok(None)) => "not in the package".to_string(),
                Ok(Err(e)) => format!("not read: {e}"),
                Err(e) => format!("not read: {e}"),
            }
        );
    }
    sizes_changed(addr, id, &prepared);
    let mut run = RestoreRun::new(addr, id, "installing");
    run.job = Some(job.clone());
    run.prepared = Some(prepared.clone());
    set_run(run);
    Ok((job, prepared))
}

/// `n` bytes of a console file at `off`, in the helper's read-sized pieces.
fn console_read(addr: &str, path: &str, off: u64, n: u64) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(n.min(4 << 20) as usize);
    while (out.len() as u64) < n {
        let at = off + out.len() as u64;
        let piece = ps5upload_core::fs_ops::fs_read(addr, path, at, n - out.len() as u64)
            .map_err(|e| std::io::Error::other(format!("{e:#}")))?;
        if piece.is_empty() {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        out.extend_from_slice(&piece);
    }
    out.truncate(n as usize);
    Ok(out)
}

/// One helper step on the blocking pool.
async fn helper_step(
    mgmt: &str,
    id: &str,
    f: fn(&str, &str) -> anyhow::Result<RestoreReply>,
) -> anyhow::Result<RestoreReply> {
    let (m, i) = (mgmt.to_string(), id.to_string());
    tokio::task::spawn_blocking(move || f(&m, &i))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
}

fn finish_rollback(m: &str, i: &str) -> anyhow::Result<RestoreReply> {
    tr::finish(m, i, true, true)
}

fn finish_ended(m: &str, i: &str) -> anyhow::Result<RestoreReply> {
    tr::finish(m, i, false, true)
}

fn finish_look(m: &str, i: &str) -> anyhow::Result<RestoreReply> {
    tr::finish(m, i, false, false)
}

/// Clear the console's unfinished copy and put the package back, waiting a while for Sony to
/// finish removing its copy. The last answer (`rolled_back`, or why not).
async fn clear_and_put_back(mgmt: &str, id: &str, tries: u32) -> anyhow::Result<RestoreReply> {
    let mut result = helper_step(mgmt, id, tr::clear).await;
    for _ in 0..tries {
        if !matches!(&result, Ok(r) if r.clearing()) {
            break;
        }
        tokio::time::sleep(SETTLE_EVERY).await;
        let cleared = matches!(&result, Ok(r) if r.cleared == Some(true));
        result = helper_step(mgmt, id, finish_rollback).await;
        if cleared {
            if let Ok(r) = &mut result {
                r.cleared.get_or_insert(true);
            }
        }
    }
    result
}

/// Install the copy aside in the reserved slot, then finish by the install's verdict. True when
/// the installer stopped answering (a batch stops there).
async fn run_one(
    state: PkgInstallStateHandle,
    addr: &str,
    id: &str,
    job: String,
    prepared: RestoreReply,
) -> bool {
    let Some(path) = prepared.install_path.clone() else {
        end_if_active(
            addr,
            id,
            "the console did not say where it moved the package",
        );
        state
            .jobs
            .release_unstarted(&job, "the restore could not be prepared");
        return false;
    };
    let req = InstallRequest {
        ps5_addr: addr.to_string(),
        source: Source::ConsolePath(path),
        content_id: String::new(),
        title_id: Some(id.to_string()),
        package_app_ver: None,
        category: None,
        options: InstallOptions::default(),
    };
    install::run_reserved(state.clone(), job.clone(), req).await;
    let st = state.jobs.get(&job);
    let mut decision = after_install(st.as_ref());
    let stuck = installer_stuck(st.as_ref());
    let summary = install_summary(st.as_ref());
    if decision == AfterInstall::Finish {
        crate::log_at!(Info, "install", "restore {id} on {addr}: {summary}");
    } else {
        // WARN, so a bug report carries why (#961).
        crate::log_at!(Warn, "install", "restore {id} on {addr}: {summary}");
    }
    update_run(addr, id, |r| {
        r.phase = "finishing".into();
        r.install_verdict = st.as_ref().and_then(|s| s.verdict);
        r.install_hint = if stuck {
            Some(INSTALLER_STUCK.to_string())
        } else {
            st.as_ref().and_then(|s| s.hint.clone())
        };
    });
    let mgmt = crate::pkg_install::normalize_mgmt_addr(addr);
    let mut result: anyhow::Result<RestoreReply> = Err(anyhow::anyhow!("not asked"));
    match decision {
        AfterInstall::Finish => {
            for _ in 0..SETTLE_TRIES {
                result = helper_step(&mgmt, id, finish_ended).await;
                let waiting = matches!(&result, Ok(r) if r.state == "waiting");
                if !waiting {
                    break;
                }
                tokio::time::sleep(SETTLE_EVERY).await;
            }
            if matches!(&result, Ok(r) if r.state == "waiting") {
                // Read in full, and still not on the home screen: the install failed at the end
                // (the PS5 shows it stopped near 100%), and its download stays in the PS5's queue.
                crate::log_at!(
                    Warn,
                    "install",
                    "restore {id} on {addr}: the console installed the package but never listed the game in {} s; clearing its copy and putting the package back",
                    SETTLE_TRIES as u64 * SETTLE_EVERY.as_secs()
                );
                decision = AfterInstall::Clear;
                result = clear_and_put_back(&mgmt, id, CLEAR_TRIES).await;
            }
        }
        AfterInstall::Rollback => {
            result = helper_step(&mgmt, id, finish_rollback).await;
            if matches!(&result, Ok(r) if rollback_needs_clear(r)) {
                decision = AfterInstall::Clear;
                result = clear_and_put_back(&mgmt, id, CLEAR_TRIES).await;
            }
        }
        AfterInstall::Clear => {
            result = clear_and_put_back(&mgmt, id, CLEAR_TRIES).await;
        }
        AfterInstall::Leave => {
            // Only reads where it stands: with neither flag the helper touches nothing.
            result = helper_step(&mgmt, id, finish_look).await;
        }
    }
    if decision == AfterInstall::Clear
        && matches!(
            st.as_ref().and_then(|s| s.reason),
            Some(FailReason::Stalled)
        )
        && !state.jobs.active_for(addr)
    {
        // A stalled job can keep the installer daemon serving the package for minutes: the next
        // restore would be refused as busy. Nothing else installs on the console right now.
        let ip = install::console_id(addr);
        install::recycle_daemon(&ip).await;
    }
    let phase = match &result {
        Ok(r) if r.state == "restored" => "restored",
        Ok(r) if r.state == "rolled_back" => "rolled_back",
        _ => "pending",
    };
    let cleared = matches!(&result, Ok(r) if r.cleared == Some(true));
    sizes_changed(addr, id, &prepared);
    if let Ok(r) = &result {
        sizes_changed(addr, id, r);
    }
    let log_line = format!(
        "restore {id} on {addr}: {decision:?}: {phase}{} ({})",
        if cleared {
            ", the console's unfinished copy removed"
        } else {
            ""
        },
        match &result {
            Ok(r) => format!(
                "{} {}{}",
                r.state,
                r.reason.as_deref().unwrap_or(""),
                r.sony_rc
                    .map(|c| format!(" uninstall rc=0x{c:08X}"))
                    .unwrap_or_default()
            ),
            Err(e) => format!("{e:#}"),
        }
    );
    if phase == "restored" {
        crate::log_at!(Info, "install", "{log_line}");
    } else {
        crate::log_at!(Warn, "install", "{log_line}");
    }
    update_run(addr, id, |r| {
        r.phase = phase.into();
        r.cleared = cleared;
        match result {
            Ok(reply) => r.result = Some(reply),
            Err(e) => r.error = Some(format!("{e:#}")),
        }
    });
    stuck
}

/// `GET /api/ps5/app/restore/status?addr=&title_id=`: how this engine's restore of a title stands.
/// `phase`: `queued`, `installing` (with `job`, the install's id for `/api/pkg/install/status`;
/// `install` is that status), `finishing`, `restored`, `rolled_back` (the install did not take, the
/// package is back where it was), `pending` (the console may still be working on it, or the
/// database did not list the game in time: nothing was deleted; finish it later), `failed`
/// (refused before anything moved: `error`, and the helper's `result`). 404 when this engine ran
/// none (after a restart: the check's `pending` state says an unfinished one is on the console).
pub async fn status_handler(
    State(state): State<PkgInstallStateHandle>,
    Query(q): Query<RestoreQuery>,
) -> Response {
    let Some(id) = normalized_id(&q.title_id) else {
        return fail(StatusCode::BAD_REQUEST, "title_id is not a title id");
    };
    match get_run(&q.addr, &id) {
        Some(run) => {
            let install = run.job.as_deref().and_then(|j| state.jobs.get(j));
            let mut v = serde_json::to_value(&run).unwrap_or_default();
            v["ok"] = serde_json::Value::Bool(true);
            if let Some(st) = install {
                v["install"] = serde_json::to_value(st).unwrap_or_default();
            }
            Json(v).into_response()
        }
        None => fail(
            StatusCode::NOT_FOUND,
            "no restore of that title is known to this engine",
        ),
    }
}

#[derive(Debug, Deserialize)]
pub struct RestoreFinish {
    #[serde(alias = "console", alias = "host", alias = "ps5_addr")]
    pub addr: String,
    pub title_id: String,
    /// Put the package and its update back where they were (when the game is not on the home
    /// screen). Only once nothing installs on the console.
    #[serde(default)]
    pub rollback: bool,
    /// The install has ended (the user saw the game on the home screen): the copy aside may go.
    #[serde(default)]
    pub install_ended: bool,
    /// Put back after removing what a failed install left on the console (its unfinished copy and
    /// its download in the PS5's queue) with Sony's uninstall. Only with the copy aside there and
    /// no update or add-on of the title in place.
    #[serde(default)]
    pub clear: bool,
}

/// `POST /api/ps5/app/restore/finish {addr, title_id, rollback, install_ended, clear}`: finish a
/// restore left `pending` (an engine restart, a slow database). With `install_ended` the copy aside
/// is deleted once app.db lists the game and a package is in place; with `rollback` the package
/// and update go back where they were, only while app.db does not list the game and the place is
/// free; with `clear`, what a failed install left (the console's unfinished copy and its download
/// in the PS5's queue) is removed with Sony's uninstall first, then the package goes back
/// (`cleared`, `sony_rc`; `reason` `clearing` while Sony is still removing it: ask again); with
/// none, nothing changes and the answer says where it stands. Refused (409 `busy`) while an install
/// or a restore runs on that console. The helper's answer.
pub async fn finish_handler(
    State(state): State<PkgInstallStateHandle>,
    Json(req): Json<RestoreFinish>,
) -> Response {
    let Some(id) = normalized_id(&req.title_id) else {
        return fail(StatusCode::BAD_REQUEST, "title_id is not a title id");
    };
    if install::console_id(&req.addr).is_empty() {
        return fail(StatusCode::BAD_REQUEST, "addr is required");
    }
    let busy = || {
        fail(
            StatusCode::CONFLICT,
            "busy: an install is running on this console; finish the restore after it ends",
        )
    };
    if state.jobs.active_for(&req.addr) {
        return busy();
    }
    // A finish runs Sony's uninstall and puts packages back for up to minutes: nothing else of
    // Restore starts on the console meanwhile, nor a second finish.
    let _reserved = match try_reserve(&req.addr, &id, "finishing") {
        Ok(prev) => Reserved {
            addr: req.addr.clone(),
            title_id: id.clone(),
            prev,
        },
        Err(()) => return busy(),
    };
    let mgmt = crate::pkg_install::normalize_mgmt_addr(&req.addr);
    if req.clear {
        crate::log_at!(
            Info,
            "install",
            "restore {id} on {}: put back after clearing, asked by the user",
            req.addr
        );
    }
    let (i, rollback, ended) = (id.clone(), req.rollback, req.install_ended);
    let answer = if req.clear {
        // A short wait for Sony to finish removing its copy; the page can ask again after.
        Ok(clear_and_put_back(&mgmt, &id, CLEAR_TRIES / 4).await)
    } else {
        tokio::task::spawn_blocking(move || tr::finish(&mgmt, &i, rollback, ended)).await
    };
    match answer {
        Ok(Ok(r)) => {
            sizes_changed(&req.addr, &id, &r);
            let phase = match r.state.as_str() {
                "restored" => Some("restored"),
                "rolled_back" => Some("rolled_back"),
                "nothing_pending" => None,
                _ => Some("pending"),
            };
            if let Some(phase) = phase {
                let mut run = get_run(&req.addr, &id)
                    .unwrap_or_else(|| RestoreRun::new(&req.addr, &id, phase));
                run.phase = phase.into();
                run.cleared = run.cleared || r.cleared == Some(true);
                run.result = Some(r.clone());
                set_run(run);
            }
            Json(r).into_response()
        }
        Ok(Err(e)) => helper_failure(&e),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(phase: Phase, verdict: Option<Verdict>, reason: Option<FailReason>) -> InstallStatus {
        let mut st = InstallStatus::new("j", "192.168.1.2", "");
        st.phase = phase;
        st.verdict = verdict;
        st.reason = reason;
        st
    }

    #[test]
    fn only_an_install_the_console_never_took_is_put_back() {
        assert_eq!(after_install(None), AfterInstall::Leave);
        assert_eq!(
            after_install(Some(&status(Phase::Done, Some(Verdict::Installed), None))),
            AfterInstall::Finish
        );
        assert_eq!(
            after_install(Some(&status(
                Phase::Done,
                Some(Verdict::MayNotLaunch),
                None
            ))),
            AfterInstall::Finish
        );
        // The whole package read and nothing installed in the wait: a failed install to clear.
        assert_eq!(
            after_install(Some(&status(Phase::Done, Some(Verdict::Unverified), None))),
            AfterInstall::Clear
        );
        for r in [
            FailReason::SonyRefused,
            FailReason::StreamUnreachable,
            FailReason::StagedRefused,
            FailReason::BadRequest,
            FailReason::Busy,
        ] {
            assert_eq!(
                after_install(Some(&status(Phase::Failed, Some(Verdict::Failed), Some(r)))),
                AfterInstall::Rollback,
                "{r:?}"
            );
        }
        // Taken and then stopped (#961: stuck at 99.9% in the PS5's queue): cleared, put back.
        for r in [FailReason::Stalled, FailReason::Cancelled] {
            assert_eq!(
                after_install(Some(&status(Phase::Failed, Some(Verdict::Failed), Some(r)))),
                AfterInstall::Clear,
                "{r:?}"
            );
        }
        // Nobody knows what the installer is doing: nothing is touched.
        assert_eq!(
            after_install(Some(&status(
                Phase::Failed,
                Some(Verdict::Failed),
                Some(FailReason::ReplyLost)
            ))),
            AfterInstall::Leave
        );
        // Still running: never acted on.
        assert_eq!(
            after_install(Some(&status(Phase::Deliver, None, None))),
            AfterInstall::Leave
        );
    }

    #[test]
    fn a_rollback_blocked_by_the_failed_install_is_cleared() {
        let reply = |state: &str, reason: Option<&str>| RestoreReply {
            state: state.into(),
            reason: reason.map(str::to_string),
            ..Default::default()
        };
        for why in ["recorded_without_package", "on_home_screen", "place_taken"] {
            assert!(
                rollback_needs_clear(&reply("unsettled", Some(why))),
                "{why}"
            );
        }
        for (state, why) in [
            ("rolled_back", None),
            ("unsettled", Some("db_unreadable")),
            ("unsettled", Some("move_back_failed")),
            ("unsettled", Some("clearing")),
            ("nothing_pending", None),
        ] {
            assert!(!rollback_needs_clear(&reply(state, why)), "{state} {why:?}");
        }
    }

    #[test]
    fn a_stuck_installer_is_named_and_the_install_is_summed_up() {
        assert!(!installer_stuck(None));
        for r in [FailReason::ReplyLost, FailReason::LoaderUnreachable] {
            assert!(installer_stuck(Some(&status(
                Phase::Failed,
                Some(Verdict::Failed),
                Some(r)
            ))));
        }
        for r in [
            FailReason::Stalled,
            FailReason::SonyRefused,
            FailReason::Busy,
        ] {
            assert!(!installer_stuck(Some(&status(
                Phase::Failed,
                Some(Verdict::Failed),
                Some(r)
            ))));
        }
        let mut st = status(
            Phase::Failed,
            Some(Verdict::Failed),
            Some(FailReason::SonyRefused),
        );
        st.code = 0x80B2_116F;
        st.metrics.served_bytes = 99;
        st.metrics.total_bytes = 100;
        st.hint = Some("refused".into());
        let line = install_summary(Some(&st));
        for part in [
            "phase=Failed",
            "reason=Some(SonyRefused)",
            "sony_code=0x80B2116F",
            "served=99/100",
            "hint=\"refused\"",
        ] {
            assert!(line.contains(part), "{line}");
        }
        assert_eq!(install_summary(None), "install job unknown");
    }

    /// Two starts or finishes at once on one console: only one gets the console, the other is
    /// refused without touching the first one's run; a finish that ends without a phase of its
    /// own gives the console back as it was.
    #[test]
    fn only_one_restore_step_reserves_a_console() {
        let a = "10.9.8.4";
        assert_eq!(try_reserve(a, "PPSA55551", "queued"), Ok(None));
        assert!(console_busy(a));
        assert_eq!(try_reserve(a, "PPSA55551", "finishing"), Err(()));
        assert_eq!(try_reserve(a, "PPSA55552", "queued"), Err(()));
        assert_eq!(get_run(a, "PPSA55551").unwrap().phase, "queued");
        // Another console is not held up.
        assert!(try_reserve("10.9.8.3", "PPSA55551", "queued").is_ok());
        end_if_active("10.9.8.3", "PPSA55551", "test");

        // The first one ends; a finish takes the console, and gives it back when it ends with
        // nothing of its own to record.
        end_if_active(a, "PPSA55551", "test");
        let prev = try_reserve(a, "PPSA55551", "finishing").unwrap();
        assert_eq!(prev.as_deref(), Some("failed"));
        drop(Reserved {
            addr: a.into(),
            title_id: "PPSA55551".into(),
            prev,
        });
        assert_eq!(get_run(a, "PPSA55551").unwrap().phase, "failed");
        assert!(!console_busy(a));
        // A finish of a title with no run leaves none behind; one that recorded a phase keeps it.
        let prev = try_reserve(a, "PPSA55553", "finishing").unwrap();
        drop(Reserved {
            addr: a.into(),
            title_id: "PPSA55553".into(),
            prev,
        });
        assert!(get_run(a, "PPSA55553").is_none());
        let prev = try_reserve(a, "PPSA55553", "finishing").unwrap();
        update_run(a, "PPSA55553", |r| r.phase = "rolled_back".into());
        drop(Reserved {
            addr: a.into(),
            title_id: "PPSA55553".into(),
            prev,
        });
        assert_eq!(get_run(a, "PPSA55553").unwrap().phase, "rolled_back");
        assert!(!console_busy(a));
    }

    #[test]
    fn a_batch_that_ends_early_leaves_no_run_active() {
        let a = "10.9.8.5";
        set_run(RestoreRun::new(a, "PPSA66661", "restored"));
        set_run(RestoreRun::new(a, "PPSA66662", "installing"));
        set_run(RestoreRun::new(a, "PPSA66663", "queued"));
        assert!(console_busy(a));
        drop(BatchGuard {
            addr: a.into(),
            titles: vec!["PPSA66661".into(), "PPSA66662".into(), "PPSA66663".into()],
        });
        assert!(!console_busy(a));
        assert_eq!(get_run(a, "PPSA66661").unwrap().phase, "restored");
        let r = get_run(a, "PPSA66663").unwrap();
        assert_eq!(r.phase, "failed");
        assert!(r.error.unwrap().contains("stopped"));
        for t in ["PPSA66661", "PPSA66662", "PPSA66663"] {
            with_runs(|m| m.remove(&key(a, t)));
        }
    }

    #[test]
    fn start_names_titles_once_and_checks_them() {
        let req = |one: Option<&str>, many: Option<Vec<&str>>| RestoreStart {
            addr: "192.168.1.2".into(),
            title_id: one.map(str::to_string),
            title_ids: many.map(|v| v.into_iter().map(str::to_string).collect()),
        };
        assert_eq!(
            start_titles(&req(Some("ppsa01234"), None)).unwrap(),
            vec!["PPSA01234"]
        );
        assert_eq!(
            start_titles(&req(
                Some("PPSA01234"),
                Some(vec!["CUSA00900", "ppsa01234"])
            ))
            .unwrap(),
            vec!["PPSA01234", "CUSA00900"]
        );
        assert!(start_titles(&req(None, None)).is_err());
        assert!(start_titles(&req(Some("../x"), None)).is_err());
        let many: Vec<String> = (0..=MAX_TITLES).map(|i| format!("PPSA{i:05}")).collect();
        let r = RestoreStart {
            addr: "x".into(),
            title_id: None,
            title_ids: Some(many),
        };
        assert!(start_titles(&r).is_err());
    }

    #[test]
    fn runs_are_kept_per_console_and_title() {
        let a = "10.9.8.7:8000";
        set_run(RestoreRun::new(a, "PPSA77777", "installing"));
        assert!(console_busy("10.9.8.7"));
        assert!(!console_busy("10.9.8.6"));
        update_run("10.9.8.7", "PPSA77777", |r| r.phase = "restored".into());
        assert_eq!(get_run(a, "PPSA77777").unwrap().phase, "restored");
        assert!(!console_busy(a));
        with_runs(|m| m.remove(&key(a, "PPSA77777")));
    }
}
