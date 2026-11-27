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
//!      and update go back where they were; after anything that may still be running on the
//!      console (stalled, cancelled, reply lost, unverified), nothing is touched and the restore
//!      stays pending: the game page offers Finish or Put back later.
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
/// How long a finished install waits for app.db to list the title before the restore is left
/// pending (the database lags the files, both ways).
const SETTLE_TRIES: u32 = 36;
const SETTLE_EVERY: Duration = Duration::from_secs(5);

/// What to do once the install of the copy aside has ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterInstall {
    /// Installed: confirm with the helper and delete the copy aside.
    Finish,
    /// The console never took it: put the package back.
    Rollback,
    /// The console may still be working on it: touch nothing.
    Leave,
}

/// The decision, from the install job's last status (None: the job is unknown).
pub fn after_install(st: Option<&InstallStatus>) -> AfterInstall {
    let Some(st) = st else {
        return AfterInstall::Leave;
    };
    match (st.phase, st.verdict) {
        (_, Some(Verdict::Installed)) | (_, Some(Verdict::MayNotLaunch)) => AfterInstall::Finish,
        (_, Some(Verdict::Unverified)) => AfterInstall::Leave,
        (Phase::Failed, _) | (_, Some(Verdict::Failed)) => match st.reason {
            // Sony may have accepted it and go on pulling or writing: a package put back now
            // could be written over, or pulled from under the console.
            Some(FailReason::Stalled)
            | Some(FailReason::Cancelled)
            | Some(FailReason::ReplyLost) => AfterInstall::Leave,
            _ => AfterInstall::Rollback,
        },
        // Not ended.
        _ => AfterInstall::Leave,
    }
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

fn console_busy(addr: &str) -> bool {
    let cid = install::console_id(addr);
    with_runs(|m| {
        m.values()
            .any(|r| r.active() && install::console_id(&r.addr) == cid)
    })
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
        for id in ids.into_iter().take(MAX_TITLES * 2) {
            let check = tr::check(&mgmt, &id)?;
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
        Ok(out)
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
    if console_busy(&req.addr) {
        return fail(
            StatusCode::CONFLICT,
            "busy: a restore is already running on this console",
        );
    }
    let addr = req.addr.clone();
    let first = titles[0].clone();
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
    tokio::spawn(async move {
        run_one(st.clone(), &addr, &first, job2, prepared2).await;
        for t in rest {
            // A refusal is recorded by begin_one; the next title still goes.
            if let Ok((job, prepared)) = begin_one(&st, &addr, &t).await {
                run_one(st.clone(), &addr, &t, job, prepared).await;
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
        Err(e) => return Err(fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    };
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
    let mut run = RestoreRun::new(addr, id, "installing");
    run.job = Some(job.clone());
    run.prepared = Some(prepared.clone());
    set_run(run);
    Ok((job, prepared))
}

/// Install the copy aside in the reserved slot, then finish by the install's verdict.
async fn run_one(
    state: PkgInstallStateHandle,
    addr: &str,
    id: &str,
    job: String,
    prepared: RestoreReply,
) {
    let Some(path) = prepared.install_path.clone() else {
        return;
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
    let decision = after_install(st.as_ref());
    update_run(addr, id, |r| {
        r.phase = "finishing".into();
        r.install_verdict = st.as_ref().and_then(|s| s.verdict);
        r.install_hint = st.as_ref().and_then(|s| s.hint.clone());
    });
    let mgmt = crate::pkg_install::normalize_mgmt_addr(addr);
    let mut result: anyhow::Result<RestoreReply> = Err(anyhow::anyhow!("not asked"));
    match decision {
        AfterInstall::Finish => {
            for _ in 0..SETTLE_TRIES {
                let (m, i) = (mgmt.clone(), id.to_string());
                result = tokio::task::spawn_blocking(move || tr::finish(&m, &i, false, true))
                    .await
                    .map_err(anyhow::Error::from)
                    .and_then(|r| r);
                let waiting = matches!(&result, Ok(r) if r.state == "waiting");
                if !waiting {
                    break;
                }
                tokio::time::sleep(SETTLE_EVERY).await;
            }
        }
        AfterInstall::Rollback => {
            let (m, i) = (mgmt.clone(), id.to_string());
            result = tokio::task::spawn_blocking(move || tr::finish(&m, &i, true, true))
                .await
                .map_err(anyhow::Error::from)
                .and_then(|r| r);
        }
        AfterInstall::Leave => {
            // Only reads where it stands: with neither flag the helper touches nothing.
            let (m, i) = (mgmt.clone(), id.to_string());
            result = tokio::task::spawn_blocking(move || tr::finish(&m, &i, false, false))
                .await
                .map_err(anyhow::Error::from)
                .and_then(|r| r);
        }
    }
    let phase = match &result {
        Ok(r) if r.state == "restored" => "restored",
        Ok(r) if r.state == "rolled_back" => "rolled_back",
        _ => "pending",
    };
    crate::log_at!(
        Info,
        "install",
        "restore {id} on {addr}: install {:?}, {decision:?}: {phase} ({})",
        st.as_ref().and_then(|s| s.verdict),
        match &result {
            Ok(r) => format!("{} {}", r.state, r.reason.as_deref().unwrap_or("")),
            Err(e) => format!("{e:#}"),
        }
    );
    update_run(addr, id, |r| {
        r.phase = phase.into();
        match result {
            Ok(reply) => r.result = Some(reply),
            Err(e) => r.error = Some(format!("{e:#}")),
        }
    });
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
}

/// `POST /api/ps5/app/restore/finish {addr, title_id, rollback, install_ended}`: finish a restore
/// left `pending` (an engine restart, a slow database). With `install_ended` the copy aside is
/// deleted once app.db lists the game and a package is in place; with `rollback` the package and
/// update go back where they were, only while app.db does not list the game and the place is
/// free; with neither, nothing changes and the answer says where it stands. Refused (409 `busy`)
/// while an install or a restore runs on that console. The helper's `finish` answer.
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
    if state.jobs.active_for(&req.addr) || console_busy(&req.addr) {
        return fail(
            StatusCode::CONFLICT,
            "busy: an install is running on this console; finish the restore after it ends",
        );
    }
    let mgmt = crate::pkg_install::normalize_mgmt_addr(&req.addr);
    let (i, rollback, ended) = (id.clone(), req.rollback, req.install_ended);
    match tokio::task::spawn_blocking(move || tr::finish(&mgmt, &i, rollback, ended)).await {
        Ok(Ok(r)) => {
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
        assert_eq!(
            after_install(Some(&status(Phase::Done, Some(Verdict::Unverified), None))),
            AfterInstall::Leave
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
        for r in [
            FailReason::Stalled,
            FailReason::Cancelled,
            FailReason::ReplyLost,
        ] {
            assert_eq!(
                after_install(Some(&status(Phase::Failed, Some(Verdict::Failed), Some(r)))),
                AfterInstall::Leave,
                "{r:?}"
            );
        }
        // Still running: never acted on.
        assert_eq!(
            after_install(Some(&status(Phase::Deliver, None, None))),
            AfterInstall::Leave
        );
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
