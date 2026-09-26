/**
 * Collapse repeated identical command failures into one line plus a count.
 *
 * A console that is switched off, unreachable, or mid-reboot fails the same
 * poll every few seconds indefinitely. Logged plainly that buries everything
 * else: one real bug report contained 63 identical
 * `process_list_get failed: connect to …` warnings out of 100 total, so the
 * one line that mattered — a 10-minute outage and its recovery — had to be
 * dug out of its own echo.
 *
 * The engine already does this for HTTP responses (`log_dedup` in lib.rs).
 * This is the same contract for Tauri commands: first failure speaks, repeats
 * stay quiet, a periodic reminder proves it is still broken, and recovery is
 * announced with how many were swallowed.
 */

export type DedupAction =
  | { kind: "warn"; suppressed: number }
  | { kind: "recovered"; suppressed: number }
  | { kind: "quiet" };

/** How long a run of identical failures stays quiet before one reminder. */
export const REMINDER_MS = 60_000;

interface FailState {
  message: string;
  /** Failures not yet reported. */
  suppressed: number;
  lastLoggedMs: number;
}

const failing = new Map<string, FailState>();

/** Test seam: drop all remembered failure state. */
export function resetInvokeLogDedup(): void {
  failing.clear();
}

/**
 * Decide what to log for one command outcome.
 *
 * `message` is undefined for a success. A CHANGED message counts as new
 * information and is always reported — a console that starts refusing for a
 * different reason is not the same failure.
 */
export function observeInvokeOutcome(
  cmd: string,
  message: string | undefined,
  nowMs: number,
): DedupAction {
  const prior = failing.get(cmd);

  if (message === undefined) {
    if (!prior) return { kind: "quiet" };
    failing.delete(cmd);
    // Only worth announcing if something was actually hidden.
    return prior.suppressed > 0
      ? { kind: "recovered", suppressed: prior.suppressed }
      : { kind: "quiet" };
  }

  if (!prior || prior.message !== message) {
    failing.set(cmd, { message, suppressed: 0, lastLoggedMs: nowMs });
    return { kind: "warn", suppressed: 0 };
  }

  prior.suppressed += 1;
  if (nowMs - prior.lastLoggedMs >= REMINDER_MS) {
    const suppressed = prior.suppressed;
    prior.suppressed = 0;
    prior.lastLoggedMs = nowMs;
    return { kind: "warn", suppressed };
  }
  return { kind: "quiet" };
}
