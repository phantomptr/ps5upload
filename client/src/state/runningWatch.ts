import { useConnectionStore } from "./connection";
import { useRunningAppsStore } from "./runningApps";
import { fetchRunningGames } from "../lib/runningGames";
import { mgmtAddr } from "../lib/addr";
import { transferScreenBusy } from "../lib/ps5Transfers";

/**
 * The app-wide "is a game running?" watcher.
 *
 * `useRunningAppsStore` has always been the shared answer to that question,
 * but nothing filled it unless a specific screen was open: RunningAppsPanel
 * (inside Library) and the Games grid each ran their own poll loop. Leave
 * both screens and the store simply stopped being updated, which is fine
 * for a badge on a row you are looking at and useless for a badge on the
 * navigation itself — the whole point of which is to tell you something
 * while you are somewhere else.
 *
 * So this installs one low-frequency poll at the shell level.
 *
 * ## Backing off
 *
 * It deliberately does NOT poll whenever a screen-level loop is already
 * doing so. Rather than coordinate through a registry of "who is polling"
 * — which would have to be kept correct through every mount, unmount and
 * host switch — it reads the freshness the store already tracks:
 * `updatedAtMs`. If somebody published recently, there is nothing to do.
 * The screens poll faster than this loop, so while Games or Library is
 * open this one does nothing at all, and it resumes on its own when they
 * unmount. A missed unmount cannot leave a stale suppression, because the
 * only thing that suppresses it is data that keeps arriving.
 *
 * ## Load
 *
 * The console's mgmt port serves one client at a time, and stacking polls
 * onto it during a many-file upload collapsed effective throughput once
 * before (the exfat.ffpfsc case: 120 MB/s down to 10). This loop is
 * therefore slower than any screen loop, skips entirely while a transfer
 * to this console is in flight, and pauses when the window is hidden.
 * Read-only: a process list never touches a running game.
 */

/** Slower than any screen-level loop — this drives a badge, not a control. */
const POLL_MS = 10_000;

/** Treat the store as current if someone published within this window. A
 *  screen loop at 3s clears this comfortably; a screen that has just
 *  unmounted stops clearing it within one tick. */
const FRESH_MS = 15_000;

let installed = false;

export function installRunningWatch(): void {
  if (installed) return;
  installed = true;

  let inFlight = false;

  const tick = async () => {
    if (inFlight) return;
    // Nothing to poll, and nothing worth spending a request on.
    if (typeof document !== "undefined" && document.hidden) return;

    const host = useConnectionStore.getState().host;
    if (!host?.trim()) return;
    if (transferScreenBusy(host)) return;

    // Somebody with a faster loop is already keeping this current.
    const store = useRunningAppsStore.getState();
    if (store.host === host && Date.now() - store.updatedAtMs < FRESH_MS) {
      return;
    }

    inFlight = true;
    try {
      const running = await fetchRunningGames(mgmtAddr(host));
      // Re-read the host: a console switch during the request would
      // otherwise publish one PS5's running titles under another's name.
      if (useConnectionStore.getState().host === host) {
        useRunningAppsStore
          .getState()
          .setRunning(Array.from(running.keys()), host);
      }
    } catch {
      // A failed poll means "we don't know", not "nothing is running".
      // Leaving the last known set in place keeps the badge stable across
      // a blip instead of flickering off and back on.
    } finally {
      inFlight = false;
    }
  };

  void tick();
  setInterval(() => void tick(), POLL_MS);
}
