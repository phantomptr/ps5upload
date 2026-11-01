import { createContext, useContext } from "react";

import type { InstalledTitle } from "../../api/ps5";
import { iconTitleId } from "../../lib/titleId";
import { useTitleInfo } from "../../lib/useTitleInfo";

/** Title ID (upper case) → the name the console's app.db gives it, as Games shows it. */
export type ConsoleNames = ReadonlyMap<string, string>;

/** The names the console knows for its titles. A title app.db has no name for (its name is
 *  just the id again) is left out, so the online name can still fill it in. */
export function consoleNames(titles: readonly InstalledTitle[] | null | undefined): ConsoleNames {
  const out = new Map<string, string>();
  for (const t of titles ?? []) {
    const name = t.titleName?.trim();
    if (name && name.toUpperCase() !== t.titleId.toUpperCase()) out.set(t.titleId.toUpperCase(), name);
  }
  return out;
}

/** A save folder's game name: the console's own name first (what Games and Recent games show),
 *  then the online one, then the title ID itself. A backup folder (`PPSA17221.bak`) is named
 *  after its game. */
export function saveGameName(
  folder: string,
  names: ConsoleNames,
  online?: string | null,
): string | null {
  const id = (iconTitleId(folder) ?? folder).toUpperCase();
  return names.get(id) ?? (online?.trim() || null);
}

export const ConsoleNamesContext = createContext<ConsoleNames>(new Map());

/** The game's name for a save folder, or null while neither the console nor the online lookup
 *  knows it. */
export function useSaveGameName(folder: string): { name: string | null; coverUrl: string | null } {
  const names = useContext(ConsoleNamesContext);
  const info = useTitleInfo(iconTitleId(folder) ?? folder);
  return { name: saveGameName(folder, names, info?.title), coverUrl: info?.coverImageUrl ?? null };
}
