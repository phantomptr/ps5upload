/**
 * The bug report's timeline: the engine's journal and the app's merged into one list, oldest
 * first (spec §1, §3.1). Each record keeps its `src`, so a clock difference between the engine's
 * machine and this one stays visible instead of being smoothed over.
 */
import { getEngineUrl } from "../state/engine";
import { readAppEvents, appJournalDropped } from "./appJournal";
import type { EventCat, EventRecord } from "./eventRecord";

export interface Timeline {
  events: EventRecord[];
  engineDropped: number;
  appDropped: number;
  /** Why the engine's journal could not be read; null when it was. */
  engineError: string | null;
}

/** Engine records first, then the app's, sorted by time; a stable sort keeps each source's order. */
export function mergeEvents(engine: EventRecord[], app: EventRecord[]): EventRecord[] {
  return [...engine, ...app].sort((a, b) => a.ts - b.ts);
}

export function filterCats(events: EventRecord[], cats: ReadonlySet<EventCat>): EventRecord[] {
  return events.filter((e) => cats.has(e.cat));
}

/** The last `n` warnings and errors, newest first: step 1's "Recent problems we noticed". */
export function recentProblems(events: EventRecord[], n = 10): EventRecord[] {
  return events
    .filter((e) => e.level !== "info")
    .slice(-n)
    .reverse();
}

function parts(ts: number, tz?: string) {
  const f = new Intl.DateTimeFormat("en-CA", {
    timeZone: tz,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hourCycle: "h23",
  });
  const p = Object.fromEntries(f.formatToParts(new Date(ts)).map((x) => [x.type, x.value]));
  return { date: `${p.year}-${p.month}-${p.day}`, time: `${p.hour}:${p.minute}:${p.second}` };
}

/** One readable line: `2026-10-08 18:27:14 [engine] 192.168.86.100  connection WARN conn_lost: msg (×37 until 18:33:02)`. */
export function formatLine(e: EventRecord, tz?: string): string {
  const { date, time } = parts(e.ts, tz);
  const who = e.console ? ` ${e.console} ` : "";
  const code = e.code ? ` ${e.code}:` : "";
  const folded = e.count && e.count > 1 ? ` (×${e.count} until ${parts(e.last_ts ?? e.ts, tz).time})` : "";
  return `${date} ${time} [${e.src}]${who} ${e.cat} ${e.level.toUpperCase()}${code} ${e.msg}${folded}`;
}

/** Both journals for `since..until`. The engine read is bounded to 5 s; a failure is reported, not thrown. */
export async function fetchTimeline(since: number, until: number): Promise<Timeline> {
  let engine: EventRecord[] = [];
  let engineDropped = 0;
  let engineError: string | null = null;
  try {
    // No end time: the engine's clock may run ahead of this one (a Docker host), and cutting at
    // this machine's "now" would drop the engine's newest events, the ones that matter most.
    const r = await fetch(`${getEngineUrl()}/api/event-journal?since=${since}`, {
      signal: AbortSignal.timeout(5000),
    });
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    const body = (await r.json()) as { events?: EventRecord[]; dropped?: number };
    engine = body.events ?? [];
    engineDropped = body.dropped ?? 0;
  } catch (e) {
    engineError = e instanceof Error ? e.message : String(e);
  }
  const app = await readAppEvents(since, until).catch(() => [] as EventRecord[]);
  return { events: mergeEvents(engine, app), engineDropped, appDropped: appJournalDropped(), engineError };
}
