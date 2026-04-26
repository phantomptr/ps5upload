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
  let saveTimer: ReturnType<typeof setTimeout> | null = null;

  const scheduleSave = () => {
    if (saveTimer !== null) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      saveTimer = null;
      const { playlists } = get();
      const doc: PlaylistDocument = { playlists };
      void payloadPlaylistsSave(doc).catch(() => {
        // Persistence failure is degraded UX (playlists won't survive
        // restart) but doesn't break the in-session experience.
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
      } catch {
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
      const isLive = () => runId === myRun;

      let successCount = 0;
      let failureCount = 0;

      for (let i = 0; i < playlist.steps.length; i++) {
        if (!isLive()) return;
        const step = playlist.steps[i];
        set({ runStatus: { kind: "running", playlistId: id, stepIndex: i } });

        try {
          await sendPayload(host, step.path, port);
          successCount++;
        } catch (e) {
          failureCount++;
          if (!playlist.continueOnFailure) {
            if (isLive()) {
              set({
                runStatus: {
                  kind: "failed",
                  playlistId: id,
                  stepIndex: i,
                  error: e instanceof Error ? e.message : String(e),
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
          await sleep(step.sleepMs);
        }
      }

      if (isLive()) {
        set({
          runStatus: {
            kind: "done",
            playlistId: id,
            successCount,
            failureCount,
          },
        });
      }
    },

    stop() {
      runId++;
      set({ runStatus: { kind: "idle" } });
    },
  };
});

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
