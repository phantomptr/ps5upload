import { create } from "zustand";

/**
 * Cross-screen log of every operation the user has triggered: uploads,
 * downloads, FS bulk ops, Library actions. Persists the last
 * `MAX_ENTRIES` to localStorage so the Activity tab survives reloads
 * and remounts.
 *
 * Why a separate store rather than aggregating from the per-feature
 * stores at read time:
 *   - The per-feature stores reset on idle, so a successful upload
 *     leaves no trace once the banner clears. The user has no way to
 *     answer "did that thing I started 10 minutes ago actually
 *     finish?" without scrolling back through engine logs.
 *   - The Activity tab needs *historical* outcomes (failed/done) the
 *     per-feature stores actively forget.
 *
 * Lifecycle: callers `start()` an entry when they kick off the op,
 * `update()` it with progress or label refinements, and `finish()` it
 * on terminal transition. The id returned by `start()` is the handle.
 */

const STORAGE_KEY = "ps5upload.activityHistory";
const MAX_ENTRIES = 100;

export type ActivityKind =
  | "upload"
  | "upload-dir"
  | "upload-reconcile"
  | "download"
  | "fs-delete"
  | "fs-paste-copy"
  | "fs-paste-move"
  | "library-delete"
  | "library-move"
  | "library-chmod"
  | "library-mount"
  | "library-unmount"
  | "library-download";

export type ActivityOutcome = "running" | "done" | "failed" | "stopped";

export interface ActivityEntry {
  id: string;
  kind: ActivityKind;
  /** Short user-readable headline (e.g. "Copy PPSA09519.exfat"). */
  label: string;
  /** Optional second line — typically From/To paths or the dest dir. */
  detail?: string;
  /** Source path (where data came from on the PS5). Stored
   *  separately from `detail` so the Activity row can render
   *  "From: …" and "To: …" on their own lines. */
  fromPath?: string;
  /** Destination path. Same rationale as `fromPath`. */
  toPath?: string;
  startedAtMs: number;
  /** null while outcome === "running". */
  endedAtMs: number | null;
  outcome: ActivityOutcome;
  /** Last-known error string if outcome === "failed" or "stopped". */
  error?: string;
  /** Optional progress fields for currently-running entries. */
  bytes?: number;
  totalBytes?: number;
  /** Files involved (count). Helps the Activity row summarize a
   *  bulk op as "Copying 3 items, 12.4 GiB total" instead of a
   *  bare "Copying 3 items". */
  files?: number;
  /** When set, the activity row knows how to actually cancel the
   *  in-flight op via fsOpCancel — required for Library row ops
   *  whose state isn't in any global Zustand store the row can
   *  reach. The `addr` is the transfer-port host:port the op was
   *  fired against, used to route the cancel call. */
  opId?: number;
  addr?: string;
}

interface ActivityHistoryState {
  entries: ActivityEntry[];
  /** Begin a new entry. Returns its id so the caller can pair the
   *  `finish()` call later. */
  start: (
    kind: ActivityKind,
    label: string,
    extras?: Partial<Omit<ActivityEntry, "id" | "kind" | "label" | "startedAtMs" | "endedAtMs" | "outcome">>,
  ) => string;
  /** Patch an in-flight entry — typically progress numbers. No-op
   *  if the id has been evicted or already finished. */
  update: (id: string, patch: Partial<Omit<ActivityEntry, "id" | "kind">>) => void;
  /** Move an entry to a terminal state. Idempotent — finishing an
   *  already-finished entry replaces its outcome (useful when, e.g.,
   *  a "stopped" wins out over an in-flight "running"). */
  finish: (
    id: string,
    outcome: Exclude<ActivityOutcome, "running">,
    extras?: Partial<Pick<ActivityEntry, "error" | "bytes" | "totalBytes" | "detail">>,
  ) => void;
  /** Drop everything. The Activity tab's "Clear" button calls this. */
  clear: () => void;
  /** Force every still-`running` entry into a terminal `stopped`
   *  state with a synthetic note. Use case: rows that got orphaned
   *  because their underlying op died without firing `finish()` —
   *  the desktop process was killed, the engine restarted, the
   *  payload disconnected mid-poll, etc. Without this the OperationBar
   *  shows a forever-running ghost until the user restarts the app
   *  (which `loadInitial` then converts via the same logic). Past
   *  entries are left alone — only running rows get touched. */
  clearRunning: () => void;
  /** All entries currently in `running` state — the OperationBar
   *  reads this to show the global in-flight indicator. Computed
   *  in-place so callers don't need a selector. */
  runningEntries: () => ActivityEntry[];
}

function loadInitial(): ActivityEntry[] {
  if (typeof window === "undefined") return [];
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    // Defensive: any entry that was "running" when the app last
    // closed is now stale (the underlying op didn't actually keep
    // running). Mark those as "stopped" with a synthetic note so the
    // history stays accurate. Without this, restarting the app
    // leaves the OperationBar showing forever-running ghosts.
    return parsed
      .filter((e: unknown): e is ActivityEntry =>
        typeof e === "object" && e !== null && typeof (e as ActivityEntry).id === "string",
      )
      .map((e: ActivityEntry) =>
        e.outcome === "running"
          ? {
              ...e,
              outcome: "stopped" as ActivityOutcome,
              endedAtMs: e.endedAtMs ?? Date.now(),
              error: e.error ?? "ended when the app closed",
            }
          : e,
      )
      .slice(0, MAX_ENTRIES);
  } catch {
    return [];
  }
}

function persist(entries: ActivityEntry[]) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(entries));
  } catch {
    // localStorage write can fail (quota exceeded, private mode).
    // Activity history is nice-to-have; swallow rather than crash.
  }
}

let nextIdCounter = 0;
function nextId(): string {
  // Time-prefixed counter — uniqueness across the same render frame
  // (which Date.now() alone can't guarantee) and across sessions
  // (the counter resets but Date.now() doesn't).
  return `${Date.now().toString(36)}-${(nextIdCounter++).toString(36)}`;
}

export const useActivityHistoryStore = create<ActivityHistoryState>(
  (set, get) => ({
    entries: loadInitial(),

    start(kind, label, extras) {
      const id = nextId();
      const entry: ActivityEntry = {
        id,
        kind,
        label,
        startedAtMs: Date.now(),
        endedAtMs: null,
        outcome: "running",
        ...(extras ?? {}),
      };
      set((s) => {
        const next = [entry, ...s.entries].slice(0, MAX_ENTRIES);
        persist(next);
        return { entries: next };
      });
      return id;
    },

    update(id, patch) {
      set((s) => {
        let changed = false;
        const next = s.entries.map((e) => {
          if (e.id !== id) return e;
          changed = true;
          return { ...e, ...patch };
        });
        if (!changed) return s;
        persist(next);
        return { entries: next };
      });
    },

    finish(id, outcome, extras) {
      set((s) => {
        let changed = false;
        const next = s.entries.map((e) => {
          if (e.id !== id) return e;
          changed = true;
          return {
            ...e,
            ...(extras ?? {}),
            outcome,
            endedAtMs: Date.now(),
          };
        });
        if (!changed) return s;
        persist(next);
        return { entries: next };
      });
    },

    clear() {
      persist([]);
      set({ entries: [] });
    },

    clearRunning() {
      set((s) => {
        const now = Date.now();
        let changed = false;
        const next = s.entries.map((e) => {
          if (e.outcome !== "running") return e;
          changed = true;
          return {
            ...e,
            outcome: "stopped" as ActivityOutcome,
            endedAtMs: e.endedAtMs ?? now,
            error: e.error ?? "cleared manually (orphaned running entry)",
          };
        });
        if (!changed) return s;
        persist(next);
        return { entries: next };
      });
    },

    runningEntries() {
      return get().entries.filter((e) => e.outcome === "running");
    },
  }),
);
