/**
 * Progress of an upload that may have resumed. The engine's `bytes_sent` counts what the console
 * already held when the job opened (`resumed_bytes`: files in place plus the durable part of an
 * unfinished file) as done, so the bar starts where the last run stopped. Speed and time left
 * are about this run: the resumed part arrived at once and must not count as speed, and only
 * what is still to send counts toward the time left.
 */

/** This run's own bytes: `bytesSent` (which includes the resumed part) less `resumedBytes`. */
export function runBytes(bytesSent: number, resumedBytes: number | undefined): number {
  return Math.max(0, bytesSent - Math.max(0, resumedBytes ?? 0));
}

export interface ProgressFigures {
  /** 0–100, from everything on the console (the resumed part included). 0 without a total. */
  pct: number;
  /** What is still to send. */
  remainingBytes: number;
  /** Seconds left at this run's speed; null without a speed, a total or anything left. */
  etaSec: number | null;
}

/** Percent, bytes left and time left. `doneBytes` is everything on the console (the engine's
 *  `bytes_sent`, resumed part included); `bytesPerSec` is this run's measured speed. */
export function progressFigures(
  doneBytes: number,
  totalBytes: number,
  bytesPerSec: number,
): ProgressFigures {
  const done = Math.max(0, doneBytes);
  const pct = totalBytes > 0 ? Math.max(0, Math.min(100, (done / totalBytes) * 100)) : 0;
  const remainingBytes = Math.max(0, totalBytes - done);
  const etaSec = bytesPerSec > 0 && remainingBytes > 0 ? remainingBytes / bytesPerSec : null;
  return { pct, remainingBytes, etaSec };
}

/** The bytes a resumed upload found already on the console, worth a line of its own: null on a
 *  fresh upload. Never more than the job. */
export function resumedShown(
  resumedBytes: number | undefined,
  totalBytes: number,
): number | null {
  const r = Math.max(0, resumedBytes ?? 0);
  if (r <= 0) return null;
  return totalBytes > 0 ? Math.min(r, totalBytes) : r;
}
