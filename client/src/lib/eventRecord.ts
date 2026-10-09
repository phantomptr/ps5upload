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

/** Two records are "the same event" for folding; helper log lines never are. */
export function sameKey(a: EventRecord, b: EventRecord): boolean {
  return (
    a.src !== "helper" &&
    a.src === b.src &&
    a.cat === b.cat &&
    a.code === b.code &&
    a.console === b.console
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
