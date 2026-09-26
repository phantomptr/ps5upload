import { create } from "zustand";

import { invoke } from "@tauri-apps/api/core";

import { LEVEL_RANK, currentMinRank } from "./diagSettings";

export type LogLevel = "trace" | "debug" | "info" | "warn" | "error";

export interface LogEntry {
  id: number;
  timestamp: number;
  level: LogLevel;
  source: string;
  message: string;
  /** Optional structured detail (JSON-stringified). Surfaced in the UI
   *  as an expandable block so the main message stays short. */
  detail?: string;
}

/**
 * Ring-buffered log store backing the Log tab.
 *
 * Capacity is bounded at LOG_CAP — once full, the oldest entry is
 * dropped on every new append. This is a debug aid, not a compliance
 * log: we don't persist to disk, and we don't try to capture every
 * event in the app. The focus is "surface things users might need
 * to report in a bug filing" — errors, failed API calls, important
 * lifecycle events.
 *
 * Sources are self-assigned strings like "connection", "upload",
 * "library", "ps5-api". Keep them short — the UI renders them as
 * inline chips.
 */
const LOG_CAP = 500;
let nextId = 0;

interface LogsState {
  entries: LogEntry[];
  /** Current filter applied in the UI. Drives the display; doesn't
   *  affect what's stored. */
  filter: LogLevel | "all";
  append: (
    level: LogLevel,
    source: string,
    message: string,
    detail?: unknown,
  ) => void;
  clear: () => void;
  setFilter: (f: LogLevel | "all") => void;
}

function stringifyDetail(d: unknown): string | undefined {
  if (d === undefined) return undefined;
  if (typeof d === "string") return d;
  if (d instanceof Error) return `${d.name}: ${d.message}\n${d.stack ?? ""}`;
  try {
    return JSON.stringify(d, null, 2);
  } catch {
    return String(d);
  }
}

export const useLogsStore = create<LogsState>((set, get) => ({
  entries: [],
  filter: "all",
  append: (level, source, message, detail) => {
    const entry: LogEntry = {
      id: nextId++,
      timestamp: Date.now(),
      level,
      source,
      message,
      detail: stringifyDetail(detail),
    };
    const current = get().entries;
    const next =
      current.length >= LOG_CAP
        ? [...current.slice(current.length - LOG_CAP + 1), entry]
        : [...current, entry];
    set({ entries: next });
    // Mirror to the durable on-disk log (gated by the configured level) so a
    // bug report can package a time window even after a crash. Best-effort.
    recordToDisk(entry);
  },
  clear: () => set({ entries: [] }),
  setFilter: (filter) => set({ filter }),
}));

/**
 * Module-level convenience wrapper. Lets any code call `log.info(...)`
 * without having to plumb through the hook API. The store itself
 * handles subscription — anyone watching the Log screen will re-render
 * automatically.
 */
export const log = {
  trace: (source: string, message: string, detail?: unknown) =>
    useLogsStore.getState().append("trace", source, message, detail),
  info: (source: string, message: string, detail?: unknown) =>
    useLogsStore.getState().append("info", source, message, detail),
  warn: (source: string, message: string, detail?: unknown) =>
    useLogsStore.getState().append("warn", source, message, detail),
  error: (source: string, message: string, detail?: unknown) =>
    useLogsStore.getState().append("error", source, message, detail),
  debug: (source: string, message: string, detail?: unknown) =>
    useLogsStore.getState().append("debug", source, message, detail),
};

// ── Durable on-disk sink ───────────────────────────────────────────────────
//
// The in-memory ring above is a 500-line display buffer. For bug reports we
// also stream entries to a rotating JSONL file under ~/.ps5upload/logs/ via
// the `diag_log_append` Tauri command, so "package the last N minutes"
// survives a crash and isn't capped at 500 lines. This sink:
//   - gates by the configured minimum level (diagSettings.logLevel),
//   - batches entries and flushes every ~2s (and on tab-hide / unload),
//   - never throws and self-disables if the backend command isn't there
//     (e.g. a non-Tauri/test environment), so logging can never become a
//     second failure.

/** Truncate a field so one pathological entry (a console.error dumping a
 *  multi-MB object) can't bloat the file or a single JSONL line. */
function capField(s: string, max: number): string {
  if (s.length <= max) return s;
  return s.slice(0, max) + "…[truncated]";
}

const DISK_MSG_MAX = 8 * 1024;
const DISK_DETAIL_MAX = 16 * 1024;
/** Flush eagerly once the buffer reaches this many lines (a burst of errors
 *  shouldn't wait the full interval). */
const DISK_FLUSH_AT = 200;
const DISK_FLUSH_MS = 2000;

let diskEnabled = false;
let diskBuffer: string[] = [];
let diskFlushing = false;
let diskFailures = 0;
let diskTimer: ReturnType<typeof setInterval> | null = null;

function recordToDisk(e: LogEntry): void {
  if (!diskEnabled) return;
  if (LEVEL_RANK[e.level] < currentMinRank()) return;
  const obj: Record<string, unknown> = {
    ts: e.timestamp,
    level: e.level,
    source: e.source,
    message: capField(e.message, DISK_MSG_MAX),
  };
  if (e.detail) obj.detail = capField(e.detail, DISK_DETAIL_MAX);
  try {
    diskBuffer.push(JSON.stringify(obj));
  } catch {
    return; // unserializable — drop this one line, never throw
  }
  if (diskBuffer.length >= DISK_FLUSH_AT) void flushDisk();
}

async function flushDisk(): Promise<void> {
  if (!diskEnabled || diskFlushing || diskBuffer.length === 0) return;
  diskFlushing = true;
  const batch = diskBuffer;
  diskBuffer = [];
  try {
    await invoke("diag_log_append", { lines: batch });
    diskFailures = 0;
  } catch {
    // Backend not present (non-Tauri/test) or transient write error. Don't
    // re-queue (avoid unbounded growth); after a few consecutive failures,
    // assume there's no sink and stop trying.
    diskFailures += 1;
    if (diskFailures >= 3) {
      diskEnabled = false;
      diskBuffer = [];
      if (diskTimer) {
        clearInterval(diskTimer);
        diskTimer = null;
      }
    }
  } finally {
    diskFlushing = false;
  }
}

/**
 * Start the durable disk sink. Idempotent — safe to call twice. Wired from
 * main.tsx at boot, right after installConsoleCapture(). No-ops gracefully
 * outside a browser/Tauri runtime.
 */
let diskInstalled = false;
export function installDiskLogSink(): void {
  if (diskInstalled || typeof window === "undefined") return;
  diskInstalled = true;
  diskEnabled = true;
  diskTimer = setInterval(() => void flushDisk(), DISK_FLUSH_MS);
  // Flush opportunistically when the user navigates away or hides the window,
  // so the last couple of seconds before a close are captured.
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") void flushDisk();
  });
  window.addEventListener("beforeunload", () => void flushDisk());

  // Vite HMR: tear the interval down so reloads don't stack timers.
  const hot = (import.meta as unknown as { hot?: { dispose: (cb: () => void) => void } }).hot;
  if (hot) {
    hot.dispose(() => {
      if (diskTimer) clearInterval(diskTimer);
      diskTimer = null;
      diskInstalled = false;
      diskEnabled = false;
    });
  }
}

/** Force a flush now (e.g. just before building a bug report so the buffered
 *  tail is on disk). Returns when the in-flight write settles. */
export async function flushDiskLogNow(): Promise<void> {
  await flushDisk();
}

/**
 * Patch console.error / console.warn once so uncaught errors (React
 * warnings, network failures logged by fetch wrappers, etc.) also
 * show up in the Log tab without every call site remembering to
 * dual-log. Originals are preserved and still fire so DevTools users
 * aren't surprised.
 *
 * Idempotent — calling twice is safe. Wired from main.tsx at boot.
 */
let installed = false;
export function installConsoleCapture(): void {
  if (installed) return;
  installed = true;
  const origError = console.error.bind(console);
  const origWarn = console.warn.bind(console);
  console.error = (...args: unknown[]) => {
    origError(...args);
    useLogsStore.getState().append(
      "error",
      "console",
      args.map((a) => (typeof a === "string" ? a : stringifyDetail(a) ?? "")).join(" "),
    );
  };
  console.warn = (...args: unknown[]) => {
    origWarn(...args);
    useLogsStore.getState().append(
      "warn",
      "console",
      args.map((a) => (typeof a === "string" ? a : stringifyDetail(a) ?? "")).join(" "),
    );
  };

  // Also capture unhandled promise rejections — these typically point
  // at API calls that blew up without a try/catch around them.
  window.addEventListener("unhandledrejection", (e) => {
    // Tauri 2's `_unlisten` returns `() => void` synchronously, but
    // internally spawns an async `unregisterListener` whose rejection
    // we have no way to attach `.catch` to from userland (safeUnlisten
    // does its best on the returned value, but there isn't one). When
    // the listener table is torn down between subscribe and cleanup
    // (HMR reload, route remount, parent webview destroyed) that
    // inner Promise rejects with `TypeError: undefined is not an
    // object (evaluating 'listeners[eventId].handlerId')`. The
    // listener is already gone — the symptom is exactly the thing we
    // wanted to happen — so swallow it silently. Log at debug so
    // bug-reporters still see we noticed.
    const reason = e.reason;
    const msg =
      reason && typeof (reason as { message?: unknown }).message === "string"
        ? ((reason as { message: string }).message)
        : "";
    if (msg.includes("listeners[eventId].handlerId")) {
      e.preventDefault();
      useLogsStore.getState().append(
        "debug",
        "tauri",
        "Tauri unregisterListener race (listener already gone)",
      );
      return;
    }
    useLogsStore.getState().append(
      "error",
      "promise",
      "Unhandled rejection",
      reason,
    );
  });
  window.addEventListener("error", (e) => {
    const msg = e.message || "";
    // React 19 replays "continuous" native events (pointermove, mouseover,
    // dragover, scroll) that landed on a component mid-render. The replay
    // does `nativeEvent.target.dispatchEvent(clone)` — and WebKit nulls
    // `event.target` once the original node is detached, so when the hovered
    // element unmounts (e.g. a view swap as an upload starts) the target is
    // gone and it throws `… is not an object (evaluating 'el.dispatchEvent')`
    // (WebKit) / `Cannot read properties of null (reading 'dispatchEvent')`
    // (Chromium). It's harmless — one stale move/hover event fails to replay
    // — and not ours (we only ever dispatch on `window`, never null). Swallow
    // it; log at debug so bug-reporters still see we noticed.
    if (msg.includes("dispatchEvent")) {
      e.preventDefault();
      useLogsStore.getState().append(
        "debug",
        "react",
        "React continuous-event replay race (target already unmounted)",
      );
      return;
    }
    // Capture where it happened. WebKit nulls `e.error` for some uncaught
    // errors even when the message survives, so fall back to file:line:col
    // (which pins the bundle chunk + offset) when no Error/stack is present.
    const where = e.filename
      ? `${e.filename}:${e.lineno ?? 0}:${e.colno ?? 0}`
      : undefined;
    useLogsStore.getState().append(
      "error",
      "runtime",
      msg || "Uncaught error",
      e.error ?? where,
    );
  });
}
