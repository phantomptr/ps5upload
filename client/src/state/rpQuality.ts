import { create } from "zustand";

import type { RemotePlayStream } from "../lib/remotePlayStream";
import { useRemotePlayStore, type RpPrefs } from "./remotePlay";

/** Sessions the lower quality was already offered for (once each, taken or not). */
const offered = new WeakSet<RemotePlayStream>();

export function wasOffered(s: RemotePlayStream): boolean {
  return offered.has(s);
}

/** How long a switch keeps trying while the PS5 still holds its last session, ms. */
const SWITCH_FOR_MS = 90_000;

interface QualityState {
  /** The session the 720p offer is showing for. */
  offerFor: RemotePlayStream | null;
  /** A switch to 720p in progress, and when the next try is (while the PS5 frees its slot). */
  switching: { retryAt: number | null } | null;
  offer: (s: RemotePlayStream) => void;
  dismiss: () => void;
  switchTo720: () => Promise<void>;
  restartWith: (prefs: Partial<RpPrefs>) => Promise<void>;
  cancelSwitch: () => void;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Wait until `done()` (checked every 100 ms) or `ms` pass. */
async function until(done: () => boolean, ms: number): Promise<void> {
  for (const end = Date.now() + ms; !done() && Date.now() < end; ) await sleep(100);
}

export const useRpQualityStore = create<QualityState>((set, get) => ({
  offerFor: null,
  switching: null,

  offer(s) {
    if (offered.has(s)) return;
    offered.add(s);
    set({ offerFor: s });
  },

  dismiss() {
    set({ offerFor: null });
  },

  /** Stop the stream and start it again at 720p (see restartWith). */
  async switchTo720() {
    if (!useRemotePlayStore.getState().stream || get().switching) return;
    set({ offerFor: null });
    await get().restartWith({ res: "1280x720" });
  },

  /** Stop the stream and start it again with `prefs` (720p, or H.264 at a site's bitrate for
   *  Go live). Resolution, codec and bitrate are fixed for a session, and the PS5 keeps its one
   *  Remote Play slot for ~12 s after a session ends: the engine answers "busy, try in N s"
   *  without asking the console, and this waits that long and tries again. */
  async restartWith(prefs) {
    const rp = useRemotePlayStore.getState();
    const t = rp.target;
    if (!t || !rp.stream || get().switching) return;
    set({ switching: { retryAt: null } });
    rp.setPrefs(prefs);
    rp.stop();
    await until(() => !useRemotePlayStore.getState().stream, 10_000);
    const deadline = Date.now() + SWITCH_FOR_MS;
    while (get().switching && Date.now() < deadline) {
      if (useRemotePlayStore.getState().stream) break; // started again by hand
      useRemotePlayStore.setState({ error: null });
      await useRemotePlayStore.getState().start(t);
      await until(() => {
        const s = useRemotePlayStore.getState();
        return s.phase === "playing" || !s.stream || !!s.error;
      }, 60_000);
      const err = useRemotePlayStore.getState().error;
      if (err?.code !== "busy") break;
      // Not an error to show: the wait is the plan.
      const wait = Math.max(1, err.retryS ?? 5) * 1000 + 500;
      useRemotePlayStore.setState({ error: null });
      set({ switching: { retryAt: Date.now() + wait } });
      await until(() => !get().switching, wait);
      await until(() => !useRemotePlayStore.getState().stream, 5_000);
    }
    set({ switching: null });
  },

  cancelSwitch() {
    set({ switching: null });
  },
}));
