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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::deliver::{decide_delivery, Delivery, Source};

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
    fn status_serializes_the_unified_shape() {
        let s = InstallStatus::new("job1", "1.2.3.4:9114", "CID");
        let j = serde_json::to_value(&s).unwrap();
        assert_eq!(j["phase"], "resolve");
        assert!(j.get("route").is_some() && j["route"].is_null());
        assert!(j.get("metrics").is_some());
    }
}
