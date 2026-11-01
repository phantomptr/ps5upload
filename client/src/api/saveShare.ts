// Sharing a PS5 save with another account (engine save_copy.rs): every image of a game's save,
// decrypted, in one zip; put into another account's save of the same game (on this console or
// another), which the console then keeps as theirs. Copy to another user and Restore to another
// user do the same in the engine, with no file through this computer (#612).

import { getEngineUrl } from "../state/engine";

/** A refusal from the engine; `missing` names the target's save parts that aren't there yet
 *  (`sdimg_STARBOARD`, …: see `partLabel`). */
export class SaveCopyError extends Error {
  missing: string[];
  constructor(message: string, missing: string[] = []) {
    super(message);
    this.name = "SaveCopyError";
    this.missing = missing;
  }
}

async function failed(r: Response): Promise<never> {
  const b = (await r.json().catch(() => ({}))) as { error?: string; missing?: unknown };
  const missing = Array.isArray(b.missing) ? b.missing.filter((m): m is string => typeof m === "string") : [];
  throw new SaveCopyError(b.error || `HTTP ${r.status}`, missing);
}

/** Every image of the save folder `folder` as one zip, with the name to save it under. */
export async function shareSave(addr: string, folder: string): Promise<{ blob: Blob; name: string }> {
  const r = await fetch(`${getEngineUrl()}/api/save-backup/share`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ addr, folder }),
  });
  if (!r.ok) return failed(r);
  const cd = r.headers.get("content-disposition") ?? "";
  const name = /filename="([^"]+)"/.exec(cd)?.[1] ?? "shared save.zip";
  return { blob: await r.blob(), name };
}

/** Put a shared save's files into the save folder `folder` of `userId`: each part into the part
 *  of the same name (backed up first, put back on failure). The parts replaced. */
export async function putSharedSave(addr: string, folder: string, userId: number, zip: Blob): Promise<string[]> {
  const q = new URLSearchParams({ addr, folder, user_id: String(userId >>> 0) });
  const r = await fetch(`${getEngineUrl()}/api/save-backup/use-shared?${q}`, {
    method: "POST",
    headers: { "content-type": "application/zip" },
    body: zip,
  });
  if (!r.ok) return failed(r);
  const b = (await r.json().catch(() => ({}))) as { parts?: string[] };
  return b.parts ?? [];
}

/** What a copy put where: the source (a folder, or a backup file), the target folder and the
 *  parts replaced. */
export interface CopyDone {
  ok: boolean;
  from: string;
  to: string;
  parts: string[];
}

async function postJson<T>(path: string, body: unknown): Promise<T> {
  const r = await fetch(`${getEngineUrl()}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!r.ok) return failed(r);
  return (await r.json()) as T;
}

/** Copy one user's PS5 save of a game, every part of it, into another user's save of the same
 *  game on the same console (#612). */
export function copySaveToUser(
  addr: string,
  req: { title_id: string; from_user: number; to_user: number },
): Promise<CopyDone> {
  return postJson("/api/save-backup/copy", {
    addr,
    title_id: req.title_id,
    from_user: req.from_user >>> 0,
    to_user: req.to_user >>> 0,
  });
}

/** Put a backup of one user's save into another user's save of the game (#612). */
export function restoreBackupToUser(
  addr: string,
  req: { title_id: string; file: string; to_user: number; server?: boolean },
): Promise<CopyDone> {
  return postJson("/api/save-backup/restore-to-user", { addr, ...req, to_user: req.to_user >>> 0 });
}

/** A save part's name for people: `sdimg_STARBOARD` is "STARBOARD"; `sce_sdmemory`, the part
 *  the console's save API makes for a game's one main save, is `main` (translated by the
 *  caller). */
export function partLabel(name: string, main: string): string {
  const bare = name.replace(/^sdimg_/, "");
  return bare === "sce_sdmemory" ? main : bare;
}

/** One of the console's own backup copies of a save image (`sdimg_sce_bu_<name>`): never a
 *  save to pick. */
export function isConsoleBackupImage(name: string): boolean {
  return /^(sdimg_)?sce_bu_/.test(name);
}
