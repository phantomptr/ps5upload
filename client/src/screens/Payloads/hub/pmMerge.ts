// "On your PS5": the Payload Manager list and the running cards, as one picture. A payload
// Payload Manager keeps that is running shows on its saved row (badge, pid, Stop); its card is
// not shown a second time below. What is left running gets one line on who started it, so
// nobody reads a card as "PS5Upload loaded this". Pure: OnPs5Panel feeds it and renders.

import type { PmStoredPayload } from "../../../api/payloadManager";
import type { ProcessInfo } from "../../../api/ps5";
import { isGenericProcName, normProcName, type HubCard, type HubEntry } from "./registry";

/** A saved file's name as compared with process names: no folder, no extension, lower case. */
export function savedKey(filename: string): string {
  const base = filename.split(/[\\/]/).pop() ?? filename;
  return base.trim().toLowerCase().replace(/\.(elf|bin)$/, "");
}

/** Short names some payloads' files go by that their process names don't contain (whole words
 *  of the file name only: "smp-1.7.elf", not "smpx.elf"). */
const FILE_ALIASES: Record<string, string[]> = {
  shadowmountplus: ["smp", "shadowmount"],
};

/** A name part shorter than this is too loose to find inside a file name. */
const MIN_PART = 4;

interface CardNames {
  /** Names the card's payload runs as (process names, catalogue hints), normalised. */
  exact: Set<string>;
  /** Parts that mark a file as the card's payload when found inside its name. */
  parts: string[];
  /** Whole words of a file name that mark it as the card's. */
  words: string[];
}

function namesOf(card: HubCard, entries: readonly HubEntry[]): CardNames {
  const exact = new Set<string>();
  const parts: string[] = [];
  const words: string[] = [];
  const add = (raw: string) => {
    if (!raw || isGenericProcName(raw)) return;
    const n = normProcName(raw);
    if (!n) return;
    exact.add(n);
    if (n.length >= MIN_PART) parts.push(n);
  };
  for (const p of card.processes) {
    add(p.comm);
    add(p.name);
  }
  for (const id of card.entryIds) {
    const e = entries.find((x) => x.id === id);
    if (!e) continue;
    e.processNames.forEach(add);
    for (const c of e.processContains ?? []) {
      const l = c.toLowerCase();
      exact.add(l);
      parts.push(l);
    }
    for (const a of FILE_ALIASES[id] ?? []) {
      if (a.length >= MIN_PART) parts.push(a);
      words.push(a);
    }
  }
  return { exact, parts, words };
}

/** The cards a saved row can stand for: running, with a process to show and stop, and not
 *  Payload Manager itself (its card holds the list). */
function candidates(cards: readonly HubCard[]): HubCard[] {
  return cards.filter(
    (c) => c.state === "running" && c.processes.length > 0 && c.id !== "payload-manager",
  );
}

/** Pairs where each side has exactly one partner; ambiguity on either side pairs nothing. */
function uniquePairs(pairs: ReadonlyArray<readonly [number, HubCard]>): Array<[number, HubCard]> {
  const perRow = new Map<number, number>();
  const perCard = new Map<HubCard, number>();
  for (const [r, c] of pairs) {
    perRow.set(r, (perRow.get(r) ?? 0) + 1);
    perCard.set(c, (perCard.get(c) ?? 0) + 1);
  }
  return pairs.filter(([r, c]) => perRow.get(r) === 1 && perCard.get(c) === 1).map(([r, c]) => [r, c]);
}

/**
 * Which running card each saved Payload Manager file is, by the file's path.
 *
 * - exact: the file's name without extension is a name the card's payload runs as
 *   ("kstuff.elf" ↔ kstuff.elf);
 * - family: the file's name contains one of them ("kstuff-1.13-fpkg.elf" ↔ kstuff.elf,
 *   "kraken-mount.elf" ↔ Kraken Mount, which may run as payload.elf: the card's own pid
 *   evidence decides that), or a known short name as a whole word ("smp.elf" ↔ ShadowMount+).
 *
 * Exact pairs are made first. A file or a card with two candidates at the same step is paired
 * with nothing, so a card is never claimed by a guess.
 */
export function matchSavedToCards(
  saved: ReadonlyArray<Pick<PmStoredPayload, "path" | "filename">>,
  cards: readonly HubCard[],
  entries: readonly HubEntry[],
): Map<string, HubCard> {
  const out = new Map<string, HubCard>();
  const pool = candidates(cards).map((card) => ({ card, names: namesOf(card, entries) }));
  const keys = saved.map((s) => savedKey(s.filename));
  const takenRows = new Set<number>();
  const takenCards = new Set<HubCard>();
  const step = (test: (key: string, n: CardNames) => boolean) => {
    const pairs: Array<[number, HubCard]> = [];
    keys.forEach((key, r) => {
      if (!key || takenRows.has(r)) return;
      for (const { card, names } of pool) {
        if (!takenCards.has(card) && test(key, names)) pairs.push([r, card]);
      }
    });
    for (const [r, card] of uniquePairs(pairs)) {
      takenRows.add(r);
      takenCards.add(card);
      out.set(saved[r].path, card);
    }
  };
  step((key, n) => n.exact.has(key));
  step((key, n) => {
    if (n.parts.some((p) => key.includes(p))) return true;
    const words = key.split(/[^a-z0-9]+/);
    return n.words.some((w) => words.includes(w));
  });
  return out;
}

export interface CardGroups {
  /** Shown as today, at the top: Payload Manager's card (it holds the list), a matched card
   *  that has its own panel (ShadowMount+, Hush…: Manage stays reachable), and payloads found
   *  installed but not running. */
  top: HubCard[];
  /** Running, not in the Payload Manager list: under "Also running". */
  also: HubCard[];
  /** Matched to a saved row and shown there only. */
  hidden: Set<string>;
}

/** Splits the cards once the Payload Manager list is known (`matched` from
 *  `matchSavedToCards`). Without a list nothing is merged: call it only with one. */
export function groupCards(cards: readonly HubCard[], matched: ReadonlyMap<string, HubCard>): CardGroups {
  const matchedIds = new Set([...matched.values()].map((c) => c.id));
  const top: HubCard[] = [];
  const also: HubCard[] = [];
  const hidden = new Set<string>();
  for (const c of cards) {
    if (matchedIds.has(c.id)) {
      if (c.integration) top.push(c);
      else hidden.add(c.id);
    } else if (c.id === "payload-manager" || c.state !== "running") {
      top.push(c);
    } else {
      also.push(c);
    }
  }
  return { top, also, hidden };
}

/** Who started a running payload, as the card's one line says it. */
export type Origin =
  /** In the Payload Manager list. */
  | "pm"
  /** The PS5Upload helper or one of its own pieces. */
  | "ours"
  /** elfldr, the build PS5Upload swapped in. */
  | "loader-ours"
  /** elfldr as the jailbreak or Payload Manager started it. */
  | "loader"
  /** Anything else running, Payload Manager itself included. */
  | "console";

/** PS5Upload's own processes: the helper (`ps5upload.elf`), its installer daemon, and pieces it
 *  starts under `ps5u-` names (the converter, the extractor, the keyboard companion). */
export function isOurProcess(p: Pick<ProcessInfo, "comm" | "name" | "is_self">): boolean {
  if (p.is_self) return true;
  return [p.comm, p.name].some((raw) => {
    const n = normProcName(raw ?? "");
    return n === "ps5upload" || n.startsWith("ps5upload-") || n.startsWith("ps5upload_") || n.startsWith("ps5u-");
  });
}

/** Where a running card's payload came from. `ourLoaderPid` is the elfldr pid PS5Upload put
 *  in place on this console, when it knows one. */
export function originOf(card: HubCard, matched: boolean, ourLoaderPid: number | null): Origin | null {
  if (card.state !== "running") return null;
  if (matched) return "pm";
  const procs = card.processes;
  if (procs.length > 0 && procs.every(isOurProcess)) return "ours";
  if (procs.length > 0 && procs.every((p) => normProcName(p.comm || p.name) === "elfldr")) {
    return ourLoaderPid !== null && procs.some((p) => p.pid === ourLoaderPid) ? "loader-ours" : "loader";
  }
  return "console";
}
