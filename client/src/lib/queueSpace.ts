// How much console storage the upload queue still needs, and whether it
// fits. The Upload screen's batch flow already pre-checks ITS rows
// (lib/uploadBatch); nothing watched the queue as a whole — ten games
// queued one at a time could each pass their own check and still add up
// to more than the drive holds. These helpers size the live queue and
// compare it against the destination volumes the payload reports.
//
// Pure helpers first (unit-testable, no I/O); `checkQueueSpace` at the
// bottom is the only call that talks to the console, and it is
// best-effort everywhere: an unreachable payload or an unreadable
// volume yields "no findings", never an exception and never a block.

import {
  fetchVolumes,
  volumeAllocatableBytes,
  volumeForPath,
  type Volume,
} from "../api/ps5";
import type { QueueItem } from "../state/uploadQueue";

/** Best-known size of a queue item in bytes. Prefers the size captured at
 *  queue-add time (pkg header, folder walk or archive central directory);
 *  falls back to the engine's pre-stat, which only lands once the item
 *  starts running. Plain files, multi-part archive sets and queued
 *  installs have no size until then — they count as 0, which makes the
 *  check conservative (it can only ever UNDER-report). */
export function queueItemBytes(it: QueueItem): number {
  if (it.sourceKind === "install") {
    // Install requests carry no size; progress only starts reporting
    // once the install runs.
    return it.installProgress?.total ?? 0;
  }
  return it.estimatedBytes || it.totalBytes || 0;
}

/** Bytes this item still needs on the console: everything for a waiting
 *  item, the un-transferred remainder for the one running. Done and
 *  failed rows need nothing. */
export function queueItemRemainingBytes(it: QueueItem): number {
  if (it.status === "done" || it.status === "failed") return 0;
  const total = queueItemBytes(it);
  if (it.status === "running") return Math.max(0, total - (it.bytesSent || 0));
  return total;
}

/** Total bytes the queue still has to write. */
export function queueRemainingBytes(items: readonly QueueItem[]): number {
  return items.reduce((n, it) => n + queueItemRemainingBytes(it), 0);
}

/** Like `queueRemainingBytes`, but counts every live item in FULL — it
 *  ignores the running item's ticking `bytesSent`. The value only moves
 *  when the queue's composition changes (add / remove / an item's
 *  pre-stated total lands), so a React effect keyed on it doesn't
 *  re-fire at progress-tick rate. */
export function queueCommittedBytes(items: readonly QueueItem[]): number {
  return items.reduce((n, it) => {
    if (it.status === "done" || it.status === "failed") return n;
    return n + queueItemBytes(it);
  }, 0);
}

/** One drive whose queued work doesn't fit. */
export interface QueueSpaceFinding {
  /** Volume mount path, e.g. `/data` or `/mnt/ext0`. */
  volumePath: string;
  /** Bytes the queue still wants to write to this volume. */
  requiredBytes: number;
  /** Bytes the volume can still safely allocate (reserve-aware). */
  allocatableBytes: number;
  /** `requiredBytes - allocatableBytes`. */
  overBy: number;
}

/** Compare the queue against the console's writable drives. Items are
 *  grouped by the volume their `resolvedDest` resolves to (longest
 *  prefix, same rule the upload itself uses) and each group is checked
 *  against that volume's allocatable space. Returns a finding per
 *  OVERSUBSCRIBED volume only; empty when everything fits. Never
 *  throws. */
export async function checkQueueSpace(
  transferAddr: string,
  items: readonly QueueItem[],
): Promise<QueueSpaceFinding[]> {
  const live = items.filter(
    (it) => it.status === "pending" || it.status === "running",
  );
  if (live.length === 0) return [];
  let volumes: Volume[];
  try {
    volumes = await fetchVolumes(transferAddr);
  } catch {
    // Payload unreachable / engine down — warn nobody rather than
    // blocking a queue that might still fit.
    return [];
  }
  const real = volumes.filter(
    (v) => v.writable && !v.is_placeholder,
  );
  if (real.length === 0) return [];

  const needByVolume = new Map<Volume, number>();
  for (const it of live) {
    const need = queueItemRemainingBytes(it);
    if (need <= 0) continue;
    const vol = volumeForPath(real, it.resolvedDest);
    // No volume covers this destination — its free space is unknown,
    // so leave it out instead of guessing.
    if (!vol) continue;
    needByVolume.set(vol, (needByVolume.get(vol) ?? 0) + need);
  }

  const findings: QueueSpaceFinding[] = [];
  for (const [vol, need] of needByVolume) {
    const free = volumeAllocatableBytes(vol);
    if (need > free) {
      findings.push({
        volumePath: vol.path,
        requiredBytes: need,
        allocatableBytes: free,
        overBy: need - free,
      });
    }
  }
  return findings;
}
