import { useEffect, useState } from "react";

/** Returns a ticking `elapsedMs` that updates every `intervalMs` while
 *  `active` is true, and resets to 0 whenever `active` flips from
 *  false → true. Used by FS-op progress panels so "Deleting… 0:03"
 *  advances in real time even though the underlying operation is a
 *  single synchronous round-trip on the PS5 side.
 *
 *  Why a hook instead of a setInterval inline: React's StrictMode
 *  double-mounts components in dev, which can leave orphan timers.
 *  Wrapping in useEffect gives clean cleanup per mount. */
export function useElapsed(active: boolean, intervalMs = 500): number {
  const [elapsed, setElapsed] = useState(0);
  useEffect(() => {
    if (!active) {
      setElapsed(0);
      return;
    }
    const startedAt = Date.now();
    setElapsed(0);
    const t = setInterval(() => {
      setElapsed(Date.now() - startedAt);
    }, intervalMs);
    return () => clearInterval(t);
  }, [active, intervalMs]);
  return elapsed;
}
