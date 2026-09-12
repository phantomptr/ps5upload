/** Browsing and naming for the cheats collection.
 *
 *  Every published index line is
 *  `<TITLE_ID>_<VERSION>[_variant].<ext>=<game name>`, so the game's name, its
 *  title id and the version a cheat targets are all derivable from the index
 *  the browser already fetches. */

import type { CheatRepoEntry } from "../api/ps5";

export interface CheatFilters {
  /** `json` | `shn` | `mc4`, or "" for all. */
  format: string;
  /** Game version as published (`01.05`), or "" for all. */
  version: string;
  /** Only cheats for games that are installed on this console. */
  installedOnly: boolean;
}

export const NO_CHEAT_FILTERS: CheatFilters = {
  format: "",
  version: "",
  installedOnly: false,
};

/** A repo `game_title` that is not actually a name.
 *
 *  Some index lines carry a content id where the title should be
 *  (`EP4800-CUSA14936_00-0000111122223334`). Showing that is worse than
 *  showing nothing, because it looks like data rather than a gap. */
export function isUsableGameTitle(title: string | undefined): boolean {
  const t = (title ?? "").trim();
  if (!t) return false;
  // EP1234-CUSA12345_00-<16 chars> and friends.
  if (/^[A-Z]{2}\d{4}-[A-Z]{4}\d{5}_\d{2}-/i.test(t)) return false;
  // A bare title id is the same non-answer the UI already falls back to.
  if (/^[A-Z]{4}\d{5}$/i.test(t)) return false;
  return true;
}

/** The best name we can show for a title, in order of trustworthiness.
 *
 *  1. the cheat file's own name — written by whoever made the cheat
 *  2. the game as installed on THIS console — matches what the player sees
 *  3. the repo index — covers games that are not installed on this console
 *  4. the title id, when nothing better exists
 */
export function resolveCheatName(
  titleId: string,
  sources: {
    fromCheatFile?: string;
    installed?: Map<string, string>;
    fromRepoIndex?: Map<string, string>;
  },
): string {
  const key = titleId.toUpperCase();
  if (isUsableGameTitle(sources.fromCheatFile)) return sources.fromCheatFile!.trim();
  const installed = sources.installed?.get(key);
  if (isUsableGameTitle(installed)) return installed!.trim();
  const repo = sources.fromRepoIndex?.get(key);
  if (isUsableGameTitle(repo)) return repo!.trim();
  return titleId;
}

/** title id → game name, built from repo entries.
 *
 *  First usable name wins: the same game appears in several repos and under
 *  several versions, and they do not always agree on spelling. */
export function namesFromRepoEntries(entries: CheatRepoEntry[]): Map<string, string> {
  const out = new Map<string, string>();
  for (const e of entries) {
    const id = (e.title_id || "").toUpperCase();
    if (!id || out.has(id)) continue;
    if (isUsableGameTitle(e.game_title)) out.set(id, e.game_title.trim());
  }
  return out;
}

/** What the filter controls should offer, derived from what is actually here.
 *
 *  Deliberately not a fixed list: a repo that starts publishing a new format
 *  should show up without a code change, and offering a version nothing
 *  matches would just be a way to get an empty list. */
export function cheatFilterOptions(entries: CheatRepoEntry[]): {
  formats: string[];
  versions: string[];
} {
  const formats = new Set<string>();
  const versions = new Set<string>();
  for (const e of entries) {
    if (e.format) formats.add(e.format.toLowerCase());
    if (e.game_version) versions.add(e.game_version);
  }
  return {
    formats: [...formats].sort(),
    // Newest first: a player looking for a version usually wants the latest
    // their game could be on.
    versions: [...versions].sort().reverse(),
  };
}

/** Formats the console can actually read.
 *
 *  MC4 is encrypted XML and the payload has no decryption for it, so a
 *  downloaded .mc4 installs fine and then shows "no cheats found" — which is
 *  indistinguishable from a broken download. Roughly a quarter of the
 *  published collection is MC4, so this is not a rare corner. */
export const SUPPORTED_CHEAT_FORMATS = ["json", "shn"];

export function isSupportedCheatFormat(format: string | undefined): boolean {
  return SUPPORTED_CHEAT_FORMATS.includes((format ?? "").toLowerCase());
}

export function filterCheatEntries(
  entries: CheatRepoEntry[],
  filters: CheatFilters,
  installedTitleIds: Set<string>,
): CheatRepoEntry[] {
  const format = filters.format.toLowerCase();
  return entries.filter((e) => {
    if (format && (e.format || "").toLowerCase() !== format) return false;
    if (filters.version && e.game_version !== filters.version) return false;
    if (filters.installedOnly) {
      const id = (e.title_id || "").toUpperCase();
      // An entry whose filename breaks the convention has no title id, so it
      // cannot be matched against the console. Hiding it would make the
      // filter quietly lose rows; it is excluded only from THIS filter.
      if (!id || !installedTitleIds.has(id)) return false;
    }
    return true;
  });
}
