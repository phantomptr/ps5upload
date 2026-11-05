import { hostOf } from "../../lib/addr";
import { humanizeCaught } from "../../lib/humanizeError";

/** What the game page holds: the answer for one title id, tagged with that id. */
export interface TitledResult<T> {
  titleId: string;
  value: T | null;
  error: string | null;
}

/** The held answer, when it belongs to `titleId`. Moving from one game page to
 *  another keeps the component (only the route param changes), so without the
 *  tag the previous game showed until the new reply landed. */
export function resultFor<T>(held: TitledResult<T> | null, titleId: string): TitledResult<T> | null {
  return held && held.titleId === titleId ? held : null;
}

/**
 * Read `titleId` and hand the result to `apply`, unless the page has moved on
 * to another title by the time the reply arrives (`current()` names the title
 * on screen now). A late reply for a game you already left is dropped rather
 * than painted over the one you are looking at.
 */
export async function loadTitled<T>(
  titleId: string,
  read: (titleId: string) => Promise<T | null>,
  current: () => string,
  apply: (r: TitledResult<T>) => void,
): Promise<void> {
  let r: TitledResult<T>;
  try {
    r = { titleId, value: await read(titleId), error: null };
  } catch (e) {
    r = { titleId, value: null, error: humanizeCaught(e) };
  }
  if (current() !== titleId) return;
  apply(r);
}

/** The queue fields the game page matches finished jobs on. */
export interface FinishedJobLike {
  id: string;
  status: string;
  addr: string;
  contentId?: string | null;
  sourcePath: string;
}

const jobKey = (it: FinishedJobLike) => `${it.id}@${hostOf(it.addr)}`;

/** Every job already done, matched to this game or not. Taken when the page opens: which
 *  jobs match depends on the game's copies, and those are not known until the view loads, so
 *  a baseline of only the matching jobs would let an old one count as new once it did. */
export function finishedBaseline(items: readonly FinishedJobLike[]): Set<string> {
  return new Set(items.filter((it) => it.status === "done").map(jobKey));
}

/** This game's finished jobs (by content id, or by a source that is one of its copies), as
 *  `id@host`. */
export function finishedFor(
  items: readonly FinishedJobLike[],
  titleId: string,
  copyPaths: readonly string[],
): string[] {
  return items
    .filter(
      (it) =>
        it.status === "done" &&
        ((it.contentId ?? "").includes(titleId) || copyPaths.includes(it.sourcePath)),
    )
    .map(jobKey);
}
