// Restore to home screen (#961): the words and decisions around the helper's answers. Pure
// functions (no React), so they are tested on their own.
//
// After Settings › Rebuild Database, games installed from fake packages are gone from the PS5's
// home screen and from Settings › Storage, but their files stay on the drive. Pressing Play on one
// cannot work (nothing on the console knows the title), so the game's page offers Restore instead:
// the package already on the console is installed again by the PS5's own installer.

import type { RestorePhase, RestoreReply, RestoreRun } from "../api/titleRestore";
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

const ACTIVE_PHASES = new Set(["queued", "installing", "finishing"]);
const PHASES = new Set([...ACTIVE_PHASES, "restored", "rolled_back", "pending", "failed"]);

/** Whether a restore phase has ended (nothing more will change without the user). Only the
 *  phases known to be running are running: a phase this app does not know (a newer engine, an odd
 *  answer) must not leave the page, the Games banner and the poll waiting forever. */
export function restoreEnded(phase: string | undefined): boolean {
  return !phase || !ACTIVE_PHASES.has(phase);
}

const str = (v: unknown): string | undefined => (typeof v === "string" ? v : undefined);
const num = (v: unknown): number | undefined =>
  typeof v === "number" && Number.isFinite(v) && v >= 0 ? v : undefined;
const obj = (v: unknown): Record<string, unknown> | undefined =>
  v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : undefined;

/** A helper answer as the page can render it: strings are strings, numbers are numbers, anything
 *  else is dropped. Null when it is not an object at all. */
export function normalizeRestoreReply(raw: unknown): RestoreReply | null {
  const r = obj(raw);
  if (!r) return null;
  const out: RestoreReply = { ok: r.ok === true, title_id: str(r.title_id) ?? "", state: str(r.state) ?? "" };
  for (const k of [
    "err", "reason", "where", "volume", "aside", "pkg", "kind", "patch", "install_path", "patch_aside",
    "backup", "installed", "installed_where", "patch_kept", "kept", "update", "addons",
  ] as const) {
    const v = str(r[k]);
    if (v !== undefined) out[k] = v;
  }
  for (const k of ["size", "patch_size", "free_volume", "free_internal", "need_volume", "need_internal", "sony_rc"] as const) {
    const v = num(r[k]);
    if (v !== undefined) out[k] = v;
  }
  if (r.cleared === true) out.cleared = true;
  return out;
}

/** The engine's restore status as the page can render it, whatever came back (#961: the desktop
 *  app went white during a long Restore all). An unknown phase is kept as `failed` with the raw
 *  word, so it ends and says what it was. */
export function normalizeRestoreRun(raw: unknown, titleId: string): RestoreRun {
  const r = obj(raw);
  if (!r) return { title_id: titleId, phase: "failed", error: "the engine sent an answer this app can't read" };
  const rawPhase = str(r.phase);
  const phase = (rawPhase && PHASES.has(rawPhase) ? rawPhase : "failed") as RestorePhase;
  const run: RestoreRun = { title_id: str(r.title_id) ?? titleId, phase };
  if (rawPhase !== phase) run.error = `unknown restore phase ${JSON.stringify(rawPhase ?? null)}`;
  for (const k of ["job", "install_verdict", "install_hint", "error"] as const) {
    const v = str(r[k]);
    if (v !== undefined) run[k] = v;
  }
  const prepared = normalizeRestoreReply(r.prepared);
  if (prepared) run.prepared = prepared;
  const result = normalizeRestoreReply(r.result);
  if (result) run.result = result;
  if (r.cleared === true) run.cleared = true;
  const install = obj(r.install);
  if (install) {
    const m = obj(install.metrics);
    run.install = {
      phase: str(install.phase),
      metrics: m ? { total_bytes: num(m.total_bytes), served_bytes: num(m.served_bytes) } : undefined,
    };
  }
  return run;
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
        { name, where, size: size(reply.need_volume ?? reply.size) },
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

/** A put-back the failed install's leftovers stand in the way of: the PS5 has a record of the
 *  title or a package in its place, written by the restore's own install. The engine's `clear`
 *  removes them (Sony's uninstall) first; same rule as the engine's `rollback_needs_clear`. */
export function restoreRollbackNeedsClear(r: RestoreReply | null | undefined): boolean {
  return (
    !!r &&
    r.state === "unsettled" &&
    (r.reason === "recorded_without_package" || r.reason === "on_home_screen" || r.reason === "place_taken")
  );
}

/** Why a pending restore is still pending, in words, when the helper said why; null otherwise. */
export function restorePendingWhy(result: RestoreReply | undefined, name: string): string | null {
  switch (result?.reason) {
    case "clearing":
      return trStatic(
        "restore_pending_clearing",
        "{name} could not be restored, and the PS5 is still removing its unfinished copy. Open the game's page in a minute and choose Put back.",
        { name },
      );
    case "update_in_place":
      return trStatic(
        "restore_pending_update_in_place",
        "{name} could not be restored, and the PS5 may still list the failed install. PS5Upload did not remove it, because the game's update is in its place and removing the install could take the update too. Delete the failed download on the PS5, then choose Put back on the game's page.",
        { name },
      );
    case "addons_present":
      return trStatic(
        "restore_pending_addons",
        "{name} could not be restored, and the PS5 may still list the failed install. PS5Upload did not remove it, because the game has add-ons on the PS5 and removing the install could take them too. Delete the failed download on the PS5, then choose Put back on the game's page.",
        { name },
      );
    case "uninstall_refused":
    case "uninstall_unavailable":
      return trStatic(
        "restore_pending_uninstall_refused",
        "{name} could not be restored, and the PS5 would not remove its failed install. Delete the failed download on the PS5, then choose Put back on the game's page.",
        { name },
      );
    default:
      return null;
  }
}

/** What a finished restore says: the notification's level and words. */
export function restoreOutcome(
  run: Pick<RestoreRun, "phase" | "result" | "error" | "install_hint" | "cleared">,
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
    case "rolled_back": {
      const why = run.install_hint?.trim() || trStatic("restore_install_failed", "the PS5 did not install it.");
      return {
        level: "error",
        body:
          run.cleared || run.result?.cleared
            ? trStatic(
                "restore_rolled_back_cleared",
                "{name} could not be restored: {why} The PS5's unfinished copy was removed, so it no longer holds up other installs, and the package is back where it was.",
                { name, why },
              )
            : trStatic(
                "restore_rolled_back",
                "{name} could not be restored: {why} Its package is back where it was; nothing else changed.",
                { name, why },
              ),
      };
    }
    case "pending":
      if (/installer stopped answering/i.test(`${run.error ?? ""} ${run.install_hint ?? ""}`)) {
        return { level: "error", body: restoreInstallerStuckText(name) };
      }
      return {
        level: "info",
        body:
          restorePendingWhy(run.result, name) ??
          trStatic(
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

function restoreInstallerStuckText(name: string): string {
  return trStatic(
    "restore_installer_stuck",
    "{name}: the PS5's installer stopped answering, so nothing more was tried. Restart the PS5, then open the game's page to finish or put back its package.",
    { name },
  );
}

/** A failed start or finish in words. Unknown text passes through. */
export function restoreErrorText(raw: string, name: string): string {
  raw = typeof raw === "string" ? raw : String(raw ?? "");
  if (/installer stopped answering/i.test(raw)) return restoreInstallerStuckText(name);
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
