import { invoke } from "@tauri-apps/api/core";

import { buildDiagnosticBundle } from "./diagnosticBundle";
import { useNotificationsStore } from "../state/notifications";

/**
 * Automatic crash/error report collection.
 *
 * Whenever the app surfaces an error (an error-level notification) or the
 * React tree crashes (ErrorBoundary), we build the same rich diagnostic
 * bundle the manual "Save bug report" produces and write it to
 * `~/.ps5upload/crash-reports/` via the backend (a bounded ring buffer).
 * Settings can then zip every collected report for sharing.
 *
 * Design notes:
 *   - Never throws. A reporter that fails must not cause a *second* error.
 *   - Debounced: at most one auto-capture per `MIN_INTERVAL_MS`, and capped
 *     per session, so an error storm can't write hundreds of files or spin
 *     the disk. (The backend also prunes to a fixed maximum.)
 *   - Redacted by default (host IPs → /16), same as the manual export.
 */

const MIN_INTERVAL_MS = 15_000;
const MAX_PER_SESSION = 40;
/** Crash reports keep more log context than the manual export. */
const CRASH_LOG_LINES = 400;

let lastCaptureMs = 0;
let capturesThisSession = 0;
let cachedVersion: string | null = null;

async function appVersion(): Promise<string> {
  if (cachedVersion) return cachedVersion;
  try {
    const { getVersion } = await import("@tauri-apps/api/app");
    cachedVersion = await getVersion();
  } catch {
    cachedVersion = "unknown";
  }
  return cachedVersion;
}

/**
 * Capture a crash/error report. `trigger` is a short human description of
 * what prompted it. `force` bypasses the debounce (used for hard crashes,
 * which are rare and always worth capturing).
 */
export async function captureCrashReport(
  trigger: string,
  opts?: { force?: boolean },
): Promise<void> {
  try {
    const now = Date.now();
    if (!opts?.force) {
      if (capturesThisSession >= MAX_PER_SESSION) return;
      if (now - lastCaptureMs < MIN_INTERVAL_MS) return;
    }
    lastCaptureMs = now;
    capturesThisSession += 1;

    const bundle = buildDiagnosticBundle({
      appVersion: await appVersion(),
      redact: true,
      trigger,
      logLimit: CRASH_LOG_LINES,
    });
    await invoke("crash_report_save", {
      contents: JSON.stringify(bundle, null, 2),
    });
  } catch {
    // Swallow — the reporter is best-effort and must never surface its own
    // failure (e.g. non-Tauri context, disk full) as a new error.
  }
}

/**
 * Known-benign error signatures that must NOT trigger an auto-capture.
 *
 * The Tauri JS event layer throws a TypeError when a listener is torn down
 * twice — e.g. an unmount/`unlisten` race or a StrictMode double-invoke. It
 * reads as `Cannot read properties of undefined (reading 'handlerId')` and
 * bubbles up as an unhandledrejection, but nothing actually failed: the event
 * is already gone. Capturing it produced phantom crash reports.
 */
const BENIGN_SIGNATURES = [
  "handlerId", // Tauri unlisten race: listeners[eventId].handlerId
  "dispatchEvent", // React 19 continuous-event replay onto an unmounted target
  "ResizeObserver loop", // browser-internal, never actionable
];

function isBenignNoise(msg: string): boolean {
  return BENIGN_SIGNATURES.some((sig) => msg.includes(sig));
}

let wired = false;

/**
 * Wire automatic capture to the app's error signals. Call once at startup.
 * Currently captures on every error-level notification; ErrorBoundary calls
 * `captureCrashReport(..., { force: true })` directly for render crashes.
 */
export function initCrashReporter(): void {
  if (wired) return;
  wired = true;

  // 1. Every error-level notification — job failures, payload rejections,
  //    FS-op failures, connection errors all surface here.
  useNotificationsStore.subscribe((state, prev) => {
    const newest = state.entries[0];
    if (!newest) return;
    // entries are prepended newest-first; only react to a genuinely new one.
    if (prev.entries[0] && newest.id === prev.entries[0].id) return;
    if (newest.level !== "error") return;
    void captureCrashReport(`error: ${newest.title}`);
  });

  // 2. Uncaught errors and unhandled promise rejections — the widest net.
  //    This catches engine-proxy failures ("error sending request"), bugs in
  //    our own code paths, and anything that never made it to a notification.
  //    Debounced + session-capped in captureCrashReport, so a burst can't
  //    spin the disk.
  if (typeof window !== "undefined") {
    window.addEventListener("error", (e) => {
      const msg = e?.error?.message ?? e?.message ?? "uncaught error";
      if (isBenignNoise(msg)) return;
      void captureCrashReport(`uncaught-error: ${msg}`);
    });
    window.addEventListener("unhandledrejection", (e) => {
      const r: any = (e as PromiseRejectionEvent).reason;
      const msg = r?.message ?? (typeof r === "string" ? r : "unhandled rejection");
      if (isBenignNoise(msg)) return;
      void captureCrashReport(`unhandled-rejection: ${msg}`);
    });
  }
}
