import { create } from "zustand";

import { payloadPlaylistsLoad, payloadPlaylistsSave, sendPayload } from "../api/ps5";
import {
  appendStep,
  moveStepDown,
  moveStepUp,
  patchPlaylist,
  removePlaylist,
  removeStep,
  type Playlist,
  type PlaylistRunStatus,
  type PlaylistStep,
} from "../lib/playlistOps";

/**
 * Persisted payload-playlist store.
 *
 * A playlist is a named ordered list of (payload_path, sleep_after_ms)
 * steps the user can replay against any reachable PS5. Useful for
 * scripted boot sequences — e.g. send GoldHEN, sleep 5s for it to
 * settle, send a debug payload, sleep 2s, send the launcher.
 *
 * Whole-document persistence: the entire playlist set is saved to
 * `payload_playlists.json` after every mutation (debounced 300 ms).
 * A single-document store keeps editing snappy and import/export a
 * one-file affair; per-playlist sharding would only matter if users
 * accumulated thousands of playlists, which they won't.
 *
 * The runner uses generation-counted cancellation (same pattern as
 * uploadQueue): every `start()` bumps `runId`; every await re-checks
 * before mutating state. `stop()` just bumps the counter.
 */

interface PlaylistDocument {
  playlists: Playlist[];
}

interface PlaylistState {
  playlists: Playlist[];
  loaded: boolean;
  /** Live status of the currently-running playlist. The SendPayload
   *  screen renders a banner from this so the user always knows
   *  whether the runner is mid-step, sleeping, or settled. */
  runStatus: PlaylistRunStatus;

  hydrate: () => Promise<void>;

  // CRUD on playlists
  createPlaylist: (name: string) => string;
  renamePlaylist: (id: string, name: string) => void;
  deletePlaylist: (id: string) => void;
  setContinueOnFailure: (id: string, b: boolean) => void;

  // CRUD on steps within a playlist
  addStep: (id: string, step: PlaylistStep) => void;
  removeStep: (id: string, index: number) => void;
  moveStepUp: (id: string, index: number) => void;
  moveStepDown: (id: string, index: number) => void;
  updateStep: (id: string, index: number, patch: Partial<PlaylistStep>) => void;

  // Run control
  run: (id: string, host: string, port: number) => Promise<void>;
  stop: () => void;
}

const SAVE_DEBOUNCE_MS = 300;

function newId(): string {
  return crypto.randomUUID().replace(/-/g, "");
}

export const usePayloadPlaylistsStore = create<PlaylistState>((set, get) => {
  let runId = 0;
  // Active abort controller for the runner's sleep step. Bumped on
  // every `run()` so a stale stop() can't cancel a fresh run; cleared
  // when the runner exits the sleep cleanly. `stop()` aborts it so
  // `cancellableSleep` resolves immediately and the runner notices
  // the runId change at its next isLive() check.
  let runAbort: AbortController | null = null;
  let saveTimer: ReturnType<typeof setTimeout> | null = null;

  const scheduleSave = () => {
    if (saveTimer !== null) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      saveTimer = null;
      const { playlists } = get();
      const doc: PlaylistDocument = { playlists };
      void payloadPlaylistsSave(doc).catch((e) => {
        // Same rationale as upload-queue: log so it lands in the
        // dev console + Windows engine.log; a UI toast is future
        // work but would need throttling.
        console.error("[payload-playlists] save failed:", e);
      });
    }, SAVE_DEBOUNCE_MS);
  };

  return {
    playlists: [],
    loaded: false,
    runStatus: { kind: "idle" },

    async hydrate() {
      try {
        const doc = await payloadPlaylistsLoad<Partial<PlaylistDocument>>();
        set({ playlists: doc.playlists ?? [], loaded: true });
      } catch (e) {
        // load_json_or_default returns {} for missing file, so a
        // throw here means real corruption. Log + fall through to
        // empty state so the user can rebuild — the alternative
        // (blocking the screen with an error) hides every other
        // payload feature behind a recoverable issue.
        console.error("[payload-playlists] hydrate failed:", e);
        set({ loaded: true });
      }
    },

    createPlaylist(name) {
      const id = newId();
      const now = Date.now();
      const playlist: Playlist = {
        id,
        name: name || "Untitled playlist",
        steps: [],
        continueOnFailure: false,
        createdAt: now,
        updatedAt: now,
      };
      set((s) => ({ playlists: s.playlists.concat(playlist) }));
      scheduleSave();
      return id;
    },

    renamePlaylist(id, name) {
      set((s) => ({
        playlists: patchPlaylist(s.playlists, id, {
          name: name || "Untitled playlist",
        }),
      }));
      scheduleSave();
    },

    deletePlaylist(id) {
      set((s) => ({ playlists: removePlaylist(s.playlists, id) }));
      scheduleSave();
    },

    setContinueOnFailure(id, b) {
      set((s) => ({
        playlists: patchPlaylist(s.playlists, id, { continueOnFailure: b }),
      }));
      scheduleSave();
    },

    addStep(id, step) {
      set((s) => ({ playlists: appendStep(s.playlists, id, step) }));
      scheduleSave();
    },

    removeStep(id, index) {
      set((s) => ({ playlists: removeStep(s.playlists, id, index) }));
      scheduleSave();
    },

    moveStepUp(id, index) {
      set((s) => ({ playlists: moveStepUp(s.playlists, id, index) }));
      scheduleSave();
    },

    moveStepDown(id, index) {
      set((s) => ({ playlists: moveStepDown(s.playlists, id, index) }));
      scheduleSave();
    },

    updateStep(id, index, patch) {
      const list = get().playlists;
      const pl = list.find((p) => p.id === id);
      if (!pl || index < 0 || index >= pl.steps.length) return;
      const steps = pl.steps.slice();
      steps[index] = { ...steps[index], ...patch };
      set({ playlists: patchPlaylist(list, id, { steps }) });
      scheduleSave();
    },

    async run(id, host, port) {
      const playlist = get().playlists.find((p) => p.id === id);
      if (!playlist) return;
      if (get().runStatus.kind === "running" || get().runStatus.kind === "sleeping") {
        return;
      }
      const myRun = ++runId;
      // Fresh abort signal for this run. stop() will fire it; the
      // sleep races against it so a Stop click during a long sleep
      // doesn't wait out the timer.
      runAbort = new AbortController();
      const myAbort = runAbort.signal;
      const isLive = () => runId === myRun;

      let successCount = 0;
      let failureCount = 0;
      const failures: { stepIndex: number; error: string }[] = [];

      for (let i = 0; i < playlist.steps.length; i++) {
        if (!isLive()) return;
        const step = playlist.steps[i];
        // Per-step overrides take precedence over the playlist-wide
        // host/port supplied to run(). Empty string / 0 / undefined
        // all fall back to the playlist defaults.
        const stepHost = step.ip && step.ip.trim() ? step.ip.trim() : host;
        const stepPort =
          typeof step.port === "number" && step.port > 0 ? step.port : port;
        set({ runStatus: { kind: "running", playlistId: id, stepIndex: i } });

        try {
          await sendPayload(stepHost, step.path, stepPort);
          successCount++;
        } catch (e) {
          const errMsg = e instanceof Error ? e.message : String(e);
          failureCount++;
          failures.push({ stepIndex: i, error: errMsg });
          if (!playlist.continueOnFailure) {
            if (isLive()) {
              set({
                runStatus: {
                  kind: "failed",
                  playlistId: id,
                  stepIndex: i,
                  error: errMsg,
                },
              });
            }
            return;
          }
        }

        if (!isLive()) return;
        if (step.sleepMs > 0 && i < playlist.steps.length - 1) {
          set({
            runStatus: {
              kind: "sleeping",
              playlistId: id,
              stepIndex: i,
              sleepStartedAtMs: Date.now(),
              sleepDurationMs: step.sleepMs,
            },
          });
          await cancellableSleep(step.sleepMs, myAbort);
          // After the sleep returns (whether timed out or cancelled),
          // re-check liveness so a Stop during sleep doesn't push
          // through to "running" on the next step.
          if (!isLive()) return;
        }
      }

      if (isLive()) {
        set({
          runStatus: {
            kind: "done",
            playlistId: id,
            successCount,
            failureCount,
            failures,
          },
        });
      }
    },

    stop() {
      runId++;
      // Abort any in-flight sleep so the runner's await resolves now
      // instead of when the timer would have fired.
      runAbort?.abort();
      runAbort = null;
      set({ runStatus: { kind: "idle" } });
    },
  };
});

/** Sleep that resolves early if `signal` aborts. AbortError is
 *  swallowed — caller treats both timer-fire and abort the same way
 *  (it re-checks isLive() after this returns). */
function cancellableSleep(ms: number, signal: AbortSignal): Promise<void> {
  if (signal.aborted) return Promise.resolve();
  return new Promise((resolve) => {
    const onAbort = () => {
      clearTimeout(timer);
      signal.removeEventListener("abort", onAbort);
      resolve();
    };
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    signal.addEventListener("abort", onAbort, { once: true });
  });
}
