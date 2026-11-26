// "Send to another console" (#433) as queue rows: one copy per item, from one console to another,
// on the target console's queue. Files' Send dialog and its two panes (#952) both queue here.

import { hostOf } from "../lib/addr";
import type { SendItem } from "../lib/consoleSend";
import { useC2cRoutes } from "./c2cRoutes";
import { useUploadQueueStore, type QueueItem } from "./uploadQueue";

/** Queues each item from `fromHost` to `target` at its `dests[i]` and starts them; the row ids. */
export function queueConsoleSend(fromHost: string, target: string, items: SendItem[], dests: string[]): string[] {
  const from = hostOf(fromHost);
  const to = hostOf(target);
  const route = useC2cRoutes.getState().routeFor(from, to);
  const q = useUploadQueueStore.getState();
  const ids = items.map((it, i) =>
    q.add({
      sourceKind: "ps5",
      fromConsole: from,
      route: route.mode,
      directAddr: route.directAddr || null,
      sourcePath: it.path,
      displayName: it.name,
      resolvedDest: dests[i],
      addr: to,
      strategy: "overwrite",
      reconcileMode: "fast",
      excludes: [],
      mountAfterUpload: false,
      mountReadOnly: true,
      registerAfterUpload: false,
      estimatedBytes: it.size,
    }),
  );
  q.startAdded(to, ids);
  return ids;
}

/** How a queued row ended: its last state, or "gone" when it was removed from the queue first. */
export type QueueRowEnd = { status: "done" | "failed"; row: QueueItem } | { status: "gone" };

/** Resolves once the queue row `id` has ended (done or failed) or left the queue. */
export function whenQueueRowEnds(id: string): Promise<QueueRowEnd> {
  return new Promise((resolve) => {
    let settled = false;
    let unsub: () => void = () => {};
    const check = (items: QueueItem[]) => {
      if (settled) return;
      const row = items.find((r) => r.id === id);
      if (row && row.status !== "done" && row.status !== "failed") return;
      settled = true;
      unsub();
      resolve(row ? { status: row.status as "done" | "failed", row } : { status: "gone" });
    };
    unsub = useUploadQueueStore.subscribe((s) => check(s.items));
    check(useUploadQueueStore.getState().items);
  });
}
