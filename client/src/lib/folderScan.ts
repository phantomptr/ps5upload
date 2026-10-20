// "Add games from a folder…": the picked folder's immediate children as batch entries. One level
// only; games, packages, archives and images start checked, anything else unchecked.

import { localFs } from "../api/localFs";
import { remoteApi } from "../api/remote";
import type { BatchEntry } from "../state/uploadBatch";
import { classifyScanEntry } from "./uploadBatch";
import { parseRemotePath, remotePath } from "./remotePath";

export async function scanChildren(folder: string): Promise<BatchEntry[]> {
  const entry = (path: string, name: string, isDir: boolean, size: number): BatchEntry => ({
    path,
    isDir,
    // A folder's listed size is not its contents'.
    size: isDir ? null : size,
    include: classifyScanEntry(name, isDir) !== "other",
  });
  const parsed = parseRemotePath(folder);
  if (parsed) {
    const out: BatchEntry[] = [];
    let cursor: string | undefined;
    for (;;) {
      const page = await remoteApi.listDir(folder, cursor);
      for (const e of page.entries) {
        const child = remotePath(parsed.connectionId, `${parsed.path.replace(/\/+$/, "")}/${e.name}`);
        out.push(entry(child, e.name, e.is_dir, e.size));
      }
      if (!page.next_cursor) return out;
      cursor = page.next_cursor;
    }
  }
  return (await localFs.listDir(folder)).map((e) => entry(e.path, e.name, e.is_dir, e.size));
}

/** `<folder>/sce_sys`, in the folder's own form: a server path, or a local one with its
 *  separator (`\` for a Windows path). */
function sceSysOf(folder: string): string {
  const parsed = parseRemotePath(folder);
  if (parsed) return remotePath(parsed.connectionId, `${parsed.path.replace(/\/+$/, "")}/sce_sys`);
  const sep = folder.includes("/") || !folder.includes("\\") ? "/" : "\\";
  return `${folder.replace(/[\\/]+$/, "")}${sep}sce_sys`;
}

/** A game folder: its `sce_sys` holds a `param.json` (PS5) or `param.sfo` (PS4) file, the
 *  rule the engine's own folder check uses (`game_meta::inspect_folder`). Reads one small
 *  listing, never the game's files; a folder that cannot be read is not a game. */
export async function isGameFolder(folder: string): Promise<boolean> {
  const isParam = (name: string, isDir: boolean) => !isDir && (name === "param.json" || name === "param.sfo");
  try {
    const sceSys = sceSysOf(folder);
    if (parseRemotePath(sceSys)) {
      const page = await remoteApi.listDir(sceSys);
      return page.entries.some((e) => isParam(e.name, e.is_dir));
    }
    return (await localFs.listDir(sceSys)).some((e) => isParam(e.name, e.is_dir));
  } catch {
    return false;
  }
}

/** Check folders a few at a time (each is one listing; a server answers slower than a disk). */
async function gameFlags(folders: string[]): Promise<boolean[]> {
  const out: boolean[] = [];
  for (let i = 0; i < folders.length; i += 8) {
    out.push(...(await Promise.all(folders.slice(i, i + 8).map(isGameFolder))));
  }
  return out;
}

/** The picked folders that are games, in order, and how many were not. */
export async function gameFoldersAmong(folders: string[]): Promise<{ games: string[]; skipped: number }> {
  const flags = await gameFlags(folders);
  const games = folders.filter((_, i) => flags[i]);
  return { games, skipped: folders.length - games.length };
}

/** What "Add games from a folder…" converts: the folder itself when it is a game (never its
 *  sce_sys, sce_module…), else its child games, packages, images and archives, including files one wrapper level below. `skipped`: child
 *  folders that are not games. */
export async function gamesInFolder(
  folder: string,
): Promise<{ sources: { path: string; isDir: boolean }[]; skipped: number }> {
  if (await isGameFolder(folder)) return { sources: [{ path: folder, isDir: true }], skipped: 0 };
  const nameOf = (p: string) => p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? p;
  const children = (await scanChildren(folder)).filter((e) => !nameOf(e.path).startsWith("."));
  const dirs = children.filter((e) => e.isDir).map((e) => e.path);
  const flags = new Map((await gameFlags(dirs)).map((f, i) => [dirs[i], f]));
  const sources: { path: string; isDir: boolean }[] = [];
  let skipped = 0;
  const eligible = (e: BatchEntry) => ["image", "archive", "package"].includes(classifyScanEntry(nameOf(e.path), false));
  for (const e of children) {
    if (!e.isDir) {
      if (eligible(e)) sources.push({ path: e.path, isDir: false });
    } else if (flags.get(e.path)) {
      sources.push({ path: e.path, isDir: true });
    } else {
      // One wrapper level: Games/<title>/game.pkg. Never descend into a game dump,
      // hidden directories, or an unbounded tree (including symlink cycles).
      const nested = await scanChildren(e.path).catch(() => []);
      const files = nested.filter((x) => !x.isDir && eligible(x));
      sources.push(...files.map((x) => ({ path: x.path, isDir: false })));
      if (files.length === 0) skipped++;
    }
  }
  return { sources, skipped };
}
