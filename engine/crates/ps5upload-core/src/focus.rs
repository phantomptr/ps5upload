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
//! Opens a fresh management-port connection (`host:9114`) per call. Cheap
//! enough to poll at 1 Hz.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn minus_one() -> i64 {
    -1
}

use crate::connection::Connection;

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
    /// True when `app_id` is the full-screen app currently on screen.
    ///
    /// Returns `None` — not `Some(false)` — when the console could not
    /// answer, so callers never report "backgrounded" on missing data.
    pub fn is_foreground(&self, app_id: u32) -> Option<bool> {
        // The event flag is the real answer; the BigApp getter does not even
        // exist on FW 9.60. Fall back to it only if some other firmware has
        // it but lacks the flag.
        if self.focus_available && self.focus_app_id >= 0 {
            return Some(self.focus_app_id as u64 == app_id as u64);
        }
        if self.apis.get(BIG_APP_SYMBOL).copied() != Some(true) {
            return None;
        }
        Some(self.big_app_id > 0 && self.big_app_id as u32 == app_id)
    }

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
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::FocusProbe, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected FOCUS_PROBE: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::FocusProbeAck {
        bail!("expected FOCUS_PROBE_ACK, got {ft:?}");
    }
    Ok(serde_json::from_slice(&resp)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(big_app: bool, id: i32) -> FocusProbe {
        let mut apis = BTreeMap::new();
        apis.insert(BIG_APP_SYMBOL.to_string(), big_app);
        FocusProbe {
            ok: true,
            apis,
            big_app_id: id,
            ..Default::default()
        }
    }

    #[test]
    fn foreground_when_ids_match() {
        assert_eq!(probe(true, 24600).is_foreground(24600), Some(true));
    }

    #[test]
    fn background_when_another_app_owns_the_screen() {
        assert_eq!(probe(true, 32775).is_foreground(24600), Some(false));
    }

    #[test]
    fn background_when_nothing_is_full_screen() {
        assert_eq!(probe(true, 0).is_foreground(24600), Some(false));
    }

    /// The distinction that matters: an unavailable API must never be
    /// reported as "the game is backgrounded".
    #[test]
    fn unknown_when_api_unavailable() {
        assert_eq!(probe(false, 0).is_foreground(24600), None);
    }

    #[test]
    fn negative_id_is_not_foreground() {
        assert_eq!(probe(true, -1).is_foreground(24600), Some(false));
    }

    #[test]
    fn focus_flag_beats_the_missing_big_app_getter() {
        // FW 9.60 has no BigApp getter at all, so the event flag must be the
        // deciding input — otherwise is_foreground() is permanently None.
        let p = FocusProbe {
            ok: true,
            focus_available: true,
            focus_app_id: 24600,
            ..Default::default()
        };
        assert_eq!(p.is_foreground(24600), Some(true));
        assert_eq!(p.is_foreground(7), Some(false));
    }

    #[test]
    fn unreadable_flag_with_no_getter_is_unknown_not_false() {
        let p = FocusProbe {
            ok: true,
            focus_available: false,
            focus_app_id: -1,
            ..Default::default()
        };
        assert_eq!(p.is_foreground(24600), None);
    }

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
        assert_eq!(p.is_foreground(24600), Some(true));
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
        assert_eq!(p.is_foreground(24600), Some(true));
    }

    /// A probe from an OLD payload carries none of the focus fields; it must
    /// default to "unknown", never to "focused".
    #[test]
    fn old_payload_json_defaults_to_unknown() {
        let p: FocusProbe = serde_json::from_slice(br#"{"ok":true}"#).unwrap();
        assert!(!p.focus_available);
        assert_eq!(p.focus_app_id, -1);
        assert_eq!(p.is_foreground(24600), None);
    }
}
