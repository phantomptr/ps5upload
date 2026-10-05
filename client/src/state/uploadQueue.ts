import { consoleAddr } from "../lib/addr";
import { create } from "zustand";

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
  type ReconcileMode,
} from "../api/ps5";
import { restAfterUploadEnabled } from "./restAfterUpload";
import {
  moveItemDownWithinGroup,
  moveItemUpWithinGroup,
  patchItem,
  removeItem,
  resetFailedToPending,
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
import { withConsolePrefix } from "./roster";
import { hostOf, mgmtAddr } from "../lib/addr";
import { log } from "./logs";
import { isRemotePath } from "../lib/remotePath";
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
  registerInstallEnqueuer,
  registerPkgQueueApi,
  type EnqueueInstallInput,
  type EnqueuedInstall,
  type InstallProgress,
  type InstallRequest,
  type InstallResult,
} from "./consoleQueueBridge";
import { trStatic } from "../lib/trStatic";
import { ensurePayloadCurrent } from "../lib/ensurePayloadCurrent";
import { effectiveUploadStreams } from "../lib/uploadStreams";
import {
  autoRecoverBackoffMs,
  isAutoRecoverable,
  MAX_AUTO_RECOVER_ATTEMPTS,
  PostUploadStepError,
  refineHelperReason,
  shouldAutoRecover,
} from "../lib/uploadRecovery";
import { helperState } from "../api/ava1";
import { isTauriEnv } from "../lib/tauriEnv";
import { createQueueLeader } from "../lib/queueLeader";
import { useConnectionStore } from "./connection";

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

/** What a queue item does: an upload of some source kind, or an install. */
export type QueueSourceKind = SourceKind | "install";

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
  /** Pkg-only: the installed title (or content id) the finisher resolved,
   *  shown on the done row. Null otherwise. */
  installedTitle?: string | null;
  /** Stable tx_id for this queue item, minted at add-time and
   *  persisted alongside the item. Used so a queue interrupted by
   *  app restart can resume against the payload's existing journal
   *  entry instead of orphaning the in-flight tx and starting fresh.
   *  Folder uploads use TX_FLAG_RESUME with this id; file uploads
   *  ignore it (single-file resume isn't wired payload-side today). */
  txIdHex: string;
  status: QueueItemStatus;
  /** Live progress while running, final count when done, 0 otherwise. */
  bytesSent: number;
  /** Total bytes the engine pre-stat'd for this source. 0 until first
   *  Running tick lands. */
  totalBytes: number;
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
  | "strategy"
  | "reconcileMode"
  | "excludes"
  | "rarPassword"
  | "mountAfterUpload"
  | "mountReadOnly"
  | "registerAfterUpload"
  | "contentId"
  | "category"
  | "installAfterUpload"
  | "deletePkgAfterInstall"
  | "install"
>;

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
  add: (item: AddQueueItem) => void;
  /** Queue an install on its console and start that console. `done` resolves
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
  moveUp: (id: string) => void;
  moveDown: (id: string) => void;
  clear: () => void;
  retryFailed: () => void;
  /** Retry one failed row. Returns false when the row is missing/not failed. */
  retryItem: (id: string) => boolean;
  /** Retry one archive row that failed for a missing or wrong password, with the password
   *  the person just typed. The password lives on the in-memory item only (the save redacts
   *  it) and is never logged. False when the password is empty or the row is not a failed one. */
  retryWithPassword: (id: string, password: string) => boolean;
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
  /** Start every console that has pending work, each in its own parallel
   *  drain loop (== "Start all"). */
  start: () => Promise<void>;
  /** Stop every running console (== "Stop all"). */
  stop: () => void;
  /** Start (or no-op if already running) just one console's drain loop. */
  startHost: (host: string) => Promise<void>;
  /** Stop just one console; siblings keep running. */
  stopHost: (host: string) => void;
}

interface QueueDocument {
  items: QueueItem[];
  continueOnFailure: boolean;
}

const POLL_INTERVAL_MS = 500;
const SAVE_DEBOUNCE_MS = 300;

function newId(): string {
  // Same compatibility path as the transfer tx id. In an insecure LAN
  // browser context randomUUID is absent, but queueing must still work.
  return generateTxIdHex();
}

/** Distinct console hosts (bare IP, port-stripped) among the pending items,
 *  in first-seen order. The per-console parallel runner spawns one drain
 *  loop per host. Pure — exported for tests. */
export function distinctPendingHosts(items: QueueItem[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const it of items) {
    if (it.status !== "pending") continue;
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
 *  within a category is preserved). Pure — exported for tests. */
export function nextPendingForHost(
  items: QueueItem[],
  host: string,
): QueueItem | null {
  let best: QueueItem | null = null;
  let bestPrio = Number.POSITIVE_INFINITY;
  for (const it of items) {
    if (it.status !== "pending" || hostOf(it.addr) !== host) continue;
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

export const useUploadQueueStore = create<QueueState>((set, get) => {
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
    for (const h of hosts) void get().startHost(h);
  };

  /** The next pending item on `h` that the runner may take: not one whose engine job is still
   *  being looked up (see `reattaching`). */
  const pickPending = (h: string) =>
    nextPendingForHost(
      reattaching.size === 0
        ? get().items
        : get().items.filter((it) => !reattaching.has(it.id)),
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
          set({ isLeader: false, runningHosts: {}, running: false });
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
    for (let i = 0; i < CANCEL_SETTLE_TRIES; i++) {
      let status: string;
      try {
        status = (await jobStatus(jobId, addr)).status;
      } catch {
        return null; // the engine does not know it
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
  const settle = (id: string, r: InstallResult) => {
    const list = waiters.get(id);
    if (list) {
      waiters.delete(id);
      for (const w of list) w(r);
    }
  };

  /** Run an install item through the executor pkgLibrary registered. Throws
   *  InstallItemError when the install finished but didn't install. */
  const runInstallItem = async (item: QueueItem) => {
    const exec = getInstallExecutor();
    if (!exec || !item.install) {
      throw new Error("This install can't run: its details were not kept.");
    }
    set((s) => ({
      items: patchItem(s.items, item.id, {
        installPhase: "installing",
        installPct: 0,
        installProgress: null,
        installNote: null,
      }),
    }));
    const h = hostOf(item.addr);
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
      if (installInFlight.get(h) === tracked) installInFlight.delete(h);
    }
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
  const runOne = async (
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
    if (item.sourceKind === "install") return runInstallItem(item);
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
        // Clamp to THIS item's console — the queue drains every console
        // in parallel, so the active tab's advertised max is the wrong
        // capability for a background console's transfer.
        effectiveUploadStreams(item.addr),
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
      jobId = await startTransferFile(
        item.sourcePath,
        item.resolvedDest,
        item.addr,
        item.txIdHex,
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

    // Trailing-window samples for the live bytes/sec readout. Closure-
    // scoped so a Stop + restart of the same item resets cleanly: the
    // next runOne builds a fresh array.
    const startedAtMs = Date.now();
    const samples: RateSample[] = [{ ts: startedAtMs, bytes: 0 }];

    while (isLive()) {
      const snap = await jobStatus(jobId, item.addr);
      if (!isLive()) {
        throw new Error("queue stopped");
      }
      if (snap.status === "done") {
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
                  "Installed, but it may not launch on this firmware — re-install from the Install Package tab if it won't start.",
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
      set((s) => ({
        items: patchItem(s.items, item.id, {
          bytesSent,
          totalBytes: snap.total_bytes ?? 0,
          bytesPerSec,
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
      await sleep(POLL_INTERVAL_MS);
    }
    throw new Error("queue stopped");
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
  ) => {
    // Make sure the console is on the payload that matches this build
    // BEFORE running any jobs. Older payloads lack the mgmt-port
    // hardening (backlog 8→128 + reconcile storm mitigations from
    // v2.23.1). Best-effort; never throws. Each loop preflights its OWN
    // console's first item.
    const head = pickNext();
    // An item that adopts a running job needs no payload check: the console is mid-transfer.
    if (head && !head.attachJobId) {
      try {
        await ensurePayloadCurrent(hostOf(head.addr));
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
          if (!isLive()) return;
          break; // success → next item
        } catch (e) {
          // An install's outcome is recorded even after Stop: it really ran.
          if (!isLive() && next.sourceKind !== "install") return;
          const message = e instanceof Error ? e.message : String(e);
          // Lift the structured payload error fields onto the item
          // if waitForJob's thrown error carries them. UI uses these
          // to render a humanized hint via `humanizeJobErrorReason`
          // — without the structured fields the user just sees the
          // raw chain (which often ends in {"error":"…","detail":"…"}
          // JSON that's hard to read in a queue row).
          let reason =
            e instanceof UploadJobError ? (e.reason ?? null) : null;
          // "No AVA1 listener" says nothing about WHY. Ask the engine what the console runs:
          // an older helper is not fixed by re-sending (and the browser build cannot send at
          // all), so surface it as `helper_old` and let the banner's Update helper handle it,
          // instead of three blind retries.
          if (reason === "helper_not_ava1") {
            const refined = refineHelperReason(
              reason,
              await helperState(hostOf(next.addr)),
            );
            if (refined === "helper_old") {
              useConnectionStore
                .getState()
                .setHostStatus(hostOf(next.addr), { session: "helper_old" });
            }
            reason = refined;
          }
          const detail =
            e instanceof UploadJobError ? (e.detail ?? null) : null;

          const autoResume = useUploadSettingsStore.getState().autoResume;
          // An install is never re-run by itself: a repeated patch install
          // can wipe the base game. Only uploads auto-recover.
          const isInstall = next.sourceKind === "install";
          const canRecover =
            !isInstall &&
            autoResume &&
            recoverAttempt < MAX_AUTO_RECOVER_ATTEMPTS &&
            shouldAutoRecover(e, reason, message, {
              canSendHelper: isTauriEnv(),
            });

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
            if (!isLive()) return;
            // One failed install never holds up the installs queued after it
            // (Install all used to carry on past a failure too).
            if (!isInstall && !shouldContinueAfterFailure(get().continueOnFailure)) {
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
          try {
            await ensurePayloadCurrent(
              hostOf(next.addr),
              () => !isLive(),
              true,
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

  const actions: QueueState = {
    items: [],
    continueOnFailure: false,
    running: false,
    runningHosts: {},
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
        const doc = await uploadQueueLoad<Partial<QueueDocument>>();
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
        const items = (doc.items ?? []).map((it) => {
          const next = { ...it };
          // Never carried across a reload: a stale id would adopt a job that is not this item's.
          delete next.attachJobId;
          // An install that was running is never re-run by itself: Sony may
          // already have accepted it, and repeating a patch install can wipe
          // the base game.
          if (next.sourceKind === "install" && next.status === "running") {
            next.status = "failed";
            next.installPhase = "error";
            next.error = trStatic(
              "queue_install_interrupted",
              "Interrupted when the app closed. Check the game on the PS5, then retry if it isn't installed.",
            );
            next.completedAt = Date.now();
          }
          if (next.status === "running") {
            // The web UI's engine outlives the tab, so this item's job may still be going:
            // remember it so it can be re-attached below. (The desktop engine dies with the
            // app, so there the job is gone and the item simply re-runs.)
            if (next.jobId && next.sourceKind !== "install") {
              wasRunning.push({ id: next.id, jobId: next.jobId, addr: next.addr });
              // Hold the runner off this item until its job has been looked up.
              if (!isTauriEnv()) reattaching.add(next.id);
            }
            next.status = "pending";
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
                "queue_links_cleared_body",
                "{n} install link(s) weren't kept after restart — links are never saved to disk. Add them again from Install Package.",
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
      });
      const items = get().items;
      const added = items[items.length - 1];
      const done = wait(added.id);
      void get().startHost(bare);
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
        txIdHex: generateTxIdHex(),
        status: "pending",
        bytesSent: 0,
        totalBytes: 0,
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
    },

    remove(id) {
      settle(id, { ok: false, message: "Removed from the queue." });
      releaseArchiveCopy(id);
      set((s) => ({ items: removeItem(s.items, id) }));
      scheduleSave();
    },

    cancelItem(id) {
      const item = get().items.find((it) => it.id === id);
      if (!item) return;
      // Sony's install can't be stopped halfway; the row finishes on its own.
      if (item.sourceKind === "install" && item.status === "running") return;
      settle(id, { ok: false, message: "Removed from the queue." });
      const h = hostOf(item.addr);
      // A pending / done / failed item isn't touching the wire — just drop it.
      // (Pending removal also keeps the drain loop from ever claiming it.)
      if (item.status !== "running") {
        set((s) => ({ items: removeItem(s.items, id) }));
        scheduleSave();
        return;
      }
      // The actively-uploading item. The transfer port is single-client, so
      // there is exactly one running item per console. Tear this console's loop
      // down the same way Stop does (aborts the in-flight job, re-stamps the
      // generation so runOne bails at its next await), drop the cancelled item,
      // then resume the console so its OTHER pending jobs aren't held hostage by
      // cancelling this one.
      const wasRunning = !!get().runningHosts[h];
      get().stopHost(h);
      set((s) => ({ items: removeItem(s.items, id) }));
      scheduleSave();
      if (wasRunning && nextPendingForHost(get().items, h)) {
        void get().startHost(h);
      }
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

    clear() {
      // If an item is mid-transfer, the engine job + the real PS5-side
      // write keep running after we wipe the queue (the transfer port is
      // single-client, so the next upload will block behind it until it
      // finishes). Surface that instead of going silent — otherwise the
      // user clicks Clear, the UI empties, and a subsequent upload
      // mysteriously stalls behind the orphaned transfer. Mirrors the
      // documented reset() caveat in transfer.ts.
      // An install that is running stays (it can't be stopped); everything
      // else goes.
      const keep = get().items.filter(
        (it) => it.sourceKind === "install" && it.status === "running",
      );
      for (const it of get().items) {
        if (!keep.includes(it)) settle(it.id, { ok: false, message: "Removed from the queue." });
      }
      const inFlight = get().items.find((it) => it.status === "running");
      if (inFlight) {
        pushNotification(
          "info",
          withConsolePrefix(
            inFlight.addr,
            "Queue cleared — one upload is still finishing",
          ),
          {
            body: `"${inFlight.displayName}" is already transferring to the PS5 and will run to completion. The next upload waits until it's done.`,
          },
        );
      }
      // Re-stamp every running console's generation so any in-flight loop
      // exits at the next await, then wipe the list + run state.
      for (const h of Object.keys(get().runningHosts)) {
        hostGen.set(h, ++genCounter);
        // Truly cancel each console's in-flight engine job too.
        const jid = runningJobByHost.get(h);
        if (jid) {
          runningJobByHost.delete(h);
          void cancelEngineJob(jid).catch(() => {});
        }
      }
      set({ items: keep, runningHosts: {}, running: false });
      scheduleSave();
    },

    retryFailed() {
      set((s) => ({
        items: resetFailedToPending(s.items).map((it) =>
          it.sourceKind === "install" && it.status === "pending" && it.installPhase
            ? {
                ...it,
                installPhase: null,
                installPct: null,
                installProgress: null,
                installNote: null,
                fallbackToUpload: false,
              }
            : it,
        ),
      }));
      scheduleSave();
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
          isAutoRecoverable(it.errorReason, it.error, {
            canSendHelper: isTauriEnv(),
          }),
      );
      let resumed = 0;
      for (const it of candidates) {
        if (get().retryItem(it.id)) resumed += 1;
      }
      // Only spin up the drain loop when we actually reset a row — never
      // start uploads a user merely queued but hasn't launched. startHost is
      // guarded by its own generation counter, so a redundant call is a no-op.
      if (resumed > 0) await get().startHost(host);
      return resumed;
    },

    setContinueOnFailure(b) {
      set({ continueOnFailure: b });
      scheduleSave();
    },

    async startHost(host) {
      const h = hostOf(host);
      // Already draining this console → no-op (idempotent; a second Start
      // click or a re-loop must not spawn a duplicate loop that double-
      // claims items).
      if (get().runningHosts[h]) return;
      const myGen = ++genCounter;
      hostGen.set(h, myGen);
      const isLive = () => hostGen.get(h) === myGen;
      set((s) => {
        const rh = { ...s.runningHosts, [h]: true };
        return { runningHosts: rh, running: true };
      });
      try {
        await runDrainLoop(() => pickPending(h), isLive);
      } finally {
        // Only clear our own flag if we're still the live generation — a
        // stopHost() or a superseding startHost() already owns it otherwise.
        if (isLive()) {
          set((s) => {
            const rh = { ...s.runningHosts };
            delete rh[h];
            return { runningHosts: rh, running: anyRunning(rh) };
          });
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
      for (;;) {
        const hosts = distinctPendingHosts(get().items).filter(
          (h) => !get().runningHosts[h],
        );
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
    moveUp: () => undefined,
    moveDown: () => undefined,
    clear: () => undefined,
    retryFailed: () => undefined,
    retryItem: () => false,
    retryWithPassword: () => false,
    resumeFailedRecoverable: () => Promise.resolve(0),
    setContinueOnFailure: () => undefined,
    start: () => Promise.resolve(),
    stop: () => undefined,
    startHost: () => Promise.resolve(),
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
    const q = useUploadQueueStore.getState();
    q.add(item);
    const items = useUploadQueueStore.getState().items;
    const id = items[items.length - 1].id;
    void useUploadQueueStore.getState().startHost(hostOf(item.addr));
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

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
