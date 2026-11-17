// Save sync's choices on screen (engine save_sync.rs): which way each save goes, which ones can
// be picked, and which suggestion to show. Pure, so they're tested without a console.
import type { BadgeTone } from "../../components/Badge";
import type { SyncDirection, SyncPair, SyncRow, SyncStatus, SyncSuggestion } from "../../api/saveSync";
import { hostOf } from "../../lib/addr";

/** A PS5 save on one side only can't be sent: the game must make a save there first. An
 *  emulator's goes into the same place on the other console. */
export function needsFirstStart(row: SyncRow): "a" | "b" | null {
  if (row.kind !== "ps5") return null;
  if (row.status === "only_on_a") return "b";
  if (row.status === "only_on_b") return "a";
  return null;
}

/** The way a save goes unless the user picks: from the side that changed. None when both
 *  changed or it was never synced (the user picks), or nothing can be sent. */
export function defaultDirection(row: SyncRow): SyncDirection | null {
  switch (row.status) {
    case "a_newer":
      return "a_to_b";
    case "b_newer":
      return "b_to_a";
    case "only_on_a":
      return needsFirstStart(row) ? null : "a_to_b";
    case "only_on_b":
      return needsFirstStart(row) ? null : "b_to_a";
    default:
      return null;
  }
}

/** Both changed, or never synced: the user picks which side to keep. */
export function needsPick(row: SyncRow): boolean {
  return row.status === "both_changed" || row.status === "first_time";
}

/** The save can be sent now (after a pick where one is needed). */
export function canSync(row: SyncRow): boolean {
  return needsPick(row) || defaultDirection(row) !== null;
}

/** The ways a save may go: only from the changed side, either way when the user picks. */
export function directionsFor(row: SyncRow): SyncDirection[] {
  if (needsPick(row)) return ["a_to_b", "b_to_a"];
  const d = defaultDirection(row);
  return d ? [d] : [];
}

/** The copies to ask for: each selected save, the way picked for it or its own way. A save that
 *  needs a pick and has none is left out. */
export function applyItems(
  rows: readonly SyncRow[],
  selected: ReadonlySet<string>,
  picks: Readonly<Record<string, SyncDirection | undefined>>,
): { key: string; direction: SyncDirection }[] {
  const out: { key: string; direction: SyncDirection }[] = [];
  for (const r of rows) {
    if (!selected.has(r.key) || !canSync(r)) continue;
    const pick = picks[r.key];
    const direction = pick && directionsFor(r).includes(pick) ? pick : defaultDirection(r);
    if (direction) out.push({ key: r.key, direction });
  }
  return out;
}

/** The saves picked at first: every one that changed on one side only. Conflicts wait for a
 *  pick. */
export function initialSelection(rows: readonly SyncRow[]): Set<string> {
  return new Set(rows.filter((r) => defaultDirection(r) !== null).map((r) => r.key));
}

const ORDER: Record<SyncStatus, number> = {
  both_changed: 0,
  first_time: 1,
  a_newer: 2,
  b_newer: 2,
  only_on_a: 3,
  only_on_b: 3,
  unavailable: 4,
  in_step: 5,
  excluded: 6,
};

/** What needs the user first, then by game. */
export function sortRows(rows: readonly SyncRow[]): SyncRow[] {
  return [...rows].sort((x, y) => ORDER[x.status] - ORDER[y.status] || x.title_id.localeCompare(y.title_id));
}

export function statusTone(s: SyncStatus): BadgeTone {
  switch (s) {
    case "in_step":
      return "good";
    case "a_newer":
    case "b_newer":
      return "accent";
    case "both_changed":
    case "first_time":
      return "warn";
    default:
      return "neutral";
  }
}

/** A console's name from the app's console list, else its address. */
export function consoleName(host: string, profiles: readonly { host: string; name: string }[]): string {
  const h = hostOf(host);
  return profiles.find((p) => hostOf(p.host) === h)?.name || h;
}

export interface Prompt {
  suggestion: SyncSuggestion;
  pair: SyncPair;
  from: string;
  to: string;
}

/** The suggestion to show: the oldest one of a pair still set up, less those put off here. */
export function promptFor(
  suggestions: readonly SyncSuggestion[],
  pairs: readonly SyncPair[],
  profiles: readonly { host: string; name: string }[],
  later: ReadonlySet<string>,
): Prompt | null {
  const sorted = [...suggestions].sort((x, y) => x.at_ms - y.at_ms);
  for (const s of sorted) {
    if (later.has(suggestionId(s))) continue;
    const pair = pairs.find((p) => p.id === s.pair);
    if (!pair) continue;
    const [from, to] = s.direction === "a_to_b" ? [pair.a, pair.b] : [pair.b, pair.a];
    return { suggestion: s, pair, from: consoleName(from.host, profiles), to: consoleName(to.host, profiles) };
  }
  return null;
}

/** One suggestion, as it is (a later one of the same save, or another state, is new). */
export function suggestionId(s: SyncSuggestion): string {
  return `${s.pair}|${s.key}|${s.state}|${s.at_ms}`;
}

/** The other PS5 picked in the setup: the one picked while it is still offered, else the first
 *  one offered (the console list can load after the form, or change under it). */
export function pickOther(picked: string, offered: readonly string[]): string {
  return offered.includes(picked) ? picked : (offered[0] ?? "");
}

/** Whether "Keep in step" can go: a user on each side and another console (never this one). */
export function canKeepInStep(s: { host: string; here: string; other: string; there: string }): boolean {
  return !!s.here && !!s.there && !!s.other && hostOf(s.other) !== hostOf(s.host);
}
