/**
 * Search by name with the helper's file index (/api/index/*, over INDEX_START /
 * STATUS / SEARCH / CANCEL). One index for the whole helper: built by a thread
 * of its own over up to 8 roots (at most 200,000 files and 32 MiB of paths, ten
 * folders deep, dot names skipped), cancellable between folders. A search
 * matches the whole file NAME against a pattern (* and ?, ASCII
 * case-insensitive); a plain word is looked for anywhere in the name.
 *
 * The flow (createSearchFlow) is a small state machine with injected calls, so
 * the page's behaviour is tested without a DOM: first use builds the index for
 * the scope (progress + Cancel), then searches; a later search reuses an index
 * that covers the scope.
 */
import { ApiError, call } from "../../api";
import { isDrive, type Volume } from "../../home/Home";
import { baseName, parentPath } from "../move";

export interface IndexStatus {
  phase: "idle" | "building" | "ready";
  files: number;
  truncated: boolean;
  /** The page stopped the build: what it found is all there is. */
  cancelled: boolean;
  /** The roots the page built this index for; null when it is not the page's (never built, or another client's). */
  roots: string[] | null;
}

export interface Hit {
  path: string;
  name: string;
  folder: string;
  size: number;
}

export interface Results {
  hits: Hit[];
  /** There may be more than these (the helper's limit, or the page's 500). */
  truncated: boolean;
}

/** A root or pattern the helper's scan can carry: no quote, backslash or control character, < 256 bytes. */
function plain(s: string): boolean {
  if (!s) return false;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 0x20 || c === 0x7f || c === 0x22 || c === 0x5c) return false;
  }
  return new TextEncoder().encode(s).length < 256;
}

/**
 * What the field holds as the helper's pattern: as written when it has * or ?,
 * else "*word*" (anywhere in the name). null: empty, or a character the helper
 * cannot take (" \ or a control character), or too long.
 */
export function namePattern(text: string): string | null {
  const q = text.trim();
  if (!q) return null;
  // A run of stars means one star (fewer backtrack points for the helper's matcher).
  const p = (/[*?]/.test(q) ? q : `*${q}*`).replace(/\*{2,}/g, "*");
  return plain(p) ? p : null;
}

/** The drives the drive list shows (internal, M.2/extended, USB), as index roots (at most 8). */
export function driveRoots(volumes: Volume[]): string[] {
  return volumes
    .filter(isDrive)
    .map((v) => v.path.replace(/\/+$/, "") || "/")
    .filter(plain)
    .slice(0, 8);
}

/** `path` is `root` or inside it. */
export function isUnder(path: string, root: string): boolean {
  const r = root.replace(/\/+$/, "");
  if (r === "") return true;
  return path === r || path.startsWith(`${r}/`);
}

/** The index (its roots) covers every one of `scope`'s roots. */
export function covers(roots: string[] | null, scope: string[]): boolean {
  if (!roots || !roots.length || !scope.length) return false;
  return scope.every((s) => roots.some((r) => isUnder(s, r)));
}

/** /api/index/status's reply, checked. */
export function toStatus(raw: unknown): IndexStatus | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  if (o.phase !== "idle" && o.phase !== "building" && o.phase !== "ready") return null;
  return {
    phase: o.phase,
    files: typeof o.files === "number" && o.files >= 0 ? o.files : 0,
    truncated: o.truncated === true,
    cancelled: o.cancelled === true,
    roots: Array.isArray(o.roots) ? o.roots.filter((r): r is string => typeof r === "string") : null,
  };
}

/** /api/index/search's reply as hits (entries without a usable path are left out). */
export function toResults(raw: unknown): Results {
  const o = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
  const list = Array.isArray(o.results) ? o.results : [];
  const hits: Hit[] = [];
  for (const r of list) {
    if (!r || typeof r !== "object") continue;
    const x = r as Record<string, unknown>;
    if (typeof x.path !== "string" || !x.path.startsWith("/")) continue;
    const name = baseName(x.path);
    if (!name) continue;
    hits.push({ path: x.path, name, folder: parentPath(x.path), size: typeof x.size === "number" ? x.size : 0 });
  }
  return { hits, truncated: o.truncated === true };
}

export interface SearchApi {
  status(): Promise<IndexStatus>;
  start(roots: string[]): Promise<void>;
  cancel(): Promise<void>;
  search(pattern: string, under: string | null): Promise<Results>;
}

export const searchApi: SearchApi = {
  async status() {
    const s = toStatus(await call<unknown>("/api/index/status"));
    if (!s) throw new ApiError(500, "bad_index_reply");
    return s;
  },
  async start(roots) {
    await call("/api/index/start", { roots });
  },
  async cancel() {
    await call("/api/index/cancel");
  },
  async search(pattern, under) {
    return toResults(await call<unknown>("/api/index/search", under ? { query: pattern, under } : { query: pattern }));
  },
};

/* ---- the flow ---- */

export type Scope = "all" | "folder";

export interface SearchState {
  /** null until the first status read. */
  index: IndexStatus | null;
  /** The helper has no index routes (an older helper). */
  unsupported: boolean;
  /** The last call's failure (shown as a line), or null. */
  error: unknown;
  /** The search that runs once the index is built (the field's pattern), or null. */
  pending: string | null;
  /** A search request is running. */
  searching: boolean;
  /** The last search's pattern and scope, with its results. */
  done: { pattern: string; scope: Scope; results: Results } | null;
  /** Cancel was asked and the build has not stopped yet. */
  stopping: boolean;
}

export const INITIAL_SEARCH: SearchState = {
  index: null,
  unsupported: false,
  error: null,
  pending: null,
  searching: false,
  done: null,
  stopping: false,
};

/** How often the status is read while the index builds. */
export const POLL_MS = 1000;

export interface FlowDeps {
  api: SearchApi;
  setTimer: (fn: () => void, ms: number) => unknown;
  clearTimer: (t: unknown) => void;
}

export interface SearchFlow {
  state(): SearchState;
  /** Read the index status (on open); polls while it builds. */
  open(): Promise<void>;
  /**
   * Search for `text` in `scope` (`roots` its index roots, `under` the folder
   * to search within, or null for every root): builds the index first when the
   * one there does not cover `roots`.
   */
  submit(text: string, scope: Scope, roots: string[], under: string | null): Promise<void>;
  /** Build the index again for `roots` (results may be stale). */
  rebuild(roots: string[]): Promise<void>;
  /** Stop the build: what it found so far stays searchable. */
  cancel(): Promise<void>;
  /** The dialog closed: stop polling and forget the pending search (a build keeps running in the helper). */
  close(): void;
}

const isMissingRoute = (e: unknown) => e instanceof ApiError && e.status === 404 && e.code === "not_found";

export function createSearchFlow(deps: FlowDeps, onChange: (s: SearchState) => void): SearchFlow {
  let s: SearchState = INITIAL_SEARCH;
  let timer: unknown = null;
  let closed = false;
  /** The scope the pending search runs in once the build ends. */
  let pendingScope: { scope: Scope; under: string | null; roots: string[]; built: boolean } | null = null;
  const set = (patch: Partial<SearchState>) => {
    if (closed) return;
    s = { ...s, ...patch };
    onChange(s);
  };
  const stopPoll = () => {
    if (timer != null) deps.clearTimer(timer);
    timer = null;
  };

  const runSearch = async (pattern: string, scope: Scope, under: string | null) => {
    set({ searching: true, error: null, pending: null });
    try {
      const results = await deps.api.search(pattern, under);
      set({ searching: false, done: { pattern, scope, results } });
    } catch (e) {
      set({ searching: false, error: e });
    }
  };

  /** Status reads started: only the latest one's answer counts. A search sent while the first read
   *  was out started a build; that first, older "not built" answer then ran the search on an index
   *  not built yet ("nothing found"), and the search was gone when the build ended. */
  let reads = 0;
  const poll = async (): Promise<void> => {
    timer = null;
    if (closed) return;
    const read = ++reads;
    let st: IndexStatus;
    try {
      st = await deps.api.status();
    } catch (e) {
      if (read !== reads) return;
      if (isMissingRoute(e)) return set({ unsupported: true });
      set({ error: e });
      // A read that failed (the helper busy, a blip) is tried again while the build may still run.
      if (!closed) timer = deps.setTimer(() => void poll(), POLL_MS);
      return;
    }
    if (read !== reads) return;
    set({ index: st, error: null, ...(st.phase !== "building" ? { stopping: false } : {}) });
    if (st.phase === "building") {
      if (!closed) timer = deps.setTimer(() => void poll(), POLL_MS);
      return;
    }
    const pending = s.pending;
    const where = pendingScope;
    pendingScope = null;
    if (!pending || !where) return;
    // The build this search asked for (or one that covers it) ended: search. A build that was
    // someone else's and does not cover the scope: build once for it, then search.
    if (where.built || covers(st.roots, where.roots)) await runSearch(pending, where.scope, where.under);
    else {
      pendingScope = { ...where, built: true };
      await build(where.roots);
    }
  };

  const build = async (roots: string[]): Promise<boolean> => {
    stopPoll();
    try {
      await deps.api.start(roots);
    } catch (e) {
      // Already building (another start won the race): wait for that one instead.
      if (!(e instanceof ApiError && e.status === 409)) {
        pendingScope = null;
        if (isMissingRoute(e)) set({ unsupported: true, pending: null });
        else set({ error: e, pending: null });
        return false;
      }
    }
    set({ index: { phase: "building", files: 0, truncated: false, cancelled: false, roots }, stopping: false, done: null });
    await poll();
    return true;
  };

  return {
    state: () => s,
    async open() {
      closed = false;
      await poll();
    },
    async submit(text, scope, roots, under) {
      const pattern = namePattern(text);
      if (!pattern || s.unsupported) return;
      set({ error: null });
      const idx = s.index;
      if (idx?.phase === "building") {
        // Searched while it builds: run it when the build ends (the latest one wins).
        pendingScope = { scope, under, roots, built: false };
        set({ pending: pattern });
        return;
      }
      if (idx?.phase === "ready" && covers(idx.roots, roots)) {
        await runSearch(pattern, scope, under);
        return;
      }
      pendingScope = { scope, under, roots, built: true };
      set({ pending: pattern });
      await build(roots);
    },
    async rebuild(roots) {
      if (s.index?.phase === "building") return;
      await build(roots);
    },
    async cancel() {
      if (s.index?.phase !== "building") return;
      set({ stopping: true, pending: null });
      pendingScope = null;
      try {
        await deps.api.cancel();
      } catch (e) {
        set({ stopping: false, error: e });
      }
    },
    close() {
      closed = true;
      stopPoll();
      pendingScope = null;
    },
  };
}
