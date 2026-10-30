// Move saves from a PS4: read a jailbroken PS4's saves over its FTP server (read only, never a
// write to the PS4), keep them on this computer, then put chosen games' saves into a PS5 user's
// PS4 save folders through the engine (save_backup.rs import_ps4), which backs up the PS5's own
// save first and refuses anything that does not look right.

import { remoteApi, type RemoteEntry } from "../api/remote";
import { fetchRemote } from "./materialize";
import { remotePath } from "./remotePath";
import { getEngineUrl } from "../state/engine";

/** A PS4 user folder: 8 hex digits under /user/home. */
export const PS4_USER = /^[0-9a-f]{8}$/i;
/** A game's title id (PS4 games are CUSA…, but region prefixes vary). */
export const TITLE_ID = /^[A-Z]{4}\d{5}$/;

export interface Ps4User {
  id: string;
  games: number;
}

export interface Ps4Game {
  titleId: string;
  files: number;
  bytes: number;
}

/** The PS4's FTP server as a saved connection (made once, then reused). */
export async function ps4Connection(host: string, port: number): Promise<string> {
  const h = host.trim();
  const existing = (await remoteApi.list()).find(
    (c) => c.protocol === "ftp" && c.host === h && c.port === port,
  );
  if (existing) return existing.id;
  const made = await remoteApi.add({ name: `PS4 ${h}`, protocol: "ftp", host: h, port, share: "", user: "", start_path: "/" });
  return made.id;
}

async function listAll(conn: string, path: string): Promise<RemoteEntry[]> {
  const out: RemoteEntry[] = [];
  let cursor: string | undefined;
  for (let i = 0; i < 50; i++) {
    const r = await remoteApi.listDir(remotePath(conn, path), cursor);
    out.push(...r.entries);
    if (!r.next_cursor) break;
    cursor = r.next_cursor;
  }
  return out;
}

/** The PS4's users that have saves. */
export async function ps4Users(conn: string): Promise<Ps4User[]> {
  const homes = (await listAll(conn, "/user/home")).filter((e) => e.is_dir && PS4_USER.test(e.name));
  const users: Ps4User[] = [];
  for (const h of homes) {
    const games = await listAll(conn, `/user/home/${h.name}/savedata`).catch(() => []);
    const n = games.filter((g) => g.is_dir && TITLE_ID.test(g.name)).length;
    if (n > 0) users.push({ id: h.name.toLowerCase(), games: n });
  }
  return users;
}

/** One PS4 user's games with saves, and how big each save is. */
export async function ps4Games(conn: string, user: string): Promise<Ps4Game[]> {
  const dirs = (await listAll(conn, `/user/home/${user}/savedata`)).filter((g) => g.is_dir && TITLE_ID.test(g.name));
  const out: Ps4Game[] = [];
  for (const d of dirs) {
    const files = (await listAll(conn, `/user/home/${user}/savedata/${d.name}`)).filter((f) => !f.is_dir);
    out.push({ titleId: d.name, files: files.length, bytes: files.reduce((n, f) => n + f.size, 0) });
  }
  return out.sort((a, b) => a.titleId.localeCompare(b.titleId));
}

/** The folder on this computer (the engine's) a PS4 backup goes in. */
export function ps4BackupFolder(dir: string, host: string, user: string, at: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  const day = `${at.getFullYear()}-${p(at.getMonth() + 1)}-${p(at.getDate())} ${p(at.getHours())}${p(at.getMinutes())}`;
  return `${dir.replace(/[\\/]+$/, "")}/PS4 ${host.replace(/[^0-9A-Za-z.-]/g, "_")} ${user} ${day}`;
}

/** Copy one game's PS4 save folder into `dest` (making `dest/<TITLE>`); checks the copy is
 *  the size the PS4 listed. Returns the local folder. */
export async function backUpPs4Game(
  conn: string,
  user: string,
  game: Ps4Game,
  dest: string,
  onProgress?: (done: number, total: number) => void,
): Promise<string> {
  let copied = -1;
  const local = await fetchRemote(remotePath(conn, `/user/home/${user}/savedata/${game.titleId}`), {
    destDir: dest,
    onProgress: (done, total) => {
      copied = total;
      onProgress?.(done, total);
    },
  });
  if (copied >= 0 && copied !== game.bytes) {
    throw new Error(`${game.titleId}: copied ${copied} bytes, the PS4 listed ${game.bytes}`);
  }
  return local;
}

/** Put one game's PS4 save (the local folder `src`) into the PS5 save folder `savePath`. */
export async function putPs4Save(req: {
  addr: string;
  titleId: string;
  savePath: string;
  userId: number;
  src: string;
}): Promise<void> {
  const r = await fetch(`${getEngineUrl()}/api/save-backup/import-ps4`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      addr: req.addr,
      title_id: req.titleId,
      save_path: req.savePath,
      user_id: req.userId >>> 0,
      src_dir: req.src,
    }),
  });
  if (!r.ok) {
    const b = (await r.json().catch(() => ({}))) as { error?: string };
    throw new Error(b.error || `HTTP ${r.status}`);
  }
}
