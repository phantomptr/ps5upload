// The app's queue and the engine's own queue (/api/queue) take turns on a console. The app runs
// its queue as it always has; for as long as it works on a console it holds that console, so the
// engine's queue starts nothing there, and it waits while an engine queue item runs there.
//
// Anything but a clear "an item is running" lets the app go on: an older engine has no such route,
// and the app's queue must never stop because of this.

import { ENGINE_TIMEOUT_MS, engineFetch } from "../lib/engineFetch";
import { getEngineUrl } from "../state/engine";

/** The engine lets a hold lapse after this, so a closed app holds nothing for long. Past the once a
 *  minute a browser throttles a hidden tab's timers to, or a renewal could come too late. */
const HOLD_TTL_S = 150;
const RENEW_EVERY_MS = 10_000;
/** Shorter than the engine's pause between items, so the app gets in between two of them. */
const ASK_AGAIN_MS = 2_000;

function newHolder(): string {
  // crypto.randomUUID needs a secure context, which the web UI over a LAN address is not.
  return `app-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

async function post(path: string, body: unknown): Promise<Response> {
  return engineFetch(`${getEngineUrl()}${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
    timeoutMs: ENGINE_TIMEOUT_MS,
  });
}

/** False only when the engine says one of its queue's items runs on this console. */
async function askForHold(host: string, holder: string): Promise<boolean> {
  try {
    const res = await post("/api/queue/hold", {
      console: host,
      holder,
      ttl_s: HOLD_TTL_S,
    });
    if (res.status !== 409) return true;
    const body = (await res.json().catch(() => null)) as { code?: string } | null;
    return body?.code !== "queue_running";
  } catch {
    return true;
  }
}

export interface EngineQueueTurn {
  /** Still wanted: false ends the wait. */
  isLive: () => boolean;
  /** The app starts waiting (true), or stops waiting (false). Not called if there was no wait. */
  onWaiting: (waiting: boolean) => void;
}

/** Waits for the app's turn on a console, then holds it. Call the answer to give the console back;
 *  it is safe to call when the wait ended because `isLive` went false. */
export async function holdConsoleFromEngineQueue(
  host: string,
  turn: EngineQueueTurn,
): Promise<() => void> {
  const holder = newHolder();
  let waited = false;
  try {
    while (!(await askForHold(host, holder))) {
      if (!waited) {
        waited = true;
        turn.onWaiting(true);
      }
      await new Promise((resolve) => setTimeout(resolve, ASK_AGAIN_MS));
      if (!turn.isLive()) return () => undefined;
    }
  } finally {
    if (waited) turn.onWaiting(false);
  }
  const hold: Held = { holder, turn, askedAt: Date.now(), lost: false };
  held.set(host, hold);
  const renew = setInterval(() => void renewHold(host, hold), RENEW_EVERY_MS);
  return () => {
    clearInterval(renew);
    if (held.get(host) === hold) held.delete(host);
    void post("/api/queue/release", { console: host, holder }).catch(() => undefined);
  };
}

interface Held {
  holder: string;
  turn: EngineQueueTurn;
  askedAt: number;
  /** The engine's queue started something here: it lapsed, and must be taken again. */
  lost: boolean;
}

const held = new Map<string, Held>();

async function renewHold(host: string, hold: Held): Promise<void> {
  hold.askedAt = Date.now();
  const kept = await askForHold(host, hold.holder);
  if (!kept) hold.lost = true;
}

/** An upload made progress: renews a hold that is due. A hidden tab's timers can run late, and
 *  this path runs whenever the work does. Cheap when none is due. */
export function keepEngineQueueHold(host: string): void {
  const hold = held.get(host);
  if (hold && Date.now() - hold.askedAt >= RENEW_EVERY_MS) void renewHold(host, hold);
}

/** Before a job starts: a hold that lapsed is taken again first, waiting out what the engine's
 *  queue started meanwhile. Does nothing when the hold is known to be good. */
export async function retakeEngineQueueHold(host: string): Promise<void> {
  const hold = held.get(host);
  if (!hold || (!hold.lost && Date.now() - hold.askedAt < RENEW_EVERY_MS)) return;
  let waited = false;
  try {
    hold.askedAt = Date.now();
    while (!(await askForHold(host, hold.holder))) {
      if (!waited) {
        waited = true;
        hold.turn.onWaiting(true);
      }
      await new Promise((resolve) => setTimeout(resolve, ASK_AGAIN_MS));
      if (!hold.turn.isLive()) return;
    }
    hold.lost = false;
  } finally {
    if (waited) hold.turn.onWaiting(false);
  }
}
