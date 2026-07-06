import { create } from "zustand";

import { hostOf } from "../lib/addr";
import {
  payloadPlaylistsLoad,
  payloadPlaylistsSave,
  payloadsDownload,
  payloadsLocalInventory,
  payloadsRelease,
  sendPayload,
} from "../api/ps5";
import { log } from "./logs";
import {
  appendStep,
  DEFAULT_AUTO_LOADER,
  isRepoStep,
  movePlaylistDown,
  movePlaylistUp,
  moveStepDown,
  moveStepUp,
  patchPlaylist,
  removePlaylist,
  removeStep,
  resolveRepoStepPath,
  type AutoLoaderConfig,
  type Playlist,
  type PlaylistRunStatus,
  type PlaylistStep,
} from "../lib/playlistOps";

/** Catalogue-API deps for `resolveRepoStepPath`, wired to the real Tauri
 *  commands (#93). Extracted so the run loop stays readable. */
const REPO_RESOLVE_DEPS = {
  localInventory: () => payloadsLocalInventory(),
  latestRelease: (id: string) => payloadsRelease(id, false),
  download: (id: string, assetUrl: string, version: string) =>
    payloadsDownload(id, assetUrl, version),
};

/**
 * Persisted payload-playlist store.
 *
 * A playlist is a named ordered list of (payload_path, sleep_after_ms)
 * steps the user can replay against any reachable PS5. Useful for
 * scripted boot sequences — e.g. send a loader, sleep 5s for it to
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
 *
 * PER-HOST (multi-console). A run is owned by the console it was launched
 * against (`runStatusByHost`, keyed by the playlist-wide host passed to
 * `run()`), with its OWN generation counter and sleep-abort controller. Two
 * consoles can each run a playlist at the same time without one's Stop or
 * completion clobbering the other's banner. The playlist *documents* stay
 * global (they're reusable scripts); only the live run state is per-console.
 */

interface PlaylistDocument {
  playlists: Playlist[];
  /** Persisted alongside the playlists in the same JSON file — it's a
   *  property OF the playlist set (which one auto-runs), so co-locating it
   *  keeps import/export a single document and avoids a second store. */
  autoLoader?: AutoLoaderConfig;
}

interface PlaylistState {
  playlists: Playlist[];
  loaded: boolean;
  /** The one playlist that auto-runs when a console's helper becomes
   *  ready. See AutoLoaderConfig. Drives the Auto-loader card in the
   *  Playlists panel and the trigger in AppShell's status poller. */
  autoLoader: AutoLoaderConfig;
  /** Live status of each console's running playlist, keyed by bare host
   *  (port-stripped). The SendPayload screen renders a banner from the
   *  active console's slot so the user always knows whether that console's
   *  runner is mid-step, sleeping, or settled. Reads go through
   *  `runStatusForHost(s, host)`. */
  runStatusByHost: Record<string, PlaylistRunStatus>;

  hydrate: () => Promise<void>;

  // CRUD on playlists
  createPlaylist: (name: string) => string;
  renamePlaylist: (id: string, name: string) => void;
  deletePlaylist: (id: string) => void;
  setContinueOnFailure: (id: string, b: boolean) => void;
  movePlaylistUp: (id: string) => void;
  movePlaylistDown: (id: string) => void;

  // CRUD on steps within a playlist
  addStep: (id: string, step: PlaylistStep) => void;
  removeStep: (id: string, index: number) => void;
  moveStepUp: (id: string, index: number) => void;
  moveStepDown: (id: string, index: number) => void;
  updateStep: (id: string, index: number, patch: Partial<PlaylistStep>) => void;

  // Run control
  run: (id: string, host: string, port: number) => Promise<void>;
  /** Stop the run owned by `host` (the console the run was launched against).
   *  Other consoles' runs are untouched. */
  stop: (host: string) => void;

  /** Patch the auto-loader config (enable/disable, choose playlist). */
  setAutoLoader: (patch: Partial<AutoLoaderConfig>) => void;
}

const SAVE_DEBOUNCE_MS = 300;

/** Stable idle status returned by `runStatusForHost` for a console with no
 *  run. MUST be a singleton so Zustand selectors comparing by `Object.is`
 *  stay stable (a fresh `{ kind: "idle" }` per render would thrash). */
const IDLE_RUN_STATUS: PlaylistRunStatus = { kind: "idle" };

const keyOf = (host: string | null | undefined): string =>
  host?.trim() ? hostOf(host) : "";

/** Read one console's playlist run status from a store snapshot. */
export function runStatusForHost(
  s: { runStatusByHost: Record<string, PlaylistRunStatus> },
  host: string | null | undefined,
): PlaylistRunStatus {
  return s.runStatusByHost[keyOf(host)] ?? IDLE_RUN_STATUS;
}

function newId(): string {
  return crypto.randomUUID().replace(/-/g, "");
}

export const usePayloadPlaylistsStore = create<PlaylistState>((set, get) => {
  // Per-console generation counters (shared lib/runGen). Lazily created per
  // host key the first time a console runs, so each console's run() and
  // stop() only affect that console's runner.
  const gens = new Map<string, ReturnType<typeof createRunGen>>();
  const genFor = (key: string) => {
    let g = gens.get(key);
    if (!g) {
      g = createRunGen();
      gens.set(key, g);
    }
    return g;
  };
  // Per-console abort controllers for the runner's sleep step. Set on every
  // `run()` so a stale stop() can't cancel a fresh run; cleared when the
  // runner exits the sleep cleanly. `stop(host)` aborts that console's so
  // `cancellableSleep` resolves immediately and the runner notices the
  // runId change at its next isLive() check.
  const aborts = new Map<string, AbortController>();
  let saveTimer: ReturnType<typeof setTimeout> | null = null;

  /** Write one console's run status (merges into the byHost map). */
  const setRunStatus = (key: string, status: PlaylistRunStatus) =>
    set((s) => ({
      runStatusByHost: { ...s.runStatusByHost, [key]: status },
    }));

  const scheduleSave = () => {
    if (saveTimer !== null) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      saveTimer = null;
      const { playlists, autoLoader } = get();
      const doc: PlaylistDocument = { playlists, autoLoader };
      void payloadPlaylistsSave(doc).catch((e) => {
        // Same rationale as upload-queue: log so it lands in the
        // dev console + Windows engine.log; a UI toast is future
        // work but would need throttling.
        log.error("playlist", `save failed: ${e instanceof Error ? e.message : String(e)}`);
      });
    }, SAVE_DEBOUNCE_MS);
  };

  return {
    playlists: [],
    loaded: false,
    runStatusByHost: {},
    autoLoader: DEFAULT_AUTO_LOADER,

    async hydrate() {
      // Browser-only contexts: Tauri invoke unavailable. Same rationale
      // as uploadQueue.hydrate — silently no-op so the user-visible Logs
      // tab doesn't fill with "invoke undefined" errors during dev/test.
      const w = window as unknown as { isTauri?: boolean; __TAURI_INTERNALS__?: unknown };
      if (!w.isTauri && !w.__TAURI_INTERNALS__) {
        set({ loaded: true });
        return;
      }
      try {
        const doc = await payloadPlaylistsLoad<Partial<PlaylistDocument>>();
        set({
          playlists: doc.playlists ?? [],
          autoLoader: { ...DEFAULT_AUTO_LOADER, ...(doc.autoLoader ?? {}) },
          loaded: true,
        });
      } catch (e) {
        // load_json_or_default returns {} for missing file, so a
        // throw here means real corruption. Log + fall through to
        // empty state so the user can rebuild — the alternative
        // (blocking the screen with an error) hides every other
        // payload feature behind a recoverable issue.
        log.error("playlist", `hydrate failed: ${e instanceof Error ? e.message : String(e)}`);
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

    movePlaylistUp(id) {
      set((s) => ({ playlists: movePlaylistUp(s.playlists, id) }));
      scheduleSave();
    },

    movePlaylistDown(id) {
      set((s) => ({ playlists: movePlaylistDown(s.playlists, id) }));
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
      // A run is owned by the console it's launched against. Only block if
      // THIS console is already mid-run; another console running its own
      // playlist is fine.
      const key = keyOf(host);
      const cur = get().runStatusByHost[key]?.kind;
      if (cur === "running" || cur === "sleeping") {
        return;
      }
      // Stamp "recently run" (on start, so a run that fails mid-way still
      // counts as recently used). Direct map rather than patchPlaylist so
      // updatedAt isn't bumped — running isn't a content edit. Persisted
      // so the Recently-run strip survives an app restart.
      set((s) => ({
        playlists: s.playlists.map((p) =>
          p.id === id ? { ...p, lastRunAt: Date.now() } : p,
        ),
      }));
      scheduleSave();
      const g = genFor(key);
      const myRun = g.next();
      // Fresh abort signal for this run. stop(host) will fire it; the
      // sleep races against it so a Stop click during a long sleep
      // doesn't wait out the timer.
      const myAbortCtl = new AbortController();
      aborts.set(key, myAbortCtl);
      const myAbort = myAbortCtl.signal;
      const isLive = () => g.isLive(myRun);

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
        setRunStatus(key, {
          kind: "running",
          playlistId: id,
          stepIndex: i,
          host,
        });

        try {
          // Repo-sourced step (#129): resolve the payload id to a cached ELF
          // — using the cached copy if present, else fetching the latest
          // release + downloading it. This is what lets a playlist reference
          // repos instead of local files: the user keeps no ELFs on their PC.
          // A freshly-resolved path is cached back onto the step so a later
          // OFFLINE run (or the auto-loader on next boot) can still find it.
          let sendPath = step.path;
          if (isRepoStep(step)) {
            sendPath = await resolveRepoStepPath(
              step.payloadId as string,
              REPO_RESOLVE_DEPS,
            );
            if (isLive() && sendPath && sendPath !== step.path) {
              set((s) => ({
                playlists: s.playlists.map((p) =>
                  p.id === id
                    ? {
                        ...p,
                        steps: p.steps.map((st, idx) =>
                          idx === i ? { ...st, path: sendPath } : st,
                        ),
                      }
                    : p,
                ),
              }));
              scheduleSave();
            }
          }
          if (!isLive()) return;
          await sendPayload(stepHost, sendPath, stepPort);
          successCount++;
        } catch (e) {
          const errMsg = e instanceof Error ? e.message : String(e);
          failureCount++;
          failures.push({ stepIndex: i, error: errMsg });
          // Log every step failure: the run-status banner is transient (gone by
          // the time a user files a bug report), so this is the only durable
          // trace of "my playlist / auto-loader didn't run". Includes the step
          // file + target so a maintainer can see exactly what failed where.
          const stepName =
            step.payloadLabel ||
            (step.path.split(/[\\/]/).pop() ?? step.payloadId ?? step.path);
          log.warn(
            "playlist",
            `step ${i + 1} (${stepName}) failed on ${stepHost}:${stepPort} — ${errMsg}`,
          );
          if (!playlist.continueOnFailure) {
            if (isLive()) {
              setRunStatus(key, {
                kind: "failed",
                playlistId: id,
                host,
                stepIndex: i,
                error: errMsg,
              });
            }
            return;
          }
        }

        if (!isLive()) return;
        if (step.sleepMs > 0 && i < playlist.steps.length - 1) {
          setRunStatus(key, {
            kind: "sleeping",
            playlistId: id,
            host,
            stepIndex: i,
            sleepStartedAtMs: Date.now(),
            sleepDurationMs: step.sleepMs,
          });
          await cancellableSleep(step.sleepMs, myAbort);
          // After the sleep returns (whether timed out or cancelled),
          // re-check liveness so a Stop during sleep doesn't push
          // through to "running" on the next step.
          if (!isLive()) return;
        }
      }

      if (isLive()) {
        setRunStatus(key, {
          kind: "done",
          playlistId: id,
          host,
          successCount,
          failureCount,
          failures,
        });
      }
      // Clean up this run's abort controller if it's still the active one
      // (a later run() on the same host would have replaced it).
      if (aborts.get(key) === myAbortCtl) aborts.delete(key);
    },

    stop(host) {
      const key = keyOf(host);
      genFor(key).next();
      // Abort any in-flight sleep on this console so the runner's await
      // resolves now instead of when the timer would have fired.
      aborts.get(key)?.abort();
      aborts.delete(key);
      setRunStatus(key, { kind: "idle" });
    },

    setAutoLoader(patch) {
      set((s) => ({ autoLoader: { ...s.autoLoader, ...patch } }));
      scheduleSave();
    },
  };
});

/** Sleep that resolves early if `signal` aborts. AbortError is
 *  swallowed — caller treats both timer-fire and abort the same way
 *  (it re-checks isLive() after this returns).
 *
 *  2.12.0: the underlying timer/listener dance is now in
 *  lib/cancellableSleep; we wrap to preserve the resolve-on-abort
 *  semantics this caller relies on. */
import {
  cancellableSleep as canonicalCancellableSleep,
  isAbortError,
} from "../lib/cancellableSleep";
import { createRunGen } from "../lib/runGen";

async function cancellableSleep(
  ms: number,
  signal: AbortSignal,
): Promise<void> {
  try {
    await canonicalCancellableSleep(ms, signal);
  } catch (e) {
    if (!isAbortError(e)) throw e;
    // Swallow AbortError — caller re-checks isLive() to decide.
  }
}
