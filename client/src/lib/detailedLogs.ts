/*
 * "Detailed logs" (the Logs tab's switch): the app keeps when it turns off (state/diagSettings),
 * and this module carries it to the engine (`POST /api/diag/log-level`), which then writes its
 * extra debug lines: the Remote Play handshake (the console's answers, timings, retries) and
 * Play Together (guests connecting, asking, coming back, keyframe requests).
 *
 * The engine keeps the switch in memory and caps it at a day, so it is sent again on start and
 * every few minutes while it is on (an engine that restarted picks it up), and the app turns it
 * off itself when its time is up. Nobody stays verbose by accident.
 */

import { detailedActive, useDiagSettingsStore } from "../state/diagSettings";
import { getEngineUrl } from "../state/engine";
import { log } from "../state/logs";

/** While on, the engine is told again this often (it forgets on a restart). */
export const RESEND_MS = 5 * 60 * 1000;

/** Tell the engine. False when it could not be reached (it is told again later). */
export async function sendDetailedToEngine(
  until: number | null,
  fetchImpl: typeof fetch = fetch,
): Promise<boolean> {
  try {
    const r = await fetchImpl(`${getEngineUrl()}/api/diag/log-level`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(until === null ? { detailed: false } : { detailed: true, until_ms: until }),
      signal: AbortSignal.timeout(4000),
    });
    return r.ok;
  } catch {
    return false;
  }
}

let installed = false;

/** Start keeping the engine in step with the switch. Idempotent. */
export function installDetailedLogs(): () => void {
  if (installed) return () => {};
  installed = true;
  let offTimer: ReturnType<typeof setTimeout> | null = null;
  let resend: ReturnType<typeof setInterval> | null = null;

  const apply = (until: number | null, changed: boolean) => {
    if (offTimer) clearTimeout(offTimer);
    if (resend) clearInterval(resend);
    offTimer = null;
    resend = null;
    if (detailedActive(until)) {
      void sendDetailedToEngine(until);
      // Its time is up: off, here and in the engine.
      offTimer = setTimeout(() => useDiagSettingsStore.getState().setDetailed(false), until! - Date.now());
      resend = setInterval(() => void sendDetailedToEngine(until), RESEND_MS);
      if (changed)
        log.info("diag", `Detailed logs on until ${new Date(until!).toLocaleString()}`);
    } else if (changed) {
      // Only a change turns the engine off: at start-up another window (the web UI) may have
      // turned it on.
      void sendDetailedToEngine(null);
      log.info("diag", "Detailed logs off");
    }
  };

  apply(useDiagSettingsStore.getState().detailedUntil, false);
  const unsub = useDiagSettingsStore.subscribe((s, prev) => {
    if (s.detailedUntil !== prev.detailedUntil) apply(s.detailedUntil, true);
  });
  return () => {
    unsub();
    if (offTimer) clearTimeout(offTimer);
    if (resend) clearInterval(resend);
    installed = false;
  };
}
