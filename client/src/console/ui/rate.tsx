/**
 * Speed, elapsed time and time left on the console page (#981), from the bytes a job reports:
 * the app's own computation (lib/transferRate.ts), so a copy reads the same here as in the app.
 *
 * Trackers live at module level, by job: going from Home to Files and back keeps a job's speed
 * window. A line re-reads every second on its own, so a copy that stops reporting shows the speed
 * fall and then "Stalled" even when nothing new comes in.
 */
import { useEffect, useState } from "react";
import { formatBytes, formatDuration, formatEta } from "../../lib/format";
import { newRateTracker, trackRate, type RateTracker, type RateView } from "../../lib/transferRate";
import { getLanguage, t } from "../strings";

/** "12.3 MiB/s · 1m 5s elapsed · about 3m left", "Stalled · 4m elapsed"; "" when there is nothing yet. */
export function rateText(v: RateView | null | undefined, elapsedS?: number | null): string {
  const lang = getLanguage();
  const parts: string[] = [];
  if (v?.stalled) parts.push(t("cw_rate_stalled"));
  else if (v && v.bytesPerSec > 0) parts.push(`${formatBytes(v.bytesPerSec)}/s`);
  if (elapsedS != null && Number.isFinite(elapsedS) && elapsedS >= 1)
    parts.push(t("cw_rate_elapsed", { time: formatDuration(Math.floor(elapsedS), lang) }));
  if (v && !v.stalled && v.etaSeconds != null) parts.push(t("cw_rate_left", { time: formatEta(v.etaSeconds, lang) }));
  return parts.join(" · ");
}

interface Kept {
  t: RateTracker;
  seenAt: number;
}
const kept = new Map<string, Kept>();
/** A tracker no line has read for this long is forgotten. */
const FORGET_MS = 10 * 60_000;

/** Track `id` at `now` (module-level, so a remount keeps the window). */
export function trackJob(id: string, now: number, done: number, total: number): RateView {
  let k = kept.get(id);
  if (!k) {
    for (const [key, v] of kept) if (now - v.seenAt > FORGET_MS) kept.delete(key);
    k = { t: newRateTracker(), seenAt: now };
    kept.set(id, k);
  }
  k.seenAt = now;
  return trackRate(k.t, now, done, total);
}

/** Tests: forget every tracker. */
export function _resetRates(): void {
  kept.clear();
}

/** The rate of job `id` from its byte counts, re-read every second while `live`. */
export function useJobRate(id: string, done: number, total: number, live = true): RateView | null {
  const [tick, setTick] = useState(0);
  const [view, setView] = useState<RateView | null>(null);
  useEffect(() => {
    if (!live) return;
    const h = setInterval(() => setTick((x) => x + 1), 1000);
    return () => clearInterval(h);
  }, [live]);
  useEffect(() => {
    if (!live) return;
    setView(trackJob(id, Date.now(), done, total));
  }, [id, done, total, tick, live]);
  return live ? view : null;
}

/**
 * One line of speed · elapsed · time left for job `id`. `elapsedS`: what the helper says (it
 * knows when the job began); with `ownElapsed` the line counts from when it first saw the job
 * (the page started it). Nothing when there is nothing to say yet.
 */
export function RateLine({
  id,
  done,
  total,
  elapsedS,
  ownElapsed,
  live = true,
  prefix = "",
  className = "cw-dim cw-rate",
}: {
  id: string;
  done: number;
  total: number;
  elapsedS?: number | null;
  ownElapsed?: boolean;
  live?: boolean;
  /** Put before the text (" · "), only when there is text. */
  prefix?: string;
  className?: string;
}) {
  const v = useJobRate(id, done, total, live);
  const text = rateText(v, elapsedS ?? (ownElapsed ? v?.elapsedSeconds : null));
  return text ? (
    <span className={className} data-rate="">
      {prefix}
      {text}
    </span>
  ) : null;
}
