// Pure helpers for upload-queue and payload-playlist mutations.
//
// Extracted from the Zustand store so reorder, remove, retry, and
// terminal-state transitions are unit-testable without the
// store/Tauri/React surface.

/** Move the item with `id` one slot earlier in the array. Returns the
 *  same array reference when nothing changes (already at top, or id
 *  not found) so a Zustand `set` can short-circuit. */
export function moveItemUp<T extends { id: string }>(items: T[], id: string): T[] {
  const i = items.findIndex((it) => it.id === id);
  if (i <= 0) return items;
  const next = items.slice();
  [next[i - 1], next[i]] = [next[i], next[i - 1]];
  return next;
}

/** Move the item with `id` one slot later in the array. Returns the
 *  same array reference when nothing changes. */
export function moveItemDown<T extends { id: string }>(items: T[], id: string): T[] {
  const i = items.findIndex((it) => it.id === id);
  if (i < 0 || i >= items.length - 1) return items;
  const next = items.slice();
  [next[i], next[i + 1]] = [next[i + 1], next[i]];
  return next;
}

/** Move `id` earlier, but only past items in the SAME group (e.g. same
 *  target console). Swaps with the nearest preceding same-group item so the
 *  per-console grouped queue reorders within its console without disturbing
 *  other consoles' rows. `groupOf` maps an item to its group key. Returns the
 *  same reference when there's no earlier same-group item. Pure — exported for
 *  tests. */
export function moveItemUpWithinGroup<T extends { id: string }>(
  items: T[],
  id: string,
  groupOf: (it: T) => string,
): T[] {
  const i = items.findIndex((it) => it.id === id);
  if (i < 0) return items;
  const g = groupOf(items[i]);
  let j = -1;
  for (let k = i - 1; k >= 0; k--) {
    if (groupOf(items[k]) === g) {
      j = k;
      break;
    }
  }
  if (j < 0) return items;
  const next = items.slice();
  [next[j], next[i]] = [next[i], next[j]];
  return next;
}

/** Move `id` later within its group — the mirror of
 *  `moveItemUpWithinGroup`. */
export function moveItemDownWithinGroup<T extends { id: string }>(
  items: T[],
  id: string,
  groupOf: (it: T) => string,
): T[] {
  const i = items.findIndex((it) => it.id === id);
  if (i < 0) return items;
  const g = groupOf(items[i]);
  let j = -1;
  for (let k = i + 1; k < items.length; k++) {
    if (groupOf(items[k]) === g) {
      j = k;
      break;
    }
  }
  if (j < 0) return items;
  const next = items.slice();
  [next[i], next[j]] = [next[j], next[i]];
  return next;
}

/** Remove the item with `id`. Same-reference short-circuit when not
 *  found. */
export function removeItem<T extends { id: string }>(items: T[], id: string): T[] {
  const i = items.findIndex((it) => it.id === id);
  if (i < 0) return items;
  return items.slice(0, i).concat(items.slice(i + 1));
}

/** Replace the item with `id` by applying `patch`. Returns the same
 *  reference when not found so callers don't trigger pointless
 *  re-renders. */
export function patchItem<T extends { id: string }>(
  items: T[],
  id: string,
  patch: Partial<T>,
): T[] {
  const i = items.findIndex((it) => it.id === id);
  if (i < 0) return items;
  const next = items.slice();
  next[i] = { ...next[i], ...patch };
  return next;
}

/** Find the next "pending" item to run. Returns null when the queue
 *  has no remaining pending work. Stable order = array order, so
 *  reorder-by-user maps directly to run-order. */
export function nextPending<T extends { id: string; status: string }>(
  items: T[],
): T | null {
  return items.find((it) => it.status === "pending") ?? null;
}

/** Decide whether the runner should continue after a failed item.
 *  Centralised so the rule (default = stop, opt-in skip) is testable
 *  and obvious instead of buried in the runner loop. */
export function shouldContinueAfterFailure(continueOnFailure: boolean): boolean {
  return continueOnFailure;
}

/** Reset every non-running item back to pending. Used by the "retry
 *  failed" action so users don't have to dismiss + re-add each one. */
export function resetFailedToPending<T extends { id: string; status: string }>(
  items: T[],
): T[] {
  let changed = false;
  const next = items.map((it) => {
    if (it.status === "failed") {
      changed = true;
      return { ...it, status: "pending" as const };
    }
    return it;
  });
  return changed ? next : items;
}

/** Reset any item that was actively running back to pending and zero
 *  the live counters. Used by the queue's stop() so a stopped row
 *  doesn't keep displaying its last mid-flight bytes/sec readout
 *  against a frozen progress bar. Same shape as resetFailedToPending
 *  (same-reference short-circuit when nothing was running).
 *
 *  `match` optionally narrows which running items reset — the per-console
 *  Stop passes a host predicate so stopping console B doesn't disturb
 *  console A's still-running row. Omit it to reset every running item. */
export function resetRunningToPending<
  T extends {
    id: string;
    status: string;
    bytesSent: number;
    totalBytes: number;
    bytesPerSec: number;
    recovering?: boolean;
    recoverAttempt?: number;
  },
>(items: T[], match?: (it: T) => boolean): T[] {
  let changed = false;
  const next = items.map((it) => {
    if (it.status === "running" && (!match || match(it))) {
      changed = true;
      return {
        ...it,
        status: "pending" as const,
        bytesSent: 0,
        totalBytes: 0,
        bytesPerSec: 0,
        // A "running" item may have been mid-auto-recovery; clear the
        // transient recovery flags so a stopped/hydrated item isn't
        // persisted as a pending row with `recovering:true`.
        recovering: false,
        recoverAttempt: 0,
      };
    }
    return it;
  });
  return changed ? next : items;
}
