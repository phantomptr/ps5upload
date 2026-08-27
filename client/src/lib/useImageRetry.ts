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
    // One retry when there is another transport to fall back to, two when
    // this is the only one. A webview refusing the load is deterministic —
    // retrying it just delays the recovery — whereas a busy console is
    // worth a couple of attempts.
    maxRetries,
    delayMs = 1200,
    fallbackLoader,
  }: {
    maxRetries?: number;
    delayMs?: number;
    /**
     * Last resort once the retries are spent: fetch the same image over a
     * different transport and return it as a `data:` URL (or null if that
     * fails too).
     *
     * The desktop webview is the only place in this app that loads a URL
     * directly, rather than going through the Tauri IPC — and when the
     * webview refuses that load (a CSP or mixed-content decision made by
     * whatever WebKit ships with the user's OS), every cover in the app
     * turns into a controller glyph while the engine logs a healthy 200.
     * Routing the bytes through the IPC instead sidesteps the question
     * entirely, and the CSP already permits `data:` images.
     */
    fallbackLoader?: () => Promise<string | null>;
  } = {},
) {
  // The attempt counter lives in a ref, and only the committed value is
  // mirrored into state. onError must not call one setState from inside
  // another's updater: React invokes updaters twice under StrictMode, which
  // would double-count every failure and burn the retry budget instantly.
  const retries = maxRetries ?? (fallbackLoader ? 1 : 2);
  const attempts = useRef(0);
  const [attempt, setAttempt] = useState(0);
  const [failed, setFailed] = useState(false);
  const [fallbackSrc, setFallbackSrc] = useState<string | null>(null);
  const fallbackTried = useRef(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Held in a ref so a caller can pass an inline arrow without the reset
  // effect below re-running (and re-clearing the fallback) every render.
  // Seeded at mount and refreshed in an effect — assigning during render
  // would be a render-phase side effect.
  const loaderRef = useRef(fallbackLoader);
  useEffect(() => {
    loaderRef.current = fallbackLoader;
  });

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
    fallbackTried.current = false;
    setAttempt(0);
    setFailed(false);
    setFallbackSrc(null);
  }, [src]);

  useEffect(() => clear, []);

  const onError = useCallback(() => {
    if (attempts.current >= retries) {
      // Retrying the same transport again will not help. Try the other one
      // once, and only give up (glyph) if that fails too.
      const loader = loaderRef.current;
      if (loader && !fallbackTried.current) {
        fallbackTried.current = true;
        void loader()
          .then((url) => {
            if (url) setFallbackSrc(url);
            else setFailed(true);
          })
          .catch(() => setFailed(true));
        return;
      }
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
  }, [retries, delayMs]);

  // A resolved fallback wins outright — it is the transport that worked.
  return {
    src: fallbackSrc ?? retryUrl(src, attempt, failed),
    onError,
    failed,
  };
}
