import { create } from "zustand";

import {
  startTransferFile,
  startTransferDir,
  startTransferDirReconcile,
  startTransferZip,
  startTransfer7z,
  startTransferRar,
  fsMount,
  fsUnmount,
  jobStatus,
  appRegister,
  smpManualInstall,
  smpStatus,
  resumeTxidLookup,
  resumeTxidRemember,
  resumeTxidForget,
  generateTxIdHex,
  toastPush,
  type JobSnapshot,
  type PlannedFile,
  type ReconcileMode,
} from "../api/ps5";
import { createRunGen } from "../lib/runGen";
import { log } from "./logs";
import {
  computeRate,
  pushRateSample,
  type RateSample,
} from "../lib/rollingRate";
import { archiveFormat, useUploadStore, type SourceKind } from "./upload";
import { useUploadSettingsStore } from "./uploadSettings";
import { useRecentHostMetricsStore } from "./recentHostMetrics";
import { effectiveUploadStreams } from "../lib/uploadStreams";

/** Module-level shortcut to the recent-host-metrics recorder. Pulled
 *  out as a function (not a direct `store.record`) so future call
 *  sites in this file — and in `uploadQueue.ts` once we wire it
 *  there too — can share a single line without each importing the
 *  store. Pure passthrough; no business logic added. */
function recordHostMetrics(
  addr: string,
  metrics: {
    throughputMibps: number;
    commitMsPerFile?: number;
    measuredAtMs: number;
  },
): void {
  useRecentHostMetricsStore.getState().record(addr, metrics);
}

/** For folder sources the caller picks one of these strategies at the
 *  moment the dialog is confirmed. Files/exfat images always use
 *  `overwrite` (no reconcile notion for a single blob). */
export type UploadStrategy = "overwrite" | "resume";

/**
 * Upload lifecycle for the Upload screen.
 *
 * idle → starting → running → done | failed
 *
 * `starting` covers the initial POST to /api/transfer/*; once the
 * engine returns a `job_id` we flip to `running` and poll
 * `/api/jobs/{id}` every 500ms for bytes/shards updates. Terminal
 * states (done/failed) stop polling and stay on screen until the user
 * dismisses them (by clicking Upload again or Cancel).
 */
export type TransferPhase =
  | { kind: "idle" }
  | { kind: "starting" }
  | {
      kind: "running";
      jobId: string;
      /** Wall-clock ms when we entered `running`. */
      startedAtMs: number;
      /** Live bytes counter from the engine's 200 ms progress ticker. */
      bytesSent: number;
      /** Total expected bytes (pre-stat of source). May be 0 if the
       *  engine couldn't stat the source before sending began. */
      totalBytes: number;
      /** Bytes/sec, smoothed across the last ~1s of samples. 0 until
       *  we have at least 2 samples. */
      bytesPerSec: number;
      /** Planned file list — used to render the per-file status panel.
       *  Empty until the first tick with a populated list arrives. */
      files: PlannedFile[];
      /** Number of files in `files` whose cumulative byte range is
       *  below `bytesSent` — treat these as "done". Derived; lives
       *  in state so the UI doesn't recompute every render. */
      filesCompleted: number;
      /** Reconcile-mode counters. 0 for plain uploads. */
      skippedFiles: number;
      skippedBytes: number;
      /** P3 / v2.18.0 — post-100% commit-phase counters fed by
       *  payload's APPLY_PROGRESS frames. Zero outside the
       *  finalize phase and on old payloads that don't emit. */
      filesFinalized: number;
      filesFinalizingTotal: number;
      bytesFinalized: number;
    }
  | {
      kind: "done";
      jobId: string;
      bytesSent: number;
      elapsedMs: number;
      dest: string;
      filesSent: number;
      skippedFiles: number;
      skippedBytes: number;
      mountedAt?: string;
      /** Non-fatal mount diagnostics surfaced when `mountAfterUpload`
       *  ran — image-layout invalid, kernel forced RO, etc. Kept on
       *  the done-phase so the Upload screen can display the same
       *  warnings the Library tab shows for a manual mount. Empty /
       *  absent on non-mount completions or on pre-2.2.52 payloads
       *  that didn't emit the diagnostic fields. */
      mountWarnings?: string[];
      /** When `registerAfterUpload` ran and succeeded: the game's
       *  display name (falls back to title id) — the Upload done card
       *  shows "On your PS5 home screen as <name>". */
      registeredAs?: string;
      /** When `registerAfterUpload` ran and FAILED: a human-readable
       *  reason. Non-fatal by design — the upload itself succeeded, and
       *  the user can still register manually from the Library. */
      registerWarning?: string;
    }
  | { kind: "failed"; error: string };

interface StartArgs {
  sourceKind: SourceKind;
  srcPath: string;
  dest: string;
  addr: string;
  /** For folder sources: "overwrite" = traditional re-send-everything,
   *  "resume" = use the reconcile endpoint (skips already-present files).
   *  Ignored for file/exfat sources. */
  strategy?: UploadStrategy;
  /** When strategy === "resume", which equality check to use. */
  reconcileMode?: ReconcileMode;
  /** Folder-only exclude patterns. Empty = upload everything. */
  excludes?: string[];
  /** Image-only: mount the uploaded disk image after the transfer commits. */
  mountAfterUpload?: boolean;
  /** Image-only: when mounting, mount read-only (default true). The PS5
   *  can't write save-data into the image (which would silently corrupt
   *  the source file on disk and ruin re-mount on next boot). Set false
   *  only when the user explicitly wants RW for editable scratch images. */
  mountReadOnly?: boolean;
  /** Game-folder-only: after the upload commits, register the game with
   *  the PS5 OS so it appears on the home screen — collapsing the old
   *  upload → Library → Register journey into one step. Best-effort:
   *  a registration failure does NOT fail the upload (the files are
   *  safely on the console); it surfaces as `registerWarning` on the
   *  done phase instead. */
  registerAfterUpload?: boolean;
}

/** Stable idle reference — returned by `phaseForHost`/selectors when a console
 *  has no one-shot upload. MUST be a singleton (not a fresh `{kind:"idle"}`
 *  per call) or Zustand selectors comparing by `Object.is` would see a new
 *  object every render and loop. */
export const IDLE_PHASE: TransferPhase = { kind: "idle" };

interface TransferState {
  /** One-shot Upload-screen phase PER console, keyed by host (port-stripped
   *  via `hostOf`). Each console's one-shot upload is fully independent — a
   *  start() on console B never clobbers console A's live phase, abandons A's
   *  UI, or misattributes A's bytes. Absent host → idle (see `IDLE_PHASE`).
   *  Mirrors uploadQueue.ts's per-host model so the two surfaces behave the
   *  same with multiple consoles. */
  phasesByHost: Record<string, TransferPhase>;
  start: (args: StartArgs) => Promise<void>;
  /** Reset one console's one-shot phase to idle (pass its host), or — with no
   *  argument — every console's (the Activity tab's global Stop, which has no
   *  per-entry host today). */
  reset: (host?: string) => void;
}

const POLL_INTERVAL_MS = 500;
const POLL_INITIAL_DELAY_MS = 200;

/** Read one console's phase from a store snapshot. Falls back to the shared
 *  IDLE_PHASE singleton so selectors stay referentially stable. */
export function phaseForHost(
  s: TransferState,
  host: string | null | undefined,
): TransferPhase {
  // Empty/absent host → "" sentinel key. The Upload screen stores its
  // "set your PS5's IP first" pre-host error under "" so it can still render
  // when no console is selected; everything else keys by the real host.
  const key = host?.trim() ? hostFromAddr(host) : "";
  return s.phasesByHost[key] ?? IDLE_PHASE;
}

export const useTransferStore = create<TransferState>((set) => {
  // Per-host generation counters + poll timers. Every start() for a host bumps
  // THAT host's counter; the in-flight poll closure captures `thisRun` and
  // pre-gates every state write on `gen.isLive(thisRun)`. This kills the race
  // where a stale poll's `await jobStatus()` resolves after a new run started
  // and writes stale "running" over the new "starting" — now scoped per
  // console so consoles don't cancel each other. (Same shape as uploadQueue's
  // per-host `hostGen` map.) Orphan timers are cleared per host on reset/restart
  // so a rapid start/cancel loop on one console can't leak pending timeouts.
  const gens = new Map<string, ReturnType<typeof createRunGen>>();
  const pollTimers = new Map<string, ReturnType<typeof setTimeout>>();
  const genFor = (key: string) => {
    let g = gens.get(key);
    if (!g) {
      g = createRunGen();
      gens.set(key, g);
    }
    return g;
  };
  const clearTimerFor = (key: string) => {
    const t = pollTimers.get(key);
    if (t !== undefined) {
      clearTimeout(t);
      pollTimers.delete(key);
    }
  };
  // Write ONE host's phase, leaving every other console's untouched.
  const setPhase = (key: string, phase: TransferPhase) =>
    set((s) => ({ phasesByHost: { ...s.phasesByHost, [key]: phase } }));

  return {
    phasesByHost: {},

    async start({
      sourceKind,
      srcPath,
      dest,
      addr,
      strategy = "overwrite",
      reconcileMode = "fast",
      excludes = [],
      mountAfterUpload = false,
      mountReadOnly = true,
      registerAfterUpload = false,
    }) {
      // Per-console key: everything below (gen, poll timer, phase write) is
      // scoped to this host so a concurrent one-shot on another console runs
      // fully in parallel.
      const key = hostFromAddr(addr);
      const gen = genFor(key);
      const thisRun = gen.next();
      // A previous run on THIS host (rapid re-click) leaves a pending timer;
      // clear it so it doesn't fire after the new run takes over.
      clearTimerFor(key);
      setPhase(key, { kind: "starting" });

      const isFolder = sourceKind === "folder" || sourceKind === "game-folder";
      // A zip archive is a multi-file transfer like a folder, so it carries a
      // tx_id for cross-session shard resume (worth it for big dumps over
      // flaky wifi) — but it has no reconcile mode (there's no local folder
      // to diff against), so it always routes through startTransferZip.
      const isArchive = sourceKind === "archive";

      // Helper: is this run still the live one? If the user cancelled
      // or kicked off another upload, we're stale and MUST NOT write
      // state (would clobber whatever the new run set up).
      const isLive = () => gen.isLive(thisRun);

      // ── Cross-session tx_id resolution ─────────────────────────────
      // For folder uploads we carry the tx_id ourselves so a Resume
      // click (even after an app restart, within the 24 h TTL) reuses
      // the same tx and the payload's journal can surface
      // last_acked_shard. For file uploads, engine mints a fresh tx_id
      // each time — no Resume UX on single files today, so persisting
      // a tx_id would just create surprise skip-behavior on re-upload.
      const host = hostFromAddr(addr);
      // Friendly name for the PS5-side toasts (start + complete).
      const uploadName =
        srcPath
          .replace(/[\\/]+$/, "")
          .split(/[\\/]/)
          .pop() || "files";
      let txId: string | null = null;
      if (isFolder || isArchive) {
        const persistedMode =
          isFolder && strategy === "resume" ? "reconcile" : "dir";
        if (strategy === "overwrite") {
          // Override means "start from scratch" — drop any prior tx_id
          // so the next Resume click doesn't accidentally skip shards
          // from this fresh run's manifest.
          try {
            await resumeTxidForget(host, srcPath, dest);
          } catch (e) {
            // forget() failure is not worth failing the upload over
            // (worst case: an orphan record with a stale tx_id,
            // pruned by TTL within 24 h). But a persistent failure
            // here means cross-session Resume silently breaks
            // forever — log so a user grepping the dev console
            // can find it.
            console.warn("[transfer] resumeTxidForget failed:", e);
          }
          txId = generateTxIdHex();
        } else {
          // strategy === "resume": look up a prior tx_id for this
          // (host, src, dest). Missing or expired → fresh id.
          try {
            txId = await resumeTxidLookup(host, srcPath, dest);
          } catch (e) {
            console.warn(
              "[transfer] resumeTxidLookup failed; treating as fresh upload:",
              e,
            );
            txId = null;
          }
          if (!txId) txId = generateTxIdHex();
        }
        try {
          await resumeTxidRemember(host, srcPath, dest, txId, persistedMode);
        } catch (e) {
          // Persistence failure degrades UX (no cross-session
          // resume) but doesn't break the upload itself — still
          // pass the tx_id in-memory so within-session resume via
          // the engine's retry loop still fires. Log so a
          // permanent failure (corrupt store, perms) surfaces.
          console.warn("[transfer] resumeTxidRemember failed:", e);
          log.warn(
            "upload",
            "resumeTxidRemember failed (cross-session resume degraded)",
            e,
          );
        }
      }

      log.info(
        "upload",
        `start "${uploadName}" → ${dest} [${sourceKind}, ${strategy}${txId ? `, tx=${txId.slice(0, 8)}` : ""}]`,
      );

      // Read bandwidth cap at the moment we start the upload — the
      // engine only honors it at start-time (per-job config). The cap
      // setting was previously wired into the UI but never threaded
      // through here, so users believed throttling was active when
      // it wasn't.
      const bandwidthCap = useUploadSettingsStore.getState().bandwidthCapMbps;
      // Resolve parallel streams once at start (min of user setting +
      // payload's advertised max). Only the reconcile/resume folder path
      // is multi-stream today; everything else stays single-stream.
      const streams = effectiveUploadStreams(addr);
      let jobId: string;
      try {
        if (isFolder && strategy === "resume") {
          jobId = await startTransferDirReconcile(
            srcPath,
            dest,
            addr,
            reconcileMode,
            txId,
            excludes,
            bandwidthCap,
            streams,
          );
        } else if (isFolder) {
          jobId = await startTransferDir(
            srcPath,
            dest,
            addr,
            txId,
            excludes,
            bandwidthCap,
          );
        } else if (isArchive) {
          // .zip → host deflate; .7z → host LZMA2; .rar → host UnRAR extract.
          const fmt = archiveFormat(srcPath);
          if (fmt === "rar") {
            // RAR carries an optional password (held in the upload store for
            // the current pick; never persisted).
            const pw = useUploadStore.getState().rarPassword;
            jobId = await startTransferRar(
              srcPath,
              dest,
              addr,
              pw,
              txId,
              excludes,
              bandwidthCap,
            );
          } else {
            const start =
              fmt === "7z" ? startTransfer7z : startTransferZip;
            jobId = await start(srcPath, dest, addr, txId, excludes, bandwidthCap);
          }
        } else {
          jobId = await startTransferFile(srcPath, dest, addr);
        }
      } catch (e) {
        if (!isLive()) return;
        const msg = e instanceof Error ? e.message : String(e);
        log.error("upload", `start failed for "${uploadName}": ${msg}`);
        setPhase(key, { kind: "failed", error: msg });
        return;
      }
      if (!isLive()) return;

      const startedAtMs = Date.now();
      setPhase(key, {
        kind: "running",
        jobId,
        startedAtMs,
        bytesSent: 0,
        totalBytes: 0,
        bytesPerSec: 0,
        files: [],
        filesCompleted: 0,
        skippedFiles: 0,
        skippedBytes: 0,
        filesFinalized: 0,
        filesFinalizingTotal: 0,
        bytesFinalized: 0,
      });

      // Tell the PS5 itself that an upload started — a styled toast on the
      // console UI. Best-effort over the mgmt port (responsive even during
      // a transfer); a matching "complete" toast fires on done.
      if (host) {
        void toastPush(mgmtAddr(host), "Upload started", {
          subtitle: uploadName,
        }).catch(() => {});
      }

      // Trailing-window rate smoother. Shared with the queue runner via
      // lib/rollingRate so the two surfaces never disagree on what
      // "smoothed bytes/sec" means.
      const samples: RateSample[] = [{ ts: startedAtMs, bytes: 0 }];

      const poll = async () => {
        if (!isLive()) return;
        let snap: JobSnapshot;
        try {
          snap = await jobStatus(jobId);
        } catch (e) {
          if (!isLive()) return;
          const msg = e instanceof Error ? e.message : String(e);
          log.error(
            "upload",
            `transfer poll failed for "${uploadName}" (job ${jobId}): ${msg}`,
          );
          setPhase(key, { kind: "failed", error: msg });
          return;
        }
        if (!isLive()) return;
        if (snap.status === "done") {
          let mountedAt: string | undefined;
          const mountWarnings: string[] = [];
          const finalDest = snap.dest ?? dest;
          // Re-check liveness before initiating the mount. Same race
          // fix as uploadQueue.ts: Stop / reset between done-snapshot
          // and fsMount would otherwise leave a real mount on the
          // PS5 with the UI showing nothing. Skip-and-return keeps
          // user-visible state honest.
          if (isLive() && sourceKind === "image" && mountAfterUpload) {
            // ShadowMount+ hand-off (same as the queue path, uploadQueue.ts):
            // when SMP is running it OWNS mount + register, so hand the image
            // off via its watched manual.lst and SKIP our own mount — no
            // double-mount race for /user/app + app.db. Best-effort: fall back
            // to the native mount below if SMP is unreachable.
            const mgmt = mgmtAddr(hostFromAddr(addr));
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
              handedToSmp = false;
            }
            // Only mount it ourselves when SMP didn't take it.
            if (!handedToSmp)
              try {
                // Mount point lives next to the source file: strip the
                // image extension from finalDest. So an upload to
                // `/data/homebrew/MyGame.ffpkg` mounts at
                // `/data/homebrew/MyGame/`. Source + mount in the same
                // conventional folder — both discoverable by third-party
                // PS5 game scanners that walk /data/homebrew/.
                // The payload auto-derives the same name when mountPoint
                // is omitted but lands it under /mnt/ps5upload/<name>;
                // setting mountPoint explicitly keeps it where the user
                // dropped the file.
                const mountPoint = finalDest.replace(
                  /\.(exfat|ffpkg|ffpfs)$/i,
                  "",
                );
                const mounted = await fsMount(addr, finalDest, {
                  mountPoint,
                  readOnly: mountReadOnly,
                });
                mountedAt = mounted.mount_point;
                // Surface non-fatal mount diagnostics — same warnings
                // the Library row's manual Mount button shows. Pre-2.2.52
                // this single-shot upload path swallowed them silently
                // when `mountAfterUpload` was on.
                if (mounted.layout_valid === false) {
                  mountWarnings.push(
                    "Image is missing sce_sys/param.json at root — Register/Launch will fail. Re-build the image with files at root (no extra folder).",
                  );
                }
                if (mounted.kernel_ro && !mountReadOnly) {
                  mountWarnings.push(
                    "Kernel mounted this read-only despite the RW pick — common for UFS .ffpkg images on some firmwares. Reads work; writes through the mount will fail.",
                  );
                }
                // Stop / reset / new-run can land between fsMount
                // completing and the set({phase:"done"}) below. Two
                // bad shapes if we just `return` here:
                //   1) UI flips to idle/failed via stop handler but the
                //      mount is real on the PS5 — divergent state.
                //   2) (worse — 2.9.0 race-audit finding F2) a second
                //      Upload kicked off with DIFFERENT mountReadOnly
                //      races the first's in-flight fsMount; the older
                //      mount may land second and silently win the same
                //      mount point, leaving the user with RW when they
                //      asked for RO (or vice versa) — irreversible save-
                //      corruption window for the wrong-direction case.
                // Mitigation: best-effort unmount the superseded mount
                // before returning. Drop errors — the most common
                // failure is "already gone" from the new run's own
                // unmount-and-remount, which is exactly what we want.
                if (!isLive()) {
                  if (mountedAt) {
                    await fsUnmount(addr, mountedAt).catch(() => {});
                  }
                  return;
                }
              } catch (e) {
                if (!isLive()) return;
                setPhase(key, {
                  kind: "failed",
                  error: `upload completed, but mount failed: ${
                    e instanceof Error ? e.message : String(e)
                  }`,
                });
                return;
              }
          }
          // Register-after-upload: put the game on the PS5 home screen
          // right here instead of making the user discover the Library's
          // overflow-menu Register. Best-effort by design — the upload
          // already committed, so a registration failure must NOT turn the
          // done card into a failure; it lands as `registerWarning` and
          // the user can register manually from the Library. Mirrors the
          // Library flow's DRM-type-patch retry for the firmwares where
          // the plain register is rejected.
          let registeredAs: string | undefined;
          let registerWarning: string | undefined;
          if (isLive() && sourceKind === "game-folder" && registerAfterUpload) {
            try {
              let res;
              try {
                res = await appRegister(addr, finalDest);
              } catch {
                res = await appRegister(addr, finalDest, {
                  patchDrmType: true,
                });
              }
              registeredAs = res.title_name?.trim()
                ? res.title_name
                : res.title_id;
              log.info(
                "upload",
                `registered "${uploadName}" as ${res.title_id} after upload`,
              );
            } catch (e) {
              registerWarning = `Couldn't add it to the home screen automatically: ${
                e instanceof Error ? e.message : String(e)
              }. You can still do it from the Library.`;
            }
            // A Stop that landed during the register round-trip: the phase
            // writes below are gated on isLive(), so state stays honest —
            // the registration itself is already durable on the PS5 either
            // way (same contract as a manual Library register).
            if (!isLive()) return;
          }
          // Tx is committed on the payload — the tx_id we persisted at
          // start can be evicted, since there's nothing to resume. A
          // lingering record would make a subsequent upload-of-same-
          // source's Resume click look up a committed tx_id, which the
          // payload's journal may or may not still carry (depending on
          // eviction pressure). Forget proactively. Archives persist a
          // resume tx_id at start too (the remember/lookup block is
          // gated on `isFolder || isArchive`), so they must be evicted
          // here as well — otherwise a re-upload of the same .zip
          // "resumes" against a committed tx.
          if (isFolder || isArchive) {
            void resumeTxidForget(host, srcPath, dest);
          }
          // Persist this host's measured throughput so the next upload
          // to the same PS5 can render a sharper ETA in the pre-flight
          // banner (lib/uploadEta). Only record on real completion —
          // failed or stopped uploads are not representative samples.
          // commitMsPerFile is left undefined for now; deriving it
          // requires the per-file APPLY_PROGRESS frames coming in P3
          // (design doc §4). Once those land, this is the natural
          // recording site.
          //
          // Known bias: `snap.elapsed_ms` is the engine's *full*
          // elapsed time, which INCLUDES the post-100% PS5 commit
          // phase. For multi-file uploads (e.g. a 169k-file folder
          // with ~20 min of apply on top of ~30 min transfer) this
          // under-reports raw throughput by ~40%. The next upload's
          // banner will then over-estimate transfer time — which is
          // the conservative direction (over-promise wait, not
          // under-promise) so the bias is acceptable shipped. Proper
          // fix lands with P3, when APPLY_PROGRESS gives us a clean
          // apply-start timestamp to subtract. See review notes
          // surface C1.
          const recordedBytes = snap.bytes_sent ?? 0;
          const recordedElapsedMs = snap.elapsed_ms ?? 0;
          if (recordedBytes > 0 && recordedElapsedMs > 0) {
            const throughputMibps =
              recordedBytes / 1024 / 1024 / (recordedElapsedMs / 1000);
            recordHostMetrics(addr, {
              throughputMibps,
              measuredAtMs: Date.now(),
            });
          }
          setPhase(key, {
            kind: "done",
            jobId,
            bytesSent: snap.bytes_sent ?? 0,
            elapsedMs: snap.elapsed_ms ?? 0,
            dest: finalDest,
            filesSent: snap.files_sent ?? 0,
            skippedFiles: snap.skipped_files ?? 0,
            skippedBytes: snap.skipped_bytes ?? 0,
            mountedAt,
            mountWarnings: mountWarnings.length > 0 ? mountWarnings : undefined,
            registeredAs,
            registerWarning,
          });
          log.info(
            "upload",
            `done "${uploadName}" → ${finalDest}: ${snap.files_sent ?? 0} files, ${snap.bytes_sent ?? 0} bytes in ${snap.elapsed_ms ?? 0}ms${mountedAt ? `, mounted ${mountedAt}` : ""}`,
          );
          // Matching "complete" toast on the PS5 itself.
          if (host) {
            void toastPush(mgmtAddr(host), "Upload complete", {
              subtitle: uploadName,
            }).catch(() => {});
          }
        } else if (snap.status === "failed") {
          log.error(
            "upload",
            `engine reported failure for "${uploadName}" (job ${jobId}): ${snap.error ?? "upload failed"}`,
          );
          setPhase(key, {
            kind: "failed",
            error: snap.error ?? "upload failed",
          });
        } else {
          const now = Date.now();
          const bytesSent = snap.bytes_sent ?? 0;
          pushRateSample(samples, now, bytesSent);
          const bytesPerSec = computeRate(samples, now);
          const files = snap.files ?? [];
          // Cumulative-sum the file sizes to find how many have been
          // "completed" — defined as: their cumulative end byte is at
          // or below the bytes accounted for. Close enough for UI: on
          // packed shards, a burst of files all flip to ✓ at once when
          // the shard commits, which is honest.
          //
          // On a resume/reconcile upload, `files` is the FULL planned
          // list but `bytesSent` counts only the delta actually streamed;
          // already-present files are tracked separately in skippedBytes.
          // Add skippedBytes to the threshold so the head-of-list skipped
          // files count as done — otherwise the "N of M files" readout
          // stalls during exactly the resume flow this is meant for.
          const skippedBytes = snap.skipped_bytes ?? 0;
          const accountedBytes = bytesSent + skippedBytes;
          let cum = 0;
          let derivedFilesCompleted = 0;
          for (const f of files) {
            cum += f.size;
            if (cum <= accountedBytes) derivedFilesCompleted++;
            else break;
          }
          // Prefer the engine's per-file counter when it's reporting
          // (climbs as each source file is pulled into a pack frame).
          // The bytes-derived estimate jumps by ~200 every packed-shard
          // ACK and looked frozen between bursts on 46k-file game
          // folders. Clamp to files.length so a brief over-count (eg.
          // engine in mid-pack-build while we just hydrated a stale
          // snap.files) never renders ">N of N". 0 means the engine
          // path doesn't report it (single-file uploads, plain
          // transfer_dir without a progress_files atomic) — fall back
          // to the derived value.
          const engineFiles = snap.files_processing ?? 0;
          const filesCompleted =
            engineFiles > 0
              ? Math.min(engineFiles, files.length)
              : derivedFilesCompleted;
          setPhase(key, {
            kind: "running",
            jobId,
            startedAtMs,
            bytesSent,
            totalBytes: snap.total_bytes ?? 0,
            bytesPerSec,
            files,
            filesCompleted,
            skippedFiles: snap.skipped_files ?? 0,
            skippedBytes: snap.skipped_bytes ?? 0,
            filesFinalized: snap.files_finalized ?? 0,
            filesFinalizingTotal: snap.files_finalizing_total ?? 0,
            bytesFinalized: snap.bytes_finalized ?? 0,
          });
          pollTimers.set(key, setTimeout(poll, POLL_INTERVAL_MS));
        }
      };
      pollTimers.set(key, setTimeout(poll, POLL_INITIAL_DELAY_MS));
    },

    reset(host) {
      // Bump the generation(s) so any in-flight poll's next state write
      // becomes a no-op. Engine job keeps running in the background —
      // the payload's single-client transfer port (per console) means a
      // brand-new upload to that console waits until this one finishes.
      // Wiring a real ABORT_TX cancel is a separate follow-up.
      if (host !== undefined) {
        // Reset just this console's one-shot.
        const key = hostFromAddr(host);
        genFor(key).next();
        clearTimerFor(key);
        setPhase(key, { kind: "idle" });
        return;
      }
      // No host: reset EVERY console's one-shot (Activity-tab global Stop,
      // which has no per-entry host). Bump every live gen so all in-flight
      // polls go no-op, clear every timer, and drop all phases to idle.
      for (const g of gens.values()) g.next();
      for (const t of pollTimers.values()) clearTimeout(t);
      pollTimers.clear();
      set({ phasesByHost: {} });
    },
  };
});

/** Pull the host (no port) out of an addr like `192.168.1.2:9113` for
 *  use as the resume-txid cache key. We deliberately key by host, not
 *  full addr, so the port choice doesn't fragment records — a user who
 *  later lands on a payload with a different transfer port should still
 *  be able to resume, because the payload's tx journal is port-agnostic.
 *
 *  2.12.0: migrated to canonical `hostOf` from lib/addr. Behaviour
 *  difference: old `hostFromAddr` used `lastIndexOf`, new `hostOf`
 *  uses `indexOf` — collapses a double-suffix `ip:port:port` to the
 *  leftmost colon. For resume-txid lookup that's strictly safer (a
 *  legacy double-suffix record won't fragment further). */
import { hostOf as hostFromAddr, mgmtAddr } from "../lib/addr";
