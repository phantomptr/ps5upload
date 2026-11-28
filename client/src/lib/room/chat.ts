// Play Rooms chat (design 7A.4): quick messages travel as ids 1–16 and show in each person's own
// language; typed lines are 1–140 characters, never clickable, links spelled out with "·". Lines
// live only in memory (the host engine keeps the last 50, each device what it was sent).

import { dropInvisible, looksLikeLink } from "./names";

/** Off, quick messages only, or quick + typed text. */
export type ChatMode = "off" | "quick" | "on";

export function readChatMode(v: unknown): ChatMode | null {
  // The guest push says "text" for typed text allowed.
  if (v === "text") return "on";
  return v === "off" || v === "quick" || v === "on" ? v : null;
}

/** The 16 quick messages, by id (the order of the design's list). */
export const QUICK_IDS = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16] as const;

type Tr = (key: string, vars: undefined, fallback: string) => string;

/** A quick message in this device's language; null for an id it doesn't know (a newer host). */
export function quickText(tr: Tr, id: number): string | null {
  switch (id) {
    case 1:
      return tr("room_quick_1", undefined, "Ready?");
    case 2:
      return tr("room_quick_2", undefined, "Ready!");
    case 3:
      return tr("room_quick_3", undefined, "GG");
    case 4:
      return tr("room_quick_4", undefined, "One more?");
    case 5:
      return tr("room_quick_5", undefined, "Brb");
    case 6:
      return tr("room_quick_6", undefined, "Wait");
    case 7:
      return tr("room_quick_7", undefined, "Let's go");
    case 8:
      return tr("room_quick_8", undefined, "Thanks");
    case 9:
      return tr("room_quick_9", undefined, "Nice!");
    case 10:
      return tr("room_quick_10", undefined, "Sorry");
    case 11:
      return tr("room_quick_11", undefined, "Pause please");
    case 12:
      return tr("room_quick_12", undefined, "Lag on my side");
    case 13:
      return tr("room_quick_13", undefined, "Can't hear you");
    case 14:
      return tr("room_quick_14", undefined, "Switch seats?");
    case 15:
      return tr("room_quick_15", undefined, "Bye");
    case 16:
      return "👍";
    default:
      return null;
  }
}

export const MAX_CHAT = 140;

/** A typed line as it is sent and shown: trimmed, at most 140 characters, links never clickable
 *  (their dots spelled "·"), invite codes dropped in a Public room. Empty: nothing to send. */
export function cleanChat(raw: string, opts: { publicRoom?: boolean } = {}): string {
  let s = dropInvisible(raw).replace(/\s+/g, " ").trim();
  if (opts.publicRoom) s = s.replace(/\b[A-Z0-9]{3}-?[A-Z0-9]{3}\b/g, (m) => (/\d/.test(m) && /[A-Z]/.test(m) ? "…" : m));
  if (looksLikeLink(s)) s = s.replace(/\./g, "·").replace(/:\/\//g, " ");
  return [...s].slice(0, MAX_CHAT).join("");
}

/** One chat line on this device. */
export interface ChatLine {
  /** Local order. */
  n: number;
  from: string;
  /** The sender is this device (sent here). */
  mine: boolean;
  quick: number | null;
  text: string | null;
  /** The same line came again: shown once with "×N". */
  times: number;
  at: number;
}

/** A chat message from the host engine (`{"chat": {from, quick | text}}`), or null. */
export function readChat(v: unknown): { from: string; quick: number | null; text: string | null } | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  const from = typeof o.from === "string" ? o.from.slice(0, 40) : "";
  const quick = typeof o.quick === "number" && Number.isInteger(o.quick) && o.quick >= 1 && o.quick <= 99 ? o.quick : null;
  const text = typeof o.text === "string" && o.text.trim() ? cleanChat(o.text) : null;
  if (quick == null && text == null) return null;
  return { from, quick, text };
}

/** Adds a line, folding a repeat of the last one from the same person, keeping the last 50. */
export function addLine(lines: readonly ChatLine[], line: Omit<ChatLine, "n" | "times">): ChatLine[] {
  const last = lines[lines.length - 1];
  if (last && last.from === line.from && last.mine === line.mine && last.quick === line.quick && last.text === line.text) {
    return [...lines.slice(0, -1), { ...last, times: last.times + 1, at: line.at }];
  }
  const n = (last?.n ?? 0) + 1;
  return [...lines, { ...line, n, times: 1 }].slice(-50);
}

/** The sending limits (one line per 2 s, ten a minute): the engine drops past them; the page
 *  waits instead and says "Slow down". */
export class ChatPace {
  private sent: number[] = [];
  constructor(private readonly now: () => number = () => Date.now()) {}
  /** True when a line may go now (and counts it). */
  take(): boolean {
    const t = this.now();
    this.sent = this.sent.filter((x) => t - x < 60_000);
    const last = this.sent[this.sent.length - 1];
    if ((last != null && t - last < 2000) || this.sent.length >= 10) return false;
    this.sent.push(t);
    return true;
  }
}

/** Unread lines: those after the last one seen, not sent here. */
export function unread(lines: readonly ChatLine[], seen: number): number {
  return lines.filter((l) => l.n > seen && !l.mine).reduce((a, l) => a + l.times, 0);
}

/** A small local "hide rude words" filter (on by default in Public): stars over a short list.
 *  The host engine and hub carry the real list; this only covers what a viewer sees. */
const RUDE = /\b(f+u+c+k+\w*|s+h+i+t+\w*|b+i+t+c+h+\w*|c+u+n+t+\w*|a+s+s+h+o+l+e+\w*|d+i+c+k+\w*|n+i+g+g+\w*|f+a+g+\w*|w+h+o+r+e+\w*|s+l+u+t+\w*|scheisse|scheiße|puta\w*|merde|putain|cazzo|блядь|сука)\b/giu;
export function hideRude(text: string): string {
  return text.replace(RUDE, (m) => "*".repeat([...m].length));
}
