import type { PayloadLocalEntry } from "../api/ps5";

/** The file the wizard sends for one payload of the chain: the one the user picked, else the copy
 *  already in the app's payload library (left by an older version of the app, which downloaded
 *  them), else none. The app never downloads payloads: the user gets them from each project. */
export interface ChainFile {
  path: string;
  /** Picked by the user, or the library's copy (with its version). */
  from: "picked" | "library";
  version?: string;
}

export function chainFileFor(
  picked: string | null | undefined,
  inventory: readonly PayloadLocalEntry[],
  id: string,
): ChainFile | null {
  const p = picked?.trim();
  if (p) return { path: p, from: "picked" };
  const entry = inventory.find((e) => e.payload_id === id);
  return entry ? { path: entry.path, from: "library", version: entry.version } : null;
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
