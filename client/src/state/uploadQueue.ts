import { create } from "zustand";

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
  uploadQueueSave,
  UploadJobError,
  type ReconcileMode,
} from "../api/ps5";
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
import { runPkgInstall, pkgLibraryStore } from "./pkgLibrary";
import type { UploadStrategy } from "./transfer";
import { useUploadSettingsStore } from "./uploadSettings";
import { useRecentHostMetricsStore } from "./recentHostMetrics";
import { pushNotification } from "./notifications";
import { withConsolePrefix } from "./roster";
import { hostOf, mgmtAddr } from "../lib/addr";
import { log } from "./logs";
import { ensurePayloadCurrent } from "../lib/ensurePayloadCurrent";
import { effectiveUploadStreams } from "../lib/uploadStreams";
import {
  autoRecoverBackoffMs,
  isAutoRecoverable,
  MAX_AUTO_RECOVER_ATTEMPTS,
} from "../lib/uploadRecovery";

/** The engine job id currently uploading on each console (bare host key).
 *  runOne records it so stopHost/stop can ask the engine to TRULY cancel the
 *  in-flight transfer (not just halt the queue worker). Module-level — it's
 *  transient run state, not persisted queue data. A stale id (job already
 *  finished) is a harmless no-op server-side. */
const runningJobByHost = new Map<string, string>();

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

/** One queued upload. The shape is whatever the Upload screen
 *  captures at "Add to queue" time — source path, destination,
 *  strategy, exclude rules — plus runtime status that the runner
 *  updates as it processes the item. */
export interface QueueItem {
  id: string;
  sourceKind: SourceKind;
  sourcePath: string;
  /** Display-only basename so the list row doesn't re-derive it on
   *  every render. */
  displayName: string;
  /** Resolved final on-PS5 path (volume + subpath + basename). The
   *  user picked these on the Upload screen at queue-add time; the
   *  runner sends the file to this exact path. */
  resolvedDest: string;
  /** Transfer-port addr (e.g. `192.168.1.2:9113`). */
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
  /** Pkg-only: run the installer once the .pkg upload commits (default on —
   *  staging a pkg exists to install it). Mirrors installSettings, captured at
   *  add time so toggling the default mid-queue doesn't disturb queued rows. */
  installAfterUpload?: boolean;
  /** Pkg-only: delete the staged .pkg from the PS5 after a successful install
   *  (default on — it's just a staging copy). */
  deletePkgAfterInstall?: boolean;
  /** Pkg-only runtime state: the install phase after the upload commits.
   *  null until the finisher runs (or for non-pkg items). */
  installPhase?: "installing" | "done" | "warn" | "error" | null;
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
  /** P3 / v2.18.0 — apply-phase counters forwarded from JobSnapshot.
   *  Surface a "Finalized N of M files" pill on the queue row during
   *  the post-100% commit-apply phase. Both 0 outside the finalize
   *  phase and on pre-P3 payloads that don't emit APPLY_PROGRESS. */
  filesFinalized: number;
  filesFinalizingTotal: number;
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
  | "installAfterUpload"
  | "deletePkgAfterInstall"
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

  hydrate: () => Promise<void>;
  add: (item: AddQueueItem) => void;
  remove: (id: string) => void;
  moveUp: (id: string) => void;
  moveDown: (id: string) => void;
  clear: () => void;
  retryFailed: () => void;
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
  // 32-char hex from crypto UUID (same trick as generateTxIdHex).
  return crypto.randomUUID().replace(/-/g, "");
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

/** First pending item targeting `host` (port-stripped match), or null. The
 *  per-console runner's `pickNext`; stable array order = run order, same
 *  contract as `nextPending`. Pure — exported for tests. */
export function nextPendingForHost(
  items: QueueItem[],
  host: string,
): QueueItem | null {
  return (
    items.find((it) => it.status === "pending" && hostOf(it.addr) === host) ??
    null
  );
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

  /** Schedule a debounced whole-document save. Idempotent — multiple
   *  calls within 300 ms collapse into one fsync. The runner can
   *  legitimately fire a half-dozen patches per second (bytes_sent
   *  updates), and we don't want to round-trip Tauri/disk on each. */
  const scheduleSave = () => {
    if (saveTimer !== null) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      saveTimer = null;
      const { items, continueOnFailure } = get();
      // Redact RAR passwords before persisting — they stay in the live
      // in-memory items (so the current run can extract) but never touch disk.
      const persistItems = items.map((it) =>
        it.rarPassword ? { ...it, rarPassword: null } : it,
      );
      const doc: QueueDocument = { items: persistItems, continueOnFailure };
      void uploadQueueSave(doc).catch((e) => {
        // Persistence failure means the queue won't survive an app
        // restart. Log so it surfaces in the dev console + the
        // engine startup log on Windows; users debugging "my queue
        // disappeared" can find this. A toast-level UI surface is
        // future work — would need a new error channel that's
        // throttled (we save on every mutation, so a transient
        // disk-full would otherwise spam toasts).
        console.error("[upload-queue] save failed:", e);
      });
    }, SAVE_DEBOUNCE_MS);
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
  }> => {
    const isFolder =
      item.sourceKind === "folder" || item.sourceKind === "game-folder";
    const isArchive = item.sourceKind === "archive";

    // A .pkg stages into the package-library dir, then installs in the
    // finisher. Make sure the staging dir exists first — the single-file
    // transfer's open() fails ENOENT on a missing parent. EEXIST-tolerant.
    if (item.sourceKind === "pkg") {
      const parent = item.resolvedDest.replace(/\/[^/]*$/, "");
      if (parent) {
        try {
          await fsMkdir(item.addr, parent);
        } catch {
          /* dir already exists (or will fail loudly at open) */
        }
      }
    }

    let jobId: string;
    if (isArchive) {
      // A .zip/.7z is decompressed host-side and streamed in (lands
      // extracted). Carry the persisted tx_id for cross-session shard resume,
      // just like folders; there's no reconcile mode (no local tree to diff).
      // 7z re-decompresses from the start on resume (LZMA2 can't seek) and
      // re-sends only un-acked shards.
      const bandwidthCap = useUploadSettingsStore.getState().bandwidthCapMbps;
      const fmt = archiveFormat(item.sourcePath);
      if (fmt === "rar") {
        // .rar → host UnRAR extract; carry the (optional) password captured
        // at add time. Resume re-extracts and re-sends only un-acked shards.
        jobId = await startTransferRar(
          item.sourcePath,
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
          item.sourcePath,
          item.resolvedDest,
          item.addr,
          item.txIdHex,
          item.excludes,
          bandwidthCap,
        );
      }
    } else if (isFolder && item.strategy === "resume") {
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

    // Record the live job id for this console so stopHost/stop can ask the
    // engine to truly cancel the transfer (overwritten by the next item;
    // staleness is harmless — cancelling a finished job is a server no-op).
    runningJobByHost.set(hostOf(item.addr), jobId);

    // Trailing-window samples for the live bytes/sec readout. Closure-
    // scoped so a Stop + restart of the same item resets cleanly: the
    // next runOne builds a fresh array.
    const startedAtMs = Date.now();
    const samples: RateSample[] = [{ ts: startedAtMs, bytes: 0 }];

    while (isLive()) {
      const snap = await jobStatus(jobId);
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
          // Best-effort: if the SMP status probe or the list write fails,
          // fall back to mounting it ourselves.
          let handedToSmp = false;
          try {
            const smp = await smpStatus(mgmt);
            if (smp.running) {
              const r = await smpManualInstall(mgmt, finalDest);
              handedToSmp = true;
              mountedAt = r.added
                ? "handed to ShadowMount+"
                : "already in ShadowMount+ list";
            }
          } catch {
            handedToSmp = false; // SMP unreachable → mount it ourselves
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
              const wrapped = new Error(
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
          try {
            // delete_staging = the per-item Auto Delete preference (captured
            // from the setting at queue-add time). When off, the engine keeps
            // the uploaded pkg instead of deleting it post-install.
            const r = await runPkgInstall(
              item.addr,
              finalDest,
              item.contentId ?? null,
              item.deletePkgAfterInstall !== false,
              // Surface the live install % on this console's pkg screen while a
              // large queued title installs in the background.
              (installedBytes, total) => {
                if (total > 0) {
                  const pct = Math.min(
                    99,
                    Math.floor((installedBytes / total) * 100),
                  );
                  pkgStore.setState({
                    busyNotice: `Installing "${item.displayName}" on the PS5… ${pct}%`,
                  });
                }
              },
            );
            if (r.installed) {
              installPhase = r.mayNotLaunch ? "warn" : "done";
              installedTitle =
                item.installedTitle ?? item.contentId ?? item.displayName ?? null;
              if (r.mayNotLaunch) {
                mountWarnings.push(
                  "Installed, but it may not launch on this firmware — re-install from the Install Package tab if it won't start.",
                );
              }
              // Free the staging copy on success (default on). Best-effort —
              // a leftover staged pkg isn't a transfer failure.
              if (item.deletePkgAfterInstall !== false) {
                try {
                  await fsDelete(mgmtAddr(hostOf(item.addr)), finalDest);
                } catch {
                  /* leftover staged pkg is harmless */
                }
              }
            } else {
              // The bytes landed but the install — the point of a pkg — did
              // not COMPLETE. The staged pkg was KEPT on the PS5 (never deleted
              // on a non-confirmed install), so the user can retry. Fail the row
              // so they notice; the message (stall vs reject) routes through the
              // queue's humanizer.
              installPhase = "error";
              log.info(
                "install",
                r.stalled
                  ? `pkg "${item.displayName}" install stalled — staged pkg KEPT for retry: ${finalDest}`
                  : `pkg "${item.displayName}" install not confirmed — staged pkg KEPT: ${finalDest}`,
              );
              throw new Error(r.errMessage || "Install was rejected.");
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
        return {
          bytesSent: finalBytes,
          bytesPerSec: averageRate(finalBytes, elapsedMs),
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
    if (head) {
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
              recovering: false,
              recoverAttempt: 0,
              completedAt: Date.now(),
            }),
          }));
          scheduleSave();
          if (!isLive()) return;
          break; // success → next item
        } catch (e) {
          if (!isLive()) return;
          const message = e instanceof Error ? e.message : String(e);
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
          const canRecover =
            autoResume &&
            recoverAttempt < MAX_AUTO_RECOVER_ATTEMPTS &&
            isAutoRecoverable(reason, message);

          if (!canRecover) {
            set((s) => ({
              items: patchItem(s.items, next.id, {
                status: "failed",
                bytesPerSec: 0,
                recovering: false,
                recoverAttempt: 0,
                error: message,
                errorReason: reason,
                errorDetail: detail,
                completedAt: Date.now(),
              }),
            }));
            scheduleSave();
            if (!shouldContinueAfterFailure(get().continueOnFailure)) {
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
          // payload is suspect — its transfer port (:9113) may be dead even
          // if the mgmt port (:9114) still answers the version check. A
          // plain (non-force) call would see "version matches → current" and
          // skip the redeploy, leaving the dead :9113 in place so the resume
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

  return {
    items: [],
    continueOnFailure: false,
    running: false,
    runningHosts: {},
    loaded: false,

    async hydrate() {
      // Browser-only dev/test contexts: Tauri invoke is unavailable.
      // Mark loaded with the empty in-memory state and skip the call,
      // otherwise every Upload screen mount logs an "invoke undefined"
      // error to the user-visible logs. In production (Tauri), this
      // guard is a no-op.
      const w = window as unknown as {
        isTauri?: boolean;
        __TAURI_INTERNALS__?: unknown;
      };
      if (!w.isTauri && !w.__TAURI_INTERNALS__) {
        set({ loaded: true });
        return;
      }
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
        const items = (doc.items ?? []).map((it) => {
          const next = { ...it };
          if (next.status === "running") next.status = "pending";
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
        set({
          items,
          continueOnFailure: doc.continueOnFailure ?? false,
          loaded: true,
        });
      } catch (e) {
        // load_json_or_default returns {} on missing file, so this
        // catch only fires on real corruption (bad JSON, IO error,
        // mutex poison). Don't silently treat that as "empty" — the
        // user might have a recoverable file. Log so it shows up in
        // engine.log and surface a banner via runStatus alongside
        // the empty queue.
        console.error("[upload-queue] hydrate failed:", e);
        set({ loaded: true });
      }
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
      set((s) => ({ items: removeItem(s.items, id) }));
      scheduleSave();
    },

    moveUp(id) {
      // Reorder within the item's OWN console group — the grouped queue
      // renders each console's rows together, so "up" means "earlier among
      // this console's jobs," never swapping across consoles.
      set((s) => ({
        items: moveItemUpWithinGroup(s.items, id, (it) => hostOf(it.addr)),
      }));
      scheduleSave();
    },

    moveDown(id) {
      set((s) => ({
        items: moveItemDownWithinGroup(s.items, id, (it) => hostOf(it.addr)),
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
          void jobCancel(jid).catch(() => {});
        }
      }
      set({ items: [], runningHosts: {}, running: false });
      scheduleSave();
    },

    retryFailed() {
      set((s) => ({ items: resetFailedToPending(s.items) }));
      scheduleSave();
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
        await runDrainLoop(() => nextPendingForHost(get().items, h), isLive);
      } finally {
        // Only clear our own flag if we're still the live generation — a
        // stopHost() or a superseding startHost() already owns it otherwise.
        if (isLive()) {
          set((s) => {
            const rh = { ...s.runningHosts };
            delete rh[h];
            return { runningHosts: rh, running: anyRunning(rh) };
          });
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
        void jobCancel(jid).catch(() => {
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
          items: resetRunningToPending(s.items, (it) => hostOf(it.addr) === h),
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
});

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
