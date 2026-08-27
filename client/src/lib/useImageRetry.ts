import { useCallback, useEffect, useRef, useState } from "react";

/**
 * An <img> src that survives a transient failure.
 *
 * Cover art is fetched from the console through the engine, and every link
 * in that chain can fail for a moment without being broken: the engine may
 * still be starting when a screen first renders, the payload serves one
 * client at a time, and `/api/ps5/app-icon` answers 404 for *any* read
 * miss — a busy console is indistinguishable from a title that genuinely
 * has no art.
 *
 * Every cover in this app used to latch `failed = true` on the first error
 * and never look again, so a single blip blanked the art until the
 * component remounted. That is why covers "keep breaking": not one bug,
 * but no recovery from any of them.
 *
 * Retries are bounded and spaced, because the console is a single-client
 * server and a grid of 40 covers retrying in a tight loop is a worse
 * failure than a missing thumbnail. The cache-buster is required — without
 * it the browser serves the failed response back from its own cache and
 * the retry is a no-op.
 */
/**
 * The URL for a given retry attempt, or null when there is nothing to show.
 *
 * Split out from the hook so the one genuinely subtle part is testable: the
 * cache-buster has to merge into a src that *already* carries a query
 * string, because every cover URL does (`?addr=...&title_id=...`). Getting
 * that wrong appends a second `?`, which the engine rejects — the retry
 * would then fail permanently and look exactly like the bug it is meant to
 * fix.
 */
export function retryUrl(
  src: string | null,
  attempt: number,
  failed: boolean,
): string | null {
  if (!src || failed) return null;
  if (attempt <= 0) return src;
  return `${src}${src.includes("?") ? "&" : "?"}_retry=${attempt}`;
}

export function useImageRetry(
  src: string | null,
  {
    maxRetries = 2,
    delayMs = 1200,
  }: { maxRetries?: number; delayMs?: number } = {},
) {
  // The attempt counter lives in a ref, and only the committed value is
  // mirrored into state. onError must not call one setState from inside
  // another's updater: React invokes updaters twice under StrictMode, which
  // would double-count every failure and burn the retry budget instantly.
  const attempts = useRef(0);
  const [attempt, setAttempt] = useState(0);
  const [failed, setFailed] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clear = () => {
    if (timer.current) {
      clearTimeout(timer.current);
      timer.current = null;
    }
  };

  // Reset when the image itself changes — a row reused for a different
  // title must not inherit the previous one's failure.
  useEffect(() => {
    clear();
    attempts.current = 0;
    setAttempt(0);
    setFailed(false);
  }, [src]);

  useEffect(() => clear, []);

  const onError = useCallback(() => {
    if (attempts.current >= maxRetries) {
      setFailed(true);
      return;
    }
    attempts.current += 1;
    const next = attempts.current;
    clear();
    // Staggered so a screenful of covers failing together does not retry in
    // lockstep against the console's single connection slot.
    timer.current = setTimeout(
      () => setAttempt(next),
      delayMs + Math.random() * delayMs,
    );
  }, [maxRetries, delayMs]);

  return { src: retryUrl(src, attempt, failed), onError, failed };
}
