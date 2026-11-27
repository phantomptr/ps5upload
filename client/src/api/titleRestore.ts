// Restore to home screen (#961): the engine's /api/ps5/app/restore* routes, through the
// `ps5_app_restore` command (desktop) or the same routes over HTTP (browser build).
//
// A game installed from a package whose home-screen record is gone (Settings › Rebuild Database
// drops games installed from fake packages; their files stay) is put back by installing the
// package that is already on the console again, with the console's own installer.

import { mgmtAddr } from "../lib/addr";
import { invoke } from "../lib/invokeLogged";

/** The helper's answer (`app.restore`), as the engine passes it on. */
export interface RestoreReply {
  ok: boolean;
  err?: string;
  title_id: string;
  /** check: restorable | registered | record_elsewhere | not_installed | pending |
   *  folder_registration | ambiguous | retail | no_room | aside_cross_device | db_unreadable |
   *  invalid_title | system_title. finish: restored | rolled_back | nothing_pending | waiting |
   *  unsettled. */
  state: string;
  reason?: string;
  /** internal | m2 | usb | extended */
  where?: string;
  volume?: string;
  aside?: string;
  pkg?: string;
  size?: number;
  kind?: string;
  patch?: string;
  patch_size?: number;
  free_volume?: number;
  free_internal?: number;
  install_path?: string;
  patch_aside?: string;
  backup?: string;
  installed?: string;
  installed_where?: string;
  patch_kept?: string;
  kept?: string;
}

export interface RestoreCandidate {
  title_id: string;
  title_name: string;
  check: RestoreReply;
}

/** queued | installing | finishing | restored | rolled_back | pending | failed */
export type RestorePhase =
  | "queued"
  | "installing"
  | "finishing"
  | "restored"
  | "rolled_back"
  | "pending"
  | "failed";

export interface RestoreRun {
  title_id: string;
  phase: RestorePhase;
  job?: string;
  prepared?: RestoreReply;
  result?: RestoreReply;
  install_verdict?: string;
  install_hint?: string;
  error?: string;
  /** The install job's status while it runs (metrics.served_bytes / total_bytes). */
  install?: { phase?: string; metrics?: { total_bytes?: number; served_bytes?: number } };
}

function call<T>(
  op: "check" | "candidates" | "status" | "start" | "finish",
  host: string,
  req: { title_id?: string; title_ids?: string[]; rollback?: boolean; install_ended?: boolean },
): Promise<T> {
  return invoke<T>("ps5_app_restore", { op, req: { addr: mgmtAddr(host), ...req } });
}

/** Read-only: can this title be put back on the home screen. Rejects when the helper is too old
 *  (`restore_unsupported`) or the console could not answer. */
export function restoreCheck(host: string, titleId: string): Promise<RestoreReply> {
  return call<RestoreReply>("check", host, { title_id: titleId });
}

/** Every game the console has the package of but no home-screen record for. Read-only. */
export async function restoreCandidates(host: string): Promise<RestoreCandidate[]> {
  const r = await call<{ titles?: RestoreCandidate[] }>("candidates", host, {});
  return r.titles ?? [];
}

/** Start restoring one or more titles (one install after the other). */
export function restoreStart(
  host: string,
  titleIds: string[],
): Promise<{ ok: boolean; job: string; prepared: RestoreReply; queued: string[] }> {
  return call("start", host, { title_ids: titleIds });
}

/** How this engine's restore of a title stands. Rejects (404) when it ran none. */
export function restoreStatus(host: string, titleId: string): Promise<RestoreRun> {
  return call<RestoreRun>("status", host, { title_id: titleId });
}

/** Finish a restore left pending: `installEnded` deletes the copy aside once the game is on the
 *  home screen; `rollback` puts the package back where it was. */
export function restoreFinish(
  host: string,
  titleId: string,
  opts: { rollback?: boolean; installEnded?: boolean },
): Promise<RestoreReply> {
  return call<RestoreReply>("finish", host, {
    title_id: titleId,
    rollback: opts.rollback === true,
    install_ended: opts.installEnded === true,
  });
}
