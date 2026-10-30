// Automatic save backups, run by the engine (engine/crates/ps5upload-engine/src/save_backup.rs).

import { getEngineUrl } from "../state/engine";

export interface SaveBackupSettings {
  enabled: boolean;
  /** A folder the engine can write. */
  dir: string;
  keep: number;
  /** Consoles to back up (bare IPs). */
  hosts: string[];
}

export interface SaveBackupStatus {
  running: boolean;
  /** Unix ms of the last finished pass; 0 when none. */
  last_at: number;
  backed_up: number;
  failed: number;
  last_error: string | null;
}

export interface SaveBackupView {
  settings: SaveBackupSettings;
  status: SaveBackupStatus;
}

export interface SaveBackupFile {
  file: string;
  title_id: string;
  user_id: number;
  /** `YYYY-MM-DD_HHMMSS` (UTC for backups the engine made), `_snap` after it for a snapshot. */
  stamp: string;
  size: number;
  /** `snap`: taken while the game ran (Play › Game › Snapshot). */
  tag?: string;
}

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${getEngineUrl()}/api/save-backup${path}`, {
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
    throw new Error((body as { error?: string } | null)?.error ?? `HTTP ${res.status}`);
  }
  return body as T;
}

export const saveBackupApi = {
  view: () => call<SaveBackupView>(""),
  set: (patch: Partial<SaveBackupSettings>) =>
    call<SaveBackupView>("", { method: "PUT", body: JSON.stringify(patch) }),
  runNow: () => call<SaveBackupView>("/run", { method: "POST" }),
  list: async (titleId: string) =>
    (await call<{ backups: SaveBackupFile[] }>(`/list?title_id=${encodeURIComponent(titleId)}`)).backups,
  restore: (req: { addr: string; title_id: string; file: string; save_path: string; user_id: number }) =>
    call<{ ok: boolean }>("/restore", { method: "POST", body: JSON.stringify(req) }),
  /** Back up a game's save now, while it may be running: a copy taken only while the save sits
   *  still, kept apart from the automatic backups. */
  snapshot: (req: { addr: string; title_id: string }) =>
    call<{ ok: boolean; files: string[] }>("/snapshot", { method: "POST", body: JSON.stringify(req) }),
};
