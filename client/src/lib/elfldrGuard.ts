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
import { runStatusForHost, usePayloadPlaylistsStore } from "../state/payloadPlaylists";

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

/** The other thing that looks exactly like a stuck elfldr from here: a jailbreak's own payload
 *  listener on :9021 (WebKit-only setups) reads an ELF until the sender closes and never
 *  replies, so the probe can't tell them apart. kstuff and ShadowMount+ run fine through such a
 *  listener; the helper needs elfldr (a mewx report, 2026-10-20: loading elfldr-ps5.elf first
 *  fixed it). Shown under the stuck message rather than guessed at. */
export const LISTENER_LOADER_HINT =
  "No elfldr loaded? A jailbreak's own payload listener on port 9021 behaves the same way. " +
  "Send elfldr-ps5.elf to it first (Payloads → elfldr), then send the helper.";

const lastAsked = new Map<string, number>();
const GUARD_INTERVAL_MS = 10 * 60_000;
/** How long the swap waits for a playlist on that console before leaving it for the next time. */
const PLAYLIST_WAIT_MS = 2 * 60_000;
const PLAYLIST_POLL_MS = 2_000;

function playlistRunsOn(host: string): boolean {
  const kind = runStatusForHost(usePayloadPlaylistsStore.getState(), host).kind;
  return kind === "running" || kind === "sleeping";
}

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
  deps: { ensure: (host: string) => Promise<unknown>; playlistRuns?: (host: string) => boolean } & Clock = {
    ensure: elfldrEnsure,
    ...realClock,
  },
): Promise<void> {
  const key = host.trim().toLowerCase();
  const last = lastAsked.get(key);
  if (!fresh && last !== undefined && deps.now() - last < GUARD_INTERVAL_MS) return;
  lastAsked.set(key, deps.now());
  // The swap closes :9021 for about a second, and the auto-loader starts on the same "helper came
  // up" edge. A step landing in that gap fails and drops the rest of the playlist; one the old
  // elfldr takes just before it is killed reports success and never runs.
  const playlistRuns = deps.playlistRuns ?? playlistRunsOn;
  const giveUpAt = deps.now() + PLAYLIST_WAIT_MS;
  while (playlistRuns(host)) {
    if (deps.now() >= giveUpAt) {
      lastAsked.delete(key);
      return;
    }
    await deps.sleep(PLAYLIST_POLL_MS);
  }
  try {
    const outcome = await deps.ensure(host);
    log.info("connection", `elfldr on ${host}: ${JSON.stringify(outcome)}`);
  } catch (e) {
    log.warn("connection", `elfldr check on ${host} failed: ${e instanceof Error ? e.message : String(e)}`);
  }
}
