/*
 * Engine log bridge.
 *
 * The ps5upload-engine sidecar runs in its own process. Its stdout/stderr
 * is piped into the Tauri main process's stderr, which developers see
 * during `tauri dev` but packaged-app users can't. The engine also keeps
 * an in-memory log ring and exposes `GET /api/engine-logs?since=<seq>`;
 * this module polls that endpoint and mirrors new entries into the
 * renderer's Log tab store so they surface alongside frontend events.
 *
 * Polling interval is 1 s — small enough that reconcile per-parent
 * progress feels live, large enough that an idle engine doesn't burn
 * CPU. On any error (engine not up yet, command missing in dev mode)
 * we silently retry; there's no point surfacing the plumbing itself.
 */

import { engineLogsTail, type EngineLogEntry } from "../api/ps5";
import { log } from "./logs";

const POLL_INTERVAL_MS = 1000;

let running = false;
let nextSince = 0;

function routeToLogStore(entry: EngineLogEntry) {
  // Strip the `[engine:<level>]` prefix the sidecar's stderr tagging
  // adds — we're already rendering the level as a badge in the Log UI,
  // so duplicating it in the message text is noise.
  const msg = entry.msg.replace(/^\[engine:[a-z]+\]\s*/, "");
  switch (entry.level) {
    case "error":
      log.error("engine", msg);
      break;
    case "warn":
      log.warn("engine", msg);
      break;
    case "debug":
      log.debug("engine", msg);
      break;
    default:
      log.info("engine", msg);
      break;
  }
}

async function tick() {
  // Skip when the window is hidden — log catch-up will happen on
  // the first tick after the window becomes visible again, since
  // `nextSince` advances only on success. Without this, a minimized
  // ps5upload still hits the engine HTTP endpoint every second for
  // hours, burning IPC and battery for no UI benefit. The Page
  // Visibility API works in Tauri webview the same as in browsers.
  if (typeof document !== "undefined" && document.visibilityState === "hidden") {
    return;
  }
  try {
    const res = await engineLogsTail(nextSince);
    for (const e of res.entries) routeToLogStore(e);
    if (res.entries.length > 0) {
      // Advance to the last seq we saw. Guard: `next_seq` is the server's
      // claim, but use the entry-derived value so a buggy server can't
      // make us skip a window.
      const maxSeq = res.entries[res.entries.length - 1].seq;
      nextSince = Math.max(nextSince, maxSeq);
    } else if (typeof res.next_seq === "number" && res.next_seq > nextSince) {
      // No new entries but the server bumped its cursor (e.g. ring wrap) —
      // accept it so we don't re-request the already-dropped range.
      nextSince = res.next_seq;
    }
  } catch {
    // Engine not up yet, Tauri command missing in dev, network blip —
    // all fine, retry next tick.
  }
}

/** Start the poller. Idempotent: calling twice is a no-op. */
export function installEngineLogBridge() {
  if (running) return;
  running = true;
  const interval = setInterval(tick, POLL_INTERVAL_MS);

  // Vite HMR cleanup: when this module is re-evaluated (file save in
  // dev), `import.meta.hot.dispose` runs BEFORE the new module
  // evaluates. Clearing the old interval here prevents timer
  // accumulation across saves. Without this, each save in dev
  // doubles the engine-log poll rate, which over a long dev session
  // would multi-count log entries (each timer's `tick()` advances
  // the SAME `nextSince` cursor by the entries it sees).
  //
  // In production builds `import.meta.hot` is undefined and the
  // dispose call is a typeof-guarded no-op — the interval runs for
  // the lifetime of the renderer page (which exits when the user
  // closes the desktop window).
  //
  // Cast through unknown so we don't need `vite/client` types in the
  // tsconfig — this single reference is the only place we touch
  // Vite's HMR API.
  const meta = import.meta as unknown as {
    hot?: { dispose: (cb: () => void) => void };
  };
  if (meta.hot) {
    meta.hot.dispose(() => {
      clearInterval(interval);
      running = false;
    });
  }
}
