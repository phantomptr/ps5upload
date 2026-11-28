import { useEffect, useRef, useState } from "react";

import { MIN_SHOWN_RATE, newRateTracker, trackRate, type RateTracker } from "./transferRate";

export interface RateEta {
  /** Units per second over the trailing window (0 until there is a speed to show, or stalled). */
  rate: number;
  /** Seconds left, or null when it would be a guess (no rate yet, no total, or nothing left). */
  etaSeconds: number | null;
  /** Nothing has moved for a while though some is still to come (#981). */
  stalled: boolean;
}

/** What is counted: bytes (a speed under a KiB/s is not shown) or files/items (any pace is). */
export type RateUnit = "bytes" | "count";

/** One step of the estimator, pure so it can be tested: records `(now, done)` and reads the
 *  rate (lib/transferRate, the computation every surface shares). A fall in `done` (a new item,
 *  a restarted job) starts the window again. */
export function stepRateEta(t: RateTracker, now: number, done: number, total: number, unit: RateUnit = "bytes"): RateEta {
  const v = trackRate(t, now, done, total, unit === "bytes" ? MIN_SHOWN_RATE : 0);
  return { rate: v.bytesPerSec, etaSeconds: v.etaSeconds, stalled: v.stalled };
}

/** The rate and time left of something that counts up (bytes copied, files settled), read from
 *  the values the caller already polls. The window is kept in a ref and read when `done` or
 *  `total` changes, and once a second while `live`, so a count that stops coming in shows its
 *  speed fall and then stalled. `key` names the thing being measured: a new key starts a fresh
 *  window. */
export function useRateEta(key: string, done: number, total: number, unit: RateUnit = "bytes", live = true): RateEta {
  const tracker = useRef<{ key: string; t: RateTracker }>({ key, t: newRateTracker() });
  const [tick, setTick] = useState(0);
  const [eta, setEta] = useState<RateEta>({ rate: 0, etaSeconds: null, stalled: false });
  useEffect(() => {
    if (!live) return;
    const h = setInterval(() => setTick((x) => x + 1), 1000);
    return () => clearInterval(h);
  }, [live]);
  useEffect(() => {
    if (tracker.current.key !== key) tracker.current = { key, t: newRateTracker() };
    setEta(stepRateEta(tracker.current.t, Date.now(), done, total, unit));
  }, [key, done, total, unit, tick]);
  return eta;
}
