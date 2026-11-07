/**
 * Console clean-up (spec 2026-10-22-console-cleanup-design.md): the helper scans the console for
 * what can safely be removed and removes the items picked. These calls go straight to the engine
 * (desktop, browser and Docker alike), which forwards them to the helper.
 */
import { getEngineUrl } from "../state/engine";
import { mgmtAddr } from "../lib/addr";

/** A: update of an installed game; B: update of a game that is gone; C: leftover package file;
 *  D: ps5upload temporary files; E: console download file of a removed game; F: leftovers of a
 *  removed game; G: worth a look (never removed here). */
export type CleanupCategory = "A" | "B" | "C" | "D" | "E" | "F" | "G";

export interface CleanupItem {
  id: number;
  cat: CleanupCategory;
  path: string;
  title_id?: string;
  /** The title's name from the console's app list, when it is installed. */
  name?: string;
  /** internal, ext0, ext1, usb0… */
  drive: string;
  /** Why it is listed (a token; see cleanupModel.stateText). */
  state: string;
  /** For packages and updates: base, patch or dlc. */
  kind?: string;
  version?: string;
  installed_version?: string;
  /** A: where the game itself is (internal, ext0, usb0…). */
  game_drive?: string;
  /** A: the game runs from a backup (a folder or image registered on the console). */
  game_backup?: boolean;
  bytes: number;
  /** Space it takes on the drive (what removing it frees). */
  alloc_bytes: number;
  files: number;
  is_dir: boolean;
  mtime: number;
  /** Ticked when the list first shows. */
  default: boolean;
  /** sony: Sony's own update removal; delete: the files are deleted; none: listed only. */
  action: "sony" | "delete" | "none";
}

export interface CleanupScan {
  ok: boolean;
  scan_id: number;
  state: "running" | "done" | "cancelled" | "failed";
  phase: string;
  age_ms: number;
  /** app.db could be read (otherwise nothing that depends on a game being gone is listed). */
  appdb: boolean;
  /** This firmware can remove updates (sceAppInstUtilAppUnInstallPat). */
  can_remove_updates: boolean;
  install_running: boolean;
  found: number;
  truncated: boolean;
  reserve_bytes: number;
  free_bytes: number;
  total_bytes: number;
  /** Once the scan is done. */
  items?: CleanupItem[];
}

export interface CleanupApplyStatus {
  ok: boolean;
  apply_id: number;
  scan_id: number;
  state: "running" | "done" | "cancelled";
  /** Item id being removed now, 0 for none. */
  current: number;
  phase: string;
  total: number;
  done: number[];
  skipped: { id: number; reason: string; code?: string }[];
  failed: { id: number; reason: string; code?: string }[];
  freed_bytes: number;
}

/** A refusal from the engine or helper: `reason` is the token (busy, install_running, …). */
export class CleanupError extends Error {
  readonly reason: string;
  readonly status: number;
  constructor(message: string, reason: string, status: number) {
    super(message);
    this.name = "CleanupError";
    this.reason = reason;
    this.status = status;
  }
}

/** A status read or a Stop: the helper answers these at once, even while it waits on Sony. One
 *  that hangs means the helper is gone, and the follower should hear so soon (#714). */
const QUICK_TIMEOUT_MS = 15_000;

async function request<T>(method: "GET" | "POST", path: string, body?: unknown, timeoutMs = 60_000): Promise<T> {
  const res = await fetch(`${getEngineUrl()}${path}`, {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(timeoutMs),
  });
  const json = (await res.json().catch(() => ({}))) as T & { error?: string; reason?: string };
  if (!res.ok) {
    throw new CleanupError(json.error || `HTTP ${res.status}`, json.reason || "failed", res.status);
  }
  return json;
}

export function cleanupScanStart(host: string): Promise<{ scan_id: number }> {
  return request("POST", "/api/ps5/cleanup/scan", { addr: mgmtAddr(host) });
}

export function cleanupScanStatus(host: string, scanId: number): Promise<CleanupScan> {
  return request("GET", `/api/ps5/cleanup/scan/${scanId}?addr=${encodeURIComponent(mgmtAddr(host))}`);
}

export function cleanupApply(host: string, scanId: number, itemIds: number[]): Promise<{ apply_id: number }> {
  return request("POST", `/api/ps5/cleanup/scan/${scanId}/apply`, {
    addr: mgmtAddr(host),
    item_ids: itemIds,
  });
}

export function cleanupApplyStatus(host: string, applyId: number): Promise<CleanupApplyStatus> {
  return request(
    "GET",
    `/api/ps5/cleanup/apply/${applyId}?addr=${encodeURIComponent(mgmtAddr(host))}`,
    undefined,
    QUICK_TIMEOUT_MS,
  );
}

export function cleanupCancel(host: string, id: number): Promise<unknown> {
  return request("POST", "/api/ps5/cleanup/cancel", { addr: mgmtAddr(host), id }, QUICK_TIMEOUT_MS);
}
