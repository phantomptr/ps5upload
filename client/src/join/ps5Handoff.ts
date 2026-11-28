// "Play on my PS5" (apps/play/docs/EASY_JOIN.md, way 4): the phone hands an invite link to
// PS5Upload Play on the friend's own PS5. The app's TV shows a QR code (`#ps5=<room>&t=<nonce>&hub=…`,
// a room it hosts on the hub); this page joins that room for a moment, signals the link and
// leaves. Either order works: the page keeps the last invite opened on this phone for 30 minutes
// and a scanned PS5 for 10.

import { parseJoinLink, parsePs5Fragment, type Ps5Target } from "../lib/joinProtocol";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { hubSocketUrl } from "./rtcTransport";

/** The hub the app's QR code names unless it was set up with another one. */
export const DEFAULT_HUB = "hub.phantomptr.com";
/** The website: its storage holds the scanned PS5 (the QR code opens this page there). */
export const SITE = "https://ps5upload.phantomptr.com";
export const TARGET_KEY = "ps5upload.join.ps5";
export const INVITE_KEY = "ps5upload.join.last";
/** A scanned PS5 is used this long. */
export const TARGET_FRESH_MS = 10 * 60_000;
/** An invite opened on this phone is offered this long (an invite's own life). */
export const INVITE_FRESH_MS = 30 * 60_000;
/** How long the hub and then the app get to answer. */
export const ANSWER_MS = 10_000;

export interface SavedInvite {
  link: string;
  name: string | null;
}

function load(key: string): Record<string, unknown> | null {
  try {
    const v: unknown = JSON.parse(safeGetItem(key) ?? "null");
    return v && typeof v === "object" ? (v as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

const fresh = (v: Record<string, unknown> | null, ms: number, now: number) =>
  !!v && typeof v.at === "number" && now - v.at >= 0 && now - v.at < ms;

/** Remembers the PS5 a QR code named. */
export function saveTarget(t: Ps5Target, now = Date.now()): void {
  safeSetItem(TARGET_KEY, JSON.stringify({ ...t, at: now }));
}

/** The PS5 scanned in the last 10 minutes, or null. */
export function freshTarget(now = Date.now()): Ps5Target | null {
  const v = load(TARGET_KEY);
  if (!fresh(v, TARGET_FRESH_MS, now)) return null;
  return parsePs5Fragment(new URLSearchParams({ ps5: String(v!.room), t: String(v!.t), hub: String(v!.hub) }).toString());
}

/** Remembers an invite link opened on this phone; null when it is not one. */
export function saveInvite(link: string, now = Date.now()): SavedInvite | null {
  const j = parseJoinLink(link);
  if (!j?.invite) return null;
  const inv = { link: link.trim(), name: j.name };
  safeSetItem(INVITE_KEY, JSON.stringify({ ...inv, at: now }));
  return inv;
}

/** The invite opened on this phone in the last 30 minutes, or null. */
export function freshInvite(now = Date.now()): SavedInvite | null {
  const v = load(INVITE_KEY);
  if (!fresh(v, INVITE_FRESH_MS, now) || typeof v!.link !== "string" || !parseJoinLink(v!.link)?.invite) return null;
  return { link: v!.link, name: typeof v!.name === "string" && v!.name ? v!.name : null };
}

/** Only the default hub, or the invite's own: a made-up QR code must not get links sent to
 *  any server it names. */
export function hubAllowed(target: Ps5Target, link: string): boolean {
  const hub = target.hub.toLowerCase();
  return hub === DEFAULT_HUB || hub === parseJoinLink(link)?.net?.hub.toLowerCase();
}

/** `hint`: no PS5 to send to (or a hub it may not use): scan the TV's code. */
export type HandoffResult = "ok" | "expired" | "refused" | "unreachable" | "hint";

/** Sends an invite to the PS5's room on the hub: join, one signal, the app's answer, leave. */
export function sendToPs5(
  target: Ps5Target | null,
  invite: SavedInvite,
  o: { WebSocketImpl?: typeof WebSocket; answerMs?: number } = {},
): Promise<HandoffResult> {
  const WS = o.WebSocketImpl ?? (typeof WebSocket === "undefined" ? undefined : WebSocket);
  if (!target || !hubAllowed(target, invite.link)) return Promise.resolve("hint");
  if (!WS) return Promise.resolve("unreachable");
  return new Promise((resolve) => {
    let ws: WebSocket | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const end = (r: HandoffResult) => {
      clearTimeout(timer);
      if (ws) {
        const s = ws;
        s.onopen = s.onmessage = s.onclose = s.onerror = null;
        try {
          if (s.readyState === 1) s.send(JSON.stringify({ t: "leave" }));
          s.close(1000);
        } catch {
          // Already closing.
        }
      }
      resolve(r);
    };
    const wait = () => {
      clearTimeout(timer);
      timer = setTimeout(() => end("unreachable"), o.answerMs ?? ANSWER_MS);
    };
    try {
      ws = new WS(hubSocketUrl(target.hub));
    } catch {
      end("unreachable");
      return;
    }
    const s = ws;
    wait();
    s.onopen = () => s.send(JSON.stringify({ v: 1, t: "join", room: target.room }));
    s.onmessage = (e: MessageEvent) => {
      let m: { t?: string; why?: string; from?: string; data?: { invite_ok?: unknown } };
      try {
        m = JSON.parse(typeof e.data === "string" ? e.data : "");
      } catch {
        return;
      }
      if (m.t === "joined") {
        const data = { invite: { link: invite.link, ...(invite.name ? { name: invite.name } : {}), t: target.t } };
        s.send(JSON.stringify({ v: 1, t: "signal", to: "host", data }));
        wait();
      } else if (m.t === "error") end(m.why === "no_room" ? "expired" : "unreachable");
      else if (m.t === "closed") end("unreachable");
      else if (m.t === "signal" && m.from === "host" && typeof m.data?.invite_ok === "boolean") end(m.data.invite_ok ? "ok" : "refused");
    };
    s.onclose = () => end("unreachable");
  });
}
