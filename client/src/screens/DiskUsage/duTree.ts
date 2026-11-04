// Disk usage's data side (#495): the engine walks a console folder and answers with every
// folder below it, sized recursively (engine/crates/ps5upload-engine/src/disk_usage.rs).
// This file runs that walk (start, poll, cancel) and keeps each console's finished walks
// for the session, so drilling into a folder a parent walk already covered is instant.
import { invoke } from "../../lib/invokeLogged";

export interface DuFile {
  name: string;
  size: number;
}

/** One folder: `bytes`/`files`/`dirs` count everything below it. */
export interface DuDir {
  name: string;
  bytes: number;
  files: number;
  dirs: number;
  /** Folders at or below this one that were not read. */
  skipped: number;
  /** Why this folder itself was not read. */
  note?: "unreadable" | "other_drive" | "system" | "not_scanned";
  children: DuDir[];
  /** The largest files directly inside; `more_*` sums the rest. */
  top_files: DuFile[];
  more_files: number;
  more_bytes: number;
}

export interface DuStatus {
  id: string;
  path: string;
  state: "running" | "done" | "cancelled" | "failed";
  dirs_scanned: number;
  files: number;
  bytes: number;
  skipped: number;
  elapsed_ms: number;
  /** Why the walk stopped early, when it did. */
  partial?: "cancelled" | "folder_limit" | "time_limit";
  error?: string;
  tree?: DuDir;
}

/** What the screen draws: a folder or a file, or the folder's other files summed. */
export interface DuBlock {
  name: string;
  isDir: boolean;
  totalSize: number;
  note?: DuDir["note"];
  /** Set on the summed block: how many files it stands for. */
  count?: number;
}

/** The blocks of one folder, largest first. */
export function blocksOf(dir: DuDir): DuBlock[] {
  const out: DuBlock[] = [
    ...dir.children.map((c) => ({ name: c.name, isDir: true, totalSize: c.bytes, note: c.note })),
    ...dir.top_files.map((f) => ({ name: f.name, isDir: false, totalSize: f.size })),
  ];
  out.sort((a, b) => b.totalSize - a.totalSize);
  if (dir.more_files > 0) {
    out.push({ name: "", isDir: false, totalSize: dir.more_bytes, count: dir.more_files });
  }
  return out;
}

export function normalizePath(path: string): string {
  const t = path.replace(/\/+$/, "");
  return t === "" ? "/" : t;
}

/** `path`'s folder inside a walk of `root`, or null when the walk does not reach it (or did
 *  not read it: a skipped folder has no sizes to show). */
export function findSubtree(tree: DuDir, root: string, path: string): DuDir | null {
  const r = normalizePath(root);
  const p = normalizePath(path);
  if (p === r) return tree;
  const prefix = r === "/" ? "/" : `${r}/`;
  if (!p.startsWith(prefix)) return null;
  let node: DuDir | undefined = tree;
  for (const seg of p.slice(prefix.length).split("/")) {
    node = node.children.find((c) => c.name === seg);
    if (!node) return null;
  }
  return node.note ? null : node;
}

interface CachedWalk {
  path: string;
  tree: DuDir;
  partial?: DuStatus["partial"];
}

/** Finished walks per console, for this session. A cancelled walk is never kept. */
const cache = new Map<string, CachedWalk[]>();

export function rememberWalk(host: string, s: DuStatus): void {
  if (!s.tree || s.state !== "done") return;
  const path = normalizePath(s.path);
  const list = (cache.get(host) ?? []).filter((w) => w.path !== path);
  list.push({ path, tree: s.tree, partial: s.partial });
  cache.set(host, list);
}

/** The cached folder for `path`, from the walk of it or of any folder above it. */
export function cachedFolder(
  host: string,
  path: string,
): { dir: DuDir; partial?: DuStatus["partial"] } | null {
  // The deepest covering walk first: it is the most recent look at that part of the tree.
  const walks = [...(cache.get(host) ?? [])].sort((a, b) => b.path.length - a.path.length);
  for (const w of walks) {
    const dir = findSubtree(w.tree, w.path, path);
    if (dir) return { dir, partial: w.partial };
  }
  return null;
}

/** Drops the walks that cover `path` or lie inside it, so a refresh really asks again. */
export function forgetWalks(host: string, path: string): void {
  const p = normalizePath(path);
  const within = (a: string, b: string) => a === b || a.startsWith(b === "/" ? "/" : `${b}/`);
  const list = (cache.get(host) ?? []).filter((w) => !within(p, w.path) && !within(w.path, p));
  cache.set(host, list);
}

export function clearWalkCache(): void {
  cache.clear();
}

export const POLL_MS = 500;

export const sleep = (ms: number, signal: AbortSignal) =>
  new Promise<void>((resolve) => {
    // One listener per poll, taken off again: a walk of a few minutes would otherwise pile up
    // hundreds of them on this one signal.
    const onAbort = () => {
      clearTimeout(t);
      resolve();
    };
    const t = setTimeout(() => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    signal.addEventListener("abort", onAbort, { once: true });
  });

/** The engine's refusal while another folder on this console is being walked. */
export function isWalkBusy(message: string): boolean {
  return message.includes("disk_usage_busy");
}

/** How many times a start is retried while the walk before it stops. Picking another folder
 *  cancels the old walk and starts the new one at once, and either can reach the engine first. */
const START_TRIES = 8;

async function startWalk(addr: string, path: string, signal: AbortSignal): Promise<DuStatus | null> {
  for (let tries = 1; ; tries++) {
    try {
      return await invoke<DuStatus>("ps5_disk_usage_start", { addr, path });
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      if (!isWalkBusy(message) || tries >= START_TRIES) throw e;
    }
    await sleep(POLL_MS, signal);
    if (signal.aborted) return null;
  }
}

/**
 * Walks `path` on the console at `addr`, reporting each poll to `onProgress`, and resolves
 * with the final status. Aborting `signal` cancels the engine's walk and resolves null.
 */
export async function runWalk(
  addr: string,
  path: string,
  onProgress: (s: DuStatus) => void,
  signal: AbortSignal,
): Promise<DuStatus | null> {
  const started = await startWalk(addr, path, signal);
  if (!started) return null;
  let s = started;
  const cancel = () => {
    void invoke("ps5_disk_usage_cancel", { id: s.id }).catch(() => {});
  };
  for (;;) {
    if (signal.aborted) {
      cancel();
      return null;
    }
    if (s.state !== "running") return s;
    onProgress(s);
    await sleep(POLL_MS, signal);
    if (signal.aborted) {
      cancel();
      return null;
    }
    s = await invoke<DuStatus>("ps5_disk_usage_status", { id: s.id });
  }
}
