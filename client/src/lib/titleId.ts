// Title-id normalisation for metadata lookups.
//
// The console's metadata lookup accepts a full title id (CUSA12345_00)
// or a full content id, but enumerating installed games yields the bare
// 9-character form (CUSA12345). Passing that straight through failed
// with "Invalid format", so every lookup started by clicking an
// installed game was rejected.
//
// The payload accepts the bare form as of 5.4.3, but people run
// whatever payload they last loaded, so normalise here too: the fix
// then works without requiring a payload update first.

/** `_00` is the first-release suffix, and is what enumerating installed
 *  games implies when it reports a bare title id. */
const DEFAULT_SUFFIX = "_00";

/** Expand a bare title id to its full form. Anything already full
 *  length, a content id, or simply unrecognised is returned unchanged
 *  so the console stays the authority on what is valid. */
export function normalizeTitleId(input: string): string {
  const id = input.trim().toUpperCase();
  if (/^[A-Z]{4}\d{5}$/.test(id)) return id + DEFAULT_SUFFIX;
  return id;
}
