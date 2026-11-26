// Game starts this app asked a console for and that have not answered yet.
//
// A helper sent in the middle of one restarts the helper under the launch (#903's precautions:
// nothing replaces the helper while the console is being asked to do something). The launch
// itself is a component's own state (lib/useGameLaunch), so the places that decide about a helper
// send had no way to see it.

import { hostOf } from "./addr";

const inFlight = new Map<string, number>();

const keyOf = (host: string) => hostOf(host.trim()).toLowerCase();

/** Marks a game start on `host` as running; call the returned function when it has ended. */
export function beginLaunch(host: string): () => void {
  const key = keyOf(host);
  if (!key) return () => {};
  inFlight.set(key, (inFlight.get(key) ?? 0) + 1);
  let ended = false;
  return () => {
    if (ended) return;
    ended = true;
    const n = (inFlight.get(key) ?? 1) - 1;
    if (n <= 0) inFlight.delete(key);
    else inFlight.set(key, n);
  };
}

/** Whether this app is starting a game on `host` right now. */
export function launchInFlight(host: string): boolean {
  return inFlight.has(keyOf(host));
}

/** Test seam. */
export function resetLaunchInFlight(): void {
  inFlight.clear();
}
