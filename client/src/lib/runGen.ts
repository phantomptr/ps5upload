// Generation counter for "stop the loop" cancellation.
//
// Pattern: long-running work captures a generation number at start;
// each await checks "am I still the live generation?" via `isLive`.
// `next()` bumps the counter (called from `stop()`), which makes
// every captured `thisRun` go stale.
//
// Before 2.12.0 this was hand-rolled in 4 stores: `transfer.ts`,
// `uploadQueue.ts`, `installQueue.ts`, `payloadPlaylists.ts`. The
// shapes drifted: some used closure `let runId = 0`, one used a
// store field, the `start()` semantics varied (functional set vs
// bare bump). All 4 implementations correctly worked but the next
// reviewer assumed "they must share something" and was wrong.
// This module is the shared something.
//
// Non-hook design: the 4 callers all live in zustand `create()`
// closures, not React components. A hook would force them through
// React subscriptions for state they don't render. Plain factory
// is the right shape.

export interface RunGen {
  /** Bump the counter and return the new live generation. Captured
   *  by the caller at the top of long work; subsequent isLive(thisRun)
   *  checks compare against it. */
  next(): number;
  /** True iff `thisRun` is the most-recently-issued generation,
   *  meaning the work that captured it is still the "live" one
   *  and may safely commit results. Idempotent; cheap. */
  isLive(thisRun: number): boolean;
  /** Current generation, exposed for diagnostics. Don't use this
   *  for liveness checks (race-prone — use `isLive(thisRun)`). */
  current(): number;
}

/** Create a fresh generation counter. Each caller (each zustand
 *  store) owns one; they don't share state across stores. */
export function createRunGen(): RunGen {
  let runId = 0;
  return {
    next() {
      runId += 1;
      return runId;
    },
    isLive(thisRun) {
      return runId === thisRun;
    },
    current() {
      return runId;
    },
  };
}
