//! Unified install module (spec 2): one endpoint + orchestrator, status,
//! delivery decision, and per-console history. See
//! ps5upload-docs/superpowers/specs/2026-09-26-unified-install-module-design.md

pub mod status;

pub mod history;

pub mod deliver;

use std::collections::HashMap;
use std::sync::Mutex;

use ps5upload_core::installer_client as ic;
use ps5upload_core::payload_lifecycle::{
    DPI_REASON_LOADER_UNREACHABLE, DPI_REASON_NO_BRINGUP, DPI_REASON_NO_IMAGE,
};

use status::{FailReason, InstallStatus, Phase, Verdict};

/// One install job per console at a time. `begin` reserves the console;
/// `finish` releases it. Statuses live here until the process restarts (the
/// durable record is the history log), up to `KEEP_FINISHED` finished ones.
///
/// Every lock recovers from poisoning: the maps stay consistent between
/// statements, and a panic in one console's install must not break every
/// console's installs until a restart.
pub struct JobStore {
    jobs: Mutex<HashMap<String, InstallStatus>>,
    active: Mutex<HashMap<String, String>>, // console_id -> job_id
    seq: Mutex<u64>,
    /// Jobs the user asked to stop.
    cancelled: Mutex<std::collections::HashSet<String>>,
    /// Wakes what a job is waiting for when someone asks a job to stop (see `stop_requested`).
    stop_signal: tokio::sync::Notify,
}

/// Finished statuses kept for `/api/pkg/install/status` (the client reads a job's result
/// shortly after it ends); older ones are dropped.
const KEEP_FINISHED: usize = 200;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
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
            cancelled: Mutex::new(std::collections::HashSet::new()),
            stop_signal: tokio::sync::Notify::new(),
        }
    }

    fn new_job_id(&self) -> String {
        let mut s = lock(&self.seq);
        *s += 1;
        format!("{}-{}", status::now_unix(), *s)
    }

    /// Reserve the console. `Ok(job_id)` if free; `Err(active_job_id)` if an
    /// install is already running for that console.
    pub fn begin(&self, ps5_addr: &str) -> Result<String, String> {
        let cid = slot_key(ps5_addr);
        let mut active = lock(&self.active);
        if let Some(j) = active.get(&cid) {
            return Err(j.clone());
        }
        let job = self.new_job_id();
        active.insert(cid, job.clone());
        let st = InstallStatus::new(&job, ps5_addr, "");
        publish_status(&st);
        lock(&self.jobs).insert(job.clone(), st);
        Ok(job)
    }

    /// Release the console's slot (if this job still holds it), forget the job's
    /// stop request and drop the oldest finished statuses past `KEEP_FINISHED`.
    pub fn finish(&self, job: &str) {
        // Taken before the jobs lock: `begin` locks the slots first, then the jobs.
        let holding: std::collections::HashSet<String> =
            lock(&self.active).values().cloned().collect();
        // Read before the prune, which may drop this very status.
        let ended = {
            let mut jobs = lock(&self.jobs);
            let ended = jobs.get(job).cloned();
            prune_finished(&mut jobs, KEEP_FINISHED, &holding);
            ended
        };
        lock(&self.cancelled).remove(job);
        if let Some(st) = ended {
            let cid = slot_key(&st.ps5_addr);
            let mut active = lock(&self.active);
            // only clear if this job still owns the slot
            if active.get(&cid).map(|j| j.as_str()) == Some(job) {
                active.remove(&cid);
                // Once per job: a second finish finds the slot already released.
                crate::activity::install_ended(&st);
            }
        }
    }

    /// Every status still known, running or finished (up to `KEEP_FINISHED` of those).
    pub fn all(&self) -> Vec<InstallStatus> {
        lock(&self.jobs).values().cloned().collect()
    }

    /// Statuses of the installs still holding a console's slot.
    pub fn running(&self) -> Vec<InstallStatus> {
        let ids: Vec<String> = lock(&self.active).values().cloned().collect();
        let jobs = lock(&self.jobs);
        ids.iter().filter_map(|j| jobs.get(j).cloned()).collect()
    }

    /// The install task ended without finishing the job (it panicked): a job still
    /// marked running is marked failed so the client stops waiting, and the slot is
    /// released. A job that finished normally was already released; nothing changes.
    pub fn abandon(&self, job: &str) {
        let holds_slot = lock(&self.active).values().any(|j| j == job);
        if !holds_slot {
            return;
        }
        self.update(job, |st| {
            if !matches!(st.phase, Phase::Done | Phase::Failed) {
                st.phase = Phase::Failed;
                st.verdict = Some(Verdict::Failed);
                st.hint = Some("the install stopped unexpectedly (see the engine log)".into());
            }
        });
        self.finish(job);
    }

    /// Whether `ps5_addr` has an install running.
    pub fn active_for(&self, ps5_addr: &str) -> bool {
        lock(&self.active).contains_key(&slot_key(ps5_addr))
    }

    /// Whether any console has an install running.
    pub fn any_active(&self) -> bool {
        !lock(&self.active).is_empty()
    }

    /// Ask a running job to stop. False when there is no such job or it already ended.
    pub fn request_cancel(&self, job: &str) -> bool {
        let running = lock(&self.jobs)
            .get(job)
            .is_some_and(|st| !matches!(st.phase, Phase::Done | Phase::Failed));
        if running {
            lock(&self.cancelled).insert(job.to_string());
            self.stop_signal.notify_waiters();
        }
        running
    }

    pub fn is_cancelled(&self, job: &str) -> bool {
        lock(&self.cancelled).contains(job)
    }

    /// Resolves once `job` has been asked to stop.
    pub async fn stop_requested(&self, job: &str) {
        loop {
            let woken = self.stop_signal.notified();
            tokio::pin!(woken);
            // Armed before the flag is read: a request in between still wakes this.
            woken.as_mut().enable();
            if self.is_cancelled(job) {
                return;
            }
            woken.await;
        }
    }

    pub fn get(&self, job: &str) -> Option<InstallStatus> {
        lock(&self.jobs).get(job).cloned()
    }

    pub fn update(&self, job: &str, f: impl FnOnce(&mut InstallStatus)) {
        let mut jobs = lock(&self.jobs);
        if let Some(st) = jobs.get_mut(job) {
            let (phase, verdict, stamp) = (st.phase, st.verdict, st.updated_at);
            f(st);
            st.updated_at = status::now_unix();
            // On a change of phase or result, and at most once a second for progress.
            if st.phase != phase || st.verdict != verdict || st.updated_at != stamp {
                publish_status(st);
            }
        }
    }
}

/// An install's job id (`<epoch>-<seq>`), which a transfer's UUID can never be taken for: a UUID has
/// four dashes.
pub fn is_install_job_id(id: &str) -> bool {
    id.split_once('-').is_some_and(|(epoch, seq)| {
        !epoch.is_empty()
            && !seq.is_empty()
            && epoch.bytes().all(|b| b.is_ascii_digit())
            && seq.bytes().all(|b| b.is_ascii_digit())
    })
}

/// One install in the `/api/jobs` list, beside the transfers.
pub fn job_list_entry(st: &InstallStatus) -> serde_json::Value {
    serde_json::json!({
        "job_id": st.job,
        "kind": "install",
        "status": match st.phase {
            Phase::Done => "done",
            Phase::Failed => "failed",
            _ => "running",
        },
        "job": st,
    })
}

/// The install's status as an `install` event on `/api/events`, and its place for `/api/jobs`.
fn publish_status(st: &InstallStatus) {
    if let Ok(v) = serde_json::to_value(st) {
        let bus = crate::event_bus::bus();
        let seq = bus.publish_json("install", &v);
        bus.note_job(&st.job, seq);
    }
}

/// Drops the oldest finished statuses until at most `keep` remain. Running ones (those holding
/// a console's slot) stay; one that ended without reaching Done or Failed counts as finished,
/// or it would be kept forever.
fn prune_finished(
    jobs: &mut HashMap<String, InstallStatus>,
    keep: usize,
    holding: &std::collections::HashSet<String>,
) {
    let mut finished: Vec<(u64, String)> = jobs
        .values()
        .filter(|st| matches!(st.phase, Phase::Done | Phase::Failed) || !holding.contains(&st.job))
        .map(|st| (st.updated_at, st.job.clone()))
        .collect();
    if finished.len() <= keep {
        return;
    }
    finished.sort();
    let excess = finished.len() - keep;
    for (_, job) in finished.into_iter().take(excess) {
        jobs.remove(&job);
    }
}

/// Releases a console's install slot when dropped: when the install task ends,
/// including by a panic, which used to leave the console "busy" until a restart.
pub struct SlotRelease {
    state: crate::pkg_install::PkgInstallStateHandle,
    job: String,
}

impl SlotRelease {
    pub fn new(state: crate::pkg_install::PkgInstallStateHandle, job: String) -> Self {
        Self { state, job }
    }
}

impl Drop for SlotRelease {
    fn drop(&mut self) {
        self.state.jobs.abandon(&self.job);
    }
}

/// The per-console key: the host part of the address, port-stripped, so
/// `ip:<port>` and a bare `ip` map to one console. Bracketed and bare IPv6
/// addresses keep their whole host (a plain split at the last colon cut them).
pub fn console_id(ps5_addr: &str) -> String {
    ps5upload_core::events::console_host(ps5_addr)
}

/// The install-slot key: [`console_id`] case-folded, so `PS5.local` and `ps5.local` (or an
/// IPv6 address in either case) are one console with one slot.
fn slot_key(ps5_addr: &str) -> String {
    console_id(ps5_addr).to_ascii_lowercase()
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

/// Whether the plain-path last resort may run.
///
/// The situation it exists for: the package is on the console, the console refused it from
/// its own loopback server (0x80B2116F), and it cannot fetch it from this engine either (a
/// firewall, a direct cable with no route back). Sony's installer treats a plain file path
/// differently from a URL on the console itself, and sometimes accepts it.
///
/// That route has destroyed things: a patch or add-on shares its base game's content id and
/// a failed attempt wiped the base; a failed re-install removed the working copy it was
/// replacing (Sony clears the old title before writing the new one). So it runs only where a
/// failure has nothing to delete: a base game (`…gd`, stated, never assumed) whose title id
/// is known and is NOT installed. And only when asked for, and only as the last route.
pub fn path_fallback_allowed(
    enabled: bool,
    source: &Source,
    category: &str,
    title_known: bool,
    already_installed: bool,
    console_cannot_reach_engine: bool,
) -> bool {
    enabled
        && console_cannot_reach_engine
        && matches!(source, Source::ConsolePath(_))
        && category.to_ascii_lowercase().ends_with("gd")
        && title_known
        && !already_installed
}

/// How long the last resort waits for the title's content to appear before calling the
/// accept empty. A stream install of a 3 MB package wrote `app.pkg` within seconds; a large
/// one preallocates it at the start.
pub const PLAIN_PATH_CONTENT_WAIT: std::time::Duration = std::time::Duration::from_secs(90);

/// What the user reads when Sony accepted the file path and then installed nothing.
pub const PLAIN_PATH_NO_CONTENT_HINT: &str = "The PS5 accepted the package by file path, but no game content appeared on any drive, so it is not installed. If a tile for it shows on the home screen, delete it there. The package was kept: let the PS5 reach this computer and use Stream & install, or install it on the console from Settings > System > Debug Settings > Game > Package Installer.";

/// What the user reads when the last resort was refused too.
pub fn path_fallback_refused_hint(code: u32) -> String {
    format!(
        "The PS5 could not reach this computer to fetch the package, and it also refused the copy on its own storage by file path (0x{code:08X}). Nothing was changed and the package was kept. Either let the PS5 reach this computer (allow ps5upload through the firewall, same network, Proxy Server set to Do Not Use) and use Stream & install, or install it on the console from Settings > System > Debug Settings > Game > Package Installer."
    )
}

/// Re-applying an update the game already has, or an older one: Sony removes the applied update
/// first, and the game can end up back on its base version (measured; `PKG_PATCH_REGRESSED_HINT`).
/// Unknown versions say nothing, so they never refuse.
pub fn update_already_applied(package_app_ver: &str, installed_app_ver: &str) -> bool {
    use ps5upload_core::patch_verify::parse_app_ver;
    match (
        parse_app_ver(package_app_ver),
        parse_app_ver(installed_app_ver),
    ) {
        (Some(p), Some(i)) => i >= p,
        _ => false,
    }
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
        Some(DPI_REASON_LOADER_UNREACHABLE) => FailReason::LoaderUnreachable,
        Some(DPI_REASON_NO_IMAGE) => FailReason::NoImage,
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

/// What a finished version check means for the install. A wait that ran out says nothing about
/// the install, unless the console is known to hold the whole package and the title has gone below
/// where it started: only then is it a failure. And of a link the console fetches by itself, only
/// a version that moved says anything: it may not have got that far.
pub fn verdict_after_wait(
    pv: ps5upload_core::patch_verify::PatchVerdict,
    timed_out: bool,
    delivered: bool,
) -> Verdict {
    use ps5upload_core::patch_verify::PatchVerdict::*;
    if (timed_out || !delivered) && !matches!((pv, delivered), (Applied, _) | (Regressed, true)) {
        return Verdict::Unverified;
    }
    verdict_from_verify(pv, !matches!(pv, DidNotApply | Regressed))
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
    stream && never_fetched && is_network_class(code)
}

/// Sony's network-class codes (0x8043xxxx): only these say the console could not fetch. Any other
/// code on a stream it never fetched from is a verdict on the package, not on the firewall.
pub fn is_network_class(code: u32) -> bool {
    (code & 0xFFFF_0000) == 0x8043_0000
}

/// What failed when the install call came back without Sony's verdict, for the job's reason.
pub fn reason_from_reply(reply: &Result<ic::InstallReply, String>) -> Option<FailReason> {
    match reply {
        Ok(ic::InstallReply::Busy { .. }) => Some(FailReason::Busy),
        Ok(ic::InstallReply::SourceUnreadable) => Some(FailReason::SourceGone),
        Ok(ic::InstallReply::BadPath) | Ok(ic::InstallReply::BadRequest) => {
            Some(FailReason::BadRequest)
        }
        Ok(ic::InstallReply::LoopbackFailed) | Ok(ic::InstallReply::NotReady { .. }) => {
            Some(FailReason::NoBringup)
        }
        Ok(ic::InstallReply::Unknown(_)) | Err(_) => Some(FailReason::ReplyLost),
        Ok(ic::InstallReply::Accepted { .. })
        | Ok(ic::InstallReply::UnknownJob)
        | Ok(ic::InstallReply::Sony { .. }) => None,
    }
}

/// Whether a console-path install the console refused goes on through this engine. A network-class
/// refusal needs a fresh daemon first (see `should_recycle_daemon`).
pub fn staged_retry_through_engine(reply: &Result<ic::InstallReply, String>) -> Option<u32> {
    match reply {
        Ok(ic::InstallReply::Sony { code, .. })
            if STAGED_ROUTE_REFUSALS.contains(code) || is_network_class(*code) =>
        {
            Some(*code)
        }
        Ok(ic::InstallReply::LoopbackFailed) => Some(0),
        _ => None,
    }
}

/// The form a package's type takes for the artifact check: its platform (a PS5 title id is PPSA)
/// and its kind, from a category that may be `gp` or `PS4DP`.
pub fn package_type_for_verify(category: &str, title_id: &str) -> String {
    let c = category.to_ascii_lowercase();
    let platform = if title_id.starts_with("PPSA") || c.starts_with("ps5") {
        "PS5"
    } else {
        "PS4"
    };
    let kind = if c.ends_with("gp") || c.ends_with("dp") {
        "DP"
    } else if c.ends_with("ac") {
        "AC"
    } else {
        "GD"
    };
    format!("{platform}{kind}")
}

/// The package needs a base game and its absence is a known silent no-op: PS4 only. A PS5 update
/// can carry the whole app and installs on its own (measured), so there the PS5 decides.
pub fn base_check_applies(category: &str, title_id: &str) -> Option<&'static str> {
    if package_type_for_verify(category, title_id).starts_with("PS5") {
        return None;
    }
    let c = category.to_ascii_lowercase();
    if c.ends_with("gp") || c.ends_with("dp") {
        Some("gp")
    } else if c.ends_with("ac") {
        Some("ac")
    } else {
        None
    }
}

/// What the console's next request for this session should find, once the job has stopped
/// waiting on delivery. A released session answers 410, and PlayGo answers that by deleting what
/// it downloaded (observed: 64.8 GiB). So only a user's Stop releases one at once. A stall may be
/// a pause: the PS5 runs one install at a time and holds a second until the first ends, and a
/// download can be paused there. It stays served until the idle prune, and `is_rival` lets a
/// retry through. A complete delivery stays for a moment too, for a request still in flight.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SessionAfterDelivery {
    Release,
    ReleaseWhenQuiet,
    Keep,
}

pub fn session_after_delivery(delivery: DeliveryProgress) -> SessionAfterDelivery {
    match delivery {
        DeliveryProgress::Cancelled => SessionAfterDelivery::Release,
        DeliveryProgress::Complete => SessionAfterDelivery::ReleaseWhenQuiet,
        DeliveryProgress::Stalled | DeliveryProgress::Pending => SessionAfterDelivery::Keep,
    }
}

/// Whether a refused start should release the session it had set up. Only an answer that says
/// Sony did not take it: a lost or unreadable reply says nothing, and the console may already be
/// fetching.
pub fn release_after_refusal(reply: &Result<ic::InstallReply, String>) -> bool {
    !matches!(reply, Err(_) | Ok(ic::InstallReply::Unknown(_)))
}

/// How long a fully delivered session goes on answering after its last request.
const QUIET_BEFORE_RELEASE_SECS: u64 = 60;

/// Release `sid` once no console has asked it for anything for `QUIET_BEFORE_RELEASE_SECS`.
/// Coverage counts a range as it goes out, so the last range can still fail on its way; the
/// console's retry must find the session.
fn release_when_quiet(state: &PkgInstallStateHandle, sid: &str) {
    let (state, sid) = (state.clone(), sid.to_string());
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let last = {
                let s = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
                s.get(&sid).map(|x| x.last_activity_unix)
            };
            let Some(last) = last else { return };
            if status::now_unix().saturating_sub(last) >= QUIET_BEFORE_RELEASE_SECS {
                crate::pkg_install::release_serve_session(&state.sessions, &sid);
                return;
            }
        }
    });
}

/// Stop the installer daemon and wait (bounded) until :9115 has actually
/// closed, so the next install's ensure sends a fresh one. A daemon still
/// answering while it exits makes that ensure skip sending a new one and then
/// hit a refused connect.
async fn recycle_daemon(ip: &str) {
    let ip_s = ip.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        // Forced: a plain stop is refused for exactly the job this is clearing.
        if let Err(e) = ic::stop_forced(&ip_s) {
            crate::log_at!(
                Warn,
                "install",
                "installer daemon on {ip_s} did not stop: {e}"
            );
            return;
        }
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
    match (never_fetched && is_network_class(code), code) {
        (true, SCE_HTTP_ERROR_PROXY) => FailReason::StreamProxy,
        (true, _) => FailReason::StreamUnreachable,
        _ if route == Route::Loopback && STAGED_ROUTE_REFUSALS.contains(&code) => {
            FailReason::StagedRefused
        }
        _ => FailReason::SonyRefused,
    }
}

/// Whether a failed install should offer "Retry with Stream".
///
/// Only a package the console refused from its own storage qualifies, and only a `console_path`
/// source (every other source already streams). A patch or add-on shares its base game's
/// content_id, and every in-process fallback tier re-registers that id and WIPES the base, so
/// DPI (the installer daemon) is its only safe route: it is offered for those categories only
/// when the stream goes through the daemon (`via_daemon`). Every route this engine's
/// `install_handler` runs does, which is why the engine passes `true`; the parameter keeps the
/// rule explicit and testable.
pub fn stream_retry_offered(
    reason: Option<FailReason>,
    source: &Source,
    category: &str,
    via_daemon: bool,
) -> bool {
    reason == Some(FailReason::StagedRefused)
        && matches!(source, Source::ConsolePath(_))
        && stream_retry_allowed(category, via_daemon)
}

/// A forced-stream install of a patch/DLC package may only run through the daemon.
pub fn stream_retry_allowed(category: &str, via_daemon: bool) -> bool {
    let c = category.to_ascii_lowercase();
    let shares_base_id = c.ends_with("gp") || c.ends_with("dp") || c.ends_with("ac");
    via_daemon || !shares_base_id
}

/// Sony codes seen refusing the staged (Loopback) route while the same
/// package streamed from a computer installed: 0x80B2116F on FW 9.60 and
/// 13.60, 0x80B2150F on FW 5.10. On 13.60 the cause was ours, not the route
/// (installer daemon before 1.3.9 handed Sony a too-short MetaInfo; see
/// payload/include/sceAppInstUtil.h), and the staged route installs there
/// now. 9.60 and 5.10 have not been measured since, so the retry through
/// this engine stays.
///
/// 0x80A30003 is the install source being too long (127 bytes, see `inst_loopback_serve_name`):
/// a daemon older than 1.6.0 hits it on a long package name, and the engine's URL is short.
const STAGED_ROUTE_REFUSALS: &[u32] = &[0x80B2_116F, 0x80B2_150F, 0x80A3_0003];

pub fn stream_unreachable_hint(
    served_from: Option<&str>,
    code: u32,
    diag: Option<&crate::win_net::NetDiag>,
) -> String {
    let mut hint = stream_unreachable_hint_for(
        served_from,
        code,
        crate::pkg_install::bridged_container_without_pkg_host_ip(),
        diag,
    );
    // Publishing the container on another host port looks exactly like a firewall from here.
    if crate::pkg_install::in_container() {
        hint.push(' ');
        hint.push_str(HOST_PORT_ADVICE);
    }
    hint
}

/// The remedy for a container published on another host port (`-p 8080:19113`): the engine told
/// the console the port it listens on inside.
const HOST_PORT_ADVICE: &str = "If the container is published on a different port from the one shown, set PS5UPLOAD_PKG_HOST_PORT to that port.";

/// `stream_unreachable_hint` with the container check passed in, so both
/// wordings are testable. In a bridged container the firewall advice is
/// wrong: the engine told the PS5 to fetch from the container's internal
/// address, which nothing outside the Docker host can reach.
fn stream_unreachable_hint_for(
    served_from: Option<&str>,
    code: u32,
    bridged_container: bool,
    diag: Option<&crate::win_net::NetDiag>,
) -> String {
    // Code 0 means the console reported nothing (a reach check or a stall):
    // do not print a made-up "0x00000000".
    let rc = if code == 0 {
        String::new()
    } else {
        format!(" (0x{code:08x})")
    };
    let at = served_from.map(|o| format!(" at {o}")).unwrap_or_default();
    if bridged_container {
        return format!(
            "The PS5 never reached the engine{at} to fetch the package{rc}. The engine is running in a container, so that is the container's internal address, which the PS5 cannot reach. Run the container with host networking (`--network host`, or `network_mode: host` in Compose), or set PS5UPLOAD_PKG_HOST_IP to the Docker host's LAN IP and publish port 19113."
        );
    }
    if code == SCE_HTTP_ERROR_PROXY {
        return format!(
            "The PS5's proxy setting blocked the stream{rc}. In the PS5's network Advanced Settings set Proxy Server to \u{201c}Do Not Use\u{201d}, or use Upload & install, which reads the package from PS5-local storage."
        );
    }
    // Windows knows which network the console is on and whether the firewall lets us in on it:
    // say that instead of the generic firewall paragraph (F2.3).
    if let Some(why) = diag.and_then(|d| d.explain()) {
        return format!(
            "The PS5 never reached this computer{at} to fetch the package{rc}. {why} Keep the computer and the PS5 on the same network with any VPN off, and set the PS5's Proxy Server to \u{201c}Do Not Use\u{201d}. {HOST_IP_ADVICE} Upload & install works without this connection."
        );
    }
    format!(
        "The PS5 never reached this computer{at} to fetch the package{rc}. Allow ps5upload through this computer's firewall (on Windows, for both Private and Public networks), keep the computer and the PS5 on the same network with any VPN off, and set the PS5's Proxy Server to \u{201c}Do Not Use\u{201d}. {HOST_IP_ADVICE} Upload & install works without this connection."
    )
}

/// The remedy for the other half of "the PS5 could not reach us": the engine
/// advertised an address the console cannot route to (a VPN, a virtual-machine
/// or container adapter) rather than this computer's LAN address.
const HOST_IP_ADVICE: &str = "If the address shown is not this computer's LAN address (a VPN, virtual-machine or container address), set PS5UPLOAD_PKG_HOST_IP to this computer's LAN IP (for example 192.168.x.y) and restart the engine.";

/// The Windows diagnosis of the link to the console at `ip`, for an engine reachable at `origin`
/// (`http://host:port`). `None` off Windows. Runs only after a failure: it takes a second or two.
async fn net_diag_for(ip: &str, origin: &str) -> Option<crate::win_net::NetDiag> {
    let port: u16 = origin.rsplit_once(':')?.1.parse().ok()?;
    let console = crate::console_addr(ip);
    tokio::task::spawn_blocking(move || crate::win_net::diagnose(&console, port))
        .await
        .ok()
        .flatten()
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
    /// The user stopped the install (POST /api/pkg/install/stop).
    Cancelled,
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
    /// Skip the console-local attempt and serve the package from this engine through
    /// the installer daemon ("Retry with Stream" after 0x80B2116F). Only meaningful for a
    /// `console_path` source; see [`stream_retry_allowed`].
    #[serde(default)]
    pub force_stream: bool,
    /// Allow the last resort for a console that cannot reach this engine: install the
    /// console's copy by its plain file path. Off unless the client asks (a beta setting);
    /// see [`path_fallback_allowed`] for everything else that must hold.
    #[serde(default)]
    pub console_path_fallback: bool,
    /// For a link: this engine downloads it and serves it to the console ("Stream through
    /// this computer"), instead of handing the console the link to fetch by itself. Without
    /// it the console is given the link first, and a link only this computer can reach (a
    /// private server, a VPN, localhost) simply failed with a network error.
    #[serde(default)]
    pub proxy_link: bool,
    /// Skip certificate checks when THIS engine fetches a link. Never applies to the
    /// console's own fetch.
    #[serde(default)]
    pub insecure_tls: bool,
}

#[derive(Debug, Deserialize)]
pub struct InstallRequest {
    #[serde(alias = "console", alias = "addr", alias = "host")]
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
    // A test that ends a job must not write to the user's own history.
    if cfg!(test) {
        return std::env::temp_dir().join(format!("ps5u-install-history-{}", std::process::id()));
    }
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
/// machine, return the job id. A 409 `busy` (with the active job) if an install
/// is already running for that console.
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
    if req.options.force_stream {
        if !matches!(req.source, Source::ConsolePath(_)) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"ok":false,"error":
                    "force_stream only applies to a package on the console; other sources already stream"})),
            )
                .into_response();
        }
        // Every route in this handler goes through the installer daemon (DPI).
        if !stream_retry_allowed(req.category.as_deref().unwrap_or(""), true) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"ok":false,"error":
                    "a patch or add-on can only be installed through the installer daemon"})),
            )
                .into_response();
        }
    }
    // Away on Tailscale: an install talks to the installer daemon (port 9040) and the PS5
    // fetches the package from this device, neither of which Tailscale carries (the console
    // serves only Remote Play and the helper there, and never dials out over it). Say so now
    // instead of a timeout.
    {
        let ip = ip.clone();
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
                    "error": ps5upload_core::route::home_only("Installing a package"),
                })),
            )
                .into_response();
        }
    }
    let job = match state.jobs.begin(&req.ps5_addr) {
        Ok(j) => j,
        Err(active) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"ok":false,"error":"busy","job":active})),
            )
                .into_response()
        }
    };
    let st = state.clone();
    let job2 = job.clone();
    tokio::spawn(async move {
        // Released however the task ends (finalize also releases it on the normal path).
        let _slot = SlotRelease::new(st.clone(), job2.clone());
        run_install(st, job2, req).await;
    });
    Json(serde_json::json!({"ok":true,"job":job})).into_response()
}

#[derive(Debug, Deserialize)]
pub struct JobQuery {
    pub job: String,
}

/// `POST /api/pkg/install/stop {job}`: stop a running install. The job ends as `cancelled` at
/// once, whatever it was waiting for. The console cannot be interrupted: a call it is still
/// answering finishes by itself and its answer is dropped.
pub async fn install_stop_handler(
    State(state): State<PkgInstallStateHandle>,
    Json(q): Json<JobQuery>,
) -> Response {
    if state.jobs.request_cancel(&q.job) {
        Json(serde_json::json!({"ok":true})).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"ok":false,"error":"no running install with that id"})),
        )
            .into_response()
    }
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
    #[serde(alias = "console", alias = "addr", alias = "host")]
    pub ps5_addr: String,
}

pub async fn install_history_handler(Query(q): Query<AddrQuery>) -> Response {
    let entries = history::read_recent(&history_dir(), &q.ps5_addr, 50);
    Json(entries).into_response()
}

/// What a stopped job tells the user. The console may already have it.
const STOPPED_HINT: &str = "stopped; if the PS5 still lists the download, delete it there";

/// `fut`'s answer, or `None` as soon as the user stops `job`. A blocking call behind it cannot be
/// interrupted: it finishes on its own thread and the answer goes nowhere.
async fn unless_stopped<T>(
    state: &PkgInstallStateHandle,
    job: &str,
    fut: impl std::future::Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        _ = state.jobs.stop_requested(job) => None,
        out = fut => Some(out),
    }
}

/// One call to the installer daemon, which may take as long as its 15-minute timeout to answer.
/// The user stopping the job answers it with an error at once; `ended_by_stop` then ends the job.
async fn install_call(
    state: &PkgInstallStateHandle,
    job: &str,
    call: impl FnOnce() -> Result<ic::InstallReply, String> + Send + 'static,
) -> Result<ic::InstallReply, String> {
    match unless_stopped(state, job, tokio::task::spawn_blocking(call)).await {
        Some(r) => r.unwrap_or_else(|e| Err(format!("install task failed: {e}"))),
        None => Err("stopped by the user".into()),
    }
}

/// Ends the job if the user asked it to stop, and says so. `session`: a pkg-host session to release.
async fn ended_by_stop(
    state: &PkgInstallStateHandle,
    job: &str,
    req: &InstallRequest,
    started: std::time::Instant,
    session: Option<&str>,
) -> bool {
    if !state.jobs.is_cancelled(job) {
        return false;
    }
    crate::log_at!(
        Info,
        "install",
        "{}: stopped by the user",
        correlation_tag(job)
    );
    if let Some(sid) = session {
        crate::pkg_install::release_serve_session(&state.sessions, sid);
    }
    state.jobs.update(job, |s| {
        s.phase = Phase::Failed;
        s.verdict = Some(Verdict::Failed);
        s.reason = Some(FailReason::Cancelled);
        s.hint = Some(STOPPED_HINT.into());
    });
    finalize(state, job, req, started).await;
    true
}

/// The async state machine: resolve → deliver → install → verify → done/failed.
/// Always clears the active-job guard and records history at the end. A stop ends it wherever it
/// is: every wait goes through `unless_stopped` or checks the flag, and `ended_by_stop` follows.
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
            let meta = unless_stopped(
                &state,
                &job,
                tokio::task::spawn_blocking(move || {
                    ps5upload_pkg::metadata_from_reader(|off, len| {
                        ps5upload_core::fs_ops::fs_read(&m, &p, off, len).ok()
                    })
                }),
            )
            .await
            .and_then(Result::ok)
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
    // The same for a file on this computer: whatever the caller left out, for every check below.
    if let Source::HostFile(path) = &req.source {
        if req.category.is_none()
            || req.package_app_ver.is_none()
            || req.title_id.is_none()
            || req.content_id.trim().is_empty()
        {
            let p = path.clone();
            let meta = tokio::task::spawn_blocking(move || read_host_header(&p))
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
    // And for a package on a saved server: the client does not read it, the serve session would.
    if let Source::Remote { connection, path } = &req.source {
        let remote = format!("remote://{connection}/{path}");
        let head = unless_stopped(&state, &job, async move {
            crate::pkg_install::open_remote_package(&remote).await
        })
        .await;
        if let Some(Ok((_, _, h, _, _))) = head {
            fill_from_header(
                &mut req,
                &h.content_id,
                &h.title_id,
                &h.category,
                &h.app_ver,
            );
        }
    }
    crate::log_at!(
        Info,
        "install",
        "{tag}: install start ps5={ip} source={} content_id={} category={} app_ver={}",
        req.source.kind(),
        req.content_id,
        req.category.as_deref().unwrap_or("?"),
        req.package_app_ver.as_deref().unwrap_or("?")
    );
    // A category that says full game, for a package whose header says add-on: go on the header
    // for every check below (#724, #776). Read only when the category says game. A bare link is
    // left alone: the console fetches it itself and nothing is verified against it.
    if let Some(sent) = req
        .category
        .clone()
        .filter(|c| certainly_a_game(c) && !matches!(req.source, Source::Url(_)))
    {
        let header = unless_stopped(
            &state,
            &job,
            read_source_header(&req.source, &mgmt, req.options.insecure_tls),
        )
        .await
        .flatten();
        if let Some(fixed) = category_from_header(&sent, header.as_ref()) {
            crate::log_at!(
                Info,
                "install",
                "{tag}: the category sent says full game ({sent}), the package's header says add-on: going on {fixed}"
            );
            req.category = Some(fixed);
        }
    }
    ps5upload_core::events::emit_detail(
        ps5upload_core::events::Cat::Install,
        ps5upload_core::events::Level::Info,
        "install_start",
        Some(&req.ps5_addr),
        format!("install start {} ({})", req.content_id, req.source.kind()),
        serde_json::json!({ "category": req.category, "app_ver": req.package_app_ver }),
    );
    let category = req.category.clone().unwrap_or_default();
    let patch_ver = patch_check_version(&category, req.package_app_ver.as_deref());
    let title_id = req.title_id.clone().filter(|t| !t.trim().is_empty());
    let mut route = match decide_delivery(&req.source) {
        Delivery::Loopback => Route::Loopback,
        Delivery::Stream => Route::Stream,
    };
    crate::log_at!(
        Debug,
        "install",
        "{tag}: first route {route:?}; category {category:?}, patch check {}, title id {}",
        patch_ver.is_some(),
        title_id.as_deref().unwrap_or("-")
    );
    state.jobs.update(&job, |s| {
        s.content_id = req.content_id.clone();
        s.title_id = title_id.clone();
        s.route = Some(route);
        s.phase = Phase::Resolve;
        s.package_type = title_id
            .as_deref()
            .filter(|_| !category.trim().is_empty())
            .map(|t| package_type_for_verify(&category, t));
    });

    // resolve: what the console has. Could-not-read stays apart from not-installed: it never
    // refuses an install, and never lets the last resort through either.
    let installed = match &title_id {
        Some(t) => {
            let (m, t) = (mgmt.clone(), t.clone());
            unless_stopped(
                &state,
                &job,
                tokio::task::spawn_blocking(move || {
                    crate::pkg_install::read_installed_state(&m, &t)
                }),
            )
            .await
            .and_then(Result::ok)
            .unwrap_or(crate::pkg_install::InstalledState::Unknown)
        }
        None => crate::pkg_install::InstalledState::Unknown,
    };
    let already_installed = !matches!(installed, crate::pkg_install::InstalledState::Absent);
    if ended_by_stop(&state, &job, &req, started, None).await {
        return;
    }
    if let Some(t) = &title_id {
        if let Some((reason, hint)) = refusal_before_delivery(&req, &category, t, &mgmt).await {
            state.jobs.update(&job, |s| {
                s.phase = Phase::Failed;
                s.verdict = Some(Verdict::Failed);
                s.reason = Some(reason);
                s.hint = Some(hint.clone());
            });
            finalize(&state, &job, &req, started).await;
            return;
        }
    }
    // The destructive-reinstall guard, for what is known to be installed.
    let installed_ver = match &installed {
        crate::pkg_install::InstalledState::Installed(v) => Some(v.as_str()),
        _ => None,
    };
    let reapplied_update = patch_ver
        .as_deref()
        .zip(installed_ver)
        .is_some_and(|(p, i)| update_already_applied(p, i));
    let erases_game = installed_ver.is_some()
        && matches!(
            guard_decision(&category, true, req.options.allow_destructive_reinstall),
            GuardDecision::Refuse
        );
    if erases_game || (reapplied_update && !req.options.allow_destructive_reinstall) {
        state.jobs.update(&job, |s| {
            s.phase = Phase::Failed;
            s.verdict = Some(Verdict::Failed);
            s.reason = Some(FailReason::DestructiveGuard);
            s.replaces = Some(if erases_game { "game" } else { "update" }.into());
            s.hint = Some(if erases_game {
                "this would erase the installed game first; re-run with allow_destructive_reinstall".into()
            } else {
                "the game already has this update, and applying it again can undo it; re-run with allow_destructive_reinstall".into()
            });
        });
        finalize(&state, &job, &req, started).await;
        return;
    }

    // deliver: for a stream source, create a serve-only pkg-host session and
    // get its URL by reusing the existing start handler internally.
    let app_ver_before = match (&title_id, &patch_ver) {
        (Some(t), Some(_)) => {
            let (m, t) = (mgmt.clone(), t.clone());
            unless_stopped(
                &state,
                &job,
                tokio::task::spawn_blocking(move || {
                    crate::pkg_install::read_installed_app_ver(&m, &t)
                }),
            )
            .await
            .and_then(Result::ok)
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
    let elf_for_ensure = elf.clone();
    let ip_for_ensure = ip.clone();
    let ens = unless_stopped(
        &state,
        &job,
        tokio::task::spawn_blocking(move || {
            // Wait out an elfldr swap in progress rather than send into its gap.
            let lock = crate::elfldr_guard::loader_lock(&ip_for_ensure);
            let _loader = lock.lock().unwrap_or_else(|e| e.into_inner());
            ic::ensure(&ip_for_ensure, elf_for_ensure.as_deref(), false)
        }),
    )
    .await;
    if ended_by_stop(&state, &job, &req, started, None).await {
        return;
    }
    let ens = ens.and_then(Result::ok).unwrap_or(ic::Ensure {
        listening: false,
        sent: false,
        state: None,
        reason: Some(DPI_REASON_NO_BRINGUP),
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
        finalize(&state, &job, &req, started).await;
        return;
    }

    // build the install call per source.
    let hint_name = name_hint(title_id.as_deref());
    state.jobs.update(&job, |s| s.phase = Phase::Install);
    let mut served_from: Option<String> = None;
    let mut serve_failure: Option<FailReason> = None;
    let (reply, session_id, shortened): (Result<ic::InstallReply, String>, Option<String>, bool) =
        match &req.source {
            Source::ConsolePath(path) => {
                let (i, p, h) = (ip.clone(), path.clone(), hint_name.clone());
                // "Retry with Stream": the user already saw the console refuse its own
                // copy, so skip straight to serving it from this engine.
                let forced = req.options.force_stream;
                let r = if forced {
                    Ok(ic::InstallReply::Sony {
                        code: STAGED_ROUTE_REFUSALS[0],
                        hint: None,
                    })
                } else {
                    install_call(&state, &job, move || ic::install_path(&i, &p, &h)).await
                };
                // The console refused its own copy (see STAGED_ROUTE_REFUSALS;
                // not expected on FW 13.60 since daemon 1.3.9). Try once more
                // with the same bytes served from this engine — read back
                // through the helper, one job, one result.
                match staged_retry_through_engine(&r) {
                    Some(code) => {
                        crate::log_at!(Info, "install", 
                            "{tag}: the console refused its own copy (0x{code:08X}); serving it from this engine instead"
                        );
                        // A network refusal stays with Sony's library for the life of the
                        // process, so the next URL would fail the same way. The daemon no longer
                        // answers one by trying the bare path by itself; this is the way on.
                        if is_network_class(code) {
                            recycle_daemon(&ip).await;
                            let (i, e) = (ip.clone(), elf.clone());
                            let _ = tokio::task::spawn_blocking(move || {
                                let lock = crate::elfldr_guard::loader_lock(&i);
                                let _loader = lock.lock().unwrap_or_else(|e| e.into_inner());
                                ic::ensure(&i, e.as_deref(), false)
                            })
                            .await;
                        }
                        let console_url = format!("ps5://{ip}{path}");
                        match serve_session_unless_stopped(&state, &job, &req, &console_url).await {
                            Ok((sid, url)) => {
                                served_from = origin_of(&url);
                                route = Route::Stream;
                                state.jobs.update(&job, |s| s.route = Some(Route::Stream));
                                let (i, u, h) = (ip.clone(), url, hint_name.clone());
                                let r2 =
                                    install_call(&state, &job, move || ic::install_url(&i, &u, &h))
                                        .await;
                                (r2, Some(sid), false)
                            }
                            Err(e) => {
                                crate::log_at!(
                                    Warn,
                                    "install",
                                    "{tag}: could not serve the console's copy: {}",
                                    e.message
                                );
                                if forced {
                                    serve_failure = e.reason;
                                    (Err(e.message), None, false)
                                } else {
                                    (r, None, false)
                                }
                            }
                        }
                    }
                    None => (r, None, false),
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
                // "Stream through this computer": the console is never given the link.
                let proxied = req.options.proxy_link;
                let r = if proxied {
                    Ok(ic::InstallReply::Sony {
                        code: STAGED_ROUTE_REFUSALS[0],
                        hint: None,
                    })
                } else {
                    install_call(&state, &job, move || ic::install_url(&i, &final_url, &h)).await
                };
                // The console refused the link as its own server serves it
                // (once put down to a missing Last-Modified; on FW 13.60 the
                // refusal was the daemon's own call, fixed in 1.3.9). This
                // engine's link proxy is the second try either way.
                match &r {
                    Ok(ic::InstallReply::Sony { code, .. })
                        if STAGED_ROUTE_REFUSALS.contains(code) =>
                    {
                        if proxied {
                            crate::log_at!(
                                Info,
                                "install",
                                "{tag}: streaming the link through this engine"
                            );
                        } else {
                            crate::log_at!(Info, "install", 
                                "{tag}: the console refused the link (0x{code:08X}); proxying it through this engine instead"
                            );
                        }
                        match serve_session_unless_stopped(&state, &job, &req, url).await {
                            Ok((sid, purl)) => {
                                served_from = origin_of(&purl);
                                route = Route::Stream;
                                state.jobs.update(&job, |s| s.route = Some(Route::Stream));
                                let (i, u, h) = (ip.clone(), purl, hint_name.clone());
                                let r2 =
                                    install_call(&state, &job, move || ic::install_url(&i, &u, &h))
                                        .await;
                                (r2, Some(sid), false)
                            }
                            Err(e) => {
                                crate::log_at!(
                                    Warn,
                                    "install",
                                    "{tag}: could not proxy the link: {}",
                                    e.message
                                );
                                // When the proxy was the only thing asked for, its own
                                // failure is the answer, not a made-up refusal code.
                                if proxied {
                                    serve_failure = e.reason;
                                    (Err(e.message), None, was_short)
                                } else {
                                    (r, None, was_short)
                                }
                            }
                        }
                    }
                    _ => (r, None, was_short),
                }
            }
            Source::HostFile(_) | Source::Remote { .. } => {
                match create_serve_session(&state, &job, &req).await {
                    Ok((sid, url)) => {
                        served_from = origin_of(&url);
                        // No reach pre-check: hand the URL to the console and let it try. If it
                        // never fetches, the failure below explains why (firewall, host IP).
                        let (i, u, h) = (ip.clone(), url, hint_name.clone());
                        let r =
                            install_call(&state, &job, move || ic::install_url(&i, &u, &h)).await;
                        (r, Some(sid), false)
                    }
                    Err(e) => {
                        serve_failure = e.reason;
                        (Err(e.message), None, false)
                    }
                }
            }
        };
    if ended_by_stop(&state, &job, &req, started, session_id.as_deref()).await {
        return;
    }
    // The guarded last resort (see `path_fallback_allowed`): the console refused its own
    // copy and cannot fetch it from this engine, so offer Sony the plain file path.
    let (mut reply, mut session_id) = (reply, session_id);
    let mut via_plain_path = false;
    if let (Source::ConsolePath(path), Ok(ic::InstallReply::Sony { code, .. })) =
        (&req.source, &reply)
    {
        let fetched = match &session_id {
            Some(sid) => {
                let s = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
                s.get(sid).map_or(0, |x| x.bytes_served)
            }
            None => 0,
        };
        // Served from this engine and never fetched, or refused from the console with no
        // way to serve it at all.
        let unreachable = *code != 0
            && fetched == 0
            && (session_id.is_some() || STAGED_ROUTE_REFUSALS.contains(code));
        if path_fallback_allowed(
            req.options.console_path_fallback,
            &req.source,
            &category,
            title_id.is_some(),
            already_installed,
            unreachable,
        ) {
            crate::log_at!(Info, "install", 
                "{tag}: the console cannot fetch from this engine (0x{code:08X}); last resort: installing its own copy by file path"
            );
            if let Some(sid) = session_id.take() {
                crate::pkg_install::release_serve_session(&state.sessions, &sid);
            }
            let (i, p, h) = (ip.clone(), path.clone(), hint_name.clone());
            reply = install_call(&state, &job, move || ic::install_path_plain(&i, &p, &h)).await;
            if ended_by_stop(&state, &job, &req, started, None).await {
                return;
            }
            route = Route::Path;
            via_plain_path = true;
            state.jobs.update(&job, |s| s.route = Some(Route::Path));
        }
    }
    // A daemon older than 1.6.0 can fall back to the bare path by itself. Sony's "accepted" says
    // little on that route, so it gets the check the last resort gets.
    if let (Ok(ic::InstallReply::Accepted { via, .. }), Source::ConsolePath(_)) =
        (&reply, &req.source)
    {
        if session_id.is_none() && (via == "path" || via == "file") {
            crate::log_at!(
                Warn,
                "install",
                "{tag}: the daemon installed by file path ({via}) by itself"
            );
            route = Route::Path;
            via_plain_path = true;
            state.jobs.update(&job, |s| s.route = Some(Route::Path));
        }
    }
    let deliver_ms = deliver_started.elapsed().as_millis() as u64;

    // interpret the daemon reply.
    crate::log_at!(
        Debug,
        "install",
        "{tag}: the installer daemon answered after {deliver_ms} ms on route {route:?} (plain path {via_plain_path}): {reply:?}"
    );
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
        Ok(ic::InstallReply::SourceUnreadable) => (
            false,
            0,
            Some("the package could not be opened on the PS5; check it is still there".into()),
        ),
        Ok(ic::InstallReply::LoopbackFailed) => (
            false,
            0,
            Some("the installer could not serve the package on the PS5".into()),
        ),
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
        // How far the console got before it refused: the UI tells "refused at once" from
        // "refused after fetching it all", which point at different causes.
        let fetched = session_id.as_ref().and_then(|sid| {
            let s = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
            s.get(sid).map(|x| (x.bytes_served, x.total_size))
        });
        let never_fetched = fetched.is_some_and(|(served, _)| served == 0);
        let net_diag = if never_fetched && is_network_class(code) {
            match served_from.as_deref() {
                Some(o) => net_diag_for(&ip, o).await,
                None => None,
            }
        } else {
            None
        };
        let hint = if via_plain_path {
            Some(path_fallback_refused_hint(code))
        } else if never_fetched && is_network_class(code) {
            Some(stream_unreachable_hint(
                served_from.as_deref(),
                code,
                net_diag.as_ref(),
            ))
        } else {
            hint
        };
        if let Some(sid) = &session_id {
            if release_after_refusal(&reply) {
                crate::pkg_install::release_serve_session(&state.sessions, sid);
            }
        }
        // A link the console fetches by itself has no session and never counts as fetched, and it
        // poisons Sony's library just the same.
        let nothing_fetched = fetched.is_none_or(|(served, _)| served == 0);
        crate::log_at!(
            Info,
            "install",
            "{tag}: refused on route {route:?}: Sony code 0x{code:08X}, nothing fetched {nothing_fetched}, hint {hint:?}"
        );
        if should_recycle_daemon(route == Route::Stream, nothing_fetched, code) {
            crate::log_at!(Warn, "install", 
                "{tag}: network-class refusal 0x{code:08X} before any fetch — recycling the installer daemon so the next install starts clean"
            );
            recycle_daemon(&ip).await;
        }
        state.jobs.update(&job, |s| {
            s.phase = Phase::Failed;
            s.verdict = Some(Verdict::Failed);
            let reason = serve_failure
                .or_else(|| reason_from_reply(&reply))
                .unwrap_or_else(|| refusal_reason(never_fetched, code, route));
            s.retry_with_stream = stream_retry_offered(Some(reason), &req.source, &category, true);
            s.reason = Some(reason);
            s.code = code;
            s.hint = hint.clone();
            s.net_diag = net_diag.clone();
            s.shortened = shortened;
            s.metrics.sony_rc = code;
            if let Some((served, total)) = fetched {
                s.metrics.served_bytes = served;
                s.metrics.total_bytes = total;
            }
            s.metrics.phase_ms.insert("deliver".into(), deliver_ms);
        });
        finalize(&state, &job, &req, started).await;
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
    // The file behind the link changed: reading on would only hand the console errors until it gave
    // up, and the job would read as a stall.
    let origin_changed = || {
        session_id.as_ref().is_some_and(|sid| {
            let s = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
            s.get(sid)
                .and_then(|x| x.remote.as_ref())
                .is_some_and(|r| r.origin_changed())
        })
    };
    let delivery = if !observable {
        DeliveryProgress::Complete
    } else {
        loop {
            // (served, covered, total, daemon job phase). A stream session's
            // completion is judged on distinct bytes received, never the raw
            // sum: Sony re-reads the header, and on a 37.6 MB package ten
            // re-reads made the sum hit the package size with 640 KiB never
            // fetched. Releasing the session then answered the console's
            // request for it with 410 and the PS5 download failed. Any served
            // request, re-read or not, still counts as movement.
            let obs: Option<(u64, u64, u64, Option<String>)> = if let Some(sid) = &session_id {
                let s = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
                s.get(sid)
                    .map(|x| (x.bytes_served, x.transfer.bytes(), x.total_size, None))
            } else if let Some(jid) = &install_job_id {
                let (i, j) = (ip.clone(), jid.clone());
                tokio::task::spawn_blocking(move || ic::job(&i, &j))
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .map(|j| (j.bytes_served, j.bytes_served, j.total, Some(j.phase)))
            } else {
                None
            };
            // An unreadable observation counts as "no movement": it can only
            // run the stall clock, never declare success.
            let (served, covered, total, dphase) = obs.unwrap_or((last_served, 0, 0, None));
            if served > last_served {
                last_served = served;
                last_move = std::time::Instant::now();
            }
            state.jobs.update(&job, |s| {
                s.metrics.served_bytes = covered;
                if total > 0 {
                    s.metrics.total_bytes = total;
                }
            });
            let p = if let Some(p) = dphase.as_deref().and_then(daemon_phase_progress) {
                p
            } else {
                delivery_progress(
                    covered,
                    total,
                    last_move.elapsed().as_millis() as u64,
                    DELIVERY_STALL_MS,
                )
            };
            if p != DeliveryProgress::Pending {
                break p;
            }
            if state.jobs.is_cancelled(&job) {
                break DeliveryProgress::Cancelled;
            }
            if origin_changed() {
                break DeliveryProgress::Stalled;
            }
            let round = tokio::time::sleep(std::time::Duration::from_secs(2));
            let _ = unless_stopped(&state, &job, round).await;
        }
    };
    // A stall because the file behind the link changed fails with that reason. It is a stall all
    // the same for the session below: a 410 would still make the console delete its download.
    let link_changed = delivery == DeliveryProgress::Stalled && origin_changed();
    // Whether the console may still come back for the stream source decides what happens to its
    // session (see `session_after_delivery`). Its counters stay readable for the metrics below.
    if let Some(sid) = &session_id {
        match session_after_delivery(delivery) {
            SessionAfterDelivery::Release => {
                crate::pkg_install::release_serve_session(&state.sessions, sid);
            }
            SessionAfterDelivery::ReleaseWhenQuiet => release_when_quiet(&state, sid),
            SessionAfterDelivery::Keep => {}
        }
    }
    let deliver_ms = deliver_started.elapsed().as_millis() as u64;
    if delivery == DeliveryProgress::Cancelled {
        // Stopped by the user, often after cancelling the download on the PS5, which this
        // engine cannot see (the console just stops asking for data). The stream source is
        // already released above; a daemon still serving a staged copy is recycled so the
        // next install is not refused as busy.
        crate::log_at!(
            Info,
            "install",
            "{tag}: stopped by the user at {last_served} bytes"
        );
        if session_id.is_none() && install_job_id.is_some() {
            recycle_daemon(&ip).await;
        }
        state.jobs.update(&job, |s| {
            s.phase = Phase::Failed;
            s.verdict = Some(Verdict::Failed);
            s.reason = Some(FailReason::Cancelled);
            s.hint = Some(STOPPED_HINT.into());
            s.shortened = shortened;
            s.metrics.phase_ms.insert("deliver".into(), deliver_ms);
        });
        finalize(&state, &job, &req, started).await;
        return;
    }
    if link_changed {
        crate::log_at!(
            Warn,
            "install",
            "{tag}: the file behind the link changed at {last_served} bytes"
        );
        state.jobs.update(&job, |s| {
            s.phase = Phase::Failed;
            s.verdict = Some(Verdict::Failed);
            s.reason = Some(FailReason::OriginChanged);
            s.hint = Some(crate::remote_pkg::ORIGIN_CHANGED.into());
            s.shortened = shortened;
            s.metrics.phase_ms.insert("deliver".into(), deliver_ms);
        });
        finalize(&state, &job, &req, started).await;
        return;
    }
    if delivery == DeliveryProgress::Stalled {
        crate::log_at!(
            Warn,
            "install",
            "{tag}: delivery stalled at {last_served} bytes — source kept, reporting failed"
        );
        // A daemon serving this job is left alone too: it gives way to the next install by itself
        // once the job has been idle, and until then a paused download can still resume.
        // A stream the console never fetched a single byte of is not a stall:
        // it could not reach this computer, so give the same guidance as the
        // other unreachable paths (host IP, firewall) rather than "stopped
        // fetching".
        let never_reached = route == Route::Stream && session_id.is_some() && last_served == 0;
        // Sony took the URL and then never fetched a byte: the same dead end as a refusal,
        // so the same guarded last resort.
        let mut plain_path_refusal: Option<u32> = None;
        if let Source::ConsolePath(path) = &req.source {
            if !via_plain_path
                && path_fallback_allowed(
                    req.options.console_path_fallback,
                    &req.source,
                    &category,
                    title_id.is_some(),
                    already_installed,
                    never_reached,
                )
            {
                crate::log_at!(Info, "install", 
                    "{tag}: the console never fetched from this engine; last resort: installing its own copy by file path"
                );
                if let Some(sid) = &session_id {
                    crate::pkg_install::release_serve_session(&state.sessions, sid);
                }
                let (i, p, h) = (ip.clone(), path.clone(), hint_name.clone());
                let r =
                    install_call(&state, &job, move || ic::install_path_plain(&i, &p, &h)).await;
                if ended_by_stop(&state, &job, &req, started, None).await {
                    return;
                }
                via_plain_path = true;
                match r {
                    Ok(ic::InstallReply::Accepted { .. }) => {
                        route = Route::Path;
                        state.jobs.update(&job, |s| s.route = Some(Route::Path));
                    }
                    Ok(ic::InstallReply::Sony { code, .. }) => plain_path_refusal = Some(code),
                    _ => plain_path_refusal = Some(0),
                }
            }
        }
        let recovered = via_plain_path && route == Route::Path && plain_path_refusal.is_none();
        if !recovered {
            let stall_diag = match (never_reached, served_from.as_deref()) {
                (true, Some(o)) => net_diag_for(&ip, o).await,
                _ => None,
            };
            let stall_hint = if let Some(code) = plain_path_refusal {
                path_fallback_refused_hint(code)
            } else if never_reached {
                stream_unreachable_hint(served_from.as_deref(), 0, stall_diag.as_ref())
            } else {
                "the console has not asked for the package for 5 minutes; this engine goes on serving it, so a paused download can still resume, and the package was kept"
                .into()
            };
            state.jobs.update(&job, |s| {
                s.phase = Phase::Failed;
                s.verdict = Some(Verdict::Failed);
                s.reason = Some(if never_reached {
                    FailReason::StreamUnreachable
                } else {
                    FailReason::Stalled
                });
                s.hint = Some(stall_hint.clone());
                s.net_diag = stall_diag.clone();
                s.shortened = shortened;
                s.metrics.phase_ms.insert("deliver".into(), deliver_ms);
            });
            finalize(&state, &job, &req, started).await;
            return;
        }
    }

    // verify.
    state.jobs.update(&job, |s| s.phase = Phase::Verify);
    let verify_started = std::time::Instant::now();
    // Whether the console is known to hold the whole package now. A bare link is fetched by the
    // console itself, so nothing here saw any of it.
    let delivered = observable && delivery == DeliveryProgress::Complete;
    // An add-on the check found nothing of: worded as such (see `ADDON_UNVERIFIED_HINT`).
    let mut addon_not_seen = false;
    let (verdict, patch_verdict, app_ver_after) = match (&title_id, &patch_ver) {
        (Some(t), Some(pv)) => {
            let (m, t, pv, before) = (mgmt.clone(), t.clone(), pv.clone(), app_ver_before.clone());
            let total = state.jobs.get(&job).map_or(0, |s| s.metrics.total_bytes);
            let budget = crate::pkg_install::patch_verify_budget(total);
            let (st, jb) = (state.clone(), job.clone());
            let check = tokio::task::spawn_blocking(move || {
                crate::pkg_install::verify_patch_after_install(
                    &m,
                    &t,
                    before.as_deref(),
                    &pv,
                    budget,
                    &|| st.jobs.is_cancelled(&jb),
                )
            })
            .await
            .unwrap_or(crate::pkg_install::PatchCheck {
                verdict: ps5upload_core::patch_verify::PatchVerdict::Inconclusive,
                after: None,
                timed_out: false,
            });
            (
                verdict_after_wait(check.verdict, check.timed_out, delivered),
                Some(status::PatchVerdict::from(check.verdict)),
                check.after,
            )
        }
        // A base game or add-on: Sony's failures arrive after the bytes, so the whole package
        // reaching the console says little. The artifact on a drive does.
        (Some(t), None) if observable && !req.content_id.trim().is_empty() => {
            let (m, st, jb) = (mgmt.clone(), state.clone(), job.clone());
            let cid = req.content_id.clone();
            let ptype = package_type_for_verify(&category, t);
            let total = state.jobs.get(&job).map_or(0, |s| s.metrics.total_bytes);
            let budget = crate::pkg_install::patch_verify_budget(total);
            let console_copy = match &req.source {
                Source::ConsolePath(p) => Some(p.clone()),
                _ => None,
            };
            let sid = session_id.clone();
            let addon = is_addon_type(&ptype);
            // Say what the 99% waits for, so a report settles it from the log (#776).
            crate::log_at!(
                Info,
                "install",
                "{tag}: verify: waiting for {} ({ptype}, {cid}): {}",
                if addon {
                    "the add-on"
                } else {
                    "the package file"
                },
                verify_target_text(&cid, &ptype, t, budget)
            );
            let (cid_log, ptype_log) = (cid.clone(), ptype.clone());
            let found = tokio::task::spawn_blocking(move || {
                let fingerprint = console_copy
                    .map(|p| crate::pkg_install::remote_pkg_identity(&m, &p).1)
                    .or_else(|| {
                        let sid = sid?;
                        let s = st.sessions.lock().unwrap_or_else(|e| e.into_inner());
                        s.get(&sid).map(|x| x.package_fingerprint.clone())
                    })
                    .unwrap_or_default();
                let stop = || st.jobs.is_cancelled(&jb);
                if addon {
                    crate::pkg_install::wait_for_installed_addon(
                        &m,
                        &cid,
                        &ptype,
                        total,
                        &fingerprint,
                        &stop,
                    )
                } else {
                    crate::pkg_install::wait_for_installed_artifact(
                        &m,
                        &cid,
                        &ptype,
                        total,
                        &fingerprint,
                        budget,
                        &stop,
                    )
                }
            })
            .await
            .ok();
            let outcome = found.map_or(crate::pkg_install::ArtifactWait::CannotTell, |r| r.outcome);
            match &found {
                Some(r) => crate::log_at!(
                    Info,
                    "install",
                    "{tag}: verify: {outcome:?} for {ptype_log} {cid_log} after {} ms, {} checks; last check {}, title files {} bytes{}",
                    r.waited_ms,
                    r.checks,
                    r.last_check,
                    r.most_seen,
                    if r.hit_ceiling { ", ceiling reached" } else { "" }
                ),
                None => crate::log_at!(
                    Warn,
                    "install",
                    "{tag}: verify: the check for {ptype_log} {cid_log} did not run to the end"
                ),
            }
            (
                match outcome {
                    crate::pkg_install::ArtifactWait::Landed
                    | crate::pkg_install::ArtifactWait::CannotTell => verdict_no_identity(true),
                    crate::pkg_install::ArtifactWait::NotSeen => {
                        if addon {
                            addon_not_seen = true;
                        }
                        Verdict::Unverified
                    }
                },
                None,
                None,
            )
        }
        // Nothing to check the install against: the whole package reaching the console is the
        // best word there is, and of a bare link not even that.
        _ if observable => (verdict_no_identity(true), None, None),
        _ => (Verdict::Unverified, None, None),
    };
    if ended_by_stop(&state, &job, &req, started, None).await {
        return;
    }
    // The last resort is the one route with nothing to observe: no bytes served, no session.
    // Sony's "accepted" is not proof there: one form of it returned 0 and left a tile with no
    // content. So this route is only "installed" once the content is on a drive.
    let mut no_content_hint: Option<String> = None;
    let verdict =
        match (&title_id, via_plain_path && route == Route::Path) {
            (Some(t), true) => {
                let (m, t) = (mgmt.clone(), t.clone());
                let (st, jb) = (state.clone(), job.clone());
                let present = tokio::task::spawn_blocking(move || {
                    let deadline = std::time::Instant::now() + PLAIN_PATH_CONTENT_WAIT;
                    loop {
                        if crate::pkg_install::base_content_present(&m, &t) {
                            return true;
                        }
                        if std::time::Instant::now() >= deadline || st.jobs.is_cancelled(&jb) {
                            return false;
                        }
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                })
                .await
                .unwrap_or(false);
                if ended_by_stop(&state, &job, &req, started, None).await {
                    return;
                }
                if present {
                    verdict
                } else {
                    crate::log_at!(Warn, "install", 
                    "{tag}: the console accepted the package by file path but no content appeared"
                );
                    no_content_hint = Some(PLAIN_PATH_NO_CONTENT_HINT.to_string());
                    Verdict::Failed
                }
            }
            _ => verdict,
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
        s.phase = if no_content_hint.is_some() {
            Phase::Failed
        } else {
            Phase::Done
        };
        s.verdict = Some(verdict);
        if let Some(h) = &no_content_hint {
            s.reason = Some(FailReason::SonyRefused);
            s.hint = Some(h.clone());
        }
        if verdict == Verdict::Unverified {
            s.hint = Some(if addon_not_seen && delivered {
                ADDON_UNVERIFIED_HINT.to_string()
            } else {
                unverified_hint(delivered, verify_ms)
            });
        }
        s.patch_verdict = patch_verdict;
        s.app_ver_after = app_ver_after.clone();
        s.shortened = shortened;
        s.metrics.total_bytes = total;
        s.metrics.served_bytes = served;
        s.metrics.throughput_mbps = status::throughput_mbps(served, deliver_ms);
        s.metrics.phase_ms.insert("deliver".into(), deliver_ms);
        s.metrics.phase_ms.insert("verify".into(), verify_ms);
    });
    finalize(&state, &job, &req, started).await;
}

/// Why this engine cannot call an install finished: said to anyone reading the status directly
/// (the app words its own message from the verdict and the time waited).
fn unverified_hint(delivered: bool, waited_ms: u64) -> String {
    if delivered {
        format!(
            "the PS5 has the whole package and it had not shown up installed after {} s; it may still be installing, so the package was kept",
            waited_ms / 1000
        )
    } else {
        "the PS5 took the link and downloads it by itself; check Downloads on the PS5".to_string()
    }
}

/// An add-on the console has all of, with nothing of it found on a drive (#776). An add-on that
/// only unlocks content leaves no files, so this is no sign of a failure.
pub const ADDON_UNVERIFIED_HINT: &str = "the PS5 has the whole add-on, but no files of it were found to confirm it installed; an add-on that only unlocks content leaves none, so check the game's add-ons on the PS5. The package was kept";

/// The package type is an add-on (`PS5AC`, `PS4AC`).
pub fn is_addon_type(package_type: &str) -> bool {
    package_type.to_ascii_uppercase().ends_with("AC")
}

/// What the verify step looks for, in words for the log.
pub fn verify_target_text(
    content_id: &str,
    package_type: &str,
    title_id: &str,
    budget: std::time::Duration,
) -> String {
    if is_addon_type(package_type) {
        let label = crate::pkg_install::addon_label(content_id).unwrap_or_else(|| "?".into());
        format!(
            "an ac.pkg with this content id under <drive>/user/addcont/{title_id}/, or {} (idle {} s, at most {} s)",
            crate::pkg_install::addon_appmeta_path(title_id, &label),
            crate::pkg_install::ADDON_VERIFY_IDLE.as_secs(),
            crate::pkg_install::ADDON_VERIFY_CEILING.as_secs()
        )
    } else {
        let file = match package_type.to_ascii_uppercase() {
            p if p.ends_with("DP") => format!("<drive>/user/patch/{title_id}/patch.pkg"),
            _ => format!("<drive>/user/app/{title_id}/app.pkg"),
        };
        format!(
            "{file} matching this package (idle {} s, at most 4 h)",
            budget.as_secs()
        )
    }
}

/// The category to go on when the caller's says full game and the package's own header says
/// add-on. A PS5 add-on's param.json has no category, so a client that derives the package type
/// from it sends `PS5GD` (#724). Kept past the image check, that sent the add-on's verify looking
/// for a game's `app.pkg` it never writes, at 99% for the whole wait (#776). The caller's form is
/// kept: `PS5GD` becomes `PS5AC`, `gd` becomes `ac`. `None`: keep what was sent.
pub fn category_from_header(
    sent: &str,
    header: Option<&ps5upload_pkg::ReaderMetadata>,
) -> Option<String> {
    let h = header?;
    if !certainly_a_game(sent) {
        return None;
    }
    let (kind, _, _) = ps5upload_pkg::kind::classify(h.content_type, h.content_flags, &h.category);
    if kind != ps5upload_pkg::kind::Kind::Dlc {
        return None;
    }
    let s = sent.trim();
    let upper = s.to_ascii_uppercase();
    Some(if upper.starts_with("PS5") || upper.starts_with("PS4") {
        format!("{}AC", &upper[..3])
    } else {
        "ac".to_string()
    })
}

/// Reuse the existing start handler in serve-only mode to create a pkg-host
/// session for a host/remote source, returning (session_id, url).
async fn create_serve_session(
    state: &PkgInstallStateHandle,
    job: &str,
    req: &InstallRequest,
) -> Result<(String, String), ServeFailure> {
    let path = match &req.source {
        Source::HostFile(p) => p.clone(),
        Source::Remote { connection, path } => format!("remote://{connection}/{path}"),
        _ => {
            return Err(ServeFailure::plain(
                "create_serve_session called for a non-stream source",
            ))
        }
    };
    serve_session_unless_stopped(state, job, req, &path).await
}

/// `create_serve_session_for`, given up on as soon as the user stops `job`. Setting a session up can
/// take a while (a link's first requests, a server's connection). It runs on: a session that still
/// comes up is released, or it would hold the package as "already running".
async fn serve_session_unless_stopped(
    state: &PkgInstallStateHandle,
    job: &str,
    req: &InstallRequest,
    path: &str,
) -> Result<(String, String), ServeFailure> {
    let start_req = serve_session_request(req, path);
    let st = state.clone();
    let mut task = tokio::spawn(async move { create_serve_session_for(&st, start_req).await });
    tokio::select! {
        biased;
        _ = state.jobs.stop_requested(job) => {
            let st = state.clone();
            tokio::spawn(async move {
                if let Ok(Ok((sid, _))) = task.await {
                    crate::pkg_install::release_serve_session(&st.sessions, &sid);
                }
            });
            Err(ServeFailure::plain("stopped by the user"))
        }
        out = &mut task => out.unwrap_or_else(|e| {
            Err(ServeFailure::plain(format!("serve session task failed: {e}")))
        }),
    }
}

/// A serve session for an explicit source path — a local file, `remote://…`
/// on a saved server, or `ps5://<console>/<path>` read back through the helper.
/// The serve-session request for `path`: a link is proxied (its origin read over many
/// connections, with the caller's certificate choice); anything else is a path the engine
/// reads, local, `remote://` or `ps5://`.
fn serve_session_request(req: &InstallRequest, path: &str) -> serde_json::Value {
    let is_link = path.starts_with("http://") || path.starts_with("https://");
    if is_link {
        serde_json::json!({
            "ps5_addr": req.ps5_addr,
            "serve_only": true,
            "remote_url": path,
            "content_id": req.content_id,
            "insecure_tls": req.options.insecure_tls,
        })
    } else {
        serde_json::json!({
            "ps5_addr": req.ps5_addr,
            "serve_only": true,
            "path": path,
            "content_id": req.content_id,
        })
    }
}

/// Why this engine could not set up a stream session: what to tell the user, in plain text rather
/// than the handler's JSON, and which reason that is.
pub struct ServeFailure {
    reason: Option<FailReason>,
    message: String,
}

impl ServeFailure {
    fn plain(message: impl Into<String>) -> Self {
        Self {
            reason: None,
            message: message.into(),
        }
    }

    /// From the start handler's refusal.
    fn from_response(status: StatusCode, body: &[u8]) -> Self {
        let text = String::from_utf8_lossy(body);
        let message = serde_json::from_slice::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| text.into_owned());
        let reason = if status == StatusCode::CONFLICT {
            FailReason::Busy
        } else if message.contains("isn't installed on this PS5") {
            FailReason::BaseMissing
        } else if status == StatusCode::BAD_REQUEST {
            FailReason::SourceGone
        } else {
            FailReason::BadRequest
        };
        Self {
            reason: Some(reason),
            message,
        }
    }
}

async fn create_serve_session_for(
    state: &PkgInstallStateHandle,
    start_req: serde_json::Value,
) -> Result<(String, String), ServeFailure> {
    let start_req: crate::pkg_install::InstallStartRequest = serde_json::from_value(start_req)
        .map_err(|e| ServeFailure::plain(format!("build start request: {e}")))?;
    let resp =
        crate::pkg_install::install_start_handler(State(state.clone()), Json(start_req)).await;
    let (parts, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|e| ServeFailure::plain(format!("read start response: {e}")))?;
    if !parts.status.is_success() {
        return Err(ServeFailure::from_response(parts.status, &bytes));
    }
    let start: crate::pkg_install::InstallStartResponse = serde_json::from_slice(&bytes)
        .map_err(|e| ServeFailure::plain(format!("parse start response: {e}")))?;
    Ok((start.session_id, start.url))
}

/// A package file on this computer's header, for what the caller did not say about it.
fn read_host_header(path: &str) -> Option<ps5upload_pkg::ReaderMetadata> {
    use std::io::{Read, Seek, SeekFrom};
    let file = std::cell::RefCell::new(std::fs::File::open(path).ok()?);
    ps5upload_pkg::metadata_from_reader(|off, len| {
        let mut f = file.borrow_mut();
        f.seek(SeekFrom::Start(off)).ok()?;
        let mut buf = Vec::new();
        (&mut *f).take(len).read_to_end(&mut buf).ok()?;
        Some(buf)
    })
}

/// The package is a full game, known from its category: a PARAM one (`gd`, `gde`, ...) or the
/// BGFT package type the client sends in its place (`PS5GD`). Anything else, unknown or not
/// sent, is not a game for a refusal: an add-on whose category the caller didn't send was
/// refused as one whenever its base game ran from an image (#721).
fn certainly_a_game(category: &str) -> bool {
    let c = category.trim().to_ascii_lowercase();
    let c = c
        .strip_prefix("ps4")
        .or_else(|| c.strip_prefix("ps5"))
        .unwrap_or(&c);
    matches!(
        ps5upload_pkg::kind::classify_category(c),
        (ps5upload_pkg::kind::Kind::Base, true, _)
    )
}

/// Whether the image-or-folder refusal applies: the category says a full game, and the
/// package's own header, when it could be read, does not say otherwise. A PS5 add-on's
/// param.json has no category, so only its header says add-on, and a client that sent the
/// package type it derived (`PS5GD`) had the add-on refused as the game (#724).
fn image_refusal_applies(category: &str, header: Option<&ps5upload_pkg::ReaderMetadata>) -> bool {
    if !certainly_a_game(category) {
        return false;
    }
    match header {
        Some(h) => {
            ps5upload_pkg::kind::classify(h.content_type, h.content_flags, &h.category).0
                == ps5upload_pkg::kind::Kind::Base
        }
        None => true,
    }
}

#[cfg(test)]
mod certainly_a_game_tests {
    #[test]
    fn only_a_known_game_category_is_a_game() {
        assert!(super::certainly_a_game("gd"));
        assert!(super::certainly_a_game("GDE"));
        assert!(super::certainly_a_game("PS5GD"));
        assert!(super::certainly_a_game("PS4GD"));
        assert!(!super::certainly_a_game("ac"));
        assert!(!super::certainly_a_game("PS5AC"));
        assert!(!super::certainly_a_game("gp"));
        assert!(!super::certainly_a_game("PS5DP"));
        assert!(!super::certainly_a_game("dp"));
        // Not sent or not known: an add-on is never refused as a game (#721, #724).
        assert!(!super::certainly_a_game(""));
        assert!(!super::certainly_a_game("  "));
        assert!(!super::certainly_a_game("la"));
        assert!(!super::certainly_a_game("PS5LA"));
        assert!(!super::certainly_a_game("zz"));
    }

    /// A PS5 package as a FIH envelope around a CNT with no param category, as Sony's are.
    fn ps5_pkg(content_type: u32, content_flags: u32) -> Vec<u8> {
        let base = 0x1000usize;
        let mut pkg = vec![0u8; base + 0x100];
        pkg[0..4].copy_from_slice(&ps5upload_pkg::PKG_MAGIC_FIH.to_be_bytes());
        pkg[6..8].copy_from_slice(&3u16.to_le_bytes());
        pkg[0x58..0x60].copy_from_slice(&(base as u64).to_le_bytes());
        let cnt = &mut pkg[base..];
        cnt[0..4].copy_from_slice(&ps5upload_pkg::PKG_MAGIC.to_be_bytes());
        let cid = b"UP0000-PPSA01234_00-DELUXEUPGRADE000";
        cnt[0x40..0x40 + cid.len()].copy_from_slice(cid);
        cnt[0x74..0x78].copy_from_slice(&content_type.to_be_bytes());
        cnt[0x78..0x7C].copy_from_slice(&content_flags.to_be_bytes());
        pkg
    }

    fn header_of(bytes: Vec<u8>) -> ps5upload_pkg::ReaderMetadata {
        ps5upload_pkg::metadata_from_reader(move |off, len| {
            let (o, l) = (off as usize, len as usize);
            bytes.get(o..(o + l).min(bytes.len())).map(<[u8]>::to_vec)
        })
        .expect("a PS5 package header")
    }

    const ADD_ON: (u32, u32) = (0x21, 0x0a02_0000);
    const GAME: (u32, u32) = (0x20, 0x0a02_0000);

    /// #724, on every route the title's image is registered: what each route's caller sends as
    /// the category, with the package's own header. An add-on, or a package whose category is
    /// not known, is never refused as the game; a full game still is.
    #[test]
    fn only_a_full_game_is_refused_for_an_image_on_every_route() {
        use super::image_refusal_applies as refused;
        let add_on = header_of(ps5_pkg(ADD_ON.0, ADD_ON.1));
        let game = header_of(ps5_pkg(GAME.0, GAME.1));
        assert_eq!(add_on.category, "ac");
        assert_eq!(game.category, "gd");

        // Stream from this computer (host_file): the client sends the probe's package type,
        // which for a PS5 add-on is PS5GD. Upload then install / staged (console_path), a saved
        // server (remote) and a link (url) send the same field, or nothing.
        for sent in ["PS5GD", "gd"] {
            assert!(!refused(sent, Some(&add_on)), "add-on sent as {sent}");
            assert!(refused(sent, Some(&game)), "game sent as {sent}");
        }
        // The header read fills a missing category itself: an add-on's is ac.
        assert!(!refused(&add_on.category, Some(&add_on)));
        assert!(refused(&game.category, Some(&game)));
        // Category ac: never refused, whatever the header.
        for sent in ["ac", "PS5AC"] {
            assert!(!refused(sent, Some(&add_on)));
            assert!(!refused(sent, Some(&game)));
            assert!(!refused(sent, None));
        }
        // No category or an unknown one: never refused.
        for sent in ["", "  ", "la", "PS5LA", "zz"] {
            assert!(!refused(sent, Some(&game)), "{sent:?}");
            assert!(!refused(sent, None), "{sent:?}");
        }
        // The header unreadable: a category that says game is all there is, so it stands.
        assert!(refused("PS5GD", None));
        assert!(refused("gd", None));
        // A patch's header is not the game either.
        let patch = header_of(ps5_pkg(0x20, 0x4000_0000));
        assert!(!refused("PS5GD", Some(&patch)));
    }

    /// #776: past the image check, an add-on sent as PS5GD went on being a game, and its verify
    /// waited for a game's app.pkg it never writes. The header puts it right, in the caller's form.
    #[test]
    fn an_add_on_sent_as_a_game_is_verified_as_an_add_on() {
        use super::category_from_header as fix;
        let add_on = header_of(ps5_pkg(ADD_ON.0, ADD_ON.1));
        let game = header_of(ps5_pkg(GAME.0, GAME.1));
        let patch = header_of(ps5_pkg(0x20, 0x4000_0000));
        assert_eq!(fix("PS5GD", Some(&add_on)).as_deref(), Some("PS5AC"));
        assert_eq!(fix("ps4gd", Some(&add_on)).as_deref(), Some("PS4AC"));
        assert_eq!(fix("gd", Some(&add_on)).as_deref(), Some("ac"));
        // What it fixes is what the verify step goes on.
        let ptype =
            super::package_type_for_verify(&fix("PS5GD", Some(&add_on)).unwrap(), "PPSA11386");
        assert_eq!(ptype, "PS5AC");
        assert!(super::is_addon_type(&ptype));
        assert!(!super::is_addon_type(&super::package_type_for_verify(
            "PS5GD",
            "PPSA11386"
        )));
        // A game stays a game; an unread header or a category that is not a game changes nothing.
        assert_eq!(fix("PS5GD", Some(&game)), None);
        assert_eq!(fix("PS5GD", None), None);
        assert_eq!(fix("PS5AC", Some(&add_on)), None);
        assert_eq!(fix("", Some(&add_on)), None);
        // Patches are out of scope here: their own check reads the version.
        assert_eq!(fix("PS5GD", Some(&patch)), None);
    }

    /// The log says what the 99% waits for (#776).
    #[test]
    fn the_verify_target_is_named() {
        let d = std::time::Duration::from_secs(240);
        let a = super::verify_target_text(
            "EP3969-PPSA11386_00-007FLCOSTUME0001",
            "PS5AC",
            "PPSA11386",
            d,
        );
        assert!(a.contains("/user/addcont/PPSA11386/"), "{a}");
        assert!(
            a.contains("/user/appmeta/addcont/PPSA11386/007FLCOSTUME0001"),
            "{a}"
        );
        let g = super::verify_target_text(
            "EP3969-PPSA11386_00-0000000000000000",
            "PS5GD",
            "PPSA11386",
            d,
        );
        assert!(
            g.contains("/user/app/PPSA11386/app.pkg") && g.contains("240 s"),
            "{g}"
        );
        let p = super::verify_target_text(
            "EP3969-PPSA11386_00-0000000000000000",
            "PS5DP",
            "PPSA11386",
            d,
        );
        assert!(p.contains("/user/patch/PPSA11386/patch.pkg"), "{p}");
    }

    /// The stream route reads the header from the file on this computer.
    #[tokio::test]
    async fn the_stream_route_reads_the_add_on_header_from_the_file_here() {
        let dir = std::env::temp_dir().join(format!("ps5u-724-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut verdicts = Vec::new();
        for (name, (t, f)) in [("addon.pkg", ADD_ON), ("game.pkg", GAME)] {
            let path = dir.join(name);
            std::fs::write(&path, ps5_pkg(t, f)).unwrap();
            let source = super::Source::HostFile(path.to_string_lossy().into_owned());
            let h = super::read_source_header(&source, "192.0.2.1:9113", false).await;
            assert!(h.is_some(), "{name} header read");
            verdicts.push(super::image_refusal_applies("PS5GD", h.as_ref()));
            let _ = std::fs::remove_file(&path);
        }
        let _ = std::fs::remove_dir(&dir);
        assert_eq!(
            verdicts,
            [false, true],
            "the add-on installs, the game is refused"
        );
    }
}

/// The package's header, read from wherever the install takes it. Asked for only when a
/// refusal is about to be given, so it costs a few ranged reads on that path alone.
async fn read_source_header(
    source: &Source,
    mgmt: &str,
    insecure_tls: bool,
) -> Option<ps5upload_pkg::ReaderMetadata> {
    match source {
        Source::HostFile(path) => {
            let p = path.clone();
            tokio::task::spawn_blocking(move || read_host_header(&p))
                .await
                .ok()
                .flatten()
        }
        Source::ConsolePath(path) => {
            let (m, p) = (mgmt.to_string(), path.clone());
            tokio::task::spawn_blocking(move || {
                ps5upload_pkg::metadata_from_reader(|off, len| {
                    ps5upload_core::fs_ops::fs_read(&m, &p, off, len).ok()
                })
            })
            .await
            .ok()
            .flatten()
        }
        Source::Remote { connection, path } => {
            let remote = format!("remote://{connection}/{path}");
            crate::pkg_install::open_remote_package(&remote)
                .await
                .ok()
                .map(|(_, _, h, _, _)| h)
        }
        Source::Url(url) => {
            let u = url.clone();
            tokio::task::spawn_blocking(move || {
                let probe =
                    crate::remote_pkg::RemoteSource::probe_with_options(&u, insecure_tls).ok()?;
                let remote = crate::remote_pkg::RemoteSource::new_with_options(
                    u,
                    probe.total_size,
                    insecure_tls,
                );
                ps5upload_pkg::metadata_from_reader(|off, len| {
                    if len == 0 {
                        return Some(Vec::new());
                    }
                    remote.read_range(off, off + len - 1).ok()
                })
            })
            .await
            .ok()
            .flatten()
        }
    }
}

/// What refuses an install before anything reaches the console, when it is certain. Reads that
/// fail refuse nothing.
async fn refusal_before_delivery(
    req: &InstallRequest,
    category: &str,
    title_id: &str,
    mgmt: &str,
) -> Option<(FailReason, String)> {
    if certainly_a_game(category) {
        let (m, t) = (mgmt.to_string(), title_id.to_string());
        let mounted = tokio::task::spawn_blocking(move || {
            ps5upload_core::fs_ops::app_list_registered(&m)
                .map(|l| {
                    l.apps
                        .iter()
                        .any(|a| a.title_id == t && (a.image_backed || !a.src.is_empty()))
                })
                .unwrap_or(false)
        })
        .await
        .unwrap_or(false);
        if mounted {
            let header = read_source_header(&req.source, mgmt, req.options.insecure_tls).await;
            if image_refusal_applies(category, header.as_ref()) {
                return Some((
                    FailReason::ImageMounted,
                    format!("{title_id} is registered on the PS5 from a game image or folder; move that off the scan folder first"),
                ));
            }
            crate::log_at!(
                Info,
                "install",
                "{title_id} runs from an image or folder, but this package's header says it is not the full game (category {category:?}); installing"
            );
        }
    }
    if let Some(kind) = base_check_applies(category, title_id) {
        if !req.content_id.trim().is_empty() {
            let (m, cid) = (mgmt.to_string(), req.content_id.clone());
            let refusal = tokio::task::spawn_blocking(move || {
                ps5upload_core::pkg_install::preflight_patch_install(&m, &cid, kind)
            })
            .await
            .unwrap_or(None);
            if let Some(message) = refusal {
                return Some((FailReason::BaseMissing, message));
            }
        }
    }
    None
}

/// Record history and release the console's active-job guard. Always runs.
async fn finalize(
    state: &PkgInstallStateHandle,
    job: &str,
    req: &InstallRequest,
    _started: std::time::Instant,
) {
    if let Some(st) = state.jobs.get(job) {
        crate::log_at!(
            Info,
            "install",
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
        // The history file is rewritten on a blocking thread, not an async worker.
        {
            let (addr, entry) = (req.ps5_addr.clone(), entry.clone());
            let _ = tokio::task::spawn_blocking(move || {
                history::append(&history_dir(), &addr, &entry, history::HISTORY_CAP)
            })
            .await;
        }
        ps5upload_core::events::emit_detail(
            ps5upload_core::events::Cat::Install,
            if entry.code == 0 {
                ps5upload_core::events::Level::Info
            } else {
                ps5upload_core::events::Level::Error
            },
            "install_result",
            Some(&req.ps5_addr),
            format!("install {} code=0x{:08x}", entry.content_id, entry.code),
            serde_json::json!({ "verdict": entry.verdict, "route": entry.route }),
        );

        // Optional cleanup of an engine-side working copy (e.g. a Convert
        // output) after a successful install. Never touches a file the user
        // placed on the console themselves (console_path / remote / url).
        if req.options.delete_source_copy_after && st.verdict == Some(Verdict::Installed) {
            if let Source::HostFile(path) = &req.source {
                let _ = tokio::fs::remove_file(path).await;
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
            ps5_addr: "192.0.2.99:9114".into(),
            source,
            content_id: String::new(),
            title_id: None,
            package_app_ver: None,
            category: None,
            options: InstallOptions::default(),
        }
    }

    #[test]
    fn a_proxied_link_carries_the_certificate_choice_and_a_path_does_not() {
        let mut req = bare_req(Source::Url("https://h/x.pkg".into()));
        req.options.insecure_tls = true;
        let link = serve_session_request(&req, "https://h/x.pkg");
        assert_eq!(link["remote_url"], "https://h/x.pkg");
        assert_eq!(link["insecure_tls"], true);
        assert_eq!(link["serve_only"], true);
        let path = serve_session_request(&req, "ps5://10.0.0.5/data/a.pkg");
        assert_eq!(path["path"], "ps5://10.0.0.5/data/a.pkg");
        assert!(path.get("insecure_tls").is_none());
        assert!(path.get("remote_url").is_none());
    }

    #[test]
    fn the_link_options_default_to_off() {
        let o: InstallOptions = serde_json::from_str("{}").expect("parse");
        assert!(!o.proxy_link && !o.insecure_tls);
        let o: InstallOptions =
            serde_json::from_str(r#"{"proxy_link":true,"insecure_tls":true}"#).expect("parse");
        assert!(o.proxy_link && o.insecure_tls);
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

    // ── a stream the console never fetched from: say why, not "declined" ──

    #[test]
    fn unreachable_stream_names_the_address_and_the_firewall() {
        // Measured on the Phat: a firewall-blocked engine gave 0x80431064 with
        // 0 bytes served, and the UI said only "The PS5 declined the install."
        let h = stream_unreachable_hint(Some("http://192.0.2.199:19200"), 0x80431064, None);
        assert!(
            h.contains("never reached this computer at http://192.0.2.199:19200"),
            "{h}"
        );
        assert!(h.contains("0x80431064"), "{h}");
        assert!(h.contains("firewall"), "{h}");
        assert!(h.contains("Upload & install"), "{h}");
    }

    #[test]
    fn every_unreachable_path_carries_the_host_ip_guidance() {
        // 1. a bridged container: host networking.
        let c = stream_unreachable_hint_for(Some("http://172.17.0.2:19113"), 0, true, None);
        assert!(
            c.contains("PS5UPLOAD_PKG_HOST_IP") && c.contains("--network host"),
            "{c}"
        );
        assert!(c.contains("172.17.0.2"), "{c}");
        // 2. Sony refused a stream it never fetched (no proxy error).
        let r = stream_unreachable_hint_for(Some("http://10.8.0.2:19113"), 0x80431068, false, None);
        assert!(
            r.contains("PS5UPLOAD_PKG_HOST_IP") && r.contains("0x80431068"),
            "{r}"
        );
        // 3. accepted but never fetched (the stall path passes code 0): no
        //    made-up return code, same remedy.
        let z = stream_unreachable_hint_for(Some("http://10.8.0.2:19113"), 0, false, None);
        assert!(z.contains("PS5UPLOAD_PKG_HOST_IP"), "{z}");
        assert!(!z.contains("0x0000"), "{z}");
        // The proxy case is its own cause and keeps its own remedy.
        let p = stream_unreachable_hint_for(None, SCE_HTTP_ERROR_PROXY, false, None);
        assert!(
            p.contains("Do Not Use") && !p.contains("PKG_HOST_IP"),
            "{p}"
        );
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
    fn a_windows_diagnosis_replaces_the_generic_firewall_paragraph() {
        use crate::win_net::{NetCategory, NetDiag};
        let d = NetDiag {
            adapter: "Ethernet 3".into(),
            local_ip: "192.168.88.1".into(),
            category: NetCategory::Public,
            firewall_enabled: Some(true),
            allowed_by_rule: Some(false),
        };
        let origin = "http://192.168.88.1:19113";
        // the stream the console never fetched (a Sony code, or none)
        for code in [0x80431068, 0] {
            let h = stream_unreachable_hint_for(Some(origin), code, false, Some(&d));
            assert!(
                h.contains("Ethernet 3") && h.contains("Public network"),
                "{h}"
            );
            assert!(h.contains("Make this network Private"), "{h}");
            assert!(
                !h.contains("on Windows, for both Private and Public"),
                "{h}"
            );
            assert!(
                h.contains("PS5UPLOAD_PKG_HOST_IP") && h.contains("Upload & install"),
                "{h}"
            );
        }
        let g = stream_unreachable_hint_for(Some(origin), 0x80431068, false, None);
        assert!(g.contains("on Windows, for both Private and Public"), "{g}");
        // a container still gets the container advice, never a Windows one
        let c = stream_unreachable_hint_for(Some(origin), 0, true, Some(&d));
        assert!(
            c.contains("--network host") && !c.contains("Ethernet 3"),
            "{c}"
        );
    }

    #[test]
    fn the_plain_path_last_resort_runs_only_where_a_failure_has_nothing_to_delete() {
        let on_console = Source::ConsolePath("/user/data/ps5upload/pkg_library/a.pkg".into());
        let ok = |enabled, cat: &str, known, installed, unreachable| {
            path_fallback_allowed(enabled, &on_console, cat, known, installed, unreachable)
        };
        // The one case: asked for, a base game, title known and not installed, no way to stream.
        assert!(ok(true, "PS5GD", true, false, true));
        assert!(ok(true, "gd", true, false, true));
        // Off unless the client asks.
        assert!(!ok(false, "PS5GD", true, false, true));
        // A console that can reach this engine streams instead.
        assert!(!ok(true, "PS5GD", true, false, false));
        // A patch or add-on shares the base game's content id: a failure wiped the base.
        assert!(!ok(true, "PS4DP", true, false, true));
        assert!(!ok(true, "gp", true, false, true));
        assert!(!ok(true, "PS5AC", true, false, true));
        // An unknown category is not assumed to be a base game.
        assert!(!ok(true, "", true, false, true));
        // Installed already: Sony clears the old copy first, so a failure leaves neither.
        assert!(!ok(true, "PS5GD", true, true, true));
        // Without a title id nobody checked whether it is installed.
        assert!(!ok(true, "PS5GD", false, false, true));
        // Only the console's own copy has a path to offer.
        let on_pc = Source::HostFile("/games/a.pkg".into());
        assert!(!path_fallback_allowed(
            true, &on_pc, "PS5GD", true, false, true
        ));
        // The refusal says nothing was lost and names both ways out.
        let h = path_fallback_refused_hint(0x80B2116F);
        assert!(
            h.contains("0x80B2116F") && h.contains("Nothing was changed"),
            "{h}"
        );
        assert!(
            h.contains("Stream & install") && h.contains("Package Installer"),
            "{h}"
        );
    }

    /// A panicking install task released nothing: the console stayed "busy" until a
    /// restart. The slot guard releases it and fails the job.
    #[tokio::test]
    async fn a_panicking_install_task_frees_the_console_and_fails_the_job() {
        let state = std::sync::Arc::new(crate::pkg_install::PkgInstallState::default());
        let job = state.jobs.begin("192.0.2.9:1234").unwrap();
        let (st, j) = (state.clone(), job.clone());
        let joined = tokio::spawn(async move {
            let _slot = SlotRelease::new(st, j.clone());
            if !j.is_empty() {
                panic!("injected");
            }
        })
        .await;
        assert!(joined.is_err());
        let after = state.jobs.get(&job).unwrap();
        assert_eq!(after.phase, Phase::Failed);
        assert_eq!(after.verdict, Some(Verdict::Failed));
        assert!(
            state.jobs.begin("192.0.2.9").is_ok(),
            "the console is free again"
        );
    }

    /// A job that finished normally keeps its result when the guard drops later.
    #[test]
    fn the_slot_guard_leaves_a_finished_job_alone() {
        let state = std::sync::Arc::new(crate::pkg_install::PkgInstallState::default());
        let job = state.jobs.begin("192.0.2.10").unwrap();
        state.jobs.update(&job, |s| {
            s.phase = Phase::Done;
            s.verdict = Some(Verdict::Installed);
        });
        state.jobs.finish(&job);
        let next = state.jobs.begin("192.0.2.10").unwrap();
        drop(SlotRelease::new(state.clone(), job.clone()));
        assert_eq!(
            state.jobs.get(&job).unwrap().verdict,
            Some(Verdict::Installed)
        );
        // ... and does not take the next job's slot.
        assert_eq!(state.jobs.begin("192.0.2.10"), Err(next));
    }

    /// A poisoned lock (a panic while one was held) must not break every console.
    #[test]
    fn a_poisoned_store_still_works() {
        let jobs = std::sync::Arc::new(JobStore::new());
        let j2 = jobs.clone();
        let _ = std::thread::spawn(move || {
            let _held = j2.jobs.lock().unwrap();
            let _held2 = j2.active.lock().unwrap();
            panic!("poison");
        })
        .join();
        assert!(jobs.jobs.is_poisoned());
        let job = jobs.begin("192.0.2.11").unwrap();
        assert!(jobs.get(&job).is_some());
        assert!(jobs.request_cancel(&job));
        jobs.finish(&job);
        assert!(!jobs.any_active());
    }

    /// Finishing forgets the stop request and keeps only the newest finished statuses.
    #[test]
    fn running_lists_only_installs_holding_a_slot() {
        let jobs = JobStore::new();
        let a = jobs.begin("10.1.0.1").unwrap();
        let b = jobs.begin("10.1.0.2").unwrap();
        jobs.update(&b, |s| s.phase = Phase::Done);
        jobs.finish(&b);
        let running: Vec<String> = jobs.running().into_iter().map(|s| s.job).collect();
        assert_eq!(running, vec![a]);
    }

    #[test]
    fn finish_prunes_old_statuses_and_stop_requests() {
        let jobs = JobStore::new();
        let mut ids = Vec::new();
        for i in 0..(KEEP_FINISHED + 5) {
            let job = jobs
                .begin(&format!("10.0.{}.{}", i / 250, i % 250))
                .unwrap();
            jobs.request_cancel(&job);
            jobs.update(&job, |s| s.phase = Phase::Failed);
            jobs.update(&job, |s| s.updated_at = i as u64);
            jobs.finish(&job);
            assert!(!jobs.is_cancelled(&job));
            ids.push(job);
        }
        let running = jobs.begin("10.9.9.9").unwrap();
        // The oldest of all (set directly: `update` stamps the time).
        jobs.jobs
            .lock()
            .unwrap()
            .get_mut(&running)
            .unwrap()
            .updated_at = 0;
        // Still holding its slot (Resolve): a finish elsewhere never prunes it.
        let other = jobs.begin("10.9.9.8").unwrap();
        jobs.update(&other, |s| s.phase = Phase::Done);
        jobs.finish(&other);
        assert!(jobs.get(&running).is_some());
        assert!(jobs.get(&ids[0]).is_none(), "the oldest went");
        assert!(jobs.get(ids.last().unwrap()).is_some());
        // Ended without reaching Done or Failed: no longer kept forever.
        jobs.finish(&running);
        let last = jobs.begin("10.9.9.7").unwrap();
        jobs.update(&last, |s| s.phase = Phase::Done);
        jobs.finish(&last);
        assert!(jobs.get(&running).is_none());
        assert_eq!(jobs.jobs.lock().unwrap().len(), KEEP_FINISHED);
    }

    #[test]
    fn one_console_has_one_slot_whatever_the_case_of_its_name() {
        let jobs = JobStore::new();
        let first = jobs.begin("PS5.local:1234").unwrap();
        assert_eq!(jobs.begin("ps5.LOCAL"), Err(first.clone()));
        jobs.finish(&first);
        assert!(!jobs.any_active());
    }

    #[test]
    fn console_id_keeps_ipv6_hosts_whole() {
        assert_eq!(console_id("192.0.2.7:1234"), "192.0.2.7");
        assert_eq!(console_id("192.0.2.7"), "192.0.2.7");
        assert_eq!(console_id("[fe80::1]:1234"), "fe80::1");
        assert_eq!(console_id("fe80::1"), "fe80::1");
        assert_eq!(console_id("fe80::1"), console_id("[fe80::1]:99"));
        assert_ne!(console_id("fe80::1"), console_id("fe80::2"));
    }

    #[test]
    fn only_a_running_install_can_be_stopped() {
        let jobs = JobStore::new();
        let job = jobs.begin("192.0.2.7:9113").unwrap();
        assert!(!jobs.is_cancelled(&job));
        assert!(jobs.request_cancel(&job));
        assert!(jobs.is_cancelled(&job));
        // A finished job, or one that never existed, is not "stopped".
        let done = {
            jobs.finish(&job);
            jobs.begin("192.0.2.7:9113").unwrap()
        };
        jobs.update(&done, |s| s.phase = Phase::Done);
        assert!(!jobs.request_cancel(&done));
        assert!(!jobs.request_cancel("no-such-job"));
    }

    #[test]
    fn the_last_resort_is_off_unless_the_request_asks_for_it() {
        let o: InstallOptions = serde_json::from_str("{}").unwrap();
        assert!(!o.console_path_fallback);
        let o: InstallOptions = serde_json::from_str(r#"{"console_path_fallback":true}"#).unwrap();
        assert!(o.console_path_fallback);
    }

    #[test]
    fn retry_with_stream_is_offered_only_for_a_staged_refusal_from_the_consoles_own_copy() {
        let on_console = Source::ConsolePath("/user/data/ps5upload/pkg_library/a.pkg".into());
        let staged = Some(FailReason::StagedRefused);
        assert!(stream_retry_offered(staged, &on_console, "PS4GD", true));
        // Unrelated failures never get the action.
        for other in [
            FailReason::SonyRefused,
            FailReason::StreamUnreachable,
            FailReason::Stalled,
        ] {
            assert!(!stream_retry_offered(
                Some(other),
                &on_console,
                "PS4GD",
                true
            ));
        }
        assert!(!stream_retry_offered(None, &on_console, "PS4GD", true));
        // A source that already streams has nothing to retry.
        let url = Source::Url("http://x/a.pkg".into());
        assert!(!stream_retry_offered(staged, &url, "PS4GD", true));
    }

    #[test]
    fn a_patch_or_addon_gets_retry_with_stream_only_through_the_daemon() {
        let on_console = Source::ConsolePath("/user/data/ps5upload/pkg_library/p.pkg".into());
        let staged = Some(FailReason::StagedRefused);
        for cat in ["PS4DP", "PS5DP", "gp", "PS4AC", "ac"] {
            // A patch shares its base's content_id; any in-process route wipes the base.
            assert!(
                !stream_retry_offered(staged, &on_console, cat, false),
                "{cat}"
            );
            assert!(!stream_retry_allowed(cat, false), "{cat}");
            assert!(
                stream_retry_offered(staged, &on_console, cat, true),
                "{cat}"
            );
        }
        // A base game does not carry that risk.
        assert!(stream_retry_allowed("PS4GD", false));
    }

    #[tokio::test]
    async fn the_install_route_refuses_a_games_own_installed_pkg_before_touching_the_console() {
        // The handler must say no from the path alone: no console is contacted, no job begins.
        let state = std::sync::Arc::new(crate::pkg_install::PkgInstallState::default());
        let req: InstallRequest = serde_json::from_value(serde_json::json!({
            "ps5_addr": "192.0.2.1:9113",
            "source": {"console_path": "/mnt/ext0/user/patch/CUSA02092/patch.pkg"},
        }))
        .unwrap();
        let resp = install_handler(State(state.clone()), Json(req)).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(
            state.jobs.begin("192.0.2.1:9113").is_ok(),
            "no job may have been started"
        );
    }

    #[test]
    fn a_bridged_container_is_told_about_host_networking_not_firewalls() {
        // A homelab engine in Docker with default bridge networking hands the
        // PS5 its 172.17.x address; "allow it through the Windows firewall"
        // sent people chasing the wrong thing.
        let h =
            stream_unreachable_hint_for(Some("http://172.17.0.2:19113"), 0x80431064, true, None);
        assert!(h.contains("container"), "{h}");
        assert!(h.contains("--network host"), "{h}");
        assert!(h.contains("PS5UPLOAD_PKG_HOST_IP"), "{h}");
        assert!(!h.contains("firewall"), "{h}");
        let plain = stream_unreachable_hint_for(None, 0x80431064, false, None);
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
        let h = stream_unreachable_hint(None, 0x80431084, None);
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
            reason: Some(DPI_REASON_LOADER_UNREACHABLE),
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
    fn only_an_explicit_stop_lets_go_of_a_session_at_once() {
        assert_eq!(
            session_after_delivery(DeliveryProgress::Cancelled),
            SessionAfterDelivery::Release
        );
        assert_eq!(
            session_after_delivery(DeliveryProgress::Stalled),
            SessionAfterDelivery::Keep,
            "a stall may be a pause: a 410 makes PlayGo delete what it downloaded"
        );
        assert_eq!(
            session_after_delivery(DeliveryProgress::Complete),
            SessionAfterDelivery::ReleaseWhenQuiet
        );
    }

    #[test]
    fn a_lost_reply_keeps_the_session_a_refusal_releases_it() {
        assert!(!release_after_refusal(&Err("read timed out".into())));
        assert!(!release_after_refusal(&Ok(ic::InstallReply::Unknown(
            "garbled".into()
        ))));
        assert!(release_after_refusal(&Ok(ic::InstallReply::Sony {
            code: 0x80B2_116F,
            hint: None
        })));
        assert!(release_after_refusal(&Ok(ic::InstallReply::Busy {
            job: "j".into()
        })));
    }

    #[test]
    fn failures_that_are_not_sonys_say_so() {
        let r = |x: Result<ic::InstallReply, String>| reason_from_reply(&x);
        assert_eq!(
            r(Ok(ic::InstallReply::Busy { job: "j".into() })),
            Some(FailReason::Busy)
        );
        assert_eq!(
            r(Ok(ic::InstallReply::SourceUnreadable)),
            Some(FailReason::SourceGone)
        );
        assert_eq!(
            r(Ok(ic::InstallReply::BadPath)),
            Some(FailReason::BadRequest)
        );
        assert_eq!(
            r(Ok(ic::InstallReply::LoopbackFailed)),
            Some(FailReason::NoBringup)
        );
        assert_eq!(
            r(Err("connect refused".into())),
            Some(FailReason::ReplyLost)
        );
        assert_eq!(
            r(Ok(ic::InstallReply::Sony {
                code: 1,
                hint: None
            })),
            None
        );
    }

    #[test]
    fn a_serve_failure_reads_as_its_inner_error() {
        let f = ServeFailure::from_response(
            StatusCode::BAD_REQUEST,
            br#"{"error":"This is an update/patch for CUSA00001, but that game isn't installed on this PS5. Install the base game first."}"#,
        );
        assert_eq!(f.reason, Some(FailReason::BaseMissing));
        assert!(f.message.starts_with("This is an update"), "{}", f.message);
        let f =
            ServeFailure::from_response(StatusCode::CONFLICT, br#"{"error":"already running"}"#);
        assert_eq!(f.reason, Some(FailReason::Busy));
        let f = ServeFailure::from_response(StatusCode::BAD_REQUEST, b"no such file");
        assert_eq!(f.reason, Some(FailReason::SourceGone));
        assert_eq!(f.message, "no such file");
    }

    #[test]
    fn only_a_network_code_blames_the_network() {
        assert_eq!(
            refusal_reason(true, 0x8043_1064, Route::Stream),
            FailReason::StreamUnreachable
        );
        assert_eq!(
            refusal_reason(true, 0x80B2_116F, Route::Stream),
            FailReason::SonyRefused
        );
        assert_eq!(
            refusal_reason(true, 0, Route::Stream),
            FailReason::SonyRefused
        );
    }

    #[test]
    fn the_console_path_goes_on_through_this_engine_for_what_the_route_causes() {
        let sony = |code| Ok(ic::InstallReply::Sony { code, hint: None });
        assert_eq!(
            staged_retry_through_engine(&sony(0x80B2_116F)),
            Some(0x80B2_116F)
        );
        // The daemon no longer tries the bare path by itself on these.
        assert_eq!(
            staged_retry_through_engine(&sony(0x8043_1064)),
            Some(0x8043_1064)
        );
        // Too long for Sony, from a daemon older than 1.6.0.
        assert_eq!(
            staged_retry_through_engine(&sony(0x80A3_0003)),
            Some(0x80A3_0003)
        );
        assert_eq!(
            staged_retry_through_engine(&Ok(ic::InstallReply::LoopbackFailed)),
            Some(0)
        );
        assert_eq!(staged_retry_through_engine(&sony(0x80A3_000C)), None);
        assert_eq!(
            staged_retry_through_engine(&Ok(ic::InstallReply::SourceUnreadable)),
            None
        );
    }

    #[test]
    fn an_update_the_game_already_has_needs_asking() {
        assert!(update_already_applied("01.04", "01.04"));
        assert!(update_already_applied("01.04", "01.09"));
        assert!(!update_already_applied("01.09", "01.04"));
        assert!(!update_already_applied("01.09", ""));
    }

    #[test]
    fn package_type_and_base_check_follow_the_title() {
        assert_eq!(package_type_for_verify("gd", "CUSA00001"), "PS4GD");
        assert_eq!(package_type_for_verify("gp", "PPSA00001"), "PS5DP");
        assert_eq!(package_type_for_verify("PS5AC", "XXXX00001"), "PS5AC");
        assert_eq!(base_check_applies("gp", "CUSA00001"), Some("gp"));
        assert_eq!(base_check_applies("PS4AC", "CUSA00001"), Some("ac"));
        assert_eq!(base_check_applies("gd", "CUSA00001"), None);
        // A PS5 update can carry the whole app (measured), so the PS5 decides.
        assert_eq!(base_check_applies("gp", "PPSA00001"), None);
    }

    #[test]
    fn a_version_check_that_ran_out_is_not_a_failure() {
        use ps5upload_core::patch_verify::PatchVerdict as PV;
        // The console holds the whole package.
        assert_eq!(
            verdict_after_wait(PV::Applied, false, true),
            Verdict::Installed
        );
        assert_eq!(
            verdict_after_wait(PV::Inconclusive, false, true),
            Verdict::Installed,
            "a same-version re-install has nothing to wait for"
        );
        assert_eq!(
            verdict_after_wait(PV::Inconclusive, true, true),
            Verdict::Unverified,
            "a large update may still be writing"
        );
        assert_eq!(
            verdict_after_wait(PV::Regressed, true, true),
            Verdict::Failed,
            "a loss is still concluded at the deadline"
        );
        // A link the console fetches by itself: nothing here saw any of it.
        assert_eq!(
            verdict_after_wait(PV::Applied, false, false),
            Verdict::Installed
        );
        assert_eq!(
            verdict_after_wait(PV::Inconclusive, false, false),
            Verdict::Unverified
        );
        assert_eq!(
            verdict_after_wait(PV::Inconclusive, true, false),
            Verdict::Unverified
        );
        assert_eq!(
            verdict_after_wait(PV::Regressed, true, false),
            Verdict::Unverified
        );
    }

    #[test]
    fn the_unverified_hint_says_which_of_the_two_it_is() {
        assert!(unverified_hint(true, 300_000).contains("after 300 s"));
        assert!(unverified_hint(false, 0).contains("Downloads"));
    }

    #[tokio::test]
    async fn a_stop_is_seen_before_and_while_waiting() {
        let store = JobStore::new();
        let job = store.begin("10.0.0.1").unwrap();
        let other = store.begin("10.0.0.2").unwrap();
        let waiter = {
            let store = std::sync::Arc::new(store);
            let (s, j) = (store.clone(), job.clone());
            let handle = tokio::spawn(async move { s.stop_requested(&j).await });
            tokio::task::yield_now().await;
            // Another job's stop wakes the waiter, which goes back to waiting.
            assert!(store.request_cancel(&other));
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            assert!(!handle.is_finished());
            assert!(store.request_cancel(&job));
            handle.await.unwrap();
            store
        };
        // Asked before anyone waits: it must not be missed.
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            waiter.stop_requested(&job),
        )
        .await
        .expect("an earlier stop is seen at once");
    }

    /// The daemon can take 15 minutes to answer: Stop must not wait for it (it used to be
    /// acknowledged and then ignored until the call returned).
    #[tokio::test]
    async fn a_stop_ends_a_blocking_call_it_cannot_interrupt() {
        let state = std::sync::Arc::new(crate::pkg_install::PkgInstallState::default());
        let job = state.jobs.begin("10.0.0.3").unwrap();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let call = {
            let (state, job) = (state.clone(), job.clone());
            tokio::spawn(async move {
                install_call(&state, &job, move || {
                    let _ = gate.recv();
                    Ok(ic::InstallReply::BadPath)
                })
                .await
            })
        };
        tokio::task::yield_now().await;
        assert!(state.jobs.request_cancel(&job));
        let answer = tokio::time::timeout(std::time::Duration::from_secs(5), call)
            .await
            .expect("the stop ends the wait")
            .unwrap();
        assert!(answer.unwrap_err().contains("stopped"));
        let _ = release.send(());
    }

    #[tokio::test]
    async fn a_stopped_job_ends_cancelled_and_frees_the_console() {
        let state = std::sync::Arc::new(crate::pkg_install::PkgInstallState::default());
        let job = state.jobs.begin("10.0.0.4").unwrap();
        let req = bare_req(Source::Url("http://h/x.pkg".into()));
        let started = std::time::Instant::now();
        assert!(!ended_by_stop(&state, &job, &req, started, None).await);
        assert!(state.jobs.get(&job).unwrap().verdict.is_none());
        assert!(state.jobs.request_cancel(&job));
        assert!(ended_by_stop(&state, &job, &req, started, None).await);
        let st = state.jobs.get(&job).unwrap();
        assert_eq!(st.phase, Phase::Failed);
        assert_eq!(st.reason, Some(FailReason::Cancelled));
        assert!(!state.jobs.active_for("10.0.0.4"), "the slot is released");
    }

    #[test]
    fn an_install_id_is_told_from_a_transfer_id() {
        assert!(is_install_job_id("1790000000-3"));
        assert!(!is_install_job_id("f983a63c-e6f7-489c-b2d7-14d994eff321"));
        assert!(!is_install_job_id("1790000000"));
        assert!(!is_install_job_id("-3"));
        assert!(!is_install_job_id("1-"));
        assert!(!is_install_job_id("a-1"));
        assert!(!is_install_job_id(""));
    }

    #[test]
    fn the_job_list_carries_installs_with_their_kind_and_state() {
        let store = JobStore::new();
        let job = store.begin("10.0.0.5").unwrap();
        let running = job_list_entry(&store.get(&job).unwrap());
        assert_eq!(running["kind"], "install");
        assert_eq!(running["job_id"], job.as_str());
        assert_eq!(running["status"], "running");
        store.update(&job, |s| {
            s.phase = Phase::Failed;
            s.reason = Some(FailReason::Cancelled);
        });
        let all = store.all();
        assert_eq!(all.len(), 1);
        let failed = job_list_entry(&all[0]);
        assert_eq!(failed["status"], "failed");
        assert_eq!(failed["job"]["reason"], "cancelled");
    }

    /// A refusal must not read as a success: the client told the two apart only by the body.
    #[tokio::test]
    async fn a_second_install_on_a_busy_console_is_a_409_naming_the_running_one() {
        let state = std::sync::Arc::new(crate::pkg_install::PkgInstallState::default());
        let running = state.jobs.begin("10.0.0.6").unwrap();
        let req = bare_req(Source::Url("http://h/x.pkg".into()));
        let req = InstallRequest {
            ps5_addr: "10.0.0.6".into(),
            ..req
        };
        let resp = install_handler(State(state.clone()), Json(req)).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 16)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "busy");
        assert_eq!(v["job"], running.as_str());
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
