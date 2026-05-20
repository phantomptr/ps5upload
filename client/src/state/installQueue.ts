import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { bundledPayloadPath, payloadCheck, sendPayload } from "../api/ps5";
import { compareVersions } from "../lib/semver";
import { isNpxsContentId } from "../lib/npxs";
import { stagingBasename } from "../lib/pkgStagingPath";
import {
  useInstallSettingsStore,
  type InstallMethod,
} from "./installSettings";

/**
 * Install Package queue. Sequential, like uploadQueue — one BGFT
 * install at a time on the PS5 because Sony's BGFT service serializes
 * concurrent registers internally and we don't want the second one
 * to deadlock against the first.
 *
 * Persisted to localStorage (the user's input items) so a queue
 * survives an app restart. Status fields are NOT persisted — on boot
 * we reset everything to "pending" because we don't know whether a
 * previously-running BGFT task is still alive on the PS5.
 *
 * Runner pattern matches uploadQueue.ts:
 *   - generation-counted via `runId`; stop() bumps it
 *   - the worker loop awaits between async calls and re-checks runId
 *   - per-item progress is polled from the engine every 1 s
 */

export type InstallStatus =
  | "pending"
  | "running"
  | "done"
  | "failed"
  | "cancelled";

export type InstallPhase =
  | "idle"
  | "staging" // Tier 1: uploading pkg bytes to PS5 staging dir before install
  | "queued"
  | "download"
  | "install"
  | "done"
  | "error";

export interface InstallQueueItem {
  id: string;
  /** Absolute path of the lead .pkg. For split-pkg sets, the
   *  `<base>.pkg` file. */
  pkgPath: string;
  /** True if `pkgPath` is the lead of a split-pkg set; engine will
   *  auto-detect siblings (`.0`, `.1`, ...). */
  isSplit: boolean;
  /** Display name — basename of pkgPath, or the parsed title when
   *  available. Set at parse-time for nicer rows. */
  displayName: string;
  /** Parsed metadata (set after first parse). content_id, size, etc. */
  contentId: string;
  totalBytes: number;
  /** BGFT package_type (PS4GD/PS4AC/...) — derived from PARAM.SFO
   *  or set explicitly via the override picker. */
  packageType: string;
  /** PS5 mgmt-port addr ("ip:9114"). Captured at add-time so a
   *  later connection-tab change doesn't redirect old queue items. */
  addr: string;
  status: InstallStatus;
  phase: InstallPhase;
  bytesDownloaded: number;
  errCode: number;
  errMessage: string | null;
  /** Set after install/start succeeds — used for status polling. */
  sessionId: string | null;
  taskId: number | null;
  addedAt: number;
  startedAt: number | null;
  finishedAt: number | null;
  /** Warnings from the parse step (e.g. unknown magic). Surfaced as
   *  a yellow caution row before install starts. */
  warnings: string[];
  /** How this item is being installed.
   *  - "stream" (DPI 2.0): the desktop engine serves the .pkg over
   *    HTTP with Range; BGFT on the PS5 streams + installs in one
   *    pass. No 2× disk space, no upload step, native pause/resume.
   *    This is the default for new items.
   *  - "stage" (legacy Tier-1): upload the .pkg to PS5 disk first,
   *    then BGFT installs from that local file. 2× space required;
   *    used when stream mode is unreachable (PS5 can't see the
   *    desktop's HTTP port — segregated VLAN, host firewall).
   *  Recorded on the item so toggling the default mid-install
   *  doesn't switch what's already in flight. The retry path
   *  preserves the original choice unless the user explicitly
   *  re-queues. */
  installMethod: InstallMethod;
  /** 2.2.52 Tier-1 staging: PS5-side absolute path the pkg was
   *  uploaded to before install. Set after the staging upload succeeds
   *  and the install request fires with `local_ps5_path = stagingPath`.
   *  Null when Tier-2 (HTTP) was used (split sets, or staging dir
   *  unavailable). Engine deletes this file after the install
   *  terminates; the payload also sweeps stale files older than 24 h
   *  on startup. Surfaced in the diag <details> so the user can verify
   *  the file via Files tab if anything weird happens. */
  stagingPath: string | null;
  /** Bytes uploaded so far during the staging step. Drives the pre-
   *  install progress bar. Resets to 0 once staging completes. */
  stagingBytes: number;
  /** 2.2.52 install-start diagnostics. Recorded on the queue item the
   *  first time the engine returns them so the user can expand a
   *  "Why did this fail?" row even after the install attempt is over.
   *  All fields default to null/empty for back-compat with rows
   *  created against pre-2.2.52 payloads. */
  diag: {
    /** Which BGFT Register variant the payload used / would use.
     *  "intdebug" — fakepkg-friendly BGFT path; cred-elevation works.
     *  "regular"  — entitlement-checked BGFT path; fakepkgs likely fail.
     *  "appinst"  — sceAppInstUtilInstallByPackage succeeded (Sony's
     *               high-level install API; bypasses our direct BGFT
     *               calls but ends up in the same Sony installer).
     *  "none"     — Register hasn't been attempted yet, or BGFT is
     *               unavailable on this firmware. */
    registerPath: "shellui-rpc" | "intdebug" | "regular" | "appinst" | "none" | "";
    /** Whether the IntDebug Register symbol resolved at payload init.
     *  False = fakepkg installs effectively unsupported on this FW. */
    intdebugAvail: boolean;
    /** Whether the payload's process-wide ucred elevation succeeded. */
    kernelRw: boolean;
    /** Per-tier err codes from the most recent install attempt.
     *  null  — tier wasn't attempted (no diag from payload yet, or
     *          older payload that doesn't emit the field).
     *  0     — tier completed without error.
     *  other — Sony err_code (or our 0xE000_xxxx machinery error).
     *  Lets the user see where Tier 1 / Tier 2 actually broke even
     *  when registerPath="none" (no tier succeeded). Added 2.2.52-fix. */
    shelluiErr: number | null;
    appinstErr: number | null;
  };
}

interface InstallQueueState {
  items: InstallQueueItem[];
  runId: number;
  isRunning: boolean;
  /** True once `hydrate()` has loaded persisted items from localStorage.
   *  Guards against a second hydrate call (e.g. when InstallPackage
   *  re-mounts after a tab switch) overwriting live runtime state —
   *  the persisted snapshot strips `status: "running"` → `"pending"`,
   *  `phase` → `"idle"`, `bytesDownloaded` → 0, etc. */
  _hydrated: boolean;

  add(item: Omit<InstallQueueItem, "id" | "addedAt" | "status" | "phase" |
    "bytesDownloaded" | "errCode" | "errMessage" | "sessionId" | "taskId" |
    "startedAt" | "finishedAt" | "diag" | "stagingPath" | "stagingBytes" |
    "installMethod"> & { installMethod?: InstallMethod }): void;
  remove(id: string): void;
  clearFinished(): void;
  retry(id: string): void;

  retryWith(id: string, installMethod: InstallMethod): void;
  start(): Promise<void>;
  stop(): void;
  cancel(id: string): Promise<void>;

  hydrate(): void;
}

const STORAGE_KEY = "ps5upload.install_queue.v1";

function persistNow(items: InstallQueueItem[]) {
  try {
    const persisted = items.map((it) => ({
      ...it,
      // Don't carry transient runtime state across restarts — we
      // can't trust a previously-"running" status because the BGFT
      // task it referred to may be gone.
      status: it.status === "running" ? "pending" : it.status,
      phase: "idle" as InstallPhase,
      bytesDownloaded: 0,
      sessionId: null,
      taskId: null,
      startedAt: null,
      finishedAt: it.status === "done" ? it.finishedAt : null,
      // Don't carry an in-flight staging path across restarts — the
      // payload's 24h sweep may have removed the file. The worker's
      // staging step re-uploads on next start. `stagingBytes` is pure
      // UI progress, never relevant after a restart.
      stagingPath:
        it.status === "done" || it.status === "failed"
          ? it.stagingPath
          : null,
      stagingBytes: 0,
    }));
    localStorage.setItem(STORAGE_KEY, JSON.stringify(persisted));
  } catch {
    // localStorage can throw on quota; the queue still works in
    // memory, the user just loses persistence for this session.
  }
}

// Debounced persist. The runner polls status at 1Hz and would
// otherwise call persist() on every tick — for a multi-hour install
// across a queue of 10 items, that's thousands of redundant
// JSON.stringify + localStorage writes. We coalesce them into a
// single write at most every 500ms.
//
// Terminal-state transitions (markDone / markFailed / markCancelled,
// add / remove / clearFinished, retry) MUST persist immediately so
// a sudden app exit between debounce delay and write doesn't lose
// the user's queue state. Those call sites use `persist()` directly,
// which flushes any pending debounced write first.
let persistTimer: ReturnType<typeof setTimeout> | null = null;
let persistPending: InstallQueueItem[] | null = null;

function persistDebounced(items: InstallQueueItem[]) {
  persistPending = items;
  if (persistTimer) return;
  persistTimer = setTimeout(() => {
    const snap = persistPending;
    persistPending = null;
    persistTimer = null;
    if (snap) persistNow(snap);
  }, 500);
}

/** Flush any pending debounced write + write `items` synchronously.
 *  Used at terminal-state transitions where losing the latest items
 *  to an app crash would orphan a session id in localStorage. */
function persist(items: InstallQueueItem[]) {
  if (persistTimer) {
    clearTimeout(persistTimer);
    persistTimer = null;
    persistPending = null;
  }
  persistNow(items);
}

function load(): InstallQueueItem[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as Partial<InstallQueueItem>[];
    // Defensive: drop anything that's missing required fields,
    // and back-fill new optional fields for items persisted by
    // older app versions (so an upgrade doesn't break hydration).
    return parsed
      .filter(
        (it): it is InstallQueueItem =>
          typeof it.id === "string" &&
          typeof it.pkgPath === "string" &&
          typeof it.addr === "string",
      )
      .map((it) => {
        // Strip the legacy `localPs5Path` (case-sensitive) field if a
        // row persisted from a 2.2.50–2.2.51 build still carries it.
        // The 2.2.52 staging flow uses `stagingPath` instead — same
        // semantic but tied to a per-install staging upload rather
        // than a user-supplied PS5-side path. Destructure-and-spread
        // drops the legacy field cleanly.
        const { localPs5Path: _legacy, ...rest } =
          it as InstallQueueItem & { localPs5Path?: unknown };
        // Back-fill 2.2.52 diagnostics. Be strict about shape — a
        // corrupted localStorage row with `diag: "oops"` (string),
        // `diag: 42` (number), or `diag: [...]` (array) used to slip
        // through a `?? defaults` check, then crash later on a
        // `.registerPath` access in InstallRow's <details> expander.
        // Validate object-shape explicitly and replace anything that
        // doesn't match with the empty default.
        const rawDiag = (it as { diag?: unknown }).diag;
        const diagOk =
          rawDiag !== null &&
          typeof rawDiag === "object" &&
          !Array.isArray(rawDiag);
        // Back-fill 2.2.52 staging fields. Persisted rows from 2.2.51
        // and earlier won't have them; default to null/0 so the
        // worker's pre-install staging step decides fresh whether
        // to stage. We don't carry an in-flight stagingPath across
        // restarts because the file may have been swept by the 24h
        // payload cleanup; safer to re-stage than to point install
        // at a missing file.
        const rawStagingPath = (rest as { stagingPath?: unknown }).stagingPath;
        const stagingPath =
          typeof rawStagingPath === "string" && rawStagingPath.length > 0
            ? rawStagingPath
            : null;
        // Back-fill `installMethod` for items persisted by 2.5.x or
        // earlier — those rows always staged (it was the only path).
        // We default un-tagged rows to "stage" rather than "stream" so
        // a retry of an already-completed-by-staging install doesn't
        // unexpectedly switch methods mid-life. Brand-new items added
        // after hydration get the user's current default via add().
        const rawMethod = (rest as { installMethod?: unknown }).installMethod;
        const installMethod: InstallMethod =
          rawMethod === "stream" ? "stream" : "stage";
        return {
          ...rest,
          diag: diagOk
            ? {
                ...(rawDiag as InstallQueueItem["diag"]),
                // Back-fill new 2.2.52-fix per-tier err fields if the
                // persisted row predates them — undefined on a row from
                // an older app version, defaulted here so the union
                // type is satisfied.
                shelluiErr:
                  (rawDiag as { shelluiErr?: unknown }).shelluiErr ===
                    null ||
                  typeof (rawDiag as { shelluiErr?: unknown }).shelluiErr ===
                    "number"
                    ? ((rawDiag as { shelluiErr?: number | null })
                        .shelluiErr ?? null)
                    : null,
                appinstErr:
                  (rawDiag as { appinstErr?: unknown }).appinstErr ===
                    null ||
                  typeof (rawDiag as { appinstErr?: unknown }).appinstErr ===
                    "number"
                    ? ((rawDiag as { appinstErr?: number | null })
                        .appinstErr ?? null)
                    : null,
              }
            : {
                registerPath: "",
                intdebugAvail: false,
                kernelRw: false,
                shelluiErr: null,
                appinstErr: null,
              },
          stagingPath,
          stagingBytes: 0,
          installMethod,
        };
      });
  } catch {
    return [];
  }
}

function newId(): string {
  // Random 9-char id; doesn't need to be cryptographically unique,
  // just stable + non-colliding within the queue.
  return Math.random().toString(36).slice(2, 11);
}

// 2.12.0 — local `bareIp` migrated to canonical `hostOf` from
// lib/addr. Identical behaviour, but consolidating to one source
// of truth so future "where do we strip ports?" greps find one
// answer.
import { hostOf as bareIp } from "../lib/addr";

/** Ensure the PS5 is running the same payload version that the
 *  desktop app bundles. Auto-pushes via the loader (port 9021) if
 *  the running payload is older or unreachable. Returns once the
 *  payload reports the expected version, or after a max-attempt
 *  poll budget — whichever comes first.
 *
 *  Why this lives in the install worker: the OLD UX required the
 *  user to manually click Connection → Send payload before every
 *  install, and forgetting that resulted in 0x80B22404 against the
 *  old payload no matter what install path the engine picked. The
 *  install screen has no way to know "user has the latest payload
 *  loaded" without probing, so we just probe + push as needed.
 *
 *  Returns:
 *    "current"   — already on the right version, no push needed.
 *    "pushed"    — push succeeded + payload booted with the new ver.
 *    "stale-ok"  — push succeeded but new version didn't appear in
 *                  poll window; install proceeds (payload may still
 *                  be a new-enough build that lacks the version
 *                  bump for some reason).
 *    "no-push"   — couldn't probe AND couldn't push (no PS5
 *                  reachable). Install will fail with its native
 *                  error; we don't synthesize a different one. */
async function ensurePayloadCurrent(
  host: string,
): Promise<"current" | "pushed" | "stale-ok" | "no-push"> {
  let appVersion: string;
  try {
    appVersion = await getVersion();
  } catch {
    // Can't read our own version — abort the auto-push entirely so
    // we don't accidentally push the wrong file. Install proceeds
    // with whatever the running payload is.
    return "no-push";
  }
  // Probe what's running.
  let running: string | null = null;
  try {
    const probe = await payloadCheck(host);
    if (probe.reachable) {
      running = probe.payloadVersion;
    }
  } catch {
    // payloadCheck threw — fall through to push attempt.
  }
  if (running && compareVersions(running, appVersion) === 0) {
    return "current";
  }
  // Need to push. Locate the bundled ELF + send it.
  let elfPath: string;
  try {
    elfPath = await bundledPayloadPath();
  } catch {
    return "no-push";
  }
  try {
    await sendPayload(host, elfPath);
  } catch {
    return "no-push";
  }
  // Poll up to ~30 s for the new payload to come up + report
  // matching version. ps5-payload-sdk's loader takes a few seconds
  // to gunzip + execute; the elevateUcred step takes a few more.
  // 1.5s initial + 1s × 28 = 29.5s budget total.
  await sleep(1500);
  for (let i = 0; i < 28; i++) {
    try {
      const probe = await payloadCheck(host);
      if (
        probe.reachable &&
        probe.payloadVersion &&
        compareVersions(probe.payloadVersion, appVersion) === 0
      ) {
        return "pushed";
      }
    } catch {
      // ignore; keep polling
    }
    await sleep(1000);
  }
  // Push went through, but the new version never showed up in the
  // poll window. Continue with the install — the new payload may
  // be running but reporting an unexpected version (e.g. user
  // sideloaded a different build during the wait).
  return "stale-ok";
}

export const useInstallQueue = create<InstallQueueState>((set, get) => ({
  items: [],
  runId: 0,
  isRunning: false,
  _hydrated: false,

  hydrate() {
    // One-shot: subsequent calls are no-ops. The InstallPackage screen
    // calls hydrate() in a useEffect that re-fires every time the
    // screen mounts (tab switch → unmount → mount on return). Without
    // this guard, the load() result (which strips `status:"running"`
    // → `"pending"`, `phase` → `"idle"`, `bytesDownloaded` → 0) wipes
    // out the in-flight progress every time the user comes back to
    // the tab mid-install.
    if (get()._hydrated) return;
    set({ items: load(), _hydrated: true });
  },

  add(input) {
    // Default installMethod from the settings store when the caller
    // doesn't specify it. We read at add-time (not at run-time) so a
    // queued item locks in the method the user picked when they added
    // it — toggling the default later doesn't retroactively switch
    // already-queued items.
    const installMethod: InstallMethod =
      input.installMethod ??
      useInstallSettingsStore.getState().installMethod;
    const item: InstallQueueItem = {
      ...input,
      installMethod,
      id: newId(),
      status: "pending",
      phase: "idle",
      bytesDownloaded: 0,
      errCode: 0,
      errMessage: null,
      sessionId: null,
      taskId: null,
      addedAt: Date.now(),
      startedAt: null,
      finishedAt: null,
      diag: {
        registerPath: "",
        intdebugAvail: false,
        kernelRw: false,
        shelluiErr: null,
        appinstErr: null,
      },
      stagingPath: null,
      stagingBytes: 0,
    };
    const items = [...get().items, item];
    set({ items });
    persist(items);
  },

  remove(id) {
    const items = get().items.filter((it) => it.id !== id);
    set({ items });
    persist(items);
  },

  clearFinished() {
    const items = get().items.filter(
      (it) =>
        it.status !== "done" &&
        it.status !== "failed" &&
        it.status !== "cancelled",
    );
    set({ items });
    persist(items);
  },

  retry(id) {
    const items = get().items.map((it) =>
      it.id === id
        ? {
            ...it,
            status: "pending" as InstallStatus,
            phase: "idle" as InstallPhase,
            bytesDownloaded: 0,
            errCode: 0,
            errMessage: null,
            sessionId: null,
            taskId: null,
            startedAt: null,
            finishedAt: null,
          }
        : it,
    );
    set({ items });
    persist(items);
  },

  retryWith(id, installMethod) {
    // Same as retry() but also switches the item's installMethod.
    // Used by the "Retry as staged install" fallback button on a
    // failed stream-mode row when the PS5 couldn't reach the
    // desktop's HTTP port (firewall, segregated VLAN). We clear the
    // staging marker too so the runner's pre-install staging step
    // re-decides fresh whether to stage (it will, since
    // installMethod is now "stage").
    const items = get().items.map((it) =>
      it.id === id
        ? {
            ...it,
            installMethod,
            status: "pending" as InstallStatus,
            phase: "idle" as InstallPhase,
            bytesDownloaded: 0,
            errCode: 0,
            errMessage: null,
            sessionId: null,
            taskId: null,
            startedAt: null,
            finishedAt: null,
            stagingPath: null,
            stagingBytes: 0,
          }
        : it,
    );
    set({ items });
    persist(items);
  },

  async cancel(id) {
    const it = get().items.find((x) => x.id === id);
    if (!it || !it.sessionId) {
      // Pending or no session yet — just drop to cancelled state.
      const items = get().items.map((x) =>
        x.id === id
          ? { ...x, status: "cancelled" as InstallStatus, finishedAt: Date.now() }
          : x,
      );
      set({ items });
      persist(items);
      return;
    }
    try {
      await invoke("pkg_install_cancel", { session: it.sessionId });
    } catch (e) {
      // Continue marking cancelled anyway — the user clicked cancel,
      // they want it gone from their queue regardless of whether
      // we successfully told the engine.
      console.warn("pkg_install_cancel failed:", e);
    }
    const items = get().items.map((x) =>
      x.id === id
        ? {
            ...x,
            status: "cancelled" as InstallStatus,
            finishedAt: Date.now(),
          }
        : x,
    );
    set({ items });
    persist(items);
  },

  stop() {
    // Bump runId — the worker loop sees this on its next await and
    // exits without advancing to the next pending item.
    set({ runId: get().runId + 1, isRunning: false });
  },

  async start() {
    if (get().isRunning) return;
    // Atomic increment-and-claim. Naive `read get().runId, +1, set
    // {runId: myRun, isRunning: true}` lets two interleaved start()
    // calls both compute the same myRun (the second's get() sees
    // pre-set state because zustand updates are sync-but-microtask).
    // The functional-set form forces a single observable transition.
    let myRun = -1;
    set((s) => {
      myRun = s.runId + 1;
      return { runId: myRun, isRunning: true };
    });

    const isLive = () => get().runId === myRun;

    // Auto-push the bundled payload to whichever PS5 the FIRST
    // pending item targets. If multiple items target different PS5s
    // (rare), each item's loop iteration re-checks below — but in
    // practice the queue is per-host. Skipping when no items exist.
    {
      const head = get().items.find((it) => it.status === "pending");
      if (head) {
        const host = bareIp(head.addr);
        try {
          await ensurePayloadCurrent(host);
        } catch (e) {
          // ensurePayloadCurrent already swallows expected errors;
          // a throw here is unexpected. Log + continue with install.
          console.warn("ensurePayloadCurrent threw:", e);
        }
        if (!isLive()) {
          set({ isRunning: false });
          return;
        }
      }
    }

    while (isLive()) {
      const next = get().items.find((it) => it.status === "pending");
      if (!next) break;

      // Mark running.
      {
        const items = get().items.map((it) =>
          it.id === next.id
            ? { ...it, status: "running" as InstallStatus, startedAt: Date.now() }
            : it,
        );
        set({ items });
        persist(items);
      }

      // ── Tier-1 staging upload (legacy "stage" install method) ───
      // Upload the pkg bytes to PS5-side staging dir before kicking
      // off the install. Sony's installer reads from local disk +
      // ShellUI-RPC fires the call from ShellUI's authid context —
      // this combo bypasses the FW 9.60 PlayGo HTTP-fetch reject
      // (0x80B22404) entirely.
      //
      // Skipped for:
      //   - DPI 2.0 (`installMethod === "stream"`): the desktop engine
      //     serves the .pkg over HTTP with Range support, BGFT streams
      //     + installs in one pass, no 2× disk space needed, pause/
      //     resume comes from BGFT natively. localPs5Path stays null
      //     → engine builds the HTTP URL in pkg_install_start.
      //   - Split-pkg sets: staging would double wire time on 50GB+
      //     uploads. The Tier-2 HTTP path is fine for split sets
      //     because the install call still originates from ShellUI.
      //   - Any case where the staging upload itself fails — we fall
      //     through with localPs5Path null (Tier-2 best-effort).
      let localPs5Path: string | null = null;
      if (!next.isSplit && next.installMethod === "stage") {
        // Name the staging file after the PKG's ContentID when we
        // have it. Sony's installer code paths key on basename for
        // some FW points — sonicloader's homebrew.c:436
        // (canonicalise_pkg_filename) discovered this empirically by
        // observing silent rejections of perfectly-valid PKGs whose
        // filename didn't match the embedded ContentID. Falls back
        // to `<queueId>_<ts>.pkg` when ContentID is missing/unsafe.
        const basename = stagingBasename(next.contentId, next.id, Date.now());
        const stagingPath = `/user/data/ps5upload/pkg_temp/${basename}`;
        // Mark phase=staging so the row's progress bar shows the
        // upload, not a stale "queued" state. bytesDownloaded gets
        // re-purposed via stagingBytes for the upload progress.
        {
          const items = get().items.map((it) =>
            it.id === next.id
              ? {
                  ...it,
                  phase: "staging" as InstallPhase,
                  stagingPath,
                  stagingBytes: 0,
                }
              : it,
          );
          set({ items });
          persist(items);
        }
        let stagedOk = false;
        try {
          // The install queue stores `addr` as the mgmt-port address
          // (`ip:9114`) because pkg_install_status uses the mgmt port.
          // transfer_file uses the bulk-data transfer port (`ip:9113`)
          // — same port the Upload screen's queue uses. Swap suffix
          // before kicking off staging or transfer_file connects to
          // the wrong port and the upload fails (silently, since the
          // mgmt port doesn't speak the transfer protocol — connection
          // gets accepted but BeginTx is rejected, status flips to
          // failed, my poll bails with stagedOk=false, install
          // proceeds with HTTP URL → 0x80B22404 on Tier 2).
          const transferAddr = `${bareIp(next.addr)}:9113`;
          const txResp = (await invoke("transfer_file", {
            req: {
              src: next.pkgPath,
              dest: stagingPath,
              addr: transferAddr,
              tx_id: null,
            },
          })) as { job_id?: string };
          const jobId = txResp.job_id;
          if (jobId) {
            // Poll job_status until done. Bail on cancel (runId bump).
            let polls = 0;
            while (isLive()) {
              await sleep(500);
              if (!isLive()) return;
              // JobState serializes with `tag="status"` — see
              // engine/main.rs:JobState. Variants: running/done/failed.
              let js: {
                status?: string;
                bytes_sent?: number;
                total_bytes?: number;
                error?: string | null;
              };
              try {
                js = (await invoke("job_status", { jobId })) as typeof js;
              } catch (e) {
                polls += 1;
                if (polls >= 5) {
                  console.warn("staging job_status poll failed:", e);
                  break;
                }
                continue;
              }
              polls = 0;
              if (typeof js.bytes_sent === "number") {
                const sent = js.bytes_sent;
                const items = get().items.map((it) =>
                  it.id === next.id ? { ...it, stagingBytes: sent } : it,
                );
                set({ items });
                persistDebounced(items);
              }
              if (js.status === "done") {
                stagedOk = true;
                break;
              }
              if (js.status === "failed") {
                console.warn("staging upload failed:", js.error);
                break;
              }
            }
          }
        } catch (e) {
          // transfer_file invocation itself errored — engine offline
          // or arg shape mismatch. Fall through to Tier-2.
          console.warn("staging transfer_file invoke failed:", e);
        }
        if (!isLive()) return;
        if (stagedOk) {
          localPs5Path = stagingPath;

          // 2.2.54-fix-round-10 — drain Sony's stuck task queue.
          //
          // Sony's installer holds onto file-path references for
          // every previously-registered task (even failed ones).
          // After ~3-5 successful registers for the same content_id,
          // Sony rejects new same-content_id registers with
          // 0x80B21106 ("duplicate task queued") because the prior
          // tasks haven't completed/errored out yet — they're stuck
          // referencing files we no longer care about.
          //
          // The fix discovered via direct probing: deleting the
          // stale staging files makes Sony's stuck tasks fail with
          // file-not-found, which Sony then clears from the queue.
          // After the deletes, new registers succeed cleanly.
          //
          // We delete EVERY .pkg in /user/data/ps5upload/pkg_temp/ that
          // ISN'T the file we just staged. The mgmt-port addr is
          // what fs_delete needs (`ip:9114`).
          try {
            const listing = (await invoke("ps5_list_dir", {
              path: "/user/data/ps5upload/pkg_temp",
              addr: next.addr,
            })) as { entries?: { name: string; kind: string }[] };
            const stagingBasename = stagingPath.replace(
              "/user/data/ps5upload/pkg_temp/",
              "",
            );
            for (const e of listing.entries ?? []) {
              if (e.kind !== "file") continue;
              if (e.name === stagingBasename) continue;
              if (!e.name.endsWith(".pkg")) continue;
              try {
                await invoke("ps5_fs_delete", {
                  req: {
                    path: `/user/data/ps5upload/pkg_temp/${e.name}`,
                    addr: next.addr,
                  },
                });
              } catch (e2) {
                console.warn(
                  `staging cleanup: fs_delete /user/data/ps5upload/pkg_temp/${e.name} failed`,
                  e2,
                );
              }
            }
            // Brief settle window so Sony's installer notices the
            // file-gone state and errors out the stale tasks before
            // we register the new one. ~2s empirically suffices.
            await sleep(2000);
            if (!isLive()) return;
          } catch (e) {
            // List failed — proceed without cleanup. The install may
            // still succeed if Sony's queue happens to be clean.
            console.warn("staging cleanup: list failed, skipping", e);
          }
        } else {
          // Tier-1 unavailable for this row; clear the staging
          // marker so the diag panel doesn't claim a path that
          // doesn't exist on the PS5.
          const items = get().items.map((it) =>
            it.id === next.id ? { ...it, stagingPath: null, stagingBytes: 0 } : it,
          );
          set({ items });
          persist(items);
        }
      }

      type StartResp = {
        session_id?: string;
        task_id?: number;
        err_code?: number;
        err_message?: string;
        detail?: string;
        register_path?: string;
        intdebug_avail?: boolean;
        kernel_rw?: boolean;
        shellui_err?: number | null;
        appinst_err?: number | null;
      };
      let startResp: StartResp = {};
      // 2.2.54-fix-round-7: retry policy refined. Direct testing
      // proved that 0x80B21106 on a same-content_id install means
      // "Sony already has a task queued for this content_id, won't
      // accept a duplicate" — NOT a transient queue-busy. Retrying
      // 0x80B21106 just pollutes Sony's queue further. Same goes
      // for 0x80B22404 (PlayGo HTTP-404 — process-context reject;
      // the underlying authid issue, not transient).
      // Only retry on 0x80020023 EAGAIN ("Sony's daemon momentarily
      // busy, try again in a few seconds"). 2 attempts × 5s.
      const RETRYABLE = new Set([0x80020023]);
      const MAX_ATTEMPTS = 2;
      const BACKOFF_MS = 5000;
      let attempt = 0;
      let lastInvokeError: unknown = null;
      while (attempt < MAX_ATTEMPTS) {
        attempt += 1;
        try {
          startResp = (await invoke("pkg_install_start", {
            ps5Addr: next.addr,
            path: next.isSplit ? null : next.pkgPath,
            splitRoot: next.isSplit ? next.pkgPath : null,
            packageTypeOverride: next.packageType || null,
            localPs5Path,
          })) as StartResp;
          lastInvokeError = null;
        } catch (e) {
          lastInvokeError = e;
          if (attempt < MAX_ATTEMPTS && isLive()) {
            await sleep(BACKOFF_MS);
            continue;
          }
          break;
        }
        if (!isLive()) return;
        const code = startResp.err_code ?? 0;
        if (code === 0) break;
        const tier1 = startResp.shellui_err ?? 0;
        const tier2 = startResp.appinst_err ?? 0;
        const anyRetryable =
          RETRYABLE.has(code) ||
          RETRYABLE.has(tier1) ||
          RETRYABLE.has(tier2);
        if (!anyRetryable || attempt >= MAX_ATTEMPTS) break;
        await sleep(BACKOFF_MS);
        if (!isLive()) return;
      }
      if (lastInvokeError !== null) {
        markFailed(get, set, next.id, 0, `${lastInvokeError}`);
        if (!isLive()) return;
        continue;
      }

      if (!isLive()) return;

      // Capture install-start diagnostics — recorded onto the item
      // whether the start succeeded or failed so the user can expand
      // a "Why?" disclosure on a failed row even after the fact.
      // Coerce register_path to the union; unknown values fall back
      // to empty string (renders as "—" in UI).
      const rawRegPath = startResp.register_path ?? "";
      const registerPath: InstallQueueItem["diag"]["registerPath"] =
        rawRegPath === "shellui-rpc" ||
        rawRegPath === "intdebug" ||
        rawRegPath === "regular" ||
        rawRegPath === "appinst" ||
        rawRegPath === "none"
          ? rawRegPath
          : "";
      const diag: InstallQueueItem["diag"] = {
        registerPath,
        intdebugAvail: startResp.intdebug_avail ?? false,
        kernelRw: startResp.kernel_rw ?? false,
        shelluiErr:
          typeof startResp.shellui_err === "number"
            ? startResp.shellui_err
            : null,
        appinstErr:
          typeof startResp.appinst_err === "number"
            ? startResp.appinst_err
            : null,
      };
      {
        const items = get().items.map((it) =>
          it.id === next.id ? { ...it, diag } : it,
        );
        set({ items });
        persist(items);
      }

      const sessionId = startResp.session_id ?? null;
      const taskId = startResp.task_id ?? null;
      const startErr = startResp.err_code ?? 0;
      if (startErr !== 0 || !sessionId || taskId === null) {
        // Augment the failure message with diagnostic context that
        // points at the most likely root cause.
        const baseMsg =
          startResp.err_message ||
          startResp.detail ||
          `BGFT register failed (0x${startErr.toString(16)})`;
        const hints: string[] = [];
        if (!diag.kernelRw) {
          hints.push(
            "kernel R/W not available — load via :9021 (ps5-payload-sdk loader)",
          );
        }
        if (!diag.intdebugAvail) {
          hints.push(
            "IntDebug Register symbol missing on this firmware — fakepkg install may not be supported",
          );
        }
        const suffix = hints.length ? ` — ${hints.join("; ")}` : "";
        markFailed(get, set, next.id, startErr, `${baseMsg}${suffix}`);
        continue;
      }

      // Record session_id + task_id so cancel can reach it.
      {
        const items = get().items.map((it) =>
          it.id === next.id ? { ...it, sessionId, taskId } : it,
        );
        set({ items });
        persist(items);
      }

      // 2.2.57: NPXS system-pkg fast-path. Sony accepted the register
      // (we have a task_id), but `sceAppInstUtilInstallByPackage`
      // wasn't designed for system patches — the mgmt service freezes
      // while Sony does its install work, every status poll fails,
      // and we'd hit pollErrors >= 5 and surface as "failed" even
      // though the install IS proceeding on the PS5 (the user can
      // confirm via the console's notification panel + Settings →
      // Notifications). System pkgs require fire-and-forget with
      // on-console verification, not status polling — Sony's API
      // isn't designed for system patches and the poll path is
      // unreliable for them.
      //
      // Path:
      //   - register accepts → sleep ~3s (let Sony's install start)
      //   - mark as "done" with a special message pointing the user
      //     at the PS5's notification panel
      //   - skip the poll loop entirely
      //
      // Game pkgs (CUSA / PPSA / PCSA / EP / UP / etc.) keep the
      // normal poll loop — they reliably surface phase=done via
      // `sceAppInstUtilGetInstallStatus` and the user gets real
      // progress.
      if (isNpxsContentId(next.contentId)) {
        await sleep(3000);
        if (!isLive()) return;
        markDone(get, set, next.id);
        continue;
      }

      // Poll until done or error.
      let phase: InstallPhase = "queued";
      let pollErrors = 0;
      while (isLive()) {
        await sleep(1000);
        if (!isLive()) return;
        let st: {
          phase?: InstallPhase;
          downloaded?: number;
          total?: number;
          err_code?: number;
          err_message?: string | null;
          detail?: string;
          cancelled?: boolean;
          // 2.2.52-fix-round-2: re-emitted on every status poll so a
          // mid-install BGFT transition (download → error) refreshes
          // the diag block instead of leaving it pinned at start-time.
          register_path?: string;
          intdebug_avail?: boolean;
          kernel_rw?: boolean;
          shellui_err?: number | null;
          appinst_err?: number | null;
        };
        try {
          st = (await invoke("pkg_install_status", {
            session: sessionId,
          })) as typeof st;
          pollErrors = 0;
        } catch (e) {
          // Tolerate transient poll errors (PS5 swap, brief network
          // hiccup) up to 5 in a row before giving up.
          pollErrors += 1;
          if (pollErrors >= 5) {
            markFailed(get, set, next.id, 0, `status poll failed: ${e}`);
            break;
          }
          continue;
        }
        if (!isLive()) return;
        // The user may have cancelled THIS item via cancel(), which sets its
        // status to "cancelled" locally but does not bump runId (that would
        // tear down the whole queue worker, not just this item). The engine's
        // pkg_install_status doesn't always report cancelled=true — cancel
        // may only stop host-side serving, or BGFT may complete on the PS5
        // anyway — so re-check our own status and stop without re-marking.
        // Without this, a finished/failed install overwrites the user's
        // "Cancelled" with "Done"/"Failed" and re-fires library-invalidate.
        const liveItem = get().items.find((x) => x.id === next.id);
        if (!liveItem || liveItem.status === "cancelled") break;
        phase = st.phase ?? phase;
        // Refresh the diag block from the live status response. If
        // the payload omits the fields (pre-2.2.52 firmware), keep
        // whatever we captured at install/start. Coerce
        // register_path through the union the type expects.
        const liveRegPath = st.register_path ?? "";
        const liveDiag: InstallQueueItem["diag"] | null =
          liveRegPath === "shellui-rpc" ||
          liveRegPath === "intdebug" ||
          liveRegPath === "regular" ||
          liveRegPath === "appinst" ||
          liveRegPath === "none"
            ? {
                registerPath: liveRegPath,
                intdebugAvail: st.intdebug_avail ?? false,
                kernelRw: st.kernel_rw ?? false,
                shelluiErr:
                  typeof st.shellui_err === "number"
                    ? st.shellui_err
                    : null,
                appinstErr:
                  typeof st.appinst_err === "number"
                    ? st.appinst_err
                    : null,
              }
            : null;
        const items = get().items.map((it) =>
          it.id === next.id
            ? {
                ...it,
                phase,
                bytesDownloaded: st.downloaded ?? it.bytesDownloaded,
                totalBytes: st.total ?? it.totalBytes,
                errCode: st.err_code ?? it.errCode,
                errMessage: st.err_message ?? it.errMessage,
                diag: liveDiag ?? it.diag,
              }
            : it,
        );
        set({ items });
        // Hot path — runs once per second per active install. Debounce
        // the localStorage write so we don't hammer storage. Terminal
        // transitions below (markDone/markFailed/markCancelled) call
        // `persist(items)` which flushes any pending debounced write,
        // so a sudden app exit doesn't lose the user's last status.
        persistDebounced(items);

        if (st.cancelled) {
          markCancelled(get, set, next.id);
          break;
        }
        if (phase === "done") {
          markDone(get, set, next.id);
          break;
        }
        if (phase === "error") {
          markFailed(
            get,
            set,
            next.id,
            st.err_code ?? 0,
            st.err_message ||
              st.detail ||
              `BGFT install failed (0x${(st.err_code ?? 0).toString(16)})`,
          );
          break;
        }
      }
      if (!isLive()) return;
    }

    set({ isRunning: false });
  },
}));

function markFailed(
  get: () => InstallQueueState,
  set: (s: Partial<InstallQueueState>) => void,
  id: string,
  errCode: number,
  errMessage: string,
) {
  const items = get().items.map((it) =>
    it.id === id
      ? {
          ...it,
          status: "failed" as InstallStatus,
          phase: "error" as InstallPhase,
          errCode,
          errMessage,
          finishedAt: Date.now(),
        }
      : it,
  );
  set({ items });
  persist(items);
}

function markDone(
  get: () => InstallQueueState,
  set: (s: Partial<InstallQueueState>) => void,
  id: string,
) {
  const items = get().items.map((it) =>
    it.id === id
      ? {
          ...it,
          status: "done" as InstallStatus,
          phase: "done" as InstallPhase,
          finishedAt: Date.now(),
        }
      : it,
  );
  set({ items });
  persist(items);

  // Tell the Library screen to refresh — a freshly-installed title
  // is now in /user/app/ and the user expects to see it the moment
  // they navigate to the Library tab. Library listens via
  // window.addEventListener("ps5upload:library:invalidate", ...).
  // CustomEvent works in browsers + Tauri webview without any extra
  // imports, and we don't need cross-window broadcasting since the
  // queue + Library live in the same renderer.
  if (typeof window !== "undefined") {
    try {
      window.dispatchEvent(new CustomEvent("ps5upload:library:invalidate"));
    } catch {
      /* noop — browsers/test envs without CustomEvent fall through */
    }
  }
}

function markCancelled(
  get: () => InstallQueueState,
  set: (s: Partial<InstallQueueState>) => void,
  id: string,
) {
  const items = get().items.map((it) =>
    it.id === id
      ? {
          ...it,
          status: "cancelled" as InstallStatus,
          finishedAt: Date.now(),
        }
      : it,
  );
  set({ items });
  persist(items);
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
