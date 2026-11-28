/**
 * Speed, time left and "stalled" for anything that counts bytes up (#981): one computation for
 * every surface, the app's task list and transfer readouts and the console's own page (Minion),
 * so the same copy reads the same speed wherever it is watched. No imports: the console page
 * bundles this file too.
 *
 * - Bytes are the source: the caller feeds (time, bytes done, bytes total) as it polls; nothing
 *   here polls or sets a timer.
 * - Speed is the bytes moved over the last {@link RATE_WINDOW_MS}, measured up to `now`: copies
 *   land in bursts (a helper writes a 32 MiB block, a NAS read stalls a moment), and a window of
 *   several seconds bridges them; measuring to `now` lets the figure fall when bytes stop.
 * - Time left only after {@link ETA_AFTER_MS} of data, and only when the total is known and not
 *   yet reached (at the total the job is finishing, and "0 s left" would be a lie).
 * - Stalled: nothing moved for {@link STALL_MS} while bytes are still to come; speed and time left
 *   are then hidden.
 * - Bytes going down (a new item, or a job restarted after a resume) start the window again; the
 *   elapsed time keeps counting from the first sample.
 */

/** The speed is averaged over this much time. */
export const RATE_WINDOW_MS = 8000;
/** A time left is shown once the window has held data this long. */
export const ETA_AFTER_MS = 3000;
/** No bytes for this long, with bytes still to come: stalled. */
export const STALL_MS = 15_000;
/** Under a kilobyte a second there is no speed worth showing (a step that only hashes or settles). */
export const MIN_SHOWN_RATE = 1024;

interface Sample {
  at: number;
  done: number;
}

export interface RateTracker {
  samples: Sample[];
  /** First sample ever (elapsed counts from here). */
  startAt: number;
  /** First sample since the window last started again. */
  windowAt: number;
  /** Last time the bytes went up (or the window started). */
  lastMoveAt: number;
  /** The bytes ever went up while watched. */
  moved: boolean;
}

export interface RateView {
  /** Bytes a second over the window; 0 when there is no speed to show (or stalled). */
  bytesPerSec: number;
  /** Seconds left, or null when it would be a guess. */
  etaSeconds: number | null;
  /** Nothing has moved for {@link STALL_MS} and bytes are still to come. */
  stalled: boolean;
  /** Seconds since the first sample. */
  elapsedSeconds: number;
}

export function newRateTracker(): RateTracker {
  return { samples: [], startAt: NaN, windowAt: NaN, lastMoveAt: NaN, moved: false };
}

/** One poll: record `(now, done)` and read the speed, time left and stalled state. `minRate`: under
 *  this the speed reads 0 (a count of files or items passes 0: any pace is worth showing). */
export function trackRate(t: RateTracker, now: number, done: number, total: number, minRate = MIN_SHOWN_RATE): RateView {
  const d = Number.isFinite(done) && done > 0 ? done : 0;
  const tot = Number.isFinite(total) && total > 0 ? total : 0;
  const last = t.samples[t.samples.length - 1];
  if (Number.isNaN(t.startAt)) t.startAt = now;
  if (!last || d < last.done) {
    // The first sample, or the count went back: the speed starts again from here.
    t.samples = [{ at: now, done: d }];
    t.windowAt = now;
    t.lastMoveAt = now;
  } else {
    if (d > last.done) {
      t.lastMoveAt = now;
      t.moved = true;
    }
    if (now > last.at) t.samples.push({ at: now, done: d });
    else last.done = d;
    // Keep one sample at or before the window's start, so the window spans all of it.
    while (t.samples.length > 2 && t.samples[1].at <= now - RATE_WINDOW_MS) t.samples.shift();
  }
  const elapsedSeconds = Math.max(0, (now - t.startAt) / 1000);
  const remaining = tot > 0 ? tot - d : 0;
  const pending = tot > 0 ? remaining > 0 : t.moved;
  const stalled = pending && now - t.lastMoveAt >= STALL_MS;
  const anchor = t.samples[0];
  const span = now - anchor.at;
  let bytesPerSec = span > 0 ? ((d - anchor.done) * 1000) / span : 0;
  if (stalled || !(bytesPerSec > 0) || bytesPerSec < minRate) bytesPerSec = 0;
  const etaSeconds =
    bytesPerSec > 0 && remaining > 0 && now - t.windowAt >= ETA_AFTER_MS ? remaining / bytesPerSec : null;
  return { bytesPerSec, etaSeconds, stalled, elapsedSeconds };
}

/** Trackers by key (a job id), for code that watches several at once. Forgets keys not seen in
 *  the last {@link prune} call so a long session does not keep every finished job. */
export class RateTrackers {
  private readonly map = new Map<string, RateTracker>();

  track(key: string, now: number, done: number, total: number): RateView {
    let t = this.map.get(key);
    if (!t) {
      t = newRateTracker();
      this.map.set(key, t);
    }
    return trackRate(t, now, done, total);
  }

  /** Drop every tracker whose key is not in `keep`. */
  prune(keep: Iterable<string>): void {
    const k = new Set(keep);
    for (const key of Array.from(this.map.keys())) if (!k.has(key)) this.map.delete(key);
  }

  delete(key: string): void {
    this.map.delete(key);
  }
}
