/** True when an error is the app failing to reach its own engine, not the console. The
 *  desktop's command proxies say "error sending request for url (http://127.0.0.1:…)", which
 *  used to be reported as the PS5's helper not starting. */
export function isEngineUnreachable(error: string): boolean {
  return /error sending request for url \(https?:\/\/(127\.\d+\.\d+\.\d+|localhost|\[::1\])[:/]/i.test(
    error,
  );
}

/** Probes in a row that must miss before an engine that was answering is called down. One
 *  probe can miss its 2 s on a busy machine (a Remote Play stream decoding on Windows) with the
 *  engine fine: the banner then flashed "engine unreachable" and back every minute (#590). */
export const ENGINE_MISSES_FOR_DOWN = 2;

/** What one liveness probe means: the status to show, or null to keep the current one and
 *  probe again soon (a first miss after "up"). `misses` counts this probe. */
export function engineProbeVerdict(
  before: string,
  up: boolean,
  misses: number,
): "up" | "down" | null {
  if (up) return "up";
  if (before === "up" && misses < ENGINE_MISSES_FOR_DOWN) return null;
  return "down";
}
