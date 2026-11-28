// "Choose files" in an archive (#969). The rows a person ticks go to the engine as `=path`
// entries in the transfer's excludes (see ps5upload_core::excludes): only those entries are read
// out of the archive and sent, a ticked folder with everything in it. Everything ticked sends no
// such entry at all, so the whole archive goes exactly as it did before.

import { isRemotePath } from "./remotePath";
import { tarSuffix } from "./tarNames";

/** Whether the chooser can list this archive: a .zip, .7z, .rar or tar (a lone .zst is one
 *  file, with nothing to choose). */
export function canChooseEntries(path: string): boolean {
  const name = (path.replace(/\\/g, "/").split("/").pop() ?? "").toLowerCase();
  if (name.endsWith(".zip") || name.endsWith(".7z") || name.endsWith(".rar")) return true;
  const tar = tarSuffix(name);
  return tar !== null && tar !== ".zst";
}

/** Whether a queue row offers "Choose files": an archive waiting to be unpacked, read where
 *  the engine can list it (not an archive still on a saved server). */
export function archiveChoosable(item: { sourceKind: string; sourcePath: string; status: string }): boolean {
  return (
    item.sourceKind === "archive" &&
    item.status === "pending" &&
    !isRemotePath(item.sourcePath) &&
    canChooseEntries(item.sourcePath)
  );
}

/** The excludes entries that keep only `chosen` (paths inside the archive). */
export function chosenPatterns(chosen: readonly string[] | null | undefined): string[] {
  return chosen && chosen.length > 0 ? chosen.map((p) => `=${p}`) : [];
}

/** `excludes` plus the choice, for a transfer of the archive. */
export function withArchiveChoice(
  excludes: readonly string[],
  chosen: readonly string[] | null | undefined,
): string[] {
  return [...excludes, ...chosenPatterns(chosen)];
}

export interface ChoiceRow {
  path: string;
  size: number;
  files?: number;
}

export interface ArchiveChoice {
  /** The ticked paths, or null when every row is ticked (the whole archive). */
  paths: string[] | null;
  /** Bytes the ticked rows unpack to. */
  bytes: number;
  /** Ticked rows. */
  count: number;
  /** Files the ticked rows stand for. */
  files: number;
}

/** What the ticks come to. `complete` false (the listing was cut) never narrows: a choice made
 *  from part of a listing would leave the rest out unseen. */
export function choiceOf(
  rows: readonly ChoiceRow[],
  ticked: ReadonlySet<string>,
  complete = true,
): ArchiveChoice {
  const on = rows.filter((r) => ticked.has(r.path));
  const all = on.length === rows.length || !complete;
  const take = all ? rows : on;
  return {
    paths: all ? null : on.map((r) => r.path),
    bytes: take.reduce((n, r) => n + Math.max(0, r.size), 0),
    count: take.length,
    files: take.reduce((n, r) => n + (r.files ?? 1), 0),
  };
}

/** The ticks to start from: an earlier choice (those of its paths still listed), else all. */
export function initialTicks(rows: readonly ChoiceRow[], chosen: readonly string[] | null | undefined): Set<string> {
  if (!chosen || chosen.length === 0) return new Set(rows.map((r) => r.path));
  const listed = new Set(rows.map((r) => r.path));
  const kept = chosen.filter((p) => listed.has(p));
  return new Set(kept.length > 0 ? kept : rows.map((r) => r.path));
}

const NAMED_PART = /^(.*)_(\d+)\.pkg$/i;

function dirOf(path: string): string {
  return path.slice(0, path.lastIndexOf("/") + 1);
}

/**
 * The packages an install unpacks for the ticked ones: each ticked package, and the other parts
 * of a set named by part (`Game_0.pkg` brings `Game_1.pkg`, `Game_2.pkg`… from its folder).
 * Null when every package is ticked (the whole allow-list, as before).
 */
export function packagesToUnpack(
  all: readonly { path: string }[],
  ticked: ReadonlySet<string>,
): string[] | null {
  if (all.every((p) => ticked.has(p.path))) return null;
  const out = new Set<string>();
  for (const p of all) {
    if (!ticked.has(p.path)) continue;
    out.add(p.path);
    const base = p.path.slice(dirOf(p.path).length);
    const m = NAMED_PART.exec(base);
    if (!m || Number(m[2]) !== 0) continue;
    const stem = m[1].toLowerCase();
    for (const q of all) {
      if (dirOf(q.path) !== dirOf(p.path)) continue;
      const n = NAMED_PART.exec(q.path.slice(dirOf(q.path).length));
      if (n && n[1].toLowerCase() === stem && Number(n[2]) > 0) out.add(q.path);
    }
  }
  return all.map((p) => p.path).filter((p) => out.has(p));
}

/** What a package is, from its category: base game, update or DLC (null when not known). */
export function packageRole(category: string | null | undefined): "base" | "update" | "dlc" | null {
  switch ((category ?? "").trim().toLowerCase()) {
    case "gd":
      return "base";
    case "gp":
      return "update";
    case "ac":
      return "dlc";
    default:
      return null;
  }
}
