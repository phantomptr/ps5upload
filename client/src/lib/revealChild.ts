// "Going back from a folder highlights the folder you came out of": which row of the folder now
// shown was on the way to the folder left. Pure, so every file browser shares it.

const SEP = /[\\/]/;

/** Drops trailing separators, but keeps a root ("/", "C:\") whole. */
function trimEnd(p: string): string {
  const t = p.replace(/[\\/]+$/, "");
  return t === "" || /^[A-Za-z]:$/.test(t) ? p : t;
}

/**
 * The name of the direct child of `to` that leads to `from`, when `from` is inside `to` (going up
 * one level gives the folder left; a breadcrumb several levels up gives the child on the way).
 * Null when `from` is not below `to`: going into a folder, sideways, or to the same folder.
 * Works with "/" and "\" separators.
 */
export function childToReveal(
  from: string | null | undefined,
  to: string | null | undefined,
): string | null {
  if (!from || !to) return null;
  const f = trimEnd(from);
  const t = trimEnd(to);
  if (f === t || !f.startsWith(t)) return null;
  let rest = f.slice(t.length);
  if (!SEP.test(t.slice(-1))) {
    // "/data/gamesX" is not inside "/data/games": the next character must be a separator.
    if (!SEP.test(rest.charAt(0))) return null;
    rest = rest.slice(1);
  }
  return rest.split(SEP).find((s) => s !== "") ?? null;
}

/** How long the folder you came out of stays highlighted. */
export const REVEAL_MS = 2000;
