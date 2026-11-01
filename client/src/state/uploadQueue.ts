import { consoleAddr } from "../lib/addr";
import { create } from "zustand";
import { audit } from "./auditLog";

import { pkgMkdirChain } from "../lib/pkgStorage";

import {
  fsMount,
  fsDelete,
  fsMkdir,
  appRegister,
  generateTxIdHex,
  jobStatus,
  jobCancel,
  smpManualInstall,
  smpStatus,
  startTransferDir,
  startTransferDirReconcile,
  startTransferFile,
  startTransferZip,
  startTransfer7z,
  startTransferRar,
  uploadQueueLoad,
  UPLOAD_QUEUE_KEY,
  uploadQueueSave,
  UploadJobError,
  powerStandby,
  pkgInstallStop,
  resumeTxidForget,
  type JobSnapshot,
  type ReconcileMode,
} from "../api/ps5";
import { fileResumeTxId } from "../lib/fileResumeTx";
import { restAfterUploadEnabled } from "./restAfterUpload";
import {
  moveItemDownWithinGroup,
  moveItemToWithinGroup,
  moveItemUpWithinGroup,
  patchItem,
  removeItem,
  resetRunningToPending,
  shouldContinueAfterFailure,
} from "../lib/queueOps";
import {
  averageRate,
  computeRate,
  pushRateSample,
  type RateSample,
} from "../lib/rollingRate";
import { archiveFormat, type SourceKind } from "./upload";
import { rarPasswordProblem } from "../lib/rarPassword";
import { jobLiveFromSnapshot, type JobLive } from "../lib/jobLive";
import {
  runPkgInstall,
  installSampleFeed,
  pkgLibraryStore,
  recordPkgInstalled,
  waitForConsoleReady,
} from "./pkgLibrary";
import type { UploadStrategy } from "./transfer";
import { useUploadSettingsStore } from "./uploadSettings";
import { useRecentHostMetricsStore } from "./recentHostMetrics";
import { pushNotification } from "./notifications";
import { sleep } from "../lib/wakeableSleep";
import { guardSet } from "../lib/renderLoopGuard";
import { wakeForUpload } from "../lib/wakeForUpload";
import { labelEngineJob } from "../lib/bgWork";
import { withConsolePrefix } from "./roster";
import { hostOf, mgmtAddr } from "../lib/addr";
import { log } from "./logs";
import { isRemotePath } from "../lib/remotePath";
import { partialUploadPath } from "../lib/partialUpload";
import { materializeRemote, releaseCopy } from "../lib/materialize";

/** Local copies of server archives, by queue item: made when the item's turn comes (not at
 *  pick time — ten archives would otherwise be ten downloads before any upload), reused when
 *  the item retries, released when it finishes or is removed. */
const archiveCopies = new Map<string, string>();

async function archiveSourceFor(item: { id: string; sourcePath: string }): Promise<string> {
  if (!isRemotePath(item.sourcePath)) return item.sourcePath;
  const have = archiveCopies.get(item.id);
  if (have) return have;
  const local = await materializeRemote(item.sourcePath);
  archiveCopies.set(item.id, local);
  return local;
}

function releaseArchiveCopy(id: string) {
  const local = archiveCopies.get(id);
  if (!local) return;
  archiveCopies.delete(id);
  void releaseCopy(local);
}
import {
  getInstallExecutor,
  resolveInstallJob,
  watchInstallJob,
  registerInstallEnqueuer,
  registerPkgQueueApi,
  type EnqueueInstallInput,
  type EnqueuedInstall,
  type InstallProgress,
  type InstallRequest,
  type InstallResult,
} from "./consoleQueueBridge";
import { takeMeshLock } from "./mesh";
import { useToastStore } from "./toasts";
import { trStatic } from "../lib/trStatic";
import { fpkg } from "../api/fpkg";
import { ensurePayloadCurrent } from "../lib/ensurePayloadCurrent";
import {
  autoRecoverBackoffMs,
  isAutoRecoverable,
  MAX_AUTO_RECOVER_ATTEMPTS,
  PostUploadStepError,
  shouldAutoRecover,
  shouldParkForWake,
} from "../lib/uploadRecovery";
import { useConnectionStore } from "./connection";
import { startPs5ToPs5 } from "../api/ava1";
import { isTauriEnv } from "../lib/tauriEnv";
import { createQueueLeader } from "../lib/queueLeader";

/** The engine job id currently uploading on each console (bare host key).
 *  runOne records it so stopHost/stop can ask the engine to TRULY cancel the
 *  in-flight transfer (not just halt the queue worker). Module-level — it's
 *  transient run state, not persisted queue data. A stale id (job already
 *  finished) is a harmless no-op server-side. */
const runningJobByHost = new Map<string, string>();

/** Engine jobs this tab asked to cancel. A cancel lands at the next shard boundary, so the job
 *  can still report `running` for a moment: a restart of its item waits for it to end instead of
 *  adopting a job that is about to die (see `liveJobFor`). */
const cancelledJobs = new Set<string>();
function cancelEngineJob(jobId: string): Promise<void> {
  cancelledJobs.add(jobId);
  return jobCancel(jobId);
}

/** How long a restart waits for a job this tab cancelled to finish ending. */
const CANCEL_SETTLE_TRIES = 30;
const CANCEL_SETTLE_MS = 500;

/** Items whose engine job is being looked up after a reload (see `reattachRunning`). The runner
 *  skips them, so a start from elsewhere cannot launch a fresh upload while the engine's job for
 *  the same item is still running. */
const reattaching = new Set<string>();

/** One queue runner per browser profile; see lib/queueLeader. */
const queueLeader = createQueueLeader();

/**
 * Pause between queued jobs so the PS5 payload can drain the detached
 * mgmt-port threads + TIME_WAIT sockets that a folder reconcile leaves
 * behind. Without it, job N+1's reconcile fires its own connection burst
 * while the payload is still cleaning up after job N — the cumulative
 * pressure is what tipped the payload over and killed the rest of the queue.
 */
const INTER_JOB_SETTLE_MS = 1500;

/**
 * Sequential upload queue. Lives in its own Zustand store separate
 * from `useTransferStore` so a queued run doesn't fight with the
 * single-shot manual upload state on the same screen — the user can
 * keep eyeing the live transfer panel while the queue runs the next
 * item in the background.
 *
 * Persisted to a single Tauri JSON document (`upload_queue.json` in
 * app-data). Saves are debounced — a 300 ms idle window after the
 * last mutation collapses bursty reorders into one disk write.
 *
 * The runner is generation-counted: every `start()` bumps `runId`,
 * and the loop checks the live runId between every async await so
 * `stop()` (which just bumps runId) tears the loop down at the next
 * await boundary. Without that, a clicking-stop-mid-poll would still
 * mark the next pending item as running before noticing the cancel.
 */

export type QueueItemStatus = "pending" | "running" | "done" | "failed";

/** What a queue item does: an upload of some source kind, an install, or a copy from another
 *  console in the roster (`ps5`: `sourcePath` is on `fromConsole`). */
export type QueueSourceKind = SourceKind | "install" | "ps5";

/** One queued upload. The shape is whatever the Upload screen
 *  captures at "Add to queue" time — source path, destination,
 *  strategy, exclude rules — plus runtime status that the runner
 *  updates as it processes the item. */
export interface QueueItem {
  id: string;
  sourceKind: QueueSourceKind;
  sourcePath: string;
  /** Display-only basename so the list row doesn't re-derive it on
   *  every render. */
  displayName: string;
  /** Resolved final on-PS5 path (volume + subpath + basename). The
   *  user picked these on the Upload screen at queue-add time; the
   *  runner sends the file to this exact path. */
  resolvedDest: string;
  /** Console address (a bare host such as `192.168.1.2`). */
  addr: string;
  /** `ps5` items only: the console the file or folder is copied from (#433). */
  fromConsole?: string | null;
  /** `ps5` items only (#444): never through this computer (`direct`), never direct (`relay`). */
  route?: "auto" | "direct" | "relay";
  /** `ps5` items only (#444): what the sending console dials, when not the receiver's address. */
  directAddr?: string | null;
  /** `ps5` items only (#444): a send another device started, adopted under its id on the next
   *  start: `take` ends it there and sends from here, `watch` follows it while it runs direct.
   *  Cleared once the engine has it, so a later retry is an ordinary resume. */
  adopt?: "take" | "watch" | null;
  strategy: UploadStrategy;
  reconcileMode: ReconcileMode;
  excludes: string[];
  /** Archive-only (.rar): password for an encrypted archive, captured at add
   *  time and held IN MEMORY for the live run. It is deliberately REDACTED from
   *  the persisted queue document (scheduleSave) so a secret never lands on
   *  disk in cleartext. Consequence: a queued encrypted .rar that survives an
   *  app restart loses its password and re-prompts (the transfer surfaces
   *  `rar_password_required`); the user re-adds it from the Upload screen.
   *  Null/absent for unencrypted archives and non-rar items. */
  rarPassword?: string | null;
  /** Image-only: mount the uploaded image after the transfer commits. */
  mountAfterUpload: boolean;
  /** Image-only: when mounting, mount read-only (default true — RO
   *  prevents the PS5 from silently writing save-data into the image
   *  and corrupting it on next mount). */
  mountReadOnly: boolean;
  /** Game-folder-only: after the upload commits, register the game with
   *  the PS5 OS so it lands on the home screen without a Library visit.
   *  Best-effort — a register failure is a warning, never an upload
   *  failure. Old persisted items (pre-v3) lack the field; undefined
   *  reads as false. */
  registerAfterUpload: boolean;
  /** Pkg-only: the parsed ContentID, passed to the installer (the staged file
   *  is already on the PS5). Empty string for a headerless pkg (install still
   *  accepts it). Null/absent for non-pkg items. */
  contentId?: string | null;
  /** Pkg-only: PARAM.SFO category (`gd` base / `gp` update / `ac` DLC), parsed
   *  at add time. Drives install ORDER — a base game must install before its
   *  update before its DLC (an add-on installed before its base wastes the
   *  upload + space and can't apply). Null/absent for non-pkg or headerless
   *  items; the runner then falls back to the staged dest path. */
  category?: string | null;
  /** Install-only: what to install and from where. A link is never persisted
   *  (see scheduleSave), so a restored item without it is dropped on hydrate. */
  install?: InstallRequest;
  /** Install-only: a stream install the PS5 never fetched from — offer
   *  "Retry via upload", which copies the file to the PS5 first. */
  fallbackToUpload?: boolean;
  /** Pkg-only: run the installer once the .pkg upload commits (default on —
   *  staging a pkg exists to install it). Mirrors installSettings, captured at
   *  add time so toggling the default mid-queue doesn't disturb queued rows. */
  installAfterUpload?: boolean;
  /** Pkg-only: delete the staged .pkg from the PS5 after a successful install
   *  (default on — it's just a staging copy). */
  deletePkgAfterInstall?: boolean;
  /** Pkg-only runtime state: the install phase after the upload commits.
   *  null until the finisher runs (or for non-pkg items). */
  installPhase?: "installing" | "done" | "warn" | "unverified" | "error" | null;
  /** Pkg-only: live install progress (0-99) while installPhase is "installing". */
  installPct?: number | null;
  /** Install-only: the bytes, phase and rate behind `installPct`. Kept on a
   *  finished row so it can say how big the install was. */
  installProgress?: InstallProgress | null;
  /** Install-only: what the install is doing that the numbers can't say
   *  (waiting for the console, verifying…) or, once done, the installer's
   *  closing message. Null when there is nothing to add. */
  installNote?: string | null;
  /** Install-only: the engine job running this install, so a reload can ask the engine how it
   *  went instead of calling it interrupted. */
  installJobId?: string | null;
  /** Install-only: the user asked to stop it; the row goes once the engine ends the job. */
  stopping?: boolean;
  /** A game image Convert built on this computer: delete it here once it is on the PS5. The
   *  engine refuses to delete any file it did not build itself. */
  deleteSourceAfterUpload?: boolean;
  /** Pkg-only: the installed title (or content id) the finisher resolved,
   *  shown on the done row. Null otherwise. */
  installedTitle?: string | null;
  /** Stable tx_id for this queue item, minted at add-time and
   *  persisted alongside the item. Used so a queue interrupted by
   *  app restart can resume against the payload's existing journal
   *  entry instead of orphaning the in-flight tx and starting fresh.
   *  Every kind of upload (folder, file, archive, PS5-to-PS5) passes
   *  this id, so Stop and Start continue from the console's journal. */
  txIdHex: string;
  status: QueueItemStatus;
  /** It was running when it was stopped or the app closed: it is waiting with its console-side
   *  progress kept, and starting it continues from there. Cleared when it runs again. */
  interrupted?: boolean;
  /** Paused: the row waits (status stays "pending") and the runner skips it until it is
   *  resumed. `user`: the person pressed Pause, and only they resume it. `sendNow`: a Send now
   *  paused it, and it resumes by itself once the sent-now rows on its console have finished.
   *  Its tx id is kept, so a resume continues from what the console already has. */
  paused?: "user" | "sendNow" | null;
  /** Send now: the row runs ahead of everything else waiting on its console. Rows sent now
   *  run in the order they were clicked (this value). Cleared once the row finishes. */
  sendNowAt?: number | null;
  /** Parked because the console went into rest mode: the row waits (pending) for the PS5 to
   *  wake and resumes by itself on the helper-up edge after it (resumeFailedRecoverable). Not
   *  a failure and not a spent recovery attempt. Cleared when it runs again. */
  waitingForWake?: boolean;
  /** While running: how long nothing has moved, once that is STALL_AFTER_MS or more (the row then
   *  offers to restart the transfer, which resumes). Null otherwise. */
  stalledForMs?: number | null;
  /** Live progress while running, final count when done, 0 otherwise. */
  bytesSent: number;
  /** Total bytes the engine pre-stat'd for this source. 0 until first
   *  Running tick lands. */
  totalBytes: number;
  /** Bytes the source is expected to take on the PS5, captured at add time
   *  from the Upload screen's inspection (pkg header, folder walk or archive
   *  central directory — for archives the UNCOMPRESSED total). 0 when the
   *  inspection had no size (plain files, multi-part sets, queued installs);
   *  the queue size chip (lib/queueSize) then falls back to `totalBytes`,
   *  which only lands once the item starts running. */
  estimatedBytes?: number;
  /** Smoothed bytes/sec while running (trailing 2 s window via
   *  `lib/rollingRate`); set to the wall-clock average bytes/sec on
   *  done; 0 when pending or failed. Persisted with the queue so the
   *  done-row average survives an app restart and stays comparable
   *  across runs. */
  bytesPerSec: number;
  /** Durable-file counters forwarded from JobSnapshot. Surface a
   *  "Finalized N of M files" pill on the queue row while the console
   *  makes the last files durable. Both 0 until the console reports its
   *  first durable file. */
  filesFinalized: number;
  filesFinalizingTotal: number;
  /** Transient live notes (skipping phase, bottleneck, "finishing on the
   *  console") forwarded from the job snapshot while running; only set when
   *  the engine sent them. Not persisted meaningfully: stale on reload. */
  live?: JobLive;
  /** The engine job that ran this item (set when it starts, kept after it ends): the key for
   *  the finished job's "Why was this slow?" summary. Stale after a restart is harmless: the
   *  summary is simply not found. */
  jobId?: string;
  /** Set on a reopened web UI for an item whose engine job was still running (or had just
   *  finished) when the tab closed: the runner adopts this job instead of starting a new
   *  one, then does the item's post-upload steps. Cleared the moment the runner takes it. */
  attachJobId?: string;
  /** Mount path the runner produced when `mountAfterUpload` is true and
   *  the image upload + mount succeeded. Surfaced to the row so users
   *  see where the image landed without flipping to the Volumes tab. */
  mountedAt: string | null;
  /** Display name (or title id) the post-upload register produced when
   *  `registerAfterUpload` ran and succeeded. Null otherwise. */
  registeredAs: string | null;
  /** Non-fatal warnings the post-upload mount surfaced — layout
   *  invalid (no sce_sys/param.json at root), kernel forced RO, etc.
   *  Pre-2.2.52 these warnings only appeared when the user mounted
   *  via the Library tab; the upload-then-mount path silently
   *  swallowed them so users with `mountAfterUpload` got no feedback
   *  about an image that mounted successfully but won't register. */
  mountWarnings: string[];
  /** Transient: true while the runner is between auto-recovery attempts
   *  for this item (waiting out a backoff and re-deploying a crashed
   *  payload before resuming). Only meaningful when `status === "running"`;
   *  not persisted in any meaningful way (a `running`/recovering item
   *  resets to `pending` on hydrate). The row renders a "recovering" hint
   *  instead of the live speed while this is set. */
  recovering?: boolean;
  /** Which recovery attempt is in progress (1-based), for the
   *  "recovering (2/3)…" readout. 0/undefined when not recovering. */
  recoverAttempt?: number;
  error: string | null;
  /** Payload-side error category, when the failure originated from a
   *  PS5 protocol error frame (e.g. `direct_writer_io_error`,
   *  `fs_write_failed_errno_28`). Used by the UI to render a
   *  humanized hint via `humanizeJobErrorReason`. null for failures
   *  that didn't come from the payload (local I/O, connection refuse). */
  errorReason: string | null;
  /** Free-form human-readable detail string from the payload's error
   *  frame `"detail"` field. Shown as the secondary line under the
   *  humanized hint when present. */
  errorDetail: string | null;
  addedAt: number;
  startedAt: number | null;
  completedAt: number | null;
}

/** Subset of `QueueItem` that the caller supplies; the store fills in
 *  id + addedAt + status + counters. */
export type AddQueueItem = Pick<
  QueueItem,
  | "sourceKind"
  | "sourcePath"
  | "displayName"
  | "resolvedDest"
  | "addr"
  | "fromConsole"
  | "route"
  | "directAddr"
  | "strategy"
  | "reconcileMode"
  | "excludes"
  | "rarPassword"
  | "mountAfterUpload"
  | "mountReadOnly"
  | "registerAfterUpload"
  | "contentId"
  | "category"
  | "estimatedBytes"
  | "installAfterUpload"
  | "deletePkgAfterInstall"
  | "install"
  | "deleteSourceAfterUpload"
  | "adopt"
> & {
  /** Keep this job id instead of minting one: a send another device started (#444). */
  txIdHex?: string;
};

interface QueueState {
  items: QueueItem[];
  /** When false, runner stops at the first failure. When true, it
   *  marks the failed item and moves to the next pending. */
  continueOnFailure: boolean;
  /** True while ANY console's runner loop is active. Derived from
   *  `runningHosts` — kept as a flat boolean for the many consumers that
   *  only care "is the queue doing anything" (AppShell keep-awake, the
   *  Activity badge, the one-shot/queue mutual-exclusion gate). */
  running: boolean;
  /** Per-console run state, keyed by bare host (port-stripped). Each
   *  console drains independently and in parallel, so the grouped queue
   *  UI can show — and Start/Stop — each console on its own. A host is
   *  present-and-true exactly while its drain loop is live. */
  runningHosts: Record<string, boolean>;
  /** Consoles whose live run takes only some rows (one row's own Start, a retry, a
   *  restart): rows waiting outside that run stay waiting. The queue panel offers
   *  "Start queue" next to Stop for these, which turns the run into a full one. */
  limitedHosts: Record<string, boolean>;
  /** True after the first hydrate() completes. Lets the UI distinguish
   *  "no items yet" from "still loading from disk." */
  loaded: boolean;
  /** Last queue load/save failure. Non-null means restart durability is not
   * currently guaranteed and must be shown in the queue UI. */
  persistenceError: string | null;
  /** False in a second web-UI tab: another tab of this browser runs the queue, and this one only
   *  shows it (every action is a no-op). Always true on the desktop. */
  isLeader: boolean;

  hydrate: () => Promise<void>;
  /** Adds the item and returns its id. */
  add: (item: AddQueueItem) => string;
  /** Queue an install on its console; the console's queue then starts as `startAdded`
   *  says. `done` resolves
   *  when the item finishes or is removed; a duplicate resolves at once with
   *  ok:false. */
  enqueueInstall: (input: EnqueueInstallInput) => EnqueuedInstall;
  /** Re-run one failed install item; returns a fresh waiter, or null when the
   *  row is missing, not an install, or not failed. */
  retryInstall: (id: string) => EnqueuedInstall | null;
  /** Replace a failed stream install of a file on this computer with an
   *  upload & install of the same file. */
  retryInstallViaUpload: (id: string) => Promise<{ ok: boolean; message?: string }>;
  remove: (id: string) => void;
  /** Cancel a single item and drop it from the queue. If it's the one
   *  actively uploading, its in-flight engine job is aborted (at the next
   *  shard boundary, partial tx left resumable) and that console's remaining
   *  pending work keeps draining — unlike Stop, which halts the whole console.
   *  A pending/finished item is just removed. */
  cancelItem: (id: string) => void;
  /** A waiting row with no size learned one from reading its source (the queue row's
   *  inspection): it becomes the row's `estimatedBytes`. A row that already has a size, or is
   *  no longer waiting, is left alone. */
  noteSourceSize: (id: string, bytes: number) => void;
  moveUp: (id: string) => void;
  moveDown: (id: string) => void;
  /** Drag and drop: put `id` where `targetId` is. Only between waiting rows of the same
   *  console and install tier (as moveUp/moveDown); anything else is ignored. */
  moveTo: (id: string, targetId: string) => void;
  /** Drop every item (or, with `host`, that console's), finished ones included. A running
   *  upload is stopped; a running install stays. */
  clear: (host?: string) => void;
  /** Put every failed row (or, with `host`, that console's) back and run it. */
  retryFailed: (host?: string) => void;
  /** Stops a running (stalled) upload and starts it again; it continues from what the console
   *  already holds. Not for installs, which cannot be stopped. */
  restartItem: (id: string) => void;
  /** Retry one failed row. Returns false when the row is missing/not failed. */
  retryItem: (id: string) => boolean;
  /** Pauses the running upload: its transfer stops with what the console has kept, the row
   *  waits as Paused, and the console's queue goes on with the next row. Not for installs. */
  pauseItem: (id: string) => void;
  /** Puts a paused row back in line at its place. With nothing running on its console it
   *  starts at once (continuing from what the console has); otherwise it waits its turn. */
  resumeItem: (id: string) => void;
  /** Runs this waiting row next: the upload running on its console is paused, and resumes by
   *  itself once this row (and any other row sent now after it) has finished. */
  sendNow: (id: string) => void;
  /** Retry one archive row that failed for a missing or wrong password, with the password
   *  the person just typed. The password lives on the in-memory item only (the save redacts
   *  it) and is never logged. False when the password is empty or the row is not a failed one. */
  retryWithPassword: (id: string, password: string) => boolean;
  /** Deletes the partial file a single-file upload left when the drive filled up, and gives
   *  the row a new job id so its next attempt starts fresh. Throws with the console's reason. */
  discardPartial: (id: string) => Promise<void>;
  /** Re-drive one console's uploads that FAILED on a recoverable
   *  (connection-class) error, then restart that console's drain loop.
   *  This is the "slept past the in-loop recovery budget" case: a standby
   *  outlasts the 3-attempt window, the row goes terminally failed, and the
   *  helper is later restored on wake — but nothing re-runs the upload. Called
   *  on the wake-recovery edge, it resumes those rows with no manual Retry.
   *  Fatal failures (no-space, bad path) and post-commit failures are left
   *  alone (same `isAutoRecoverable` policy the in-loop recovery uses), and
   *  the whole thing no-ops unless the `autoResume` setting is on. Returns how
   *  many rows it re-drove. */
  resumeFailedRecoverable: (host: string) => Promise<number>;
  setContinueOnFailure: (b: boolean) => void;
  /** Rows were just added by an action the person expects to go (Install, Upload now,
   *  Send, Convert & install). One rule for all of them (#536): with the
   *  "Start the queue when you add something" setting on (the default), the console's
   *  WHOLE queue runs, in queue order — not just the new rows — and a run already going
   *  (even one limited to a row's own Start) takes everything. Off, nothing starts: the
   *  rows wait for Start like everything else waiting (a full run already going still
   *  takes them in order). `ids` are the rows just added. */
  startAdded: (host: string, ids: string[]) => void;
  /** Start every console that has pending work, each in its own parallel
   *  drain loop (== "Start all"). */
  start: () => Promise<void>;
  /** Stop every running console (== "Stop all"). */
  stop: () => void;
  /** Start (or no-op if already running) just one console's drain loop.
   *  `installsOnly` / `onlyIds` limit the run to those rows (a row's own Start, a retry);
   *  everything else waiting stays waiting. Start with neither runs everything. Newly added
   *  rows go through `startAdded`, never a limited run (#536).
   *  `unattended`: nobody clicked for this run (wake resume, re-attaching after a restart), so it
   *  never sends a helper the user stopped (#471). */
  startHost: (
    host: string,
    opts?: { installsOnly?: boolean; onlyIds?: string[]; unattended?: boolean },
  ) => Promise<void>;
  /** Stop just one console; siblings keep running. */
  stopHost: (host: string) => void;
}

interface QueueDocument {
  items: QueueItem[];
  continueOnFailure: boolean;
}

const POLL_INTERVAL_MS = 500;
/** Status reads that may fail in a row before the transfer counts as lost (the bound
 *  transfer.ts's single-shot poller uses). The transfer runs on the engine by itself: a
 *  failed read is not a failed transfer. */
export const MAX_POLL_FAILURES = 6;
/** Past MAX_POLL_FAILURES the queue keeps asking at this slower pace (transfer.ts's
 *  lost-contact interval) instead of giving the transfer up at once. */
const LOST_POLL_INTERVAL_MS = 5000;
/** How long reads may keep failing before the transfer counts as lost and goes to recovery. */
export const LOST_CONTACT_GIVE_UP_MS = 120_000;
/** Errors thrown because status reads kept failing (not because the transfer failed): the
 *  recovery for these re-sends the helper only when it no longer answers, since the
 *  transfer may still be running on it. */
const lostContactErrors = new WeakSet<object>();
/** The engine answered that it has no such job (it restarted): the transfer is gone. */
const isJobUnknownError = (e: unknown) =>
  /job not found/i.test(e instanceof Error ? e.message : String(e));
/** No new bytes for this long while sending (not while the console finalizes): say so on the row. */
const STALL_AFTER_MS = 90_000;
const SAVE_DEBOUNCE_MS = 300;

function newId(): string {
  // Same compatibility path as the transfer tx id. In an insecure LAN
  // browser context randomUUID is absent, but queueing must still work.
  return generateTxIdHex();
}

/** Distinct console hosts (bare IP, port-stripped) among the pending items,
 *  in first-seen order. The per-console parallel runner spawns one drain
 *  loop per host. Pure — exported for tests. */
export function distinctPendingHosts(
  items: QueueItem[],
  opts: { includeWaitingForWake?: boolean } = {},
): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const it of items) {
    if (it.status !== "pending" || it.paused) continue;
    // A row parked for a sleeping console waits for the wake, not for the next Start-all pass.
    if (it.waitingForWake && opts.includeWaitingForWake === false) continue;
    const h = hostOf(it.addr);
    if (!seen.has(h)) {
      seen.add(h);
      out.push(h);
    }
  }
  return out;
}

/** Install-order priority for a queued item. A base game must install before
 *  its update, which must install before its DLC — an add-on uploaded+installed
 *  before its base wastes the transfer + storage and can't apply (and the
 *  reported bug: a Far Cry UPDATE queued ahead of its base). Lower runs sooner.
 *
 *  Uses the parsed PARAM.SFO category when present (`gd` base → 0, `gp` update →
 *  1, `ac` DLC → 2); falls back to the staged dest path (which
 *  `stagingSubdirForCategory` routes to `/updates/` or `/dlc/`) for older
 *  persisted items that predate the `category` field. Non-pkg items and base
 *  games share priority 0 and keep their add-order. Exported for tests. */
export function installOrderPriority(it: QueueItem): number {
  if (it.sourceKind !== "pkg" && it.sourceKind !== "install") return 0;
  const cat = it.category;
  if (cat === "gp") return 1;
  if (cat === "ac") return 2;
  if (cat === "gd") return 0;
  // No category (headerless or pre-field item) → infer from the dest path.
  if (/\/updates\/[^/]*$/.test(it.resolvedDest)) return 1;
  if (/\/dlc\/[^/]*$/.test(it.resolvedDest)) return 2;
  return 0;
}

/** What a row may swap places with when moved: waiting rows on the same
 *  console in the same install tier. Anything else is its own group, so it
 *  never moves. */
export function moveGroupOf(it: QueueItem): string {
  return it.status === "pending"
    ? `${hostOf(it.addr)}|${installOrderPriority(it)}`
    : `${it.id}|fixed`;
}

/** A console's library installs that are waiting or running, by staged path:
 *  Install Package shows these on the library rows (and blocks Delete). */
/** Failed stream installs of `source` — the browser build deletes that staged
 *  copy after a failure, so these rows could never be retried. */
export function failedStreamInstallIds(items: QueueItem[], source: string): string[] {
  return items
    .filter(
      (it) =>
        it.status === "failed" &&
        it.sourceKind === "install" &&
        it.install?.via === "stream" &&
        it.install.source === source,
    )
    .map((it) => it.id);
}

export function libraryInstallStates(
  items: QueueItem[],
  host: string,
): Map<string, "queued" | "installing"> {
  const h = hostOf(host);
  const out = new Map<string, "queued" | "installing">();
  for (const it of items) {
    if (it.sourceKind !== "install" || it.install?.via !== "library") continue;
    if (hostOf(it.addr) !== h) continue;
    if (it.status === "pending") out.set(it.install.path, "queued");
    else if (it.status === "running") out.set(it.install.path, "installing");
  }
  return out;
}

/** Whether a queue item is an upload the activity bar and task list should
 *  show as one. An install item's executor registers its own task, so
 *  mirroring it too would show every install twice, once as an "Upload". */
export function isUploadItem(it: QueueItem): boolean {
  return it.sourceKind !== "install";
}

/** The same install already waiting or running on the same console. Matched on
 *  the source (file, on-console path, link), not the content id: two files with
 *  one content id are legitimate (same-version variants, a deliberate reinstall). */
export function sameInstall(it: QueueItem, input: EnqueueInstallInput): boolean {
  if (it.sourceKind !== "install" || !it.install) return false;
  if (it.status !== "pending" && it.status !== "running") return false;
  if (hostOf(it.addr) !== hostOf(input.host)) return false;
  return installKey(it.install) === installKey(input.request);
}

/** A caller's size, or 0 (unknown) for anything that is not a positive whole byte count. */
function knownSize(n: number | null | undefined): number {
  return typeof n === "number" && Number.isFinite(n) && n > 0 ? Math.floor(n) : 0;
}

function installKey(r: InstallRequest): string {
  switch (r.via) {
    case "library":
    case "console-path":
      return `ps5:${r.path}`;
    case "external":
      return `ps5:${r.pkg.path}`;
    case "stream":
      return `pc:${r.source}`;
    case "link":
      return `url:${r.url}`;
  }
}

/** A finished install whose verdict was "not installed". Carries the result so
 *  the drain loop can hand it to the waiter unchanged. */
class InstallItemError extends Error {
  constructor(readonly result: InstallResult) {
    super(result.message || "The install didn't complete.");
  }
}

/** The next pending item to run for `host` (port-stripped match), or null.
 *  Picks the lowest install-order priority (base → update → DLC), breaking ties
 *  by add-order (the first such pending item wins, so a user's manual reorder
 *  within a category is preserved). A row sent now beats all of that; a paused row is never
 *  picked. Pure — exported for tests. */
export function nextPendingForHost(
  items: QueueItem[],
  host: string,
): QueueItem | null {
  let best: QueueItem | null = null;
  let bestPrio = Number.POSITIVE_INFINITY;
  // A row sent now goes first (the earliest click first); a paused row never runs.
  let sent: QueueItem | null = null;
  for (const it of items) {
    if (it.status !== "pending" || it.paused || hostOf(it.addr) !== host || !it.sendNowAt) continue;
    if (!sent || it.sendNowAt < (sent.sendNowAt ?? 0)) sent = it;
  }
  if (sent) return sent;
  for (const it of items) {
    if (it.status !== "pending" || it.paused || hostOf(it.addr) !== host) continue;
    const prio = installOrderPriority(it);
    // Strict `<` keeps the FIRST item at a given priority (add-order stable).
    if (prio < bestPrio) {
      bestPrio = prio;
      best = it;
      if (prio === 0) break; // 0 is the minimum — first base/non-pkg wins
    }
  }
  return best;
}

/** "Delete the source after upload": the upload already succeeded, so a
 *  failed delete must not fail the row — but it must not vanish either, or
 *  the user believes the space was freed. */
export async function deleteSourceAfterUpload(
  path: string,
  addr: string,
  del: (p: string) => Promise<unknown> = (p) => fpkg.deletePackage(p),
): Promise<void> {
  try {
    await del(path);
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    pushNotification(
      "warning",
      withConsolePrefix(
        addr,
        trStatic("queue_delete_source_failed", "Uploaded, but the source was not deleted"),
      ),
      { body: `${path} — ${msg}` },
    );
  }
}

/** What Clear tells the user about the work it could not simply drop: a
 *  running upload is stopped (its engine job cancelled), a running install
 *  keeps going. Null when nothing was running. */
export function clearNotice(
  items: QueueItem[],
  kept: QueueItem[],
): { addr: string; title: string; body: string } | null {
  const stopped = items.find((it) => it.status === "running" && !kept.includes(it));
  if (stopped) {
    return {
      addr: stopped.addr,
      title: trStatic("queue_clear_stopped_title", "Queue cleared — the running upload was stopped"),
      body: trStatic(
        "queue_clear_stopped_body",
        "The upload that was sending was stopped. Add it to the queue again to send it.",
      ),
    };
  }
  const install = kept[0];
  if (install) {
    return {
      addr: install.addr,
      title: trStatic("queue_clear_install_kept_title", "Queue cleared — one install is still running"),
      body: trStatic(
        "queue_clear_install_kept_body",
        "An install can't be stopped once the PS5 has it, so it stays in the queue until it ends.",
      ),
    };
  }
  return null;
}

export const useUploadQueueStore = create<QueueState>((rawSet, get) => {
  // A render loop elsewhere in the UI must not fail a transfer (lib/renderLoopGuard). The stack
  // is logged once per run of loops, which is what a bug report needs to find the component.
  let loopLogged = 0;
  const set = guardSet(rawSet, (e) => {
    const now = Date.now();
    if (now - loopLogged < 60_000) return;
    loopLogged = now;
    log.error(
      "queue",
      `render loop during a queue update (kept going): ${e instanceof Error ? (e.stack ?? e.message) : String(e)}`,
    );
  });
  // PER-CONSOLE generation counters. Each console drains in its own loop;
  // every startHost() bumps a monotonic counter and stamps it as that
  // host's live generation. A loop captures its generation and bails
  // between awaits once the host's live generation moves on — so
  // stopHost() (which just re-stamps the host with a fresh value) tears
  // down only that console's loop at the next await boundary, leaving
  // sibling consoles untouched. (Pre-2.25.1 this was a single shared
  // runId, which forced one global Start/Stop for all consoles.)
  let genCounter = 0;
  const hostGen = new Map<string, number>();
  /** Derive the flat `running` flag from the per-host map. */
  const anyRunning = (rh: Record<string, boolean>) =>
    Object.values(rh).some(Boolean);
  let saveTimer: ReturnType<typeof setTimeout> | null = null;

  /** Web UI only: the engine kept running these items' jobs while the tab was closed. Ask it
   *  about each; one that is still running, or finished while the tab was away, goes back to its
   *  item (`attachJobId`) and the console's queue restarts, so the runner adopts the job,
   *  shows its progress and does the item's post-upload steps. A job the engine no longer knows
   *  (it restarted) or one that failed leaves the item pending, to be started again and
   *  resumed from the console's journal as before. Never throws. */
  const reattachRunning = async (
    list: Array<{ id: string; jobId: string; addr: string }>,
  ) => {
    const hosts = new Set<string>();
    for (const { id, jobId, addr } of list) {
      try {
        const snap = await jobStatus(jobId, addr);
        if (snap.status === "running" || snap.status === "done") {
          set((s) => ({ items: patchItem(s.items, id, { attachJobId: jobId }) }));
          hosts.add(hostOf(addr));
        }
      } catch {
        /* the engine does not know the job: the item stays pending */
      } finally {
        // Looked up: the runner may take this item now (to adopt its job, or to start it fresh).
        reattaching.delete(id);
      }
    }
    for (const h of hosts) void get().startHost(h, { unattended: true });
  };

  /** The next pending item on `h` that the runner may take: not one whose engine job is still
   *  being looked up (see `reattaching`). */
  /** Consoles whose current run takes only some rows: one row's own Start, a retry, a
   *  restart. Rows outside it wait. Adding something never makes a run like this any more
   *  (#536 — that ran a new package alone while the queue above it waited); see startAdded.
   *  Pressing Start while it runs turns it into a full run. */
  type RunScope = { installs: boolean; ids: Set<string> };
  /** Per console: what the current run may take, or absent for everything. */
  const runScope = new Map<string, RunScope>();
  /** Mirrors "this console's run is limited" into state for the queue panel. */
  const markLimited = (h: string, on: boolean) => {
    if (!!get().limitedHosts[h] === on) return;
    set((s) => {
      const lh = { ...s.limitedHosts };
      if (on) lh[h] = true;
      else delete lh[h];
      return { limitedHosts: lh };
    });
  };
  /** When each console last showed "Added — press Start" (auto-start off). */
  const manualNoticeAt = new Map<string, number>();
  const inScope = (h: string, it: QueueItem) => {
    const scope = runScope.get(h);
    return !scope || (scope.installs && it.sourceKind === "install") || scope.ids.has(it.id);
  };
  const pickPending = (h: string) =>
    nextPendingForHost(
      get().items.filter((it) => !reattaching.has(it.id) && inScope(h, it)),
      h,
    );

  /** Elects this tab's role once, then keeps it up to date. Resolves to whether this tab is the
   *  runner. A tab that loses the lease stops its loops without touching the engine's jobs (the
   *  new runner re-attaches to them); a tab that wins it loads the saved queue and adopts. */
  let electionStarted = false;
  const ensureLeaderElection = async (): Promise<boolean> => {
    if (!electionStarted) {
      electionStarted = true;
      queueLeader.start((lead) => {
        if (lead === get().isLeader) return;
        if (lead) {
          set({ isLeader: true });
          void get().hydrate();
        } else {
          // Another tab took the lease (this one stalled past the TTL): stop scheduling and
          // show the queue read-only. The jobs keep running; the new runner adopts them.
          for (const h of Object.keys(get().runningHosts)) hostGen.set(h, ++genCounter);
          set({ isLeader: false, runningHosts: {}, limitedHosts: {}, running: false });
          void syncFollower();
        }
      });
      if (typeof window !== "undefined" && typeof window.addEventListener === "function") {
        window.addEventListener("storage", (e: StorageEvent) => {
          if (e.key === UPLOAD_QUEUE_KEY && !get().isLeader) void syncFollower();
        });
      }
    }
    const lead = await queueLeader.claim();
    set({ isLeader: lead });
    return lead;
  };

  /** A second tab's view of the queue: the runner's saved document, as is. Running rows stay
   *  running, so the progress the runner saves shows here too. */
  const syncFollower = async () => {
    try {
      const doc = await uploadQueueLoad<Partial<QueueDocument>>();
      const items = doc.items ?? [];
      const rh: Record<string, boolean> = {};
      for (const it of items) if (it.status === "running") rh[hostOf(it.addr)] = true;
      set({
        items,
        continueOnFailure: doc.continueOnFailure ?? false,
        loaded: true,
        persistenceError: null,
        isLeader: false,
        runningHosts: rh,
        running: anyRunning(rh),
      });
    } catch (e) {
      console.error("[upload-queue] follower sync failed:", e);
      set({ loaded: true, isLeader: false });
    }
  };

  /** The engine job of `item`'s earlier attempt, when it is still running and should be adopted
   *  instead of starting the upload again; null when a fresh start is right. A job this tab
   *  cancelled is waited out (it is about to end) rather than adopted. */
  const liveJobFor = async (
    jobId: string,
    addr: string,
    isLive: () => boolean,
  ): Promise<string | null> => {
    let misses = 0;
    for (let i = 0; i < CANCEL_SETTLE_TRIES; i++) {
      let status: string | null = null;
      let readErr: unknown = null;
      try {
        status = (await jobStatus(jobId, addr)).status;
      } catch (e) {
        readErr = e;
      }
      if (status === null) {
        if (isJobUnknownError(readErr)) return null; // the engine does not know it
        // A read that failed for another reason (engine busy, proxy blip) says nothing
        // about the job: a fresh start now could run beside the live one with the same
        // tx id. Ask again; give up after the same tolerance the poll has.
        if (++misses >= MAX_POLL_FAILURES) throw readErr;
        await sleep(CANCEL_SETTLE_MS);
        if (!isLive()) throw new Error("queue stopped");
        continue;
      }
      if (status !== "running") return null;
      if (!cancelledJobs.has(jobId)) return jobId;
      await sleep(CANCEL_SETTLE_MS);
      if (!isLive()) throw new Error("queue stopped");
    }
    throw new Error("The previous transfer of this item is still stopping. Try again in a moment.");
  };

  /** Schedule a debounced whole-document save. Idempotent — multiple
   *  calls within 300 ms collapse into one fsync. The runner can
   *  legitimately fire a half-dozen patches per second (bytes_sent
   *  updates), and we don't want to round-trip Tauri/disk on each. */
  const scheduleSave = () => {
    // Never save before the saved queue has loaded: it would overwrite it
    // with only what was added since the app started. hydrate() saves the
    // merged list once it has loaded.
    if (!get().loaded) return;
    // A second tab never writes: it would overwrite the runner's queue with its stale view.
    if (!get().isLeader) return;
    if (saveTimer !== null) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      saveTimer = null;
      if (!get().loaded || !get().isLeader) return;
      const { items, continueOnFailure } = get();
      // Redact RAR passwords before persisting — they stay in the live
      // in-memory items (so the current run can extract) but never touch disk.
      const persistItems = items.map((it) => {
        let out = it.rarPassword ? { ...it, rarPassword: null } : it;
        // A link can carry a signed token: never persist it. The item is
        // dropped on the next hydrate.
        if (out.install?.via === "link") {
          out = { ...out, install: undefined, sourcePath: "url:" };
        }
        return out;
      });
      const doc: QueueDocument = { items: persistItems, continueOnFailure };
      void uploadQueueSave(doc)
        .then(() => set({ persistenceError: null }))
        .catch((e) => {
          const message = e instanceof Error ? e.message : String(e);
          console.error("[upload-queue] save failed:", e);
          set({
            persistenceError: `Queue changes could not be saved: ${message}. Keep the app open and free disk space or fix permissions before retrying.`,
          });
        });
    }, SAVE_DEBOUNCE_MS);
  };

  /** Callers awaiting an install item's outcome. In memory only: after a
   *  restart nobody is waiting, and the row itself shows the result. */
  const waiters = new Map<string, ((r: InstallResult) => void)[]>();
  /** A promise for item `id`'s outcome; any number of callers can wait. */
  const wait = (id: string) =>
    new Promise<InstallResult>((resolve) => {
      const list = waiters.get(id) ?? [];
      list.push(resolve);
      waiters.set(id, list);
    });
  /** Per console: the install that is running right now. Sony's install
   *  can't be stopped halfway, so Stop and Clear leave it running; a new
   *  drain loop waits for it before starting anything else on that console
   *  (the PS5 installs one package at a time). */
  const installInFlight = new Map<string, Promise<unknown>>();

  /** An install that was running when the page went away: follow its engine job to the end
   *  (holding the console's next install back meanwhile), or, when the engine no longer knows
   *  it, say it was interrupted. Never re-run: Sony may already have it. */
  const findInstallJob = (r: { id: string; jobId: string; addr: string }) => {
    const h = hostOf(r.addr);
    const tracked = (async () => {
      for (;;) {
        const s = await resolveInstallJob(r.jobId);
        if (s.state === "running") {
          set((st) => ({ items: patchItem(st.items, r.id, { installPct: s.pct }) }));
          await sleep(2000);
          continue;
        }
        const patch: Partial<QueueItem> =
          s.state === "finished" && s.result.ok
            ? {
                status: "done",
                installPhase: s.result.mayNotLaunch ? "warn" : "done",
                installPct: 100,
                error: null,
                installNote: trStatic(
                  "queue_install_finished_away",
                  "Finished while the page was closed.",
                ),
              }
            : {
                status: "failed",
                installPhase: "error",
                error:
                  s.state === "finished"
                    ? (s.result.message ?? null)
                    : trStatic(
                        "queue_install_interrupted",
                        "Interrupted when the app closed. Check the game on the PS5, then retry if it isn't installed.",
                      ),
                installNote: null,
              };
        set((st) => ({
          items: patchItem(st.items, r.id, { ...patch, completedAt: Date.now() }),
        }));
        scheduleSave();
        return;
      }
    })();
    installInFlight.set(h, tracked);
    void tracked.finally(() => {
      if (installInFlight.get(h) === tracked) installInFlight.delete(h);
    });
  };
  const settle = (id: string, r: InstallResult) => {
    const list = waiters.get(id);
    if (list) {
      waiters.delete(id);
      for (const w of list) w(r);
    }
  };

  /** Run an install item through the executor pkgLibrary registered. Throws
   *  InstallItemError when the install finished but didn't install. */
  const runInstallItem = async (item: QueueItem, isLive: () => boolean) => {
    const exec = getInstallExecutor();
    if (!exec || !item.install) {
      throw new Error("This install can't run: its details were not kept.");
    }
    // A PS5 installs one package at a time, whichever device asks (8.0.0): wait while another
    // device's install holds the console. An older helper has no locks and this goes straight on.
    const releaseTurn = await waitForInstallTurn(hostOf(item.addr), item, isLive);
    try {
      return await runInstallItemHeld(item, exec);
    } finally {
      void releaseTurn();
    }
  };

  const waitForInstallTurn = async (
    h: string,
    item: QueueItem,
    isLive: () => boolean,
  ): Promise<() => Promise<void>> => {
    const label = `Installing ${item.displayName}`.slice(0, 110);
    for (;;) {
      const turn = await takeMeshLock(h, "install", label);
      if (turn.ok) {
        set((s) => ({ items: patchItem(s.items, item.id, { installNote: null }) }));
        return turn.release;
      }
      set((s) => ({
        items: patchItem(s.items, item.id, {
          installPhase: "installing",
          installNote: `Waiting for ${turn.byName}: ${turn.label || "installing"} on this console`,
        }),
      }));
      await new Promise((r) => setTimeout(r, 5_000));
      if (!isLive()) throw new Error(`Stopped while waiting for ${turn.byName} to finish installing`);
    }
  };

  const runInstallItemHeld = async (item: QueueItem, exec: NonNullable<ReturnType<typeof getInstallExecutor>>) => {
    if (!item.install) throw new Error("This install can't run: its details were not kept.");
    set((s) => ({
      items: patchItem(s.items, item.id, {
        installPhase: "installing",
        installPct: 0,
        installProgress: null,
        installNote: null,
      }),
    }));
    const h = hostOf(item.addr);
    const unwatch = watchInstallJob(h, (job) => {
      set((s) => ({ items: patchItem(s.items, item.id, { installJobId: job }) }));
      scheduleSave();
    });
    const running = exec(item.install, h, {
      // Progress keeps flowing even after Stop: the install is still going.
      // Fresh numbers retire a status note: "waiting for the PS5" is no
      // longer true once bytes move.
      onProgress: (pct, progress) =>
        set((s) => ({
          items: patchItem(s.items, item.id, {
            installPct: pct,
            installNote: null,
            ...(progress ? { installProgress: progress } : {}),
          }),
        })),
      onStatus: (msg) =>
        set((s) => ({
          items: patchItem(s.items, item.id, { installNote: msg || null }),
        })),
    });
    const tracked = running.catch(() => {});
    installInFlight.set(h, tracked);
    let r: InstallResult;
    try {
      r = await running;
    } finally {
      unwatch();
      if (installInFlight.get(h) === tracked) installInFlight.delete(h);
    }
    audit("pkg_install", item.displayName, { context: h, failed: !r.ok });
    if (!r.ok) throw new InstallItemError(r);
    settle(item.id, r);
    return {
      bytesSent: 0,
      bytesPerSec: 0,
      mountedAt: null,
      mountWarnings: r.mayNotLaunch
        ? ["Installed, but it may not launch on this firmware."]
        : [],
      registeredAs: null,
      installPhase: (r.mayNotLaunch ? "warn" : "done") as QueueItem["installPhase"],
      installedTitle: item.displayName,
      // The installer's own closing words (e.g. "Sent to the PS5 — it
      // downloads on its own") belong on the finished row.
      installNote: r.message ?? null,
    };
  };

  /** Run a single queued item to terminal state. Returns when the
   *  engine job hits done; throws on failure (caller decides whether
   *  to continue or stop). The poll loop re-checks `isLive()` after
   *  every await — `stop()` mid-poll exits cleanly without writing
   *  stale state. */
  const runOne = async (item: QueueItem, isLive: () => boolean): ReturnType<typeof runUploadItem> => {
    if (item.sourceKind === "install") return runInstallItem(item, isLive);
    // Writing into a folder another device writes into too (8.0.0): say so, and go on: two
    // uploads into one folder are allowed, only the user knows whether they collide.
    const h = hostOf(item.addr);
    const write = await takeMeshLock(h, `write:${item.resolvedDest}`, `Uploading ${item.displayName}`.slice(0, 110));
    if (!write.ok) {
      useToastStore.getState().push({
        tone: "warn",
        message: `${write.byName} is also writing here (${write.label || item.resolvedDest}).`,
      });
    }
    try {
      return await runUploadItem(item, isLive);
    } finally {
      if (write.ok) void write.release();
    }
  };

  const runUploadItem = async (
    item: QueueItem,
    isLive: () => boolean,
  ): Promise<{
    bytesSent: number;
    bytesPerSec: number;
    mountedAt: string | null;
    mountWarnings: string[];
    registeredAs: string | null;
    installPhase: QueueItem["installPhase"];
    installedTitle: string | null;
    installNote?: string | null;
    /** The finished transfer's bottleneck note, when the engine reported one. */
    live?: JobLive;
  }> => {
    const isFolder =
      item.sourceKind === "folder" || item.sourceKind === "game-folder";
    const isArchive = item.sourceKind === "archive";

    // A .pkg stages into the package-library dir, then installs in the
    // finisher. Make sure the staging dir exists first — the single-file
    // transfer's open() fails ENOENT on a missing parent. EEXIST-tolerant.
    if (item.sourceKind === "pkg") {
      // mkdir is one level at a time; a package on another drive or in an
      // update/DLC sub-folder needs every level from <drive>/ps5upload down.
      const parent = item.resolvedDest.replace(/\/[^/]*$/, "");
      for (const dir of parent ? pkgMkdirChain(parent) : []) {
        try {
          await fsMkdir(item.addr, dir);
        } catch {
          /* dir already exists (or will fail loudly at open) */
        }
      }
    }

    // A reopened web UI hands the engine's still-running job back to its item (see hydrate):
    // adopt it rather than start the upload again. Read from the store, not the argument,
    // so a recovery pass of this same item starts fresh.
    const attachId = get().items.find((it) => it.id === item.id)?.attachJobId;
    if (attachId) {
      set((s) => ({ items: patchItem(s.items, item.id, { attachJobId: undefined }) }));
    }
    // Never start a fresh upload for an item whose engine job is still running: adopt it.
    // (The reload path above covers the common case; this closes the window where something else
    // starts the item while its job is still being looked up, and a retry of a job that is alive.)
    const earlierJob = attachId
      ? null
      : get().items.find((it) => it.id === item.id)?.jobId;
    const adoptId =
      attachId ?? (earlierJob ? await liveJobFor(earlierJob, item.addr, isLive) : null);
    let jobId: string;
    if (adoptId) {
      jobId = adoptId;
    } else if (item.sourceKind === "ps5") {
      // From another console: the engine copies console to console (directly, or through this
      // computer when they cannot reach each other). The persisted tx id resumes it.
      const adopt = get().items.find((it) => it.id === item.id)?.adopt ?? null;
      jobId = await startPs5ToPs5(
        item.fromConsole ?? "",
        item.sourcePath,
        item.addr,
        item.resolvedDest,
        item.txIdHex,
        { mode: item.route, directAddr: item.directAddr },
        adopt,
      );
      if (adopt) set((s) => ({ items: patchItem(s.items, item.id, { adopt: null }) }));
    } else if (isArchive) {
      // A .zip/.7z is decompressed host-side and streamed in (lands
      // extracted). Carry the persisted tx_id for cross-session shard resume,
      // just like folders; there's no reconcile mode (no local tree to diff).
      // 7z re-decompresses from the start on resume (LZMA2 can't seek) and
      // re-sends only un-acked shards.
      const bandwidthCap = useUploadSettingsStore.getState().bandwidthCapMbps;
      const fmt = archiveFormat(item.sourcePath);
      const archivePath = await archiveSourceFor(item);
      if (fmt === "rar") {
        // .rar → host UnRAR extract; carry the (optional) password captured
        // at add time. Resume re-extracts and re-sends only un-acked shards.
        jobId = await startTransferRar(
          archivePath,
          item.resolvedDest,
          item.addr,
          item.rarPassword ?? null,
          item.txIdHex,
          item.excludes,
          bandwidthCap,
        );
      } else {
        const start = fmt === "7z" ? startTransfer7z : startTransferZip;
        jobId = await start(
          archivePath,
          item.resolvedDest,
          item.addr,
          item.txIdHex,
          item.excludes,
          bandwidthCap,
        );
      }
    } else if (isFolder && item.strategy === "resume" && !isRemotePath(item.sourcePath)) {
      // (A folder on a saved server always takes the plain folder upload below: the resume
      // walk compares against local disk. The transfer itself still resumes by tx id.)
      // Pass the persisted tx_id so a Resume after app restart
      // picks up the payload's existing journal entry instead of
      // minting a fresh tx and re-sending everything.
      const bandwidthCap = useUploadSettingsStore.getState().bandwidthCapMbps;
      jobId = await startTransferDirReconcile(
        item.sourcePath,
        item.resolvedDest,
        item.addr,
        item.reconcileMode,
        item.txIdHex,
        item.excludes,
        bandwidthCap,
      );
    } else if (isFolder) {
      const bandwidthCap = useUploadSettingsStore.getState().bandwidthCapMbps;
      jobId = await startTransferDir(
        item.sourcePath,
        item.resolvedDest,
        item.addr,
        item.txIdHex,
        item.excludes,
        bandwidthCap,
      );
    } else {
      // Single-file uploads now thread the persisted txIdHex through
      // too. The engine sets TX_FLAG_RESUME when a caller-supplied
      // tx_id is present, which is a no-op on the very first attempt
      // (payload doesn't know the id yet, falls through to fresh-
      // allocate) but lets a subsequent attempt for the SAME queue
      // item — typically after wifi-drop retries are exhausted and the
      // user clicks "Retry / Resume" — pick up from the payload's
      // last-acked shard instead of restarting from zero. Same pattern
      // as folder uploads.
      // An earlier attempt at this same file (the Upload screen's Start, or
      // a queue item since removed) left its partial under ITS id; only that
      // id resumes it and has its bytes credited by the space check (#401).
      const txIdHex = await fileResumeTxId(
        hostOf(item.addr),
        item.sourcePath,
        item.resolvedDest,
        item.txIdHex,
      );
      if (txIdHex !== item.txIdHex) {
        set((s) => ({ items: patchItem(s.items, item.id, { txIdHex }) }));
        scheduleSave();
      }
      jobId = await startTransferFile(
        item.sourcePath,
        item.resolvedDest,
        item.addr,
        txIdHex,
        useUploadSettingsStore.getState().bandwidthCapMbps,
      );
    }

    // A Stop / Cancel that lands while the start request is still in flight
    // has no job id to act on, so it can only bump the generation. By the
    // time the engine answers, this loop is already dead — and without the
    // cancel below the transfer would run to completion with nothing able to
    // stop it but killing the app. The window is widest for .rar, whose route
    // plans the whole archive inside the request handler before minting the
    // id (user report, 5.4.7).
    if (!isLive()) {
      void cancelEngineJob(jobId).catch(() => {
        /* engine gone — the transfer dies with it either way */
      });
      throw new Error("queue stopped");
    }
    // Record the live job id for this console so stopHost/stop can ask the
    // engine to truly cancel the transfer (overwritten by the next item;
    // staleness is harmless — cancelling a finished job is a server no-op).
    runningJobByHost.set(hostOf(item.addr), jobId);
    set((s) => ({ items: patchItem(s.items, item.id, { jobId }) }));
    labelEngineJob(jobId, "upload", item.displayName);

    // Trailing-window samples for the live bytes/sec readout. Closure-
    // scoped so a Stop + restart of the same item resets cleanly: the
    // next runOne builds a fresh array.
    const startedAtMs = Date.now();
    const samples: RateSample[] = [{ ts: startedAtMs, bytes: 0 }];
    // When bytes last moved, for the "nothing is moving" note on the row.
    let lastBytes = -1;
    let lastMoveAt = startedAtMs;

    // Consecutive status reads that failed. One failed read (engine busy, the
    // 60 s fetch timeout, a proxy blip) says nothing about the transfer, which
    // the engine runs by itself: it used to throw into the recovery path, which
    // re-sent the helper to the console and killed the live transfer (the
    // single-shot poller in transfer.ts tolerates MAX_POLL_FAILURES misses).
    // Past MAX_POLL_FAILURES it keeps asking every LOST_POLL_INTERVAL_MS for
    // LOST_CONTACT_GIVE_UP_MS before giving the transfer to the recovery path.
    let pollFailures = 0;
    let lostSince: number | null = null;
    while (isLive()) {
      let snap: JobSnapshot | null = null;
      let readErr: unknown = null;
      try {
        snap = await jobStatus(jobId, item.addr);
      } catch (e) {
        readErr = e;
      }
      if (!isLive()) {
        throw new Error("queue stopped");
      }
      if (snap === null) {
        // The engine has no such job (it restarted): the transfer is gone, and
        // the recovery path's resume is the right answer.
        if (isJobUnknownError(readErr)) throw readErr;
        if (++pollFailures >= MAX_POLL_FAILURES) {
          if (lostSince === null) {
            lostSince = Date.now();
            set((s) => ({ items: patchItem(s.items, item.id, { bytesPerSec: 0 }) }));
          }
          if (Date.now() - lostSince >= LOST_CONTACT_GIVE_UP_MS) {
            const lost = readErr instanceof Error ? readErr : new Error(String(readErr));
            lostContactErrors.add(lost);
            throw lost;
          }
        }
        await sleep(pollFailures >= MAX_POLL_FAILURES ? LOST_POLL_INTERVAL_MS : POLL_INTERVAL_MS, {
          poll: true,
        });
        continue;
      }
      pollFailures = 0;
      lostSince = null;
      if (snap.status === "done") {
        if (!isFolder && !isArchive) {
          // Nothing left to resume: the next upload of this file is a new job.
          try {
            void resumeTxidForget(
              hostOf(item.addr),
              item.sourcePath,
              item.resolvedDest,
            ).catch(() => undefined);
          } catch {
            /* no store */
          }
        }
        let mountedAt: string | null = null;
        const mountWarnings: string[] = [];
        // Re-check liveness before initiating the mount. Without this,
        // a Stop click between the engine's done-snapshot arrival and
        // the fsMount call would let the mount happen on the PS5
        // anyway — start() then sees `!isLive()` after the await and
        // skips patching the item, leaving the row stuck at "pending"
        // while a real mount sits on /mnt/ps5upload/. Skip-and-return
        // here keeps the user's mental model consistent: Stop = stop.
        if (isLive() && item.sourceKind === "image" && item.mountAfterUpload) {
          // Mount point lives next to the source file (same logic as
          // transfer.ts): strip the image extension from the resolved
          // destination so /data/homebrew/MyGame.ffpkg mounts at
          // /data/homebrew/MyGame/. Source + mount discoverable by
          // every PS5 manager that scans /data/homebrew/.
          const finalDest = snap.dest ?? item.resolvedDest;
          const mgmt = mgmtAddr(hostOf(item.addr));
          // ShadowMount+ hand-off: when SMP is running it OWNS mount +
          // register, so doing our OWN mount here would race it for
          // /user/app + app.db (the exact conflict SMP's own "duplicate
          // uninstall / blocked PPSA" fixes had to handle). Hand the image
          // off via SMP's watched manual.lst and skip the native mount.
          // Two failures are possible here and they are NOT the same:
          //
          //   1. the status probe fails / SMP isn't running — mounting it
          //      ourselves is the correct outcome, no warning needed;
          //   2. SMP IS running but the manual.lst hand-off fails — falling
          //      back to our own mount then races SMP for /user/app + app.db,
          //      which is the exact conflict the hand-off exists to avoid.
          //
          // The old code caught both in one silent `catch` and self-mounted
          // either way, so case 2 produced a conflicting mount with no hint
          // that anything went wrong.
          let handedToSmp = false;
          let smpRunning: boolean;
          try {
            smpRunning = (await smpStatus(mgmt)).running;
          } catch {
            smpRunning = false; // SMP unreachable → mount it ourselves
          }
          if (smpRunning) {
            try {
              const r = await smpManualInstall(mgmt, finalDest);
              handedToSmp = true;
              mountedAt = r.added
                ? "handed to ShadowMount+"
                : "already in ShadowMount+ list";
            } catch (e) {
              // Still fall back — an unmounted image helps nobody — but say
              // so, because this mount may later fight ShadowMount+.
              mountWarnings.push(
                `ShadowMount+ is running but the hand-off failed (${
                  e instanceof Error ? e.message : String(e)
                }). Mounted it directly instead — if ShadowMount+ also picks ` +
                  `it up you may see a duplicate or a blocked title.`,
              );
            }
          }
          if (!handedToSmp) {
            try {
              const mountPoint = finalDest.replace(
                /\.(exfat|ffpkg|ffpfs)$/i,
                "",
              );
              const mounted = await fsMount(item.addr, finalDest, {
                mountPoint,
                readOnly: item.mountReadOnly,
              });
              mountedAt = mounted.mount_point;
              // Surface non-fatal mount diagnostics — same warnings the
              // Library row's Mount button shows.
              if (mounted.layout_valid === false) {
                mountWarnings.push(
                  "Image is missing sce_sys/param.json at root — Register/Launch will fail. Re-build the image with files at root (no extra folder).",
                );
              }
              if (mounted.kernel_ro && !item.mountReadOnly) {
                mountWarnings.push(
                  "Kernel mounted this read-only despite the RW pick — common for UFS .ffpkg images on some firmwares. Reads work; writes through the mount will fail.",
                );
              }
            } catch (e) {
              // Post-upload step: the transfer already committed, so this is
              // not something re-sending the bytes can fix.
              const wrapped = new PostUploadStepError(
                `upload completed, but mount failed: ${
                  e instanceof Error ? e.message : String(e)
                }`,
              );
              // Preserve the original error so downstream consumers can
              // inspect the underlying mount failure (eslint's
              // preserve-caught-error rule enforces this).
              (wrapped as Error & { cause?: unknown }).cause = e;
              throw wrapped;
            }
          }
        }
        // Register-after-upload (game folders): same one-step journey as
        // the single-shot path in transfer.ts. Best-effort — a register
        // failure lands in mountWarnings (the row's existing warning list)
        // rather than failing an upload whose bytes are already committed.
        let registeredAs: string | null = null;
        if (
          isLive() &&
          item.sourceKind === "game-folder" &&
          item.registerAfterUpload
        ) {
          const finalDest = snap.dest ?? item.resolvedDest;
          try {
            let res;
            try {
              res = await appRegister(item.addr, finalDest);
            } catch {
              // Some firmwares reject the plain register; the Library
              // flow's DRM-type-patch retry usually lands it.
              res = await appRegister(item.addr, finalDest, {
                patchDrmType: true,
              });
            }
            registeredAs = res.title_name?.trim()
              ? res.title_name
              : res.title_id;
          } catch (e) {
            mountWarnings.push(
              `Couldn't add it to the home screen automatically: ${
                e instanceof Error ? e.message : String(e)
              }. You can still do it from the Library.`,
            );
          }
        }
        // Pkg finisher (the queue merge): once the .pkg lands in the staging
        // dir, install it via the shared runPkgInstall helper — the same
        // HW-proven cascade the Install Package screen uses (main-payload
        // InstallByPackage → DPI fallback → restore) — then optionally delete
        // the staged copy. So a queued .pkg uploads → installs → cleans up as
        // ONE unit, alongside every other upload in the same queue.
        let installPhase: QueueItem["installPhase"] = undefined;
        let installedTitle: string | null = null;
        // Record the post-upload decision for EVERY pkg (install or not), so a
        // "it installed/deleted even though I turned that off" report shows the
        // exact flags this item carried — captured from the settings at add
        // time. Without this the decision was invisible in bug bundles.
        if (isLive() && item.sourceKind === "pkg") {
          log.info(
            "install",
            `pkg "${item.displayName}" uploaded — auto-install=${
              item.installAfterUpload !== false
            }, auto-delete=${item.deletePkgAfterInstall !== false}`,
          );
        }
        if (
          isLive() &&
          item.sourceKind === "pkg" &&
          item.installAfterUpload !== false
        ) {
          const finalDest = snap.dest ?? item.resolvedDest;
          // Cross-surface serialization: flip THIS console's pkgLibrary
          // `installing` flag so a manual install / one-shot upload on the same
          // console waits (they check it), and the install (which swaps the
          // payload) can't race them. Cleared in finally.
          const pkgStore = pkgLibraryStore(item.addr);
          pkgStore.setState({ installing: true });
          // Tell the row it is installing now (not still committing the upload).
          set((s) => ({
            items: patchItem(s.items, item.id, { installPhase: "installing", installPct: 0 }),
          }));
          // The row shows the install's bytes, rate and notes, like an
          // install item does.
          const feedRow = installSampleFeed({
            onProgress: (pct, progress) =>
              set((s) => ({
                items: patchItem(s.items, item.id, {
                  installPct: pct,
                  installNote: null,
                  ...(progress ? { installProgress: progress } : {}),
                }),
              })),
            onStatus: (msg) =>
              set((s) => ({
                items: patchItem(s.items, item.id, { installNote: msg || null }),
              })),
          });
          try {
            // delete_staging = the per-item Auto Delete preference (captured
            // from the setting at queue-add time). When off, the engine keeps
            // the uploaded pkg instead of deleting it post-install.
            const r = await runPkgInstall(
              item.addr,
              finalDest,
              item.contentId ?? null,
              // Upload-queue items don't carry the PARAM.SFO category; the
              // engine reads it from the staged pkg to detect a patch and arm
              // the data-loss guard.
              null,
              item.deletePkgAfterInstall !== false,
              // Surface the live install % on this console's pkg screen while a
              // large queued title installs in the background.
              (sample) => {
                const { installedBytes, total } = sample;
                if (total > 0) {
                  const pct = Math.min(
                    99,
                    Math.floor((installedBytes / total) * 100),
                  );
                  pkgStore.setState({
                    busyNotice: `Installing "${item.displayName}" on the PS5… ${pct}%`,
                  });
                }
                feedRow(sample);
              },
              // Readiness-gate status (pre-install wait / DPI transient retry).
              (msg) => {
                pkgStore.setState({ busyNotice: msg });
                set((s) => ({
                  items: patchItem(s.items, item.id, { installNote: msg || null }),
                }));
              },
              // Identity for the post-install check AND for the background
              // re-verify of an accepted-but-unverified install. Without a
              // size the re-verify has nothing to match the installed artifact
              // against and returns false at its first line, forever — this is
              // the queue's mainline path, so it must carry one. The uploaded
              // byte count IS the staged pkg's size.
              (snap.total_bytes ?? 0) > 0
                ? { size: snap.total_bytes }
                : (snap.bytes_sent ?? 0) > 0
                  ? { size: snap.bytes_sent }
                  : undefined,
            );
            if (r.installed) {
              installPhase = r.mayNotLaunch ? "warn" : "done";
              installedTitle =
                item.installedTitle ?? item.contentId ?? item.displayName ?? null;
              // Persist this exact package as installed ON THIS CONSOLE so its
              // library row shows "Reinstall" — works for an update/DLC, which
              // the console's app_list can't confirm. Scoped to the item's host
              // so a sibling console with the same staged file isn't affected.
              // Survives auto-delete being off (the staged file then reappears).
              recordPkgInstalled(hostOf(item.addr), finalDest);
              if (r.mayNotLaunch) {
                mountWarnings.push(
                  "Installed, but it may not launch on this firmware — re-install it from Upload & Install if it won't start.",
                );
              }
              // Free the staging copy on success (default on). Best-effort —
              // a leftover staged pkg isn't a transfer failure. Let Sony's
              // installer release the file first (it can hold it for a beat
              // after the title registers — the cause of "fs_delete_failed"),
              // then retry a couple of times instead of a single attempt.
              if (item.deletePkgAfterInstall !== false) {
                await sleep(800);
                for (let attempt = 0; attempt < 3; attempt++) {
                  try {
                    await fsDelete(mgmtAddr(hostOf(item.addr)), finalDest);
                    break;
                  } catch {
                    if (attempt < 2) await sleep(700);
                    /* leftover staged pkg is harmless */
                  }
                }
              }
              // FW12+ install settle: the main-payload install briefly
              // destabilises SceShellUI (the "screen goes black" blip), and on a
              // multi-item queue (base + small updates/DLC) the NEXT item's
              // upload could start before the payload/connection fully recovers
              // — which the user saw as the queue stalling mid-DLC. Give the
              // console a moment to come back before runOne returns to the drain
              // loop. Gated to FW >= 12 where the blip is observed.
              await fw12InstallSettle(hostOf(item.addr));
            } else {
              // The bytes landed but the install — the point of a pkg — did not
              // COMPLETE (the engine's verdict was `failed`). The staged pkg was
              // KEPT on the PS5 (never deleted on a non-confirmed install), so
              // the user can retry. Fail the row so they notice; the message
              // routes through the queue's humanizer.
              installPhase = "error";
              log.info(
                "install",
                `pkg "${item.displayName}" install not confirmed — staged pkg KEPT: ${finalDest}`,
              );
              // PostUploadStepError, not Error: the bytes are committed, so
              // the auto-recovery loop must not re-run this item — that would
              // re-upload the whole package (a user watched a 5.93 GiB update
              // restart itself six seconds after an install error, unasked).
              throw new PostUploadStepError(
                r.errMessage || "Install was rejected.",
              );
            }
          } finally {
            pkgStore.setState({ installing: false, busyNotice: null });
          }
        }
        // Final readout = total bytes / total elapsed. Prefer the
        // engine's elapsed_ms (measured payload-side) over a wall-
        // clock diff so a slow first poll doesn't skew the average.
        const finalBytes = snap.bytes_sent ?? 0;
        const elapsedMs = snap.elapsed_ms ?? Date.now() - startedAtMs;
        // Persist this host's measured throughput so the next upload's
        // pre-flight ETA banner can use a sharper number. Same shape
        // as transfer.ts's recording site; queue uploads were the
        // primary user-reported pain point (huge folders) so this
        // path matters most. commitMsPerFile is left undefined until
        // P3's APPLY_PROGRESS frames give us the apply-time signal.
        //
        // Known bias (same as transfer.ts): `snap.elapsed_ms`
        // includes the post-100% PS5 commit phase, so a 50-min
        // upload that was 30 min transfer + 20 min apply records as
        // ~55 MiB/s instead of the actual ~90 MiB/s transfer rate.
        // The next banner estimate is then conservatively long, which
        // is the right direction for UX. Sharper accounting waits for
        // P3 APPLY_PROGRESS — see review notes surface C1.
        if (finalBytes > 0 && elapsedMs > 0) {
          const throughputMibps = finalBytes / 1024 / 1024 / (elapsedMs / 1000);
          useRecentHostMetricsStore.getState().record(item.addr, {
            throughputMibps,
            measuredAtMs: Date.now(),
          });
        }
        // The finished transfer's bottleneck (commit ack); a stale skipping/settling
        // note from the last running tick must not outlive the job.
        const finishedLive = jobLiveFromSnapshot({
          bottleneck: snap.bottleneck,
          commit_ack: snap.commit_ack,
        });
        if (finishedLive?.unsettled) {
          // The engine stopped waiting for the console to finish saving the files: not a clean success.
          pushNotification(
            "warning",
            withConsolePrefix(
              item.addr,
              trStatic("upload_warn_unsettled_title", "Upload finished, but not confirmed saved"),
            ),
            {
              body: trStatic(
                "upload_warn_unsettled",
                "Every byte reached the console, but it has not confirmed saving all files yet. They finish on their own; if the console loses power first, send the folder again.",
              ),
            },
          );
        }
        return {
          bytesSent: finalBytes,
          bytesPerSec: averageRate(finalBytes, elapsedMs),
          live: finishedLive,
          mountedAt,
          mountWarnings,
          registeredAs,
          installPhase,
          installedTitle,
        };
      }
      if (snap.status === "failed") {
        // Throw the *structured* error so start()'s catch can lift
        // error_reason/error_detail onto the item — the queue row's
        // humanized hint (humanizeJobErrorReason, e.g. "PS5 ran out of
        // space — click Retry to resume") depends on these. A plain
        // Error here drops the reason, so the row showed only the raw
        // {"error":…,"detail":…} blob the single-shot path avoids.
        throw new UploadJobError(
          snap.error ?? "upload failed",
          snap.error_reason,
          snap.error_detail,
        );
      }
      // Still running — push live progress + smoothed rate into the
      // item so the row shows a moving bar + speed without an extra
      // round-trip from the renderer.
      const now = Date.now();
      const bytesSent = snap.bytes_sent ?? 0;
      pushRateSample(samples, now, bytesSent);
      const bytesPerSec = computeRate(samples, now);
      if (bytesSent !== lastBytes) {
        lastBytes = bytesSent;
        lastMoveAt = now;
      }
      const totalNow = snap.total_bytes ?? 0;
      // Everything sent and the console making it durable is not a stall.
      const sending = !(totalNow > 0 && bytesSent >= totalNow);
      const stalledForMs = sending && now - lastMoveAt >= STALL_AFTER_MS ? now - lastMoveAt : null;
      set((s) => ({
        items: patchItem(s.items, item.id, {
          bytesSent,
          totalBytes: totalNow,
          bytesPerSec,
          stalledForMs,
          // P3 / v2.18.0 — forward apply-phase counters so the
          // QueueRow's finalize pill can show "Finalized N of M files"
          // (analogous to transfer.ts's single-shot path). Defaults to
          // 0 when the engine doesn't surface them (pre-P3 payloads
          // OR outside the finalize phase).
          filesFinalized: snap.files_finalized ?? 0,
          filesFinalizingTotal: snap.files_finalizing_total ?? 0,
          live: jobLiveFromSnapshot(snap),
        }),
      }));
      await sleep(POLL_INTERVAL_MS, { poll: true });
    }
    throw new Error("queue stopped");
  };

  /** Whether the poller last saw this item's console in rest mode (lib/consolePower). */
  const hostAsleep = (addr: string) =>
    useConnectionStore.getState().runtimeByHost[hostOf(addr) || "_"]?.power === "rest";

  /** Park an upload until its console wakes: pending, marked, nothing counted as a failure.
   *  `why` (the failure that met the sleep, if any) is only logged. */
  const parkForWake = (item: QueueItem, why?: string | null) => {
    set((s) => ({
      items: patchItem(s.items, item.id, {
        status: "pending",
        waitingForWake: true,
        interrupted: true,
        live: undefined,
        bytesPerSec: 0,
        stalledForMs: null,
        recovering: false,
        recoverAttempt: 0,
        error: null,
        errorReason: null,
        errorDetail: null,
      }),
    }));
    scheduleSave();
    wakeForUpload(item.addr, useUploadSettingsStore.getState().wakeForUploads);
    log.info(
      "queue",
      `${hostOf(item.addr)} is in rest mode — ${item.sourcePath || item.id} waits for the PS5 to wake` +
        (why ? ` (${why})` : ""),
    );
  };

  /** One console's drain loop: repeatedly pick the next item via
   *  `pickNext`, run it to a terminal state, settling between jobs on the
   *  SAME payload. `isLive` is the per-host liveness check — the loop bails
   *  between awaits once its console is stopped (or superseded by a fresh
   *  start). Each console gets its own loop + its own `isLive`, so they run
   *  in parallel and stop independently. Returns when `pickNext` is empty,
   *  on stop, or on a hard-stop-after-failure. */
  const runDrainLoop = async (
    pickNext: () => QueueItem | null,
    isLive: () => boolean,
    // A run the user started may send the helper (and clears a "stopped by you" mark); one
    // nobody clicked (wake resume) may not (#471).
    sendMode: "auto" | "explicit" = "explicit",
  ) => {
    // Make sure the console is on the payload that matches this build
    // BEFORE running any jobs. Older payloads lack the mgmt-port
    // hardening (backlog 8→128 + reconcile storm mitigations from
    // v2.23.1). Best-effort; never throws. Each loop preflights its OWN
    // console's first item.
    const head = pickNext();
    // An item that adopts a running job needs no payload check: the console is mid-transfer.
    // Nor does a console in rest mode: nothing answers until it wakes (the item parks below).
    if (head && !head.attachJobId && !hostAsleep(head.addr)) {
      try {
        await ensurePayloadCurrent(hostOf(head.addr), undefined, false, sendMode);
      } catch (e) {
        console.warn("ensurePayloadCurrent threw:", e);
      }
      if (!isLive()) return;
    }

    let jobsRun = 0;
    drain: while (isLive()) {
      const next = pickNext();
      if (!next) break;
      // An install from a stopped loop may still be running on this console.
      const nh = hostOf(next.addr);
      const inflight = installInFlight.get(nh);
      if (
        inflight &&
        get().items.some(
          (it) =>
            it.sourceKind === "install" &&
            it.status === "running" &&
            hostOf(it.addr) === nh,
        )
      ) {
        await inflight;
        if (!isLive()) return;
        continue;
      }

      // The console is in rest mode: this upload waits for the wake instead of failing on
      // a console that cannot answer (it resumes on the helper-up edge after it).
      if (
        next.sourceKind !== "install" &&
        !next.attachJobId &&
        useUploadSettingsStore.getState().autoResume &&
        hostAsleep(next.addr)
      ) {
        parkForWake(next);
        break drain;
      }

      // Let the payload settle between jobs (see INTER_JOB_SETTLE_MS).
      // Per-loop counter ⇒ the settle is per-console, not global.
      if (jobsRun > 0) {
        await sleep(INTER_JOB_SETTLE_MS);
        if (!isLive()) return;
      }
      jobsRun += 1;

      const startedAt = Date.now();
      set((s) => ({
        items: patchItem(s.items, next.id, {
          status: "running",
          interrupted: false,
          waitingForWake: false,
          stalledForMs: null,
          startedAt,
          // Reset live counters so a previously-failed-then-retried
          // item starts the bar + speed readout from zero instead
          // of inheriting the stale terminal values.
          bytesSent: 0,
          totalBytes: 0,
          bytesPerSec: 0,
          recovering: false,
          recoverAttempt: 0,
          error: null,
          errorReason: null,
          errorDetail: null,
        }),
      }));
      scheduleSave();

      // Per-item run with bounded auto-recovery. Each pass is a full
      // attempt; on a *recoverable* failure (the payload crashed or the
      // connection dropped) we surface a "recovering" state, wait a
      // backoff, re-deploy the payload via ensurePayloadCurrent if it's
      // down, then retry. The retry re-runs reconcile, so it resumes from
      // exactly the unfinished files (including the one that was mid-
      // flight) — recovery is idempotent and never double-writes. Fatal
      // errors (out of space, path rejected, source missing) and a spent
      // attempt budget fall through to a terminal "failed".
      let recoverAttempt = 0;
      for (;;) {
        try {
          const {
            bytesSent,
            bytesPerSec,
            mountedAt,
            mountWarnings,
            registeredAs,
            installPhase,
            installedTitle,
            installNote,
            live,
          } =
            await runOne(next, isLive);
          // Always flip to "done" once runOne returns success — the
          // upload + (optional) mount are committed PS5-side, and
          // resetting the row to "pending" via resetRunningToPending
          // would silently lie: the next Start would re-upload + try
          // to re-mount, hitting EBUSY at mount time and wasting the
          // bytes already on the console. Pre-2.2.52 a Stop landing
          // between runOne's success and this `set` produced exactly
          // that phantom-pending state. Honesty > liveness here:
          // record the committed work and let the user re-process
          // the queue if they want to skip the row.
          set((s) => ({
            items: patchItem(s.items, next.id, {
              status: "done",
              bytesSent,
              bytesPerSec,
              mountedAt,
              mountWarnings,
              registeredAs,
              installPhase,
              installedTitle,
              installNote: installNote ?? null,
              live,
              recovering: false,
              recoverAttempt: 0,
              completedAt: Date.now(),
            }),
          }));
          scheduleSave();
          // An archive picked on a saved server was copied here to upload; the copy is done
          // with now. (Local paths are not copies and are left alone.)
          void releaseCopy(next.sourcePath);
          releaseArchiveCopy(next.id);
          if (next.deleteSourceAfterUpload) {
            void deleteSourceAfterUpload(next.sourcePath, next.addr);
          }
          // The last row sent now finished: the upload it paused goes on next.
          settleSendNow(nh);
          if (!isLive()) return;
          break; // success → next item
        } catch (e) {
          // An install's outcome is recorded even after Stop: it really ran.
          if (!isLive() && next.sourceKind !== "install") return;
          const message = e instanceof Error ? e.message : String(e);
          // A React render loop (#185) thrown out of a store update is the UI's fault, not the
          // transfer's: keep its stack for the bug report, which is what finds the component.
          if (/Minified React error #185|Maximum update depth exceeded/i.test(message)) {
            log.error(
              "queue",
              `render loop while running ${next.sourceKind} ${next.id}: ${
                e instanceof Error ? (e.stack ?? message) : message
              }`,
            );
          }
          // Lift the structured payload error fields onto the item
          // if waitForJob's thrown error carries them. UI uses these
          // to render a humanized hint via `humanizeJobErrorReason`
          // — without the structured fields the user just sees the
          // raw chain (which often ends in {"error":"…","detail":"…"}
          // JSON that's hard to read in a queue row).
          const reason =
            e instanceof UploadJobError ? (e.reason ?? null) : null;
          const detail =
            e instanceof UploadJobError ? (e.detail ?? null) : null;

          const autoResume = useUploadSettingsStore.getState().autoResume;
          // An install is never re-run by itself: a repeated patch install
          // can wipe the base game. Only uploads auto-recover.
          const isInstall = next.sourceKind === "install";
          // The console went into rest mode: wait for the wake rather than fail or spend a
          // recovery attempt on a console that cannot answer (lib/uploadRecovery).
          if (
            !isInstall &&
            autoResume &&
            shouldParkForWake(e, reason, message, hostAsleep(next.addr))
          ) {
            parkForWake(next, detail ?? message);
            break drain;
          }
          const canRecover =
            !isInstall &&
            autoResume &&
            recoverAttempt < MAX_AUTO_RECOVER_ATTEMPTS &&
            // Every build can send the helper: the browser has the engine send it.
            shouldAutoRecover(e, reason, message, { canSendHelper: true });

          if (!canRecover) {
            set((s) => ({
              items: patchItem(s.items, next.id, {
                status: "failed",
                live: undefined,
                bytesPerSec: 0,
                recovering: false,
                recoverAttempt: 0,
                error: message,
                errorReason: reason,
                errorDetail: detail,
                completedAt: Date.now(),
                ...(isInstall
                  ? { installPhase: "error" as const, installNote: null }
                  : {}),
                ...(isInstall &&
                e instanceof InstallItemError &&
                e.result.stagedFallbackRecommended &&
                next.install?.via === "stream" &&
                !isRemotePath(next.install.source)
                  ? { fallbackToUpload: true }
                  : {}),
              }),
            }));
            scheduleSave();
            if (isInstall) {
              settle(
                next.id,
                e instanceof InstallItemError ? e.result : { ok: false, message },
              );
            }
            // A row sent now that failed still hands the console back to the upload it paused.
            settleSendNow(nh);
            if (!isLive()) return;
            // One failed install never holds up the installs queued after it
            // (Install all used to carry on past a failure too). Nor does a failed row sent
            // now hold up the upload it paused.
            if (
              !isInstall &&
              !next.sendNowAt &&
              !shouldContinueAfterFailure(get().continueOnFailure)
            ) {
              break drain; // hard stop: tear down this console's loop
            }
            break; // continueOnFailure → move to the next item
          }

          // Recoverable: show the "recovering (n/max)" state, hold the
          // failure text so the row explains why, then wait + heal.
          recoverAttempt += 1;
          set((s) => ({
            items: patchItem(s.items, next.id, {
              status: "running",
              recovering: true,
              recoverAttempt,
              bytesPerSec: 0,
              error: message,
              errorReason: reason,
              errorDetail: detail,
            }),
          }));
          scheduleSave();

          // Interruptible backoff: poll isLive() so a Stop during the
          // (up to 30 s) wait is honored within ~250 ms instead of making
          // the user wait out the whole backoff.
          const backoffMs = autoRecoverBackoffMs(recoverAttempt - 1);
          for (let waited = 0; waited < backoffMs; waited += 250) {
            await sleep(Math.min(250, backoffMs - waited));
            if (!isLive()) return;
          }
          // It fell asleep while we waited: no attempt can land until it wakes.
          if (hostAsleep(next.addr)) {
            parkForWake(next, message);
            break drain;
          }
          // Re-deploy the payload, then poll until it answers. force=true:
          // we got here from a connection-class transfer failure, so the
          // payload is suspect — its transfer listener may be dead even
          // if management still answers the version check. A
          // plain (non-force) call would see "version matches → current" and
          // skip the redeploy, leaving the dead listener in place so the resume
          // retry fails again — the "had to re-send the ELF manually" bug.
          // Re-send is idempotent and the resume continues from committed
          // shards, so a needless redeploy on a transient blip only costs the
          // boot wait, never re-uploaded data.
          // `() => !isLive()` lets a Stop bail out of the ~30 s boot-wait
          // promptly instead of leaving a ghost push running.
          // Not when only the status reads failed: the transfer may still be running on the
          // helper, so re-send it only if it no longer answers (the retry then adopts the job).
          const forceRedeploy = !(e instanceof Error && lostContactErrors.has(e));
          try {
            // Unattended: a console whose helper the user stopped is left alone (#471).
            await ensurePayloadCurrent(
              hostOf(next.addr),
              () => !isLive(),
              forceRedeploy,
              "auto",
            );
          } catch (healErr) {
            console.warn("auto-resume: ensurePayloadCurrent threw:", healErr);
          }
          if (!isLive()) return;

          // Clear the recovering banner + counters and loop to retry.
          set((s) => ({
            items: patchItem(s.items, next.id, {
              status: "running",
              recovering: false,
              bytesSent: 0,
              totalBytes: 0,
              bytesPerSec: 0,
              error: null,
              errorReason: null,
              errorDetail: null,
            }),
          }));
          scheduleSave();
        }
      }
    }
  };

  /** Ends a Send now on `h` once no row sent now is still waiting or running there: the rows
   *  it paused go back in line (and into a limited run's scope, so the loop takes them next).
   *  A finished row's mark is cleared, so a later retry waits its turn. Starts nothing itself:
   *  a live loop picks them up, and a stopped console waits for Start like before. */
  const settleSendNow = (h: string): string[] => {
    const mine = (it: QueueItem) => hostOf(it.addr) === h;
    const items = get().items;
    const live = items.some(
      (it) =>
        mine(it) &&
        !!it.sendNowAt &&
        !it.paused &&
        (it.status === "pending" || it.status === "running"),
    );
    const resumed: string[] = [];
    let changed = false;
    const next = items.map((it) => {
      if (!mine(it)) return it;
      if (it.sendNowAt && (it.status === "done" || it.status === "failed")) {
        changed = true;
        return { ...it, sendNowAt: null };
      }
      if (!live && it.paused === "sendNow" && it.status === "pending") {
        changed = true;
        resumed.push(it.id);
        return { ...it, paused: null };
      }
      return it;
    });
    if (!changed) return resumed;
    set({ items: next });
    scheduleSave();
    const scope = runScope.get(h);
    if (scope) for (const id of resumed) scope.ids.add(id);
    return resumed;
  };

  /** After a console's loop was stopped to take one row out of it (Cancel, Pause, Send now):
   *  waits for the engine to let go of the stopped job (it ends at its next shard boundary),
   *  then runs the console again. Starting at once put a second transfer on the console beside
   *  the one still ending. A Stop, Start or Clear meanwhile has the last word. */
  const restartAfterStop = (
    h: string,
    addr: string,
    stoppedJob: string | null,
    again?: { installsOnly?: boolean; onlyIds?: string[] },
  ) => {
    const stoppedGen = hostGen.get(h);
    void (async () => {
      if (stoppedJob) {
        for (let i = 0; i < CANCEL_SETTLE_TRIES; i++) {
          let status: string;
          try {
            status = (await jobStatus(stoppedJob, addr)).status;
          } catch {
            break; // the engine no longer knows it
          }
          if (status !== "running") break;
          await sleep(CANCEL_SETTLE_MS);
        }
      }
      if (hostGen.get(h) !== stoppedGen) return;
      if (nextPendingForHost(get().items, h)) void get().startHost(h, again);
    })();
  };

  /** Stops the running upload `item` and marks it paused (`why`). The console then runs again
   *  in the same scope as before, plus `alsoIds` (a row sent now). */
  const pauseRunning = (item: QueueItem, why: "user" | "sendNow", alsoIds: string[] = []) => {
    const h = hostOf(item.addr);
    const stoppedJob = runningJobByHost.get(h) ?? null;
    const scope = runScope.get(h);
    const scopeIds = scope ? [...scope.ids] : [];
    const scopeInstalls = scope?.installs ?? false;
    get().stopHost(h);
    set((s) => ({ items: patchItem(s.items, item.id, { paused: why }) }));
    scheduleSave();
    // Pausing a row sent now ends that Send now: the row it paused goes back in line.
    const resumed = why === "user" ? settleSendNow(h) : [];
    const again = scope
      ? { installsOnly: scopeInstalls, onlyIds: [...scopeIds, ...alsoIds, ...resumed] }
      : undefined;
    restartAfterStop(h, item.addr, stoppedJob, again);
  };

  const actions: QueueState = {
    items: [],
    continueOnFailure: false,
    running: false,
    runningHosts: {},
    limitedHosts: {},
    loaded: false,
    persistenceError: null,
    isLeader: true,

    async hydrate() {
      // The browser build elects one runner per profile. A tab that is not the runner shows the
      // queue the runner saves and does nothing else: no adopting jobs, no uploads, no saves.
      if (!isTauriEnv()) {
        const lead = await ensureLeaderElection();
        if (!lead) {
          await syncFollower();
          return;
        }
        if (!get().isLeader) set({ isLeader: true });
      }
      // The browser build reads the queue from localStorage (uploadQueueLoad has that branch),
      // so it hydrates too: without this a self-hosted UI forgot its whole queue on reload.
      try {
        // Nothing saved yet (a first run) loads as null.
        const doc = (await uploadQueueLoad<Partial<QueueDocument>>()) ?? {};
        // Sanitise on load:
        // - any item "running" when the app closed is stranded
        //   (engine restarted with no memory of the job) — reset to
        //   pending so the user can re-Start the queue.
        // - back-fill txIdHex for items written by an older build
        //   (pre-fix); a missing tx_id on a folder upload would
        //   crash the runner. Mint a fresh one — those items lose
        //   resume continuity (acceptable since they pre-date the
        //   feature) but they won't crash.
        const wasRunning: Array<{ id: string; jobId: string; addr: string }> = [];
        const installsToFind: Array<{ id: string; jobId: string; addr: string }> = [];
        const items = (doc.items ?? []).map((it) => {
          const next = { ...it };
          // Never carried across a reload: a stale id would adopt a job that is not this item's.
          delete next.attachJobId;
          // An install that was running is never re-run by itself: Sony may
          // already have accepted it, and repeating a patch install can wipe
          // the base game.
          if (next.sourceKind === "install" && next.status === "running" && next.installJobId) {
            // The engine may still know this job (the web UI's engine outlives the tab; a
            // reloaded window keeps the desktop engine): ask it below before calling it lost.
            installsToFind.push({ id: next.id, jobId: next.installJobId, addr: next.addr });
            next.installNote = trStatic(
              "queue_install_looking",
              "Checking how this install went…",
            );
          } else if (next.sourceKind === "install" && next.status === "running") {
            next.status = "failed";
            next.installPhase = "error";
            next.error = trStatic(
              "queue_install_interrupted",
              "Interrupted when the app closed. Check the game on the PS5, then retry if it isn't installed.",
            );
            next.completedAt = Date.now();
          }
          if (next.status === "running" && next.sourceKind !== "install") {
            // The web UI's engine outlives the tab, so this item's job may still be going:
            // remember it so it can be re-attached below. (The desktop engine dies with the
            // app, so there the job is gone and the item simply re-runs.)
            if (next.jobId) {
              wasRunning.push({ id: next.id, jobId: next.jobId, addr: next.addr });
              // Hold the runner off this item until its job has been looked up.
              if (!isTauriEnv()) reattaching.add(next.id);
            }
            next.status = "pending";
            next.interrupted = true;
            next.stalledForMs = null;
          }
          if (!next.txIdHex) next.txIdHex = generateTxIdHex();
          // Back-fill the bytes/sec field added in 2.2.22 — older
          // persisted docs don't carry it. Treat unknown as 0 so the
          // UI doesn't show NaN MiB/s on the first render after
          // upgrade.
          if (typeof next.bytesPerSec !== "number") next.bytesPerSec = 0;
          // Back-fill reconcileMode — a pre-reconcile persisted "resume" item
          // would otherwise pass undefined to startTransferDirReconcile (whose
          // engine arg is non-optional ReconcileMode). "fast" matches the
          // current add-time default.
          if (next.reconcileMode == null) next.reconcileMode = "fast";
          // Back-fill the mountWarnings field added in 2.2.52 — older
          // persisted docs don't carry it. Default to empty so the UI
          // can blindly read .mountWarnings.length without optional-
          // chaining at every site.
          if (!Array.isArray(next.mountWarnings)) next.mountWarnings = [];
          // Back-fill the structured-error fields added when payload
          // failure-reason surfacing landed: older docs only carry the
          // flat `error` string; null these so the UI doesn't read
          // undefined and crash on `.startsWith` etc.
          if (next.errorReason === undefined) next.errorReason = null;
          if (next.errorDetail === undefined) next.errorDetail = null;
          // Back-fill v2.18.0 apply-progress counters — pre-2.18 saves
          // don't carry these. Default to 0 so the QueueRow's
          // optional chain on the finalize pill stays safe.
          if (typeof next.filesFinalized !== "number") next.filesFinalized = 0;
          if (typeof next.filesFinalizingTotal !== "number")
            next.filesFinalizingTotal = 0;
          return next;
        });
        // A link item comes back without its request (never saved): drop it.
        const kept = items.filter(
          (it) => it.sourceKind !== "install" || it.install != null,
        );
        const dropped = items.length - kept.length;
        if (dropped > 0) {
          pushNotification(
            "info",
            trStatic("queue_links_cleared_title", "Queued links were cleared"),
            {
              body: trStatic(
                "queue_links_cleared_body_v2",
                "{n} install link(s) weren't kept after restart — links are never saved to disk. Add them again from Upload & Install.",
              ).replace("{n}", String(dropped)),
            },
          );
        }
        // Keep anything queued before the load finished (an install started
        // from another screen), after the saved items.
        const live = get().items.filter((it) => !kept.some((k) => k.id === it.id));
        set({
          items: [...kept, ...live],
          continueOnFailure: doc.continueOnFailure ?? false,
          loaded: true,
          persistenceError: null,
        });
        if (live.length > 0) scheduleSave();
        if (!isTauriEnv() && wasRunning.length > 0) void reattachRunning(wasRunning);
        for (const r of installsToFind) findInstallJob(r);
      } catch (e) {
        // load_json_or_default returns {} on missing file, so this
        // catch only fires on real corruption (bad JSON, IO error,
        // mutex poison). Don't silently treat that as "empty" — the
        // user might have a recoverable file. Log so it shows up in
        // engine.log and surface a banner via runStatus alongside
        // the empty queue.
        console.error("[upload-queue] hydrate failed:", e);
        const message = e instanceof Error ? e.message : String(e);
        set({
          loaded: true,
          persistenceError: `The saved upload queue could not be loaded: ${message}. The original queue file was left untouched.`,
        });
      }
    },

    enqueueInstall(input) {
      const existing = get().items.find((it) => sameInstall(it, input));
      // The same install again (a second click, or the same package from
      // another screen) joins the one already queued: one run, one result
      // for every caller.
      if (existing) return { id: existing.id, done: wait(existing.id) };
      const bare = hostOf(input.host);
      get().add({
        sourceKind: "install",
        sourcePath: installKey(input.request),
        displayName: input.displayName,
        resolvedDest: "",
        addr: consoleAddr(bare),
        strategy: "overwrite",
        reconcileMode: "fast",
        excludes: [],
        mountAfterUpload: false,
        mountReadOnly: true,
        registerAfterUpload: false,
        contentId: input.contentId ?? null,
        category: input.category ?? null,
        install: input.request,
        estimatedBytes: knownSize(input.sizeBytes),
      });
      const items = get().items;
      const added = items[items.length - 1];
      const done = wait(added.id);
      get().startAdded(bare, [added.id]);
      return { id: added.id, done };
    },

    retryInstall(id) {
      const it = get().items.find((x) => x.id === id);
      if (!it || it.sourceKind !== "install" || it.status !== "failed") return null;
      set((s) => ({
        items: patchItem(s.items, id, {
          status: "pending",
          error: null,
          errorReason: null,
          errorDetail: null,
          installPhase: null,
          installPct: null,
          installProgress: null,
          installNote: null,
          fallbackToUpload: false,
          completedAt: null,
        }),
      }));
      scheduleSave();
      const done = wait(id);
      void get().startHost(hostOf(it.addr));
      return { id, done };
    },

    async retryInstallViaUpload(id) {
      const it = get().items.find((x) => x.id === id);
      if (
        !it ||
        it.status !== "failed" ||
        it.install?.via !== "stream" ||
        isRemotePath(it.install.source)
      ) {
        return {
          ok: false,
          message: "Retry via upload works only for a file on this computer.",
        };
      }
      const source = it.install.source;
      const host = hostOf(it.addr);
      // Install Package's upload, with the options the old "Retry from PS5
      // staging?" dialog used: install once uploaded, and this file wins over
      // a same-version variant already staged. The failed row goes only once
      // the upload is accepted (onDest), so a refusal leaves it to retry.
      const lib = pkgLibraryStore(host);
      return new Promise((resolve) => {
        let accepted = false;
        void lib
          .getState()
          .addAndUpload(source, host, {
            installAfterUpload: true,
            selectVariant: true,
            onDest: () => {
              if (accepted) return;
              accepted = true;
              set((s) => ({ items: removeItem(s.items, id) }));
              scheduleSave();
              resolve({ ok: true });
            },
          })
          .then(() => {
            if (!accepted) {
              resolve({
                ok: false,
                message: lib.getState().error ?? "The package couldn't be added.",
              });
            }
          });
      });
    },

    add(input) {
      const item: QueueItem = {
        id: newId(),
        ...input,
        // Mint the tx_id at add time, not at start time, so the
        // value persists across app restarts. A queued item that
        // ran partway, app crashed, app reopens → next start of the
        // queue passes this same tx_id with TX_FLAG_RESUME and the
        // payload picks up from last_acked_shard.
        txIdHex: input.txIdHex || generateTxIdHex(),
        status: "pending",
        bytesSent: 0,
        totalBytes: 0,
        estimatedBytes: input.estimatedBytes ?? 0,
        bytesPerSec: 0,
        filesFinalized: 0,
        filesFinalizingTotal: 0,
        mountedAt: null,
        registeredAs: null,
        mountWarnings: [],
        error: null,
        errorReason: null,
        errorDetail: null,
        addedAt: Date.now(),
        startedAt: null,
        completedAt: null,
      };
      set((s) => ({ items: s.items.concat(item) }));
      scheduleSave();
      return item.id;
    },

    remove(id) {
      settle(id, { ok: false, message: "Removed from the queue." });
      // A failed item keeps its server copy for a retry; removing it is the last use. That
      // covers a copy made at pick time (Upload screen) too, which the item names directly.
      const removed = get().items.find((it) => it.id === id);
      if (removed) void releaseCopy(removed.sourcePath);
      releaseArchiveCopy(id);
      set((s) => ({ items: removeItem(s.items, id) }));
      scheduleSave();
      if (removed) settleSendNow(hostOf(removed.addr));
    },

    cancelItem(id) {
      const item = get().items.find((it) => it.id === id);
      if (!item) return;
      // A running install: ask the engine to stop it (it stops serving the package; the PS5's
      // download then fails, and a download already cancelled on the PS5 is let go at once
      // instead of after the 5-minute stall bound). The row goes once the job has ended.
      if (item.sourceKind === "install" && item.status === "running") {
        if (item.stopping) return;
        set((s) => ({ items: patchItem(s.items, id, { stopping: true }) }));
        void (async () => {
          // Stopped while the engine was still starting the install: its job id comes in a
          // moment, and only that stops it. Dropping the row alone left the install running
          // on the engine with nothing showing it.
          let job = item.installJobId ?? null;
          for (let i = 0; !job && i < 30; i++) {
            await new Promise((r) => setTimeout(r, 500));
            const now = get().items.find((it) => it.id === id);
            if (!now || now.status !== "running") break;
            job = now.installJobId ?? null;
          }
          if (job) await pkgInstallStop(job).catch(() => undefined);
          else get().stopHost(hostOf(item.addr));
          for (let i = 0; i < 60; i++) {
            const now = get().items.find((it) => it.id === id);
            if (!now || now.status !== "running") break;
            await new Promise((r) => setTimeout(r, 500));
          }
          settle(id, { ok: false, message: "Stopped." });
          set((s) => ({ items: removeItem(s.items, id) }));
          scheduleSave();
          // Stopping the loop held the console's other waiting items: let them go on.
          const h = hostOf(item.addr);
          if (!job && nextPendingForHost(get().items, h)) void get().startHost(h);
        })();
        return;
      }
      settle(id, { ok: false, message: "Removed from the queue." });
      const h = hostOf(item.addr);
      // A pending / done / failed item isn't touching the wire — just drop it.
      // (Pending removal also keeps the drain loop from ever claiming it.)
      if (item.status !== "running") {
        set((s) => ({ items: removeItem(s.items, id) }));
        scheduleSave();
        settleSendNow(h);
        return;
      }
      // The actively-uploading item. The transfer port is single-client, so
      // there is exactly one running item per console. Tear this console's loop
      // down the same way Stop does (aborts the in-flight job, re-stamps the
      // generation so runOne bails at its next await), drop the cancelled item,
      // then resume the console so its OTHER pending jobs aren't held hostage by
      // cancelling this one.
      const wasRunning = !!get().runningHosts[h];
      const cancelledJob = runningJobByHost.get(h) ?? null;
      get().stopHost(h);
      set((s) => ({ items: removeItem(s.items, id) }));
      scheduleSave();
      // Cancelling a row sent now ends that Send now: the row it paused goes back in line.
      settleSendNow(h);
      if (wasRunning && nextPendingForHost(get().items, h)) {
        restartAfterStop(h, item.addr, cancelledJob);
      }
    },

    pauseItem(id) {
      const item = get().items.find((it) => it.id === id);
      if (!item || item.status !== "running" || item.sourceKind === "install") return;
      pauseRunning(item, "user");
    },

    resumeItem(id) {
      const item = get().items.find((it) => it.id === id);
      if (!item || item.status !== "pending" || !item.paused) return;
      set((s) => ({ items: patchItem(s.items, id, { paused: null }) }));
      scheduleSave();
      // A run going takes it in its turn (a limited run is widened to it); an idle console
      // runs it now.
      void get().startHost(hostOf(item.addr), { onlyIds: [id] });
    },

    sendNow(id) {
      const item = get().items.find((it) => it.id === id);
      if (!item || item.status !== "pending" || item.paused) return;
      const h = hostOf(item.addr);
      // Clicks keep their order even within one millisecond.
      const at = Math.max(
        Date.now(),
        ...get().items.map((it) => (it.sendNowAt ?? 0) + 1),
      );
      set((s) => ({ items: patchItem(s.items, id, { sendNowAt: at }) }));
      scheduleSave();
      const running = get().items.find(
        (it) => it.status === "running" && hostOf(it.addr) === h,
      );
      // Pause the upload going now, unless it was sent now itself (they run in click order)
      // or is an install (Sony's install cannot be stopped; this row runs right after it).
      if (running && running.sourceKind !== "install" && !running.sendNowAt) {
        pauseRunning(running, "sendNow", [id]);
        return;
      }
      void get().startHost(h, { onlyIds: [id] });
    },

    noteSourceSize(id, bytes) {
      const size = knownSize(bytes);
      if (!size) return;
      const it = get().items.find((x) => x.id === id);
      if (!it || it.status !== "pending" || (it.estimatedBytes ?? 0) > 0) return;
      set((s) => ({ items: patchItem(s.items, id, { estimatedBytes: size }) }));
      scheduleSave();
    },

    moveUp(id) {
      // Reorder within the item's OWN console and run tier. The panel lists
      // a console's waiting rows in run order (base → update → DLC, then list
      // order), so "up" must swap with the waiting row shown just above — a
      // swap with a finished row, or across tiers, would change nothing the
      // user can see.
      set((s) => ({
        items: moveItemUpWithinGroup(s.items, id, moveGroupOf),
      }));
      scheduleSave();
    },

    moveDown(id) {
      set((s) => ({
        items: moveItemDownWithinGroup(s.items, id, moveGroupOf),
      }));
      scheduleSave();
    },

    moveTo(id, targetId) {
      set((s) => ({
        items: moveItemToWithinGroup(s.items, id, targetId, moveGroupOf),
      }));
      scheduleSave();
    },

    clear(host) {
      // `host`: only that console's items (the panel shows one console at a
      // time; Clear on it must not discard another console's queue).
      const h = host ? hostOf(host) : null;
      const mine = (it: QueueItem) => h === null || hostOf(it.addr) === h;
      // A running transfer is cancelled below (its engine job too); a running
      // install stays, because an install can't be stopped once Sony has it.
      // Say which of the two happened — the notice must match what Clear did.
      const cleared = get().items.filter(mine);
      const keptOfMine = cleared.filter(
        (it) => it.sourceKind === "install" && it.status === "running",
      );
      const keep = get().items.filter((it) => !mine(it) || keptOfMine.includes(it));
      for (const it of cleared) {
        if (!keep.includes(it)) settle(it.id, { ok: false, message: "Removed from the queue." });
      }
      const notice = clearNotice(cleared, keptOfMine);
      if (notice) {
        pushNotification("info", withConsolePrefix(notice.addr, notice.title), {
          body: notice.body,
        });
      }
      // Re-stamp every cleared console's generation so any in-flight loop
      // exits at the next await, then wipe the list + run state.
      const rh = { ...get().runningHosts };
      for (const rhost of Object.keys(rh)) {
        if (h !== null && rhost !== h) continue;
        hostGen.set(rhost, ++genCounter);
        delete rh[rhost];
        // Truly cancel each console's in-flight engine job too.
        const jid = runningJobByHost.get(rhost);
        if (jid) {
          runningJobByHost.delete(rhost);
          void cancelEngineJob(jid).catch(() => {});
        }
      }
      set({ items: keep, runningHosts: rh, running: anyRunning(rh) });
      scheduleSave();
    },

    retryFailed(host) {
      // `host`: only that console's failed rows, run on that console alone.
      const h = host ? hostOf(host) : null;
      const mine = (it: QueueItem) => h === null || hostOf(it.addr) === h;
      set((s) => ({
        items: s.items.map((it) => {
          if (!mine(it) || it.status !== "failed") return it;
          const pending = { ...it, status: "pending" as const };
          return it.sourceKind === "install" && it.installPhase
            ? {
                ...pending,
                installPhase: null,
                installPct: null,
                installProgress: null,
                installNote: null,
                fallbackToUpload: false,
              }
            : pending;
        }),
      }));
      scheduleSave();
      // The button says Retry: run them, don't just put them back in the list.
      if (h !== null) void get().startHost(h);
      else void get().start();
    },

    restartItem(id) {
      const item = get().items.find((candidate) => candidate.id === id);
      if (!item || item.status !== "running" || item.sourceKind === "install") return;
      const host = hostOf(item.addr);
      // Restart the run the person started, not just this item: a full Start stays full, and a
      // limited run (an install, a click's items) keeps its scope.
      const scope = runScope.get(host);
      const again = scope
        ? { installsOnly: scope.installs, onlyIds: [...scope.ids, id] }
        : undefined;
      get().stopHost(host);
      // Let the engine let go of the job; the next run resumes it from the console's journal.
      setTimeout(() => void get().startHost(host, again), 1500);
    },

    retryItem(id) {
      const item = get().items.find((candidate) => candidate.id === id);
      if (!item || item.status !== "failed") return false;
      set((s) => ({
        items: s.items.map((candidate) =>
          candidate.id === id
            ? {
                ...candidate,
                status: "pending" as const,
                error: null,
                errorReason: null,
                errorDetail: null,
                completedAt: null,
              }
            : candidate,
        ),
      }));
      scheduleSave();
      return true;
    },

    retryWithPassword(id, password) {
      if (!password) return false;
      const item = get().items.find((candidate) => candidate.id === id);
      if (!item || item.status !== "failed") return false;
      if (!rarPasswordProblem(item.errorReason, item.error)) return false;
      set((s) => ({
        items: s.items.map((candidate) =>
          candidate.id === id ? { ...candidate, rarPassword: password } : candidate,
        ),
      }));
      if (!get().retryItem(id)) return false;
      void get().startHost(hostOf(item.addr));
      return true;
    },

    async discardPartial(id) {
      const item = get().items.find((candidate) => candidate.id === id);
      const path = item ? partialUploadPath(item) : null;
      if (!item || !path) return;
      await fsDelete(mgmtAddr(hostOf(item.addr)), path);
      set((s) => ({
        items: s.items.map((candidate) =>
          candidate.id === id ? { ...candidate, txIdHex: generateTxIdHex() } : candidate,
        ),
      }));
      scheduleSave();
    },

    async resumeFailedRecoverable(host) {
      if (!useUploadSettingsStore.getState().autoResume) return 0;
      const h = hostOf(host);
      const candidates = get().items.filter(
        (it) =>
          hostOf(it.addr) === h &&
          it.status === "failed" &&
          // Never re-run an install by itself (a repeated patch install can
          // wipe the base game): only uploads resume on reconnect.
          it.sourceKind !== "install" &&
          isAutoRecoverable(it.errorReason, it.error, { canSendHelper: true }),
      );
      let resumed = 0;
      for (const it of candidates) {
        if (get().retryItem(it.id)) resumed += 1;
      }
      // Rows parked while the console slept (waitingForWake): startHost below takes them.
      resumed += get().items.filter(
        (it) => hostOf(it.addr) === h && it.status === "pending" && it.waitingForWake,
      ).length;
      // Only spin up the drain loop when we actually reset a row — never
      // start uploads a user merely queued but hasn't launched. startHost is
      // guarded by its own generation counter, so a redundant call is a no-op.
      if (resumed > 0) await get().startHost(host, { unattended: true });
      return resumed;
    },

    setContinueOnFailure(b) {
      set({ continueOnFailure: b });
      scheduleSave();
    },

    startAdded(host, ids) {
      const h = hostOf(host);
      if (useUploadSettingsStore.getState().queueAutoStart) {
        // The whole console, in order: a plain start, which also widens a limited run.
        void get().startHost(h);
        return;
      }
      // Manual: the rows wait. A full run already going takes them in order by itself.
      if (get().runningHosts[h] && !get().limitedHosts[h]) return;
      if (ids.length === 0) return;
      const now = Date.now();
      // One note per burst (Install all queues a package at a time).
      if (now - (manualNoticeAt.get(h) ?? 0) < 4000) return;
      manualNoticeAt.set(h, now);
      pushNotification(
        "info",
        withConsolePrefix(
          h,
          trStatic("queue_added_waits_for_start", "Added to the queue — press Start queue to run it"),
        ),
        {
          body: trStatic(
            "queue_added_waits_for_start_body",
            "\"Start the queue when you add something\" is off, so nothing starts until you press Start queue.",
          ),
        },
      );
    },

    async startHost(host, opts) {
      const h = hostOf(host);
      const limited = !!opts?.installsOnly || (opts?.onlyIds?.length ?? 0) > 0;
      const widen = (scope: RunScope) => {
        if (opts?.installsOnly) scope.installs = true;
        for (const id of opts?.onlyIds ?? []) scope.ids.add(id);
      };
      // Already draining this console → no-op (idempotent; a second Start
      // click or a re-loop must not spawn a duplicate loop that double-
      // claims items). A limited run takes the newly asked items too; a plain
      // Start widens it to everything.
      if (get().runningHosts[h]) {
        const scope = runScope.get(h);
        if (!limited) {
          runScope.delete(h);
          markLimited(h, false);
        } else if (scope) widen(scope);
        return;
      }
      if (limited) {
        const scope: RunScope = { installs: false, ids: new Set() };
        widen(scope);
        runScope.set(h, scope);
      } else {
        runScope.delete(h);
      }
      markLimited(h, limited);
      const myGen = ++genCounter;
      hostGen.set(h, myGen);
      const isLive = () => hostGen.get(h) === myGen;
      set((s) => {
        const rh = { ...s.runningHosts, [h]: true };
        return {
          runningHosts: rh,
          running: true,
          // A start (the user's, or the wake resume's) is the moment to try parked rows again;
          // a console still asleep parks them again at once.
          items: s.items.some((it) => it.waitingForWake && hostOf(it.addr) === h)
            ? s.items.map((it) =>
                it.waitingForWake && hostOf(it.addr) === h ? { ...it, waitingForWake: false } : it,
              )
            : s.items,
        };
      });
      try {
        await runDrainLoop(
          () => pickPending(h),
          isLive,
          opts?.unattended ? "auto" : "explicit",
        );
      } finally {
        // Only clear our own flag if we're still the live generation — a
        // stopHost() or a superseding startHost() already owns it otherwise.
        if (isLive()) {
          set((s) => {
            const rh = { ...s.runningHosts };
            delete rh[h];
            return { runningHosts: rh, running: anyRunning(rh) };
          });
          markLimited(h, false);
          // Opt-in "rest mode after uploads finish" (#165). Fire only when
          // the drain completed NATURALLY (isLive — not a Stop, which
          // re-stamps the generation), the user enabled it, this console
          // has no more pending work, AND at least one item actually
          // reached "done" (don't sleep a console whose queue was entirely
          // cancelled/failed — that wasn't a successful upload session).
          // Fire-and-forget + guarded: a standby rejection (unsupported FW)
          // must never surface as an unhandledrejection from the drain loop.
          if (restAfterUploadEnabled() && !nextPendingForHost(get().items, h)) {
            // Only real upload sessions count. An install finishes on the
            // console after our call returns, and an Install Package upload
            // has its install queued right after it: sleeping then would cut
            // either one off.
            const didWork = get().items.some(
              (it) =>
                hostOf(it.addr) === h &&
                it.status === "done" &&
                isUploadItem(it) &&
                !(it.sourceKind === "pkg" && it.installAfterUpload === false),
            );
            if (didWork) {
              void powerStandby(mgmtAddr(h))
                .then((ack) => {
                  pushNotification(
                    ack.ok ? "info" : "warning",
                    withConsolePrefix(
                      h,
                      ack.ok
                        ? "Uploads done — entering rest mode"
                        : "Uploads done — couldn't enter rest mode",
                    ),
                    ack.ok
                      ? undefined
                      : {
                          body:
                            ack.err ||
                            "The PS5 declined standby (may be unavailable on this firmware).",
                        },
                  );
                })
                .catch((e) => {
                  log.warn(
                    "queue",
                    `rest-after-upload standby failed for ${h}: ${
                      e instanceof Error ? e.message : String(e)
                    }`,
                  );
                });
            }
          }
        }
      }
    },

    stopHost(host) {
      const h = hostOf(host);
      // Truly stop the in-flight transfer: ask the engine to cancel this
      // console's running job (it aborts at the next shard boundary; the
      // partial tx stays resumable, matching the row reset to "pending").
      const jid = runningJobByHost.get(h);
      if (jid) {
        runningJobByHost.delete(h);
        void cancelEngineJob(jid).catch(() => {
          /* engine gone / already finished — worker stop below still applies */
        });
      }
      // Re-stamp this host's generation so its live loop bails at the next
      // await, and reset ONLY this console's running rows to pending —
      // sibling consoles keep draining untouched.
      hostGen.set(h, ++genCounter);
      markLimited(h, false);
      set((s) => {
        const rh = { ...s.runningHosts };
        delete rh[h];
        return {
          runningHosts: rh,
          running: anyRunning(rh),
          // A running install stays running: Sony's install can't be
          // stopped, and re-queueing it would install it twice.
          items: resetRunningToPending(
            s.items,
            (it) => hostOf(it.addr) === h && it.sourceKind !== "install",
          ),
        };
      });
      scheduleSave();
    },

    async start() {
      // "Start all": kick every console that has pending work into its own
      // parallel drain loop. Re-evaluate the pending host set after each
      // batch so a console added mid-run (or one whose items only appeared
      // after the first batch) still gets drained — but only when
      // continueOnFailure is set, so a stop-on-failure console isn't
      // silently auto-restarted. Each console's transfer port is single-
      // client, so per-console work stays serial while DIFFERENT consoles
      // overlap; each item belongs to exactly one console ⇒ exactly one
      // loop ever claims it (no cross-loop claim race).
      for (let pass = 0; ; pass++) {
        // The first pass is the user's Start and takes rows parked for a sleeping console too;
        // later passes leave them to the wake, or a console still asleep would loop here. That
        // goes for the whole console: a later pass restarting it for a second row would un-park
        // the first, which parks again ahead of it, forever.
        const hosts = distinctPendingHosts(get().items, {
          includeWaitingForWake: pass === 0,
        }).filter((h) => !get().runningHosts[h] && (pass === 0 || !hostAsleep(h)));
        if (hosts.length === 0) break;
        await Promise.all(hosts.map((h) => get().startHost(h)));
        if (!get().continueOnFailure) break;
      }
    },

    stop() {
      // "Stop all": tear down every running console's loop. Each stopHost
      // re-stamps its generation (so the loop exits at the next await) and
      // resets that console's running rows to pending — idempotent for the
      // payload because TX_FLAG_RESUME + same-tx_id semantics are
      // independent of queue state.
      for (const h of Object.keys(get().runningHosts)) {
        get().stopHost(h);
      }
    },
  };

  // A tab that is not the runner shows the queue and changes nothing: every action is a no-op
  // with the neutral answer its caller expects (see lib/queueLeader).
  const readOnlyMessage =
    "The queue is running in another tab of this browser. Use that tab to change it.";
  const refusals: Partial<Record<keyof QueueState, () => unknown>> = {
    add: () => undefined,
    enqueueInstall: () => ({
      id: "",
      done: Promise.resolve({ ok: false, message: readOnlyMessage }),
    }),
    retryInstall: () => null,
    retryInstallViaUpload: () => Promise.resolve({ ok: false, message: readOnlyMessage }),
    remove: () => undefined,
    cancelItem: () => undefined,
    noteSourceSize: () => undefined,
    moveUp: () => undefined,
    moveDown: () => undefined,
    moveTo: () => undefined,
    clear: () => undefined,
    retryFailed: () => undefined,
    restartItem: () => undefined,
    retryItem: () => false,
    pauseItem: () => undefined,
    resumeItem: () => undefined,
    sendNow: () => undefined,
    retryWithPassword: () => false,
    discardPartial: () => Promise.resolve(),
    resumeFailedRecoverable: () => Promise.resolve(0),
    setContinueOnFailure: () => undefined,
    start: () => Promise.resolve(),
    stop: () => undefined,
    startHost: () => Promise.resolve(),
    startAdded: () => undefined,
    stopHost: () => undefined,
  };
  const guarded = { ...actions } as Record<string, unknown>;
  for (const [name, refuse] of Object.entries(refusals)) {
    const real = (actions as unknown as Record<string, (...a: unknown[]) => unknown>)[name];
    guarded[name] = (...args: unknown[]) => (get().isLeader ? real(...args) : refuse!());
  }
  return guarded as unknown as QueueState;
});

// pkgLibrary queues installs through the bridge (it can't import this module).
registerInstallEnqueuer((input) => useUploadQueueStore.getState().enqueueInstall(input));
registerPkgQueueApi({
  add(item) {
    // A tab that is not the queue's runner adds nothing (its add returns no id).
    const id = useUploadQueueStore.getState().add(item);
    if (!id) return "";
    // Same rule as every add (#536): the whole queue runs, or nothing does until Start.
    useUploadQueueStore.getState().startAdded(hostOf(item.addr), [id]);
    return id;
  },
  watch(id, cb) {
    const view = (items: QueueItem[]) => {
      const it = items.find((x) => x.id === id);
      return it
        ? {
            status: it.status,
            bytesSent: it.bytesSent,
            totalBytes: it.totalBytes,
            bytesPerSec: it.bytesPerSec,
            error: it.error,
          }
        : null;
    };
    cb(view(useUploadQueueStore.getState().items));
    return useUploadQueueStore.subscribe((s, prev) => {
      if (s.items !== prev.items) cb(view(s.items));
    });
  },
  remove(id) {
    useUploadQueueStore.getState().remove(id);
  },
});

/** Post-install settle. A main-payload install briefly destabilises SceShellUI
 *  (the screen-black blip) and the connection recovers a beat later; starting
 *  the next queued item's upload/install before the console is back stalls the
 *  queue (reported on multi small DLC/updates) or draws a transient install
 *  rejection. Rather than a blind sleep, ACTIVELY wait for the console to answer
 *  the readiness probe again — adaptive (returns the moment it's ready) and
 *  applies on every firmware, not just FW12. Bounded so a console that never
 *  clears the probe doesn't wedge the queue; a short floor sleep still covers
 *  the case where the probe can't report readiness at all. */
async function fw12InstallSettle(host: string): Promise<void> {
  const ready = await waitForConsoleReady(mgmtAddr(host), { timeoutMs: 30_000 });
  // If the probe never reported ready (older payload), fall back to the old
  // fixed settle so we don't barrel straight into the recovery window.
  if (!ready) await sleep(3000);
}

