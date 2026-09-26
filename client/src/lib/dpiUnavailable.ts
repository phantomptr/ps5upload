/**
 * Which "the update installer never started" message to show.
 *
 * ps5upload applies an update through Sony's safe installer. When the
 * in-process call is rejected, the only remaining route is the standalone DPI
 * daemon on :9040 — and getting that daemon running means handing its ELF to
 * the console's payload loader on :9021.
 *
 * Three unrelated things can stop that, and 5.17.6 showed one message for all
 * of them: "use the released engine build — a source build without the PS5
 * payload SDK has no installer image to send." A user on 5.17.6 running the
 * released Docker image was sent chasing their engine build when their bundle
 * showed the real cause plainly:
 *
 *     DPI ensure result: ok=false listening=false sent=false
 *     error="send dpi.elf: connect 10.x.x.x:9021: Connection refused"
 *
 * Their console's loader had stopped answering — the engine had the image and
 * had already tried to send it. Nothing in the message pointed there.
 *
 * The engine and the desktop shell now both return a machine-readable
 * `reason`; this maps it to copy. Kept pure and separate from the store so the
 * mapping is unit-testable without a console.
 */

/** Failure causes reported by `dpi_ensure`. Mirrors the Rust constants in
 *  `ps5upload_core::payload_lifecycle`. */
export type DpiEnsureReason =
  /** This build carries no DPI daemon image to send. */
  | "no_image"
  /** Nothing accepted a connection on the loader port (:9021). */
  | "loader_unreachable"
  /** The loader accepted the connection but the transfer failed. */
  | "loader_send_failed"
  /** The image was delivered but :9040 never came up. */
  | "no_bringup";

/** An i18n key plus the English text it falls back to. */
export interface DpiUnavailableCopy {
  key: string;
  text: string;
}

/** Every one of these failures happens AFTER the bytes are committed, and a
 *  non-confirmed install always keeps its staged pkg. Saying so stops people
 *  re-sending a package that is already sitting on the console — the same
 *  waste the queue used to inflict on them automatically. */
const ALREADY_STAGED =
  "The update is already on your PS5, so nothing needs uploading again — retry the install from the Packages list.";

const SELF_INSTALL_ROUTE =
  "You can also apply the update from the PS5 itself: Settings → System → Debug Settings → Game → Package Installer.";

/** The loader process exists but the port is dead.
 *
 *  Four bug reports on 2026-09-09 had `elfldr.elf` in the process list while
 *  :9021 refused every connection. Telling that user to "re-run the loader"
 *  reads as plainly wrong when they can see it running, and they concluded the
 *  app could not help them. Naming the contradiction is the whole value here. */
export const PKG_PATCH_LOADER_NOT_LISTENING_HINT =
  "This update couldn’t be applied because your PS5’s payload loader is running but is not accepting connections on port 9021, so the installer it needs was never delivered and the console never saw the update. Your base game is untouched. Load it again from your jailbreak page — a loader process that has already served a payload does not always keep listening. " +
  ALREADY_STAGED +
  " " +
  SELF_INSTALL_ROUTE;

/** The loader on the PS5 didn't take the installer image. Nothing about the
 *  engine or the package is wrong, so the advice is entirely console-side. */
export const PKG_PATCH_LOADER_UNREACHABLE_HINT =
  "This update couldn’t be applied because ps5upload couldn’t reach your PS5’s payload loader on port 9021, so the installer it needs was never delivered and the console never saw the update. Your base game is untouched. Re-run the loader on your PS5 — the same jailbreak page or homebrew loader you use to send ps5upload — and try the update again. " +
  ALREADY_STAGED +
  " " +
  SELF_INSTALL_ROUTE;

/** This build has no daemon image at all — the only cause that is genuinely
 *  about how the engine was built. */
export const PKG_PATCH_DAEMON_UNAVAILABLE_HINT =
  "This update couldn’t be applied because this build has no PS5 update installer to send, so the console never saw the update. Your base game is untouched. Use the released engine build (or the ps5upload-engine Docker image) — a source build without the PS5 payload SDK has no installer image. " +
  ALREADY_STAGED +
  " " +
  SELF_INSTALL_ROUTE;

/** The image was delivered and the daemon still never answered. */
export const PKG_PATCH_DAEMON_NO_BRINGUP_HINT =
  "This update couldn’t be applied because the PS5’s update installer never started — nothing answered on port 9040 after it was sent — so the console never saw the update. Your base game is untouched. Restart the PS5, re-run your loader, and try again. " +
  ALREADY_STAGED +
  " " +
  SELF_INSTALL_ROUTE;

/**
 * Pick the guidance for a `dpi_ensure` failure.
 *
 * An unknown or missing reason falls back to the no-bringup copy: it is the
 * only one of the three whose advice ("restart and try again") is safe when
 * we don't actually know what happened. Blaming the engine build or the
 * console's loader on a guess is how the original bug read.
 */
export function dpiUnavailableCopy(
  reason: DpiEnsureReason | string | null | undefined,
  /** True when a loader process is visible on the console. Changes the advice
   *  from "re-run it" to "it is running but not listening", which is what the
   *  2026-09-09 reports actually showed. */
  loaderProcessRunning = false,
): DpiUnavailableCopy {
  if (
    loaderProcessRunning &&
    (reason === "loader_unreachable" || reason === "loader_send_failed")
  ) {
    return {
      key: "pkg.patch_loader_not_listening",
      text: PKG_PATCH_LOADER_NOT_LISTENING_HINT,
    };
  }
  switch (reason) {
    case "no_image":
      return {
        key: "pkg.patch_daemon_unavailable",
        text: PKG_PATCH_DAEMON_UNAVAILABLE_HINT,
      };
    case "loader_unreachable":
    case "loader_send_failed":
      return {
        key: "pkg.patch_loader_unreachable",
        text: PKG_PATCH_LOADER_UNREACHABLE_HINT,
      };
    default:
      return {
        key: "pkg.patch_daemon_no_bringup",
        text: PKG_PATCH_DAEMON_NO_BRINGUP_HINT,
      };
  }
}

/** The whole story of a failed patch install, in one message.
 *
 *  Two things must survive into it, and the DP branch used to drop the first:
 *
 *    * WHY the console rejected the install (e.g. 0x80B2116F, whose remedy is
 *      the PS5's own Package Installer). The non-DP branch always reported it;
 *      the patch branch replaced it with the daemon's transport error, so the
 *      code that names the real problem never reached the user OR the next bug
 *      report.
 *    * why the fallback could not be delivered either, which is a different
 *      problem with a different fix.
 *
 *  Both matter: on the 2026-09-09 reports the first was Sony declining the
 *  firmware/package combination and the second was a dead :9021, and knowing
 *  only the second sent the user looking in the wrong place. */
export function patchInstallFailure(input: {
  /** The primary rejection, as already formatted for display. */
  mainErr?: string | null;
  reason: DpiEnsureReason | string | null | undefined;
  dpiErr?: string | null;
  loaderProcessRunning?: boolean;
  /** Injected so this stays pure and testable; defaults to the English text. */
  translate?: (key: string, fallback: string) => string;
}): string {
  const copy = dpiUnavailableCopy(input.reason, input.loaderProcessRunning ?? false);
  const hint = input.translate ? input.translate(copy.key, copy.text) : copy.text;
  const parts: string[] = [];
  if (input.mainErr) {
    parts.push(`The PS5 rejected the install (${input.mainErr}).`);
  }
  parts.push(hint);
  // The raw daemon error stays appended: it is what makes the next bug report
  // diagnosable in one read.
  if (input.dpiErr) parts.push(`(${input.dpiErr})`);
  return parts.join(" ");
}
