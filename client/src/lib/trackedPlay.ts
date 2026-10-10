import { useEffect, useState } from "react";

import { activityGet, type ActivityEntry } from "../api/ps5";
import { transferAddr } from "./addr";
import { useConnectionStore } from "../state/connection";
import { lastSeenPlayingFor, playSecondsFor } from "../state/playTime";
import { runningOn, useRunningAppsStore } from "../state/runningApps";

/** One title's play as the helper's process watcher recorded it on the console. */
export interface TrackedPlay {
  seconds: number;
  /** When the watcher last saw it running, in ms; undefined when it never has. */
  lastSeenMs: number | undefined;
}

/** title_id → play, from the helper's activity table (what Game Activity's Tracked tab shows). */
export function trackedPlayMap(titles: readonly ActivityEntry[]): Map<string, TrackedPlay> {
  const out = new Map<string, TrackedPlay>();
  for (const t of titles) {
    if (!t.title_id) continue;
    const seen = t.last_seen_ts || t.last_launch_ts;
    out.set(t.title_id, {
      seconds: t.total_seconds ?? 0,
      lastSeenMs: seen > 0 ? seen * 1000 : undefined,
    });
  }
  return out;
}

/** One title's play on `host`: the helper's numbers when it answered (a title
 *  it never saw is unplayed), the app's own observed count otherwise. */
export function playFor(
  tracked: Map<string, TrackedPlay> | null,
  local: {
    byHost: Record<string, Record<string, number>>;
    lastSeenByHost: Record<string, Record<string, number>>;
  },
  host: string | null | undefined,
  titleId: string | null | undefined,
): { seconds: number | undefined; lastSeenMs: number | undefined } {
  if (!titleId) return { seconds: undefined, lastSeenMs: undefined };
  if (tracked) {
    const t = tracked.get(titleId);
    return { seconds: t?.seconds, lastSeenMs: t?.lastSeenMs };
  }
  return {
    seconds: playSecondsFor(local, host, titleId),
    lastSeenMs: lastSeenPlayingFor(local, host, titleId),
  };
}

/**
 * The connected console's play time from the helper, which watches processes on
 * the console itself and so counts play while this app is closed. null while it
 * has not answered, or when it cannot (an older payload without the tracker):
 * callers then fall back to the app's own observed play time (state/playTime).
 *
 * Read again whenever the running set changes, so a game that was just closed
 * shows its new total.
 */
export function useTrackedPlay(host: string | null | undefined): Map<string, TrackedPlay> | null {
  const up = useConnectionStore((s) => s.payloadStatus === "up");
  const runningKey = useRunningAppsStore((s) => Array.from(runningOn(s, host)).sort().join(","));
  const [data, setData] = useState<{ host: string; map: Map<string, TrackedPlay> } | null>(null);
  const h = host?.trim() ?? "";

  useEffect(() => {
    if (!h || !up) return;
    let cancelled = false;
    activityGet(transferAddr(h))
      .then((r) => {
        if (!cancelled) setData({ host: h, map: trackedPlayMap(r.titles ?? []) });
      })
      .catch(() => {
        if (!cancelled) setData(null);
      });
    return () => {
      cancelled = true;
    };
  }, [h, up, runningKey]);

  // Never another console's numbers while a switch is in flight.
  return data && data.host === h ? data.map : null;
}
