import { create } from "zustand";
import type { LogLevel } from "./logs";
import { safeGetItem, safeRemoveItem, safeSetItem } from "../lib/safeStorage";
import { registerSettingReload } from "../lib/liveSettings";

/**
 * Log levels: one setting for the app, the engine and the PS5 helper (engine
 * `ps5upload_core::log`, `/api/logs/level`). A global level plus per-category
 * overrides; anything above `info` is a raise that ends by itself at `until_ms`
 * (at most a day), so nobody stays verbose by accident.
 *
 * The engine owns the setting and keeps it on disk; this store mirrors what it
 * last said (`engine`, refreshed by lib/logLevels.ts) and gates the app's own
 * durable log (`~/.ps5upload/logs/`, logs.ts) with it, by the category of each
 * line's source. `logLevel` is the local fallback while the engine can't be
 * reached (and the shared `log_level` setting): `info` or quieter only, since a
 * raise must have an end.
 *
 * The display ring in the Logs tab still shows every line; this gates only what's
 * durably recorded and therefore what a bug report can include. The active
 * levels are stamped into every report.
 */

const KEY_LOG_LEVEL = "ps5upload.log_level";
/** "Detailed logs" from before log levels: Unix ms until which it stayed on. Read once to carry
 *  a running switch over to the engine as a debug raise, then removed. */
export const KEY_DETAILED_UNTIL = "ps5upload.detailed_logs_until";

export const DEFAULT_LOG_LEVEL: LogLevel = "info";

/** The longest a raise lasts; the engine caps it the same. */
export const MAX_RAISE_MS = 24 * 60 * 60 * 1000;

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

/** Every category, as the engine names them (`ps5upload_core::log::CATEGORIES`). */
export const LOG_CATEGORIES = [
  "general",
  "api",
  "connection",
  "helper",
  "rp",
  "pairing",
  "wake",
  "transfer",
  "install",
  "cleanup",
  "files",
  "convert",
  "together",
  "golive",
  "mesh",
  "ui",
] as const;
export type LogCategory = (typeof LOG_CATEGORIES)[number];

/** The levels as `GET /api/logs/level` answers them. */
export interface EngineLevels {
  level: LogLevel;
  categories: Partial<Record<string, LogLevel>>;
  /** When a raise ends (Unix ms); null when nothing is raised. */
  until_ms: number | null;
  raised?: boolean;
  /** What each category writes now. */
  effective?: Partial<Record<string, LogLevel>>;
}

/** What a `PUT /api/logs/level` asks for. */
export interface LevelsChange {
  level?: LogLevel;
  categories?: Partial<Record<string, LogLevel | null>>;
  merge?: boolean;
  minutes?: number;
  until_ms?: number;
  reset?: boolean;
}

export function isLevel(v: unknown): v is LogLevel {
  return typeof v === "string" && v in LEVEL_RANK;
}

/** A level more verbose than info: a raise, which needs an end. */
export function isRaise(l: LogLevel): boolean {
  return LEVEL_RANK[l] < LEVEL_RANK.info;
}

function loadLogLevel(): LogLevel {
  if (typeof window === "undefined") return DEFAULT_LOG_LEVEL;
  const v = safeGetItem(KEY_LOG_LEVEL);
  // A debug or trace level stored before levels had an end is not kept forever: info.
  return isLevel(v) && !isRaise(v) ? v : DEFAULT_LOG_LEVEL;
}

/** The category of an app log line's source ("remote-play" → rp, "upload" → transfer). Exported
 *  for tests and for the engine bridge (whose sources are `engine:<category>`). */
export function categoryOf(source: string): LogCategory {
  const s = source.toLowerCase();
  if (s.startsWith("engine:")) {
    const c = s.slice(7);
    return (LOG_CATEGORIES as readonly string[]).includes(c) ? (c as LogCategory) : "general";
  }
  if (s === "engine") return "general";
  const rules: [RegExp, LogCategory][] = [
    // The page's own error capture (installConsoleCapture) and runtime errors.
    [/^(console|promise|runtime|react|tauri|ui|nav)$/, "ui"],
    [/^(remote-?play|rp|play|stream|decoder|watch|hdr)/, "rp"],
    [/pair|regist/, "pairing"],
    [/wake|rest|power/, "wake"],
    [/together|join|pad|guest|phone/, "together"],
    [/go-?live|golive|broadcast/, "golive"],
    [/install|pkg/, "install"],
    [/upload|download|transfer|queue|job|copy|send/, "transfer"],
    [/clean/, "cleanup"],
    [/convert|backport|fpkg|archive|extract/, "convert"],
    [/^(files?|fs|explorer|library)/, "files"],
    [/helper|payload/, "helper"],
    [/mesh|drive/, "mesh"],
    [/connect|discover|status|ps5-api|console/, "connection"],
    [/^(cmd|api)$/, "api"],
  ];
  for (const [re, cat] of rules) if (re.test(s)) return cat;
  return "general";
}

/** The level `cat` writes at `now` under `levels` (a raise that has run out counts as info). */
export function effectiveLevel(levels: EngineLevels, cat: string, now = Date.now()): LogLevel {
  const l = levels.categories[cat] ?? levels.level;
  if (isRaise(l) && !(levels.until_ms !== null && levels.until_ms > now)) return "info";
  return l;
}

/** Something is raised above info and has not run out. */
export function raiseActive(levels: EngineLevels | null, now = Date.now()): boolean {
  if (!levels || levels.until_ms === null || levels.until_ms <= now) return false;
  return isRaise(levels.level) || Object.values(levels.categories).some((l) => l !== undefined && isRaise(l));
}

interface DiagSettingsState {
  /** The local fallback level (info or quieter), while the engine's is not known. */
  logLevel: LogLevel;
  setLogLevel: (level: LogLevel) => void;
  /** The engine's setting as it last said, or null before it answered. */
  engine: EngineLevels | null;
  setEngineLevels: (levels: EngineLevels | null) => void;
}

export const useDiagSettingsStore = create<DiagSettingsState>((set) => ({
  logLevel: loadLogLevel(),
  engine: null,
  setEngineLevels: (engine) => set({ engine }),
  setLogLevel: (logLevel) => {
    const kept = isRaise(logLevel) ? DEFAULT_LOG_LEVEL : logLevel;
    if (typeof window !== "undefined") {
      if (kept === DEFAULT_LOG_LEVEL) {
        safeRemoveItem(KEY_LOG_LEVEL);
      } else {
        safeSetItem(KEY_LOG_LEVEL, kept);
      }
    }
    set({ logLevel: kept });
  },
}));

/** The minimum rank written for a line from `source` now: the engine's level for its category when
 *  known, else the local fallback. Module-level for the non-React disk sink in `logs.ts`. */
export function currentMinRank(now = Date.now(), source?: string): number {
  const s = useDiagSettingsStore.getState();
  if (s.engine) {
    return LEVEL_RANK[effectiveLevel(s.engine, source ? categoryOf(source) : "general", now)];
  }
  return LEVEL_RANK[s.logLevel];
}

/** A line at `level` from `source` would be written now. For code that builds an expensive debug
 *  line: check first, then format. */
export function logEnabled(level: LogLevel, source: string, now = Date.now()): boolean {
  return LEVEL_RANK[level] >= currentMinRank(now, source);
}

/** The levels in force, for a report: `{level, categories, until, raised}`. */
export function levelsSummary(now = Date.now()): {
  level: LogLevel;
  categories: Partial<Record<string, LogLevel>>;
  until: string | null;
  raised: boolean;
  source: "engine" | "app";
} {
  const s = useDiagSettingsStore.getState();
  if (!s.engine) return { level: s.logLevel, categories: {}, until: null, raised: false, source: "app" };
  const raised = raiseActive(s.engine, now);
  return {
    level: s.engine.level,
    categories: s.engine.categories,
    until: raised && s.engine.until_ms !== null ? new Date(s.engine.until_ms).toISOString() : null,
    raised,
    source: "engine",
  };
}

// A change synced from another browser shows at once (lib/liveSettings).
registerSettingReload([KEY_LOG_LEVEL], () => useDiagSettingsStore.setState({ logLevel: loadLogLevel() }));
