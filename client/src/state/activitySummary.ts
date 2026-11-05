// What the activity bar shows, derived from the task store: the one-line summary, the panel's
// running rows, and this session's finished jobs. Pure, so the bar re-renders only its own rows.

import { isTerminal, type Task, type TaskKind } from "./tasks";

export interface ActivityRow {
  id: string;
  task: Task;
  /** "Upload", "Convert", "Install", … */
  short: string;
  /** 0..100, or null when the job reports no progress. */
  pct: number | null;
  /** Whole minutes without an update (1 or more), or null while it keeps reporting. */
  staleMin: number | null;
}

export type FinishedOutcome = "done" | "failed" | "cancelled" | "unverified" | "interrupted";

export interface FinishedRow {
  id: string;
  task: Task;
  outcome: FinishedOutcome;
  agoMs: number;
}

/** Work waiting in a queue that has not started yet (the console queue, the Convert queue). It
 *  is not a task until it starts, so it comes from the queue stores (see queuedWork). */
export interface QueuedRow {
  id: string;
  owner: "upload-queue" | "convert-queue";
  kind: TaskKind;
  label: string;
  consoleId: string;
  /** The screen that owns the queue. */
  route: string;
  /** Known size in bytes, or null. */
  sizeBytes: number | null;
}

export interface ActivitySummary {
  /** Running first (oldest first), then queued / waiting / paused. */
  running: ActivityRow[];
  /** The job the strip shows: one that is moving and says how far, the oldest such first. */
  primary: ActivityRow | null;
  /** Everything else live: the other running jobs and the queued work. */
  others: number;
  /** Queued work that has not started, in queue order. */
  queued: QueuedRow[];
  /** This session's finished jobs, newest first, at most eight. */
  finished: FinishedRow[];
  /** Failed jobs of this session the panel has not shown yet. */
  failedUnseen: number;
  /** A job that ended in the last five seconds, for the strip's brief note. */
  flash: { short: string; label: string; outcome: "done" | "failed" } | null;
}

const FINISHED = 8;
const STALE_MS = 60_000;
const FLASH_MS = 5_000;

export function shortLabel(kind: TaskKind): string {
  switch (kind) {
    case "upload-file":
    case "upload-dir":
    case "upload-archive":
      return "Upload";
    case "download":
      return "Download";
    case "fs-copy":
      return "Copy";
    case "fs-move":
      return "Move";
    case "fs-delete":
      return "Delete";
    case "fs-rename":
      return "Rename";
    case "fs-unpack":
      return "Decompress";
    case "fs-extract":
      return "Extract";
    case "app-dump":
      return "Dump";
    case "pkg-install":
    case "pkg-dpi-install":
      return "Install";
    case "install-batch":
      return "Install all";
    case "backup-snapshot":
      return "Backup";
    case "backup-restore":
      return "Restore";
    case "save-backup":
      return "Save backup";
    case "save-restore":
      return "Save restore";
    case "fpkg-convert":
      return "Convert";
    case "ffpfsc-compress":
      return "Compress";
    case "backport-patch":
      return "Backport";
    case "fakelib-scan":
      return "Library scan";
    case "fakelib-import":
      return "Library import";
    case "bug-report":
      return "Bug report";
    case "cheat-download":
      return "Cheats";
    case "icon-fetch":
      return "Artwork";
    case "library-move":
      return "Move";
    case "library-mount":
      return "Mount";
    case "library-register":
      return "Register";
    case "library-unregister":
      return "Unregister";
    case "library-launch":
      return "Launch";
    case "helper-send":
      return "Send helper";
    case "payload-playlist":
      return "Payloads";
    case "app-update":
      return "App update";
    case "console-task":
      return "On the PS5";
    default:
      return "Library";
  }
}

/** The translation key of `shortLabel(kind)`; `shortLabel` is its English text. */
export function shortLabelKey(kind: TaskKind): string {
  return `activity_kind_${kind.replace(/-/g, "_")}`;
}

/** The screen a job belongs to, opened when its row is clicked. Real routes only: a redirect
 *  would land on the right screen but leave a second history entry behind. */
export function routeForTask(task: Task): string {
  const k = task.kind;
  if (k.startsWith("upload-")) return "/send";
  if (k.startsWith("fs-") || k === "download") return "/files";
  if (k.startsWith("pkg-") || k === "install-batch") return "/send";
  if (k.startsWith("backup-")) return "/files?view=snapshots";
  if (k.startsWith("save-")) return "/saves";
  if (k === "fpkg-convert" || k === "ffpfsc-compress") return "/workshop";
  if (k === "backport-patch" || k.startsWith("fakelib-")) return "/games";
  if (k === "bug-report") return "/bug-report";
  if (k.startsWith("library-") || k === "app-dump") return "/games";
  if (k === "cheat-download") return "/cheats";
  if (k === "helper-send") return "/connection";
  if (k === "payload-playlist") return "/payloads";
  if (k === "app-update") return "/settings";
  return "/tasks";
}

function pctOf(task: Task): number | null {
  const p = task.progress;
  return p && p.total > 0 ? Math.min(100, (p.current / p.total) * 100) : null;
}

/** The job the strip names. Stable while the work goes on, so the line does not jump between
 *  jobs: a running job that still reports, then one that says how far it is, then the oldest. */
export function pickPrimary(running: readonly ActivityRow[]): ActivityRow | null {
  let best: ActivityRow | null = null;
  let bestScore = -1;
  for (const r of running) {
    const score =
      (r.task.status === "running" ? 4 : 0) + (r.staleMin == null ? 2 : 0) + (r.pct != null ? 1 : 0);
    // `running` is already oldest first, so the first of the best score wins.
    if (score > bestScore) {
      best = r;
      bestScore = score;
    }
  }
  return best;
}

export function summarize(
  tasks: Task[],
  opts: {
    now: number;
    sessionStart: number;
    seen: ReadonlySet<string>;
    /** Jobs this start-up interrupted; they count as ending when the session began. */
    interruptedAtLoad?: ReadonlySet<string>;
    /** Queued work from the queue stores (queuedWork). */
    queued?: readonly QueuedRow[];
  },
): ActivitySummary {
  const { now, sessionStart, seen } = opts;
  const endOf = (t: Task) =>
    opts.interruptedAtLoad?.has(t.id)
      ? Math.max(t.endedAtMs ?? 0, sessionStart)
      : (t.endedAtMs ?? 0);
  const started = (t: Task) => Date.parse(t.createdAt) || t.updatedAtMs;
  const running = tasks
    .filter((t) => !isTerminal(t.status))
    .sort((a, b) => {
      const ra = a.status === "running" ? 0 : 1;
      const rb = b.status === "running" ? 0 : 1;
      return ra - rb || started(a) - started(b);
    })
    .map<ActivityRow>((t) => {
      const quiet = now - t.updatedAtMs;
      return {
        id: t.id,
        task: t,
        short: shortLabel(t.kind),
        pct: pctOf(t),
        // Only a job that reports progress can fall silent; a one-shot job (a backup, a bug
        // report) says nothing until it ends, and a quiet minute is normal for it.
        staleMin:
          t.status === "running" && t.progress && quiet >= STALE_MS
            ? Math.floor(quiet / STALE_MS)
            : null,
      };
    });
  const ended = tasks
    .filter((t) => isTerminal(t.status) && endOf(t) >= sessionStart)
    .sort((a, b) => endOf(b) - endOf(a));
  const newest = ended[0];
  const flash =
    newest &&
    now - endOf(newest) < FLASH_MS &&
    (newest.status === "done" || newest.status === "failed")
      ? { short: shortLabel(newest.kind), label: newest.label, outcome: newest.status }
      : null;
  const queued = [...(opts.queued ?? [])];
  const primary = pickPrimary(running);
  return {
    running,
    primary,
    queued,
    others: running.length + queued.length - (primary ? 1 : 0),
    finished: ended.slice(0, FINISHED).map((t) => ({
      id: t.id,
      task: t,
      outcome: t.status as FinishedOutcome,
      agoMs: now - endOf(t),
    })),
    failedUnseen: ended.filter((t) => t.status === "failed" && !seen.has(t.id)).length,
    flash,
  };
}

/**
 * Whether the activity clock must tick: while the panel is open (its "Just finished" rows say
 * how long ago each ended, which must not freeze at the last tick), while a job runs (it can go
 * stale), and for a few seconds after one ends (the strip's brief note). `clock` is the clock's
 * last reading, `latest` the newest task stamp.
 */
export function needsClock(
  tasks: readonly Task[],
  clock: number,
  latest: number,
  panelOpen: boolean,
): boolean {
  if (panelOpen) return true;
  const now = Math.max(clock, latest);
  return tasks.some(
    (t) => !isTerminal(t.status) || (t.endedAtMs != null && now - t.endedAtMs < FLASH_MS + 1000),
  );
}
