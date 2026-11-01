// What goes into Workshop's queue while a game is being made (#621): a dropped or handed path is
// looked at as "Add games from a folder…" looks at one, and the game being made right now is
// not queued again.

import { isArchiveSource, isFolderableSource } from "../state/fpkgConversion";
import { gamesInFolder } from "./folderScan";

type Source = { path: string; isDir: boolean };

/** The games one dropped or handed path stands for. An image, package or archive is itself; a
 *  folder is scanned like "Add games from a folder…" (the folder when it is a game, else one
 *  item per game in it). A game on the PS5 is taken as it is (it is not listed from here). */
export async function sourcesForPath(
  path: string,
  scan: typeof gamesInFolder = gamesInFolder,
): Promise<{ sources: Source[]; skipped: number }> {
  if (isFolderableSource(path) || isArchiveSource(path)) return { sources: [{ path, isDir: false }], skipped: 0 };
  if (path.startsWith("ps5://")) return { sources: [{ path, isDir: true }], skipped: 0 };
  return scan(path);
}

const same = (a: string, b: string) => a.trim().replace(/[\\/]+$/, "") === b.trim().replace(/[\\/]+$/, "");

/** `sources` without the game being made right now (`building`: its source, or null). */
export function withoutBuilding(sources: Source[], building: string | null): { take: Source[]; building: number } {
  if (!building) return { take: sources, building: 0 };
  const take = sources.filter((s) => !same(s.path, building));
  return { take, building: sources.length - take.length };
}
