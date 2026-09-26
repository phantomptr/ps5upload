//! Console health check.
//!
//! Answers one question for the user: is my console set up correctly,
//! is ps5upload working correctly, and is anything wrong? See
//! `docs/superpowers/specs/2026-08-20-health-check-design.md`.
//!
//! The governing rule is that a check never guesses. Where the console
//! genuinely does not expose something -- which is common on retail
//! firmware -- the result is `Skip` with the reason, not a `Pass` we
//! cannot justify and not a `Fail` that trains users to ignore the
//! screen.
//!
//! All classification logic lives in free functions with no I/O so it
//! is unit-testable; `run_health_scan` only gathers the inputs.

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    /// Not applicable or not measurable here. Carries no judgement.
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckCategory {
    Connectivity,
    Runtime,
    Storage,
    System,
    RemotePlay,
    Hygiene,
}

/// A repair the engine can perform. A closed enum rather than a
/// command string: the UI asks for a named action, so it cannot ask
/// the engine to run something arbitrary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixAction {
    CreateToolDirs,
    CleanJunk,
    EnableRemotePlay,
    SyncClock,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    pub id: String,
    pub title: String,
    pub category: CheckCategory,
    pub status: CheckStatus,
    /// What we actually observed, with numbers where we have them.
    pub detail: String,
    /// What the user should do. Empty when the check passed.
    #[serde(default)]
    pub remedy: String,
    /// Present only when the engine can actually perform the repair.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<FixAction>,
}

impl HealthCheck {
    fn new(
        id: &str,
        title: &str,
        category: CheckCategory,
        status: CheckStatus,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            category,
            status,
            detail: detail.into(),
            remedy: String::new(),
            fix: None,
        }
    }

    fn with_remedy(mut self, remedy: impl Into<String>) -> Self {
        self.remedy = remedy.into();
        self
    }

    fn with_fix(mut self, fix: FixAction) -> Self {
        self.fix = Some(fix);
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthSummary {
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
    pub skip: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub addr: String,
    pub duration_ms: u64,
    pub checks: Vec<HealthCheck>,
    pub summary: HealthSummary,
}

// ── Pure classification ─────────────────────────────────────────────

/// Free-space verdict. Percentage-based rather than absolute: 20 GB
/// free is comfortable on a 1 TB console and nearly full on an 8 TB
/// one.
///
/// `total == 0` means the console did not report a size, which is a
/// missing measurement rather than a full disk -- so `Skip`, never
/// `Fail`.
pub fn classify_free_space(free_bytes: u64, total_bytes: u64) -> CheckStatus {
    if total_bytes == 0 {
        return CheckStatus::Skip;
    }
    let pct = (free_bytes as f64 / total_bytes as f64) * 100.0;
    if pct < 5.0 {
        CheckStatus::Fail
    } else if pct < 10.0 {
        CheckStatus::Warn
    } else {
        CheckStatus::Pass
    }
}

/// Percentage free, for display. Returns None when unmeasurable.
pub fn free_pct(free_bytes: u64, total_bytes: u64) -> Option<f64> {
    if total_bytes == 0 {
        None
    } else {
        Some((free_bytes as f64 / total_bytes as f64) * 100.0)
    }
}

/// Decode Sony's packed firmware word, which is **BCD**: 0x09600004 is
/// 9.60, not 9.96. Each byte holds two decimal digits, so any nibble
/// above 9 means the word is not a firmware version and we should not
/// pretend to have read one.
///
/// Returns None rather than a wrong string -- displaying "5.16" for
/// 0x05100023 is exactly the bug this guards.
pub fn decode_fw_bcd(magic: u32) -> Option<String> {
    if magic == 0 {
        return None;
    }
    let byte_to_dec = |b: u32| -> Option<u32> {
        let hi = (b >> 4) & 0x0f;
        let lo = b & 0x0f;
        if hi > 9 || lo > 9 {
            return None;
        }
        Some(hi * 10 + lo)
    };
    let major = byte_to_dec((magic >> 24) & 0xff)?;
    let minor = byte_to_dec((magic >> 16) & 0xff)?;
    Some(format!("{major}.{minor:02}"))
}

/// Compare the payload's reported version with the desktop's own.
///
/// A mismatch is the most common cause of "this feature does nothing":
/// the console is running a helper built from different source than the
/// app talking to it. Unknown version means an old payload that did not
/// report one -- worth flagging, but not a failure.
pub fn classify_version_match(payload: &str, engine: &str) -> CheckStatus {
    if payload.is_empty() {
        CheckStatus::Warn
    } else if payload == engine {
        CheckStatus::Pass
    } else {
        CheckStatus::Warn
    }
}

/// Is this directory entry leftover junk from an interrupted operation?
///
/// Deliberately conservative: only suffixes this tool itself creates
/// for work in progress. A real user file must never match, because
/// the cleanup action deletes what this returns true for.
pub fn is_junk_entry(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".part")
        || n.ends_with(".tmp")
        || n.ends_with(".partial")
        || n.ends_with(".staging")
        || n.ends_with(".download")
}

/// Roll individual checks up into counts.
pub fn summarize(checks: &[HealthCheck]) -> HealthSummary {
    let mut s = HealthSummary::default();
    for c in checks {
        match c.status {
            CheckStatus::Pass => s.pass += 1,
            CheckStatus::Warn => s.warn += 1,
            CheckStatus::Fail => s.fail += 1,
            CheckStatus::Skip => s.skip += 1,
        }
    }
    s
}

/// Human-readable byte size.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

// ── The scan ────────────────────────────────────────────────────────

/// Directories this tool needs on the console.
const TOOL_DIRS: [&str; 5] = [
    "/data/ps5upload/cheats",
    "/data/ps5upload/backups",
    "/data/ps5upload/spool",
    "/data/ps5upload/tx",
    "/data/ps5upload/runtime",
];

/// Read the payload's STATUS frame as raw JSON.
fn fetch_status(addr: &str) -> Result<serde_json::Value> {
    use crate::connection::Connection;
    use ftx2_proto::FrameType;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::Status, b"")?;
    let (_hdr, body) = c.recv_frame()?;
    Ok(serde_json::from_slice(&body)?)
}

/// Run every check against `addr` and return the report.
///
/// Individual checks degrade independently: a console that cannot
/// answer one frame still produces a report for everything else. A
/// gathering failure becomes a `Skip` naming what could not be read,
/// never a silent omission -- a missing row would read as "fine".
pub fn run_health_scan(addr: &str, engine_version: &str) -> HealthReport {
    let started = std::time::Instant::now();
    let mut checks: Vec<HealthCheck> = Vec::new();

    // ── connectivity + runtime ──────────────────────────────────────
    let status = fetch_status(addr);
    match &status {
        Ok(_) => checks.push(HealthCheck::new(
            "mgmt_reachable",
            "Helper is reachable",
            CheckCategory::Connectivity,
            CheckStatus::Pass,
            format!("The helper answered on {addr}."),
        )),
        Err(e) => checks.push(
            HealthCheck::new(
                "mgmt_reachable",
                "Helper is reachable",
                CheckCategory::Connectivity,
                CheckStatus::Fail,
                format!("No answer from {addr}: {e}"),
            )
            .with_remedy(
                "Load the ps5upload payload on your console, then scan again.                  Everything below depends on this.",
            ),
        ),
    }

    // Without the helper there is nothing further to measure. Say so
    // once rather than emitting a wall of failures that all have the
    // same single cause.
    let Ok(status) = status else {
        let summary = summarize(&checks);
        return HealthReport {
            addr: addr.to_string(),
            duration_ms: started.elapsed().as_millis() as u64,
            checks,
            summary,
        };
    };

    let payload_version = status
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let vstatus = classify_version_match(&payload_version, engine_version);
    let shown = if payload_version.is_empty() {
        "unknown".to_string()
    } else {
        payload_version.clone()
    };
    let mut vcheck = HealthCheck::new(
        "payload_version_match",
        "Helper matches this app",
        CheckCategory::Runtime,
        vstatus,
        format!("Console helper {shown}, desktop app {engine_version}."),
    );
    if vstatus != CheckStatus::Pass {
        vcheck = vcheck.with_remedy(
            "Send the payload again from this version of the app. A helper built              from different source is the most common reason a feature appears to              do nothing.",
        );
    }
    checks.push(vcheck);

    // ── Sony symbol resolution + firmware ───────────────────────────
    match crate::remoteplay::remoteplay_readiness(addr) {
        Ok(r) => {
            let sym_ok = r.symbols_ok != 0 && r.registry_err == 0;
            checks.push(
                HealthCheck::new(
                    "sony_symbols_resolved",
                    "System registry is readable",
                    CheckCategory::Runtime,
                    if sym_ok { CheckStatus::Pass } else { CheckStatus::Fail },
                    if sym_ok {
                        "The helper can read the console's system registry.".to_string()
                    } else {
                        format!(
                            "Registry unavailable (symbols_ok={}, err=0x{:08X}).",
                            r.symbols_ok, r.registry_err
                        )
                    },
                )
                .with_remedy(if sym_ok {
                    String::new()
                } else {
                    "Several features read the registry and will silently do nothing                      while this fails. Reload the payload; if it persists, report it."
                        .to_string()
                }),
            );

            match decode_fw_bcd(r.fw_magic) {
                Some(fw) => checks.push(HealthCheck::new(
                    "firmware_detected",
                    "Firmware detected",
                    CheckCategory::Runtime,
                    CheckStatus::Pass,
                    format!("System software {fw}."),
                )),
                None => checks.push(HealthCheck::new(
                    "firmware_detected",
                    "Firmware detected",
                    CheckCategory::Runtime,
                    CheckStatus::Skip,
                    "The console did not report a readable firmware version.".to_string(),
                )),
            }

            // Remote Play
            let svc = r.service_enabled != 0;
            let mut rp = HealthCheck::new(
                "rp_service_enabled",
                "Remote Play is enabled",
                CheckCategory::RemotePlay,
                if svc {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Warn
                },
                if svc {
                    "Remote Play is switched on.".to_string()
                } else {
                    "Remote Play is switched off on this console.".to_string()
                },
            );
            if !svc {
                rp = rp
                    .with_remedy("Turn Remote Play on.")
                    .with_fix(FixAction::EnableRemotePlay);
            }
            checks.push(rp);

            let activated = r.account_id_raw != 0 && r.account_type == "np";
            checks.push(
                HealthCheck::new(
                    "rp_account_activated",
                    "Account is usable",
                    CheckCategory::RemotePlay,
                    if activated { CheckStatus::Pass } else { CheckStatus::Warn },
                    if activated {
                        "The signed-in account has an id and is PSN-linked.".to_string()
                    } else {
                        "No usable account is signed in on this console.".to_string()
                    },
                )
                .with_remedy(if activated {
                    String::new()
                } else {
                    "Remote Play needs a signed-in account. Sign in on the console,                      or activate the profile from the Profile screen."
                        .to_string()
                }),
            );
        }
        Err(e) => checks.push(HealthCheck::new(
            "sony_symbols_resolved",
            "System registry is readable",
            CheckCategory::Runtime,
            CheckStatus::Skip,
            format!("Could not read console readiness: {e}"),
        )),
    }

    // ── storage ─────────────────────────────────────────────────────
    match crate::hw::hw_storage(addr) {
        Ok(st) => {
            let status = classify_free_space(st.free_bytes, st.total_bytes);
            let detail = match free_pct(st.free_bytes, st.total_bytes) {
                Some(pct) => format!(
                    "{} free of {} ({pct:.1}%).",
                    human_bytes(st.free_bytes),
                    human_bytes(st.total_bytes)
                ),
                None => "The console did not report a storage size.".to_string(),
            };
            checks.push(
                HealthCheck::new(
                    "internal_free_space",
                    "Console storage",
                    CheckCategory::Storage,
                    status,
                    detail,
                )
                .with_remedy(match status {
                    CheckStatus::Fail => {
                        "Very little space left. Installs and transfers will fail.                          Delete something before continuing."
                            .to_string()
                    }
                    CheckStatus::Warn => {
                        "Space is getting tight. Large installs may fail.".to_string()
                    }
                    _ => String::new(),
                }),
            );
        }
        Err(e) => checks.push(HealthCheck::new(
            "internal_free_space",
            "Console storage",
            CheckCategory::Storage,
            CheckStatus::Skip,
            format!("Could not read storage: {e}"),
        )),
    }

    // ── tool directories + junk ─────────────────────────────────────
    let mut missing: Vec<&str> = Vec::new();
    let mut junk_files: Vec<(String, u64)> = Vec::new();
    let mut dirs_readable = true;
    for dir in TOOL_DIRS {
        match crate::fs_ops::list_dir(addr, dir, Default::default()) {
            Ok(listing) => {
                for e in &listing.entries {
                    if e.kind != "dir" && is_junk_entry(&e.name) {
                        junk_files.push((format!("{dir}/{}", e.name), e.size));
                    }
                }
            }
            Err(_) => {
                // Cannot distinguish "absent" from "unreadable" here, so
                // treat both as "needs creating" -- creating an existing
                // directory is harmless.
                missing.push(dir);
                dirs_readable = false;
            }
        }
    }

    let mut dircheck = HealthCheck::new(
        "tool_dirs_present",
        "Tool folders exist",
        CheckCategory::Storage,
        if missing.is_empty() {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        if missing.is_empty() {
            "All folders ps5upload needs are present.".to_string()
        } else {
            format!("Missing or unreadable: {}.", missing.join(", "))
        },
    );
    if !missing.is_empty() {
        dircheck = dircheck
            .with_remedy("Create the missing folders.")
            .with_fix(FixAction::CreateToolDirs);
    }
    checks.push(dircheck);

    let junk_total: u64 = junk_files.iter().map(|(_, s)| s).sum();
    let mut junkcheck = if !dirs_readable && junk_files.is_empty() {
        HealthCheck::new(
            "junk_files",
            "Leftover files",
            CheckCategory::Hygiene,
            CheckStatus::Skip,
            "Could not read every tool folder, so leftovers were not counted.".to_string(),
        )
    } else {
        HealthCheck::new(
            "junk_files",
            "Leftover files",
            CheckCategory::Hygiene,
            if junk_files.is_empty() {
                CheckStatus::Pass
            } else {
                CheckStatus::Warn
            },
            if junk_files.is_empty() {
                "No leftover files from interrupted transfers.".to_string()
            } else {
                format!(
                    "{} leftover file(s) from interrupted work, {}.",
                    junk_files.len(),
                    human_bytes(junk_total)
                )
            },
        )
    };
    if !junk_files.is_empty() {
        junkcheck = junkcheck
            .with_remedy("Delete them. Only files ps5upload created are removed.")
            .with_fix(FixAction::CleanJunk);
    }
    checks.push(junkcheck);

    // ── console clock ───────────────────────────────────────────────
    match crate::sys_time::ps5_time_get(addr) {
        Ok(t) if t.ok => {
            match t.to_unix_seconds() {
                Some(console_unix) => {
                    let host = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    let skew = (console_unix - host).abs();
                    // Five minutes: past normal drift, short of anything
                    // that breaks licence checks on its own.
                    let status = if skew > 300 {
                        CheckStatus::Warn
                    } else {
                        CheckStatus::Pass
                    };
                    let mut c = HealthCheck::new(
                        "clock_sane",
                        "Console clock",
                        CheckCategory::System,
                        status,
                        format!("Console clock differs from this computer by {skew}s."),
                    );
                    if status != CheckStatus::Pass {
                        c = c
                            .with_remedy(
                                "A wrong clock breaks licence checks and makes logs                                  hard to read. Set it from this computer.",
                            )
                            .with_fix(FixAction::SyncClock);
                    }
                    checks.push(c);
                }
                None => checks.push(HealthCheck::new(
                    "clock_sane",
                    "Console clock",
                    CheckCategory::System,
                    CheckStatus::Skip,
                    "The console reported a date that could not be interpreted.".to_string(),
                )),
            }
        }
        Ok(t) => checks.push(HealthCheck::new(
            "clock_sane",
            "Console clock",
            CheckCategory::System,
            CheckStatus::Skip,
            format!(
                "The console did not report its clock: {}.",
                crate::sys_time::humanize_err(t.err_code)
            ),
        )),
        Err(e) => checks.push(HealthCheck::new(
            "clock_sane",
            "Console clock",
            CheckCategory::System,
            CheckStatus::Skip,
            format!("Could not read the console clock: {e}"),
        )),
    }

    let summary = summarize(&checks);
    HealthReport {
        addr: addr.to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        checks,
        summary,
    }
}

// ── Repairs ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixOutcome {
    pub action: FixAction,
    pub ok: bool,
    /// What was actually done, itemised. Empty on failure.
    #[serde(default)]
    pub changed: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Everything the cleanup action is willing to delete, gathered but not
/// yet removed. Returned to the UI first so the user sees the exact
/// list before anything is destroyed.
pub fn preview_junk(addr: &str) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    for dir in TOOL_DIRS {
        let Ok(listing) = crate::fs_ops::list_dir(addr, dir, Default::default()) else {
            continue;
        };
        for e in &listing.entries {
            if e.kind != "dir" && is_junk_entry(&e.name) {
                out.push((format!("{dir}/{}", e.name), e.size));
            }
        }
    }
    out
}

/// Perform one repair.
///
/// Every branch is a named, bounded operation. `CleanJunk` deletes only
/// paths that `preview_junk` produced -- which are, by construction,
/// work-in-progress files under directories this tool owns. It never
/// walks user content.
pub fn apply_fix(addr: &str, action: &FixAction, engine_version: &str) -> FixOutcome {
    let _ = engine_version;
    let mut changed = Vec::new();
    let result: Result<()> = (|| {
        match action {
            FixAction::CreateToolDirs => {
                for dir in TOOL_DIRS {
                    if crate::fs_ops::list_dir(addr, dir, Default::default()).is_ok() {
                        continue;
                    }
                    crate::fs_ops::fs_mkdir(addr, dir)?;
                    changed.push(format!("created {dir}"));
                }
                if changed.is_empty() {
                    changed.push("all folders already present".to_string());
                }
            }
            FixAction::CleanJunk => {
                for (path, size) in preview_junk(addr) {
                    crate::fs_ops::fs_delete(addr, &path)?;
                    changed.push(format!("deleted {path} ({})", human_bytes(size)));
                }
                if changed.is_empty() {
                    changed.push("nothing to clean".to_string());
                }
            }
            FixAction::EnableRemotePlay => {
                crate::remoteplay::remoteplay_enable(addr, "service")?;
                changed.push("Remote Play switched on".to_string());
            }
            FixAction::SyncClock => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                crate::sys_time::ps5_time_set(addr, now)?;
                changed.push("console clock set from this computer".to_string());
            }
        }
        Ok(())
    })();

    match result {
        Ok(()) => FixOutcome {
            action: action.clone(),
            ok: true,
            changed,
            error: None,
        },
        Err(e) => FixOutcome {
            action: action.clone(),
            ok: false,
            // Report what already succeeded before the failure rather
            // than implying nothing happened.
            changed,
            error: Some(format!("{e:#}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gb(n: u64) -> u64 {
        n * 1024 * 1024 * 1024
    }

    #[test]
    fn free_space_thresholds() {
        assert_eq!(classify_free_space(gb(500), gb(1000)), CheckStatus::Pass);
        assert_eq!(classify_free_space(gb(100), gb(1000)), CheckStatus::Pass);
        // exactly 10% is still fine; below it warns
        assert_eq!(classify_free_space(gb(99), gb(1000)), CheckStatus::Warn);
        assert_eq!(classify_free_space(gb(50), gb(1000)), CheckStatus::Warn);
        assert_eq!(classify_free_space(gb(49), gb(1000)), CheckStatus::Fail);
        assert_eq!(classify_free_space(0, gb(1000)), CheckStatus::Fail);
    }

    #[test]
    fn unmeasured_storage_is_skip_not_fail() {
        // A console that reported nothing is not a console that is full.
        assert_eq!(classify_free_space(0, 0), CheckStatus::Skip);
        assert_eq!(free_pct(0, 0), None);
    }

    #[test]
    fn firmware_decodes_as_bcd_not_hex() {
        // The two consoles this was verified against.
        assert_eq!(decode_fw_bcd(0x09600004).as_deref(), Some("9.60"));
        assert_eq!(decode_fw_bcd(0x05100023).as_deref(), Some("5.10"));
        // 0x10 is BCD ten, not hex sixteen -- FW 10.00.
        assert_eq!(decode_fw_bcd(0x10000000).as_deref(), Some("10.00"));
    }

    #[test]
    fn firmware_rejects_non_bcd_words() {
        // 0x0A.. has a nibble above 9, so it is not a version word.
        // Returning None beats displaying a confident wrong number.
        assert_eq!(decode_fw_bcd(0x0A600004), None);
        assert_eq!(decode_fw_bcd(0x09F00004), None);
        assert_eq!(decode_fw_bcd(0), None);
    }

    #[test]
    fn version_match_classification() {
        assert_eq!(classify_version_match("5.3.3", "5.3.3"), CheckStatus::Pass);
        assert_eq!(classify_version_match("5.3.2", "5.3.3"), CheckStatus::Warn);
        assert_eq!(classify_version_match("", "5.3.3"), CheckStatus::Warn);
    }

    #[test]
    fn junk_matches_only_work_in_progress_suffixes() {
        assert!(is_junk_entry("Game.pkg.part"));
        assert!(is_junk_entry("upload.TMP"));
        assert!(is_junk_entry("x.partial"));
        assert!(is_junk_entry("y.staging"));
    }

    #[test]
    fn junk_never_matches_real_user_files() {
        // The cleanup action deletes whatever this returns true for,
        // so a false positive costs the user a real file.
        for name in [
            "Game.pkg",
            "CUSA00002_01.00.json",
            "backup.zip",
            "important.txt",
            "partition", // starts with "part" but is not a suffix
            "tmpfile",   // contains "tmp" but is not a suffix
            "notes.partly",
        ] {
            assert!(!is_junk_entry(name), "{name} must not be treated as junk");
        }
    }

    #[test]
    fn tool_dirs_stay_inside_our_own_folder() {
        // CleanJunk deletes `{TOOL_DIR}/{name}` for names readdir
        // returned. That is only safe while every TOOL_DIR is ours --
        // adding a directory outside this prefix would turn the
        // cleanup button into a way to delete user content.
        for d in TOOL_DIRS {
            assert!(
                d.starts_with("/data/ps5upload/"),
                "{d} is outside the tool's own folder"
            );
            assert!(!d.contains(".."), "{d} must not contain a parent traversal");
        }
    }

    #[test]
    fn summarize_counts_each_status() {
        let checks = vec![
            HealthCheck::new("a", "A", CheckCategory::Runtime, CheckStatus::Pass, ""),
            HealthCheck::new("b", "B", CheckCategory::Runtime, CheckStatus::Warn, ""),
            HealthCheck::new("c", "C", CheckCategory::Storage, CheckStatus::Fail, ""),
            HealthCheck::new("d", "D", CheckCategory::Storage, CheckStatus::Skip, ""),
            HealthCheck::new("e", "E", CheckCategory::System, CheckStatus::Pass, ""),
        ];
        let s = summarize(&checks);
        assert_eq!(
            s,
            HealthSummary {
                pass: 2,
                warn: 1,
                fail: 1,
                skip: 1
            }
        );
    }

    #[test]
    fn human_bytes_reads_naturally() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(gb(2)), "2.0 GB");
    }

    #[test]
    fn fix_actions_serialize_as_stable_slugs() {
        // The UI sends these back by name; renaming one silently breaks
        // every fix button.
        let j = serde_json::to_string(&FixAction::CleanJunk).unwrap();
        assert_eq!(j, "\"clean_junk\"");
        let j = serde_json::to_string(&FixAction::EnableRemotePlay).unwrap();
        assert_eq!(j, "\"enable_remote_play\"");
    }
}
