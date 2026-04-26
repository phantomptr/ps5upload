/**
 * Trailing-window rate smoother shared by single-upload state
 * (`state/transfer.ts`) and queue state (`state/uploadQueue.ts`).
 *
 * Why a shared helper: both surfaces poll the engine for `bytes_sent`
 * every 500 ms and want a smoothed bytes/sec readout. Computing
 * "bytes since the last poll / 500 ms" makes the number jitter wildly
 * because the payload commits in shard bursts (32 MiB at a time on the
 * single-file path, 4 MiB packed shards on the small-file path) — one
 * tick lands a burst and reads "150 MiB/s", the next tick lands nothing
 * and reads "0 MiB/s". A trailing window of N samples gives the
 * displayed rate enough memory to bridge a single empty tick without
 * flickering to "—".
 *
 * The window only advances when `bytesSent` actually changes between
 * polls. Pushing flat samples would let the oldest sample's bytes
 * equal the newest's, collapsing the rate to 0 mid-transfer.
 */

export interface RateSample {
  ts: number;
  bytes: number;
}

/** Default window: 4 samples × 500 ms poll = 2 s trailing average. */
export const DEFAULT_RATE_WINDOW = 4;

/**
 * Push a fresh `(now, bytesSent)` sample onto `samples` *only if*
 * bytesSent advanced since the most recent sample. Mutates `samples`
 * in place, trimming from the front when it exceeds `windowSize`.
 *
 * Caller owns the array — typical use is a closure-scoped
 * `samples: RateSample[] = [{ ts: startMs, bytes: 0 }]` per upload run.
 */
export function pushRateSample(
  samples: RateSample[],
  now: number,
  bytesSent: number,
  windowSize = DEFAULT_RATE_WINDOW,
): void {
  if (samples.length === 0) {
    samples.push({ ts: now, bytes: bytesSent });
    return;
  }
  const last = samples[samples.length - 1];
  if (bytesSent === last.bytes) return; // skip flat sample
  samples.push({ ts: now, bytes: bytesSent });
  while (samples.length > windowSize) samples.shift();
}

/**
 * Compute bytes/sec as `(newest.bytes - oldest.bytes) / Δt`. Returns 0
 * when the window holds a single sample (no rate yet), when Δt rounds
 * to zero, or when the calculation would be negative (clamps).
 *
 * Safe to call on an empty array (returns 0).
 */
export function computeRate(samples: RateSample[], now: number): number {
  if (samples.length < 2) return 0;
  const oldest = samples[0];
  const newest = samples[samples.length - 1];
  // Use `now` rather than newest.ts so a long flat tail (no new
  // samples landing) still stretches Δt and lets the displayed rate
  // decay toward zero, instead of frozen at the last instant rate.
  const elapsedMs = Math.max(now - oldest.ts, 1);
  const bytes = newest.bytes - oldest.bytes;
  if (bytes <= 0) return 0;
  return (bytes * 1000) / elapsedMs;
}

/**
 * Final average for a completed upload: `bytes / elapsedMs * 1000`.
 * Use this for the done-state readout instead of the trailing window's
 * last value — at end-of-transfer the trailing window only sees the
 * tail, which is dominated by COMMIT-phase metadata work and reads
 * artificially low. The total-bytes-over-total-time average is what
 * users actually want to compare runs against.
 */
export function averageRate(bytes: number, elapsedMs: number): number {
  if (!isFinite(bytes) || !isFinite(elapsedMs) || elapsedMs <= 0 || bytes <= 0) {
    return 0;
  }
  return (bytes * 1000) / elapsedMs;
}
