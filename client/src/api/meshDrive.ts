// Mesh Drive (8.0.0, engine /api/drive*): a folder kept the same across devices, with its home on a
// console. An older helper answers {supported:false}.

import { getEngineUrl } from "../state/engine";
import { consoleAddr } from "../lib/addr";
import { EngineTooOldError } from "./mesh";

export interface DriveMember {
  id: string;
  device: string;
  device_name: string;
  path: string;
  /** Always true since 8.0.1 (joins need no approval); kept for older documents. */
  approved: boolean;
  paused: boolean;
  joined: number;
  // What that device's engine last reported to the console (0 / "" from an 8.0.0 engine).
  kind?: string;
  os?: string;
  app?: string;
  state?: string;
  error?: string;
  /** The engine's own sentences carry a code to word them with; a raw error has none. */
  error_code?: string;
  error_args?: Record<string, string>;
  files?: number;
  bytes?: number;
  last_sync?: number;
  reported?: number;
}

/** Another console holding this drive (8.1.0): the master of its own circle, kept the same as
 *  this one by an engine in the drive. */
export interface DriveLink {
  console: string;
  console_name: string;
  hub: string;
  created: number;
  state: string;
  error: string;
  error_code?: string;
  error_args?: Record<string, string>;
  last_sync: number;
  by_name: string;
  /** Devices in that console's circle. */
  circle: number;
  allow_deletes?: boolean;
}

export interface DriveStats {
  files: number;
  bytes: number;
  conflicts: number;
  updated: number;
}

export interface Drive {
  id: string;
  name: string;
  hub: string;
  created: number;
  by: string;
  members: DriveMember[];
  /** Present when joining asks for a password (only its PBKDF2 hash is kept). */
  lock?: { alg: string } | null;
  stats?: DriveStats | null;
  links?: DriveLink[];
}

export interface DriveStatus {
  state: string;
  /** English, for anything that has no words of its own. */
  detail: string;
  /** Says which of the engine's sentences `detail` is, for the user's language (none for a raw
   *  error). `detail_args` is what goes in it. */
  detail_code?: string;
  detail_args?: Record<string, string>;
  last_ok: number;
  files: number;
  uploaded: number;
  downloaded: number;
  deleted: number;
  conflicts: number;
  held_deletes: number;
  /** The first of the held deletes, by path. Absent from an older engine. */
  held_paths?: string[];
  bytes?: number;
  todo?: number;
  done?: number;
  current?: string;
  skipped?: string[];
  /** While the folder's own console is away (8.2.0): why, and since when (unix ms). */
  away?: "rest" | "unreachable" | "helper" | "";
  away_since?: number;
}

/** One of this device's folders, read from the engine alone (answers with every console away). */
export interface LocalFolder {
  drive: string;
  member: string;
  /** The drive's name when this folder joined it. */
  name: string;
  console: string;
  path: string;
  links: string[];
  status: DriveStatus;
}

/** This device's folders on every console (`GET /api/drive/local`); [] from an engine before 8.2.0. */
export async function fetchLocalFolders(): Promise<LocalFolder[]> {
  try {
    const b = await engineJson("/api/drive/local");
    return Array.isArray(b.folders) ? (b.folders as LocalFolder[]) : [];
  } catch {
    return [];
  }
}

export interface LocalDriveMember {
  drive: string;
  member: string;
  path: string;
  status: DriveStatus;
}

export type DrivesAnswer =
  | { supported: false; engineOld?: boolean }
  | {
      supported: true;
      me: string | null;
      kind: string;
      suggest: string | null;
      drives: Drive[];
      local: LocalDriveMember[];
      /** Usable bytes where each drive's home is, by drive id. */
      free: Record<string, number>;
      /** Drives here this device keeps through another (linked) console. */
      elsewhere: { drive: string; console: string; path: string }[];
    };

async function engineJson(route: string, init?: RequestInit): Promise<Record<string, unknown>> {
  const res = await fetch(`${getEngineUrl()}${route}`, { ...init, signal: AbortSignal.timeout(60_000) });
  const body = (await res.json().catch(() => ({}))) as Record<string, unknown>;
  // A route this engine does not have. (A 404 with words is "that drive is gone".)
  if (res.status === 404 && typeof body.error !== "string") throw new EngineTooOldError();
  if (!res.ok) {
    throw new Error(typeof body.error === "string" && body.error ? body.error : `request failed (${res.status})`);
  }
  return body;
}

const post = (route: string, body: object) =>
  engineJson(route, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });

export async function fetchDrives(host: string): Promise<DrivesAnswer> {
  let b: Record<string, unknown>;
  try {
    b = await engineJson(`/api/drive?addr=${encodeURIComponent(consoleAddr(host))}`);
  } catch (e) {
    if (e instanceof EngineTooOldError) return { supported: false, engineOld: true };
    throw e;
  }
  if (b.supported !== true) return { supported: false };
  return {
    supported: true,
    me: typeof b.me === "string" ? b.me : null,
    kind: typeof b.kind === "string" ? b.kind : "",
    suggest: typeof b.suggest === "string" ? b.suggest : null,
    drives: Array.isArray(b.drives) ? (b.drives as Drive[]) : [],
    local: Array.isArray(b.local) ? (b.local as LocalDriveMember[]) : [],
    free: b.free && typeof b.free === "object" ? (b.free as Record<string, number>) : {},
    elsewhere: Array.isArray(b.elsewhere) ? (b.elsewhere as { drive: string; console: string; path: string }[]) : [],
  };
}

const call = (route: string) => async (host: string, body: object) => {
  const b = await post(route, { addr: consoleAddr(host), ...body });
  if (b.supported !== true) throw new Error("Update the console's helper to use Mesh Drive.");
  return b;
};

/** What a folder choice would mean (engine `POST /api/drive/check`). */
export interface FolderCheck {
  path: string;
  ok: boolean;
  exists: boolean;
  will_create: boolean;
  files: number;
  more: boolean;
  marker: boolean;
  /** bad_name, not_absolute, no_parent, not_folder, exists, taken, inside, holds, app_data, too_broad,
   *  not_writable, unreadable. */
  problem: string | null;
  detail: string | null;
}

export interface HubChoice {
  /** The folder itself, or (with `name`) the folder a new one is made in. */
  path: string;
  name?: string;
}

export async function checkFolders(
  host: string,
  q: { hub?: HubChoice; path?: string; drive?: string; console?: string },
): Promise<{ hub: FolderCheck | null; local: FolderCheck | null }> {
  const b = await post("/api/drive/check", {
    addr: consoleAddr(host),
    hub: q.hub?.path,
    hub_name: q.hub?.name,
    path: q.path,
    drive: q.drive,
    console: q.console ? consoleAddr(q.console) : undefined,
  });
  return { hub: (b.hub as FolderCheck | null) ?? null, local: (b.local as FolderCheck | null) ?? null };
}

export const driveApi = {
  create: (host: string, name: string, hub: HubChoice, path: string, password?: string) =>
    call("/api/drive/create")(host, { name, hub: hub.path, hub_name: hub.name, path, password: password || undefined }),
  join: (host: string, drive: string, path: string, password?: string) =>
    call("/api/drive/join")(host, { drive, path, password: password || undefined }),
  /** Sets, changes or (empty) removes the password. `current` for a device not in the drive.
   *  `pending`: linked consoles that did not answer; each takes it from the next link pass. */
  setPassword: async (host: string, drive: string, password: string, current?: string) => {
    const b = await call("/api/drive/password")(host, { drive, password, current: current || undefined });
    return { pending: Array.isArray(b.pending) ? (b.pending as string[]) : [] };
  },
  leave: (host: string, drive: string, member: string) => call("/api/drive/leave")(host, { drive, member }),
  remove: (host: string, drive: string) => call("/api/drive/delete")(host, { drive }),
  pause: (host: string, drive: string, member: string, paused: boolean) =>
    call("/api/drive/pause")(host, { drive, member, paused }),
  sync: (host: string, drive: string) => call("/api/drive/sync")(host, { drive }),
  /** Links the drive with another console (its home there: a folder, or a new one in `hub`). */
  link: (host: string, drive: string, other: string, hub: HubChoice, name: string, hereName: string) =>
    call("/api/drive/link")(host, {
      drive,
      console: consoleAddr(other),
      hub: hub.path,
      hub_name: hub.name,
      name,
      here_name: hereName,
    }),
  unlink: (host: string, drive: string, other: string) =>
    call("/api/drive/unlink")(host, { drive, console: consoleAddr(other) }),
  /** Lets a mass delete held back on a link run. */
  allowLinkDeletes: (host: string, drive: string, other: string) =>
    call("/api/drive/unlink")(host, { drive, console: consoleAddr(other), allow_deletes: true }),
  resolve: (host: string, drive: string, copy: string, action: ResolveAction, name?: string) =>
    call("/api/drive/resolve")(host, { drive, copy, action, name: name || undefined }),
  confirm: (host: string, drive: string, member: string) => call("/api/drive/confirm")(host, { drive, member }),
};

/** "ps5://192.168.1.5/data/x" -> "/data/x". */
export function consolePathOf(picked: string): string {
  if (!picked.startsWith("ps5://")) return picked;
  const rest = picked.slice("ps5://".length);
  const i = rest.indexOf("/");
  return i < 0 ? "/" : rest.slice(i);
}

/** The engine's refusal code ("wrong_password: ...") of a failed call, if any. */
export function refusalCode(e: unknown): string | null {
  const m = e instanceof Error ? /^(\w+):/.exec(e.message) : null;
  return m ? m[1] : null;
}

/** One conflict copy in a drive, with the file it is a copy of (8.1.0). */
export interface DriveConflict {
  copy: string;
  original: string;
  /** The device that made the copy (its changes lost the race). */
  device: string;
  /** "YYYY-MM-DD HHMM" (UTC). */
  stamp: string;
  copy_size: number;
  copy_mtime: number;
  /** null when the original is gone. */
  original_size: number | null;
  original_mtime: number | null;
}

export type ResolveAction = "keep_copy" | "keep_original" | "keep_both";

export async function fetchConflicts(host: string, drive: string): Promise<DriveConflict[]> {
  const b = await engineJson(
    `/api/drive/conflicts?addr=${encodeURIComponent(consoleAddr(host))}&drive=${encodeURIComponent(drive)}`,
  );
  return Array.isArray(b.conflicts) ? (b.conflicts as DriveConflict[]) : [];
}
