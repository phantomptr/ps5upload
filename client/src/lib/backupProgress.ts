// What the Backup progress view shows (#895), worked out from the engine's run record
// (`GET /api/save-backup/runs`): no React here, so it is tested on its own.

import type {
  BackupDestStatus,
  BackupPlace,
  BackupRun,
  BackupRunItem,
  BackupRunsView,
  BackupStep,
  BackupWhich,
} from "../api/saveBackup";

/** The places of each kind of backup, in the order the view shows them. */
export const PLACES: Record<BackupWhich, BackupPlace[]> = {
  share: ["share", "drive"],
  local: ["local", "drive"],
};

/** How one save ended up, over every place it goes to. */
export type RowOutcome = "verified" | "failed" | "skipped" | "left";

export function itemOutcome(it: BackupRunItem): RowOutcome {
  const dests = Object.values(it.dests).filter(
    (d): d is BackupDestStatus => !!d,
  );
  if (it.step === "failed" || dests.some((d) => d.step === "failed"))
    return "failed";
  if (dests.length === 0) return it.step === "verified" ? "verified" : "left";
  if (dests.every((d) => d.step === "verified" || d.step === "skipped")) {
    return dests.every((d) => d.step === "skipped") ? "skipped" : "verified";
  }
  return "left";
}

/** One save's row: its state now (the busiest place's while it is being backed up), how far a
 *  copy is, and why it failed. */
export interface Row {
  id: string;
  title_id: string;
  user_id: number;
  kind: string;
  path: string;
  size: number;
  step: BackupStep;
  pct: number | null;
  error: string | null;
  outcome: RowOutcome;
  places: Array<{
    place: BackupPlace;
    step: BackupStep;
    pct: number | null;
    error: string | null;
  }>;
}

const BUSY: BackupStep[] = ["reading", "copying", "checking"];

export function toRow(it: BackupRunItem, places: BackupPlace[]): Row {
  const outcome = itemOutcome(it);
  const ps = places
    .filter((p) => it.dests[p])
    .map((p) => {
      const d = it.dests[p] as BackupDestStatus;
      return { place: p, step: d.step, pct: d.pct ?? null, error: d.error };
    });
  const busy = ps.find((p) => p.step === "copying" || p.step === "checking");
  let step: BackupStep;
  if (it.step === "reading") step = "reading";
  else if (busy) step = busy.step;
  else if (outcome === "left") step = "waiting";
  else step = outcome;
  const failedPlace = ps.find((p) => p.step === "failed");
  return {
    id: it.id,
    title_id: it.title_id,
    user_id: it.user_id,
    kind: it.kind,
    path: it.path,
    size: it.size,
    step,
    pct: busy?.step === "copying" ? busy.pct : null,
    error: it.error ?? failedPlace?.error ?? null,
    outcome,
    places: ps,
  };
}

/** Overall progress of a run, with speed and time left from what this view has seen. */
export interface Overall {
  done: number;
  total: number;
  failed: number;
  skipped: number;
  left: number;
  bytesDone: number;
  bytesTotal: number;
  elapsedMs: number;
  /** Bytes a second, once there is enough to tell; null before. */
  speed: number | null;
  /** A rough time left; null while unknown. */
  leftMs: number | null;
  running: boolean;
}

/** Speed from what the view has seen of a run (not from its start: a continued run starts with
 *  saves already done). Feed each answer in; it keeps the first one of the run as its start. */
export class SpeedMeter {
  private runId: string | null = null;
  private startBytes = 0;
  private startAt = 0;

  sample(runId: string, bytesDone: number, at: number): number | null {
    if (runId !== this.runId) {
      this.runId = runId;
      this.startBytes = bytesDone;
      this.startAt = at;
      return null;
    }
    const secs = (at - this.startAt) / 1000;
    const bytes = bytesDone - this.startBytes;
    // Less than a few seconds or nothing moved yet: no guess.
    if (secs < 3 || bytes <= 0) return null;
    return bytes / secs;
  }
}

export function overall(
  view: BackupRunsView,
  speed: number | null,
): Overall | null {
  const run = view.run;
  const s = view.summary;
  if (!run || !s) return null;
  const running = run.state === "running";
  const end = running ? view.now : run.ended_at || run.updated_at;
  const elapsedMs = Math.max(0, end - run.started_at);
  const bytesLeft = Math.max(0, s.bytes_total - s.bytes_done);
  const leftMs =
    running && speed && speed > 0 && bytesLeft > 0
      ? Math.round((bytesLeft / speed) * 1000)
      : null;
  return {
    done: s.verified,
    total: s.total,
    failed: s.failed,
    skipped: s.skipped,
    left: s.left,
    bytesDone: s.bytes_done,
    bytesTotal: s.bytes_total,
    elapsedMs,
    speed: running ? speed : null,
    leftMs,
    running,
  };
}

/** Which rows to show. */
export type RowFilter = "all" | "failed" | "left" | "verified";

export function rows(
  run: BackupRun | null,
  which: BackupWhich,
  filter: RowFilter = "all",
): Row[] {
  if (!run) return [];
  const out = run.items.map((it) => toRow(it, PLACES[which]));
  // The ones being backed up first, then failed, waiting, done.
  const order: Record<string, number> = {
    busy: 0,
    failed: 1,
    left: 2,
    skipped: 3,
    verified: 4,
  };
  const rank = (r: Row) =>
    BUSY.includes(r.step) ? order.busy : order[r.outcome];
  return out
    .filter((r) => filter === "all" || r.outcome === filter)
    .sort((a, b) => rank(a) - rank(b));
}

/** The saves Retry can do again: failed ones of a run that has ended. */
export function retryable(run: BackupRun | null): string[] {
  if (!run || run.state === "running") return [];
  return run.items
    .filter((it) => itemOutcome(it) === "failed")
    .map((it) => it.id);
}

/** What the card says about a run that did not go through, with Continue: null when the last run
 *  ended whole (or runs). */
export interface StoppedNote {
  done: number;
  total: number;
  /** `console_gone`, `interrupted`, `cancelled`, or another the engine names. */
  code: string;
  reason: string | null;
  /** When it stopped (Unix ms). */
  at: number;
  /** Whether there is anything to continue. */
  canContinue: boolean;
}

export function stoppedNote(view: BackupRunsView | null): StoppedNote | null {
  const run = view?.run;
  const s = view?.summary;
  if (!run || !s || run.state === "running" || run.state === "done")
    return null;
  return {
    done: s.verified,
    total: s.total,
    code: run.reason_code ?? run.state,
    reason: run.reason,
    at: run.ended_at || run.updated_at,
    canContinue: s.left > 0,
  };
}

/** Whether the view should keep asking the engine: while a run runs, and the page is shown. */
export function shouldPoll(
  view: BackupRunsView | null,
  visible: boolean,
): boolean {
  return visible && view?.run?.state === "running";
}

/** Runs `fn` unless an earlier press under the same `guard` has not settled: Continue or Retry
 *  pressed again before the first press's run shows sends nothing more (the engine answered the
 *  second with "a backup is running", shown as an error). */
export async function pressOnce(
  guard: { current: boolean },
  fn: () => Promise<void>,
): Promise<void> {
  if (guard.current) return;
  guard.current = true;
  try {
    await fn();
  } finally {
    guard.current = false;
  }
}
