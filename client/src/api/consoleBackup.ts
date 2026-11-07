// Backup & Restore (engine/crates/ps5upload-engine/src/console_backup.rs, #707).

import { getEngineUrl } from "../state/engine";
import { engineFetch } from "../lib/engineFetch";

export type BackupGroupId = "saves" | "captures" | "settings" | "homebrew" | "other";

export interface BackupGroup {
  id: BackupGroupId;
  bytes: number;
  /** Files, or saves for the saves group. */
  items: number;
  folders: { path: string; bytes: number; files: number }[];
}

export interface BackupOutcome {
  group: BackupGroupId;
  /** The console path, or a game's title id. */
  item: string;
  result: "done" | "failed" | "needs_save";
  detail?: string;
}

export interface BackupStatus {
  state: "idle" | "scanning" | "backing_up" | "restoring" | "done" | "cancelled" | "failed";
  /** What this run is or was: "scanning", "backing_up" or "restoring". */
  kind: string;
  host: string;
  current: string;
  step: number;
  steps: number;
  bytes: number;
  total_bytes: number;
  groups: BackupGroup[];
  dir: string;
  outcomes: BackupOutcome[];
  error: string | null;
}

export interface BackupManifest {
  format: number;
  app_version: string;
  made_at_ms: number;
  host: string;
  complete: boolean;
  groups: BackupGroup[];
  saves: { title_id: string; user_id: number; path: string; file: string; bytes: number }[];
  users: { uid: number; name: string }[];
  failed: BackupOutcome[];
}

async function call<T>(path: string, body?: unknown): Promise<T> {
  const res = await engineFetch(`${getEngineUrl()}/api/console-backup${path}`, {
    method: body === undefined ? "GET" : "POST",
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let parsed: unknown;
  try {
    parsed = text ? JSON.parse(text) : null;
  } catch {
    parsed = null;
  }
  if (!res.ok) throw new Error((parsed as { error?: string } | null)?.error ?? `HTTP ${res.status}`);
  return parsed as T;
}

export const consoleBackupApi = {
  status: () => call<BackupStatus>("/status"),
  scan: (addr: string) => call<{ ok: boolean }>("/scan", { addr }),
  backup: (addr: string, dir: string, groups: BackupGroupId[]) =>
    call<{ ok: boolean }>("/backup", { addr, dir, groups }),
  inspect: (dir: string) => call<BackupManifest>("/inspect", { dir }),
  restore: (addr: string, dir: string, groups: BackupGroupId[], users: Record<string, number>) =>
    call<{ ok: boolean }>("/restore", { addr, dir, groups, users }),
  cancel: () => call<{ ok: boolean }>("/cancel", {}),
};

/** What's in a backup the restore can offer: groups with something in them. */
export function groupsInBackup(m: BackupManifest): BackupGroupId[] {
  const ids = m.groups.filter((g) => g.folders.length > 0).map((g) => g.id);
  if (m.saves.length > 0) ids.unshift("saves");
  return ids;
}

/** The users a backup's saves belong to. */
export function usersWithSaves(m: BackupManifest): { uid: number; name: string }[] {
  const seen = new Set<number>();
  const out: { uid: number; name: string }[] = [];
  for (const s of m.saves) {
    if (seen.has(s.user_id)) continue;
    seen.add(s.user_id);
    out.push({ uid: s.user_id, name: m.users.find((u) => u.uid === s.user_id)?.name ?? "" });
  }
  return out;
}

/** First guess at who a backup's user is here: the same number, then the same name. */
export function matchUser(
  from: { uid: number; name: string },
  here: { uid: number; username: string }[],
): number | null {
  const same = here.find((u) => u.uid === from.uid);
  if (same) return same.uid;
  const name = from.name.trim().toLowerCase();
  const named = name ? here.find((u) => u.username.trim().toLowerCase() === name) : undefined;
  return named ? named.uid : null;
}
