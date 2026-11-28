// Save sync's choices on screen (engine save_sync.rs): which way each save goes, which ones can
// be picked, and which suggestion to show. Pure, so they're tested without a console.
import type { BadgeTone } from "../../components/Badge";
import type {
  AutoState,
  AutoView,
  SyncDirection,
  SyncGame,
  SyncPair,
  SyncRow,
  SyncStatus,
  SyncSuggestion,
} from "../../api/saveSync";
import { hostOf } from "../../lib/addr";

/** A PS5 save on one side only isn't sent by Sync: the game makes a save there first, or the user
 *  asks for the one-time first copy (`firstCopy`). An emulator's goes into the same place on the
 *  other console. The side with no save. */
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

// ── kept in step automatically (engine save_sync_auto.rs) ────────────────

/** Whether the pair keeps this save in step by itself. */
export function keptInStep(pair: SyncPair, key: string): boolean {
  return (pair.auto ?? []).includes(key);
}

/** Where a save kept in step stands, for its row: the engine's view, or "waiting for the next
 *  look" for one just turned on. Null for a save not kept in step. */
export function autoOf(row: SyncRow, pair: SyncPair): AutoView | null {
  if (!keptInStep(pair, row.key)) return null;
  return (
    row.auto ?? {
      state: "waiting",
      title_id: row.title_id,
      kind: row.kind,
      side: null,
      reason: "queued",
      detail: null,
      direction: null,
      first: false,
      kept: [],
      since_ms: 0,
      fails: 0,
      woke_ms: null,
      copied_ms: null,
      copied_direction: null,
    }
  );
}

export function autoTone(s: AutoState): BadgeTone {
  switch (s) {
    case "in_step":
      return "good";
    case "conflict":
      return "warn";
    case "cant_sync":
      return "bad";
    default:
      return "neutral";
  }
}

/** The console a state is about, by name (the one waited for, or the one a reason names). */
export function autoConsole(
  v: AutoView,
  pair: SyncPair,
  profiles: readonly { host: string; name: string }[],
): string | null {
  if (!v.side) return null;
  return consoleName(v.side === "a" ? pair.a.host : pair.b.host, profiles);
}

/** Turning on is offered for every save but a PS4 one. A PS5 save the other user has none of
 *  waits for the first copy or the game's own first save there. (The engine checks again, and
 *  says why when it refuses.) */
export function canTurnOn(row: SyncRow): boolean {
  return row.kind !== "ps4" && row.reason !== "ps4";
}

export type FirstCopyCode = "old_helper" | "no_saved_data" | "no_backup_place";

/** A PS5 save on one side only: the side the one-time first copy goes to, and whether it can be
 *  made (`code`, else `why`: the engine's words, when not). Null when there is nothing to copy that
 *  way. */
export function firstCopyOf(
  row: SyncRow,
): { to: "a" | "b"; ok: boolean; why: string | null; code: FirstCopyCode | null } | null {
  const to = needsFirstStart(row);
  if (!to || row.reason) return null;
  return {
    to,
    ok: row.first_copy?.ok === true,
    why: row.first_copy?.why ?? null,
    code: row.first_copy?.code ?? null,
  };
}

/** The side of the pair that is not the console shown: "Keep in step on <console> (<user>)". */
export function otherSide(pair: SyncPair, host: string): "a" | "b" {
  return hostOf(pair.a.host) === hostOf(host) ? "b" : "a";
}

/** Copies made by keeping in step that were not seen before (for a notification), and what is
 *  seen now. The first look only remembers: what happened before the app opened is not news. */
export function newCopies(
  games: readonly SyncGame[],
  seen: ReadonlyMap<string, number> | null,
): { fresh: SyncGame[]; seen: Map<string, number> } {
  const next = new Map<string, number>();
  const fresh: SyncGame[] = [];
  for (const g of games) {
    const id = `${g.pair}|${g.key}`;
    const at = g.copied_ms ?? 0;
    next.set(id, at);
    if (seen && at > (seen.get(id) ?? 0)) fresh.push(g);
  }
  return { fresh, seen: next };
}
