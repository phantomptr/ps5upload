// Engine sidecar liveness probe.
//
// Used by the status bar's engine dot — touches `/api/version` (served
// from in-memory state, no PS5 round-trip). The earlier `status` and
// `volumes` exports here were unused; the renderer goes through the
// Tauri IPC surface in `api/ps5.ts` for everything except this probe.

import { getEngineUrl } from "../state/engine";

export const engineApi = {
  /** Lightweight engine liveness probe. Unlike `/api/ps5/status` (which
   *  round-trips to the PS5 and fails if the console is off), this hits
   *  `/api/version` — no state read, responds immediately,
   *  touches no network. Used by the status bar's engine dot.
   *
   *  Wraps in a tight 2 s timeout: if the engine accept loop is
   *  wedged (rather than down), an unbounded `fetch` would hang the
   *  status indicator forever. The timeout converts a wedged engine
   *  into a clean "not alive" — same UX as the down case, which is
   *  what the caller expects from a probe. */
  async ping(): Promise<boolean> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 2000);
    try {
      // Not /api/jobs: that builds the whole job list, every 5 s per tab, for one bit of information.
      const res = await fetch(`${getEngineUrl()}/api/version`, {
        signal: controller.signal,
      });
      // Cancel the unused body stream: in Chromium/WebView2 on Windows, leaving a
      // fetch Response unconsumed retains its 2 MB Mojo DataPipe shared-memory
      // mapping off the V8 heap (~1.44 GB/hr at the 5 s probe cadence).
      await res.body?.cancel().catch(() => {});
      if (res.status !== 404) return res.ok;
      // An older self-hosted engine.
      const old = await fetch(`${getEngineUrl()}/api/jobs`, {
        signal: controller.signal,
      });
      await old.body?.cancel().catch(() => {});
      return old.ok;
    } catch {
      return false;
    } finally {
      clearTimeout(timer);
    }
  },
};
