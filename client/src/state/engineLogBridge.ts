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
  // Keep a handle on the interval so HMR in dev can clean it up if this
  // module is re-evaluated. Without this, every Vite HMR would stack
  // another timer.
  (window as unknown as { __engineLogBridgeInterval?: number }).__engineLogBridgeInterval = interval as unknown as number;
}
