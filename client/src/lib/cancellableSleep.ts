// AbortSignal-aware sleep.
//
// `payloadPlaylists.ts` has multi-second "pause" steps (e.g. "wait
// 3s for kstuff to settle before sending the next payload"). Plain
// `setTimeout(resolve, ms)` makes those un-cancellable — clicking
// Stop during a sleep step leaves the playlist waiting out the full
// duration before noticing. The existing code there uses a
// per-playlist `AbortController` paired with a hand-rolled
// addEventListener-and-cleanup dance; this module hoists that
// dance into one place so future "sleep but support cancel" callers
// don't need to rediscover the right shape.

/** Sleep for `ms` milliseconds, rejecting with `AbortError` if
 *  `signal` aborts first. Symmetric: if the signal is already
 *  aborted at call time, the returned promise rejects synchronously.
 *
 *  Always cleans up the timer + listener on either resolution path,
 *  so there's no "stop-during-sleep leaks a setTimeout that fires
 *  after navigation" footgun. */
export function cancellableSleep(
  ms: number,
  signal: AbortSignal,
): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(
        new DOMException(
          "cancellableSleep aborted before start",
          "AbortError",
        ),
      );
      return;
    }
    // Bare setTimeout/clearTimeout (not window.*) so this works in
    // both browser and node — vitest runs node by default; vi.useFake
    // Timers mocks the bare globals. The Tauri webview has both
    // window.setTimeout AND bare setTimeout pointing at the same impl,
    // so this is browser-safe too.
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      signal.removeEventListener("abort", onAbort);
      reject(
        new DOMException(
          "cancellableSleep aborted while sleeping",
          "AbortError",
        ),
      );
    };
    signal.addEventListener("abort", onAbort, { once: true });
  });
}

/** True iff `e` is the AbortError that `cancellableSleep` throws.
 *  Useful in catch blocks that want to distinguish "user clicked
 *  Stop" (normal) from "something actually broke". */
export function isAbortError(e: unknown): boolean {
  return (
    e instanceof DOMException && e.name === "AbortError"
  );
}
