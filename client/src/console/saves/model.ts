/**
 * Save backups on the console itself (Discord request): copy a game's save of one user, as the
 * console keeps it, to /data/ps5upload/save-backups/<user>/<title>/<YYYY-MM-DD_HH-MM-SS>/, keep as
 * many as wanted, and put one back later (before a game's final choice, say).
 *
 * A PS5 game's save is two folders (measured on 13.60): /user/home/<user>/savedata_prospero/<title>
 * (the sealed save images: sdimg_<name>, and the system's own sdimg_sce_bu_<name> copies) and
 * savedata_prospero_meta/user/<title> (each save's <name>.sfo details and icon); a PS4 game's are
 * savedata/<title> and savedata_meta/user/<title>. A backup holds each one there was, in a folder
 * named after its kind. The images stay sealed: a backup is put back on the same console and user only
 * (what the PC app's Save data screen does too, by download and upload).
 *
 * Restore replaces what is there with the backup's files: it first backs up the current save
 * (named with "_auto"), then empties each live folder and copies the backup's files in. The
 * live title folder itself is kept; when it is gone (the save was deleted) it is created, which
 * the system's save folder may refuse (the PC app's restore notes EACCES there): the game then
 * has to save once first. The game must not be running (the caller checks).
 *
 * Backups can also go to a USB drive (#528): the same layout under <drive>/ps5upload/save-backups,
 * so a stick carries them off the console and back. `root` picks the place; the safety backup a
 * restore makes always goes to the internal drive, which cannot be unplugged halfway.
 */
import { ApiError, call } from "../api";
import { watchOp } from "../files/opJob";
import { TITLE_ID_RE, type CallFn } from "../games/model";

export const BACKUP_ROOT = "/data/ps5upload/save-backups";
/** Where backups go on a USB drive mounted at `mount` (/mnt/usb0, /mnt/ext1). */
export const usbBackupRoot = (mount: string) => `${mount}/ps5upload/save-backups`;
/** The save folders a title can have, in the order a backup copies them. */
export const SAVE_KINDS = ["savedata_prospero", "savedata_prospero_meta", "savedata", "savedata_meta"] as const;
export type SaveKind = (typeof SAVE_KINDS)[number];

const LIST_PAGE = 256;
const HEX_USER = /^[0-9a-f]{8}$/;
const STAMP = /^(\d{4})-(\d{2})-(\d{2})_(\d{2})-(\d{2})-(\d{2})(_auto)?$/;

export interface SaveUser {
  /** The home folder's name: the user id as 8 hex digits. */
  id: string;
  name: string;
  foreground: boolean;
}

export interface SaveTitle {
  titleId: string;
  kinds: SaveKind[];
}

export interface Backup {
  /** The folder name (its stamp). */
  name: string;
  /** When it was made (local time, ms), from its name. */
  when: number;
  /** Made by a restore, of the save it replaced. */
  auto: boolean;
}

interface ListEntry {
  name?: unknown;
  kind?: unknown;
  size?: unknown;
}

/** `path`'s entries (every page); null when the folder is not there. */
export async function listAll(callFn: CallFn, path: string): Promise<{ name: string; kind: string; size: number }[] | null> {
  const out: { name: string; kind: string; size: number }[] = [];
  let offset = 0;
  for (;;) {
    let r: { entries?: ListEntry[]; more?: boolean; next?: number } | null;
    try {
      r = await callFn("/api/fs/list", { path, offset, limit: LIST_PAGE });
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) return null;
      throw e;
    }
    const entries = Array.isArray(r?.entries) ? r.entries : [];
    for (const e of entries) {
      if (typeof e?.name === "string")
        out.push({ name: e.name, kind: typeof e.kind === "string" ? e.kind : "other", size: typeof e.size === "number" ? e.size : 0 });
    }
    if (!r?.more || !entries.length) return out;
    offset = typeof r.next === "number" && r.next > offset ? r.next : offset + entries.length;
  }
}

const dirsOf = (l: { name: string; kind: string }[] | null) => (l ?? []).filter((e) => e.kind === "dir" || e.kind === "link").map((e) => e.name);

export const userDir = (user: string) => `/user/home/${user}`;
const isMeta = (kind: SaveKind) => kind === "savedata_prospero_meta" || kind === "savedata_meta";
/** The folder holding `kind`'s title folders: the meta ones keep theirs one level down, in user/. */
const kindRoot = (user: string, kind: SaveKind) => `${userDir(user)}/${kind}${isMeta(kind) ? "/user" : ""}`;
export const livePath = (user: string, kind: SaveKind, titleId: string) => `${kindRoot(user, kind)}/${titleId}`;
export const backupsDir = (user: string, titleId: string, root = BACKUP_ROOT) => `${root}/${user}/${titleId}`;

/** /api/users' reply → name by home folder (id as 8 hex digits). */
export function parseUsers(raw: unknown): Map<string, { name: string; foreground: boolean }> {
  const out = new Map<string, { name: string; foreground: boolean }>();
  const list = Array.isArray((raw as { users?: unknown } | null)?.users) ? (raw as { users: Record<string, unknown>[] }).users : [];
  for (const u of list) {
    if (typeof u?.id !== "number" || !Number.isFinite(u.id)) continue;
    const id = (u.id >>> 0).toString(16).padStart(8, "0");
    out.set(id, { name: typeof u.name === "string" ? u.name : "", foreground: u.foreground === true });
  }
  return out;
}

/** The users with a home folder, named where the helper can say (an older helper has no /api/users). */
export async function loadUsers(callFn: CallFn = call): Promise<SaveUser[]> {
  const homes = dirsOf(await listAll(callFn, "/user/home")).filter((n) => HEX_USER.test(n));
  let names = new Map<string, { name: string; foreground: boolean }>();
  try {
    names = parseUsers(await callFn<unknown>("/api/users"));
  } catch {
    // unnamed: the id is shown
  }
  const users = homes.map((id) => ({ id, name: names.get(id)?.name || id, foreground: names.get(id)?.foreground ?? false }));
  // users the system lists first, in its order; then the rest
  const order = Array.from(names.keys());
  const rank = (id: string) => (order.indexOf(id) < 0 ? order.length : order.indexOf(id));
  return users.sort((a, b) => rank(a.id) - rank(b.id) || a.id.localeCompare(b.id));
}

/** Every title with a save of `user`, and which save folders it has. */
export async function loadSaves(user: string, callFn: CallFn = call): Promise<SaveTitle[]> {
  const by = new Map<string, SaveKind[]>();
  for (const kind of SAVE_KINDS) {
    const l = await listAll(callFn, kindRoot(user, kind));
    for (const id of dirsOf(l)) {
      if (!TITLE_ID_RE.test(id)) continue;
      const k = by.get(id);
      if (k) k.push(kind);
      else by.set(id, [kind]);
    }
  }
  // a meta folder alone (the save itself gone) is nothing to back up
  return Array.from(by.entries())
    .filter(([, kinds]) => kinds.some((k) => !isMeta(k)))
    .map(([titleId, kinds]) => ({ titleId, kinds }));
}

const pad = (n: number) => String(n).padStart(2, "0");

/** A backup's folder name for `d` (local time), "_auto" for the one a restore makes. */
export function backupName(d: Date, auto = false): string {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}_${pad(d.getHours())}-${pad(d.getMinutes())}-${pad(d.getSeconds())}${auto ? "_auto" : ""}`;
}

export function parseBackupName(name: string): Backup | null {
  const m = STAMP.exec(name);
  if (!m) return null;
  const when = new Date(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +m[6]).getTime();
  return Number.isFinite(when) ? { name, when, auto: !!m[7] } : null;
}

/**
 * A save's own icon (savedata_prospero_meta/user/<title>/<name>_icon0.png, the system's copies
 * "sce_bu_*" last): the cover of a game that is no longer installed. Null when there is none.
 */
export async function saveIconPath(user: string, s: SaveTitle, callFn: CallFn = call): Promise<string | null> {
  const meta = s.kinds.find(isMeta);
  if (!meta) return null;
  const dir = livePath(user, meta, s.titleId);
  const icons = ((await listAll(callFn, dir).catch(() => null)) ?? []).filter((e) => e.kind === "file" && /_icon0\.png$/i.test(e.name));
  icons.sort((a, b) => Number(a.name.startsWith("sce_bu_")) - Number(b.name.startsWith("sce_bu_")) || a.name.localeCompare(b.name));
  return icons.length ? `${dir}/${icons[0].name}` : null;
}

/** `titleId`'s backups for `user`, newest first. */
export async function loadBackups(user: string, titleId: string, callFn: CallFn = call, root = BACKUP_ROOT): Promise<Backup[]> {
  const out: Backup[] = [];
  for (const n of dirsOf(await listAll(callFn, backupsDir(user, titleId, root)))) {
    const b = parseBackupName(n);
    if (b) out.push(b);
  }
  return out.sort((a, b) => b.when - a.when);
}

/** The bytes of the files in `path` and one level of folders below (a save is shallow). */
export async function shallowSize(path: string, callFn: CallFn = call): Promise<number> {
  let bytes = 0;
  const l = await listAll(callFn, path);
  for (const e of l ?? []) {
    if (e.kind === "file") bytes += e.size;
    else if (e.kind === "dir") for (const f of (await listAll(callFn, `${path}/${e.name}`)) ?? []) if (f.kind === "file") bytes += f.size;
  }
  return bytes;
}

export interface OpDeps {
  call: CallFn;
  /** Runs job `id` to its end; throws with its cause when it failed. */
  wait: (id: string) => Promise<void>;
  now: () => Date;
}

async function waitOp(id: string): Promise<void> {
  const end = await watchOp(id);
  if (end.end === "done" || end.end === "gone") return;
  if (end.end === "cancelled") throw new ApiError(409, "cancelled");
  throw new ApiError(500, (end.status.message ?? "").split(":")[0] || "io_error");
}

const defaultDeps: OpDeps = { call, wait: waitOp, now: () => new Date() };

async function runJob(d: OpDeps, route: "/api/fs/copy" | "/api/fs/delete", body: object): Promise<void> {
  const r = await d.call<{ job?: unknown }>(route, body);
  if (!r || typeof r.job !== "string") throw new ApiError(500, "bad_reply");
  await d.wait(r.job);
}

/** Copies `user`'s save of `titleId` (each folder it has) into a new backup; its name. A failed copy leaves nothing behind. */
export async function backUp(
  user: string,
  titleId: string,
  kinds: readonly SaveKind[],
  opts: { auto?: boolean; root?: string } = {},
  d: OpDeps = defaultDeps,
): Promise<string> {
  if (!kinds.length) throw new ApiError(404, "save_not_found");
  const root = opts.root ?? BACKUP_ROOT;
  let name = backupName(d.now(), opts.auto);
  // two in one second (a restore right after a backup): the next free second's name
  const taken = new Set((await loadBackups(user, titleId, d.call, root)).map((b) => b.name));
  for (let i = 1; taken.has(name) && i < 60; i++) name = backupName(new Date(d.now().getTime() + i * 1000), opts.auto);
  const dest = `${backupsDir(user, titleId, root)}/${name}`;
  await d.call("/api/fs/mkdir", { path: dest });
  try {
    for (const kind of kinds) await runJob(d, "/api/fs/copy", { src: livePath(user, kind, titleId), dest: `${dest}/${kind}` });
  } catch (e) {
    await runJob(d, "/api/fs/delete", { path: dest }).catch(() => undefined);
    throw e;
  }
  return name;
}

/** The save folders `backup` holds. */
export async function backupKinds(user: string, titleId: string, backup: string, callFn: CallFn = call, root = BACKUP_ROOT): Promise<SaveKind[]> {
  const names = new Set(dirsOf(await listAll(callFn, `${backupsDir(user, titleId, root)}/${backup}`)));
  return SAVE_KINDS.filter((k) => names.has(k));
}

/**
 * Puts `backup` back as `user`'s save of `titleId`: backs up the current save first (its name is
 * returned, null when there was none), then for each folder the backup holds empties the live one
 * and copies the backup's files in. A live folder the backup does not hold (it was not there when
 * the backup was made) is emptied, so the save is the one backed up, not a mix of old and new;
 * the safety backup keeps what it held. Like the others, the folder itself stays.
 */
export async function restore(
  user: string,
  titleId: string,
  backup: string,
  current: readonly SaveKind[],
  d: OpDeps = defaultDeps,
  root = BACKUP_ROOT,
): Promise<string | null> {
  const kinds = await backupKinds(user, titleId, backup, d.call, root);
  if (!kinds.length) throw new ApiError(404, "backup_empty");
  const safety = current.length ? await backUp(user, titleId, current, { auto: true }, d) : null;
  for (const kind of kinds) {
    const live = livePath(user, kind, titleId);
    const src = `${backupsDir(user, titleId, root)}/${backup}/${kind}`;
    const have = await listAll(d.call, live);
    if (have === null) await d.call("/api/fs/mkdir", { path: live });
    // The system keeps a save's files 0600 (images, icons) and 0700 (.sfo); a copy comes out 0777.
    // Each restored file gets the mode its live namesake had (measured on 13.60), unless that was
    // a copy's 0777 too, else the system's mode for its kind.
    const modes = new Map<string, number>();
    for (const e of have ?? []) {
      const st = await d.call<{ mode?: unknown }>("/api/fs/stat", { path: `${live}/${e.name}` }).catch(() => null);
      if (typeof st?.mode === "number" && (st.mode & 0o777) !== 0o777) modes.set(e.name, st.mode & 0o777);
    }
    for (const e of have ?? []) await runJob(d, "/api/fs/delete", { path: `${live}/${e.name}` });
    for (const e of (await listAll(d.call, src)) ?? []) {
      const dest = `${live}/${e.name}`;
      await runJob(d, "/api/fs/copy", { src: `${src}/${e.name}`, dest });
      const mode = modes.get(e.name) ?? (e.kind === "dir" || /\.sfo$/i.test(e.name) ? 0o700 : 0o600);
      await d.call("/api/fs/chmod", { path: dest, mode: `0${mode.toString(8)}` }).catch(() => undefined);
    }
  }
  for (const kind of current) {
    if (kinds.includes(kind)) continue;
    const live = livePath(user, kind, titleId);
    for (const e of (await listAll(d.call, live)) ?? []) await runJob(d, "/api/fs/delete", { path: `${live}/${e.name}` });
  }
  return safety;
}

export async function deleteBackup(user: string, titleId: string, backup: string, d: OpDeps = defaultDeps, root = BACKUP_ROOT): Promise<void> {
  if (!STAMP.test(backup)) throw new ApiError(400, "bad_backup");
  await runJob(d, "/api/fs/delete", { path: `${backupsDir(user, titleId, root)}/${backup}` });
}

/**
 * One run at a time, decided before anything is awaited. A dialog's Confirm that first asks the
 * console something (Restore reads /api/status) has not set its "working" state yet when a
 * second press arrives: this refuses that press (it answers undefined, nothing runs).
 */
export function oneAtATime(): <T>(run: () => Promise<T>) => Promise<T | undefined> {
  let running = false;
  return function <T>(run: () => Promise<T>): Promise<T | undefined> {
    if (running) return Promise.resolve(undefined);
    running = true;
    let work: Promise<T>;
    try {
      work = run();
    } catch (e) {
      running = false;
      return Promise.reject(e);
    }
    const free = () => {
      running = false;
    };
    work.then(free, free);
    return work;
  };
}
