/**
 * Module-level promise gate — caps the number of in-flight async
 * operations sharing a single limiter to `max`. Anything over that
 * waits in a FIFO queue. Used by the Library to throttle per-row
 * metadata fetches so a 50-entry refresh doesn't fire 50 concurrent
 * FS_READ requests at the PS5's single management port.
 *
 * Returns the task's result (or rethrows) so callers can treat it
 * like `fn()` directly.
 */
export function createLimiter(max: number) {
  let active = 0;
  const queue: (() => void)[] = [];

  const next = () => {
    if (active >= max) return;
    const resume = queue.shift();
    if (!resume) return;
    active += 1;
    resume();
  };

  return async function limited<T>(fn: () => Promise<T>): Promise<T> {
    await new Promise<void>((resolve) => {
      queue.push(resolve);
      next();
    });
    try {
      return await fn();
    } finally {
      active -= 1;
      next();
    }
  };
}
