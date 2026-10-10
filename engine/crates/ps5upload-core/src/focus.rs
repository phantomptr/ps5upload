//! Which application currently owns the PS5's screen.
//!
//! Thin client wrapper around the payload's FOCUS_PROBE frame. The payload
//! answers by calling `sceSystemServiceGetAppIdOfBigApp` in-process via
//! dlsym — it never ptrace-attaches SceShellUI, because a ShellUI left
//! stopped freezes the console UI until a power-button recovery.
//!
//! This exists to diagnose a foregrounded game dropping back to the
//! dashboard after a while. ShadowMount+ polls the two ShellUI event flags
//! (`SceShellCoreUtilAppFocus`, `SceLncUtilSystemStatus`) and both stayed
//! silent across a measured window in which the drop demonstrably happened,
//! so those flags cannot be used to detect it. "Big app" is the full-screen
//! foreground application; comparing its id against a game's known app id
//! is a direct foreground/background answer.
//!
//! Makes one management call per probe. Cheap
//! enough to poll at 1 Hz.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn minus_one() -> i64 {
    -1
}

use crate::mgmt::{self, m};

/// Which candidate symbol this probe uses as the authoritative answer.
pub const BIG_APP_SYMBOL: &str = "sceSystemServiceGetAppIdOfBigApp";

/// Scheduler-visible state of one running app.
///
/// This is the part that actually answers "is the game on screen?" on a
/// firmware with no focus getter. A PS5 game that loses the screen is
/// suspended or has its CPU budget collapsed, so `runtime_us` sampled over
/// time separates foreground from background behaviourally — no Sony focus
/// API, no crash risk, no firmware dependency.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppState {
    #[serde(default)]
    pub pid: i32,
    #[serde(default)]
    pub title_id: String,
    #[serde(default)]
    pub app_id: u32,
    /// FreeBSD process state. 4 == SSTOP: outright stopped, the strong
    /// "definitely not running" signal.
    #[serde(default)]
    pub stat: i32,
    /// Accumulated CPU microseconds. The rate of change is the signal.
    #[serde(default)]
    pub runtime_us: u64,
    #[serde(default)]
    pub pctcpu: u32,
    #[serde(default)]
    pub nthreads: i32,
    #[serde(default)]
    pub slptime: u32,
    #[serde(default)]
    pub swtime: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FocusProbe {
    #[serde(default)]
    pub ok: bool,
    /// Symbol name -> whether it resolved on this firmware.
    ///
    /// A map rather than fixed fields: the payload probes a table of
    /// candidate names, and which ones exist varies per firmware. Measured
    /// on FW 9.60, `sceSystemServiceGetAppIdOfBigApp` does NOT resolve.
    #[serde(default)]
    pub apis: BTreeMap<String, bool>,
    /// App id of the application that currently OWNS THE SCREEN, read from
    /// the `SceShellCoreUtilAppFocus` named event flag; -1 when unavailable.
    ///
    /// This is the authoritative answer on a firmware that exports no focus
    /// getter. Confirmed on FW 9.60: 0x6018 with the game on screen, 0x0007
    /// (SceShellUI) with the dashboard up.
    #[serde(default = "minus_one")]
    pub focus_app_id: i64,
    /// Raw return code from the flag poll, for diagnosing a failed read.
    #[serde(default)]
    pub focus_rc: i32,
    /// Whether the focus flag could be opened and polled at all.
    #[serde(default)]
    pub focus_available: bool,
    /// Every running app with a non-zero app id, with its scheduler state.
    #[serde(default)]
    pub apps: Vec<AppState>,
    /// App id of the full-screen foreground app, or 0/negative when none.
    #[serde(default)]
    pub big_app_id: i32,
    /// App id of the overlaid system UI, when one is up.
    #[serde(default)]
    pub mini_app_id: i32,
    /// Payload-side CLOCK_MONOTONIC millis, so a poller can spot a gap
    /// (helper restarted, console slept) instead of reading a stalled
    /// value as a steady one.
    #[serde(default)]
    pub monotonic_ms: i64,
}

impl FocusProbe {
    /// Candidate symbols that resolved on this console, sorted.
    ///
    /// This is the actual deliverable of the probe right now: it tells us
    /// which focus API, if any, this firmware can answer with.
    pub fn available(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .apis
            .iter()
            .filter(|(_, &ok)| ok)
            .map(|(k, _)| k.as_str())
            .collect();
        v.sort_unstable();
        v
    }
}

/// Ask the console which app owns the screen. Read-only.
pub fn focus_probe(addr: &str) -> Result<FocusProbe> {
    let resp = mgmt::call(addr, m::PROC_FOCUS, &[])?;
    Ok(serde_json::from_slice(&resp)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_payload_json() {
        let raw = br#"{"ok":true,"apis":{
            "sceSystemServiceGetAppIdOfBigApp":true,
            "sceLncUtilGetAppStatus":false},
            "big_app_id":24600,"mini_app_id":0,
            "monotonic_ms":123456}"#;
        let p: FocusProbe = serde_json::from_slice(raw).unwrap();
        assert!(p.ok);
        assert_eq!(p.available(), vec![BIG_APP_SYMBOL]);
        assert_eq!(p.big_app_id, 24600);
        assert_eq!(p.monotonic_ms, 123456);
    }

    #[test]
    fn parses_the_focus_flag_fields() {
        let raw = br#"{"ok":true,"apis":{},"focus_app_id":24600,"focus_rc":0,
            "focus_available":true,
            "apps":[{"pid":154,"title_id":"PPSA23226","app_id":24600,
                     "stat":3,"runtime_us":1,"pctcpu":0,"nthreads":80,
                     "slptime":0,"swtime":1}]}"#;
        let p: FocusProbe = serde_json::from_slice(raw).unwrap();
        assert!(p.focus_available);
        assert_eq!(p.focus_app_id, 24600);
        assert_eq!(p.apps.len(), 1);
        assert_eq!(p.apps[0].title_id, "PPSA23226");
    }

    /// A probe from an OLD payload carries none of the focus fields; it must
    /// default to "unknown", never to "focused".
    #[test]
    fn old_payload_json_defaults_to_unknown() {
        let p: FocusProbe = serde_json::from_slice(br#"{"ok":true}"#).unwrap();
        assert!(!p.focus_available);
        assert_eq!(p.focus_app_id, -1);
    }
}
