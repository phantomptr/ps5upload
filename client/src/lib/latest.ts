/**
 * Keeps only the newest of overlapping async requests: `begin()` before each, and on its answer
 * `isCurrent(token)`; an older answer that lands after a newer request started is dropped, so a
 * slow check of game A can never overwrite the check of game B chosen after it.
 */
export function createLatest() {
  let current = 0;
  return {
    begin: (): number => ++current,
    isCurrent: (token: number): boolean => token === current,
    /** Make every request in flight stale (e.g. the source was cleared). */
    invalidate: (): void => {
      current += 1;
    },
  };
}
