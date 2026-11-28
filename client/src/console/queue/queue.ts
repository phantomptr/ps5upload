/**
 * The console's queue (#982): game copies and installs added on the page, run one after another
 * by the helper (payload/src/webui_queue.c), so they finish with the page closed. The page only
 * adds, reads and changes it.
 *
 * One store for the page, read from POST /api/queue: every second while something waits or runs,
 * every ten seconds otherwise, and not at all while the page is hidden or nobody shows it.
 */
import { useEffect, useState } from "react";
import { ApiError, call } from "../api";
import { pollWhileVisible } from "../ui/visiblePoll";

export type QueueKind = "copy" | "install";
export type QueueState = "pending" | "running" | "done" | "failed" | "cancelled";

/** One item as the helper lists it (payload/include/console_queue.h). */
export interface QueueItem {
  id: number;
  kind: QueueKind;
  state: QueueState;
  /** An install from a share: 0 while its package is copied in, 1 while the console installs it. */
  step: number;
  src: string;
  dest: string;
  name: string;
  /** What it came from on the page (a library entry's key). */
  key: string;
  /** The job running it ("smb:<id>" or 32 hex), "" between steps. */
  job: string;
  /** How it failed (a cause token). */
  error: string;
  /** Why it waits (another install on the console), "" when it does not. */
  wait: string;
  elapsed_s: number;
  bytes_done: number;
  bytes_total: number;
  /** Stop was asked while it ran. */
  cancel: boolean;
}

export interface Queue {
  paused: boolean;
  /** Why it paused: "" (the user), "interrupted_install" (the helper restarted mid-install). */
  reason: string;
  /** In run order: finished, then the running one, then the pending ones. */
  items: QueueItem[];
}

const EMPTY: Queue = { paused: false, reason: "", items: [] };

const num = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : 0);
const str = (v: unknown) => (typeof v === "string" ? v : "");

/** The helper's answer, however odd: entries that are not items are left out. */
export function parseQueue(raw: unknown): Queue {
  if (!raw || typeof raw !== "object") return EMPTY;
  const r = raw as Record<string, unknown>;
  const items: QueueItem[] = [];
  for (const x of Array.isArray(r.items) ? r.items : []) {
    if (!x || typeof x !== "object") continue;
    const o = x as Record<string, unknown>;
    const kind = o.kind === "copy" || o.kind === "install" ? o.kind : null;
    const state = ["pending", "running", "done", "failed", "cancelled"].includes(str(o.state)) ? (o.state as QueueState) : null;
    if (!kind || !state || !num(o.id) || !str(o.src)) continue;
    items.push({
      id: num(o.id),
      kind,
      state,
      step: num(o.step),
      src: str(o.src),
      dest: str(o.dest),
      name: str(o.name),
      key: str(o.key),
      job: str(o.job),
      error: str(o.error),
      wait: str(o.wait),
      elapsed_s: num(o.elapsed_s),
      bytes_done: num(o.bytes_done),
      bytes_total: num(o.bytes_total),
      cancel: o.cancel === true,
    });
  }
  return { paused: r.paused === true, reason: str(r.reason), items };
}

export const unfinished = (i: QueueItem) => i.state === "pending" || i.state === "running";

/** Where a pending item stands in line (1: next), or 0. */
export function linePosition(q: Queue, id: number): number {
  const line = q.items.filter((i) => i.state === "pending");
  return line.findIndex((i) => i.id === id) + 1;
}

/** The newest item for `key` (a library entry), unfinished ones first; null when there is none. */
export function itemFor(q: Queue, key: string): QueueItem | null {
  let found: QueueItem | null = null;
  for (const i of q.items) {
    if (i.key !== key) continue;
    if (unfinished(i)) return i;
    found = i;
  }
  return found;
}

/* ---- calls ---- */

export async function fetchQueue(): Promise<Queue> {
  return parseQueue(await call<unknown>("/api/queue"));
}

/** Add an item; its id. A copy needs `dest` (the console path it becomes). */
export async function addToQueue(item: { kind: QueueKind; src: string; dest?: string; name?: string; key?: string }): Promise<number> {
  const r = await call<{ id?: unknown }>("/api/queue/add", item);
  if (!r || typeof r.id !== "number") throw new ApiError(500, "bad_reply");
  queueChanged();
  return r.id;
}

export async function cancelQueued(id: number): Promise<void> {
  await call("/api/queue/cancel", { id });
  queueChanged();
}

export async function moveToTop(id: number): Promise<void> {
  await call("/api/queue/top", { id });
  queueChanged();
}

export async function clearFinished(): Promise<void> {
  await call("/api/queue/clear", {});
  queueChanged();
}

export async function setPaused(paused: boolean): Promise<void> {
  await call("/api/queue/pause", { paused });
  queueChanged();
}

/* ---- the page's one store ---- */

export const QUEUE_FAST_MS = 1000;
export const QUEUE_SLOW_MS = 10_000;

let current: Queue = EMPTY;
let loaded = false;
const listeners = new Set<() => void>();
let stopPoll: (() => void) | null = null;
let pollMs = 0;

function emit(): void {
  listeners.forEach((fn) => fn());
}

async function read(): Promise<void> {
  try {
    current = await fetchQueue();
    loaded = true;
    emit();
  } catch {
    // An older helper (404) or a passing failure: the queue reads as it was.
  }
  schedule();
}

/** Poll at the pace the queue needs: fast while anything waits or runs. */
function schedule(): void {
  if (!listeners.size) return;
  const want = current.items.some(unfinished) ? QUEUE_FAST_MS : QUEUE_SLOW_MS;
  if (stopPoll && want === pollMs) return;
  stopPoll?.();
  pollMs = want;
  let first = true;
  stopPoll = pollWhileVisible(() => {
    // pollWhileVisible runs at once: the read that scheduled this one just happened.
    if (first) {
      first = false;
      return;
    }
    return read();
  }, want);
}

/** Something changed on this page: read again now. */
export function queueChanged(): void {
  if (listeners.size) void read();
}

/** The queue as React state, kept fresh while any component shows it. */
export function useQueue(): { queue: Queue; loaded: boolean } {
  const [, setTick] = useState(0);
  useEffect(() => {
    const fn = () => setTick((n) => n + 1);
    listeners.add(fn);
    if (listeners.size === 1) void read();
    return () => {
      listeners.delete(fn);
      if (!listeners.size) {
        stopPoll?.();
        stopPoll = null;
      }
    };
  }, []);
  return { queue: current, loaded };
}

/** Tests only. */
export function _setQueueForTests(q: Queue): void {
  current = q;
  loaded = true;
  emit();
}
