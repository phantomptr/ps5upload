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

/** Backups to a network share (engine save_backup_server.rs, #592). */
export type ServerBackupMode = "off" | "interval" | "after_close";

export interface ServerBackupSettings {
  mode: ServerBackupMode;
  /** `remote://<connection-id>/<folder>`; "" when none is chosen. */
  target: string;
  every_hours: number;
  keep: number;
  /** Consoles to back up (bare IPs); empty: every console. */
  hosts: string[];
  /** Console users to back up; empty: every user. */
  users: number[];
}

export interface ServerBackupVerified {
  title_id: string;
  file: string;
  size: number;
  sha256: string;
  entries: number;
}

export interface ServerBackupStatus {
  running: boolean;
  /** Unix ms of the last pass that backed something up or failed; 0 when none. */
  last_at: number;
  last_trigger: "" | "interval" | "closed" | "manual";
  backed_up: number;
  verified: number;
  failed: number;
  last_error: string | null;
  last_verified: ServerBackupVerified | null;
  /** Unix ms of the last time the consoles were looked at. */
  last_check_at: number;
}

export interface ServerBackupView {
  settings: ServerBackupSettings;
  status: ServerBackupStatus;
  /** Interval mode: Unix ms the next pass is due (0: now); null in the other modes. */
  next_at: number | null;
}

export const serverBackupApi = {
  view: () => call<ServerBackupView>("/server"),
  set: (patch: Partial<ServerBackupSettings>) =>
    call<ServerBackupView>("/server", { method: "PUT", body: JSON.stringify(patch) }),
  runNow: () => call<ServerBackupView>("/server/run", { method: "POST" }),
  list: async (titleId: string) =>
    (await call<{ backups: SaveBackupFile[] }>(`/server/list?title_id=${encodeURIComponent(titleId)}`)).backups,
  restore: (req: { addr: string; title_id: string; file: string; save_path: string; user_id: number }) =>
    call<{ ok: boolean }>("/server/restore", { method: "POST", body: JSON.stringify(req) }),
};

/** A save of ProsperoEden, the Switch emulator (PPSA99008): `title_id` is the game's 16-hex-digit
 *  title ID and `user_id` its profile's first 7 hex digits (0 for the console's own save). */
export interface SwitchSave {
  title_id: string;
  user_id: number;
  path: string;
  size: number;
  mtime: number;
  kind: "switch";
  /** ProsperoEden runs now; null when the console did not say. */
  running: boolean | null;
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
  /** ProsperoEden's saves on a console (bare IP): none when it is not installed. */
  switchSaves: (addr: string) =>
    call<{ saves: SwitchSave[]; running: boolean | null }>(`/switch?addr=${encodeURIComponent(addr)}`),
};
