import { create } from "zustand";

export type LogLevel = "info" | "warn" | "error" | "debug";

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
  info: (source: string, message: string, detail?: unknown) =>
    useLogsStore.getState().append("info", source, message, detail),
  warn: (source: string, message: string, detail?: unknown) =>
    useLogsStore.getState().append("warn", source, message, detail),
  error: (source: string, message: string, detail?: unknown) =>
    useLogsStore.getState().append("error", source, message, detail),
  debug: (source: string, message: string, detail?: unknown) =>
    useLogsStore.getState().append("debug", source, message, detail),
};

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
    useLogsStore.getState().append(
      "error",
      "promise",
      "Unhandled rejection",
      e.reason,
    );
  });
  window.addEventListener("error", (e) => {
    useLogsStore.getState().append(
      "error",
      "runtime",
      e.message || "Uncaught error",
      e.error,
    );
  });
}
