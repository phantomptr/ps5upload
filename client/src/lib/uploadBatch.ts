// Upload with several sources: what a scanned folder's children are, and the checks a batch
// passes before it is queued. Each check blocks or explains; none guesses.

import { formatBytes } from "./format";

export type ScanClass = "folder" | "package" | "archive" | "image" | "other";

/** A child of "Add games from a folder…": folders, packages, archives and images are
 *  offered (checked); anything else is listed unchecked. */
export function classifyScanEntry(name: string, isDir: boolean): ScanClass {
  if (name.startsWith(".")) return "other";
  if (isDir) return "folder";
  const lower = name.toLowerCase();
  if (/\.f?pkg$/.test(lower)) return "package";
  if (/\.(zip|7z|rar)$/.test(lower)) return "archive";
  if (/\.(exfat|ffpkg|ffpfs|ffpfsc)$/.test(lower)) return "image";
  return "other";
}

/** `/data/homebrew/G` → `/data`; `/mnt/ext0/…` → `/mnt/ext0`. */
export function volumeOfDest(dest: string): string {
  const parts = dest.split("/").filter(Boolean);
  if (parts[0] === "mnt" && parts[1]) return `/mnt/${parts[1]}`;
  return `/${parts[0] ?? ""}`;
}

export interface BatchCheckRow {
  id: string;
  sourcePath: string;
  dest: string;
  /** Bytes it will take on the console; null when not known. */
  size: number | null;
  /** An encrypted archive still waiting for its password. */
  needsPassword: boolean;
}

/** A message for the person: a translation key, its values, and the English text. */
export interface BatchMsg {
  key: string;
  vars?: Record<string, string | number>;
  text: string;
}

export interface BatchCheck {
  /** Per row, what is wrong with it. */
  issues: Map<string, BatchMsg[]>;
  /** Rows already in the queue: left out, not an error. */
  skip: Set<string>;
  /** Space findings, per drive. */
  space: BatchMsg[];
  /** Nothing can be added until these are fixed. */
  blocked: boolean;
}

export function validateBatch(
  rows: BatchCheckRow[],
  queued: { sourcePath: string; resolvedDest: string }[],
  freeByVolume: Map<string, number | null>,
  /** How much is likely to fit on each drive. Less than free on internal storage, where the
   *  PS5 holds back more as it writes. A batch over this is warned about, never blocked. */
  likelyFitsByVolume: Map<string, number | null> = new Map(),
): BatchCheck {
  const issues = new Map<string, BatchMsg[]>();
  const note = (id: string, msg: BatchMsg) => issues.set(id, [...(issues.get(id) ?? []), msg]);
  const skip = new Set<string>();
  let blocked = false;

  for (const r of rows) {
    if (queued.some((q) => q.sourcePath === r.sourcePath && q.resolvedDest === r.dest)) skip.add(r.id);
  }
  const live = rows.filter((r) => !skip.has(r.id));

  const byDest = new Map<string, BatchCheckRow[]>();
  for (const r of live) byDest.set(r.dest, [...(byDest.get(r.dest) ?? []), r]);
  for (const [dest, same] of byDest) {
    if (same.length < 2) continue;
    blocked = true;
    for (const r of same) {
      note(r.id, {
        key: "batch_same_dest",
        vars: { dest },
        text: `same destination as another row (${dest}); remove one`,
      });
    }
  }

  for (const r of live) {
    if (r.needsPassword) {
      blocked = true;
      note(r.id, { key: "batch_needs_password", text: "needs its password" });
    }
  }

  const space: BatchMsg[] = [];
  const byVolume = new Map<string, BatchCheckRow[]>();
  for (const r of live) {
    const v = volumeOfDest(r.dest);
    byVolume.set(v, [...(byVolume.get(v) ?? []), r]);
  }
  for (const [vol, list] of byVolume) {
    const known = list.reduce((n, r) => n + (r.size ?? 0), 0);
    const unknown = list.filter((r) => r.size == null).length;
    const free = freeByVolume.get(vol);
    if (free == null) {
      space.push({
        key: "batch_space_unknown",
        vars: { drive: vol, size: formatBytes(known) },
        text: `${vol}: free space can't be read; check it has room for ${formatBytes(known)}`,
      });
    } else if (known > free) {
      blocked = true;
      space.push({
        key: "batch_space_short",
        vars: { drive: vol, size: formatBytes(known), free: formatBytes(free) },
        text: `${vol}: needs ${formatBytes(known)}, only ${formatBytes(free)} free`,
      });
    } else if ((likelyFitsByVolume.get(vol) ?? free) < known) {
      const fits = likelyFitsByVolume.get(vol) ?? free;
      space.push({
        key: "batch_space_tight",
        vars: { drive: vol, size: formatBytes(known), fits: formatBytes(fits) },
        text: `${vol}: ${formatBytes(known)} may not fit. The PS5 holds back about a fifth more as it writes, so about ${formatBytes(fits)} is likely to fit. You can still try.`,
      });
    } else if (unknown > 0) {
      space.push({
        key: "batch_space_partial",
        vars: { drive: vol, count: unknown, size: formatBytes(known) },
        text: `${vol}: the size of ${unknown} item(s) isn't known; ${formatBytes(known)} of the rest fits`,
      });
    }
  }

  return { issues, skip, space, blocked };
}
