// Library search-bar filter. Matches a free-form query against the
// fields that LibraryEntry exposes at scan time:
//
//   - `name`    — folder name for games, filename for images
//   - `titleId` — PPSA01342-style ID from sce_sys/param.json (games)
//   - `path`    — absolute PS5 path (so "ext1" finds everything on
//                 /mnt/ext1, etc.)
//   - `scope`   — the scan suffix (e.g. "etaHEN/games", "homebrew")
//
// We deliberately do NOT search the per-row `meta.title` (the
// pretty title from param.json) here — meta is fetched
// asynchronously per row after the screen mounts and isn't on
// LibraryEntry. Searching by folder name is good enough in practice
// since the folder is usually named after the title (or the
// title_id). Adding a meta-title search later would require
// hoisting meta into a screen-level cache.
//
// Multiple words in the query are AND-matched: "dead space" must
// match a single entry's combined searchable text. Case-insensitive.
// Empty / whitespace-only queries return the input unchanged.

import type { LibraryEntry } from "../api/ps5";

/** Build the searchable haystack for one entry. Lowercased once so
 *  the per-token match is a cheap `.includes`. */
function entryHaystack(entry: LibraryEntry): string {
  const parts = [
    entry.name,
    entry.titleId ?? "",
    entry.path,
    entry.scope,
    entry.volume,
  ];
  return parts.join("\n").toLowerCase();
}

/** Filter `entries` by the user's search query. Returns the input
 *  reference unchanged when the query is empty / whitespace, so React
 *  memoization downstream can skip work. */
export function filterLibraryEntries<T extends LibraryEntry>(
  entries: T[],
  query: string,
): T[] {
  const trimmed = query.trim();
  if (trimmed === "") return entries;
  // Token-split on whitespace and keep the lowercase forms. Empty
  // tokens (from runs of whitespace) are filtered out so a query
  // like "dead   space" still works.
  const tokens = trimmed
    .toLowerCase()
    .split(/\s+/)
    .filter((t) => t.length > 0);
  if (tokens.length === 0) return entries;
  return entries.filter((e) => {
    const hay = entryHaystack(e);
    return tokens.every((t) => hay.includes(t));
  });
}
