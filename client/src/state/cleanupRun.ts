// A Clean-up "Remove" per console. It runs on the helper for as long as it takes (an update's
// removal can wait 5 minutes per item), so following it does not depend on the screen staying
// open: leave and come back, and it is still here with what became of each item.

import { create } from "zustand";

import {
  CleanupError,
  cleanupApplyStatus,
  cleanupCancel,
  cleanupScanStatus,
  type CleanupApplyStatus,
  type CleanupItem,
} from "../api/cleanup";
import { hostOf } from "../lib/addr";
import { summarizeApply, type ApplySummary } from "../lib/cleanupModel";

const POLL_MS = 1000;
/** No status read answered for this long: the helper died or the console went away (#714). A
 *  helper that is slow for a while still answers well inside it. */
export const MAX_SILENT_MS = 90_000;
/** After Stop, how long a helper that has stopped answering is given before the following ends. */
export const STOP_GRACE_MS = 10_000;
/** After Stop, how long a helper that still answers "running" is followed before the following ends. */
export const STOP_ANSWERING_GRACE_MS = 60_000;
/** `CleanupError.reason` when the helper stopped answering. */
export const HELPER_SILENT = "helper_silent";
/** `CleanupError.reason` when Stop was asked and the helper never said the run ended. */
export const STOP_UNCONFIRMED = "stop_unconfirmed";

/** When Stop was asked, per console. */
const stopAsked = new Map<string, number>();

export interface CleanupRun {
  applying: CleanupApplyStatus | null;
  summary: ApplySummary | null;
  /** Free space on the drive before the run, for the result card. */
  freeBefore: number | null;
  /** What stopped the following, when nothing more can be said about the run. */
  error: unknown;
  /** Stop was asked and the run has not ended yet. */
  stopping?: boolean;
}

const IDLE: CleanupRun = { applying: null, summary: null, freeBefore: null, error: null, stopping: false };

interface CleanupRunState {
  byHost: Record<string, CleanupRun>;
}

export const useCleanupRuns = create<CleanupRunState>(() => ({ byHost: {} }));

function patch(host: string, change: Partial<CleanupRun>): void {
  const key = hostOf(host);
  useCleanupRuns.setState((s) => ({
    byHost: { ...s.byHost, [key]: { ...(s.byHost[key] ?? IDLE), ...change } },
  }));
}

export function cleanupRunOf(state: CleanupRunState, host: string): CleanupRun {
  return state.byHost[hostOf(host)] ?? IDLE;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Follows an apply to its end, whatever the screen does. Never rejects. It ends without the
 *  helper's word when the helper stops answering for {@link MAX_SILENT_MS}, or when Stop was asked
 *  and the helper has not confirmed within {@link STOP_GRACE_MS} (#714): a dead helper must not
 *  leave "Waiting for the PS5…" up for good with a Stop that does nothing. */
export async function followApply(
  host: string,
  applyId: number,
  items: CleanupItem[],
  freeBefore: number | null,
  wait: (ms: number) => Promise<unknown> = sleep,
  clock: () => number = Date.now,
): Promise<void> {
  const key = hostOf(host);
  stopAsked.delete(key);
  patch(host, { summary: null, error: null, freeBefore, stopping: false });
  let lastAnswer = clock();
  let last: CleanupApplyStatus | null = null;
  const end = (error: unknown, st: CleanupApplyStatus | null = last) => {
    stopAsked.delete(key);
    patch(host, {
      applying: null,
      stopping: false,
      summary: st ? summarizeApply(st, items) : null,
      error,
    });
  };
  for (;;) {
    try {
      const st = await cleanupApplyStatus(host, applyId);
      lastAnswer = clock();
      last = st;
      if (st.state !== "running") {
        end(null, st);
        return;
      }
      patch(host, { applying: st });
    } catch (e) {
      // The helper restarted and forgot it: nothing more can be known.
      if (e instanceof CleanupError && e.reason === "unknown_apply") {
        end(e);
        return;
      }
      if (clock() - lastAnswer >= MAX_SILENT_MS) {
        end(new CleanupError(e instanceof Error ? e.message : String(e), HELPER_SILENT, 0));
        return;
      }
    }
    // Stop with the helper still answering "running": it is finishing the item it was on, so the
    // run is followed a while longer, showing Stopping. An older helper waits out an update's
    // removal (minutes) without looking at Stop (#714): past the longer grace the following ends
    // anyway, saying the item may still finish. A helper that stopped answering ends it sooner.
    const asked = stopAsked.get(key);
    const silentSince = clock() - lastAnswer >= STOP_GRACE_MS;
    if (asked !== undefined && clock() - asked >= (silentSince ? STOP_GRACE_MS : STOP_ANSWERING_GRACE_MS)) {
      end(new CleanupError("stop not confirmed", STOP_UNCONFIRMED, 0));
      return;
    }
    await wait(POLL_MS);
  }
}

/** Stop: asks the helper to stop after the current item. The following goes on while the helper
 *  answers (it finishes that item), and ends within {@link STOP_GRACE_MS} once it doesn't. The
 *  helper may be gone, so the ask is not waited on. */
export function stopCleanupApply(host: string, clock: () => number = Date.now): void {
  const run = cleanupRunOf(useCleanupRuns.getState(), host);
  if (!run.applying) return;
  const key = hostOf(host);
  if (!stopAsked.has(key)) stopAsked.set(key, clock());
  patch(host, { stopping: true });
  void cleanupCancel(host, run.applying.apply_id).catch(() => {});
}

/**
 * An apply running on the console that this window is not following (it reloaded, or another
 * window started it): followed from here. True when there is one. An older helper does not answer
 * the question, which reads as none.
 */
export async function attachRunningApply(host: string): Promise<boolean> {
  if (cleanupRunOf(useCleanupRuns.getState(), host).applying) return true;
  const st = await cleanupApplyStatus(host, 0).catch(() => null);
  if (!st || st.state !== "running") return false;
  // Its scan names the items, for the result.
  const scan = await cleanupScanStatus(host, st.scan_id).catch(() => null);
  patch(host, { applying: st });
  void followApply(host, st.apply_id, scan?.items ?? [], scan?.free_bytes ?? null);
  return true;
}

/** Marks a run as starting, so the screen shows it from the first moment. */
export function startedApply(host: string, st: CleanupApplyStatus): void {
  patch(host, { applying: st, summary: null, error: null });
}

/** The user saw the result: it goes when they leave. A running one stays. */
export function dismissCleanupResult(host: string): void {
  if (cleanupRunOf(useCleanupRuns.getState(), host).applying) return;
  patch(host, IDLE);
}
