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
    /// Optional install-method selector. When set, the payload runs ONLY
    /// that single tier (instead of its internal best-effort cascade) and
    /// reports its raw result — this is what lets the engine drive a
    /// verify-and-fall-through cascade (try a method, confirm the content
    /// actually landed under `/user/app/<id>/app.pkg`, else try the next).
    /// Recognised values:
    ///   "appinst-bypackage" — sceAppInstUtilInstallByPackage (local path
    ///                         or http URL); the FW-portable primary.
    ///   "appinst-local"     — sceAppInstUtilAppInstallPkg (local-disk).
    ///                         Hardware-proven to be a silent no-op on FW
    ///                         5.10 (returns 0, copies nothing) — kept only
    ///                         as a probe / last resort.
    ///   "shellui"           — ptrace into SceShellUI and install from there.
    ///   "bgft"              — legacy BGFT IntDebug register/start.
    /// `None`/"auto" keeps the payload's legacy internal cascade (back-compat
    /// with older engines that don't drive the cascade themselves).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
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

/// Whether the install tier that accepted this request registers a title
/// that may **not launch** ("can't start the game or app", CE-).
///
/// `register_path == "appinst-local"` is the payload's
/// `sceAppInstUtilAppInstallPkg` path. On some firmwares (the FW-12.xx
/// symptom users report) it installs content but registers the title in a
/// state the PS5 launcher rejects. Since 2.27.x the payload's bgft.c reaches
/// this path only as an ABSOLUTE LAST RESORT — after the in-process
/// `InstallByPackage`, the SceShellUI RPC fallback, AND legacy BGFT IntDebug
/// have all failed — so seeing it means "installed, but it may not boot."
/// Every other `register_path` ("appinst", "shellui-rpc", "intdebug",
/// "regular") is a launchable path. The host surfaces a warning for this
/// case instead of a clean success, steering the user to re-install via the
/// PS5's Settings → Package Installer if the title won't start.
pub fn install_may_not_launch(register_path: &str) -> bool {
    register_path == "appinst-local"
}

/// Whether to PRESERVE the staged .pkg after an in-process register-reject,
/// instead of auto-deleting it.
///
/// A guarded patch ("…DP") that the in-process `InstallByPackage` rejects
/// (typically `0x80B21106`, the FW 11/12 authid gate) is the EXPECTED first
/// step: the client then retries the SAME staged file through the standalone
/// DPI daemon — a separate, properly-authid'd loader process that is HW-proven
/// to land patches the in-process path can't. Deleting the file on reject pulls
/// it out from under that fallback and the update can never apply (HW bug
/// bundle, FW 12.70). For every other type the auto-clean stands: it keeps a
/// failed register idempotent and stops a stale pkg polluting Sony's installer
/// queue. The client owns cleanup of its transient copy after the full cascade.
pub fn preserve_staging_on_reject(package_type: &str) -> bool {
    package_type.ends_with("DP")
}

/// Whether `s` has the shape of a PS5 title_id: four uppercase letters
/// (CUSA / PPSA / NPXS / …) followed by five digits, e.g. "CUSA12345".
/// Mirrors `looks_like_title_id` in the payload's register.c so the two
/// stay in agreement about what counts as a real title.
fn looks_like_title_id(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 9
        && b[..4].iter().all(u8::is_ascii_uppercase)
        && b[4..].iter().all(u8::is_ascii_digit)
}

/// Derive the PS5 title_id from a PKG content_id.
///
/// A content_id has the shape `IV9999-CUSA12345_00-ZGAMEFOO00000000`
/// (region tag, '-', title_id, '_', label). elf-arsenal extracts the same
/// field — the token between the first '-' and the following '_' — to key
/// its `wait_for_install_row` app.db check. We reproduce that exactly.
///
/// Returns `None` when the content_id is empty/malformed, when the derived
/// token isn't a real title_id shape, or when it names a `FAKE…`
/// placeholder. elf-arsenal treats a "FAKE00000" titleId as a failed
/// install (a real launchable title never carries one); we instead treat
/// "can't derive a real title_id" as "verification not applicable" so the
/// caller stays on the legacy optimistic path rather than failing an
/// install we simply can't check.
pub fn title_id_from_content_id(content_id: &str) -> Option<String> {
    let cid = content_id.trim();
    if cid.is_empty() {
        return None;
    }
    // Token after the first '-', up to the next '_'.
    let after_dash = cid.split_once('-')?.1;
    let title = after_dash.split('_').next()?;
    if title.starts_with("FAKE") || !looks_like_title_id(title) {
        return None;
    }
    Some(title.to_string())
}

/// Outcome of a launchability check — the FW-safe analogue of
/// elf-arsenal's `wait_for_install_row` poll of `tbl_contentinfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchCheck {
    /// The title_id is registered on the console (present under
    /// `/user/app/<title_id>/`, and/or in app.db) — the install produced a
    /// launchable title.
    Registered,
    /// The console is reachable and enumerable but the title_id is not
    /// (yet) registered. During the verification window this means "still
    /// promoting"; past it, "installed but never became launchable".
    Absent,
    /// Verification couldn't be performed — either we couldn't derive a
    /// real title_id from the content_id, or neither the filesystem
    /// enumeration nor app.db could be read (RPC failure). The caller keeps
    /// the legacy optimistic behavior — no regression.
    Unsupported,
}

/// Check whether `content_id`'s title is registered (and therefore
/// launchable) on the PS5.
///
/// elf-arsenal verifies installs by polling app.db (`tbl_contentinfo`). We
/// reproduce the *semantics* — "did the title row materialize?" — but the
/// **primary** source is the filesystem enumeration (`app_list_registered`
/// → `/user/app/<title_id>/` scan), NOT sqlite. Hardware on FW 9.60 proved
/// the reason: the dlsym-only `AppDbQuery` returns `sqlite_unavailable`
/// there (nothing `dlopen`s libSceSqlite — by design, since that hangs on
/// 9.60), so an app.db-only check would no-op on the most common jailbreak
/// firmware. The `/user/app/` scan works on every firmware (it's what the
/// codebase already uses for the Library "installed" filter) and is the
/// on-disk equivalent of Sony's app.db row — "any title Sony's XMB knows
/// about has a /user/app/<id>/ directory" (register.h).
///
/// app.db (via `AppDbQuery`) is consulted as a **supplement** when the
/// filesystem scan doesn't (yet) show the title and sqlite happens to be
/// readable (newer firmwares) — it can surface a title that's registered
/// in the DB a beat before the `/user/app` enumeration reflects it.
///
/// Any unverifiable case (no title_id, both sources unreadable) returns
/// `Unsupported`, so a check we can't perform never fails a real install.
pub fn verify_launchable(addr: &str, content_id: &str) -> LaunchCheck {
    match title_id_from_content_id(content_id) {
        Some(title_id) => verify_title_registered(addr, &title_id),
        None => LaunchCheck::Unsupported,
    }
}

/// On-disk probe of `/user/app/<title_id>/app.pkg`.
enum PkgProbe {
    /// `app.pkg` is present at non-zero size — Sony's installer wrote the
    /// package content. Definitive "content landed".
    Present,
    /// The console answered, but the title's `app.pkg` is not there — either
    /// the directory is missing (ENOENT) or it exists without the package
    /// (a no-op tier's empty dir, or content still being copied).
    Absent,
    /// The directory couldn't be read for a reason other than "not found"
    /// (RPC/socket trouble, permission) — verdict deferred to supplements.
    Unreadable,
}

/// Probe whether Sony's installer actually wrote the package content for
/// `title_id` to disk. The discriminator is `/user/app/<id>/app.pkg` at
/// non-zero size — the real on-disk package the launcher boots (the
/// reference doc §12 checks this exact file). Crucially this is NOT
/// satisfied by a bare `/user/app/<id>/` directory, which a no-op install
/// tier can leave behind (hardware-proven: `sceAppInstUtilAppInstallPkg`
/// on FW 5.10 returns rc==0 but copies nothing, and an interrupted install
/// can leave an empty dir) — that case is exactly the false-positive the
/// older title-record scan produced.
fn probe_installed_pkg(addr: &str, title_id: &str) -> PkgProbe {
    // Internal storage first (the common case).
    let internal = probe_app_pkg_at(addr, &format!("/user/app/{title_id}"));
    if matches!(internal, PkgProbe::Present) {
        return PkgProbe::Present;
    }
    // Extended storage: when the console's install location is set to an
    // extended/M.2 drive, the title's `app.pkg` lands at
    // `<mount>/user/app/<id>/app.pkg`, NOT internal `/user/app`. An
    // internal-only probe then FALSE-NEGATIVES a perfectly good install — the
    // title appears, the game plays, but we report it never registered (and the
    // install tracker keeps the pkg / shows a failure). HW-confirmed on a PS5
    // Pro whose games install to `/mnt/ext1`. Scan every extended mount the
    // payload reports.
    // NOTE: the payload's FS_LIST_DIR sees a stale/namespaced view of extended
    // mounts and can MISS a freshly-installed ext title (HW-observed: a title
    // the spawned shell lists is ENOENT to FS_LIST_DIR). So a Present here is
    // trustworthy, but an Absent is NOT conclusive for ext storage — the engine
    // tracker's byte-accounting (free-space drop) is the authoritative fallback.
    if let Ok(vols) = crate::volumes::list_volumes(addr) {
        for v in &vols.volumes {
            if !is_extended_app_mount(&v.path) {
                continue;
            }
            if matches!(
                probe_app_pkg_at(addr, &format!("{}/user/app/{title_id}", v.path)),
                PkgProbe::Present
            ) {
                return PkgProbe::Present;
            }
        }
    }
    // Not found on any drive. Preserve "unreadable" if internal couldn't be read
    // (defers to the app.db supplement upstream) rather than a false Absent.
    internal
}

/// Whether `path` is an extended-storage mount PS5 installs apps to
/// (`/mnt/ext*` — the M.2 / extended SSD). Deliberately NOT `/mnt/usb*`
/// (exfat media drives — games don't install there) nor our own
/// `/mnt/ps5upload/*` image mounts.
fn is_extended_app_mount(path: &str) -> bool {
    path.starts_with("/mnt/ext")
}

/// The single-directory `app.pkg` discriminator: a non-zero `app.pkg` under
/// `app_dir` means Sony's installer wrote real content there. ENOENT ⇒ Absent
/// (definitively not here); any other read error ⇒ Unreadable (verdict
/// deferred). Shared by the internal + extended-storage probes.
fn probe_app_pkg_at(addr: &str, app_dir: &str) -> PkgProbe {
    match crate::fs_ops::list_dir(addr, app_dir, crate::fs_ops::ListDirOptions::default()) {
        Ok(listing) => {
            if listing
                .entries
                .iter()
                .any(|e| e.name == "app.pkg" && e.size > 0)
            {
                PkgProbe::Present
            } else {
                PkgProbe::Absent
            }
        }
        Err(e) => {
            // The payload reports a missing directory as
            // `fs_list_dir_opendir_errno_2` (ENOENT) — that's a definitive
            // "title not installed here", not an inability to check. Anchor with
            // `ends_with` so we don't also match errno 20/23/24/2xx (the payload
            // formats `..._errno_<n>`), which would mis-tag a real read failure
            // (ENOTDIR/EMFILE) as "absent".
            if e.to_string().ends_with("errno_2") {
                PkgProbe::Absent
            } else {
                PkgProbe::Unreadable
            }
        }
    }
}

/// Whether app.db lists `title_id`. `Some(true/false)` only when sqlite is
/// actually readable on this firmware (`err == None`); `None` otherwise (the
/// dlsym-only AppDbQuery returns `sqlite_unavailable` on FW 9.60 — see the
/// 9.60 note on [`verify_title_registered`]).
fn appdb_has_title(addr: &str, title_id: &str) -> Option<bool> {
    match crate::diagnostics::appdb_query(addr) {
        Ok(list) if list.err.is_none() => Some(list.apps.iter().any(|a| a.title_id == title_id)),
        _ => None,
    }
}

/// Check whether `title_id` (e.g. "PPSA01650") installed a launchable title
/// on the PS5.
///
/// The title_id-keyed core of [`verify_launchable`], split out because some
/// PKG formats (notably the `\x7FFIH` PS5-native fakepkg header) don't expose
/// a parseable content_id host-side — but their title_id is recoverable from
/// the filename. Callers with a title_id in hand can verify directly.
///
/// **Discriminator:** the on-disk package `/user/app/<id>/app.pkg`
/// ([`probe_installed_pkg`]) — the same file the reference doc §12 checks and
/// the file the PS5 launcher boots. This deliberately does NOT trust the bare
/// presence of a `/user/app/<id>/` directory: a no-op install tier can return
/// `rc==0` and leave (or not even create) an empty dir, which the older
/// title-record scan (`app_list_registered` / `list_registered_titles_json`)
/// reported as "registered" — a false success. Hardware on FW 5.10 proved the
/// no-op (`sceAppInstUtilAppInstallPkg`), and FW 9.60 proved sqlite app.db is
/// unreadable, so the filesystem `app.pkg` check is the only thing that works
/// on every firmware.
///
/// Supplements (only consulted when `app.pkg` isn't present): a homebrew
/// title we registered via nullfs has no `app.pkg` but a non-empty `src`
/// (its `mount.lnk`) and IS launchable; and app.db can confirm a title on
/// firmwares where sqlite is readable. Any unverifiable case returns
/// `Unsupported` so a check we can't perform never fails a real install.
pub fn verify_title_registered(addr: &str, title_id: &str) -> LaunchCheck {
    if !looks_like_title_id(title_id) || title_id.starts_with("FAKE") {
        return LaunchCheck::Unsupported;
    }

    // PRIMARY: did the package content actually land on disk?
    match probe_installed_pkg(addr, title_id) {
        PkgProbe::Present => return LaunchCheck::Registered,
        // Reachable, content not (yet) on disk — fall through to supplements,
        // then Absent. The engine's verification window distinguishes "still
        // promoting" from "never landed".
        PkgProbe::Absent => {}
        // Couldn't read the dir for a non-ENOENT reason — defer to app.db,
        // else stay optimistic (Unsupported) rather than fail a real install.
        PkgProbe::Unreadable => {
            return match appdb_has_title(addr, title_id) {
                Some(true) => LaunchCheck::Registered,
                Some(false) => LaunchCheck::Absent,
                None => LaunchCheck::Unsupported,
            };
        }
    }

    // SUPPLEMENT 1: a homebrew (nullfs-registered) title is launchable
    // without an app.pkg — it carries a non-empty `src` (its mount.lnk).
    // A bare-dir pkg install has `src == ""`, so this never re-introduces the
    // false positive.
    if let Ok(list) = crate::fs_ops::app_list_registered(addr) {
        if list
            .apps
            .iter()
            .any(|a| a.title_id == title_id && !a.src.is_empty())
        {
            return LaunchCheck::Registered;
        }
    }

    // SUPPLEMENT 2: app.db, where sqlite is readable (newer firmwares).
    match appdb_has_title(addr, title_id) {
        Some(true) => LaunchCheck::Registered,
        _ => LaunchCheck::Absent,
    }
}

pub fn err_code_message(code: u32) -> Option<&'static str> {
    match code {
        0x0000_0000 => None, // success — no message
        0x8099_0001 => Some("BGFT initialised already (benign)"),
        0x8099_0036 => Some("DRM mismatch — this PKG isn't valid for this console"),
        0x8099_0038 => Some("PKG entitlement check failed — wrong account / region"),
        0x8099_0039 => Some("PS5 reports not enough free space — if the console clearly has room, its storage is likely too fragmented; rebuild the database from Safe Mode, then retry"),
        0x8099_0085 => Some("Need defragmented free space — Settings → Storage → Free up space"),
        0x8099_0086 => Some("Leftover download in notifications — clear it from the PS5 first"),
        0x8099_0088 => Some("This title is already installed"),
        0x80A3_0026 => Some("PS5 reports not enough free space — if the console clearly has room, its storage is likely too fragmented; rebuild the database from Safe Mode, then retry"),
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
        // Hardware-observed on FW 9.60 when all 3 tiers fail, and on
        // FW 10.00+ when an outdated payload inits Sony's installer
        // under the wrong authid (issue #152: the install returns
        // 0x80B2116F and Sony's watchdog kills the helper ~5s later).
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
    fn title_id_parsed_from_real_content_ids() {
        assert_eq!(
            title_id_from_content_id("IV9999-CUSA12345_00-ZGAMEFOO00000000").as_deref(),
            Some("CUSA12345")
        );
        assert_eq!(
            title_id_from_content_id("EP4361-PPSA01234_00-REDEMPTION000002").as_deref(),
            Some("PPSA01234")
        );
        // Surrounding whitespace (BGFT-padded ids) is tolerated.
        assert_eq!(
            title_id_from_content_id("  UP0000-NPXS40047_00-LABEL  ").as_deref(),
            Some("NPXS40047")
        );
    }

    #[test]
    fn title_id_rejects_placeholders_and_garbage() {
        // FAKE placeholder — elf-arsenal treats this as "no real title".
        assert_eq!(title_id_from_content_id("IV9999-FAKE00000_00-X"), None);
        // No '-' separator.
        assert_eq!(title_id_from_content_id("CUSA12345"), None);
        // Wrong shape (too short / lowercase / non-digit tail).
        assert_eq!(title_id_from_content_id("IV9999-CUSA123_00-X"), None);
        assert_eq!(title_id_from_content_id("IV9999-cusa12345_00-X"), None);
        assert_eq!(title_id_from_content_id("IV9999-CUSA1234X_00-X"), None);
        // Empty.
        assert_eq!(title_id_from_content_id(""), None);
        assert_eq!(title_id_from_content_id("   "), None);
    }

    #[test]
    fn looks_like_title_id_shape() {
        assert!(looks_like_title_id("CUSA12345"));
        assert!(looks_like_title_id("PPSA00001"));
        assert!(!looks_like_title_id("CUSA1234")); // 8 chars
        assert!(!looks_like_title_id("CUSA123456")); // 10 chars
        assert!(!looks_like_title_id("CUS012345")); // only 3 letters
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
    fn may_not_launch_only_for_appinst_local() {
        // The unlaunchable last-resort path (sceAppInstUtilAppInstallPkg).
        assert!(install_may_not_launch("appinst-local"));
        // Every launchable path must NOT warn.
        assert!(!install_may_not_launch("appinst"));
        assert!(!install_may_not_launch("shellui-rpc"));
        assert!(!install_may_not_launch("intdebug"));
        assert!(!install_may_not_launch("regular"));
        assert!(!install_may_not_launch("tier0-worker"));
        assert!(!install_may_not_launch("none"));
        assert!(!install_may_not_launch(""));
    }

    #[test]
    fn preserve_staging_on_reject_only_for_patches() {
        // A patch (…DP) must KEEP its staged pkg on a register-reject so the
        // client's DPI-daemon fallback can install the same file. Both PS4 and
        // PS5 patch types end in "DP" (PS4DP / the PS5 patch category).
        assert!(preserve_staging_on_reject("PS4DP"));
        assert!(preserve_staging_on_reject("PS5DP"));
        // Base games and DLC auto-clean as before (idempotent retry, no
        // installer-queue pollution). A DLC ("…AC") carries its OWN content_id,
        // so a stale staged copy is exactly the residue the cleanup prevents.
        assert!(!preserve_staging_on_reject("PS4GD"));
        assert!(!preserve_staging_on_reject("PS5GD"));
        assert!(!preserve_staging_on_reject("PS4AC"));
        assert!(!preserve_staging_on_reject("PS5AC"));
        assert!(!preserve_staging_on_reject(""));
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
            method: None,
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
