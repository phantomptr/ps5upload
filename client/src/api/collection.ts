// The Collection: games kept on this computer's drives, indexed by the engine
// (engine/crates/ps5upload-engine/src/collection). Same field names as PS Game Library's index.

import { getEngineUrl } from "../state/engine";
import { invoke } from "../lib/invokeLogged";
import { getCachedIcon, setCachedIcon } from "../lib/iconMemoryCache";

const coverKey = (gameId: string) => `collection|${gameId}`;

/** A Collection cover this session already holds, as a `data:` URL. */
export function cachedCollectionCover(gameId: string): string | undefined {
  return getCachedIcon(coverKey(gameId));
}

/** A Collection cover over the IPC, as a `data:` URL, or null. The desktop window's own load
 *  of the cover URL is refused by the engine's cross-site guard (WebKit sends neither Origin
 *  nor Referer for it), so this is the fallback `useImageRetry` turns to. */
export async function collectionCoverDataUrl(gameId: string): Promise<string | null> {
  const hit = getCachedIcon(coverKey(gameId));
  if (hit) return hit;
  try {
    const url = await invoke<string>("collection_cover_data", { gameId });
    if (typeof url === "string" && url.startsWith("data:")) {
      setCachedIcon(coverKey(gameId), url);
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
  error?: string;
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
  ps_game_library_found?: boolean;
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
  moves: OrganizeMove[];
  skipped: OrganizeSkipped[];
  unsure_count: number;
  total_bytes: number;
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
  created_at: string;
  moved: number;
  failed: number;
  reverted_at: string | null;
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

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${getEngineUrl()}/api/collection${path}`, {
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
    const msg =
      (body as { error?: string } | null)?.error ?? `HTTP ${res.status}`;
    throw new Error(msg);
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
  importPsGameLibrary: (path?: string) =>
    call<{ ok: boolean; summary: CollectionSummary }>("/import", {
      method: "POST",
      body: JSON.stringify(path ? { path } : {}),
    }),
  /** The export's text (json | csv | md). */
  exportText: async (format: "json" | "csv" | "md"): Promise<string> => {
    const res = await fetch(
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
      method: "POST",
      body: JSON.stringify({ token }),
    }),
  organizePlan: () =>
    call<OrganizePlan>("/organize/plan", { method: "POST", body: "{}" }),
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
    call<{ reverted: OrganizeMove[]; problems: unknown[] }>(
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
  /** `version` (see coverVersion) makes a cover read again once its game changed. */
  coverUrl: (gameId: string, version?: string) =>
    `${getEngineUrl()}/api/collection/games/${encodeURIComponent(gameId)}/cover${
      version ? `?v=${encodeURIComponent(version)}` : ""
    }`,
};
