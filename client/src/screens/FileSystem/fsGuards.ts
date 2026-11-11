// Small decisions the Files screen makes around its async work. Pure, so each is tested.

/** One folder of one console: what a listing was asked for, or what is on screen now. */
export interface FsPlace {
  host: string | null | undefined;
  path: string;
}

/**
 * Whether a listing asked for `probed` may still be painted, given what the screen shows
 * `now`. The rows' actions join the path on screen with a row's name, so a listing of another
 * folder (or another console) under that path makes Delete act on the wrong item.
 */
export function listingIsCurrent(probed: FsPlace, now: FsPlace): boolean {
  return (probed.host ?? "") === (now.host ?? "") && probed.path === now.path;
}

/** What an extract reports about entries that did not come out. */
export interface ExtractLeftovers {
  skipped: number;
  unsafe: number;
  links: number;
}

/**
 * "Delete the archive after extracting" holds only when every entry came out. A skipped file
 * (one already there, left as it was) is still only in the archive in its archived form.
 */
export function archiveFullyExtracted(out: ExtractLeftovers): boolean {
  return out.skipped === 0 && out.unsafe === 0 && out.links === 0;
}

const trimSlash = (p: string): string => (p.length > 1 ? p.replace(/\/+$/, "") : p);
const joinDir = (dir: string, name: string): string =>
  dir === "/" ? `/${name}` : `${trimSlash(dir)}/${name}`;

export interface PastePlan<T> {
  /** Items the paste goes on with. */
  ok: T[];
  /** Already at the place the paste would put them: pasting would "replace" them by themselves. */
  same: T[];
  /** A folder pasted into itself or a folder inside it. */
  intoItself: T[];
}

/** Sort a paste's items into what can go into `into` and what never can. */
export function planPaste<T extends { path: string; name: string }>(
  items: T[],
  into: string,
): PastePlan<T> {
  const dest = trimSlash(into);
  const plan: PastePlan<T> = { ok: [], same: [], intoItself: [] };
  for (const item of items) {
    const src = trimSlash(item.path);
    if (dest === src || dest.startsWith(`${src}/`)) plan.intoItself.push(item);
    else if (joinDir(dest, item.name) === src) plan.same.push(item);
    else plan.ok.push(item);
  }
  return plan;
}
