// What an upload leaves on the console once nobody will finish it.
//
// A running upload writes `<dest>.ava-part`: a file preallocated to its full size at the first
// chunk, or for a new folder a staging tree (with an empty `<dest>` holding its place). They stay
// when the job stops so a Retry continues them. A row that is cancelled or removed never
// continues, and nothing else ever removes them: the space shows as "Other" on the PS5, and adding
// the same thing again is refused for room or lands beside the old copy. So they go with the row.
//
// Parts of a folder merged into an existing one (`<dest>/<file>.ava-part`) are named only by the
// console's job journal, so the helper's job sweep removes those.

import { fsDelete, fsListDir, jobCancel, jobStatus, resumeTxidForget } from "../api/ps5";
import type { QueueItem } from "../state/uploadQueue";
import { hostOf } from "./addr";
import { removeUploadPart, type UploadPartDeps } from "./uploadPart";

/** How long a stopped job gets to end (a cancel lands at the next shard boundary). */
const JOB_END_TRIES = 120;
const JOB_END_MS = 500;
/** The engine's job ends when it has told the console; the console's workers end just after. */
const CONSOLE_SETTLE_MS = 2_000;
/** A worker of the stopped job that was already past its stop check can make the part again. */
const LATE_SWEEP_MS = 15_000;

export interface LeftoverDeps extends UploadPartDeps {
  jobRunning: (jobId: string) => Promise<boolean>;
  cancelJob?: (jobId: string) => Promise<void>;
  forgetResumeId: (host: string, src: string, dest: string) => Promise<void>;
  sleep: (ms: number) => Promise<void>;
}

const realDeps: LeftoverDeps = {
  listDir: (addr, path) => fsListDir(addr, path),
  remove: (addr, path) => fsDelete(addr, path),
  jobRunning: async (jobId) => {
    try {
      return (await jobStatus(jobId)).status === "running";
    } catch {
      return false; // the engine no longer knows it
    }
  },
  cancelJob: (jobId) => jobCancel(jobId),
  forgetResumeId: (host, src, dest) => resumeTxidForget(host, src, dest),
  sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
};

type LeftoverItem = Pick<
  QueueItem,
  | "id"
  | "addr"
  | "status"
  | "sourceKind"
  | "sourcePath"
  | "resolvedDest"
  | "startedAt"
  | "interrupted"
  | "uploadDone"
  | "jobId"
>;

/** The destination whose part this row can have left: it uploads to the console, it has run, and
 *  its upload has not finished. A row that never ran owns nothing there. */
export function leftoverDest(it: LeftoverItem): string | null {
  if (it.sourceKind === "install" || it.sourceKind === "ps5") return null;
  if (it.status === "done" || it.uploadDone || !it.resolvedDest) return null;
  const ran =
    it.status === "running" ||
    it.status === "failed" ||
    !!it.interrupted ||
    it.startedAt != null;
  return ran ? it.resolvedDest : null;
}

/** Another row for the same destination on this console still counts on what is there (the parts
 *  of one archive set share a folder, and a re-add must not lose its own part to a late sweep). */
const destInUse = (it: LeftoverItem, rows: readonly LeftoverItem[]) =>
  rows.some(
    (o) =>
      o.id !== it.id &&
      o.resolvedDest === it.resolvedDest &&
      o.status !== "done" &&
      hostOf(o.addr) === hostOf(it.addr),
  );

/**
 * Call when a row is cancelled or removed, with the row as it was. A row that was running had its
 * job stopped just now: this waits for that job to end, since a part deleted under a live job
 * comes back. Never rejects: what cannot be deleted now (the console is away) stays for the
 * helper's job sweep and the delete note, and costs nothing else.
 */
export async function discardUploadLeftovers(
  it: LeftoverItem,
  rows: () => readonly LeftoverItem[],
  deps: LeftoverDeps = realDeps,
): Promise<void> {
  const dest = leftoverDest(it);
  if (!dest) return;
  const sweep = async () => {
    if (destInUse(it, rows())) return;
    await removeUploadPart(it.addr, dest, deps).catch(() => undefined);
  };
  try {
    // A single-file job's id is remembered so a re-add resumes it: nothing is left to resume.
    if (it.sourceKind !== "folder" && it.sourceKind !== "game-folder" && it.sourceKind !== "archive") {
      await deps.forgetResumeId(hostOf(it.addr), it.sourcePath, dest).catch(() => undefined);
    }
    // A row left over from before a window reload can have a job still sending: removing the row
    // is the person's way of stopping it, and nothing else would.
    const orphanJob =
      it.status !== "running" && it.interrupted && it.jobId && (await deps.jobRunning(it.jobId));
    if (it.status !== "running" && !orphanJob) {
      await sweep();
      return;
    }
    if (orphanJob && it.jobId) await deps.cancelJob?.(it.jobId).catch(() => undefined);
    if (it.jobId) {
      let running = true;
      for (let i = 0; running && i < JOB_END_TRIES; i++) {
        running = await deps.jobRunning(it.jobId);
        if (running) await deps.sleep(JOB_END_MS);
      }
      if (running) return; // still going: deleting now would only be undone
    }
    await deps.sleep(CONSOLE_SETTLE_MS);
    await sweep();
    await deps.sleep(LATE_SWEEP_MS);
    await sweep();
  } catch {
    /* best effort: see above */
  }
}
