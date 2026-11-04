/**
 * Replace without a moment where both are gone. Deleting what is there and then copying lost both
 * when the copy failed (no space, a dropped share, a helper restart). So:
 *
 *   1. The new item goes to a temporary name beside the old one (<name>.ps5u-new). A copy that
 *      fails or is stopped costs only that temporary: the old item was never touched.
 *   2. Only when it is whole: the old item moves aside (<name>.ps5u-old) and the new one takes its
 *      name, two renames inside one folder. A failure puts the old one back.
 *   3. The old item, now under its aside name, is deleted.
 *
 * A name that is taken is never overwritten: it may be the person's own, or the only copy an
 * interrupted Replace left, so the next number is used (the SMB daemon's own .ps5u-old rule).
 *
 * The cost is room for both until step 3; a copy that does not fit says so and changes nothing.
 * Files over files do not come here: the helper and the SMB daemon replace those through a part
 * file already.
 */
import { ApiError } from "../api";
import { baseName, joinPath } from "./move";

export const TEMP_NEW = "ps5u-new";
export const TEMP_OLD = "ps5u-old";
/** Tries for a free temporary name. */
const TEMP_TRIES = 16;
/** A name is cut to this many UTF-8 bytes first, so the tag cannot push it past a drive's 255. */
const TEMP_STEM_BYTES = 200;

/** The start of `name` that fits TEMP_STEM_BYTES (whole characters; the usual name fits whole). */
export function tempStem(name: string, maxBytes = TEMP_STEM_BYTES): string {
  let bytes = 0;
  let end = 0;
  for (const ch of name) {
    const c = ch.codePointAt(0) ?? 0;
    bytes += c < 0x80 ? 1 : c < 0x800 ? 2 : c < 0x10000 ? 3 : 4;
    if (bytes > maxBytes) break;
    end += ch.length;
  }
  return name.slice(0, end);
}

/** A free temporary name for `name` in `dir`: "name.tag", then "name.tag2", "name.tag3"… */
export async function tempName(
  dir: string,
  name: string,
  tag: string,
  taken: (path: string) => Promise<boolean>,
): Promise<string> {
  const stem = tempStem(name);
  for (let n = 1; n <= TEMP_TRIES; n++) {
    const candidate = `${stem}.${tag}${n === 1 ? "" : n}`;
    if (!(await taken(joinPath(dir, candidate)))) return candidate;
  }
  throw new ApiError(409, "exists");
}

/** One side's file operations: the console's, or a share's. */
export interface ReplaceSide {
  taken: (path: string) => Promise<boolean>;
  /** Inside one folder, never over something. */
  rename: (from: string, to: string) => Promise<void>;
  /** Delete a tree and wait for its end, whatever Stop says. True when it is gone. */
  remove: (path: string) => Promise<boolean>;
}

/** How the new item's way to its temporary name ended. */
export type Placed =
  /** Whole at the temporary name. `undo` puts it back where it came from (a move by rename). */
  | { end: "done"; undo?: () => Promise<void> }
  /** Stop cancelled it: what it wrote so far is not wanted. */
  | { end: "stopped" }
  /** Stop could not cancel it: its job may still be writing there, so that stays. */
  | { end: "left" }
  /** It failed; `why` says why when known. `notThere`: it never wrote to the temporary name, so nothing is cleared. */
  | { end: "failed"; why?: unknown; notThere?: boolean };

export type ReplaceFailure =
  /** The new copy did not arrive: the old item was not touched. */
  | "copy"
  /** The new copy could not be put in place: the old item has its name again. */
  | "swap"
  /** The old item is under `old` and the new one under `temp`: nothing lost, neither in place. */
  | "stuck"
  /** Replaced, but the old item could not be deleted: it stays under `old`. */
  | "old_left"
  /** Replaced, but the moved item's original could not be deleted. */
  | "source_left"
  /** Not replaced, and `temp` is still there: the old item is as it was. */
  | "temp_left";

/** A Replace that did not go all the way: what happened, and what is where. */
export class ReplaceError extends ApiError {
  readonly what: ReplaceFailure;
  /** The item's name in its folder. */
  readonly item: string;
  readonly why: unknown;
  /** The new copy's temporary name, when it is still there. */
  readonly temp: string | null;
  /** The old item's aside name, when it is still there. */
  readonly old: string | null;
  constructor(what: ReplaceFailure, item: string, opts: { why?: unknown; temp?: string | null; old?: string | null } = {}) {
    super(500, `replace_${what}`);
    this.name = "ReplaceError";
    this.what = what;
    this.item = item;
    this.why = opts.why ?? null;
    this.temp = opts.temp ?? null;
    this.old = opts.old ?? null;
  }
}

/** How a Replace ended without an error. */
export type ReplaceEnd =
  /** The new item has the name. `old`: the old one could not be deleted and stays under that name. */
  | { end: "done"; old: string | null }
  /** Stop came during the copy: the old item is as it was. `temp`: what could not be cleared away. */
  | { end: "stopped"; temp: string | null };

function isTokenGone(e: unknown): boolean {
  return e instanceof ApiError && e.status === 401;
}

/**
 * A rename whose answer never came (the call's deadline, a dropped link) may have happened all the
 * same, and what follows must not act on the wrong item. So the two names are read: moved means
 * done. Anything else, or a read that fails too, stays the failure it was.
 */
export async function renameChecked(side: ReplaceSide, from: string, to: string): Promise<void> {
  try {
    await side.rename(from, to);
  } catch (why) {
    if (!(why instanceof ApiError && why.status === 0)) throw why;
    const moved = await Promise.all([side.taken(from), side.taken(to)]).then(
      ([f, t]) => !f && t,
      () => false,
    );
    if (!moved) throw why;
  }
}

/**
 * Clear away a temporary copy; its name when it is still there. Tried twice: a cancelled job's last
 * writes can still land just after it left the job list, and the first delete then finds a folder
 * that fills up behind it. A check that fails counts as still there (never claim it is gone).
 */
async function discard(side: ReplaceSide, temp: string): Promise<string | null> {
  for (let i = 0; i < 2; i++) {
    if (!(await side.taken(temp).catch(() => true))) return null;
    await side.remove(temp).catch(() => false);
  }
  return (await side.taken(temp).catch(() => true)) ? temp : null;
}

/** The new copy is not wanted any more: it goes back where it came from, or away. */
async function dropPlaced(side: ReplaceSide, placed: { undo?: () => Promise<void> }, temp: string): Promise<string | null> {
  if (!placed.undo) return discard(side, temp);
  return placed.undo().then(
    () => null,
    () => temp,
  );
}

/**
 * Replace `dest` (in `dir`) with what `place` puts at the temporary name it is given. Throws a
 * ReplaceError for anything that stopped short, or the 401 itself when the token went away.
 */
export async function replaceItem(
  side: ReplaceSide,
  dir: string,
  dest: string,
  place: (temp: string) => Promise<Placed>,
): Promise<ReplaceEnd> {
  const item = baseName(dest);
  let temp: string;
  try {
    temp = joinPath(dir, await tempName(dir, item, TEMP_NEW, side.taken));
  } catch (why) {
    if (isTokenGone(why)) throw why;
    throw new ReplaceError("copy", item, { why });
  }
  let placed: Placed;
  try {
    placed = await place(temp);
  } catch (why) {
    if (isTokenGone(why)) throw why;
    placed = { end: "failed", why };
  }
  if (placed.end === "left") return { end: "stopped", temp };
  if (placed.end === "stopped") return { end: "stopped", temp: await discard(side, temp) };
  if (placed.end === "failed") {
    throw new ReplaceError("copy", item, { why: placed.why, temp: placed.notThere ? null : await discard(side, temp) });
  }

  let aside: string;
  try {
    aside = joinPath(dir, await tempName(dir, item, TEMP_OLD, side.taken));
    await renameChecked(side, dest, aside);
  } catch (why) {
    if (isTokenGone(why)) throw why;
    throw new ReplaceError("swap", item, { why, temp: await dropPlaced(side, placed, temp) });
  }
  try {
    await renameChecked(side, temp, dest);
  } catch (why) {
    const back = await renameChecked(side, aside, dest).then(
      () => true,
      () => false,
    );
    if (!back) throw new ReplaceError("stuck", item, { why, temp, old: aside });
    throw new ReplaceError("swap", item, { why, temp: await dropPlaced(side, placed, temp) });
  }
  const gone = await side.remove(aside).catch(() => false);
  return { end: "done", old: gone ? null : aside };
}
