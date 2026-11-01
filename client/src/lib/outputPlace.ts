// Where Workshop puts what it makes: in the one Output folder (the default), or next to each
// game's own source ("beside"), so a batch of game folders each gets its package beside it.
//
// "Next to" means in the folder that holds the source, for a game folder and a game image
// alike: never inside a game folder, where the package would sit among the game's own files
// (and a later build of that folder would pack it too). A name already used there is not
// overwritten or refused: the engine names the new one `<name> (2)` (`keepBoth`).

import { isRemotePath } from "./remotePath";

export type OutputPlace = "folder" | "beside";

export function parseOutputPlace(v: string | null | undefined): OutputPlace {
  return v === "beside" ? "beside" : "folder";
}

/** The folder that holds `path`, or null when it has none here (a bare name, a drive or
 *  filesystem root, a game on a server or the PS5). Trailing separators are ignored, and
 *  the path's own separator style is kept. */
export function parentDir(path: string): string | null {
  const p = path.trim().replace(/[\\/]+$/, "");
  if (!p || isRemotePath(p) || p.startsWith("ps5://")) return null;
  const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  if (i < 0) return null;
  // "/Game" → "/"; "C:\Game" → "C:\"; "\\server\share\Game" keeps its share.
  if (i === 0) return p.slice(0, 1);
  const head = p.slice(0, i);
  if (/^[A-Za-z]:$/.test(head)) return `${head}${p[i]}`;
  return head;
}

export interface OutputFor {
  /** The folder to build into; absent = the engine's default (~/Downloads/fpkgs). */
  outputDir?: string;
  /** Number the name when it is taken there, instead of refusing. */
  keepBoth: boolean;
  /** True when the output really goes next to the source. A game on a server or the PS5 has
   *  no folder here to go next to, so it goes to the Output folder instead. */
  beside: boolean;
}

/** Where one game's output goes under the chosen place. */
export function outputFor(source: string, place: OutputPlace, outputDir: string): OutputFor {
  const fallback: OutputFor = { outputDir: outputDir.trim() || undefined, keepBoth: false, beside: false };
  if (place !== "beside") return fallback;
  const parent = parentDir(source);
  return parent ? { outputDir: parent, keepBoth: true, beside: true } : fallback;
}
