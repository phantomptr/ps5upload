import { create } from "zustand";
import type { LogLevel } from "./logs";
import { safeGetItem, safeRemoveItem, safeSetItem } from "../lib/safeStorage";
import { registerSettingReload } from "../lib/liveSettings";

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
/** Unix ms until which "Detailed logs" stays on (absent: off). Per device, never synced. */
export const KEY_DETAILED_UNTIL = "ps5upload.detailed_logs_until";

/** How long "Detailed logs" stays on before it turns itself off. The engine caps it at the same. */
export const DETAILED_LOGS_MS = 24 * 60 * 60 * 1000;

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

/** The stored "Detailed logs" end time, or null when it is off or already over. */
function loadDetailedUntil(now = Date.now()): number | null {
  if (typeof window === "undefined") return null;
  const v = Number(safeGetItem(KEY_DETAILED_UNTIL));
  // A time further out than the longest switch-on is not one this app wrote: off.
  return Number.isFinite(v) && v > now && v <= now + DETAILED_LOGS_MS ? v : null;
}

interface DiagSettingsState {
  /** Minimum level written to the persistent on-disk log. */
  logLevel: LogLevel;
  setLogLevel: (level: LogLevel) => void;
  /** "Detailed logs": on until this Unix ms (null: off). While on, debug lines are recorded
   *  (whatever `logLevel` says, unless it is already more verbose) and the engine writes its
   *  extra Remote Play and Play Together lines. It turns itself off at this time. */
  detailedUntil: number | null;
  /** On for DETAILED_LOGS_MS from `now`, or off. */
  setDetailed: (on: boolean, now?: number) => void;
}

export const useDiagSettingsStore = create<DiagSettingsState>((set) => ({
  logLevel: loadLogLevel(),
  detailedUntil: loadDetailedUntil(),
  setDetailed: (on, now = Date.now()) => {
    const until = on ? now + DETAILED_LOGS_MS : null;
    if (typeof window !== "undefined") {
      if (until === null) safeRemoveItem(KEY_DETAILED_UNTIL);
      else safeSetItem(KEY_DETAILED_UNTIL, String(until));
    }
    set({ detailedUntil: until });
  },
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
export function currentMinRank(now = Date.now()): number {
  const s = useDiagSettingsStore.getState();
  const rank = LEVEL_RANK[s.logLevel];
  return detailedActive(s.detailedUntil, now) ? Math.min(rank, LEVEL_RANK.debug) : rank;
}

/** "Detailed logs" is on at `now` (its end time has not passed). */
export function detailedActive(until: number | null, now = Date.now()): boolean {
  return until !== null && until > now;
}

// A change synced from another browser shows at once (lib/liveSettings).
registerSettingReload([KEY_LOG_LEVEL], () => useDiagSettingsStore.setState({ logLevel: loadLogLevel() }));
registerSettingReload([KEY_DETAILED_UNTIL], () =>
  useDiagSettingsStore.setState({ detailedUntil: loadDetailedUntil() }),
);
