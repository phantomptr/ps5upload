/**
 * Console clean-up: grouping, selection and totals, shared by the PC app's Clean up screen and
 * the console page. Plain data, no React and no strings (each surface translates).
 */
import type { CleanupApplyStatus, CleanupCategory, CleanupItem } from "../api/cleanup";

/** The order the groups are shown in. */
export const CATEGORY_ORDER: CleanupCategory[] = ["A", "B", "C", "D", "E", "F", "G"];

/** Can this item be ticked at all (G, anything the helper marks "none" and Kraken Mount's games
 *  are only listed). */
export function isRemovable(item: CleanupItem): boolean {
  return item.action !== "none" && item.cat !== "G" && !item.kept;
}

/** What removing it frees: the space it takes on the drive, or its size when that is unknown. */
export function itemBytes(item: CleanupItem): number {
  return item.alloc_bytes > 0 ? item.alloc_bytes : item.bytes;
}

export interface CleanupGroup {
  cat: CleanupCategory;
  items: CleanupItem[];
  bytes: number;
}

/** Items grouped by category in CATEGORY_ORDER, largest first; empty groups are left out. */
export function groupItems(items: CleanupItem[]): CleanupGroup[] {
  return CATEGORY_ORDER.map((cat) => {
    const its = items.filter((i) => i.cat === cat).sort((a, b) => itemBytes(b) - itemBytes(a));
    return { cat, items: its, bytes: its.reduce((n, i) => n + itemBytes(i), 0) };
  }).filter((g) => g.items.length > 0);
}

/** The items ticked when the list first shows. */
export function initialSelection(items: CleanupItem[]): Set<number> {
  return new Set(items.filter((i) => i.default && isRemovable(i)).map((i) => i.id));
}

export interface SelectionTotals {
  count: number;
  bytes: number;
  /** Bytes per drive, largest first. */
  byDrive: { drive: string; bytes: number }[];
  /** An update of an installed game is selected: the confirm step warns about the downgrade. */
  downgrades: CleanupItem[];
}

export function selectionTotals(items: CleanupItem[], selected: ReadonlySet<number>): SelectionTotals {
  const per = new Map<string, number>();
  let count = 0;
  let bytes = 0;
  const downgrades: CleanupItem[] = [];
  for (const i of items) {
    if (!selected.has(i.id) || !isRemovable(i)) continue;
    count++;
    bytes += itemBytes(i);
    per.set(i.drive, (per.get(i.drive) ?? 0) + itemBytes(i));
    if (i.cat === "A") downgrades.push(i);
  }
  const byDrive = [...per.entries()].map(([drive, b]) => ({ drive, bytes: b })).sort((a, b) => b.bytes - a.bytes);
  return { count, bytes, byDrive, downgrades };
}

/** Select-all state of a group: "all", "some" or "none" of its removable items. */
export function groupState(group: CleanupGroup, selected: ReadonlySet<number>): "all" | "some" | "none" {
  const rem = group.items.filter(isRemovable);
  const n = rem.filter((i) => selected.has(i.id)).length;
  if (n === 0) return "none";
  return n === rem.length ? "all" : "some";
}

/** The selection with every removable item of `group` ticked (on) or unticked. */
export function setGroup(group: CleanupGroup, selected: ReadonlySet<number>, on: boolean): Set<number> {
  const next = new Set(selected);
  for (const i of group.items) {
    if (!isRemovable(i)) continue;
    if (on) next.add(i.id);
    else next.delete(i.id);
  }
  return next;
}

export function toggle(selected: ReadonlySet<number>, id: number): Set<number> {
  const next = new Set(selected);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  return next;
}

/** How a finished (or running) apply went, with the items it names. */
export interface ApplySummary {
  removed: CleanupItem[];
  skipped: { item: CleanupItem; reason: string; code?: string }[];
  failed: { item: CleanupItem; reason: string; code?: string }[];
  freed: number;
}

export function summarizeApply(status: CleanupApplyStatus, items: CleanupItem[]): ApplySummary {
  const byId = new Map(items.map((i) => [i.id, i]));
  const pick = (rows: { id: number; reason: string; code?: string }[]) =>
    rows.flatMap((r) => {
      const item = byId.get(r.id);
      return item ? [{ item, reason: r.reason, code: r.code }] : [];
    });
  return {
    removed: status.done.flatMap((id) => {
      const item = byId.get(id);
      return item ? [item] : [];
    }),
    skipped: pick(status.skipped),
    failed: pick(status.failed),
    freed: status.freed_bytes,
  };
}

/** The drive token as its kind and number: internal, ext 0, usb 2. */
export function driveParts(drive: string): { kind: "internal" | "ext" | "usb" | "other"; n: number } {
  if (drive === "internal") return { kind: "internal", n: 0 };
  const m = /^(ext|usb)(\d)$/.exec(drive);
  if (m) return { kind: m[1] as "ext" | "usb", n: Number(m[2]) };
  return { kind: "other", n: 0 };
}

/** The last path component (a package's file name). */
export function baseName(path: string): string {
  const i = path.lastIndexOf("/");
  return i >= 0 ? path.slice(i + 1) : path;
}

/** The folder holding `path` ("Show in Files"). */
export function parentDir(path: string): string {
  const i = path.lastIndexOf("/");
  return i > 0 ? path.slice(0, i) : "/";
}
