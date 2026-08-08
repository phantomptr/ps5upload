/**
 * Unified Task System (v5 §10).
 *
 * The headline gap in v4: three parallel job/status systems (transfer store,
 * upload queue, pkgLibrary, activityHistory) with no unified model. This
 * store provides **one** `Task` envelope that wraps every long-running
 * operation — uploads, downloads, filesystem ops, installs, backups,
 * enrichment, library ops, pipelines.
 *
 * Design: this store is a **facade** over the existing per-feature stores.
 * It does NOT replace them (that would be a breaking migration). Instead,
 * feature code calls `registerTask()` when starting an op, `updateTask()`
 * as progress arrives, and `finishTask()` on terminal transition. The
 * existing stores continue to own their engine-polling loops; this store
 * holds the unified identity + lifecycle + history that the spec requires.
 *
 * One lifecycle, one retry policy, one history, one task ID space.
 * The Task `id` is a ULID-like string that is stable across the client
 * (taskStore), the engine (linked via `engineJobId`), and history.
 *
 * Persistence: active + recently-finished tasks persist to localStorage
 * so the Tasks tab survives reloads. On reload, any task still in a
 * non-terminal state is marked "interrupted" (a flavor of failed) — the
 * underlying engine op didn't actually keep running.
 */

import { create } from "zustand";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { hostOf } from "../lib/addr";

// ---------------------------------------------------------------------------
// Types — §10.1
// ---------------------------------------------------------------------------

export type TaskKind =
  // transfers (engine: jobs map + polled)
  | "upload-file"
  | "upload-dir"
  | "upload-archive"
  | "download"
  // filesystem ops
  | "fs-delete"
  | "fs-copy"
  | "fs-move"
  | "fs-rename"
  // installs (engine + payload bgft_install_status)
  | "pkg-install"
  | "pkg-dpi-install"
  // backups (engine: snapshot/restore)
  | "backup-snapshot"
  | "backup-restore"
  | "save-backup"
  | "save-restore"
  // enrichment (lightweight client ops)
  | "tmdb-fetch"
  | "cheat-download"
  | "icon-fetch"
  // library ops
  | "library-mount"
  | "library-register"
  | "library-unregister"
  | "library-launch";

export type TaskStatus =
  | "queued"
  | "running"
  | "paused"
  | "awaiting"
  | "done"
  | "failed"
  | "cancelled"
  // synthetic terminal state assigned on reload to tasks that were
  // `running`/`queued`/`paused`/`awaiting` when the app closed — the
  // underlying engine op didn't survive the restart.
  | "interrupted";

export type TaskProgressUnit = "bytes" | "files" | "items";

export interface TaskProgress {
  current: number;
  total: number;
  unit: TaskProgressUnit;
}

export interface TaskRate {
  bytesPerSec: number;
  filesPerSec?: number;
}

export interface TaskError {
  code: string;
  message: string;
  recoverable: boolean;
}

/** Per-kind specifics. Kept loose (record of string→unknown) so each
 *  feature can store what it needs without forcing a union. The typed
 *  accessors below provide narrow views for the common kinds. */
export type TaskPayload = Record<string, unknown>;

export interface Task {
  id: string;
  kind: TaskKind;
  /** Where this task originated — e.g. "files.upload", "games.install",
   *  "schedule:nightly-backup". Drives the icon + grouping in Tasks tab. */
  origin: string;
  createdAt: string;
  status: TaskStatus;
  progress?: TaskProgress;
  rate?: TaskRate;
  eta?: number;
  lastError?: TaskError;
  attempts: number;
  maxAttempts: number;
  /** Bare host (port-stripped) of the PS5 this task runs against. */
  consoleId: string;
  /** The engine job_id / fsOp op_id / install tracking id — whichever
   *  the engine uses to identify the underlying operation. Linked here
   *  so cancel/retry/status can reach the right engine endpoint. */
  engineJobId?: string;
  payload: TaskPayload;
  /** Short user-readable headline (e.g. "Upload PPSA09519.exfat"). */
  label: string;
  /** Optional second line — typically From/To paths. */
  detail?: string;
  /** Timestamp (ms) of the last status change. Drives the "recently
   *  changed" sort in the Tasks tab and the stale-detection heuristic. */
  updatedAtMs: number;
  /** Timestamp (ms) when the task reached a terminal state. null while
   *  non-terminal. Persisted so history survives reloads. */
  endedAtMs?: number | null;
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

const STORAGE_KEY = "ps5upload.tasks.v1";
export const MAX_TASK_HISTORY = 200;

/** Bound both persisted and in-memory history while never evicting active
 * work. Tasks are newest-first, so terminal slots naturally keep the newest
 * results. If more than MAX_TASK_HISTORY tasks are active at once, all active
 * tasks win and the temporary overflow disappears as they finish. */
export function trimTaskHistory(tasks: Task[]): Task[] {
  const activeCount = tasks.reduce(
    (count, task) => count + (isTerminal(task.status) ? 0 : 1),
    0,
  );
  let terminalSlots = Math.max(0, MAX_TASK_HISTORY - activeCount);
  return tasks.filter((task) => {
    if (!isTerminal(task.status)) return true;
    if (terminalSlots <= 0) return false;
    terminalSlots -= 1;
    return true;
  });
}

interface PersistedShape {
  tasks: Task[];
}

function loadInitial(): Task[] {
  if (typeof window === "undefined") return [];
  try {
    const raw = safeGetItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as PersistedShape;
    if (!parsed || !Array.isArray(parsed.tasks)) return [];
    // Any task that was non-terminal when the app closed is now stale.
    // Mark as "interrupted" so the user sees what happened + can retry.
    return parsed.tasks.map((t) =>
      isTerminal(t.status) ? t : { ...t, status: "interrupted" as TaskStatus, endedAtMs: t.updatedAtMs },
    );
  } catch {
    return [];
  }
}

function persist(tasks: Task[]): void {
  const trimmed = trimTaskHistory(tasks);
  const shape: PersistedShape = { tasks: trimmed };
  safeSetItem(STORAGE_KEY, JSON.stringify(shape));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

export function isTerminal(status: TaskStatus): boolean {
  return (
    status === "done" ||
    status === "failed" ||
    status === "cancelled" ||
    status === "interrupted"
  );
}

export function isActivatable(status: TaskStatus): boolean {
  // "Activatable" = counts toward the active-task badge / spinner.
  return status === "running" || status === "queued" || status === "paused" || status === "awaiting";
}

/** Generate a task id. Uses crypto.randomUUID when available, else a
 *  timestamp+random fallback. Format matches the spec's "ULID-like" intent
 *  — stable across client+engine+history. */
function newTaskId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `t_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 10)}`;
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

interface TaskState {
  tasks: Task[];

  /** Register a new task. Returns its id so the caller can pair
   *  `updateTask()` / `finishTask()` calls. The task starts in the
   *  given `status` (usually "running" or "queued"). */
  registerTask: (init: {
    kind: TaskKind;
    origin: string;
    label: string;
    detail?: string;
    consoleId: string;
    payload?: TaskPayload;
    engineJobId?: string;
    status?: TaskStatus;
    progress?: TaskProgress;
    maxAttempts?: number;
  }) => string;

  /** Patch a task — typically progress/rate/eta as the engine reports.
   *  No-op if the id isn't found or is already terminal. */
  updateTask: (
    id: string,
    patch: Partial<
      Pick<Task, "label" | "progress" | "rate" | "eta" | "status" | "detail" | "engineJobId" | "lastError">
    >,
  ) => void;

  /** Move a task to a terminal state. Idempotent — finishing an
   *  already-finished task is a no-op (the first terminal state wins). */
  finishTask: (
    id: string,
    status: Extract<TaskStatus, "done" | "failed" | "cancelled">,
    extras?: Partial<Pick<Task, "lastError" | "progress" | "detail">>,
  ) => void;

  /** Remove a task from the store. Only allowed for terminal tasks —
   *  cancelling a running task should go through `finishTask(cancelled)`. */
  removeTask: (id: string) => void;

  /** Clear all terminal tasks from the history. Active tasks are
   *  preserved so a clear can't make an in-flight op disappear. */
  clearFinished: () => void;

  /** Look up a task by id. Returns undefined if not found (including
   *  terminal history evicted by MAX_TASK_HISTORY). */
  getTask: (id: string) => Task | undefined;

  /** All currently-active (non-terminal) tasks. The badge / spinner
   *  reads this. Computed in-place so callers don't need a selector. */
  activeTasks: () => Task[];

  /** All currently-running tasks (status === "running"). Stricter
   *  than activeTasks — excludes queued/paused/awaiting. */
  runningTasks: () => Task[];

  /** Tasks for a specific console (bare host). Used by the per-console
   *  queue view in the Tasks tab. */
  tasksForConsole: (consoleId: string) => Task[];
}

export const useTaskStore = create<TaskState>((set, get) => ({
  tasks: loadInitial(),

  registerTask: (init) => {
    const id = newTaskId();
    const now = Date.now();
    const task: Task = {
      id,
      kind: init.kind,
      origin: init.origin,
      label: init.label,
      detail: init.detail,
      consoleId: hostOf(init.consoleId),
      payload: init.payload ?? {},
      engineJobId: init.engineJobId,
      status: init.status ?? "running",
      progress: init.progress,
      createdAt: new Date(now).toISOString(),
      updatedAtMs: now,
      attempts: 1,
      maxAttempts: init.maxAttempts ?? 3,
    };
    set((s) => {
      // New tasks go to the front so the Tasks tab shows newest first.
      const tasks = trimTaskHistory([task, ...s.tasks]);
      persist(tasks);
      return { tasks };
    });
    return id;
  },

  updateTask: (id, patch) => {
    set((s) => {
      const idx = s.tasks.findIndex((t) => t.id === id);
      if (idx < 0) return s;
      const existing = s.tasks[idx];
      if (isTerminal(existing.status)) return s;
      const updated: Task = {
        ...existing,
        ...patch,
        updatedAtMs: Date.now(),
      };
      const tasks = [...s.tasks];
      tasks[idx] = updated;
      const trimmed = trimTaskHistory(tasks);
      persist(trimmed);
      return { tasks: trimmed };
    });
  },

  finishTask: (id, status, extras) => {
    set((s) => {
      const idx = s.tasks.findIndex((t) => t.id === id);
      if (idx < 0) return s;
      const existing = s.tasks[idx];
      if (isTerminal(existing.status)) return s;
      const now = Date.now();
      const updated: Task = {
        ...existing,
        ...extras,
        status,
        endedAtMs: now,
        updatedAtMs: now,
      };
      const tasks = [...s.tasks];
      tasks[idx] = updated;
      const trimmed = trimTaskHistory(tasks);
      persist(trimmed);
      return { tasks: trimmed };
    });
  },

  removeTask: (id) => {
    set((s) => {
      const existing = s.tasks.find((t) => t.id === id);
      if (!existing || !isTerminal(existing.status)) return s;
      const tasks = s.tasks.filter((t) => t.id !== id);
      persist(tasks);
      return { tasks };
    });
  },

  clearFinished: () => {
    set((s) => {
      const tasks = s.tasks.filter((t) => !isTerminal(t.status));
      persist(tasks);
      return { tasks };
    });
  },

  getTask: (id) => get().tasks.find((t) => t.id === id),

  activeTasks: () => get().tasks.filter((t) => isActivatable(t.status)),

  runningTasks: () => get().tasks.filter((t) => t.status === "running"),

  tasksForConsole: (consoleId) => {
    const host = hostOf(consoleId);
    return get().tasks.filter((t) => t.consoleId === host);
  },
}));

// ---------------------------------------------------------------------------
// Convenience: subscribe helper for components that need to react to a
// specific task's status change (e.g. a modal that closes when done).
// ---------------------------------------------------------------------------

/** Subscribe to status changes for a specific task. Returns an unsubscribe
 *  function. The listener is called with the new status (or undefined if
 *  the task was removed/evicted). */
export function onTaskStatusChange(
  taskId: string,
  listener: (status: TaskStatus | undefined) => void,
): () => void {
  let lastStatus: TaskStatus | undefined = useTaskStore.getState().getTask(taskId)?.status;
  return useTaskStore.subscribe((s) => {
    const t = s.tasks.find((x) => x.id === taskId);
    const next = t?.status;
    if (next !== lastStatus) {
      lastStatus = next;
      listener(next);
    }
  });
}
