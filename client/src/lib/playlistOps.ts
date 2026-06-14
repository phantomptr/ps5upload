// Pure helpers for payload-playlist mutations and run state.
//
// A playlist is an ordered list of (payload_path, sleep_ms_after) steps
// the user can name and replay later from the SendPayload screen. The
// rules below — find by id, mutate steps, etc. — are easier to test
// in isolation than as inline closures over the Zustand store.

export interface PlaylistStep {
  /** Absolute path on the host to a payload file
   *  (.elf/.bin/.js/.lua/.jar — whatever your loader accepts). */
  path: string;
  /** Milliseconds to sleep after this step's send completes (success
   *  OR failure, whichever happens). 0 = no sleep. The runner uses
   *  this as a fixed delay; "wait for ready" is a separate future
   *  feature. */
  sleepMs: number;
  /** Optional per-step IP override. When non-empty, the runner uses
   *  this address for THIS step's send instead of the playlist-wide
   *  IP supplied at run time. Useful for sequences that target
   *  multiple PS5s in one go (e.g. push a loader to dev kit, then
   *  push the harness to the test kit). Empty / undefined means
   *  "use the IP the user typed when they hit Run." */
  ip?: string;
  /** Optional per-step loader port override. Same rationale as `ip`.
   *  Most scene payloads bind 9021; a few custom builds use other
   *  ports. Empty / undefined / 0 means "use the playlist-wide
   *  port supplied at run time" (default 9021). */
  port?: number;
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
  /** Wall-clock ms of the last time this playlist was run (started),
   *  successful or not. Drives the "Recently run" quick-access strip.
   *  Undefined for playlists that have never been run. Deliberately
   *  separate from `updatedAt` (which tracks edits) so running a
   *  playlist doesn't reorder an updatedAt-sorted view. */
  lastRunAt?: number;
}

/**
 * Auto-loader config: a single playlist that runs by itself whenever a
 * console's helper payload becomes ready (cold boot, first-time-setup
 * completion, or a reconnect after the helper had gone down). One global
 * setting — the playlist runs against whichever console just came up.
 *
 * Opt-in: disabled by default, because auto-sending payloads the instant
 * a PS5 appears is a surprising side effect unless the user asked for it.
 * The trigger edge (helper went down → up) is what makes it fire once per
 * reconnect rather than on every status poll; see AppShell's status poller.
 */
export interface AutoLoaderConfig {
  enabled: boolean;
  /** Id of the playlist to auto-run when the helper becomes ready (the
   *  POST-helper chain — your usual apps/payloads). Null = none chosen. */
  playlistId: string | null;
  /** Id of the PRE-helper bring-up playlist (kernel-R/W payload, SMP, etc.)
   *  that "Quick bring-up" runs against the loader before sending the helper.
   *  Null = bring-up sends only the helper (no pre-helper chain). */
  bringUpPlaylistId: string | null;
}

export const DEFAULT_AUTO_LOADER: AutoLoaderConfig = {
  enabled: false,
  playlistId: null,
  bringUpPlaylistId: null,
};

/** Payload file extensions the loaders accept: .elf/.bin are the common
 *  ELF loaders (port 9021); .js/.lua/.jar target the scripting loaders on
 *  their own ports. Used to filter dropped/multi-selected files when
 *  building a playlist so non-payloads — and .pkg installs, which go
 *  through a different flow — are ignored. */
export const PAYLOAD_EXTENSIONS = ["elf", "bin", "js", "lua", "jar"] as const;

/** True if `path` ends in a known payload extension (case-insensitive).
 *  Pure + extension-only: it never touches the filesystem, so a path that
 *  doesn't exist still classifies by name (the send call surfaces a real
 *  read error later if the file is missing). */
export function isPayloadPath(path: string): boolean {
  const m = /\.([a-z0-9]+)$/i.exec(path.trim());
  if (!m) return false;
  return (PAYLOAD_EXTENSIONS as readonly string[]).includes(
    m[1].toLowerCase(),
  );
}

export type PlaylistRunStatus =
  | { kind: "idle" }
  | { kind: "running"; playlistId: string; stepIndex: number; host: string }
  | {
      kind: "sleeping";
      playlistId: string;
      /** Console (bare host) the playlist runs against — lets the run-status
       *  banner name the PS5 when several are connected. Execution is already
       *  per-host (host/port captured in run()'s closure); this is display. */
      host: string;
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
      /** Console (bare host) the playlist ran against. */
      host: string;
      /** Number of steps that succeeded. */
      successCount: number;
      /** Number of steps that failed. Non-zero only when the playlist
       *  had `continueOnFailure: true`. */
      failureCount: number;
      /** Per-step failures captured when continue-on-failure is set,
       *  so the user can see WHICH step broke (and why) without
       *  inspecting logs. Empty when failureCount is 0. */
      failures: { stepIndex: number; error: string }[];
    }
  | {
      kind: "failed";
      playlistId: string;
      /** Console (bare host) the playlist ran against. */
      host: string;
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

/** Move a playlist earlier in the list. Returns the same reference when
 *  it's a no-op (already first, or id missing). Note: this reorders the
 *  whole-document array, which is what gets persisted, so the order
 *  survives a restart. We do NOT bump updatedAt — reordering isn't an
 *  edit to the playlist's content. */
export function movePlaylistUp(playlists: Playlist[], id: string): Playlist[] {
  const i = playlists.findIndex((p) => p.id === id);
  if (i <= 0) return playlists;
  const next = playlists.slice();
  [next[i - 1], next[i]] = [next[i], next[i - 1]];
  return next;
}

/** Move a playlist later in the list. Returns the same reference when
 *  it's a no-op (already last, or id missing). */
export function movePlaylistDown(playlists: Playlist[], id: string): Playlist[] {
  const i = playlists.findIndex((p) => p.id === id);
  if (i < 0 || i >= playlists.length - 1) return playlists;
  const next = playlists.slice();
  [next[i], next[i + 1]] = [next[i + 1], next[i]];
  return next;
}

/** Validate that a sleep value is a non-negative finite integer. The
 *  number input on the UI side will let users type whatever; we coerce
 *  malformed values back to 0 so the runner doesn't get NaN/Infinity. */
export function sanitiseSleepMs(raw: number | string): number {
  const n = typeof raw === "string" ? Number(raw) : raw;
  if (!Number.isFinite(n) || n < 0) return 0;
  return Math.floor(n);
}
