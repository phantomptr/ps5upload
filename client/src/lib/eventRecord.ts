/**
 * One record in the bug-report event journals (spec §1.1). The engine's `Event` has the same
 * shape, so the app's journal and the engine's merge into one timeline.
 */
export type EventCat = "connection" | "helper" | "transfer" | "install" | "api" | "app" | "system";
export const EVENT_CATS: readonly EventCat[] = [
  "connection",
  "helper",
  "transfer",
  "install",
  "api",
  "app",
  "system",
];
export type EventLevel = "info" | "warn" | "error";
export interface EventRecord {
  ts: number;
  src: "engine" | "app" | "helper";
  console?: string;
  cat: EventCat;
  level: EventLevel;
  code?: string;
  msg: string;
  detail?: unknown;
  count?: number;
  last_ts?: number;
}

export const COLLAPSE_MS = 60_000;

/** One-off events: each keeps its own line and time (a notification's "Report this" link points at it). */
const NEVER_FOLD = new Set(["notification", "crash", "app_start", "install_start", "install_result", "job_done", "job_failed", "engine_start"]);

/** The message with every run of digits made one `#`: "unreachable (3 s)" and "(17 s)" match. */
const shape = (msg: string) => msg.replace(/\d+/g, "#");

/** Whether `b` repeats `a` (so it folds into a count); the engine journal uses the same rule. */
export function sameKey(a: EventRecord, b: EventRecord): boolean {
  return (
    a.src !== "helper" &&
    !NEVER_FOLD.has(a.code ?? "") &&
    a.src === b.src &&
    a.cat === b.cat &&
    a.code === b.code &&
    a.console === b.console &&
    a.level === b.level &&
    shape(a.msg) === shape(b.msg)
  );
}

/** Folds `next` into `prev` when it repeats within a minute; otherwise `prev` is done (flushed). */
export function collapseInto(
  prev: EventRecord | null,
  next: EventRecord,
): { merged: EventRecord; flushed: EventRecord | null } {
  if (prev && sameKey(prev, next) && next.ts - (prev.last_ts ?? prev.ts) <= COLLAPSE_MS) {
    return { merged: { ...prev, count: (prev.count ?? 1) + 1, last_ts: next.ts }, flushed: null };
  }
  return { merged: next, flushed: prev };
}
