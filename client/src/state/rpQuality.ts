import { create } from "zustand";

import type { RemotePlayStream } from "../lib/remotePlayStream";
import { useRemotePlayStore, type RpTarget, type SessionQuality } from "./remotePlay";

/** Sessions the lower quality was already offered for (once each, taken or not). */
const offered = new WeakSet<RemotePlayStream>();

export function wasOffered(s: RemotePlayStream): boolean {
  return offered.has(s);
}

/** How long a switch keeps trying while the PS5 still holds its last session, ms. A PS5 on
 *  FW 13.00 held it for minutes after a stop (#720); the engine spreads its tries out and ends
 *  the wait itself ("held") after about 5 minutes, so this is only the backstop. */
export const SWITCH_FOR_MS = 7 * 60_000;

/** What is under way: `quality`, a switch to 720p; `restart`, Play again with other settings;
 *  `wait`, a plain Start waiting for the PS5 to free its last session. */
export type SwitchKind = "quality" | "restart" | "wait";

interface QualityState {
  /** The session the 720p offer is showing for. */
  offerFor: RemotePlayStream | null;
  /** A restart in progress, and when the next try is (while the PS5 frees its slot). */
  switching: { kind: SwitchKind; retryAt: number | null } | null;
  offer: (s: RemotePlayStream) => void;
  dismiss: () => void;
  switchTo720: () => Promise<void>;
  restartWith: (quality: SessionQuality, kind?: SwitchKind) => Promise<void>;
  startWhenFree: (t: RpTarget) => Promise<void>;
  cancelSwitch: () => void;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Wait until `done()` (checked every 100 ms) or `ms` pass. */
async function until(done: () => boolean, ms: number): Promise<void> {
  for (const end = Date.now() + ms; !done() && Date.now() < end; ) await sleep(100);
}

export const useRpQualityStore = create<QualityState>((set, get) => {
  /** Start `t` and go on trying while the engine answers "busy, try in N s" (it does so without
   *  asking the console). */
  const startAndWait = async (t: RpTarget, kind: SwitchKind, quality?: SessionQuality) => {
    const deadline = Date.now() + SWITCH_FOR_MS;
    while (get().switching && Date.now() < deadline) {
      if (useRemotePlayStore.getState().stream) break; // started again by hand
      useRemotePlayStore.setState({ error: null });
      await useRemotePlayStore.getState().start(t, { quality });
      await until(() => {
        const s = useRemotePlayStore.getState();
        return s.phase === "playing" || !s.stream || !!s.error;
      }, 60_000);
      const err = useRemotePlayStore.getState().error;
      // After a stop of our own, "in use" is that session still leaving too.
      if (err?.code !== "busy" && !(kind !== "wait" && err?.code === "in_use")) break;
      // Not an error to show: the wait is the plan.
      const wait = Math.max(1, err.retryS ?? 5) * 1000 + 500;
      useRemotePlayStore.setState({ error: null });
      set({ switching: { kind, retryAt: Date.now() + wait } });
      await until(() => !get().switching, wait);
      await until(() => !useRemotePlayStore.getState().stream, 5_000);
    }
    set({ switching: null });
  };

  return {
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
      await get().restartWith({ res: "1280x720" }, "quality");
    },

    /** Stop the stream and start it again with `next` (720p, or H.264 at a site's bitrate for
     *  Go live), for this session only: Settings keep what the user chose, so one bad-network
     *  moment does not make every later stream 720p. Resolution, codec and bitrate are fixed for a
     *  session, and the PS5 keeps its one Remote Play slot for ~12 s after a session ends. Friends
     *  stay in: the engine keeps Play Together open through a host's restart. */
    async restartWith(next, kind = "restart") {
      const rp = useRemotePlayStore.getState();
      const t = rp.target;
      if (!t || !rp.stream || get().switching) return;
      set({ switching: { kind, retryAt: null } });
      // On top of an earlier restart of this session (720p, then Go live's H.264).
      // The picture on this device stays as it is now, when that is not what a new session gets.
      const { pictureHere: _earlier, ...earlier } = rp.sessionQuality ?? {};
      void _earlier;
      const quality = {
        ...earlier,
        ...next,
        ...(rp.pictureHere === rp.prefs.pictureHere ? {} : { pictureHere: rp.pictureHere }),
      };
      rp.stopForRestart();
      await until(() => !useRemotePlayStore.getState().stream, 10_000);
      await startAndWait(t, kind, quality);
    },

    /** A Start the engine answered "busy" (the error is still up): wait it out and start. */
    async startWhenFree(t) {
      if (get().switching) return;
      const retryS = useRemotePlayStore.getState().error?.retryS ?? 5;
      const wait = Math.max(1, retryS) * 1000 + 500;
      useRemotePlayStore.setState({ error: null });
      set({ switching: { kind: "wait", retryAt: Date.now() + wait } });
      await until(() => !get().switching, wait);
      await until(() => !useRemotePlayStore.getState().stream, 5_000);
      await startAndWait(t, "wait");
    },

    cancelSwitch() {
      set({ switching: null });
    },
  };
});
