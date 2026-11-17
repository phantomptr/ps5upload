import type { ActivationTrophies, TrophyFile, TrophyFixResult } from "../../api/ps5";

/** Repair trophies (#853): a user's trophy files still name the account ID the user had before an
 *  offline activation changed it, and the PS5 stops adding trophies to them. The flow is a check
 *  first (changes nothing), then the update, which keeps a copy of each file on the PS5 so it can
 *  be undone. */
export type TrophyRepairStep =
  | { kind: "idle" }
  | { kind: "checking" }
  /** Nothing to change. */
  | { kind: "nothing" }
  /** The check found files to update. */
  | { kind: "preview"; files: TrophyFile[]; count: number; more: boolean }
  | { kind: "applying" }
  | { kind: "done"; changed: number; failed: number; backup: string }
  | { kind: "undoing"; backup: string }
  | { kind: "undone"; restored: number }
  | { kind: "error"; message: string; gameRunning: boolean };

/** The calls the flow makes, bound to one user and console. */
export interface TrophyRepairApi {
  fix(dryRun: boolean): Promise<TrophyFixResult>;
  restore(backup: string): Promise<{ restored: number; failed: number }>;
}

/** The files a check says would be updated. */
export function changingFiles(r: TrophyFixResult): TrophyFile[] {
  return r.files.filter((f) => f.state === "would_change" || f.state === "changed");
}

/** Whether an error is the refusal for a game that is open (its trophy files may be in use). */
export function isGameRunning(e: unknown): boolean {
  return /game_running/.test(String(e));
}

function failed(e: unknown): TrophyRepairStep {
  return { kind: "error", message: String(e).replace(/^Error: /, ""), gameRunning: isGameRunning(e) };
}

/** The check: changes nothing. */
export async function checkTrophies(api: TrophyRepairApi): Promise<TrophyRepairStep> {
  try {
    const r = await api.fix(true);
    const files = changingFiles(r);
    if (r.changed === 0 || files.length === 0) return { kind: "nothing" };
    return { kind: "preview", files, count: r.changed, more: r.more };
  } catch (e) {
    return failed(e);
  }
}

/** The update, after the user saw the check. */
export async function applyTrophies(api: TrophyRepairApi): Promise<TrophyRepairStep> {
  try {
    const r = await api.fix(false);
    return { kind: "done", changed: r.changed, failed: r.failed, backup: r.backup };
  } catch (e) {
    return failed(e);
  }
}

/** Puts the files the update changed back from the copy it kept. */
export async function undoTrophies(api: TrophyRepairApi, backup: string): Promise<TrophyRepairStep> {
  try {
    const r = await api.restore(backup);
    return { kind: "undone", restored: r.restored };
  } catch (e) {
    return failed(e);
  }
}

/** What an activation's answer says about the trophy files, for one line under the account ID. */
export type ActivationTrophyNote =
  | { kind: "updated"; changed: number; failed: number }
  | { kind: "game_running" }
  | { kind: "error"; message: string }
  | null;

export function activationTrophyNote(t: ActivationTrophies | null | undefined): ActivationTrophyNote {
  if (!t) return null;
  if (t.skipped === "game_running") return { kind: "game_running" };
  if (t.skipped) return { kind: "error", message: t.error ?? t.skipped };
  if (t.error) return { kind: "error", message: t.error };
  const changed = t.changed ?? 0;
  const bad = t.failed ?? 0;
  if (changed === 0 && bad === 0) return null;
  return { kind: "updated", changed, failed: bad };
}
