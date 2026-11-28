// "Friends' PS5s" in Play Together (apps/play/docs/EASY_JOIN.md, way 5): friends who joined from
// PS5Upload Play before, and the host's push of the current invite to their TV
// (engine join_friends.rs: GET /api/join/friends, POST /api/join/friends/send and /forget).

import { getEngineUrl } from "../../state/engine";

type Tr = (key: string, vars: Record<string, string | number> | undefined, fallback: string) => string;

export interface FriendPs5 {
  id: string;
  /** The name they gave ("" for none). */
  name: string;
  /** The console they played on. */
  console: string;
  madeUnix: number;
  lastUnix: number;
  /** Their app gave its waiting room: an invite can be sent to it. */
  canInvite: boolean;
}

/** The engine's list; [] for anything else (an engine from before it says 404). */
export function readFriends(v: unknown): FriendPs5[] {
  const list = (v as { friends?: unknown } | null)?.friends;
  if (!Array.isArray(list)) return [];
  const out: FriendPs5[] = [];
  for (const f of list as Record<string, unknown>[]) {
    if (!f || typeof f.id !== "string" || !f.id) continue;
    out.push({
      id: f.id,
      name: typeof f.name === "string" ? f.name.trim() : "",
      console: typeof f.console === "string" ? f.console : "",
      madeUnix: Number(f.made_unix) || 0,
      lastUnix: Number(f.last_unix) || 0,
      canInvite: f.can_invite === true,
    });
  }
  return out;
}

/** How a send went: the engine's answer by status and error. */
export type SendOutcome = "sent" | "not_open" | "not_playing" | "no_inbox" | "refused" | "unknown" | "hub";

export function sendOutcome(status: number, json: unknown): SendOutcome {
  const j = json as { ok?: unknown; error?: unknown } | null;
  if (status >= 200 && status < 300 && j?.ok === true) return "sent";
  const e = j?.error;
  if (status === 409 && (e === "not_open" || e === "not_playing" || e === "no_inbox" || e === "refused")) return e;
  if (status === 404 && e === "unknown") return "unknown";
  return "hub";
}

/** "Send invite" shows for a friend whose app can take one, while Play Together is open here, and
 *  not after the engine said it has no waiting room for them. */
export function canSend(f: FriendPs5, open: boolean, noInbox: ReadonlySet<string>): boolean {
  return open && f.canInvite && !noInbox.has(f.id);
}

/** The line under a friend after a send (null: nothing to say). */
export function outcomeText(tr: Tr, o: SendOutcome, name: string): string | null {
  switch (o) {
    case "sent":
      return tr("pt_friends_sent", { name }, `Sent. ${name} sees it on their TV.`);
    case "not_open":
      return tr("pt_friends_not_open", { name }, `PS5Upload Play isn't open on ${name}'s PS5 right now.`);
    case "not_playing":
      return tr("pt_friends_not_playing", undefined, "Start playing first, then send the invite.");
    case "refused":
      return tr("pt_friends_refused", { name }, `${name}'s PS5 didn't take the invite.`);
    case "hub":
      return tr("pt_friends_hub", undefined, "Couldn't send it. Check this computer's internet and try again.");
    default:
      // no_inbox hides the button; unknown: the list is read again.
      return null;
  }
}

async function call(path: string, body?: object): Promise<{ status: number; json: unknown }> {
  const r = await fetch(`${getEngineUrl()}${path}`, {
    method: body ? "POST" : "GET",
    headers: body ? { "Content-Type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  let json: unknown = {};
  try {
    json = await r.json();
  } catch {
    // No body.
  }
  return { status: r.status, json };
}

export const friendsApi = {
  list: async (): Promise<FriendPs5[]> => {
    try {
      const r = await call("/api/join/friends");
      return r.status === 200 ? readFriends(r.json) : [];
    } catch {
      return [];
    }
  },
  send: async (id: string, addr: string): Promise<SendOutcome> => {
    try {
      const r = await call("/api/join/friends/send", { id, addr });
      return sendOutcome(r.status, r.json);
    } catch {
      return "hub";
    }
  },
  forget: async (id: string): Promise<void> => {
    try {
      await call("/api/join/friends/forget", { id });
    } catch {
      // The list is read again either way.
    }
  },
};
