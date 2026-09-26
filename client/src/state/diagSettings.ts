import { create } from "zustand";
import type { LogLevel } from "./logs";
import { safeGetItem, safeRemoveItem, safeSetItem } from "../lib/safeStorage";

/**
 * Diagnostics preferences — currently just the minimum log level that gets
 * written to the persistent on-disk log (`~/.ps5upload/logs/`). The display
 * ring in the Log tab still shows everything; this gates only what's durably
 * recorded and therefore what a bug report can include.
 *
 * Persisted to localStorage (manual, per the app's per-key convention — there
 * is no umbrella settings store). Mirrors the scalar pattern in
 * `uploadSettings.ts`.
 *
 * Default `info`: keeps the on-disk file small for everyday use. A user
 * chasing a hard bug raises it to `debug`/`trace`, reproduces, then packages —
 * the active level is stamped into the bundle so a maintainer knows how
 * verbose the capture was.
 */

const KEY_LOG_LEVEL = "ps5upload.log_level";

export const DEFAULT_LOG_LEVEL: LogLevel = "info";

/** Severity ranks. Higher = more severe. A line is written to disk when its
 *  rank is ≥ the configured minimum. Exported so the disk sink and any
 *  packaging filter agree on the ordering. */
export const LEVEL_RANK: Record<LogLevel, number> = {
  trace: 10,
  debug: 20,
  info: 30,
  warn: 40,
  error: 50,
};

/** Ordered most-verbose → least, for rendering a level <select>. */
export const LOG_LEVELS: LogLevel[] = ["trace", "debug", "info", "warn", "error"];

function isLevel(v: string | null): v is LogLevel {
  return v !== null && v in LEVEL_RANK;
}

function loadLogLevel(): LogLevel {
  if (typeof window === "undefined") return DEFAULT_LOG_LEVEL;
  const v = safeGetItem(KEY_LOG_LEVEL);
  return isLevel(v) ? v : DEFAULT_LOG_LEVEL;
}

interface DiagSettingsState {
  /** Minimum level written to the persistent on-disk log. */
  logLevel: LogLevel;
  setLogLevel: (level: LogLevel) => void;
}

export const useDiagSettingsStore = create<DiagSettingsState>((set) => ({
  logLevel: loadLogLevel(),
  setLogLevel: (logLevel) => {
    if (typeof window !== "undefined") {
      if (logLevel === DEFAULT_LOG_LEVEL) {
        safeRemoveItem(KEY_LOG_LEVEL);
      } else {
        safeSetItem(KEY_LOG_LEVEL, logLevel);
      }
    }
    set({ logLevel });
  },
}));

/** Module-level read of the current minimum rank, for the non-React disk
 *  sink in `logs.ts` (which can't use the hook). Reads live store state so it
 *  reflects changes made from the Settings/Logs UI without a reload. */
export function currentMinRank(): number {
  return LEVEL_RANK[useDiagSettingsStore.getState().logLevel];
}
