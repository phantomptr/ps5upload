//! The unified install status + metrics types (spec 2 §4, §5). One status
//! object replaces the scattered dpi_ok/dpi_rc/register_path/may_not_launch/
//! ambiguous/via/bridge fields. No I/O, no Sony calls.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Resolve,
    Deliver,
    Install,
    Verify,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    Loopback,
    Stream,
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Installed,
    MayNotLaunch,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchVerdict {
    Applied,
    DidNotApply,
    Regressed,
    Inconclusive,
}

impl From<ps5upload_core::patch_verify::PatchVerdict> for PatchVerdict {
    fn from(v: ps5upload_core::patch_verify::PatchVerdict) -> Self {
        use ps5upload_core::patch_verify::PatchVerdict as C;
        match v {
            C::Applied => PatchVerdict::Applied,
            C::DidNotApply => PatchVerdict::DidNotApply,
            C::Regressed => PatchVerdict::Regressed,
            C::Inconclusive => PatchVerdict::Inconclusive,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailReason {
    LoaderUnreachable,
    NoBringup,
    NoImage,
    SourceGone,
    SonyRefused,
    DestructiveGuard,
    BadRequest,
    /// Sony accepted the install but the console stopped pulling the package
    /// before it finished. The source is kept for a retry.
    Stalled,
    /// Sony refused a stream the console never fetched from: it could not
    /// reach this computer (firewall, VPN, another network).
    StreamUnreachable,
    /// As above, but Sony named the PS5's proxy setting as the cause.
    StreamProxy,
    /// Sony refused a package installed from the console's own storage (the
    /// staged / Loopback route) with a code measured on that route alone:
    /// 0x80B2116F (FW 9.60, 13.60) or 0x80B2150F (FW 5.10). The same package
    /// streamed from a computer installs, so the fix is the route, not the file.
    StagedRefused,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    #[serde(default)]
    pub total_bytes: u64,
    #[serde(default)]
    pub served_bytes: u64,
    #[serde(default)]
    pub throughput_mbps: f64,
    #[serde(default)]
    pub phase_ms: BTreeMap<String, u64>,
    #[serde(default)]
    pub retries: u32,
    #[serde(default)]
    pub sony_rc: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallStatus {
    pub job: String,
    pub ps5_addr: String,
    pub content_id: String,
    pub title_id: Option<String>,
    pub phase: Phase,
    pub route: Option<Route>,
    pub verdict: Option<Verdict>,
    pub code: u32,
    pub hint: Option<String>,
    pub reason: Option<FailReason>,
    pub metrics: Metrics,
    pub app_ver_before: Option<String>,
    pub app_ver_after: Option<String>,
    pub patch_verdict: Option<PatchVerdict>,
    pub shortened: bool,
    /// Epoch seconds (the engine has no date crate and stamps time this way
    /// everywhere, e.g. session `created_at_unix`); the client formats it.
    pub started_at: u64,
    pub updated_at: u64,
}

impl InstallStatus {
    /// A fresh status in the Resolve phase with empty metrics and `now`
    /// timestamps.
    pub fn new(job: &str, ps5_addr: &str, content_id: &str) -> Self {
        let now = now_unix();
        InstallStatus {
            job: job.to_string(),
            ps5_addr: ps5_addr.to_string(),
            content_id: content_id.to_string(),
            title_id: None,
            phase: Phase::Resolve,
            route: None,
            verdict: None,
            code: 0,
            hint: None,
            reason: None,
            metrics: Metrics::default(),
            app_ver_before: None,
            app_ver_after: None,
            patch_verdict: None,
            shortened: false,
            started_at: now,
            updated_at: now,
        }
    }
}

/// Current time as epoch seconds (matches the engine's `created_at_unix`).
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// served_bytes as MB (1_000_000) over the deliver phase in seconds.
pub fn throughput_mbps(served_bytes: u64, deliver_ms: u64) -> f64 {
    if deliver_ms == 0 {
        return 0.0;
    }
    (served_bytes as f64 / 1_000_000.0) / (deliver_ms as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_serializes_with_spec_field_names_and_nulls() {
        let s = InstallStatus {
            job: "j1".into(),
            ps5_addr: "192.168.0.100:9114".into(),
            content_id: "UP4433-PPSA17221_00".into(),
            title_id: Some("PPSA17221".into()),
            phase: Phase::Deliver,
            route: Some(Route::Stream),
            verdict: None,
            code: 0,
            hint: None,
            reason: None,
            metrics: Metrics::default(),
            app_ver_before: None,
            app_ver_after: None,
            patch_verdict: None,
            shortened: false,
            started_at: 1_790_000_000,
            updated_at: 1_790_000_001,
        };
        let v: serde_json::Value = serde_json::to_value(&s).unwrap();
        assert_eq!(v["phase"], "deliver");
        assert_eq!(v["route"], "stream");
        assert!(v["verdict"].is_null());
        assert!(v["reason"].is_null());
        assert!(v["patch_verdict"].is_null());
        assert_eq!(v["shortened"], false);
        assert_eq!(v["metrics"]["total_bytes"], 0);
        assert_eq!(v["metrics"]["retries"], 0);
    }

    #[test]
    fn enum_wire_strings_match_spec() {
        assert_eq!(
            serde_json::to_value(Verdict::MayNotLaunch).unwrap(),
            "may_not_launch"
        );
        assert_eq!(serde_json::to_value(Route::Loopback).unwrap(), "loopback");
        assert_eq!(serde_json::to_value(Phase::Failed).unwrap(), "failed");
        assert_eq!(
            serde_json::to_value(PatchVerdict::DidNotApply).unwrap(),
            "did_not_apply"
        );
        assert_eq!(
            serde_json::to_value(FailReason::LoaderUnreachable).unwrap(),
            "loader_unreachable"
        );
    }

    #[test]
    fn throughput_is_mb_over_deliver_seconds() {
        assert!((throughput_mbps(100_000_000, 1000) - 100.0).abs() < 1e-6);
        assert_eq!(throughput_mbps(1, 0), 0.0);
    }
}
