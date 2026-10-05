// How much the upload queue still has to write to the console, for the size chip in the queue
// header. Sizes come from the inspection the Upload screen already ran when the item was added.
//
// Deliberately NOT a free-space check: the engine's up-front check (the AVA1 `fs.freespace`
// reservation, resume-aware and job-wide) is the authority, and a second estimate here would
// disagree with it, e.g. by counting a resumed item in full.

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
