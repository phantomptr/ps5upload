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
    r = { titleId, value: null, error: e instanceof Error ? e.message : String(e) };
  }
  if (current() !== titleId) return;
  apply(r);
}
