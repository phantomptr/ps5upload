//! In-app process manager over AVA1 management.
//!
//! Thin client wrappers around the payload's PROCESS_LIST / PROCESS_KILL
//! frames. `process_list` enumerates every running process (pid, name,
//! resident memory, thread count, and a `kind` classification so the UI can
//! filter/guard system processes); `process_kill` sends SIGKILL to one pid.
//!
//! "Restart" is deliberately NOT a payload frame — for an app it's just
//! `process_kill` followed by the existing app-launch path (kill + relaunch
//! by title id), composed on the client so there's one launch code path.
//!
//! Each call is one management call, parsed from its reply. Enumerate is read-only; kill runs as the
//! payload's (elevated) ucred.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::mgmt::{self, m};

/// One process row. Every field defaults so the payload's trailing
/// `{"truncated":true}` sentinel object (which carries no pid) parses
/// cleanly and is split off in `process_list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    #[serde(default)]
    pub pid: i32,
    /// Thread name (ki_tdname) — what the compact list has always shown.
    #[serde(default)]
    pub name: String,
    /// Command/executable name (ki_comm), e.g. "eboot.bin".
    #[serde(default)]
    pub comm: String,
    /// Title id for app/game processes; empty for daemons/system.
    #[serde(default)]
    pub title_id: String,
    #[serde(default)]
    pub app_id: u32,
    /// Resident set size in MiB.
    #[serde(default)]
    pub memory_mib: f64,
    #[serde(default)]
    pub threads: i32,
    /// "app" | "payload" | "system" — drives the UI's filter + kill guard.
    #[serde(default)]
    pub kind: String,
    /// True for the helper's OWN process. proc_kill refuses to kill it
    /// (pid == getpid() → EPERM), so the UI disables Kill/Restart for it.
    #[serde(default)]
    pub is_self: bool,
    /// Set only on the synthetic last element when the payload truncated
    /// the list to fit its buffer. Split out by `process_list`.
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessListResult {
    pub processes: Vec<ProcessInfo>,
    /// True when the payload's buffer filled and the list was cut short.
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct RawProcessList {
    #[serde(default)]
    procs: Vec<ProcessInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessKillAck {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub pid: i32,
    #[serde(default)]
    pub err: Option<String>,
    /// Numeric errno from the failed kill (ESRCH/EPERM/…), 0 on success.
    #[serde(default)]
    pub errno: i32,
    /// Human-readable errno string (strerror) — e.g. "No such process".
    #[serde(default)]
    pub reason: Option<String>,
}

/// Enumerate running processes (detailed). Read-only.
pub fn process_list(addr: &str) -> Result<ProcessListResult> {
    let resp = mgmt::call(addr, m::PROC_PROCESS_LIST, &[])?;
    let raw: RawProcessList = serde_json::from_slice(&resp)?;
    // Split the truncation sentinel (pid == 0) from real rows.
    let truncated = raw.procs.iter().any(|p| p.truncated);
    let processes = raw.procs.into_iter().filter(|p| p.pid > 0).collect();
    Ok(ProcessListResult {
        processes,
        truncated,
    })
}

/// SIGKILL a process by pid. The payload guards self/kernel/init; the UI
/// is responsible for confirming before killing a "system" process.
pub fn process_kill(addr: &str, pid: i32) -> Result<ProcessKillAck> {
    let body = serde_json::to_vec(&serde_json::json!({ "pid": pid }))?;
    // A refused kill is `{"ok":false,"pid":..,"err":"kill_failed","errno":..,"reason":..}`; over AVA1 it
    // is the error's cause and `call_legacy_body` returns it, so errno and reason still reach the user.
    let resp = mgmt::call_legacy_body(addr, m::PROC_KILL, "PROCESS_KILL", &body)?;
    let ack: ProcessKillAck = serde_json::from_slice(&resp)?;
    if !ack.ok {
        // Prefer the specific reason (strerror) over the generic err code so
        // the user/bug-report sees "No such process" not bare "kill_failed".
        let why = ack
            .reason
            .as_deref()
            .or(ack.err.as_deref())
            .unwrap_or("payload returned ok=false");
        bail!("PROCESS_KILL failed for pid {pid}: {why}");
    }
    Ok(ack)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_info_is_self_defaults_false_and_round_trips() {
        // Old payloads omit is_self → must default false (no row wrongly marked
        // as the helper). New payloads send it; true must survive.
        let without: ProcessInfo =
            serde_json::from_str(r#"{"pid":42,"name":"x","kind":"payload"}"#).unwrap();
        assert!(!without.is_self);
        let with: ProcessInfo = serde_json::from_str(
            r#"{"pid":101,"name":"payload.elf","kind":"payload","is_self":true}"#,
        )
        .unwrap();
        assert!(with.is_self);
        assert_eq!(with.pid, 101);
    }

    #[test]
    fn raw_process_list_splits_truncation_sentinel_and_keeps_self_flag() {
        // The truncated sentinel (pid 0) is dropped; real rows (incl. the
        // is_self helper) are kept.
        let raw: RawProcessList = serde_json::from_str(
            r#"{"procs":[
                {"pid":101,"name":"payload.elf","kind":"payload","is_self":true},
                {"pid":0,"truncated":true}
            ]}"#,
        )
        .unwrap();
        let truncated = raw.procs.iter().any(|p| p.truncated);
        let processes: Vec<_> = raw.procs.into_iter().filter(|p| p.pid > 0).collect();
        assert!(truncated);
        assert_eq!(processes.len(), 1);
        assert!(processes[0].is_self);
    }
}
