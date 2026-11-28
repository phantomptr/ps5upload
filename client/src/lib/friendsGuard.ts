// The engine's "friends playing" guard (engine together_guard.rs): while friends play through a
// Play Room, a request that would end their game (stop the stream, rest mode, restart, close or
// launch a game, replace the helper, update) is answered 409 `friends_playing` with their names.
// Every such call in the app goes through `fetch`, so one wrapper asks the person once and sends
// the same request again with `X-Confirm-Friends: 1`; "Keep playing" gives the 409 back to the
// caller, which shows its usual failure. A question the app already asked (the Stop button's own
// "Stopping Play ends Play Together…") counts for a few seconds (`friendsConfirmed`).

import { create } from "zustand";

export interface FriendsAsk {
  friends: string[];
  console: string | null;
  resolve: (ok: boolean) => void;
}

export const useFriendsGuard = create<{ ask: FriendsAsk | null }>(() => ({ ask: null }));

let confirmedUntil = 0;

/** The person just said yes to ending friends' game in the app's own question: the next guarded
 *  calls go through without asking again. */
export function friendsConfirmed(forMs = 5000): void {
  confirmedUntil = Date.now() + forMs;
}

/** The person said yes to ending friends' game a moment ago (the Remote Play socket's stop then
 *  carries `confirm_friends`). */
export function friendsRecentlyConfirmed(): boolean {
  return Date.now() < confirmedUntil;
}

/** The 409's names, or null when it isn't the guard's. */
export function readFriendsPlaying(status: number, body: unknown): { friends: string[]; console: string | null } | null {
  const b = body as { error?: unknown; code?: unknown; friends?: unknown; console?: unknown } | null;
  if (status !== 409 || !b || (b.error !== "friends_playing" && b.code !== "friends_playing")) return null;
  return {
    friends: Array.isArray(b.friends) ? b.friends.filter((f): f is string => typeof f === "string").slice(0, 8) : [],
    console: typeof b.console === "string" ? b.console : null,
  };
}

/** Asks through the dialog (FriendsGuardDialog in the app shell). */
function ask(friends: string[], console: string | null): Promise<boolean> {
  const prev = useFriendsGuard.getState().ask;
  prev?.resolve(false);
  return new Promise((resolve) => useFriendsGuard.setState({ ask: { friends, console, resolve } }));
}

/** A request that can be sent again (its body a string or nothing). */
function replayable(input: RequestInfo | URL, init?: RequestInit): boolean {
  if (input instanceof Request) return false;
  const b = init?.body;
  return b == null || typeof b === "string";
}

/** Wraps `fetch` for requests to `isEngine(url)`. Installed once at start. */
export function installFriendsGuard(isEngine: (url: string) => boolean, w: { fetch: typeof fetch } = globalThis): void {
  const inner = w.fetch.bind(w);
  if ((w.fetch as typeof fetch & { __friendsGuard?: true }).__friendsGuard) return;
  const wrapped = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    if (!isEngine(url) || !replayable(input, init)) return inner(input, init);
    const withConfirm = (): RequestInit => ({ ...init, headers: { ...headersOf(init?.headers), "X-Confirm-Friends": "1" } });
    if (Date.now() < confirmedUntil) return inner(input, withConfirm());
    const r = await inner(input, init);
    if (r.status !== 409) return r;
    let body: unknown;
    try {
      body = await r.clone().json();
    } catch {
      return r;
    }
    const fp = readFriendsPlaying(r.status, body);
    if (!fp) return r;
    if (!(await ask(fp.friends, fp.console))) return r;
    friendsConfirmed();
    return inner(input, withConfirm());
  }) as typeof fetch & { __friendsGuard?: true };
  wrapped.__friendsGuard = true;
  w.fetch = wrapped;
}

function headersOf(h: HeadersInit | undefined): Record<string, string> {
  if (!h) return {};
  if (h instanceof Headers) return Object.fromEntries(h.entries());
  if (Array.isArray(h)) return Object.fromEntries(h);
  return { ...h };
}

/** Who plays through a Play Room now, as `GET /api/engine/update` says it (`friends_playing`:
 *  `[{console, friends}]`): every friend's name and the first console. */
export function readFriendsNow(body: unknown): { friends: string[]; console: string | null } {
  const list = (body as { friends_playing?: unknown } | null)?.friends_playing;
  const friends: string[] = [];
  let console: string | null = null;
  for (const e of Array.isArray(list) ? list : []) {
    const o = (e && typeof e === "object" ? e : {}) as { console?: unknown; friends?: unknown };
    const names = Array.isArray(o.friends) ? o.friends.filter((f): f is string => typeof f === "string") : [];
    if (!names.length) continue;
    if (console === null && typeof o.console === "string") console = o.console;
    friends.push(...names);
  }
  return { friends: friends.slice(0, 8), console };
}

/** Before something that ends friends' game without asking the engine (installing an update
 *  quits the app, and with it their Play Room): asks the person when friends play now. True: go
 *  ahead (nobody plays, they said yes, or the engine couldn't be asked). */
export async function okToEndFriendsGame(engineUrl: string, f: typeof fetch = globalThis.fetch): Promise<boolean> {
  if (Date.now() < confirmedUntil) return true;
  let body: unknown;
  try {
    const r = await f(`${engineUrl}/api/engine/update`, { signal: AbortSignal.timeout(5000) });
    if (!r.ok) return true;
    body = await r.json();
  } catch {
    return true;
  }
  const now = readFriendsNow(body);
  if (!now.friends.length) return true;
  const yes = await ask(now.friends, now.console);
  if (yes) friendsConfirmed();
  return yes;
}
