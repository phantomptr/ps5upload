// What is happening to a copy on this computer right now: being built into a game image, waiting
// in the queue, being sent or installed, or done. One answer for the Collection's game details
// and cards, read from the queue and from Convert's pipeline. Asked about one console: a game
// on its way to one PS5 is still free to send to another.

import { useShallow } from "zustand/react/shallow";

import { hostOf } from "../lib/addr";
import { useFpkgConversion } from "./fpkgConversion";
import { useUploadQueueStore, type QueueItem } from "./uploadQueue";

export type CopyActivity =
  | { phase: "building"; pct: number | null }
  | { phase: "queued" }
  | { phase: "sending"; pct: number | null; installing: boolean }
  | { phase: "done"; installing: boolean }
  | { phase: "failed"; message: string };

/** The path a queue item was made from: its source, or the package an install points at. */
function itemSource(it: QueueItem): string | null {
  if (it.sourceKind !== "install") return it.sourcePath;
  const req = it.install as { source?: string; path?: string } | undefined;
  return req?.source ?? req?.path ?? null;
}

function fromItem(it: QueueItem): CopyActivity {
  const installing = it.sourceKind === "install";
  if (it.status === "pending") return { phase: "queued" };
  if (it.status === "done") return { phase: "done", installing };
  if (it.status === "failed") return { phase: "failed", message: it.error ?? "" };
  const total = it.totalBytes || it.estimatedBytes || 0;
  return {
    phase: "sending",
    installing,
    pct: total > 0 ? Math.min(100, Math.round((it.bytesSent / total) * 100)) : null,
  };
}

type Pipeline = ReturnType<typeof useFpkgConversion.getState>["pipeline"];

/** Whether `addr` is the console `host` (any console when `host` is empty). */
function onConsole(addr: string | null | undefined, host: string): boolean {
  return !host || !addr || hostOf(addr) === hostOf(host);
}

/** Pure, for tests: the activity for `path` on console `host`, newest first. */
export function activityFor(
  path: string,
  items: QueueItem[],
  pipeline: Pipeline,
  host: string,
): CopyActivity | null {
  if (pipeline.phase !== "idle" && !onConsole(pipeline.host, host)) pipeline = { phase: "idle" };
  if (pipeline.phase === "running" && pipeline.source === path) {
    return {
      phase: "building",
      pct:
        pipeline.stageTotal > 0
          ? Math.min(100, Math.round((pipeline.stageDone / pipeline.stageTotal) * 100))
          : null,
    };
  }
  // A folder sent as an image: once built, the queue holds the image, not the folder.
  const viaImage =
    pipeline.phase === "done" && pipeline.source === path && pipeline.uploadQueued
      ? pipeline.packagePath
      : null;
  for (let i = items.length - 1; i >= 0; i--) {
    if (!onConsole(items[i].addr, host)) continue;
    const src = itemSource(items[i]);
    if (src === path || (viaImage && src === viaImage)) return fromItem(items[i]);
  }
  if (pipeline.phase === "failed" && pipeline.source === path) {
    return { phase: "failed", message: pipeline.message };
  }
  return null;
}

/** Live activity on console `host` for each of `paths` (same order). */
export function useCopyActivity(paths: string[], host: string): (CopyActivity | null)[] {
  const items = useUploadQueueStore(useShallow((s) => s.items));
  const pipeline = useFpkgConversion((s) => s.pipeline);
  return paths.map((p) => activityFor(p, items, pipeline, host));
}

/** Activity on console `host` for every copy something is happening to right now (waiting,
 *  building, sending, installing), by path. One subscription for a whole grid instead of one
 *  per card. */
export function useActiveCopies(host: string): Map<string, CopyActivity> {
  const items = useUploadQueueStore(useShallow((s) => s.items));
  const pipeline = useFpkgConversion((s) => s.pipeline);
  const map = new Map<string, CopyActivity>();
  if (pipeline.phase === "running" && onConsole(pipeline.host, host))
    map.set(pipeline.source, activityFor(pipeline.source, [], pipeline, host)!);
  for (const it of items) {
    if (it.status !== "pending" && it.status !== "running") continue;
    if (!onConsole(it.addr, host)) continue;
    const src = itemSource(it);
    if (src) map.set(src, fromItem(it));
  }
  return map;
}
