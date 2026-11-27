// Restore to home screen (#961): the words and decisions around the helper's answers. Pure
// functions (no React), so they are tested on their own.
//
// After Settings › Rebuild Database, games installed from fake packages are gone from the PS5's
// home screen and from Settings › Storage, but their files stay on the drive. Pressing Play on one
// cannot work (nothing on the console knows the title), so the game's page offers Restore instead:
// the package already on the console is installed again by the PS5's own installer.

import type { RestoreReply, RestoreRun } from "../api/titleRestore";
import { formatBytes } from "./format";
import { trStatic } from "./trStatic";

/** Check states that mean "installed on a drive, but not on the home screen": Play is replaced by
 *  Restore (or by why it can't be restored). */
const MISSING_STATES = new Set([
  "restorable",
  "pending",
  "no_room",
  "retail",
  "aside_cross_device",
  "ambiguous",
]);

/** Whether the check says this game is on a drive but not on the home screen. */
export function restoreNeeded(reply: RestoreReply | null | undefined): boolean {
  return !!reply && reply.ok && MISSING_STATES.has(reply.state);
}

/** Whether a restore phase has ended (nothing more will change without the user). */
export function restoreEnded(phase: string | undefined): boolean {
  return phase === "restored" || phase === "rolled_back" || phase === "pending" || phase === "failed";
}

export function restoreWhereLabel(where: string | undefined): string {
  switch (where) {
    case "internal":
      return trStatic("restore_where_internal", "the console's storage");
    case "m2":
      return trStatic("restore_where_m2", "the M.2 drive");
    case "usb":
      return trStatic("restore_where_usb", "USB extended storage");
    default:
      return trStatic("restore_where_extended", "extended storage");
  }
}

function size(n: number | undefined): string {
  return typeof n === "number" && n > 0 ? formatBytes(n) : "?";
}

/** One sentence for the game page: what the check found, or null when there is nothing to say
 *  (the game is on the home screen, not installed, or the console could not tell). */
export function restoreStateText(reply: RestoreReply | null | undefined, name: string): string | null {
  if (!reply || !reply.ok) return null;
  const where = restoreWhereLabel(reply.where);
  switch (reply.state) {
    case "restorable":
      return trStatic(
        "restore_state_restorable",
        "{name} is installed on {where} but is not on the PS5's home screen, so it can't be started. This happens after Rebuild Database, which removes games installed from fake packages but leaves their files. Restore installs it again from the package already on the PS5: nothing is downloaded.",
        { name, where },
      );
    case "pending":
      return trStatic(
        "restore_state_pending",
        "A restore of {name} did not finish. Its package is kept in {aside}. If the game is back on the PS5's home screen, choose Finish; if not, Put back returns the package to where it was.",
        { name, aside: reply.aside ?? "" },
      );
    case "no_room":
      return trStatic(
        "restore_state_no_room",
        "{name} is not on the PS5's home screen. Restoring it needs {size} free while the PS5 writes a new copy, on {where} or on the console's storage. Free up space, then open this page again.",
        { name, where, size: size(reply.size) },
      );
    case "retail":
      return trStatic(
        "restore_state_retail",
        "{name} is not on the PS5's home screen, and its package is not a fake package, so PS5Upload leaves it alone. Delete it in Settings › Storage and download it again from the Game Library.",
        { name },
      );
    case "aside_cross_device":
      return trStatic(
        "restore_state_cross_device",
        "{name} is not on the PS5's home screen. PS5Upload could not move its package aside on its own drive, so it can't restore it. Nothing was changed.",
        { name },
      );
    case "ambiguous":
      return trStatic(
        "restore_state_ambiguous",
        "{name} has an installed package on two drives, so PS5Upload can't tell which one to use. Nothing was changed.",
        { name },
      );
    default:
      return null;
  }
}

/** The confirmation before a restore: what will happen, in order. */
export function restoreConfirmText(reply: RestoreReply, name: string): string {
  const where = restoreWhereLabel(reply.where);
  const parts = [
    trStatic(
      "restore_confirm_body",
      "The PS5 installs {name} again from the package already on {where} ({size}), the same way as the first install, so it comes back on the home screen and in Settings › Storage. Nothing is downloaded.",
      { name, where, size: size(reply.size) },
    ),
    trStatic(
      "restore_confirm_steps",
      "First a copy of the PS5's game database is saved. Then the package is moved into a PS5Upload folder on the same drive, and the PS5 writes a new copy where Settings › Storage › Installation Location says, so that drive needs {size} free. When the game is back, the moved copy is deleted. If the install fails, the package is put back where it was.",
      { size: size(reply.size) },
    ),
    trStatic(
      "restore_confirm_kstuff",
      "Fake-package support (kstuff) must be loaded, as for any fake-package install. Don't start the game or turn the PS5 off until it is done.",
    ),
  ];
  if (reply.patch) {
    parts.push(
      trStatic(
        "restore_confirm_patch",
        "Its update ({patch_size}) is kept aside during the install and put back afterwards as a file; to update the game again, install the update package.",
        { patch_size: size(reply.patch_size) },
      ),
    );
  }
  return parts.join("\n\n");
}

/** What a finished restore says: the notification's level and words. */
export function restoreOutcome(
  run: Pick<RestoreRun, "phase" | "result" | "error" | "install_hint">,
  name: string,
): { level: "success" | "info" | "error"; body: string } {
  switch (run.phase) {
    case "restored":
      return {
        level: "success",
        body: run.result?.patch_kept
          ? trStatic(
              "restore_done_patch_kept",
              "{name} is back on the home screen. Its update file could not go back to its place and is kept in {path}.",
              { name, path: run.result.patch_kept },
            )
          : trStatic("restore_done", "{name} is back on the home screen.", { name }),
      };
    case "rolled_back":
      return {
        level: "error",
        body: trStatic(
          "restore_rolled_back",
          "{name} could not be restored: {why} Its package is back where it was; nothing else changed.",
          { name, why: run.install_hint?.trim() || trStatic("restore_install_failed", "the PS5 did not install it.") },
        ),
      };
    case "pending":
      return {
        level: "info",
        body: trStatic(
          "restore_pending",
          "{name}: the restore has not finished. The package is kept on the PS5. Check its home screen, then open the game's page to finish.",
          { name },
        ),
      };
    default:
      return {
        level: "error",
        body: restoreErrorText(run.error ?? run.result?.err ?? "", name),
      };
  }
}

/** A failed start or finish in words. Unknown text passes through. */
export function restoreErrorText(raw: string, name: string): string {
  if (/restore_unsupported/i.test(raw)) {
    return trStatic(
      "restore_unsupported",
      "The PS5's helper is an older version that can't restore games. Send the helper again, then try once more.",
    );
  }
  if (/\bbusy\b/i.test(raw)) {
    return trStatic(
      "restore_busy",
      "The PS5 is installing something else. Try again when that install has finished.",
    );
  }
  if (/tailnet_home_only/i.test(raw)) {
    return trStatic(
      "restore_home_only",
      "Restoring a game needs this device on the same network as the PS5.",
    );
  }
  if (/restore_backup_failed/i.test(raw)) {
    return trStatic(
      "restore_backup_failed",
      "The PS5's game database could not be copied first, so nothing was changed.",
    );
  }
  if (/restore_move_failed|restore_aside_busy/i.test(raw)) {
    return trStatic(
      "restore_move_failed",
      "The package could not be moved aside on its drive, so nothing was changed.",
    );
  }
  if (/restore_registered/i.test(raw)) {
    return trStatic("restore_already", "{name} is on the home screen already.", { name });
  }
  if (/restore_(record_elsewhere|db_unreadable|not_installed|folder_registration)/i.test(raw)) {
    return trStatic(
      "restore_not_applicable",
      "{name} can't be restored this way: the PS5 still knows it, or it was not installed from a package. Nothing was changed.",
      { name },
    );
  }
  return raw;
}
