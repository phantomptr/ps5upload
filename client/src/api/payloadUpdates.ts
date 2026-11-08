// Payload updates and the Payload Manager sync, through the engine's /api/payloads/updates and
// /api/payloads/pm-sync routes (engine payload_updates_api.rs). Plain fetches against the engine
// origin, so the desktop app, the browser build and the Docker build take the same path.

import { getEngineUrl } from "../state/engine";
import { hostOf } from "../lib/addr";

export interface PayloadUpdateRow {
  payload_id: string;
  display_name: string;
  is_custom: boolean;
  /** "" when it is not downloaded yet. */
  installed_version: string;
  /** "" when the source could not be read. */
  latest_version: string;
  update_available: boolean;
  /** The version the last update replaced, kept for a rollback. */
  previous_version?: string;
  prerelease: boolean;
  published_at: string;
  html_url: string;
  error?: string;
}

export interface AppliedPayloadUpdate {
  payload_id: string;
  display_name: string;
  from: string;
  to: string;
  at: number;
  rollback: boolean;
}

export type PmChange =
  | { kind: "stored"; payload_id: string; filename: string; from?: string; to: string }
  | { kind: "unchanged"; payload_id: string; filename: string; version: string }
  | { kind: "skipped"; payload_id: string; code: string; reason: string }
  | { kind: "failed"; payload_id: string; error: string }
  | { kind: "autoload_added"; payload_id: string; entry: string }
  | { kind: "autoload_renamed"; payload_id: string; from: string; to: string }
  | { kind: "autoload_removed"; payload_id: string; entry: string }
  | { kind: "removed_old"; payload_id: string; filename: string };

export interface PmSyncReport {
  host: string;
  at: number;
  present: boolean;
  pm_version: string;
  message?: string;
  changes: PmChange[];
  autoload_before: string[];
  autoload_after: string[];
}

export interface PayloadUpdateState {
  checked_at: number;
  rows: PayloadUpdateRow[];
  rate_limited: boolean;
  applied: AppliedPayloadUpdate[];
  available_seq: number;
  applied_seq: number;
  last_auto_run_at: number;
  pm_syncs: Record<string, PmSyncReport>;
}

export interface PayloadUpdateSettings {
  auto_update: boolean;
}

export interface PmSyncChoice {
  ids: string[];
  autoload_ids: string[];
}

export interface ApplyResult {
  updated: { payload_id: string; display_name: string; updated: boolean; from: string; to: string }[];
  errors: { payload_id: string; code: string; error: string }[];
  pm_sync?: PmSyncReport;
}

async function call<T>(method: string, path: string, body?: unknown, timeoutMs = 30_000): Promise<T> {
  const res = await fetch(`${getEngineUrl()}${path}`, {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(timeoutMs),
  });
  const v = (await res.json().catch(() => ({}))) as T & { error?: string };
  if (!res.ok) throw new Error(v.error || `request failed (${res.status})`);
  return v;
}

/** The last check, what was applied and the settings. Asks no network. */
export function payloadUpdatesOverview(): Promise<{
  settings: PayloadUpdateSettings;
  state: PayloadUpdateState;
}> {
  return call("GET", "/api/payloads/updates");
}

export function checkPayloadUpdates(force: boolean): Promise<PayloadUpdateState> {
  return call("POST", "/api/payloads/updates/check", { force }, 120_000);
}

/** Update `ids` (all available when absent), then sync `console`'s Payload Manager if given. */
export function applyPayloadUpdates(ids?: string[], console?: string): Promise<ApplyResult> {
  return call(
    "POST",
    "/api/payloads/updates/apply",
    { ids, console: console ? hostOf(console) : undefined },
    600_000,
  );
}

export function rollbackPayload(id: string): Promise<{ from: string; to: string }> {
  return call("POST", `/api/payloads/${encodeURIComponent(id)}/rollback`);
}

export function setPayloadUpdateSettings(s: PayloadUpdateSettings): Promise<PayloadUpdateSettings> {
  return call("PUT", "/api/payloads/updates/settings", s);
}

export function pmSyncChoice(host: string): Promise<PmSyncChoice> {
  return call("GET", `/api/payloads/pm-sync?host=${encodeURIComponent(hostOf(host))}`);
}

export function setPmSyncChoice(host: string, choice: PmSyncChoice): Promise<PmSyncChoice> {
  return call("PUT", "/api/payloads/pm-sync", { host: hostOf(host), ...choice });
}

export function runPmSync(host: string): Promise<PmSyncReport> {
  return call("POST", "/api/payloads/pm-sync/run", { host: hostOf(host) }, 300_000);
}
