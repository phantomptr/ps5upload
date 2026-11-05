// The Collection: games kept on this computer's drives, indexed by the engine
// (engine/crates/ps5upload-engine/src/collection). Same field names as PS Game Library's index.

import { getEngineUrl } from "../state/engine";
import { invoke } from "../lib/invokeLogged";
import { getCachedIcon, setCachedIcon } from "../lib/iconMemoryCache";
import { ENGINE_LONG_TIMEOUT_MS, ENGINE_TIMEOUT_MS, engineFetch, type EngineFetchInit } from "../lib/engineFetch";

/** Keyed by the cover's version too: a cover read out again must not be answered with the
 *  bytes this session held for the old one. */
const coverKey = (gameId: string, version?: string, size?: number) =>
  `collection|${gameId}|${version ?? ""}${size ? `|${size}` : ""}`;

/** A Collection cover this session already holds, as a `data:` URL. */
export function cachedCollectionCover(gameId: string, version?: string, size?: number): string | undefined {
  return getCachedIcon(coverKey(gameId, version, size));
}

/** A Collection cover over the IPC, as a `data:` URL, or null. The desktop window's own load
 *  of the cover URL is refused by the engine's cross-site guard (WebKit sends neither Origin
 *  nor Referer for it), so this is the fallback `useImageRetry` turns to. */
export async function collectionCoverDataUrl(
  gameId: string,
  version?: string,
  size?: number,
): Promise<string | null> {
  const hit = getCachedIcon(coverKey(gameId, version, size));
  if (hit) return hit;
  try {
    const url = await invoke<string>("collection_cover_data", { gameId, size });
    if (typeof url === "string" && url.startsWith("data:")) {
      setCachedIcon(coverKey(gameId, version, size), url);
      return url;
    }
    return null;
  } catch {
    return null;
  }
}

export interface CollectionPkg {
  platform?: string;
  title?: string;
  title_id?: string;
  content_id?: string;
  version?: string;
  region?: string;
  /** "base" | "patch" | "dlc" */
  kind?: string;
  kind_confident?: boolean;
  kind_reason?: string;
  complete?: boolean;
  /** The firmware the copy asks for ("7.00"). A PS4 copy names a PS4 firmware. */
  min_fw?: string;
  error?: string;
}

/** One problem a copy check found: a file inside the copy, or "" for the copy as a whole. */
export interface CopyProblem {
  file: string;
  error: string;
}

/** A copy check (`/api/collection/check`). `method` says how far it could look: `package`
 *  (verified against its own digests), `archive` (every entry's checksum), `files` (every file
 *  read back) or `bytes` (read start to end: finds a cut-short or unreadable copy only). */
export interface CopyCheck {
  path: string;
  state: "queued" | "running" | "ok" | "problems" | "failed" | "cancelled";
  method: "" | "package" | "archive" | "files" | "bytes";
  bytes_done: number;
  bytes_total: number;
  files_done: number;
  files_total: number;
  problems: CopyProblem[];
  problems_total: number;
  error: string | null;
  started_ms: number;
  finished_ms: number;
}

export interface CollectionLocation {
  root: string;
  container: string;
  name: string;
  /** pkg | folder | rar | 7z | zip | mount.exfat | mount.ffpkg | mount.ffpfs | mount.ffpfsc */
  type: string;
  path: string;
  absolute_path: string;
  size_bytes: number;
  added_at?: string;
  modified_at?: string;
  date_source?: string;
  added_ts: number;
  pkg?: CollectionPkg;
  /** The copy's other files (a split package's later parts, a `.rar` set's later volumes). They
   *  count in `size_bytes` and go wherever it goes. */
  parts?: string[];
}

export interface CollectionGame {
  game_id: string;
  title: string;
  platform: string;
  sources: string[];
  locations: CollectionLocation[];
  total_size_bytes: number;
  copies: number;
  is_duplicate: boolean;
  added_at?: string;
  first_added_at?: string;
  added_ts: number;
  local_cover?: string;
  /** The saved cover file's size and modified time: changes when the cover is read out again. */
  cover_stamp?: string;
  cover_url?: string;
  title_source?: string;
}

export interface CollectionSummary {
  total_games: number;
  total_locations: number;
  total_size_bytes: number;
  duplicates_count: number;
  reclaimable_bytes: number;
}

export interface CollectionLibrary {
  roots?: string[];
  /** Roots the last scan could not open (deleted, unplugged, server down); skipped. */
  missing_roots?: string[];
  /** Folders the last scan could not read: what it found there before is kept. */
  unreadable?: string[];
  generated_at?: string;
  summary: CollectionSummary;
  games: Record<string, CollectionGame>;
}

export interface CollectionSettings {
  roots: string[];
  /** Seconds between automatic scans; null turns it off. */
  refresh_secs: number | null;
  sweep_sidecars: boolean;
  /** Where the engine has no trash: Move to Trash deletes for good (after a confirmation). */
  allow_permanent_delete?: boolean;
  /** False in Docker or on a server without a desktop. */
  trash_available?: boolean;
  refresh_choices?: number[];
}

export interface CollectionScanStatus {
  running: boolean;
  deep: boolean;
  found: number;
  done: number;
  started_ms: number;
  finished_ms: number;
  error: string | null;
  games: number;
  locations: number;
  /** While running: what it is doing. Older engines leave these out. */
  phase?: "finding" | "reading" | "naming" | "saving";
  /** The folder being searched, or the item or game being read. */
  current?: string;
  roots_total?: number;
  roots_done?: number;
  games_total?: number;
  games_done?: number;
  /** Folders the last scan could not read. */
  unreadable?: string[];
}

/** A package in the collection a console could take. */
export interface CollectionOffer {
  path: string;
  name: string;
  version: string;
  content_id: string;
  title: string;
  size_bytes: number;
  /** gd | gp | ac: orders installs base → update → DLC. */
  category: string;
}

/** One game against one console. */
export interface GameConsoleState {
  game_id: string;
  installed: boolean;
  installed_version?: string;
  registered_from?: string;
  base?: CollectionOffer;
  update?: CollectionOffer;
  dlc_missing: CollectionOffer[];
  /** Kept only as a folder, image or archive: it reaches the console by upload. */
  non_package_copy: boolean;
}

export interface TrashItem {
  path: string;
  name: string;
  game_id: string;
  title: string;
  size_bytes: number;
  /** Its other files: they go with it. */
  parts?: string[];
}

export interface TrashPreview {
  token: string;
  items: TrashItem[];
  total_bytes: number;
  /** False where the copies cannot be removed (no trash, permanent delete off). */
  available: boolean;
  /** trash (recoverable) or delete (for good). */
  mode: "trash" | "delete" | "none";
}

/** One move the organizer plans: paths are relative to `container`. */
export interface OrganizeMove {
  container: string;
  from: string;
  to: string;
  platform: string;
  title_id: string;
  title: string;
  kind: string;
  confident: boolean;
  reason: string;
  version: string;
  region: string;
  size: number;
  content_id: string;
  part?: string;
  /** A part of a split package: its set. A set moves as one, all of it or none of it. */
  set?: string;
  duplicate?: boolean;
  duplicate_of?: string;
}

export interface OrganizeSkipped {
  container: string;
  path: string;
  reason: string;
  detail: string;
}

export interface OrganizePlan {
  token: string;
  containers: string[];
  /** How to show each container: its path, or "Server: /path" on a saved server. */
  labels?: Record<string, string>;
  moves: OrganizeMove[];
  skipped: OrganizeSkipped[];
  unsure_count: number;
  total_bytes: number;
}

/** The preview being made in the background, or the last one. */
export interface OrganizePlanStatus {
  running: boolean;
  /** While running: walking the folders, or reading the packages the index does not know. */
  phase: "finding" | "reading" | "";
  done: number;
  total: number;
  current: string;
  cancelled: boolean;
  error: string | null;
  plan: OrganizePlan | null;
  started_ms: number;
  finished_ms: number;
}

export interface OrganizeResult {
  moved: OrganizeMove[];
  failed: (OrganizeMove & { error: string })[];
  dropped: string[];
  kept_dirs: string[];
  logs: string[];
}

export interface OrganizeRun {
  id: string;
  container_root: string;
  /** The folder as shown: its path, or "Server: /path" on a saved server. */
  label?: string;
  created_at: string;
  moved: number;
  failed: number;
  reverted_at: string | null;
  /** Its undo log could not be read: a run happened, and it cannot be undone from here. */
  unreadable?: boolean;
}

export interface JunkFile {
  path: string;
  size: number;
  allocated: number;
  sidecar: boolean;
}

export interface JunkStatus {
  running: boolean;
  cancelled: boolean;
  checked: number;
  found: number;
  sidecars: number;
  allocated: number;
  token: string | null;
  sample: JunkFile[];
  finished_ms: number;
}

export interface MacSettings {
  available: boolean;
  finder_network_off: boolean;
  finder_usb_off: boolean;
  volumes: { volume: string; indexing: string; has_index: boolean }[];
}

/** A refusal from the engine: `code` says which, for the words in the user's language. */
export class CollectionApiError extends Error {
  code?: string;
  constructor(message: string, code?: string) {
    super(message);
    this.name = "CollectionApiError";
    this.code = code;
  }
}

async function call<T>(path: string, init?: EngineFetchInit): Promise<T> {
  // Reads a whole index, or a console: allowed more than a plain request.
  const res = await engineFetch(`${getEngineUrl()}/api/collection${path}`, {
    timeoutMs: 2 * ENGINE_TIMEOUT_MS,
    ...init,
    headers: init?.body ? { "content-type": "application/json" } : undefined,
  });
  const text = await res.text();
  let body: unknown;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = null;
  }
  if (!res.ok) {
    const b = body as { error?: string; code?: string } | null;
    throw new CollectionApiError(b?.error ?? `HTTP ${res.status}`, b?.code);
  }
  return body as T;
}

export const collection = {
  library: () => call<CollectionLibrary>("/library"),
  settings: () => call<CollectionSettings>("/settings"),
  saveSettings: (s: CollectionSettings) =>
    call<CollectionSettings>("/settings", {
      method: "PUT",
      body: JSON.stringify({
        roots: s.roots,
        refresh_secs: s.refresh_secs,
        sweep_sidecars: s.sweep_sidecars,
        allow_permanent_delete: s.allow_permanent_delete ?? false,
      }),
    }),
  scan: (deep: boolean) =>
    call<CollectionScanStatus>("/scan", {
      method: "POST",
      body: JSON.stringify({ deep }),
    }),
  scanStatus: () => call<CollectionScanStatus>("/scan"),
  cancelScan: () =>
    call<CollectionScanStatus>("/scan/cancel", { method: "POST" }),
  /** The export's text (json | csv | md). */
  exportText: async (format: "json" | "csv" | "md"): Promise<string> => {
    const res = await engineFetch(
      `${getEngineUrl()}/api/collection/export?format=${format}`,
    );
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    return res.text();
  },
  /** What the console at `addr` (IP:mgmt port) has for every game in the collection. */
  consoleState: (addr: string) =>
    call<{ games: GameConsoleState[] }>(
      `/console?addr=${encodeURIComponent(addr)}`,
    ),
  trashPreview: (paths: string[]) =>
    call<TrashPreview>("/trash/preview", {
      method: "POST",
      body: JSON.stringify({ paths }),
    }),
  trashApply: (token: string) =>
    call<{ moved: string[]; failed: string[] }>("/trash/apply", {
      timeoutMs: ENGINE_LONG_TIMEOUT_MS,
      method: "POST",
      body: JSON.stringify({ token }),
    }),
  organizePlan: () =>
    call<OrganizePlan>("/organize/plan", { method: "POST", body: "{}" }),
  organizePlanStart: () =>
    call<OrganizePlanStatus>("/organize/plan/start", {
      method: "POST",
      body: "{}",
    }),
  organizePlanStatus: () => call<OrganizePlanStatus>("/organize/plan/status"),
  organizePlanCancel: () =>
    call<OrganizePlanStatus>("/organize/plan/cancel", {
      method: "POST",
      body: "{}",
    }),
  organizeApply: (
    token: string,
    moves: { container: string; from: string; to: string }[],
  ) =>
    call<OrganizeResult>("/organize/apply", {
      method: "POST",
      body: JSON.stringify({ token, moves }),
    }),
  organizeRuns: () => call<OrganizeRun[]>("/organize/runs"),
  organizeRevert: (id: string) =>
    call<{ reverted: OrganizeMove[]; problems: unknown[]; note?: string }>(
      "/organize/revert",
      { method: "POST", body: JSON.stringify({ id }) },
    ),
  junk: () => call<JunkStatus>("/junk"),
  junkScan: () =>
    call<JunkStatus>("/junk/scan", { method: "POST", body: "{}" }),
  junkCancel: () =>
    call<JunkStatus>("/junk/cancel", { method: "POST", body: "{}" }),
  junkClean: (token: string) =>
    call<{ removed: number; freed: number; failed: string[] }>("/junk/clean", {
      method: "POST",
      body: JSON.stringify({ token }),
    }),
  macos: () => call<MacSettings>("/macos"),
  macosFinder: (off: boolean) =>
    call<MacSettings>("/macos/finder", {
      method: "PUT",
      body: JSON.stringify({ off }),
    }),
  macosSpotlight: (volume: string, action: "off" | "remove_index") =>
    call<MacSettings>("/macos/spotlight", {
      method: "POST",
      body: JSON.stringify({ volume, action }),
    }),
  checks: () => call<{ checks: CopyCheck[] }>("/check"),
  check: (paths: string[]) =>
    call<{ checks: CopyCheck[] }>("/check", {
      method: "POST",
      body: JSON.stringify({ paths }),
    }),
  checkCancel: (path?: string) =>
    call<{ checks: CopyCheck[] }>("/check/cancel", {
      method: "POST",
      body: JSON.stringify({ path: path ?? null }),
    }),
  /** `version` (see coverVersion) makes a cover read again once its game changed. */
  coverUrl: (gameId: string, version?: string, size?: number) => {
    const q = new URLSearchParams();
    if (version) q.set("v", version);
    if (size) q.set("size", String(size));
    const query = q.toString();
    return `${getEngineUrl()}/api/collection/games/${encodeURIComponent(gameId)}/cover${query ? `?${query}` : ""}`;
  },
};
