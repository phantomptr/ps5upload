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

export interface ActivitySummary {
  /** Running first (oldest first), then queued / waiting / paused. */
  running: ActivityRow[];
  /** At most three jobs for the one-line summary. */
  headline: { short: string; pct: number | null }[];
  /** How many running jobs the headline leaves out. */
  more: number;
  /** This session's finished jobs, newest first, at most five. */
  finished: FinishedRow[];
  /** Failed jobs of this session the panel has not shown yet. */
  failedUnseen: number;
  /** A job that ended in the last five seconds, for the strip's brief note. */
  flash: { short: string; label: string; outcome: "done" | "failed" } | null;
}

const HEADLINE = 3;
const FINISHED = 5;
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
    default:
      return "Library";
  }
}

/** The screen a job belongs to, opened when its row is clicked. */
export function routeForTask(task: Task): string {
  const k = task.kind;
  if (k.startsWith("upload-")) return "/upload";
  if (k.startsWith("fs-") || k === "download") return "/file-system";
  if (k.startsWith("pkg-") || k === "install-batch") return "/install-package";
  if (k.startsWith("backup-")) return "/backup";
  if (k.startsWith("save-")) return "/saves";
  if (k === "fpkg-convert" || k === "ffpfsc-compress") return "/convert";
  if (k === "backport-patch" || k.startsWith("fakelib-")) return "/installed";
  if (k === "bug-report") return "/bug-report";
  if (k.startsWith("library-")) return "/library";
  if (k === "cheat-download") return "/cheats";
  return "/tasks";
}

function pctOf(task: Task): number | null {
  const p = task.progress;
  return p && p.total > 0 ? Math.min(100, (p.current / p.total) * 100) : null;
}

export function summarize(
  tasks: Task[],
  opts: {
    now: number;
    sessionStart: number;
    seen: ReadonlySet<string>;
    /** Jobs this start-up interrupted; they count as ending when the session began. */
    interruptedAtLoad?: ReadonlySet<string>;
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
  return {
    running,
    headline: running.slice(0, HEADLINE).map((r) => ({ short: r.short, pct: r.pct })),
    more: Math.max(0, running.length - HEADLINE),
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
