// Pure helpers for payload-playlist mutations and run state.
//
// A playlist is an ordered list of (payload_path, sleep_ms_after) steps
// the user can name and replay later from the SendPayload screen. The
// rules below — find by id, mutate steps, etc. — are easier to test
// in isolation than as inline closures over the Zustand store.

export interface PlaylistStep {
  /** Absolute path on the host to a .elf/.bin payload file. */
  path: string;
  /** Milliseconds to sleep after this step's send completes (success
   *  OR failure, whichever happens). 0 = no sleep. The runner uses
   *  this as a fixed delay; "wait for ready" is a separate future
   *  feature. */
  sleepMs: number;
}

export interface Playlist {
  id: string;
  name: string;
  /** Steps in send order. */
  steps: PlaylistStep[];
  /** When true, the runner skips past failures and finishes the
   *  playlist. When false (the default), the first failure stops the
   *  run with the remaining steps still pending. */
  continueOnFailure: boolean;
  createdAt: number;
  updatedAt: number;
}

export type PlaylistRunStatus =
  | { kind: "idle" }
  | { kind: "running"; playlistId: string; stepIndex: number }
  | {
      kind: "sleeping";
      playlistId: string;
      /** Index of the step whose post-send sleep is in progress. */
      stepIndex: number;
      /** Wall-clock ms when the sleep began — UI uses this to render
       *  a count-up so the user sees progress. */
      sleepStartedAtMs: number;
      /** Total sleep duration so the UI can render percent. */
      sleepDurationMs: number;
    }
  | {
      kind: "done";
      playlistId: string;
      /** Number of steps that succeeded. */
      successCount: number;
      /** Number of steps that failed. Non-zero only when the playlist
       *  had `continueOnFailure: true`. */
      failureCount: number;
    }
  | {
      kind: "failed";
      playlistId: string;
      stepIndex: number;
      error: string;
    };

/** Replace the playlist with the given id by applying `patch`. Returns
 *  the same array reference when not found. */
export function patchPlaylist(
  playlists: Playlist[],
  id: string,
  patch: Partial<Playlist>,
): Playlist[] {
  const i = playlists.findIndex((p) => p.id === id);
  if (i < 0) return playlists;
  const next = playlists.slice();
  next[i] = { ...next[i], ...patch, updatedAt: Date.now() };
  return next;
}

/** Add a step to the end of a playlist's step list. */
export function appendStep(
  playlists: Playlist[],
  playlistId: string,
  step: PlaylistStep,
): Playlist[] {
  return patchPlaylist(playlists, playlistId, {
    steps: (playlists.find((p) => p.id === playlistId)?.steps ?? []).concat(
      step,
    ),
  });
}

/** Remove the step at `index` from the named playlist. Returns same
 *  reference when index is out of range. */
export function removeStep(
  playlists: Playlist[],
  playlistId: string,
  index: number,
): Playlist[] {
  const pl = playlists.find((p) => p.id === playlistId);
  if (!pl || index < 0 || index >= pl.steps.length) return playlists;
  const steps = pl.steps.slice(0, index).concat(pl.steps.slice(index + 1));
  return patchPlaylist(playlists, playlistId, { steps });
}

/** Move a step earlier in its playlist. Returns same reference when
 *  no-op (already first, or index out of range). */
export function moveStepUp(
  playlists: Playlist[],
  playlistId: string,
  index: number,
): Playlist[] {
  const pl = playlists.find((p) => p.id === playlistId);
  if (!pl || index <= 0 || index >= pl.steps.length) return playlists;
  const steps = pl.steps.slice();
  [steps[index - 1], steps[index]] = [steps[index], steps[index - 1]];
  return patchPlaylist(playlists, playlistId, { steps });
}

/** Move a step later in its playlist. */
export function moveStepDown(
  playlists: Playlist[],
  playlistId: string,
  index: number,
): Playlist[] {
  const pl = playlists.find((p) => p.id === playlistId);
  if (!pl || index < 0 || index >= pl.steps.length - 1) return playlists;
  const steps = pl.steps.slice();
  [steps[index], steps[index + 1]] = [steps[index + 1], steps[index]];
  return patchPlaylist(playlists, playlistId, { steps });
}

/** Remove an entire playlist. */
export function removePlaylist(
  playlists: Playlist[],
  id: string,
): Playlist[] {
  const i = playlists.findIndex((p) => p.id === id);
  if (i < 0) return playlists;
  return playlists.slice(0, i).concat(playlists.slice(i + 1));
}

/** Validate that a sleep value is a non-negative finite integer. The
 *  number input on the UI side will let users type whatever; we coerce
 *  malformed values back to 0 so the runner doesn't get NaN/Infinity. */
export function sanitiseSleepMs(raw: number | string): number {
  const n = typeof raw === "string" ? Number(raw) : raw;
  if (!Number.isFinite(n) || n < 0) return 0;
  return Math.floor(n);
}
