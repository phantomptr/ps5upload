// Folder sizes in Files (#974): what the engine already knows about a console folder's size, and
// its queue of folders to measure (engine/crates/ps5upload-engine/src/folder_sizes.rs).
//
// Nothing is measured unless someone asks ("Measure size", "Measure everything here") or the
// optional "Keep folder sizes fresh" setting is on. A known size comes with the time it was
// measured, so a cached value is never shown as a live one.

import { getEngineUrl } from "../state/engine";
import { engineFetch } from "../lib/engineFetch";

/** Where a known size came from. */
export type FolderSizeSource = "measured" | "space" | "transfer" | "cleanup";

export interface FolderSize {
  /** The size to show: stored on disk when `stored`, otherwise the files' total. */
  bytes: number;
  files: number | null;
  measured_at_ms: number;
  source: FolderSizeSource;
  stored: boolean;
  /** Some folders inside could not be read: the size is at least this. */
  partial: boolean;
}

export interface MeasureItem {
  addr: string;
  path: string;
}

export interface MeasureQueue {
  /** Bumps whenever any known size changes. */
  rev: number;
  running: (MeasureItem & { started_at_ms: number; files: number; bytes: number }) | null;
  queued: MeasureItem[];
  /** A transfer or install runs on the console: measuring waits for it. */
  paused: null | "busy";
  keep_fresh: boolean;
}

export interface FolderSizes {
  dir: string;
  self: FolderSize | null;
  /** Known sizes of the folder's sub-folders, by name. */
  children: Record<string, FolderSize>;
  queue: MeasureQueue;
}

export const EMPTY_QUEUE: MeasureQueue = {
  rev: 0,
  running: null,
  queued: [],
  paused: null,
  keep_fresh: false,
};

/** Anything waiting or being measured. */
export function queueActive(q: MeasureQueue | null | undefined): boolean {
  return !!q && (q.running !== null || q.queued.length > 0);
}

async function call<T>(path: string, body?: unknown): Promise<T> {
  const res = await engineFetch(`${getEngineUrl()}/api/ps5/folder-sizes${path}`, {
    method: body === undefined ? "GET" : "POST",
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let parsed: unknown;
  try {
    parsed = text ? JSON.parse(text) : null;
  } catch {
    parsed = null;
  }
  if (!res.ok) {
    throw new Error((parsed as { error?: string } | null)?.error ?? `HTTP ${res.status}`);
  }
  return parsed as T;
}

/** A queue as the engine sent it, with anything missing filled in (an older engine, a typo). */
export function asQueue(raw: unknown): MeasureQueue {
  const q = (raw && typeof raw === "object" ? raw : {}) as Partial<MeasureQueue>;
  return {
    rev: typeof q.rev === "number" ? q.rev : 0,
    running: q.running ?? null,
    queued: Array.isArray(q.queued) ? q.queued : [],
    paused: q.paused ?? null,
    keep_fresh: !!q.keep_fresh,
  };
}

export const folderSizesApi = {
  /** Known sizes of `dir` and its sub-folders. Also tells the engine the folder was opened. */
  async get(addr: string, dir: string): Promise<FolderSizes> {
    const q = `?addr=${encodeURIComponent(addr)}&dir=${encodeURIComponent(dir)}`;
    const r = await call<Partial<FolderSizes>>(q);
    return {
      dir: r?.dir ?? dir,
      self: r?.self ?? null,
      children: r?.children ?? {},
      queue: asQueue(r?.queue),
    };
  },
  /** Measure these folders, one at a time, in the background. */
  async measure(addr: string, paths: string[]): Promise<MeasureQueue> {
    return asQueue(await call("/measure", { addr, paths }));
  },
  /** Measure every sub-folder of `dir`. */
  async measureAllIn(addr: string, dir: string): Promise<MeasureQueue> {
    return asQueue(await call("/measure", { addr, dir }));
  },
  /** Stop measuring `paths`, or everything for this console without them. */
  async cancel(addr: string, paths?: string[]): Promise<MeasureQueue> {
    return asQueue(await call("/cancel", paths ? { addr, paths } : { addr }));
  },
  async queue(addr: string): Promise<MeasureQueue> {
    return asQueue(await call(`/queue?addr=${encodeURIComponent(addr)}`));
  },
};
