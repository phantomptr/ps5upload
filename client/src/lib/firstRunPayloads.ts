import type { PayloadLocalEntry } from "../api/ps5";

/** The cached copy to send instead of downloading: only one of the release being asked for.
 *  Any other version (or a copy from before versions were recorded) is downloaded again, which
 *  is what a firmware change that needs a newer kstuff depends on. */
export function cachedCopyOfRelease(
  inventory: readonly PayloadLocalEntry[],
  id: string,
  tag: string,
): PayloadLocalEntry | null {
  const entry = inventory.find((e) => e.payload_id === id);
  return entry && entry.version === tag ? entry : null;
}

export interface RunRow {
  state: "idle" | "busy" | "ok" | "fail";
}

/** A chain that ended without finishing leaves no spinner: `to` for rows still busy. */
export function settleBusyRows<R extends RunRow>(rows: readonly R[], to: RunRow["state"]): R[] {
  return rows.map((r) => (r.state === "busy" ? { ...r, state: to } : r));
}

/** One run of a chain at a time. Cancel and a new run both end the one before, whose next check
 *  then says stop: one shared "cancelled" flag was reset by the new run, and the old one carried
 *  on beside it (kstuff sent twice, the stacking the wizard warns about). */
export function createRunTokens(): { start: () => () => boolean; cancel: () => void } {
  let current = 0;
  return {
    start: () => {
      const mine = ++current;
      return () => mine === current;
    },
    cancel: () => {
      current += 1;
    },
  };
}
