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
): DpiUnavailableCopy {
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
