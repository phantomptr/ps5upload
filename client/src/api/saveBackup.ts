// Automatic save backups, run by the engine (engine/crates/ps5upload-engine/src/save_backup.rs).

import { getEngineUrl } from "../state/engine";
import { ENGINE_LONG_TIMEOUT_MS, engineFetch, type EngineFetchInit } from "../lib/engineFetch";

export interface SaveBackupSettings {
  enabled: boolean;
  /** A folder the engine can write. */
  dir: string;
  keep: number;
  /** Minutes from the end of one automatic pass to the next, 5 to 1440. An engine before
   *  12.5.1 leaves it out (it backs up every 10 minutes). */
  interval_minutes?: number;
  /** Consoles to back up (bare IPs). */
  hosts: string[];
  /** Backup folders used before `dir`: their backups still list and restore. */
  old_dirs?: string[];
  /** A drive in the PS5 itself (`/mnt/ext0` M.2, `/mnt/usb0` USB) that also gets a copy of each
   *  backup, so saves outlive a reset of the internal storage; "" for none. An engine before
   *  12.6.5 leaves it out (it can't). */
  console_drive?: string;
  /** How backups are arranged in the folder and on the PS5 drive (#886). An engine before
   *  12.6.16 leaves it out (by game only). */
  layout?: BackupLayout;
  /** By user: a game's new folder carries the game's name. */
  game_names?: boolean;
  /** By user: each automatic backup of a PS5 save also leaves a decrypted copy. */
  decrypted_copy?: boolean;
  /** Folders on the PS5 backed up with the saves (#899). An engine before 12.6.20 leaves it out
   *  (it can't). */
  folders?: string[];
}

/** `by_game`: `<TITLE>/<file>`; `by_user`: `<name>_<id>/Encrypted/<PS5|PS4>/<TITLE>/<file>`. */
export type BackupLayout = "by_game" | "by_user";

/** What the decrypted copies made with the backups did. */
export interface DecryptedCopyStatus {
  at: number;
  made: number;
  failed: number;
  note: string | null;
}

/** What the last copies to the PS5 drive did. */
export interface ConsoleCopyStatus {
  /** Unix ms of the last copy (or skip). */
  at: number;
  drive: string;
  copied: number;
  skipped: number;
  failed: number;
  /** Why the last one was skipped or failed; null when they all went. */
  note: string | null;
}

/** A PS5 drive a copy of each backup can go to (never the internal storage). */
export interface ConsoleDrive {
  /** `/mnt/ext0`, `/mnt/usb1` … */
  path: string;
  /** `ext`: M.2 / extended storage; `usb`: a USB drive. */
  kind: "ext" | "usb";
  total_bytes: number;
  free_bytes: number;
}

export interface SaveBackupStatus {
  running: boolean;
  /** Unix ms of the last finished pass; 0 when none. */
  last_at: number;
  backed_up: number;
  failed: number;
  last_error: string | null;
  /** The last copies to the PS5 drive; null before any (absent from an older engine). */
  console_copy?: ConsoleCopyStatus | null;
  /** The last decrypted copies made with the backups; null before any. */
  decrypted?: DecryptedCopyStatus | null;
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

/** Takes or restores a backup before it answers. */
const LONG = { timeoutMs: ENGINE_LONG_TIMEOUT_MS };

async function call<T>(path: string, init?: EngineFetchInit): Promise<T> {
  const res = await engineFetch(`${getEngineUrl()}/api/save-backup${path}`, {
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
  /** A drive in the PS5 itself (`/mnt/ext0` M.2, `/mnt/usb0` USB) that also gets a copy of each
   *  backup (#885); "" for none. An engine before 12.6.16 leaves it out (it can't). */
  console_drive?: string;
  /** Versions of each save kept on that drive; 0: as many as `keep`. */
  console_keep?: number;
  /** How backups are arranged on the server and the PS5 drive (#886). */
  layout?: BackupLayout;
  game_names?: boolean;
  decrypted_copy?: boolean;
  /** Folders on the PS5 backed up with the saves (#899); absent from an older engine. */
  folders?: string[];
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
  /** The last copies to the PS5 drive; null before any (absent from an older engine). */
  console_copy?: ConsoleCopyStatus | null;
  /** The last decrypted copies made with the backups; null before any. */
  decrypted?: DecryptedCopyStatus | null;
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
  runNow: () => call<ServerBackupView>("/server/run", { ...LONG, method: "POST" }),
  list: async (titleId: string) =>
    (await call<{ backups: SaveBackupFile[] }>(`/server/list?title_id=${encodeURIComponent(titleId)}`)).backups,
  restore: (req: { addr: string; title_id: string; file: string; save_path: string; user_id: number }) =>
    call<{ ok: boolean }>("/server/restore", { ...LONG, method: "POST", body: JSON.stringify(req) }),
};

/** Which backup a run is: the network share's or the automatic backups to a folder. */
export type BackupWhich = "share" | "local";

/** A place a backup goes to. */
export type BackupPlace = "share" | "drive" | "local";

/** Where one save (or one place of it) stands in a run. */
export type BackupStep = "waiting" | "reading" | "copying" | "checking" | "verified" | "failed" | "skipped";

export interface BackupDestStatus {
  step: BackupStep;
  /** While copying: 0 to 100. */
  pct?: number;
  error: string | null;
  file: string | null;
  tries: number;
}

/** One save (or console folder) of a run. */
export interface BackupRunItem {
  /** `<console>|<save path>`: what Retry names. */
  id: string;
  host: string;
  /** `ps5`, `ps4`, `switch`, `ps2`, `ps1`, or `folder`. */
  kind: string;
  title_id: string;
  user_id: number;
  path: string;
  /** Bytes as the console lists the save (0: unknown). */
  size: number;
  mtime: number;
  step: BackupStep;
  /** Why it could not be read off the PS5. */
  error: string | null;
  tries: number;
  dests: Partial<Record<BackupPlace, BackupDestStatus>>;
}

export type BackupRunState = "running" | "done" | "stopped" | "cancelled" | "interrupted";

export interface BackupRun {
  id: string;
  which: BackupWhich | null;
  /** `manual`, `auto`, `interval`, `closed`, `continue`, `retry`. */
  trigger: string;
  started_at: number;
  updated_at: number;
  /** 0 while it runs. */
  ended_at: number;
  state: BackupRunState;
  /** `console_gone`, `interrupted`, `cancelled`; null when it went through. */
  reason_code: string | null;
  reason: string | null;
  items: BackupRunItem[];
  /** Consoles that could not be looked at, with why. */
  unreached: Record<string, string>;
}

export interface BackupPlaceCount {
  verified: number;
  failed: number;
  skipped: number;
  left: number;
  first_error: string | null;
}

export interface BackupRunSummary {
  id: string;
  trigger: string;
  started_at: number;
  ended_at: number;
  state: BackupRunState;
  reason_code: string | null;
  reason: string | null;
  total: number;
  verified: number;
  failed: number;
  skipped: number;
  left: number;
  read_failed: number;
  bytes_total: number;
  bytes_done: number;
  first_error: string | null;
  places: Partial<Record<BackupPlace, BackupPlaceCount>>;
}

/** `GET /api/save-backup/runs`: the last run whole and the last runs in short. An engine before
 *  12.6.20 has no such route (the calls throw). */
export interface BackupRunsView {
  run: BackupRun | null;
  summary: BackupRunSummary | null;
  history: BackupRunSummary[];
  /** The engine's clock (Unix ms) when it answered. */
  now: number;
}

export const saveBackupRunsApi = {
  view: (which: BackupWhich) => call<BackupRunsView>(`/runs?which=${which}`),
  cancel: (which: BackupWhich) =>
    call<{ cancelled: boolean }>("/runs/cancel", { method: "POST", body: JSON.stringify({ which }) }),
  /** Continue the last run (`ids` left out) or Retry those saves of it. */
  again: (which: BackupWhich, ids?: string[]) =>
    call<BackupRunsView>("/runs/continue", {
      method: "POST",
      body: JSON.stringify(ids ? { which, ids } : { which }),
    }),
};

/** Where folder backups are listed and restored from (#899). */
export type FolderBackupFrom = "share" | "local" | "drive";

export interface FolderBackup {
  file: string;
  /** `YYYY-MM-DD_HHMMSS` (UTC). */
  stamp: string;
  size: number;
  /** `beforerestore`: the copy kept before a restore replaced the folder. */
  tag: string | null;
  /** Made before folder backups were kept per PS5: listed for every PS5. */
  legacy?: boolean;
}

export const saveFoldersApi = {
  /** Each chosen console folder with its backups in `from`, newest first. */
  list: async (from: FolderBackupFrom, addr = "") =>
    (
      await call<{ folders: Array<{ path: string; backups: FolderBackup[] }> }>(
        `/folders?from=${from}&addr=${encodeURIComponent(addr)}`,
      )
    ).folders,
  /** Whether a console folder may be chosen: the path made plain, or why not. */
  check: (path: string) =>
    call<{ ok: boolean; path?: string; error?: string }>(`/folders/check?path=${encodeURIComponent(path)}`),
  /** Puts a folder backup back where it came from, keeping what is there first. */
  restore: (req: { from: FolderBackupFrom; addr: string; path: string; file: string }) =>
    call<{ ok: boolean }>("/folders/restore", { ...LONG, method: "POST", body: JSON.stringify(req) }),
};

/** A Switch emulator's per-game save. `user_id` keeps the profile's first 7 hex digits
 *  in its lower 28 bits; bit 28 separates EglantinePS5 backups from ProsperoEden. */
export interface SwitchSave {
  title_id: string;
  user_id: number;
  path: string;
  size: number;
  mtime: number;
  kind: "switch";
  /** Omitted by engines that only know ProsperoEden. */
  emulator?: string;
  /** This save's emulator runs now; null when the console did not say. */
  running: boolean | null;
}

/** A PlayStation 2 emulator's save (Crux2 PPSA99249, PS5SX2 PPSA99203; `kind` "ps2") or the
 *  PlayStation emulator PSSwanStation's (PPSA99248; `kind` "ps1"): `title_id` is the emulator's,
 *  and `user_id` tells the two apart (0 its memory cards, 1 its saved states). A save loose in the
 *  emulator's data folder has a `path` such as `/data/PCSX2/*.ps2`; `folder` is where its files
 *  are. */
export interface Ps2Save {
  title_id: string;
  user_id: number;
  path: string;
  size: number;
  mtime: number;
  kind: "ps2" | "ps1";
  emulator: string;
  folder: string;
  /** The emulator runs now; null when the console did not say. */
  running: boolean | null;
}

export const saveBackupApi = {
  view: () => call<SaveBackupView>(""),
  /** `add_hosts`/`remove_hosts` change the console list by what this client knows of its own
   *  roster: several clients can share one engine. */
  set: (patch: Partial<SaveBackupSettings> & { add_hosts?: string[]; remove_hosts?: string[] }) =>
    call<SaveBackupView>("", { method: "PUT", body: JSON.stringify(patch) }),
  runNow: () => call<SaveBackupView>("/run", { ...LONG, method: "POST" }),
  list: async (titleId: string) =>
    (await call<{ backups: SaveBackupFile[] }>(`/list?title_id=${encodeURIComponent(titleId)}`)).backups,
  restore: (req: { addr: string; title_id: string; file: string; save_path: string; user_id: number }) =>
    call<{ ok: boolean }>("/restore", { ...LONG, method: "POST", body: JSON.stringify(req) }),
  /** Put a backup `.zip` on this computer back, with the same refusals, backup first and putting
   *  back on failure as `restore`. */
  restoreFile: (req: { addr: string; title_id: string; zip: string; save_path: string; user_id: number }) =>
    call<{ ok: boolean }>("/restore-file", { ...LONG, method: "POST", body: JSON.stringify(req) }),
  /** What to write in a backup zip the app makes itself, so a restore can tell which PS5 and user
   *  it came from. "" when that can't be said: a backup is never held up for it. */
  note: async (addr: string, userId: number) =>
    (
      await call<{ note: string }>(
        `/note?addr=${encodeURIComponent(addr)}&user_id=${userId}`,
      ).catch(() => ({ note: "" }))
    ).note,
  /** Back up a game's save now, while it may be running: a copy taken only while the save sits
   *  still, kept apart from the automatic backups. */
  snapshot: (req: { addr: string; title_id: string }) =>
    call<{ ok: boolean; files: string[] }>("/snapshot", { ...LONG, method: "POST", body: JSON.stringify(req) }),
  /** Back up one user's save of a game now, every part of it (a snapshot while it runs). */
  now: (req: { addr: string; title_id: string; user_id: number }) =>
    call<{ ok: boolean; file: string }>("/now", { ...LONG, method: "POST", body: JSON.stringify(req) }),
  /** "Save as a file for a friend or another PS5" straight into the user's Decrypted folder of
   *  the backup folder (`local`) or the network share (`server`), #886. */
  exportDecrypted: (req: { addr: string; folder: string; to: "local" | "server" }) =>
    call<{ ok: boolean; path: string; file: string }>("/export", {
      ...LONG,
      method: "POST",
      body: JSON.stringify(req),
    }),
  /** The console's M.2 and USB drives a copy of each backup can go to. */
  consoleDrives: async (addr: string) =>
    (await call<{ drives: ConsoleDrive[] }>(`/console/drives?addr=${encodeURIComponent(addr)}`)).drives,
  /** A game's backup copies on the chosen PS5 drive of the console `addr` (bare IP). */
  consoleList: (addr: string, titleId: string) =>
    call<{ drive: string; kind: "ext" | "usb" | ""; backups: SaveBackupFile[] }>(
      `/console/list?addr=${encodeURIComponent(addr)}&title_id=${encodeURIComponent(titleId)}`,
    ),
  /** Put a backup copy on the PS5 drive back, as `restore` does (the save there now kept first). */
  consoleRestore: (req: { addr: string; title_id: string; file: string; save_path: string; user_id: number }) =>
    call<{ ok: boolean }>("/console/restore", { ...LONG, method: "POST", body: JSON.stringify(req) }),
  /** The newest backup of each game and user in the backup folder. */
  latest: async () =>
    (await call<{ latest: Array<{ title_id: string; user_id: number; stamp: string }> }>("/latest")).latest,
  /** Switch emulators' saves on a console (bare IP): none when neither has saved. */
  switchSaves: (addr: string) =>
    call<{ saves: SwitchSave[]; running: boolean | null }>(`/switch?addr=${encodeURIComponent(addr)}`),
  /** The PlayStation 2 emulators' and PSSwanStation's cards and saved states on a console (bare
   *  IP). */
  ps2Saves: (addr: string) => call<{ saves: Ps2Save[] }>(`/ps2?addr=${encodeURIComponent(addr)}`),
};
