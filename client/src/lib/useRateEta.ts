import { useEffect, useRef, useState } from "react";

import {
  computeRate,
  pushRateSample,
  remainingSeconds,
  type RateSample,
} from "./rollingRate";

/** Trailing window for the readouts below: a copy or a settle moves in bursts, so a longer
 *  window than the transfer's 4 samples keeps the time left from lurching. */
const WINDOW = 8;

export interface RateEta {
  /** Units per second over the trailing window (0 until there are two distinct samples). */
  rate: number;
  /** Seconds left, or null when it would be a guess (no rate yet, no total, or nothing left). */
  etaSeconds: number | null;
}

/** One step of the estimator, pure so it can be tested: pushes `(now, done)` and reads the
 *  rate. A fall in `done` (a new item, a restarted job) starts the window again. */
export function stepRateEta(
  samples: RateSample[],
  now: number,
  done: number,
  total: number,
): RateEta {
  const last = samples[samples.length - 1];
  if (last && done < last.bytes) samples.length = 0;
  pushRateSample(samples, now, done, WINDOW);
  const rate = computeRate(samples, now);
  return { rate, etaSeconds: remainingSeconds(done, total, rate) };
}

/** The rate and time left of something that counts up (bytes copied, files settled), read from
 *  the values the caller already polls. Samples are kept in a ref and read when `done` or
 *  `total` changes, so it costs no timer. `key` names the thing being measured: a new key
 *  starts a fresh window. */
export function useRateEta(key: string, done: number, total: number): RateEta {
  const samples = useRef<{ key: string; samples: RateSample[] }>({ key, samples: [] });
  const [eta, setEta] = useState<RateEta>({ rate: 0, etaSeconds: null });
  useEffect(() => {
    if (samples.current.key !== key) samples.current = { key, samples: [] };
    setEta(stepRateEta(samples.current.samples, Date.now(), done, total));
  }, [key, done, total]);
  return eta;
}
