//! Did a patch install actually apply?
//!
//! Sony's installer has two paths for a `.pkg`. A fresh install goes through
//! `RequestInstall` and produces a real BGFT task with byte counts. An
//! overwrite — which is what a patch is — goes through
//! `AppPrepareOverwriteByPackage` + `DbgInstall`, and when the installer
//! cannot match the base game already on the console it returns
//! `0x00000000`, creates no task, copies nothing, and leaves the title
//! exactly as it was.
//!
//! The DPI daemon reports `rc == 0` as success, because that is what the API
//! returned. So a patch that did nothing at all was indistinguishable from one
//! that worked, and users were told their update installed when it had not.
//! That is the whole of the "base game installs but the update fails" reports.
//!
//! Confirmed on hardware (Phat, FW 5.10, internal storage), holding firmware
//! and storage constant and re-installing the base game from our own package:
//!
//!   pre-existing base (origin unknown) -> patch no-op, APP_VER stayed 01.00
//!                                         for over an hour
//!   base re-installed from our pkg     -> patch applied in 150 s, APP_VER
//!                                         01.00 -> 01.09, +8.69 GB
//!
//! CAREFUL with the conclusion. Several things changed at once in that
//! comparison — who installed the base, when, and possibly which build/dump it
//! was. A patch is built against a specific base, and nothing in either
//! package exposes a link (no base digest, no required-version field), so
//! "installed by another tool" is NOT established; "the console could not
//! match this update to that base" is.
//!
//! The check itself does not depend on the cause: for a package declaring a
//! higher app version than the title currently has, the installed version must
//! move. If it did not, the install failed however cheerful the return code.

/// What a post-install version comparison concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchVerdict {
    /// The installed version advanced to (at least) what the package declared.
    Applied,
    /// The installed version did not move. Sony accepted the package and did
    /// nothing with it — the silent-overwrite case.
    DidNotApply,
    /// The installed version ended up BELOW where it started.
    ///
    /// Read this one carefully before acting on it. Sony's overwrite removes
    /// the existing update before installing the replacement, so a re-apply
    /// legitimately reads as the base version *while it runs* — measured on
    /// FW 5.10 as 01.00 with `/user/patch/<TID>/` absent for more than a
    /// minute, before settling back to 01.09 on its own. A single sample
    /// showing a lower version therefore proves nothing.
    ///
    /// Only conclude this after waiting out the install (see the caller's
    /// deadline). Reporting it early fails a healthy install, which is worse
    /// than the silent no-op this module exists to catch.
    Regressed,
    /// Not enough information to judge: a version was unreadable, or the
    /// package does not claim to raise the version (a reinstall of the same
    /// build, or a non-versioned package kind). Never report a failure from a
    /// missing input — that is how a working install gets called broken.
    Inconclusive,
}

/// Parse a PS5 app version like `01.09` into a comparable integer.
/// Returns `None` for anything that is not `NN.NN`, which is the signal to
/// stay inconclusive rather than guess.
pub fn parse_app_ver(v: &str) -> Option<u32> {
    let t = v.trim();
    let (major, minor) = t.split_once('.')?;
    if major.is_empty() || minor.is_empty() {
        return None;
    }
    if !major.chars().all(|c| c.is_ascii_digit()) || !minor.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let major: u32 = major.parse().ok()?;
    let minor: u32 = minor.parse().ok()?;
    Some(major * 100 + minor)
}

/// Compare the version before the install, the version after, and the version
/// the package declared.
///
/// `before` is `None` when the title was not installed beforehand — that is a
/// fresh install, not a patch, and is not this check's business.
pub fn verify_patch_applied(
    before: Option<&str>,
    after: Option<&str>,
    package: Option<&str>,
) -> PatchVerdict {
    let (Some(before), Some(after), Some(package)) = (before, after, package) else {
        return PatchVerdict::Inconclusive;
    };
    let (Some(b), Some(a), Some(p)) = (
        parse_app_ver(before),
        parse_app_ver(after),
        parse_app_ver(package),
    ) else {
        return PatchVerdict::Inconclusive;
    };
    // Checked FIRST, and independently of what the package claimed: going
    // backwards is a loss however the request was framed. This case was
    // originally classed inconclusive-because-`p <= b`, which reported the
    // destructive re-apply above as a success.
    if a < b {
        return PatchVerdict::Regressed;
    }
    // The package does not claim to raise the version, so "unchanged" is the
    // correct outcome and proves nothing either way.
    if p <= b {
        return PatchVerdict::Inconclusive;
    }
    if a >= p {
        return PatchVerdict::Applied;
    }
    if a > b {
        // Moved, but not as far as the package declared. Something happened;
        // do not call it a silent no-op.
        return PatchVerdict::Inconclusive;
    }
    PatchVerdict::DidNotApply
}

/// The message shown when a patch was accepted but changed nothing. It has to
/// carry the workaround, because the workaround is not guessable: the base
/// game has to be re-installed through ps5upload before the patch will take.
/// Shown when, after waiting out the install, the game is on a lower version
/// than it started on.
pub const REGRESSED_HINT: &str = "This update left the game on an older version than it had before. The base game is intact. Apply the update again to bring the game back up to date — if it keeps ending lower, re-install the base game through ps5upload (choose Override) and then apply the update.";

pub const DID_NOT_APPLY_HINT: &str = "The PS5 accepted this update and then did nothing with it — the game is still on its previous version. This means the console could not match the update to the base game you have installed. Re-install the base game through ps5upload from the base package that goes with this update (choose Override), then apply the update again.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ps5_versions_and_rejects_junk() {
        assert_eq!(parse_app_ver("01.00"), Some(100));
        assert_eq!(parse_app_ver("01.09"), Some(109));
        assert_eq!(parse_app_ver("10.50"), Some(1050));
        assert_eq!(parse_app_ver(" 01.09 "), Some(109));
        for bad in ["", "1", "01.", ".09", "a.bc", "01.0x", "01-09"] {
            assert_eq!(parse_app_ver(bad), None, "should reject {bad:?}");
        }
    }

    #[test]
    fn the_hardware_failure_is_detected() {
        // Phat, FW 5.10: base installed elsewhere, patch declared 01.09,
        // APP_VER sat at 01.00 for over an hour.
        assert_eq!(
            verify_patch_applied(Some("01.00"), Some("01.00"), Some("01.09")),
            PatchVerdict::DidNotApply
        );
    }

    #[test]
    fn the_hardware_success_is_accepted() {
        // Same console after re-installing the base through ps5upload.
        assert_eq!(
            verify_patch_applied(Some("01.00"), Some("01.09"), Some("01.09")),
            PatchVerdict::Applied
        );
    }

    #[test]
    fn missing_inputs_never_produce_a_failure() {
        // A working install must never be reported as broken because a version
        // could not be read.
        assert_eq!(
            verify_patch_applied(None, Some("01.09"), Some("01.09")),
            PatchVerdict::Inconclusive
        );
        assert_eq!(
            verify_patch_applied(Some("01.00"), None, Some("01.09")),
            PatchVerdict::Inconclusive
        );
        assert_eq!(
            verify_patch_applied(Some("01.00"), Some("01.00"), None),
            PatchVerdict::Inconclusive
        );
        assert_eq!(
            verify_patch_applied(Some("??"), Some("01.00"), Some("01.09")),
            PatchVerdict::Inconclusive
        );
    }

    #[test]
    fn the_hardware_regression_is_caught() {
        // A title that ends BELOW where it started, once the install has been
        // waited out, is a loss and must not be reported as success — the
        // pre-existing `p <= b` rule classed exactly this as inconclusive.
        // NOTE: mid-install this same shape appears transiently on a healthy
        // re-apply, which is why the caller only asks at its deadline.
        assert_eq!(
            verify_patch_applied(Some("01.09"), Some("01.00"), Some("01.09")),
            PatchVerdict::Regressed
        );
    }

    #[test]
    fn a_regression_beats_every_other_rule() {
        // Whatever the package claims, backwards is backwards.
        for pkg in ["01.00", "01.09", "02.00"] {
            assert_eq!(
                verify_patch_applied(Some("01.09"), Some("01.00"), Some(pkg)),
                PatchVerdict::Regressed,
                "package {pkg}"
            );
        }
    }

    #[test]
    fn a_same_version_reinstall_is_not_a_failure() {
        // Re-applying the build already installed legitimately changes nothing.
        assert_eq!(
            verify_patch_applied(Some("01.09"), Some("01.09"), Some("01.09")),
            PatchVerdict::Inconclusive
        );
        // A package older than what is installed, with nothing changing, is
        // still not a failure — nothing was lost.
        assert_eq!(
            verify_patch_applied(Some("01.09"), Some("01.09"), Some("01.00")),
            PatchVerdict::Inconclusive
        );
    }

    #[test]
    fn a_partial_move_is_not_called_a_no_op() {
        assert_eq!(
            verify_patch_applied(Some("01.00"), Some("01.05"), Some("01.09")),
            PatchVerdict::Inconclusive
        );
    }

    #[test]
    fn a_version_beyond_the_package_still_counts_as_applied() {
        assert_eq!(
            verify_patch_applied(Some("01.00"), Some("01.10"), Some("01.09")),
            PatchVerdict::Applied
        );
    }
}
