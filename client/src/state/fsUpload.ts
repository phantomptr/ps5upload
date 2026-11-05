import { create } from "zustand";

import type { JobSnapshot } from "../api/ps5";
import { hostOf } from "../lib/addr";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { jobLiveFromSnapshot, type JobLive } from "../lib/jobLive";
import { resumableTxId } from "../lib/resumeGuard";
import { removeUploadPart } from "../lib/uploadPart";
import { beginTask } from "./trackTask";
import {
  baseNameOf,
  landedOfList,
  sendOrder,
  stepCount,
  type FsExistingEntry,
  type FsUploadStep,
} from "./fsUploadPlan";
import { isJobUnknownError } from "../lib/pollRead";

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
  /** Includes `resumed`: the engine counts what a resumed upload found on the console as done. */
  sent: number;
  total: number;
  /** A resumed upload: what the console already held when the job opened (part of `sent`, not
   *  of this run's speed). 0 or absent on a fresh upload. */
  resumed?: number;
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
  /** Owning queued pick, so a restart resumes it only once. */
  queueId?: string;
  /** What ended it: a failure, the user's Stop, or the app closing or crashing mid-run. */
  why: "failed" | "user" | "interrupted";
  /** The engine's error, raw; "" when nothing failed. */
  error: string;
  /** The engine's reason token, when it gave one. */
  reason: string | null;
  addr: string;
  destDir: string;
  /** The sources of the steps not finished, the failed one first (a list contributes each of
   *  its files). What a run saved before steps existed carries only this. */
  srcPaths: string[];
  replaceRemoteName?: string;
  /** The steps not finished, the failed one first. A list step is kept whole, with its job id:
   *  a resume sends the same list under the same id, and the console's journal skips the files
   *  it already has. */
  steps?: FsUploadStep[];
  /** How many of the pick had finished, and how many it was. Counts the files of a stopped list
   *  that are known to be in place (`notLanded` names the rest). */
  doneCount: number;
  count: number;
  /** How many of the pick were in finished steps: where a resume's count starts. Absent: the
   *  same as `doneCount`. */
  stepsDone?: number;
  /** The name of the item it stopped on. */
  name: string;
  /** A stopped list: the names of its files not known to be in place. */
  notLanded?: string[];
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
  /** Files of the last run the engine left out because this computer could not read them. */
  unreadable?: string[];
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
 *  (which a restart finds as "interrupted"). Called at step boundaries, never per byte. */
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
    if (!st) continue;
    const hasSteps = Array.isArray(st.steps) && st.steps.length > 0;
    const hasSrcs = Array.isArray(st.srcPaths) && st.srcPaths.length > 0;
    if (!hasSteps && !hasSrcs) continue;
    patch(key, { stopped: st });
  }
}

/** What the run needs from the app. Injected so the loop is testable without an engine. */
export interface FsUploadDeps {
  pathKind: (path: string) => Promise<string>;
  /** `capMbps`: the upload speed limit (0 = none), read as each job starts. `txId`: the job's id,
   *  the same on every retry and resume of a step, so the console continues its partial. */
  startFile: (src: string, dest: string, addr: string, capMbps: number, txId?: string) => Promise<string>;
  startDir: (src: string, dest: string, addr: string, capMbps: number, txId?: string) => Promise<string>;
  /** Mints a job id for a step that has none. Absent: the engine picks one per try. */
  newTxId?: () => string;
  /** Starts ONE file-list job (POST /api/transfer/file-list) under `txId`: every file to
   *  `dest` below `destRoot`. Absent: list steps go file by file. */
  startList?: (
    destRoot: string,
    files: { src: string; dest: string }[],
    addr: string,
    txId: string,
    capMbps: number,
  ) => Promise<string | { jobId: string; unreadable: string[] }>;
  /** The console folder's listing, to tell which files of a stopped list are in place. */
  listDir?: (addr: string, dir: string) => Promise<FsExistingEntry[]>;
  /** Deletes a path on the console (what a stopped run left, when the person gives it up). */
  deleteRemote?: (addr: string, path: string) => Promise<void>;
  /** A running job's planned files with their sizes (GET /api/jobs/{id}/files). */
  jobFiles?: (jobId: string) => Promise<{ rel_path: string; size: number }[] | null>;
  /** The user's upload speed limit in MiB/s, 0 = none. Absent: no limit. */
  bandwidthCapMbps?: () => number;
  jobStatus: (jobId: string) => Promise<JobSnapshot>;
  jobCancel: (jobId: string) => Promise<void>;
  sleep: (ms: number) => Promise<void>;
  /** Told the raw error when a run fails (not when it is cancelled). */
  onFailed?: (raw: string) => void;
  /** Whether a failed item is worth trying again unasked (a dropped connection is; a full
   *  drive is not). `host` is the console the run goes to. Absent: never. */
  shouldRetry?: (reason: string | undefined, error: string, host: string) => boolean;
}

/** How many times one step is tried again unasked, and the wait before each. */
const RETRY_WAITS_MS = [5_000, 15_000, 30_000] as const;
const RETRY_WAIT_SLICE_MS = 250;
/** Status reads that may fail in a row before the step counts as failed (the queue's bound).
 *  The engine runs the job by itself: one failed read is not a failed job. */
const MAX_POLL_FAILURES = 6;
const jobNotFound = isJobUnknownError;

/** A failed job, with the engine's reason kept beside its message. */
class JobFailed extends Error {
  constructor(
    message: string,
    readonly reason: string | undefined,
  ) {
    super(message);
  }
}

/** The engine (or the desktop shell) does not know the file-list job: the engine answers
 *  HTTP 404/405 for a route it lacks, the desktop shell "command … not found" for a command it
 *  lacks. Only those: anything else (a source that cannot be read, a busy console) is a real
 *  failure, never a reason to re-send the files one by one. The list then goes file by file,
 *  exactly as before. Every engine this app ships with has the route (since 5.35 at least), and
 *  the AVA1 job it runs is the same kind a folder upload runs (a manifest of files under one
 *  root, merged into the existing folder), so the helper on the console needs nothing newer:
 *  there is no helper version to detect. */
export function listUnsupported(e: unknown): boolean {
  const msg = e instanceof Error ? e.message : String(e);
  return /^engine HTTP (404|405)\b|^command [\w-]+ not found$/i.test(msg.trim());
}

/** What `runFsUpload` answers when the console already has a run. */
export const FS_UPLOAD_BUSY = "an upload is already running";

export interface FsUploadRequest {
  queueId?: string;
  host: string;
  addr: string;
  destDir: string;
  /** Items as one job each (what a run saved before steps existed resumes as this). */
  srcPaths: string[];
  /** The planned steps (state/fsUploadPlan). When given, `srcPaths` is ignored. */
  steps?: FsUploadStep[];
  /** Replace one file: the single picked file lands under this name. */
  replaceRemoteName?: string;
  /** The task's label in Tasks. */
  label?: string;
  /** A resumed run: how many of the original pick had already finished. */
  doneBefore?: number;
  /** A resumed run: how many the original pick was. */
  countTotal?: number;
  /** A resumed run: the engine job its first step was on, to pick back up if it still runs. */
  attachJobId?: string;
}

const joinRemote = (dir: string, name: string) =>
  dir.endsWith("/") ? `${dir}${name}` : `${dir}/${name}`;

/** The steps a request stands for: its own, or one per source path (the old shape). */
function stepsOf(req: FsUploadRequest): FsUploadStep[] {
  if (req.steps) return req.steps;
  return req.srcPaths.map((src) =>
    req.replaceRemoteName !== undefined
      ? { kind: "one", src, name: req.replaceRemoteName, asFile: true }
      : { kind: "one", src, name: baseNameOf(src) },
  );
}

const srcsOf = (steps: FsUploadStep[]) =>
  steps.flatMap((s) => (s.kind === "list" ? s.files.map((f) => f.src) : [s.src]));

/** A list step as one job per file (no file-list job on this engine). */
const unlist = (s: FsUploadStep): FsUploadStep[] =>
  s.kind === "list"
    ? s.files.map((f) => ({ kind: "one", src: f.src, name: f.name, asFile: true }))
    : [s];

/** Uploads the planned steps one after another into `destDir`. Sequential on purpose: a
 *  failure stops the run instead of racing more writes onto a full or read-only drive. */
export async function runFsUpload(
  req: FsUploadRequest,
  deps: FsUploadDeps,
): Promise<{ ok: boolean; error?: string }> {
  const { host, addr, destDir, replaceRemoteName } = req;
  // A copy: a list that falls back to file by file is spliced in place.
  const steps = [...stepsOf(req)];
  if (steps.length === 0) return { ok: true };
  const store = useFsUploadStore.getState();
  if (fsUploadForHost(store, host).active)
    return { ok: false, error: FS_UPLOAD_BUSY };

  const cancelled = () =>
    fsUploadForHost(useFsUploadStore.getState(), host).cancelRequested;
  const finished = () =>
    fsUploadForHost(useFsUploadStore.getState(), host).finishedRuns + 1;
  const startedAtMs = Date.now();
  const hostKey = hostOf(host);
  const doneBefore = req.doneBefore ?? 0;
  const pickCount = steps.reduce((n, s) => n + stepCount(s), 0);
  const count = Math.max(req.countTotal ?? 0, doneBefore + pickCount);
  const firstName =
    steps[0].kind === "list" ? steps[0].files[0]?.name ?? "" : steps[0].name;
  const task = beginTask({
    kind: pickCount === 1 ? "upload-file" : "upload-dir",
    origin: "files",
    label:
      req.label ??
      (pickCount === 1 ? firstName : `${firstName} +${pickCount - 1}`),
    consoleId: hostOf(host),
    detail: destDir,
  });
  store.patch(host, { stopped: null, cancelRequested: false, unreadable: [] });
  save();

  // The step the run is on (or about to start): where a resume picks up.
  let at = 0;
  /** Items of the pick in the steps before `at`. */
  let doneItems = 0;
  /** A stopped list step: what of it is known to be in place. */
  let partial: { landed: number; notLanded: string[] } | null = null;
  /** What is left from `at`, as a stopped run. */
  const left = (
    why: FsUploadStopped["why"],
    error: string,
    reason: string | null,
  ): FsUploadStopped => {
    const rest = steps.slice(at);
    const cur = rest[0];
    const stepsDone = doneBefore + doneItems;
    return {
      queueId: req.queueId,
      why,
      error,
      reason,
      addr,
      destDir,
      srcPaths: srcsOf(rest),
      replaceRemoteName,
      steps: rest,
      doneCount: stepsDone + (partial?.landed ?? 0),
      stepsDone,
      count,
      name:
        partial?.notLanded[0] ??
        (cur ? (cur.kind === "list" ? cur.files[0]?.name ?? "" : cur.name) : ""),
      ...(partial && partial.notLanded.length > 0
        ? { notLanded: partial.notLanded }
        : {}),
    };
  };
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

  /** After a list step stopped: which of its files are in place (best effort). */
  const settleList = async (
    step: Extract<FsUploadStep, { kind: "list" }>,
    sizes: Map<string, number> | null,
  ) => {
    const listing = deps.listDir
      ? await deps.listDir(addr, destDir).catch(() => null)
      : null;
    const r = landedOfList(step.files, listing, sizes);
    partial = { landed: r.landed.length, notLanded: r.notLanded.map((f) => f.name) };
  };

  try {
    for (; at < steps.length; ) {
      if (cancelled()) break;
      let step = steps[at];
      // A list on an engine without the file-list job goes file by file (see listUnsupported).
      if (step.kind === "list" && !deps.startList) {
        steps.splice(at, 1, ...unlist(step));
        continue;
      }
      // Written back: `left` saves the step, so a resume runs under the same id.
      if (step.kind === "one" && !step.txId && deps.newTxId) {
        step = { ...step, txId: deps.newTxId() };
        steps[at] = step;
      }
      const isList = step.kind === "list";
      const order = step.kind === "list" ? sendOrder(step.files) : [];
      const stepName =
        step.kind === "list" ? order[0] ?? "" : step.name;
      const base: FsUploadActive = {
        name: stepName,
        index: doneBefore + doneItems,
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
        step.kind === "one" &&
        !step.asFile &&
        (await deps.pathKind(step.src)) === "folder";
      // If the app dies from here on, this is what the next launch offers to resume.
      const leftNow = (jobId?: string): FsUploadStopped => ({
        ...left("interrupted", "", null),
        jobId,
      });
      /** Sizes the engine planned for the list's files, by name (for the landed report). */
      let sizes: Map<string, number> | null = null;
      let fellBack = false;
      let stepFailed: unknown = null;
      /** The last try's job when it failed without the engine saying it ended (its status
       *  reads failed): it may still be running, so the next try watches it rather than
       *  start a second job onto the same files. */
      let unsettledJob = "";
      for (let attempt = 0; ; attempt++) {
        base.jobId = "";
        base.retry = null;
        store.patch(host, { active: { ...base } });
        inflight[hostKey] = leftNow();
        save();
        try {
          // An interrupted run's job may have carried on without the app: watch it rather
          // than start a second one onto the same files.
          let jobId = "";
          if (at === 0 && attempt === 0 && req.attachJobId) {
            const old = await deps.jobStatus(req.attachJobId).catch(() => null);
            if (old?.status === "done") break;
            if (old?.status === "running") jobId = req.attachJobId;
          }
          if (!jobId && unsettledJob) {
            const prev = unsettledJob;
            let old: JobSnapshot | null = null;
            try {
              old = await deps.jobStatus(prev);
            } catch (e) {
              // Still no answer: starting now could run beside it. This try fails instead.
              if (!jobNotFound(e)) throw e;
            }
            unsettledJob = "";
            if (old?.status === "done") break;
            if (old?.status === "running") jobId = prev;
          }
          if (!jobId) {
            const cap = deps.bandwidthCapMbps?.() ?? 0;
            if (step.kind === "list") {
              try {
                // The same list under the same id every time (retry, resume): the console's
                // journal then sends only what it does not have.
                const started = await deps.startList!(
                  destDir,
                  step.files.map((f) => ({
                    src: f.src,
                    dest: joinRemote(destDir, f.name),
                  })),
                  addr,
                  step.txId,
                  cap,
                );
                if (typeof started === "string") jobId = started;
                else {
                  jobId = started.jobId;
                  if (started.unreadable.length > 0) store.patch(host, { unreadable: started.unreadable });
                }
              } catch (e) {
                if (!listUnsupported(e)) throw e;
                fellBack = true;
                break;
              }
            } else {
              const dest = joinRemote(destDir, step.name);
              // A resume trusts the console's journal: not when the folder it wrote has been
              // deleted since (the job would land with those files missing).
              if (isFolder && at === 0 && attempt === 0 && req.doneBefore !== undefined && step.txId && deps.newTxId && deps.listDir) {
                const txId = await resumableTxId(addr, dest, step.txId, deps.newTxId, deps.listDir);
                if (txId !== step.txId) {
                  step = { ...step, txId };
                  steps[at] = step;
                }
              }
              jobId = isFolder
                ? await deps.startDir(step.src, dest, addr, cap, step.txId)
                : await deps.startFile(step.src, dest, addr, cap, step.txId);
            }
          }
          base.jobId = jobId;
          store.patch(host, { active: { ...base } });
          inflight[hostKey] = leftNow(jobId);
          save();
          // A Cancel pressed before the job id arrived had nothing to end: end it now.
          if (cancelled()) void deps.jobCancel(jobId).catch(() => {});
          let askedFiles = false;
          let misses = 0;
          for (;;) {
            let snap: JobSnapshot;
            try {
              snap = await deps.jobStatus(jobId);
            } catch (e) {
              if (jobNotFound(e) || ++misses >= MAX_POLL_FAILURES) throw e;
              await deps.sleep(500);
              continue;
            }
            misses = 0;
            if (snap.status === "done") break;
            if (snap.status === "failed") {
              if (cancelled()) break;
              throw new JobFailed(
                snap.error ?? "upload failed",
                snap.error_reason,
              );
            }
            if (isList && !askedFiles && deps.jobFiles && (snap.files_count ?? 0) > 0) {
              askedFiles = true;
              const planned = await deps.jobFiles(jobId).catch(() => null);
              if (planned && planned.length > 0)
                sizes = new Map(planned.map((f) => [f.rel_path, f.size]));
            }
            const sent = snap.bytes_sent ?? 0;
            const total = snap.total_bytes ?? 0;
            base.sent = sent;
            base.total = total;
            base.resumed = snap.resumed_bytes ?? 0;
            if (isList && order.length > 0) {
              // The file the sender is on (its counter, in the order it sends them), and how
              // many the console has made permanent: "N of M" counts finished files, which the
              // sender's counter runs well ahead of on small files.
              const last = order.length - 1;
              const k = Math.min(last, Math.max(0, (snap.files_processing ?? 0) - 1));
              const durable = Math.min(last, Math.max(0, snap.files_finalized ?? 0));
              base.name = order[k];
              base.index = doneBefore + doneItems + durable;
            }
            store.patch(host, {
              active: { ...base, live: jobLiveFromSnapshot(snap) },
            });
            task.report({
              stage:
                count > 1
                  ? `${base.index + 1}/${count} ${base.name}`
                  : base.name,
              progress:
                total > 0 ? { current: sent, total, unit: "bytes" } : undefined,
            });
            await deps.sleep(500);
          }
          break;
        } catch (e) {
          const raw = e instanceof Error ? e.message : String(e);
          const reason = e instanceof JobFailed ? e.reason : undefined;
          if (base.jobId) unsettledJob = e instanceof JobFailed ? "" : base.jobId;
          if (
            cancelled() ||
            attempt >= RETRY_WAITS_MS.length ||
            !deps.shouldRetry?.(reason, raw, host)
          ) {
            stepFailed = e;
            break;
          }
          base.retry = { attempt: attempt + 1, of: RETRY_WAITS_MS.length };
          store.patch(host, { active: { ...base } });
          // In slices: Cancel ends the wait, instead of the row staying busy for up to 30 s.
          for (let left = RETRY_WAITS_MS[attempt]; left > 0 && !cancelled(); left -= RETRY_WAIT_SLICE_MS) {
            await deps.sleep(Math.min(left, RETRY_WAIT_SLICE_MS));
          }
          if (cancelled()) break;
        }
      }
      if (fellBack) {
        steps.splice(at, 1, ...unlist(step));
        continue;
      }
      // Stopped or failed during this step: it is not done, so a resume starts with it. A
      // list says which of its files made it.
      if (stepFailed !== null || cancelled()) {
        if (step.kind === "list") await settleList(step, sizes);
        if (stepFailed !== null) throw stepFailed;
        break;
      }
      doneItems += stepCount(step);
      at++;
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
  ended(wasCancelled && at < steps.length ? left("user", "", null) : null);
  return { ok: !wasCancelled };
}

/** Uploads what a stopped run left, the failed step first.
 *
 *  A stopped list is re-sent exactly as planned (same files, same names, same job id): that is
 *  what lets the console's journal skip the files it already has. Its Keep both names are NOT
 *  re-checked against the folder: the job's own partial files may already sit under them, and a
 *  renamed entry would change the job's manifest, so the console would start the job over.
 *  The window is small (a name taken by something else between the stop and the resume), and
 *  what it costs is the same as a Replace of that name. */
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
      queueId: st.queueId,
      addr: st.addr,
      destDir: st.destDir,
      srcPaths: st.srcPaths,
      steps: st.steps,
      replaceRemoteName: st.replaceRemoteName,
      doneBefore: st.stepsDone ?? st.doneCount,
      countTotal: st.count,
      attachJobId: st.why === "interrupted" ? st.jobId : undefined,
    },
    deps,
  );
}

/** Clears the stopped card without giving anything up: its steps live on as a paused pick. */
export function dismissFsUploadStopped(host: string): void {
  useFsUploadStore.getState().patch(host, { stopped: null });
  save();
}

/** Deletes what a given-up run left on the console: the partial of the step it stopped on. Only
 *  that step can have one; the steps after it never started. Best effort: what cannot be deleted
 *  now stays for the helper's job sweep. */
export async function discardFsUploadPartial(
  run: Pick<FsUploadStopped, "addr" | "destDir" | "steps"> & { jobId?: string },
  deps: Pick<FsUploadDeps, "deleteRemote" | "listDir" | "jobStatus" | "jobCancel" | "sleep">,
): Promise<void> {
  const step = run.steps?.[0];
  const { deleteRemote, listDir } = deps;
  if (!step || !deleteRemote || !listDir) return;
  try {
    // What the app closed on may still be running: a part deleted under a live job comes back.
    if (run.jobId) {
      const state = await deps.jobStatus(run.jobId).catch(() => null);
      if (state?.status === "running") {
        await deps.jobCancel(run.jobId).catch(() => {});
        for (let i = 0; i < 60; i++) {
          const now = await deps.jobStatus(run.jobId).catch(() => null);
          if (now?.status !== "running") break;
          await deps.sleep(500);
        }
      }
    }
    if (step.kind === "one") {
      await removeUploadPart(run.addr, joinRemote(run.destDir, step.name), {
        listDir,
        remove: deleteRemote,
      });
      return;
    }
    // A list merges into the folder: each file's part sits beside its final place.
    const there = new Set((await listDir(run.addr, run.destDir)).map((e) => e.name));
    for (const f of step.files) {
      const part = `${f.name}.ava-part`;
      if (there.has(part)) await deleteRemote(run.addr, joinRemote(run.destDir, part)).catch(() => {});
    }
  } catch {
    /* the console is away, or nothing was there */
  }
}

/** The person gives a stopped run up: the card goes, and so does the partial it would have
 *  resumed, which otherwise holds its space for days. */
export function giveUpFsUploadStopped(
  host: string,
  deps: Pick<FsUploadDeps, "deleteRemote" | "listDir" | "jobStatus" | "jobCancel" | "sleep">,
): void {
  const st = fsUploadForHost(useFsUploadStore.getState(), host).stopped;
  dismissFsUploadStopped(host);
  if (st) void discardFsUploadPartial(st, deps);
}

/** The person has seen which files could not be read. */
export function clearFsUploadUnreadable(host: string): void {
  useFsUploadStore.getState().patch(host, { unreadable: [] });
}

/** Stops the console's run: the engine ends the transfer job, and the batch stops before the
 *  next item. What landed stays, and so does the half-written file or folder, so Resume can
 *  continue it. Giving the run up (`giveUpFsUploadStopped`) deletes that partial. */
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
