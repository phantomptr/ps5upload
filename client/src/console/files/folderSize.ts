/**
 * "Calculate folder size": walk a folder with /api/fs/list paging and count
 * its files, folders and bytes. Bounded: at most SIZE_MAX_ENTRIES entries and
 * SIZE_MAX_MS of walking (it then stops and says the count is partial), and
 * cancellable between pages. One walk at a time, page-wide (startFolderSize
 * refuses a second), so two Info dialogs never double the load on the helper.
 *
 * Links are counted as files without bytes and never followed; a subfolder
 * that cannot be read is counted and skipped.
 */
import { ApiError, call } from "../api";
import { joinPath } from "./move";
import { LIST_PAGE, type Entry } from "./useDir";

export const SIZE_MAX_ENTRIES = 200_000;
export const SIZE_MAX_MS = 60_000;

export interface SizeCount {
  files: number;
  folders: number;
  bytes: number;
  /** Entries seen (files + folders + others). */
  entries: number;
  /** Subfolders that could not be read. */
  unreadable: number;
}

/** How a walk ended: complete, or stopped at the entry cap, the time cap, by Cancel, or the folder itself failed. */
export type SizeEnd = "done" | "capped" | "timeout" | "cancelled" | "error";

export interface SizeResult extends SizeCount {
  end: SizeEnd;
  error?: unknown;
}

export interface ListPage {
  entries?: Entry[];
  more?: boolean;
  next?: number;
}

export interface WalkDeps {
  list: (path: string, offset: number, limit: number) => Promise<ListPage>;
  now: () => number;
}

export interface WalkOptions {
  cancelled: () => boolean;
  onProgress?: (c: SizeCount) => void;
  /** Each folder found, with the folder it is in (Uninstall looks for a game inside a game). */
  onDir?: (parent: string, name: string) => void;
  maxEntries?: number;
  maxMs?: number;
}

const defaultDeps: WalkDeps = {
  list: (path, offset, limit) => call<ListPage>("/api/fs/list", { path, offset, limit }),
  now: () => Date.now(),
};

/** Walk `root` breadth-first (see the module comment). Never throws: a failure is `end: "error"`. */
export async function walkFolder(root: string, opts: WalkOptions, deps: WalkDeps = defaultDeps): Promise<SizeResult> {
  const maxEntries = opts.maxEntries ?? SIZE_MAX_ENTRIES;
  const maxMs = opts.maxMs ?? SIZE_MAX_MS;
  const c: SizeCount = { files: 0, folders: 0, bytes: 0, entries: 0, unreadable: 0 };
  const queue: string[] = [root];
  const started = deps.now();
  const end = (e: SizeEnd, error?: unknown): SizeResult => (error === undefined ? { ...c, end: e } : { ...c, end: e, error });
  for (let head = 0; head < queue.length; head++) {
    const folder = queue[head];
    // Drop what was walked: the queue holds only folders still to read.
    if (head >= 1024) {
      queue.splice(0, head);
      head = 0;
    }
    let offset = 0;
    for (;;) {
      if (opts.cancelled()) return end("cancelled");
      if (deps.now() - started > maxMs) return end("timeout");
      let page: ListPage;
      try {
        page = await deps.list(folder, offset, LIST_PAGE);
      } catch (e) {
        // The token is gone, or the folder asked about itself failed: stop with the error.
        if (folder === root || (e instanceof ApiError && e.status === 401)) return end("error", e);
        c.unreadable++;
        break;
      }
      if (opts.cancelled()) return end("cancelled");
      const entries = Array.isArray(page?.entries) ? page.entries : [];
      for (const e of entries) {
        c.entries++;
        if (e.kind === "dir") {
          c.folders++;
          opts.onDir?.(folder, e.name);
          queue.push(joinPath(folder, e.name));
        } else {
          c.files++;
          if (e.kind === "file" && typeof e.size === "number" && e.size > 0) c.bytes += e.size;
        }
        if (c.entries >= maxEntries) {
          opts.onProgress?.({ ...c });
          return end("capped");
        }
      }
      opts.onProgress?.({ ...c });
      const next = typeof page?.next === "number" ? page.next : offset + entries.length;
      // No more pages, or a page that did not move on (never loop on it).
      if (!page?.more || next <= offset) break;
      offset = next;
    }
  }
  return end("done");
}

/* ---- the one walk ---- */

let running: { cancel: () => void } | null = null;

/** Another page-wide walk (Disk usage) holds the walker: folder size waits for it. */
let held = false;

/** A walk runs now (another Info dialog's, or Disk usage's). */
export function folderSizeRunning(): boolean {
  return running != null || held;
}

/**
 * Take the page's one walk for another tool (Disk usage): null while a folder
 * size walk (or another holder) runs, else the function that gives it back.
 */
export function holdWalk(): (() => void) | null {
  if (running || held) return null;
  held = true;
  let given = false;
  return () => {
    if (given) return;
    given = true;
    held = false;
  };
}

/**
 * Start the page's one folder-size walk of `root`: null while another runs.
 * `cancel()` stops it before its next page.
 */
export function startFolderSize(
  root: string,
  onProgress: (c: SizeCount) => void,
  deps?: WalkDeps,
  limits?: { maxEntries?: number; maxMs?: number },
): { done: Promise<SizeResult>; cancel: () => void } | null {
  if (running || held) return null;
  let cancelled = false;
  const me = {
    cancel: () => {
      cancelled = true;
    },
  };
  running = me;
  const done = walkFolder(root, { cancelled: () => cancelled, onProgress, ...limits }, deps).finally(() => {
    if (running === me) running = null;
  });
  return { done, cancel: me.cancel };
}
