// Upload with several sources: what a scanned folder's children are, and the checks a batch
// passes before it is queued. Each check blocks or explains; none guesses. Only what certainly
// cannot work blocks.

import { isManualPartSet, parseArchivePart, type ArchiveSetIssue } from "./archiveParts";
import { formatBytes } from "./format";
import { isRarLaterVolume } from "./rarVolumes";
import { isTarArchiveName } from "./tarNames";

export type ScanClass = "folder" | "package" | "archive" | "image" | "other";

/** A child of "Add games from a folder…": folders, packages, archives and images are
 *  offered (checked); anything else is listed unchecked. */
export function classifyScanEntry(name: string, isDir: boolean): ScanClass {
  if (name.startsWith(".")) return "other";
  if (isDir) return "folder";
  const lower = name.toLowerCase();
  if (/\.f?pkg$/.test(lower)) return "package";
  // A later part of a RAR set is read through the first: not a game of its own.
  if (isRarLaterVolume(lower)) return "other";
  if (/\.(zip|7z|rar)$/.test(lower) || isTarArchiveName(lower)) return "archive";
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
  /** What `diagnoseArchiveSet` found about the set this file belongs to. */
  setIssue?: ArchiveSetIssue | null;
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

const fileNameOf = (p: string) => p.slice(Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\")) + 1);

/** A warning about the set a row's file belongs to, or null. Never blocks: sending the file as it
 *  is works, the person may only regret it. */
function setIssueNote(r: BatchCheckRow, live: readonly BatchCheckRow[]): BatchMsg | null {
  const issue = r.setIssue;
  if (!issue) return null;
  switch (issue.kind) {
    case "split-unsupported":
      return {
        key: "batch_split_volume",
        text: "One volume of a split set. Sent alone, the PS5 can't use it. Join the volumes into one archive first.",
      };
    case "pick-entry-volume":
      return {
        key: "batch_pick_entry",
        vars: { entry: issue.entryName },
        text: `One volume of a set. Add ${issue.entryName} instead: the other volumes come with it.`,
      };
    case "mixed-extensions":
      return {
        key: "batch_mixed_parts",
        vars: { ext: issue.selectedExt, other: issue.otherExt },
        text: `The parts beside it are .${issue.otherExt}, so they are not part of this .${issue.selectedExt} set. Only this file's contents would arrive.`,
      };
    case "manual-parts": {
      const self = parseArchivePart(fileNameOf(r.sourcePath));
      if (!self) return null;
      const listed = live.filter((o) => {
        const p = parseArchivePart(fileNameOf(o.sourcePath));
        return p?.ext === self.ext && p.stem.toLowerCase() === self.stem.toLowerCase();
      }).length;
      if (listed >= issue.total) return null;
      return {
        key: "batch_missing_parts",
        vars: { listed, total: issue.total },
        text: `${listed} of ${issue.total} parts are in this list. Add the rest, or only these parts' files arrive.`,
      };
    }
  }
}

export function validateBatch(
  rows: BatchCheckRow[],
  queued: { sourcePath: string; resolvedDest: string }[],
  freeByVolume: Map<string, number | null>,
  /** How much is likely to fit on each drive. Less than free on internal storage, where the
   *  PS5 holds back more as it writes. A batch over this is warned about, never blocked. Neither
   *  is one over free blocked: a resumed row counts here in full, though most of it may be on the
   *  console already, and the engine's check at the start of each job knows that. */
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
    // The parts of one zip/7z set land in one folder on purpose (see resolveUploadDest).
    if (isManualPartSet(same.map((r) => fileNameOf(r.sourcePath)))) continue;
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
    const setNote = setIssueNote(r, live);
    if (setNote) note(r.id, setNote);
  }

  const space: BatchMsg[] = [];
  let over = false;
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
      over = true;
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

  if (over) {
    space.push({
      key: "batch_space_short_hint",
      text: "What an earlier try left on the PS5 isn't counted here, so you can still try. The upload stops with a clear message if it runs out of room.",
    });
  }

  return { issues, skip, space, blocked };
}
