//! Host-side install helpers: title-id derivation, launch/registration
//! checks, and the patch preflight.
//!
//! The PKG_INSTALL frame RPC that used to live here (and the payload's
//! in-process install cascade it drove) is retired — every install goes
//! through the PS5Upload installer daemon via the engine's unified
//! `POST /api/pkg/install`. `InstallPhase`/`PkgInstallStatus` remain as the
//! type a persisted pkg-host session carries.

use serde::{Deserialize, Serialize};

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
    /// Sony's installer left the placeholder title id (`FAKE…`) in the
    /// content_id instead of rewriting it with the real one.
    ///
    /// This is a KNOWN-BAD outcome, not an unverifiable one, and the
    /// distinction matters: the install "succeeds" and produces a tile, but
    /// that tile launches Sony's CloudClientApp rather than the user's
    /// eboot. Reporting it as unverifiable (and therefore fine) hands the
    /// user a broken tile with a green tick — the same "claimed success for
    /// something that did not work" failure this codebase just spent a day
    /// removing from the launch path. elf-arsenal fails the install here for
    /// exactly this reason (src/homebrew.c, install_pkg_thread).
    PlaceholderTitleId,
}

/// Check whether `content_id`'s title is registered (and therefore
/// launchable) on the PS5.
///
/// elf-arsenal verifies installs by polling app.db (`tbl_contentinfo`). We
/// reproduce the *semantics* — "did the title row materialize?" — but the
/// **primary** source is the filesystem enumeration (`app_list_registered`
/// → `/user/app/<title_id>/` scan), NOT sqlite. The `/user/app/` scan works
/// on every firmware (it's what the codebase already uses for the Library
/// "installed" filter) and is the on-disk equivalent of Sony's app.db row —
/// "any title Sony's XMB knows about has a /user/app/<id>/ directory"
/// (register.h).
///
/// app.db (via `AppDbQuery`) is consulted as a **supplement** when the
/// filesystem scan doesn't (yet) show the title — it can surface a title
/// that's registered in the DB a beat before the `/user/app` enumeration
/// reflects it. That query is now answered by a SQLite the payload links
/// itself, so it works on every firmware rather than none; it stays the
/// supplement rather than the primary because a database the shell holds
/// open can still refuse to open, and the filesystem never does.
///
/// Any unverifiable case (no title_id, both sources unreadable) returns
/// `Unsupported`, so a check we can't perform never fails a real install.
pub fn verify_launchable(addr: &str, content_id: &str) -> LaunchCheck {
    // Checked BEFORE the generic derivation, because title_id_from_content_id
    // folds "FAKE placeholder" into the same None as "malformed", and those
    // two deserve opposite verdicts: one is a known-broken install, the other
    // is simply not checkable.
    if is_placeholder_content_id(content_id) {
        return LaunchCheck::PlaceholderTitleId;
    }
    match title_id_from_content_id(content_id) {
        Some(title_id) => verify_title_registered(addr, &title_id),
        None => LaunchCheck::Unsupported,
    }
}

/// Pre-flight for a patch / add-on install: is the base game already on the
/// console?
///
/// A patch (`gp`) or add-on (`ac`) shares the base game's content_id and can
/// only install onto an already-present base — otherwise Sony's installer
/// fails late with `APP_NOT_FOUND` (0x80A30004), which the DPI async poll now
/// surfaces but only after the attempt. Checking first turns the most common
/// patch failure into a clear "install the base game first" up front.
///
/// Returns `Some(message)` to warn/block, or `None` to proceed. A base game
/// (`gd`) never depends on anything, and an unverifiable console never blocks
/// — pre-flight only speaks when it is *sure* the base is missing, matching
/// SSPI's "reject a patch built against a different base" without ever
/// blocking on uncertainty.
pub fn preflight_patch_install(addr: &str, content_id: &str, category: &str) -> Option<String> {
    let cat = category.trim().to_lowercase();
    // Only patches and add-ons depend on a base being present.
    let kind = match cat.as_str() {
        "gp" => "update/patch",
        "ac" => "add-on (DLC)",
        _ => return None, // gd (base) or unknown — nothing to pre-check
    };
    match verify_launchable(addr, content_id) {
        // Definitely not installed → the one case worth stopping for. Name
        // the base title when we can derive it, so the message points at the
        // exact game to install first rather than at "the base game".
        LaunchCheck::Absent => {
            let base = title_id_from_content_id(content_id).unwrap_or_else(|| "that game".into());
            Some(format!(
                "This is an {kind} for {base}, but that game isn't installed on this PS5. \
                 Install the base game first, then install this {kind}."
            ))
        }
        // Registered → good. Unsupported / PlaceholderTitleId → cannot be
        // sure, so never block: the install (and the DPI poll) still runs.
        _ => None,
    }
}

/// True when the content_id still carries Sony's in-flight `FAKE…`
/// placeholder title id.
///
/// Sony writes this while an unsigned pkg is being installed and rewrites it
/// with the real title id on success. If it is still there when we look, the
/// rewrite never happened and the resulting tile is broken.
pub fn is_placeholder_content_id(content_id: &str) -> bool {
    let cid = content_id.trim();
    match cid.split_once('-') {
        Some((_, after)) => after
            .split('_')
            .next()
            .is_some_and(|t| t.starts_with("FAKE")),
        None => false,
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

/// Whether app.db lists `title_id`. `Some(true/false)` only when the query
/// actually returned a row set (`err == None`); `None` when the database
/// could not be read at all, so an unreadable database never counts as
/// "the title is absent".
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_only_gates_patches_and_addons() {
        // A base game (gd) or an unknown/missing category never triggers a
        // base check — so these return None without touching the network,
        // regardless of the (here unreachable) address.
        let addr = "203.0.113.1:9114"; // TEST-NET-3, never connects
        assert_eq!(
            preflight_patch_install(addr, "EP4361-PPSA01234_00-REDEMPTION000002", "gd"),
            None
        );
        assert_eq!(
            preflight_patch_install(addr, "EP4361-PPSA01234_00-REDEMPTION000002", ""),
            None
        );
        assert_eq!(
            preflight_patch_install(addr, "EP4361-PPSA01234_00-REDEMPTION000002", "GD"),
            None
        );
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
    }

    /// The placeholder must be distinguishable from "unparseable", because
    /// the two get opposite verdicts: a broken install vs. one we simply
    /// can't check. Folding them together is what let a broken tile pass.
    #[test]
    fn placeholder_content_id_is_detected() {
        assert!(is_placeholder_content_id("IV9999-FAKE00000_00-X"));
        assert!(is_placeholder_content_id("  UP0000-FAKE12345_00-LABEL  "));
    }

    #[test]
    fn real_content_ids_are_not_placeholders() {
        assert!(!is_placeholder_content_id(
            "IV9999-CUSA12345_00-ZGAMEFOO00000000"
        ));
        assert!(!is_placeholder_content_id(
            "EP4361-PPSA01234_00-REDEMPTION000002"
        ));
        // Malformed input is "can't tell", never a placeholder claim.
        assert!(!is_placeholder_content_id("CUSA12345"));
        assert!(!is_placeholder_content_id(""));
    }

    #[test]
    fn placeholder_is_not_confused_with_a_title_starting_similarly() {
        // Only the title-id token is inspected; a label containing FAKE
        // elsewhere must not trip the check.
        assert!(!is_placeholder_content_id("IV9999-CUSA12345_00-FAKELABEL"));
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
    fn phase_serializes_snake_case() {
        let s = serde_json::to_string(&InstallPhase::Download).unwrap();
        assert_eq!(s, "\"download\"");
        let p: InstallPhase = serde_json::from_str("\"done\"").unwrap();
        assert_eq!(p, InstallPhase::Done);
    }
}
