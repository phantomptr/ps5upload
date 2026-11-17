// A queue row that failed because something with its name is already where it goes on the PS5.
//
// Two engine reasons say so: `ava1_exists` (refused before any data moved) and
// `ava1_commit_exists` (every byte arrived, then the console would not put it in place). A plain
// Retry fails the same way, so the row offers Replace (delete what is there, then send again) and
// Upload somewhere else (a different folder) instead.
//
// After `ava1_commit_exists` a folder's finished tree is still in `<dest>.ava-part` (AVA1
// SPEC.md §15.5). Replace then deletes what is in the way and moves that tree into place, and the
// row runs again as a Resume, which finds every file already there and sends nothing: a big game
// is not sent twice. Only a tree shown to be this row's own job's is moved (the engine still knows
// the job, it failed at its commit, under the row's job id, with every byte sent): a
// `<dest>.ava-part` left by an abandoned job could hold an older copy. Anything else (a single
// file, an archive, no tree left, a tree not shown to be this job's) is sent again.

import type { QueueItem } from "../state/uploadQueue";
import { hostOf } from "./addr";
import { removeUploadPart } from "./uploadPart";

/** `before`: refused before anything was sent. `after`: sent, but not put in place. */
export type DestinationConflict = "before" | "after";

export function destinationConflictOf(
  reason: string | null | undefined,
): DestinationConflict | null {
  if (reason === "ava1_exists") return "before";
  if (reason === "ava1_commit_exists") return "after";
  return null;
}

type ConflictRow = Pick<QueueItem, "status" | "sourceKind" | "errorReason">;

/** Whether a failed row offers Replace and Upload somewhere else: an upload (not an install, not
 *  a copy from another console) that failed because its destination is taken. Pure. */
export function offersConflictActions(it: ConflictRow): boolean {
  if (it.status !== "failed") return false;
  if (it.sourceKind === "install" || it.sourceKind === "ps5") return false;
  return destinationConflictOf(it.errorReason) !== null;
}

/** Where the row goes in `folder`: the folder plus the name it had. `flat` (an archive
 *  extracted straight into its destination): the destination is the folder itself. Pure. */
export function destInFolder(oldDest: string, folder: string, flat = false): string {
  if (flat) return folder.replace(/\/+$/, "") || "/";
  const name = oldDest.replace(/\/+$/, "").split("/").pop() ?? "";
  const dir = folder.replace(/\/+$/, "") || "";
  return `${dir}/${name}`;
}

/** What the engine still knows of the row's last job (its job snapshot), or null when it does
 *  not know it any more (an engine restart). */
export interface LastJob {
  status: string;
  error_reason?: string;
  tx_id_hex?: string;
  bytes_sent?: number;
  total_bytes?: number;
}

/** Whether `<dest>.ava-part` is the row's own job's finished tree: the job the row ran (its
 *  job id) failed at its commit because the destination is taken, under the row's job id, with
 *  every byte it planned sent (when the engine said how many). Pure. */
export function stagedTreeIsThisJobs(row: Pick<QueueItem, "txIdHex">, job: LastJob | null): boolean {
  if (!job || job.status !== "failed" || job.error_reason !== "ava1_commit_exists") return false;
  if (job.tx_id_hex && row.txIdHex && job.tx_id_hex.toLowerCase() !== row.txIdHex.toLowerCase()) {
    return false;
  }
  if (typeof job.total_bytes === "number" && typeof job.bytes_sent === "number") {
    return job.total_bytes > 0 && job.bytes_sent >= job.total_bytes;
  }
  return true;
}

export interface ReplaceDeps {
  /** Throws unless `path` is a folder on the console. */
  listDir: (addr: string, path: string) => Promise<{ name: string }[]>;
  /** The engine's snapshot of job `jobId`, or null when it no longer knows it. */
  lastJob: (jobId: string) => Promise<LastJob | null>;
  remove: (addr: string, path: string) => Promise<void>;
  move: (addr: string, from: string, to: string) => Promise<void>;
  forgetResumeId: (host: string, src: string, dest: string) => Promise<void>;
}

type ReplaceRow = Pick<
  QueueItem,
  "addr" | "sourceKind" | "sourcePath" | "resolvedDest" | "errorReason" | "jobId" | "txIdHex"
>;

/**
 * Clears the way for Replace: deletes what is at the row's destination, then either moves the
 * finished staged tree into place (`placed: true`; the caller re-runs the row as a Resume) or
 * removes any leftover part so the caller can send it all again. Throws when the destination
 * could not be deleted: nothing has changed for the row then.
 */
export async function clearForReplace(
  it: ReplaceRow,
  deps: ReplaceDeps,
): Promise<{ placed: boolean }> {
  const dest = it.resolvedDest.replace(/\/+$/, "");
  const slash = dest.lastIndexOf("/");
  if (!dest || slash < 0) throw new Error("no destination");
  const name = dest.slice(slash + 1);
  const parent = dest.slice(0, slash) || "/";
  const part = `${dest}.ava-part`;
  const names = new Set((await deps.listDir(it.addr, parent)).map((e) => e.name));

  const folder = it.sourceKind === "folder" || it.sourceKind === "game-folder";
  const tree =
    folder &&
    destinationConflictOf(it.errorReason) === "after" &&
    names.has(`${name}.ava-part`) &&
    !!it.jobId &&
    stagedTreeIsThisJobs(it, await deps.lastJob(it.jobId).catch(() => null)) &&
    (await deps.listDir(it.addr, part).then(
      () => true,
      () => false,
    ));

  if (names.has(name)) await deps.remove(it.addr, dest);

  if (tree) {
    try {
      await deps.move(it.addr, part, dest);
      return { placed: true };
    } catch {
      /* not moved: send it again below */
    }
  }
  await removeUploadPart(it.addr, dest, deps).catch(() => undefined);
  // A single file's job id is remembered by source and destination: that job ended refused, so
  // the next run must be a new one.
  if (!folder && it.sourceKind !== "archive") {
    await deps.forgetResumeId(hostOf(it.addr), it.sourcePath, it.resolvedDest).catch(() => undefined);
  }
  return { placed: false };
}
