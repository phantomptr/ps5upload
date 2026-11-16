// Convert's "Put on the PS5 to mount" (Kraken Mount): which run it belongs to, and running it.

import type { MountPlaceResult } from "../../state/pkgMount";

export interface MountRun {
  busy: boolean;
  result: MountPlaceResult | null;
}

export const IDLE_MOUNT_RUN: MountRun = { busy: false, result: null };

/** Only a kept `.pkg` can be mounted: never an image (.ffpfsc, .exfat, ...) or a folder build,
 *  even one a failed run left behind. */
export function mountablePackage(path: string | null | undefined): path is string {
  return !!path && /\.pkg$/i.test(path);
}

/** What a mount result belongs to: the console and the run's package. A new run (while it runs,
 *  keyed by its start) or another console clears the result. */
export function mountRunKey(
  host: string,
  pipeline:
    | { phase: "idle" }
    | { phase: "running"; startedMs: number }
    | { phase: "done" | "failed"; source: string; packagePath: string | null },
): string {
  const run =
    pipeline.phase === "idle"
      ? ""
      : pipeline.phase === "running"
        ? `run:${pipeline.startedMs}`
        : `${pipeline.source}|${pipeline.packagePath ?? ""}`;
  return `${host}|${run}`;
}

/** Run `place`, reporting busy then its result (or its error) through `set`, unless the run or
 *  console changed meanwhile (`stillCurrent` false): then the stale result is dropped. */
export async function runMount(
  place: () => Promise<MountPlaceResult>,
  stillCurrent: () => boolean,
  set: (r: MountRun) => void,
): Promise<void> {
  set({ busy: true, result: null });
  let result: MountPlaceResult;
  try {
    result = await place();
  } catch (e) {
    result = { ok: false, message: e instanceof Error ? e.message : String(e) };
  }
  if (stillCurrent()) set({ busy: false, result });
}
