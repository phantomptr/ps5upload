// Poll an async probe on a fixed cadence until it reports success, or
// until the attempt budget is exhausted. Extracted from App.tsx's
// post-send "wait for payload to boot" path so it can be unit-tested
// and reused — the logic used to be inlined inside handlePayloadSend
// and was error-prone because it mixed React state updates with the
// polling loop and setTimeout closures.
//
// Contract: the probe returns "ok" on success, "fail" on a transient
// miss. The poller:
//   1. Waits `initialDelayMs` before the first probe — gives the
//      consumer (the PS5 payload) time to boot before we start asking.
//   2. If ok, calls `onResolved("ok")` and stops.
//   3. If fail and attempts remain, waits `intervalMs`, probes again.
//   4. If all attempts fail, calls `onResolved("fail")` and stops.
//   5. `cancel()` at any time stops the poll; `onResolved` is not
//      called after cancel.
//
// Every callback is synchronous from the poller's perspective — the
// probe returning a Promise is the only async boundary. The timer
// functions are injectable so tests can drive the clock.

export type ProbeResult = "ok" | "fail";

export type PollUntilReadyOptions = {
  /** Async probe; resolves to "ok" or "fail". Exceptions are treated as "fail". */
  probe: () => Promise<ProbeResult>;
  /** Delay before the first probe. */
  initialDelayMs: number;
  /** Delay between failed-probe retries. */
  intervalMs: number;
  /** Total probe attempts, including the first. Must be ≥ 1. */
  maxAttempts: number;
  /** Fires once per probe result (so consumers can log progress). */
  onAttempt?: (attemptIndex: number, result: ProbeResult) => void;
  /** Fires exactly once at the end — either "ok" on first success or
   *  "fail" when the budget is exhausted. Never fires after cancel(). */
  onResolved?: (result: ProbeResult) => void;
  /** Override for tests; default is the globals. */
  setTimeoutFn?: (fn: () => void, ms: number) => unknown;
  clearTimeoutFn?: (handle: unknown) => void;
};

export type PollHandle = {
  /** Aborts the poll. Pending timers are cleared; onResolved is
   *  suppressed. Safe to call after resolution (no-op). */
  cancel: () => void;
};

export function pollUntilReady(opts: PollUntilReadyOptions): PollHandle {
  if (opts.maxAttempts < 1) {
    throw new Error("pollUntilReady: maxAttempts must be >= 1");
  }

  const setT = opts.setTimeoutFn ?? ((fn, ms) => setTimeout(fn, ms));
  const clearT = opts.clearTimeoutFn ?? ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));

  let cancelled = false;
  let timerHandle: unknown = null;
  let attemptIndex = 0;

  const resolve = (result: ProbeResult) => {
    if (cancelled) return;
    cancelled = true;
    opts.onResolved?.(result);
  };

  const runAttempt = async () => {
    if (cancelled) return;
    attemptIndex += 1;
    let result: ProbeResult;
    try {
      result = await opts.probe();
    } catch {
      result = "fail";
    }
    if (cancelled) return;
    opts.onAttempt?.(attemptIndex, result);
    if (result === "ok") {
      resolve("ok");
      return;
    }
    if (attemptIndex >= opts.maxAttempts) {
      resolve("fail");
      return;
    }
    timerHandle = setT(runAttempt, opts.intervalMs);
  };

  timerHandle = setT(runAttempt, opts.initialDelayMs);

  return {
    cancel: () => {
      if (cancelled) return;
      cancelled = true;
      if (timerHandle !== null) {
        clearT(timerHandle);
        timerHandle = null;
      }
    },
  };
}
