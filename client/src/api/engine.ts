// Engine sidecar liveness probe.
//
// Used by the status bar's engine dot — touches `/api/jobs` (served
// from in-memory state, no PS5 round-trip). The earlier `status` and
// `volumes` exports here were unused; the renderer goes through the
// Tauri IPC surface in `api/ps5.ts` for everything except this probe.

const BASE = "http://127.0.0.1:19113";

export const engineApi = {
  /** Lightweight engine liveness probe. Unlike `/api/ps5/status` (which
   *  round-trips to the PS5 and fails if the console is off), this hits
   *  `/api/jobs` — served from in-memory state, responds immediately,
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
      const res = await fetch(`${BASE}/api/jobs`, { signal: controller.signal });
      return res.ok;
    } catch {
      return false;
    } finally {
      clearTimeout(timer);
    }
  },
};
