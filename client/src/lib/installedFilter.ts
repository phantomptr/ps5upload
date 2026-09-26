// Search filter for the Games → "Ready to play" tab.
//
// Mirrors the model already used by the Game files tab's
// `filterLibraryEntries`: multiple words are AND-matched against one
// entry, matching is case-insensitive, and an empty query returns the
// input array unchanged so downstream memoisation can skip work.
//
// Searchable fields on InstalledTitle:
//
//   - `titleName` — the pretty name shown on the card
//   - `titleId`   — the CUSA/PPSA "game code"
//   - `source`    — the path we registered from, when there is one, so
//                   "usb0" finds everything registered off that drive
//
// The one addition over the library filter is code normalisation. Game
// codes get written every which way — CUSA00900, cusa-00900, "PPSA 01342"
// pasted out of a filename or a forum post. Matching only the exact
// spelling makes code search fail exactly when someone is pasting a code
// in, which is the moment they most want it. So each entry is also
// matched with every non-alphanumeric character removed from both sides.

import type { InstalledTitle } from "../api/ps5";

/** Everything about a title that search should look at, lowercased once. */
function haystack(t: InstalledTitle): string {
  return [t.titleName, t.titleId, t.source].join("\n").toLowerCase();
}

/** Letters and digits only — the form that makes "ppsa-01342" and
 *  "PPSA 01342" both match a stored "PPSA01342". */
function alphanumeric(s: string): string {
  return s.replace(/[^a-z0-9]/gi, "").toLowerCase();
}

/**
 * Filter installed titles by a free-form query.
 *
 * Returns the input reference untouched for an empty or whitespace-only
 * query, so the caller's `useMemo` chain does no work when nobody is
 * searching.
 */
export function filterInstalledTitles<T extends InstalledTitle>(
  titles: T[],
  query: string,
): T[] {
  const trimmed = query.trim();
  if (trimmed === "") return titles;
  const tokens = trimmed
    .toLowerCase()
    .split(/\s+/)
    .filter((t) => t.length > 0);
  if (tokens.length === 0) return titles;

  return titles.filter((t) => {
    const hay = haystack(t);
    const hayAlnum = alphanumeric(hay);
    return tokens.every((token) => {
      if (hay.includes(token)) return true;
      const needle = alphanumeric(token);
      // A token of pure punctuation normalises to "", and every string
      // contains "" — which would silently match the whole library. Only
      // the raw comparison above can speak for such a token.
      if (needle === "") return false;
      return hayAlnum.includes(needle);
    });
  });
}
