import { create } from "zustand";

import type { JobSnapshot } from "../api/ps5";
import { hostOf } from "../lib/addr";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { jobLiveFromSnapshot, type JobLive } from "../lib/jobLive";
import { beginTask } from "./trackTask";

/**
 * An "Add files" / drag-in upload on the Files screen, kept outside the screen.
 *
 * The loop used to live in the screen with its progress in component state. Leaving the
 * screen dropped that state: the engine kept uploading, but on return there was no progress,
 * no Cancel and an armed drop zone, so the upload looked cancelled. Here the loop and its
 * state belong to the console, not the screen: the screen reads the store on every visit,
 * and the run shows in Tasks whichever screen is open.
 *
 * Per console, like `fsBulkOp`: one run per console at a time, each independent.
 */

export interface FsUploadActive {
  /** The name the current item gets on the console. */
  name: string;
  /** Which item of the pick this is (0-based) and how many there are. */
  index: number;
  count: number;
  /** The folder on the console the items land in. */
  destDir: string;
  /** "" until the engine has answered with the job. */
  jobId: string;
  sent: number;
  total: number;
  /** The engine's live notes: carries the console's "finishing" counts. */
  live: JobLive | undefined;
  startedAtMs: number;
  /** Set while the run waits to try the current item again after a dropped connection. */
  retry: { attempt: number; of: number } | null;
}

/** A run that did not finish, whatever ended it: what is left of it, so it can be resumed.
 *  The console keeps what already landed (and the partial file), so a resume sends only
 *  the rest. Saved to disk, so it is still offered after the app restarts. */
export interface FsUploadStopped {
  /** What ended it: a failure, the user's Stop, or the app closing or crashing mid-run. */
  why: "failed" | "user" | "interrupted";
  /** The engine's error, raw; "" when nothing failed. */
  error: string;
  /** The engine's reason token, when it gave one. */
  reason: string | null;
  addr: string;
  destDir: string;
  /** The items not finished, the failed one first. */
  srcPaths: string[];
  replaceRemoteName?: string;
  /** How many of the pick had finished, and how many it was. */
  doneCount: number;
  count: number;
  /** The name of the item it stopped on. */
  name: string;
  /** The engine job that item was on, when the run was interrupted: it may still be going. */
  jobId?: string;
}

export interface FsUploadState {
  active: FsUploadActive | null;
  /** The last run's failure and what is left of it, until resumed or dismissed. */
  stopped: FsUploadStopped | null;
  cancelRequested: boolean;
  /** Counts finished runs, so a screen can re-list the folder when one ends. */
  finishedRuns: number;
}

interface FsUploadStore {
  byHost: Record<string, FsUploadState>;
  patch: (host: string, patch: Partial<FsUploadState>) => void;
}

/** Stable idle reference: selectors compare by identity. */
export const IDLE_FS_UPLOAD: FsUploadState = {
  active: null,
  stopped: null,
  cancelRequested: false,
  finishedRuns: 0,
};

export function fsUploadForHost(
  s: { byHost: Record<string, FsUploadState> },
  host: string,
): FsUploadState {
  return s.byHost[hostOf(host)] ?? IDLE_FS_UPLOAD;
}

export const useFsUploadStore = create<FsUploadStore>((set) => ({
  byHost: {},
  patch: (host, patch) =>
    set((s) => {
      const key = hostOf(host);
      return {
        byHost: {
          ...s.byHost,
          [key]: { ...(s.byHost[key] ?? IDLE_FS_UPLOAD), ...patch },
        },
      };
    }),
}));

const SAVE_KEY = "ps5upload.fsUpload.v1";

/** Each console's run in flight, as it would be resumed if the app died right now. */
const inflight: Record<string, FsUploadStopped> = {};

/** Writes every console's resumable run to disk: the stopped ones, and the ones in flight
 *  (which a restart finds as "interrupted"). Called at item boundaries, never per byte. */
function save(): void {
  const out: Record<string, FsUploadStopped> = { ...inflight };
  for (const [key, st] of Object.entries(useFsUploadStore.getState().byHost)) {
    if (st.stopped && !(key in out)) out[key] = st.stopped;
  }
  safeSetItem(SAVE_KEY, JSON.stringify(out));
}

/** Brings back what the last session left: a stopped run as it was, a run that was in flight
 *  as "interrupted". Runs once when the app starts. */
export function restoreFsUploads(): void {
  let saved: Record<string, FsUploadStopped>;
  try {
    saved = JSON.parse(safeGetItem(SAVE_KEY) ?? "{}") as Record<
      string,
      FsUploadStopped
    >;
  } catch {
    return;
  }
  for (const key of Object.keys(inflight)) delete inflight[key];
  const { patch } = useFsUploadStore.getState();
  for (const [key, st] of Object.entries(saved ?? {})) {
    if (!st || !Array.isArray(st.srcPaths) || st.srcPaths.length === 0)
      continue;
    patch(key, { stopped: st });
  }
}

/** What the run needs from the app. Injected so the loop is testable without an engine. */
export interface FsUploadDeps {
  pathKind: (path: string) => Promise<string>;
  /** `capMbps`: the upload speed limit (0 = none), read as each job starts. */
  startFile: (src: string, dest: string, addr: string, capMbps: number) => Promise<string>;
  startDir: (src: string, dest: string, addr: string, capMbps: number) => Promise<string>;
  /** The user's upload speed limit in MB/s, 0 = none. Absent: no limit. */
  bandwidthCapMbps?: () => number;
  jobStatus: (jobId: string) => Promise<JobSnapshot>;
  jobCancel: (jobId: string) => Promise<void>;
  sleep: (ms: number) => Promise<void>;
  /** Told the raw error when a run fails (not when it is cancelled). */
  onFailed?: (raw: string) => void;
  /** Whether a failed item is worth trying again unasked (a dropped connection is; a full
   *  drive is not). Absent: never. */
  shouldRetry?: (reason: string | undefined, error: string) => boolean;
}

/** How many times one item is tried again unasked, and the wait before each. */
const RETRY_WAITS_MS = [5_000, 15_000, 30_000] as const;

/** A failed job, with the engine's reason kept beside its message. */
class JobFailed extends Error {
  constructor(
    message: string,
    readonly reason: string | undefined,
  ) {
    super(message);
  }
}

export interface FsUploadRequest {
  host: string;
  addr: string;
  destDir: string;
  srcPaths: string[];
  /** Replace one file: the single picked file lands under this name. */
  replaceRemoteName?: string;
  /** The task's label in Tasks. */
  label?: string;
  /** A resumed run: how many of the original pick had already finished. */
  doneBefore?: number;
  /** A resumed run: the engine job its first item was on, to pick back up if it still runs. */
  attachJobId?: string;
}

const joinRemote = (dir: string, name: string) =>
  dir.endsWith("/") ? `${dir}${name}` : `${dir}/${name}`;

/** Uploads the picked items one after another into `destDir`. Sequential on purpose: a
 *  failure stops the batch instead of racing more writes onto a full or read-only drive. */
export async function runFsUpload(
  req: FsUploadRequest,
  deps: FsUploadDeps,
): Promise<{ ok: boolean; error?: string }> {
  const { host, addr, destDir, srcPaths, replaceRemoteName } = req;
  if (srcPaths.length === 0) return { ok: true };
  const store = useFsUploadStore.getState();
  if (fsUploadForHost(store, host).active)
    return { ok: false, error: "an upload is already running" };

  const cancelled = () =>
    fsUploadForHost(useFsUploadStore.getState(), host).cancelRequested;
  const finished = () =>
    fsUploadForHost(useFsUploadStore.getState(), host).finishedRuns + 1;
  const startedAtMs = Date.now();
  const hostKey = hostOf(host);
  const doneBefore = req.doneBefore ?? 0;
  const count = doneBefore + srcPaths.length;
  const nameOf = (src: string) =>
    replaceRemoteName ?? (src.split(/[\\/]/).pop() || "file");
  const firstName = nameOf(srcPaths[0]);
  const task = beginTask({
    kind: srcPaths.length === 1 ? "upload-file" : "upload-dir",
    origin: "files",
    label:
      req.label ??
      (srcPaths.length === 1
        ? firstName
        : `${firstName} +${srcPaths.length - 1}`),
    consoleId: hostOf(host),
    detail: destDir,
  });
  store.patch(host, { stopped: null, cancelRequested: false });
  save();

  let i = 0;
  // The item the run is on (or about to start): where a resume picks up.
  let at = 0;
  /** What is left from `at`, as a stopped run. */
  const left = (
    why: FsUploadStopped["why"],
    error: string,
    reason: string | null,
  ): FsUploadStopped => ({
    why,
    error,
    reason,
    addr,
    destDir,
    srcPaths: srcPaths.slice(at),
    replaceRemoteName,
    doneCount: doneBefore + at,
    count,
    name: nameOf(srcPaths[Math.min(at, srcPaths.length - 1)]),
  });
  const ended = (stopped: FsUploadStopped | null) => {
    delete inflight[hostKey];
    store.patch(host, {
      active: null,
      cancelRequested: false,
      finishedRuns: finished(),
      stopped,
    });
    save();
  };
  try {
    for (; i < srcPaths.length; i++) {
      at = i;
      if (cancelled()) break;
      const src = srcPaths[i];
      const name = nameOf(src);
      const base: FsUploadActive = {
        name,
        index: doneBefore + i,
        count,
        destDir,
        jobId: "",
        sent: 0,
        total: 0,
        live: undefined,
        startedAtMs,
        retry: null,
      };
      // A folder (picked with Add folder, or dropped) uploads whole, into a same-named
      // folder; a file goes up on its own. A replacement is always one file.
      const isFolder =
        replaceRemoteName === undefined &&
        (await deps.pathKind(src)) === "folder";
      const dest = joinRemote(destDir, name);
      // One item, tried again by itself after a dropped connection: the console kept what
      // landed, so each try sends only the rest.
      // If the app dies from here on, this is what the next launch offers to resume.
      const leftNow = (jobId?: string): FsUploadStopped => ({
        why: "interrupted",
        error: "",
        reason: null,
        addr,
        destDir,
        srcPaths: srcPaths.slice(i),
        replaceRemoteName,
        doneCount: doneBefore + i,
        count,
        name,
        jobId,
      });
      for (let attempt = 0; ; attempt++) {
        base.jobId = "";
        base.retry = null;
        store.patch(host, { active: { ...base } });
        inflight[hostKey] = leftNow();
        save();
        try {
          // An interrupted run's job may have carried on without the app: watch it rather
          // than start a second one onto the same file.
          let jobId = "";
          if (i === 0 && attempt === 0 && req.attachJobId) {
            const old = await deps.jobStatus(req.attachJobId).catch(() => null);
            if (old?.status === "done") break;
            if (old?.status === "running") jobId = req.attachJobId;
          }
          if (!jobId) {
            const cap = deps.bandwidthCapMbps?.() ?? 0;
            jobId = isFolder
              ? await deps.startDir(src, dest, addr, cap)
              : await deps.startFile(src, dest, addr, cap);
          }
          base.jobId = jobId;
          store.patch(host, { active: { ...base } });
          inflight[hostKey] = leftNow(jobId);
          save();
          // A Cancel pressed before the job id arrived had nothing to end: end it now.
          if (cancelled()) void deps.jobCancel(jobId).catch(() => {});
          for (;;) {
            const snap = await deps.jobStatus(jobId);
            if (snap.status === "done") break;
            if (snap.status === "failed") {
              if (cancelled()) break;
              throw new JobFailed(
                snap.error ?? "upload failed",
                snap.error_reason,
              );
            }
            const sent = snap.bytes_sent ?? 0;
            const total = snap.total_bytes ?? 0;
            base.sent = sent;
            base.total = total;
            store.patch(host, {
              active: { ...base, live: jobLiveFromSnapshot(snap) },
            });
            task.report({
              stage:
                count > 1 ? `${doneBefore + i + 1}/${count} ${name}` : name,
              progress:
                total > 0 ? { current: sent, total, unit: "bytes" } : undefined,
            });
            await deps.sleep(500);
          }
          break;
        } catch (e) {
          const raw = e instanceof Error ? e.message : String(e);
          const reason = e instanceof JobFailed ? e.reason : undefined;
          if (
            cancelled() ||
            attempt >= RETRY_WAITS_MS.length ||
            !deps.shouldRetry?.(reason, raw)
          )
            throw e;
          base.retry = { attempt: attempt + 1, of: RETRY_WAITS_MS.length };
          store.patch(host, { active: { ...base } });
          await deps.sleep(RETRY_WAITS_MS[attempt]);
          if (cancelled()) break;
        }
      }
      // Stopped during this item: it is not done, so a resume starts with it.
      if (cancelled()) break;
    }
  } catch (e) {
    const raw = e instanceof Error ? e.message : String(e);
    task.fail(e);
    ended(
      left("failed", raw, e instanceof JobFailed ? (e.reason ?? null) : null),
    );
    deps.onFailed?.(raw);
    return { ok: false, error: raw };
  }
  const wasCancelled = cancelled();
  if (wasCancelled) task.fail(new Error("Cancelled"));
  else task.done();
  // Stopped on purpose is resumable too: the console kept what landed.
  ended(wasCancelled && at < srcPaths.length ? left("user", "", null) : null);
  return { ok: !wasCancelled };
}

/** Uploads what a stopped run left, the failed item first. */
export async function resumeFsUpload(
  host: string,
  deps: FsUploadDeps,
): Promise<{ ok: boolean; error?: string }> {
  const cur = fsUploadForHost(useFsUploadStore.getState(), host);
  const st = cur.stopped;
  if (!st || cur.active) return { ok: false };
  return runFsUpload(
    {
      host,
      addr: st.addr,
      destDir: st.destDir,
      srcPaths: st.srcPaths,
      replaceRemoteName: st.replaceRemoteName,
      doneBefore: st.doneCount,
      attachJobId: st.why === "interrupted" ? st.jobId : undefined,
    },
    deps,
  );
}

/** Forgets a stopped run (the user does not want the rest). The partial stays on the console
 *  until the helper clears it. */
export function dismissFsUploadStopped(host: string): void {
  useFsUploadStore.getState().patch(host, { stopped: null });
  save();
}

/** Stops the console's run: the engine ends the transfer job, and the batch stops before the
 *  next item. What already landed stays; the half-written file does not. */
export function cancelFsUpload(
  host: string,
  deps: Pick<FsUploadDeps, "jobCancel">,
): void {
  const s = useFsUploadStore.getState();
  const cur = fsUploadForHost(s, host);
  if (!cur.active) return;
  s.patch(host, { cancelRequested: true });
  if (cur.active.jobId) void deps.jobCancel(cur.active.jobId).catch(() => {});
}

// What the last session left behind is offered again as soon as the app starts.
restoreFsUploads();
