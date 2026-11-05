// A Clean-up "Remove" per console. It runs on the helper for as long as it takes (an update's
// removal can wait 5 minutes per item), so following it does not depend on the screen staying
// open: leave and come back, and it is still here with what became of each item.

import { create } from "zustand";

import {
  CleanupError,
  cleanupApplyStatus,
  cleanupScanStatus,
  type CleanupApplyStatus,
  type CleanupItem,
} from "../api/cleanup";
import { hostOf } from "../lib/addr";
import { summarizeApply, type ApplySummary } from "../lib/cleanupModel";

const POLL_MS = 1000;
/** Failed status reads in a row before giving up: a helper can be slow for a while. */
const MAX_FAILED_READS = 30;

export interface CleanupRun {
  applying: CleanupApplyStatus | null;
  summary: ApplySummary | null;
  /** Free space on the drive before the run, for the result card. */
  freeBefore: number | null;
  /** What stopped the following, when nothing more can be said about the run. */
  error: unknown;
}

const IDLE: CleanupRun = { applying: null, summary: null, freeBefore: null, error: null };

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

/** Follows an apply to its end, whatever the screen does. Never rejects. */
export async function followApply(
  host: string,
  applyId: number,
  items: CleanupItem[],
  freeBefore: number | null,
  wait: (ms: number) => Promise<unknown> = sleep,
): Promise<void> {
  patch(host, { summary: null, error: null, freeBefore });
  let failures = 0;
  let last: CleanupApplyStatus | null = null;
  for (;;) {
    try {
      const st = await cleanupApplyStatus(host, applyId);
      failures = 0;
      last = st;
      if (st.state !== "running") {
        patch(host, { applying: null, summary: summarizeApply(st, items) });
        return;
      }
      patch(host, { applying: st });
    } catch (e) {
      // The helper restarted and forgot it, or stayed away too long: nothing more can be known.
      const gone = e instanceof CleanupError && e.reason === "unknown_apply";
      failures += 1;
      if (gone || failures >= MAX_FAILED_READS) {
        patch(host, {
          applying: null,
          summary: last ? summarizeApply(last, items) : null,
          error: e,
        });
        return;
      }
    }
    await wait(POLL_MS);
  }
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
