/**
 * One read at a time per key (a console), and a read asked for while one runs is not dropped:
 * it runs once that one ends. A Stop or a launch that asks while a poll is in flight would
 * otherwise get no read of its own, and the screen would show the poll's older answer until the
 * next poll.
 *
 *   if (!gate.enter(key)) return;   // one runs: it runs again after
 *   try { … } finally { if (gate.leave(key)) void again(); }
 */
export interface RunGate {
  /** False while a run for `key` is in flight (that run is then asked to run once more). */
  enter(key: string): boolean;
  /** Ends the run for `key`; true when it was asked for again meanwhile. */
  leave(key: string): boolean;
}

export function runGate(): RunGate {
  const running = new Set<string>();
  const again = new Set<string>();
  return {
    enter(key) {
      if (running.has(key)) {
        again.add(key);
        return false;
      }
      running.add(key);
      return true;
    },
    leave(key) {
      running.delete(key);
      return again.delete(key);
    },
  };
}
