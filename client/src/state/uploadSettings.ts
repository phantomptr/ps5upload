import { create } from "zustand";
import type { ReconcileMode } from "../api/ps5";
import {
  safeGetItem,
  safeSetItem,
  safeRemoveItem,
} from "../lib/safeStorage";

/**
 * User preferences that govern the upload flow's defaults — what to do
 * when a destination already has content, and how strict the reconcile
 * should be when the user chooses "Resume".
 *
 * Persisted to localStorage so the settings survive restarts. Keyed on
 * a single namespace per concern — if the schema evolves, bump the key
 * (never silently migrate, the cost is one re-pick from the user).
 */

const KEY_ALWAYS_OVERWRITE = "ps5upload.always_overwrite";
const KEY_SHOW_FILES = "ps5upload.show_transfer_files";
const KEY_BANDWIDTH_CAP = "ps5upload.bandwidth_cap_mbps";
const KEY_UPLOAD_STREAMS = "ps5upload.upload_streams";
const KEY_AUTO_RESUME = "ps5upload.auto_resume";
const KEY_KEEP_PS5_AWAKE = "ps5upload.keep_ps5_awake";
const KEY_AUTO_REDEPLOY_ON_WAKE = "ps5upload.auto_redeploy_on_wake";

/** Upper bound on the user-selectable stream count, mirroring the engine's
 *  MAX_TRANSFER_STREAMS. The effective count is further clamped to whatever
 *  the connected payload advertises. */
export const MAX_UPLOAD_STREAMS = 4;

function loadAlwaysOverwrite(): boolean {
  if (typeof window === "undefined") return false;
  return safeGetItem(KEY_ALWAYS_OVERWRITE) === "true";
}

function loadReconcileMode(): ReconcileMode {
  // Safe mode was removed -- it hashed every remote file via BLAKE3
  // which took hours on large libraries and could destabilize the
  // payload. Per-shard BLAKE3 verification during actual upload
  // already catches mismatches, so size-only is a safe shortcut.
  // Ignore any stale "safe" in localStorage and always return "fast".
  return "fast";
}

function loadShowFiles(): boolean {
  if (typeof window === "undefined") return true;
  const v = safeGetItem(KEY_SHOW_FILES);
  // Default ON — most users want to see what's happening.
  return v === null ? true : v === "true";
}

function loadBandwidthCap(): number {
  if (typeof window === "undefined") return 0;
  const v = safeGetItem(KEY_BANDWIDTH_CAP);
  if (!v) return 0;
  const n = parseFloat(v);
  return isFinite(n) && n > 0 ? n : 0;
}

/** Default parallel upload streams for a fresh install.
 *
 *  1 (single stream = off). Multi-stream IS faster (hardware-validated
 *  2026-06-02: ~1.7× on Fat, ~1.4× on Pro) but drives N concurrent
 *  transactions against a payload whose memory/thread budget was tuned for
 *  ONE transaction at a time (see payload/include/config.h). On some consoles
 *  that sustained concurrency crashes the payload's transfer listener
 *  mid-upload — it then refuses connections and the user is stuck until they
 *  reload the payload (user-reported v2.24.0, 4-stream 74 GB folder, listener
 *  died ~7 min in). So we ship the rock-solid single-stream path by default
 *  and let users opt UP to 4 with an in-UI stability warning. */
const DEFAULT_UPLOAD_STREAMS = 1;

/** Clamp a requested stream count to the supported range. Rounds, floors at
 *  1 (zero/negative is meaningless), and caps at MAX_UPLOAD_STREAMS — more
 *  than the payload supports can crash it mid-upload. Pure, so it's unit-
 *  tested directly. */
export function clampUploadStreams(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_UPLOAD_STREAMS;
  return Math.min(Math.max(Math.round(n), 1), MAX_UPLOAD_STREAMS);
}

function loadUploadStreams(): number {
  // The effective count at upload time is min(setting, the payload's advertised
  // max_transfer_streams), so an older payload that predates multi-stream still
  // clamps to 1 regardless of this setting.
  if (typeof window === "undefined") return DEFAULT_UPLOAD_STREAMS;
  const v = safeGetItem(KEY_UPLOAD_STREAMS);
  if (!v) return DEFAULT_UPLOAD_STREAMS;
  const n = parseInt(v, 10);
  if (!Number.isFinite(n)) return DEFAULT_UPLOAD_STREAMS;
  return clampUploadStreams(n);
}

/** Keep-PS5-awake policy. The app periodically sends a power-tick
 *  (sceSystemServicePowerTick) that resets the console's auto-standby
 *  timer:
 *
 *    "off"       — never tick; the PS5 rests on its own schedule.
 *    "transfers" — tick only while an upload/queue item is running, so a
 *                  long transfer isn't killed by rest mode (the
 *                  spool_apply_failed failure). Default.
 *    "always"    — tick every connected console for as long as the app is
 *                  open and its helper is up, so the PS5 never auto-rests
 *                  while you're working with it. Manual rest from the
 *                  controller still works — the tick only resets the IDLE
 *                  timer, it doesn't block an explicit request.
 *
 *  Migration: the pre-v3 setting was a boolean under KEY_KEEP_PS5_AWAKE
 *  ("false" = disabled, absent = enabled). When the new mode key is absent
 *  we derive it from the old one so nobody's choice is lost. */
export type KeepPs5AwakeMode = "off" | "transfers" | "always";

const KEY_KEEP_PS5_AWAKE_MODE = "ps5upload.keep_ps5_awake_mode";

function loadKeepPs5AwakeMode(): KeepPs5AwakeMode {
  if (typeof window === "undefined") return "transfers";
  const v = safeGetItem(KEY_KEEP_PS5_AWAKE_MODE);
  if (v === "off" || v === "transfers" || v === "always") return v;
  // Legacy boolean: "false" meant disabled; absent/true meant
  // tick-during-transfers.
  return safeGetItem(KEY_KEEP_PS5_AWAKE) === "false"
    ? "off"
    : "transfers";
}

/** Auto-resume after a failed upload. Default ON: when a job fails for a
 *  recoverable reason (payload crashed, connection dropped), the runner waits
 *  a short backoff, re-deploys the payload if it's down, and resumes — bounded
 *  to a few attempts. Fatal errors (out of space, etc.) still surface at once.
 *  Stored as the string "false" only when the user disables it, so the absence
 *  of the key (fresh install) means ON. */
function loadAutoResume(): boolean {
  if (typeof window === "undefined") return true;
  return safeGetItem(KEY_AUTO_RESUME) !== "false";
}

/** Auto-redeploy the helper when a console goes offline and the app is
 *  open. Default ON: the PS5's payload dies every time the console rests
 *  (Sony tears down userspace on suspend), and without re-deploying it the
 *  user has to click Connect → Send again after every wake — and the fan
 *  threshold resets to default in the meantime (the pinned value only
 *  takes effect again once the helper is back and its watcher re-arms).
 *  With this on, the app periodically tries to push the bundled ELF to a
 *  console it has lost touch with, so the helper (and the fan setting)
 *  come back by themselves after rest mode or a network blip.
 *
 *  Stored as the string "false" only when the user disables it, so a
 *  fresh install gets the resilient behavior. No-op in a browser session
 *  (the browser has no bundled ELF to push). */
function loadAutoRedeployOnWake(): boolean {
  if (typeof window === "undefined") return true;
  return safeGetItem(KEY_AUTO_REDEPLOY_ON_WAKE) !== "false";
}

interface UploadSettingsState {
  /** When true, the Upload flow skips the Override/Resume/Cancel
   *  dialog and always overwrites the destination. Useful for
   *  iterating on dev builds. */
  alwaysOverwrite: boolean;
  /** Default mode for "Resume": Fast (size-only) or Safe (hash). */
  reconcileMode: ReconcileMode;
  /** When true, the Upload screen renders the per-file scroll panel
   *  during a transfer. Off hides the panel (keeps the progress bar +
   *  summary counters). Useful on low-end displays or when the list
   *  feels noisy for folders with thousands of files. */
  showTransferFiles: boolean;
  /** Outbound bandwidth cap in MB/s. 0 = no cap (use the engine's
   *  env-var default, also typically 0). Persisted to localStorage. */
  bandwidthCapMbps: number;
  /** Desired parallel upload streams (1 = single stream = default/off).
   *  The effective count is min(this, the payload's advertised
   *  max_transfer_streams), resolved at upload start. Breaks the
   *  single-stream ~40 MB/s write ceiling on non-Pro PS5s. */
  uploadStreams: number;
  /** When true (default), a failed upload auto-recovers: backoff, re-deploy
   *  the payload if it crashed, then resume. Bounded retries; fatal errors
   *  still surface. */
  autoResume: boolean;
  /** Keep-PS5-awake policy — see KeepPs5AwakeMode. */
  keepPs5AwakeMode: KeepPs5AwakeMode;
  /** Auto-redeploy the bundled helper to a console after it goes offline
   *  (rest-mode wake, network change, payload crash). Restores the fan
   *  threshold and the upload port without a manual click. See
   *  loadAutoRedeployOnWake. */
  autoRedeployOnWake: boolean;
  setAlwaysOverwrite: (on: boolean) => void;
  setShowTransferFiles: (on: boolean) => void;
  setBandwidthCapMbps: (n: number) => void;
  setUploadStreams: (n: number) => void;
  setAutoResume: (on: boolean) => void;
  setKeepPs5AwakeMode: (mode: KeepPs5AwakeMode) => void;
  setAutoRedeployOnWake: (on: boolean) => void;
}

export const useUploadSettingsStore = create<UploadSettingsState>((set) => ({
  alwaysOverwrite: loadAlwaysOverwrite(),
  reconcileMode: loadReconcileMode(),
  showTransferFiles: loadShowFiles(),
  bandwidthCapMbps: loadBandwidthCap(),
  uploadStreams: loadUploadStreams(),
  autoResume: loadAutoResume(),
  keepPs5AwakeMode: loadKeepPs5AwakeMode(),
  autoRedeployOnWake: loadAutoRedeployOnWake(),
  setAlwaysOverwrite: (alwaysOverwrite) => {
    safeSetItem(
      KEY_ALWAYS_OVERWRITE,
      alwaysOverwrite ? "true" : "false",
    );
    set({ alwaysOverwrite });
  },
  setShowTransferFiles: (showTransferFiles) => {
    safeSetItem(
      KEY_SHOW_FILES,
      showTransferFiles ? "true" : "false",
    );
    set({ showTransferFiles });
  },
  setBandwidthCapMbps: (bandwidthCapMbps) => {
    if (bandwidthCapMbps > 0) {
      safeSetItem(
        KEY_BANDWIDTH_CAP,
        bandwidthCapMbps.toString(),
      );
    } else {
      safeRemoveItem(KEY_BANDWIDTH_CAP);
    }
    set({ bandwidthCapMbps });
  },
  setUploadStreams: (n) => {
    const clamped = clampUploadStreams(n);
    safeSetItem(KEY_UPLOAD_STREAMS, clamped.toString());
    set({ uploadStreams: clamped });
  },
  setAutoResume: (autoResume) => {
    safeSetItem(KEY_AUTO_RESUME, autoResume ? "true" : "false");
    set({ autoResume });
  },
  setKeepPs5AwakeMode: (keepPs5AwakeMode) => {
    safeSetItem(KEY_KEEP_PS5_AWAKE_MODE, keepPs5AwakeMode);
    // Mirror into the legacy boolean so a downgrade to an older build
    // still respects an explicit "off".
    safeSetItem(
      KEY_KEEP_PS5_AWAKE,
      keepPs5AwakeMode === "off" ? "false" : "true",
    );
    set({ keepPs5AwakeMode });
  },
  setAutoRedeployOnWake: (autoRedeployOnWake) => {
    safeSetItem(
      KEY_AUTO_REDEPLOY_ON_WAKE,
      autoRedeployOnWake ? "true" : "false",
    );
    set({ autoRedeployOnWake });
  },
}));
