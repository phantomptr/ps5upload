/**
 * Copy, move, delete and rename for Files.
 *
 * Destinations: the helper's JOB_COPY takes `dest` as the copied item's own
 * new path (third_party/ava1/c/ava1_copy.c: `dest` is the job's root, refused with
 * "exists" when present; the PC app's engine sends the full target path,
 * engine/crates/ps5upload-ava1/src/copy.rs), never the folder to copy into.
 * Likewise rename's `to` is the full new path. So every item goes to
 * <destDir>/<its name>.
 *
 * One item at a time: JOB_COPY walks the source before it answers, on one of
 * the helper's four web workers, and the helper's job table (32 jobs, shared
 * with every paired PC) must not fill with one batch. So an item's request is
 * sent only after the previous item's job has left "running" (waitJob).
 */
import { ApiError, call } from "../api";

export function joinPath(dir: string, name: string): string {
  const d = dir.replace(/\/+$/, "");
  return `${d}/${name}`;
}

export function parentPath(path: string): string {
  const p = path.replace(/\/+$/, "");
  const i = p.lastIndexOf("/");
  return i <= 0 ? "/" : p.slice(0, i);
}

export function baseName(path: string): string {
  const p = path.replace(/\/+$/, "");
  return p.slice(p.lastIndexOf("/") + 1);
}

/** A folder cannot be copied or moved into itself or one of its subfolders. */
export class IntoItselfError extends Error {
  constructor(item: string) {
    super(`into_itself: ${item}`);
    this.name = "IntoItselfError";
  }
}

/** `destDir` is `item` or inside it (checked on the page, before any request). */
export function isInside(destDir: string, item: string): boolean {
  const d = destDir.replace(/\/+$/, "") || "/";
  const i = item.replace(/\/+$/, "") || "/";
  return d === i || d.startsWith(i === "/" ? "/" : `${i}/`);
}

export interface Failure {
  item: string;
  error: unknown;
}

export interface BatchResult {
  /** Jobs the helper started (copies, cross-drive moves, deletes). */
  started: string[];
  /** Items moved at once by rename (same drive). */
  moved: string[];
  /** Requests the helper refused (nothing started for these). */
  failed: Failure[];
  /** Items whose job started and then ended failed. */
  jobFailed: string[];
  /** Items never sent: the batch was stopped (or the token went away) first. */
  skipped: string[];
  /** Items whose running job Stop cancelled: what it already did stays. */
  cancelled?: string[];
}

/**
 * How a started job was left: its end state, gone from the list, "cancelled" (Stop cancelled it) or
 * "left" (the batch stopped and the job runs on: it could not be cancelled).
 */
export type JobEnd = "done" | "failed" | "gone" | "left" | "cancelled";

type Progress = (at: number, total: number) => void;

export interface BatchOptions {
  /** Item `at` (1-based) of `total` is being sent. */
  onProgress?: Progress;
  /**
   * Resolves once job `id` is no longer running; without it the next item goes at once. Stop cancels
   * the job waited on, unless `unstoppable`: a Replace's clean-up, which must finish whatever Stop says.
   */
  waitJob?: (id: string, unstoppable?: boolean) => Promise<JobEnd>;
  /** Checked before each item: true stops the batch (the rest are skipped). */
  stopped?: () => boolean;
}

/** Run `one` for each item in turn (see BatchOptions); its returned job id is waited on before the next. */
export async function each(
  items: string[],
  opts: BatchOptions | undefined,
  one: (item: string, r: BatchResult) => Promise<string | void>,
): Promise<BatchResult> {
  const r: BatchResult = { started: [], moved: [], failed: [], jobFailed: [], skipped: [] };
  for (let i = 0; i < items.length; i++) {
    if (opts?.stopped?.()) {
      r.skipped = items.slice(i);
      break;
    }
    opts?.onProgress?.(i + 1, items.length);
    let job: string | void;
    try {
      job = await one(items[i], r);
    } catch (error) {
      r.failed.push({ item: items[i], error });
      // The token is gone: every other request would 401 too.
      if (error instanceof ApiError && error.status === 401) {
        r.skipped = items.slice(i + 1);
        break;
      }
      continue;
    }
    if (!job || !opts?.waitJob) continue;
    try {
      const end = await opts.waitJob(job);
      if (end === "failed") r.jobFailed.push(items[i]);
      else if (end === "cancelled") r.cancelled = (r.cancelled ?? []).concat(items[i]);
    } catch (error) {
      // The waiter gives up only when the token is gone (401).
      r.failed.push({ item: items[i], error });
      r.skipped = items.slice(i + 1);
      break;
    }
  }
  return r;
}

/** Start a copy job (move: the source goes once copied; overwrite: an existing destination FILE is replaced). */
export async function startCopy(src: string, dest: string, move: boolean, r: BatchResult, overwrite = false): Promise<string> {
  const { job } = await call<{ job: string }>("/api/fs/copy", overwrite ? { src, dest, move, overwrite } : { src, dest, move });
  r.started.push(job);
  return job;
}

/**
 * Move each item into `destDir`: a rename on the same drive; across drives the
 * helper refuses the rename (409 cross_mount, a cross-mount rename() panics the
 * kernel) and a copy job with move set does it instead.
 */
export async function moveItems(items: string[], destDir: string, opts?: BatchOptions): Promise<BatchResult> {
  return each(items, opts, async (item, r) => {
    if (isInside(destDir, item)) throw new IntoItselfError(item);
    if (parentPath(item) === destDir.replace(/\/+$/, "")) return; // already there
    const to = joinPath(destDir, baseName(item));
    try {
      await call("/api/fs/rename", { from: item, to });
      r.moved.push(item);
    } catch (e) {
      if (!(e instanceof ApiError && e.code === "cross_mount")) throw e;
      return startCopy(item, to, true, r);
    }
  });
}

export async function copyItems(items: string[], destDir: string, opts?: BatchOptions): Promise<BatchResult> {
  return each(items, opts, async (item, r) => {
    if (isInside(destDir, item)) throw new IntoItselfError(item);
    return startCopy(item, joinPath(destDir, baseName(item)), false, r);
  });
}

export async function deleteItems(items: string[], opts?: BatchOptions): Promise<BatchResult> {
  return each(items, opts, async (item, r) => {
    const { job } = await call<{ job: string }>("/api/fs/delete", { path: item });
    r.started.push(job);
    return job;
  });
}

/** Fix permissions (chmod 777, the helper fixes the mode): one job per item. */
export async function fixPermItems(items: string[], opts?: BatchOptions): Promise<BatchResult> {
  return each(items, opts, async (item, r) => {
    const { job } = await call<{ job: string }>("/api/fs/chmod", { path: item });
    r.started.push(job);
    return job;
  });
}

/** Rename `name` in `dir` to `newName` (the helper never overwrites: 409 when taken). */
export async function renameEntry(dir: string, name: string, newName: string): Promise<void> {
  await call("/api/fs/rename", { from: joinPath(dir, name), to: joinPath(dir, newName) });
}

/** A name the page can send: not empty, no "/", not "." or "..". */
export function validName(name: string): boolean {
  return name.length > 0 && name.indexOf("/") < 0 && name !== "." && name !== ".." && name.indexOf("\0") < 0;
}

/** The drive (volume path) holding `path`: the longest volume path it starts with. */
export function volumeOf<V extends { path: string }>(path: string, volumes: V[]): V | null {
  let best: V | null = null;
  for (const v of volumes) {
    const root = v.path.replace(/\/+$/, "");
    if ((path === root || path.startsWith(`${root}/`)) && (!best || root.length > best.path.length)) best = v;
  }
  return best;
}

/** One level up from `path`: its parent, or null (the drive list) from a drive's top or "/". */
export function upFrom(path: string | null, volumes: { path: string }[]): string | null {
  if (path == null) return null;
  const p = path.replace(/\/+$/, "") || "/";
  if (p === "/" || volumes.some((v) => v.path.replace(/\/+$/, "") === p)) return null;
  return parentPath(p);
}
