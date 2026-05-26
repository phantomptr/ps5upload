//! Engine→payload client for the PKG_INSTALL frame family.
//!
//! Sends a `.pkg` URL (HTTP, served by the engine's pkg-host listener)
//! to the payload, which calls `sceBgftServiceDownloadRegisterTask` +
//! `sceBgftServiceIntDownloadStartTask` to drive Sony's BGFT installer.
//! Sony's BGFT then HTTP-pulls the bytes from our listener, decrypts
//! with device keys, and installs the title.
//!
//! The payload returns a task_id immediately; the actual install runs
//! asynchronously inside PS5 firmware. Use `pkg_install_status` to
//! poll progress + the final outcome.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

/// Request body sent in the PKG_INSTALL frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgInstallRequest {
    /// Full HTTP URL the PS5's BGFT will fetch the `.pkg` from.
    /// Engine-hosted, ephemeral, session-token-gated.
    pub url: String,
    /// 36-byte content_id from the PKG header. BGFT records this in
    /// its task table so it can deduplicate against installed titles.
    pub content_id: String,
    /// Total size in bytes (sum of split parts when applicable).
    pub size: u64,
    /// Display title — used by BGFT for the "Downloads" notification
    /// and possibly the install progress UI on the PS5.
    pub title: String,
    /// BGFT package_type ("PS4GD", "PS4AC", etc). Affects install
    /// destination and DRM behavior; mismatch usually surfaces as a
    /// non-zero err_code from BGFT itself.
    pub package_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgInstallResponse {
    pub task_id: i32,
    /// 0 on accept; non-zero is a BGFT error code (e.g. 0x80990088 =
    /// already installed). Caller maps to a user-facing message.
    pub err_code: u32,
    /// Human-readable detail when the err_code alone isn't enough —
    /// e.g. "libSceBgft.sprx not loadable on this firmware".
    #[serde(default)]
    pub detail: String,
    /// Which BGFT Register variant the payload used:
    /// `"intdebug"` — `sceBgftServiceIntDebugDownloadRegisterPkg` (the
    ///                fakepkg-friendly path; bypasses entitlement check).
    /// `"regular"`  — `sceBgftServiceDownloadRegisterTask` (entitlement
    ///                checked; only succeeds for Sony-signed pkgs).
    /// `"none"`     — Register hasn't been attempted yet, OR BGFT is
    ///                unavailable on this firmware.
    /// Added in 2.2.52. Older payloads omit the field; serde defaults
    /// to "" so a missing value doesn't crash decode.
    #[serde(default)]
    pub register_path: String,
    /// Whether `sceBgftServiceIntDebugDownloadRegisterPkg` was resolvable
    /// at payload init. False (or missing) means fakepkg installs will
    /// likely fail with entitlement errors regardless of cred state —
    /// the symbol simply isn't exposed on this firmware. Added in 2.2.52.
    #[serde(default)]
    pub intdebug_avail: bool,
    /// Whether the payload's process-wide ucred elevation succeeded
    /// (mirrors STATUS_ACK's `ucred_elevated`). False means the loader
    /// didn't grant kernel R/W; no Int-family BGFT call will work.
    /// Added in 2.2.52.
    #[serde(default)]
    pub kernel_rw: bool,
    /// Per-tier error codes from the most recent install attempt.
    /// `None` = tier wasn't attempted; `Some(0)` = tier completed without
    /// error; `Some(other)` = tier returned that err_code.
    /// `shellui_err` covers the ShellUI-RPC tier (Tier 1); `appinst_err`
    /// covers the in-process AppInstUtil tier (Tier 2). The legacy
    /// `err_code` field carries whichever tier's error the payload
    /// surfaced (typically the BGFT direct tier or the saved app err) —
    /// per-tier fields disambiguate WHERE a multi-tier failure broke.
    /// Added in 2.2.52-fix.
    #[serde(default)]
    pub shellui_err: Option<u32>,
    #[serde(default)]
    pub appinst_err: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallPhase {
    /// BGFT registered the task but hasn't started downloading yet.
    Queued,
    /// BGFT is pulling bytes from our HTTP listener.
    Download,
    /// All bytes received; Sony's installer is decrypting + writing.
    Install,
    /// Installer reported success; the title should be in Library.
    Done,
    /// BGFT or installer reported failure; `err_code` carries Sony's code.
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgInstallStatus {
    pub phase: InstallPhase,
    pub downloaded: u64,
    pub total: u64,
    pub err_code: u32,
    #[serde(default)]
    pub detail: String,
    /// Live diagnostics — same shape as `PkgInstallResponse`, re-emitted
    /// by the payload on every status frame so the host sees the
    /// CURRENT (not start-time) BGFT register_path / intdebug_avail /
    /// kernel_rw state. If BGFT transitions to phase=error mid-install,
    /// this lets the user's "Why?" disclosure show real context rather
    /// than the optimistic snapshot from start. Added in 2.2.52;
    /// older payloads omit them and serde defaults take over.
    #[serde(default)]
    pub register_path: String,
    #[serde(default)]
    pub intdebug_avail: bool,
    #[serde(default)]
    pub kernel_rw: bool,
    /// Per-tier error codes — same semantic as `PkgInstallResponse`.
    /// Re-emitted on every status poll so a mid-install transition
    /// to error refreshes the breakdown the user sees.
    #[serde(default)]
    pub shellui_err: Option<u32>,
    #[serde(default)]
    pub appinst_err: Option<u32>,
}

/// Send the PKG_INSTALL frame and parse the ack.
pub fn pkg_install(addr: &str, req: &PkgInstallRequest) -> Result<PkgInstallResponse> {
    let body = serde_json::to_vec(req)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::PkgInstall, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected PKG_INSTALL: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::PkgInstallAck {
        bail!("expected PKG_INSTALL_ACK, got {ft:?}");
    }
    let parsed: PkgInstallResponse = serde_json::from_slice(&resp)?;
    Ok(parsed)
}

/// Poll an in-flight install for status. Cheap on the payload side —
/// just a `sceBgftServiceDownloadGetProgress` call. Caller polls every
/// 1 s while the install is active.
pub fn pkg_install_status(addr: &str, task_id: i32) -> Result<PkgInstallStatus> {
    let body = serde_json::json!({ "task_id": task_id });
    let body = serde_json::to_vec(&body)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::PkgInstallStatus, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected PKG_INSTALL_STATUS: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::PkgInstallStatusAck {
        bail!("expected PKG_INSTALL_STATUS_ACK, got {ft:?}");
    }
    let parsed: PkgInstallStatus = serde_json::from_slice(&resp)?;
    Ok(parsed)
}

/// Map a Sony BGFT err_code to a user-facing message. Codes from
/// community PS4/PS5 references.
/// Returns None for unknown codes — caller falls back to the raw hex.
/// Bits set by the payload's bgft.c on synthetic task_ids to indicate
/// which install tier accepted the request. Mirrors the C constants
/// `APPINST_TASK_ID_FLAG` (0x40000000) and `APPINST_VIA_SHELLUI_FLAG`
/// (0x20000000) in `payload/src/bgft.c`. The host derives the tier name
/// from the task_id alone — no protocol/frame change required, so older
/// payloads work too. We just classify what tier number their task_id
/// shape implies.
const APPINST_TASK_ID_FLAG: i32 = 0x40000000;
const APPINST_VIA_SHELLUI_FLAG: i32 = 0x20000000;
/// v2.16.1 scaffolding for the eventual separate-process Tier-0 helper.
/// Set on task_ids issued by the in-process worker-thread variant gated
/// behind `PS5UPLOAD_TIER0_WORKER=1`. The worker serializes Sony
/// installer calls into a single thread to eliminate multi-thread
/// races on Sony's kernel-stub state and centralise authid handling;
/// it does NOT yet provide true cred isolation (ucred is per-process,
/// so sibling threads still see the swap). The real Tier-0 — a
/// dedicated helper PROCESS that owns ShellCore-authid from birth —
/// is deferred to v2.17.0 because it needs a new build artefact,
/// deploy mechanism, lifecycle management, and hardware iteration
/// rounds we can't do mid-release.
const APPINST_VIA_TIER0_FLAG: i32 = 0x10000000;

/// Human-readable tier identifier for an in-flight install. Surfaced
/// alongside `task_id` so the desktop's "Why?" diagnostic disclosure
/// can say *which* of the install paths accepted the request instead
/// of leaving the raw flags for the reader to decode. Returned strings:
/// `"tier0-worker"` (Tier 0, scaffolded opt-in for v2.16.1 — full
/// implementation in v2.17.0), `"in-proc-appinst"` (Tier 1, in-process
/// `sceAppInstUtilInstallByPackage` with per-call ucred-authid swap),
/// `"shellui-rpc"` (Tier 2, ptrace into SceShellUI and install runs
/// from there), and `"direct-bgft"` (Tier 3, legacy
/// `sceBgftServiceIntDebugDownloadRegisterPkg`).
pub fn via_tier(task_id: i32) -> &'static str {
    // Failure sentinel: the payload uses -1 (all bits set in two's
    // complement) when no tier accepted the install. We discovered
    // this during a v2.16.1 hardware test against an NPXS pkg where
    // all 3 tiers failed — the previous algorithm read the all-bits-
    // set task_id as "every synthetic flag is also set" and surfaced
    // `via=tier0-worker` for a complete failure, which was deeply
    // misleading. A task_id < 0 means "no tier accepted" — surface
    // "none" so the UI can clearly say "no tier reached register."
    if task_id < 0 {
        return "none";
    }
    if (task_id & APPINST_TASK_ID_FLAG) == 0 {
        // Raw BGFT task_id — never bears our synthetic flags.
        return "direct-bgft";
    }
    // Tier ordering inside the synthetic-id space: TIER0 wins if set
    // (it's the most-derived path), then SHELLUI, then plain APPINST.
    // Order matters because nothing in bgft.c sets both VIA_SHELLUI
    // and VIA_TIER0 simultaneously — but if a future code path ever
    // does, the user wants the most-specific label.
    if (task_id & APPINST_VIA_TIER0_FLAG) != 0 {
        "tier0-worker"
    } else if (task_id & APPINST_VIA_SHELLUI_FLAG) != 0 {
        "shellui-rpc"
    } else {
        "in-proc-appinst"
    }
}

pub fn err_code_message(code: u32) -> Option<&'static str> {
    match code {
        0x0000_0000 => None, // success — no message
        0x8099_0001 => Some("BGFT initialised already (benign)"),
        0x8099_0036 => Some("DRM mismatch — this PKG isn't valid for this console"),
        0x8099_0038 => Some("PKG entitlement check failed — wrong account / region"),
        0x8099_0039 => Some("Out of free space on the PS5"),
        0x8099_0085 => Some("Need defragmented free space — Settings → Storage → Free up space"),
        0x8099_0086 => Some("Leftover download in notifications — clear it from the PS5 first"),
        0x8099_0088 => Some("This title is already installed"),
        0x80A3_0026 => Some("Out of free space on the PS5"),
        // PlayGo (BGFT's HTTP fetcher) — the engine returned a non-2xx
        // response to BGFT during the install pull. Usually means the
        // /pkg-host URL session expired or the engine restarted mid-
        // install. Restart the install from the desktop.
        0x80B2_2404 => Some("PS5 couldn't fetch the .pkg from this PC — restart the install"),
        // Register-time rejection: the PKG header didn't validate.
        // Most often caused by:
        //   - mismatched filename vs header content_id (canonicalisation
        //     in pkg_url_filename now mitigates this)
        //   - corrupt or truncated .pkg upload
        //   - using `file://` prefix on FW 9.60+ instead of bare path
        0x80B2_1106 => Some("PS5 rejected the PKG header — file may be corrupt or wrongly named"),
        // BGFT-layer download-task errors: a previous failed/cancelled
        // task for the same content_id is still in the queue. User
        // needs to clear it from PS5 notifications, OR our automatic
        // pre-install cancel pass didn't reach it (rare).
        0x80B2_2101 => Some("Earlier download still queued — open PS5 notifications and clear it"),
        // sceLncUtilLaunchApp / app-install service refused. Title is
        // gated by parental control, age rating, or a Sony content
        // policy; not something we can work around.
        0x80B6_4002 => Some("Title blocked by PS5 parental / content controls"),
        // SCE_KERNEL_ERROR_ESRCH — Sony's install IPC tried to talk to
        // a process that no longer exists. In our context this almost
        // always means the payload hasn't elevated yet (kstuff/etaHEN
        // not loaded) so ShellCore's installer daemon can't find a
        // peer process matching our cred. Send the payload again from
        // Connection (which runs the elevation chain), then retry.
        0x8002_0005 => Some(
            "PS5 install daemon couldn't reach our process (ESRCH) — re-send the payload and retry",
        ),
        // sceAppInstUtilInstallByPackage rejected by Sony's installer —
        // most often because the pkg is a system patch (NPXS-prefix)
        // and the API isn't designed for them, OR because the firmware
        // doesn't expose the BGFT symbols our payload depends on.
        // Hardware-observed on FW 9.60 when all 3 tiers fail.
        0x80B2_116F => Some(
            "PS5 installer rejected the request — pkg may be a system patch (use Settings → Debug Settings → Game → Package Installer) or the firmware lacks the BGFT entry points we need",
        ),
        // ShellUI-RPC tier reject — the install path that routes through
        // SceShellUI's process attributes returned 0x80B21401. Most
        // often paired with 0x80B2116F on FW 9.60 when the firmware
        // lacks the BGFT registers our payload tries to use.
        0x80B2_1401 => Some(
            "PS5 ShellUI install path rejected the request — likely a firmware-point or pkg-format incompatibility",
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_err_codes_have_messages() {
        assert!(err_code_message(0x80990088).is_some());
        assert!(err_code_message(0x80990085).is_some());
        assert!(err_code_message(0).is_none());
        assert!(err_code_message(0xDEADBEEF).is_none());
    }

    #[test]
    fn via_tier_classifies_task_id_bits() {
        // Failure sentinel — was reported as "tier0-worker" before
        // the v2.16.1 hardware test caught the sign-bit bug.
        assert_eq!(via_tier(-1), "none");
        // Raw BGFT task_id (no synthetic flags): legacy Tier 3.
        assert_eq!(via_tier(42), "direct-bgft");
        assert_eq!(via_tier(0), "direct-bgft");
        // Tier-1 in-proc appinst: APPINST_TASK_ID_FLAG only.
        assert_eq!(via_tier(0x40000005), "in-proc-appinst");
        // Tier-2 ShellUI RPC: both flags set.
        assert_eq!(via_tier(0x60000003), "shellui-rpc");
        // Tier-0 worker (scaffolded v2.16.1): TIER0_FLAG + TASK_ID_FLAG.
        assert_eq!(via_tier(0x50000007), "tier0-worker");
    }

    #[test]
    fn newly_added_err_codes_have_messages() {
        // Codes added in v2.16.1 after audit found we were leaving these
        // as raw hex in the diagnostic panel.
        assert!(err_code_message(0x80990038).is_some());
        assert!(err_code_message(0x80B22404).is_some());
        assert!(err_code_message(0x80B21106).is_some());
        assert!(err_code_message(0x80B22101).is_some());
        assert!(err_code_message(0x80B64002).is_some());
        assert!(err_code_message(0x80020005).is_some());
        // Added during v2.16.1 hardware test against an NPXS pkg on
        // FW 9.60 — all 3 tiers returned these and the UI was showing
        // raw hex with no actionable copy.
        assert!(err_code_message(0x80B2116F).is_some());
        assert!(err_code_message(0x80B21401).is_some());
    }

    #[test]
    fn install_request_round_trips() {
        let req = PkgInstallRequest {
            url: "http://10.0.0.1:48710/pkg-host/abc/file.pkg".into(),
            content_id: "EP0006-CUSA12345_00-FOOBARBAZ0123456".into(),
            size: 1_234_567_890,
            title: "Test Title".into(),
            package_type: "PS4GD".into(),
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: PkgInstallRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.content_id, req.content_id);
        assert_eq!(back.size, req.size);
    }

    #[test]
    fn phase_serializes_snake_case() {
        let s = serde_json::to_string(&InstallPhase::Download).unwrap();
        assert_eq!(s, "\"download\"");
        let p: InstallPhase = serde_json::from_str("\"done\"").unwrap();
        assert_eq!(p, InstallPhase::Done);
    }
}
