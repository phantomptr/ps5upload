// The console's loader (elfldr on :9021) and the two things the app does about it.
//
// Stock elfldr serves one connection at a time and waited forever on a client that went
// silent (a dropped link, a sleep or rest mode mid-send): still running, never answering, and
// users had to load it again. The engine carries a patched build with a 15 s read deadline.
//
//   waitForLoader — before sending the helper, don't send into a stuck loader: wait out the
//                   patched build's deadline, and report a stock one that stays stuck.
//   guardElfldr   — once the helper is up, have the engine swap the stock elfldr for the
//                   patched one (it leaves any other loader alone).

import { elfldrEnsure, elfldrHealth } from "../api/ps5";
import { log } from "../state/logs";

export type LoaderHealth = "healthy" | "stuck" | "absent";

interface Clock {
  now: () => number;
  sleep: (ms: number) => Promise<void>;
}

const realClock: Clock = {
  now: () => Date.now(),
  sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
};

/** How long to wait on a stuck loader: past the patched build's 15 s deadline. */
const STUCK_WAIT_MS = 20_000;
const STUCK_POLL_MS = 5_000;

/** The loader's state, waiting up to 20 s for a stuck one to recover. "unknown" when the
 *  check itself failed (an older engine): the caller sends as it always has. */
export async function waitForLoader(
  host: string,
  deps: { health: (host: string) => Promise<LoaderHealth> } & Clock = { health: elfldrHealth, ...realClock },
): Promise<LoaderHealth | "unknown"> {
  const until = deps.now() + STUCK_WAIT_MS;
  try {
    for (;;) {
      const h = await deps.health(host);
      if (h !== "stuck" || deps.now() >= until) return h;
      await deps.sleep(STUCK_POLL_MS);
    }
  } catch {
    return "unknown";
  }
}

/** What to tell someone whose loader stayed stuck. */
export const STUCK_LOADER_MESSAGE =
  "The PS5's elfldr (port 9021) is stuck: it accepts connections but never answers, which the " +
  "stock elfldr does after a connection drops mid-send. Load elfldr again (or restart the " +
  "console); ps5upload then replaces it with a build that recovers by itself.";

const lastAsked = new Map<string, number>();
const GUARD_INTERVAL_MS = 10 * 60_000;

/** Test hook. */
export function resetElfldrGuard(): void {
  lastAsked.clear();
}

/** Have the engine put the patched elfldr in place on `host`: at most once per console every
 *  10 minutes, or at once after a fresh helper send (`fresh`), which usually follows a reboot or
 *  a wake that brought the stock one back. Never throws. */
export async function guardElfldr(
  host: string,
  fresh: boolean,
  deps: { ensure: (host: string) => Promise<unknown> } & Clock = { ensure: elfldrEnsure, ...realClock },
): Promise<void> {
  const key = host.trim().toLowerCase();
  const last = lastAsked.get(key);
  if (!fresh && last !== undefined && deps.now() - last < GUARD_INTERVAL_MS) return;
  lastAsked.set(key, deps.now());
  try {
    const outcome = await deps.ensure(host);
    log.info("connection", `elfldr on ${host}: ${JSON.stringify(outcome)}`);
  } catch (e) {
    log.warn("connection", `elfldr check on ${host} failed: ${e instanceof Error ? e.message : String(e)}`);
  }
}
