// Paths that are no longer where they were on a console: deleted, or moved or renamed away
// (api/ps5 fsDelete / fsMove, whichever screen called them). The Files clipboard listens, so a
// staged item whose source is gone stops being offered for Paste (#936). Kept apart from the
// store so the API layer does not depend on app state.

type Listener = (addr: string, paths: readonly string[]) => void;

const listeners = new Set<Listener>();

/** Call `fn` whenever paths go away on a console; returns the undo. */
export function onPathsGone(fn: Listener): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** `paths` on the console at `addr` (any of its addresses: the host decides) are gone. */
export function notifyPathsGone(addr: string, paths: readonly string[]): void {
  if (paths.length === 0) return;
  for (const fn of listeners) {
    try {
      fn(addr, paths);
    } catch (e) {
      console.warn("paths-gone listener failed:", e);
    }
  }
}

/** Whether `path` is `gone` itself or inside it. */
export function isAtOrUnder(path: string, gone: string): boolean {
  const g = gone.length > 1 ? gone.replace(/\/+$/, "") : gone;
  const p = path.length > 1 ? path.replace(/\/+$/, "") : path;
  if (p === g) return true;
  return p.startsWith(g === "/" ? "/" : `${g}/`);
}
