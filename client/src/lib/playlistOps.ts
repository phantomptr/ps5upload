// Pure helpers for payload-playlist mutations and run state.
//
// A playlist is an ordered list of (payload_path, sleep_ms_after) steps
// the user can name and replay later from the SendPayload screen. The
// rules below — find by id, mutate steps, etc. — are easier to test
// in isolation than as inline closures over the Zustand store.

export interface PlaylistStep {
  /** Absolute path on the host to a payload file
   *  (.elf/.bin/.js/.lua/.jar — whatever your loader accepts).
   *
   *  For a REPO-SOURCED step (#129) this may be empty at rest: the
   *  runner resolves `payloadId` to a cached ELF at send time (fetching
   *  the latest release + downloading if not already cached), so a
   *  playlist can reference repos instead of pinning local files. A
   *  resolved path is cached back onto the step after a successful
   *  resolve so a later offline run can still find it. */
  path: string;
  /** Repo source for this step (#129). When set, the runner resolves it
   *  to a cached payload ELF at run time via the Payloads catalogue
   *  (`payloadsRelease` + `payloadsDownload` from #93) rather than using
   *  a fixed host path — so the user doesn't have to keep the ELF on
   *  their PC. This is a catalogue id ("kstuff-echostretch") or a custom
   *  repo id ("custom-<hex>"). Undefined = a plain local-file step. */
  payloadId?: string;
  /** Display name for a repo-sourced step (the payload's display_name at
   *  add time), so the UI can label the row without re-fetching the
   *  catalogue. Ignored for local-file steps. */
  payloadLabel?: string;
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

/** True when a step is repo-sourced (#129) — the runner resolves its
 *  `payloadId` to a cached ELF at send time instead of using `path`. */
export function isRepoStep(step: Pick<PlaylistStep, "payloadId">): boolean {
  return !!step.payloadId && step.payloadId.trim().length > 0;
}

/** Minimal view of the catalogue APIs the resolver needs — injected so
 *  `resolveRepoStepPath` is unit-testable without Tauri. */
export interface RepoResolveDeps {
  /** Local inventory: payload_id → cached ELF path (from #93). */
  localInventory: () => Promise<{ payload_id: string; path: string; version: string }[]>;
  /** Latest release for a payload id: its picked asset + tag (from #93). */
  latestRelease: (
    id: string,
  ) => Promise<{ tag: string; picked_asset_url: string }>;
  /** Download an asset to the local cache, returning the cached path. */
  download: (id: string, assetUrl: string, version: string) => Promise<{ path: string }>;
}

/**
 * Resolve a repo-sourced step to a local cached payload path (#129).
 *
 * Order:
 *   1. If a specific `version` isn't pinned and the payload is already in
 *      the local cache, use the cached path (fast, offline-friendly).
 *   2. Otherwise fetch the latest release and download it (cache miss), then
 *      use the freshly-cached path.
 *   3. If the network fetch fails but SOME cached copy exists, fall back to
 *      it (stale is better than a failed boot sequence) — the catalogue's
 *      own stale-cache handling already applies to the release metadata.
 *
 * `preferCached` (default true) short-circuits at step 1 so a routine
 * auto-loader run doesn't hit GitHub every boot; the UI's explicit
 * "update" affordance passes false to force a fresh fetch. Throws only
 * when there's neither a usable release nor any cached copy.
 */
export async function resolveRepoStepPath(
  payloadId: string,
  deps: RepoResolveDeps,
  preferCached = true,
): Promise<string> {
  const inv = await deps.localInventory().catch(() => []);
  const cached = inv.find((e) => e.payload_id === payloadId);

  if (preferCached && cached?.path) {
    return cached.path;
  }

  try {
    const rel = await deps.latestRelease(payloadId);
    if (!rel.picked_asset_url) {
      // Release exists but has no downloadable asset — fall back to a
      // cached copy if we have one, else it's a hard error.
      if (cached?.path) return cached.path;
      throw new Error(
        `no downloadable payload asset in the latest release of "${payloadId}"`,
      );
    }
    // Already cached at this exact version? Skip the re-download.
    if (cached?.path && cached.version === rel.tag) {
      return cached.path;
    }
    const dl = await deps.download(payloadId, rel.picked_asset_url, rel.tag);
    return dl.path;
  } catch (e) {
    // Network / rate-limit / parse failure: a cached copy keeps the run
    // going; otherwise surface the error so the step fails loudly.
    if (cached?.path) return cached.path;
    throw e instanceof Error ? e : new Error(String(e));
  }
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
