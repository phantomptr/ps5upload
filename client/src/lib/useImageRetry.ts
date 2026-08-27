import { useCallback, useEffect, useRef, useState } from "react";

import {
  isDirectTransportBlocked,
  noteDirectTransportBlocked,
} from "./imageTransport";

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

/**
 * Whether this image should skip the direct load entirely and go straight
 * to the IPC fallback.
 *
 * Split out from the hook so the fix for "covers reload on every visit" is
 * assertable. Everything has to line up: there is something to load, we are
 * not already holding it, there IS another transport to use, and this
 * session has already proven the direct one does not work here.
 */
export function shouldSkipDirect(
  src: string | null,
  cached: string | null | undefined,
  hasFallbackLoader: boolean,
  directBlocked: boolean,
): boolean {
  if (!src || cached) return false;
  return hasFallbackLoader && directBlocked;
}

/**
 * Which src an <img> should actually get, in priority order.
 *
 * Cache first — bytes in hand need no transport at all. Then a resolved
 * fallback, because it is the transport that demonstrably worked for this
 * image. The direct URL last, and only while it still has attempts left.
 */
export function pickImageSrc(
  cached: string | null | undefined,
  fallbackSrc: string | null,
  src: string | null,
  attempt: number,
  failed: boolean,
): string | null {
  return cached ?? fallbackSrc ?? retryUrl(src, attempt, failed);
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
    cached,
  }: {
    maxRetries?: number;
    delayMs?: number;
    /**
     * Bytes this session already holds for exactly this image, as a `data:`
     * URL. When present it is used as-is and no request of any kind is
     * made — no direct load, no retry timer, no IPC hop.
     *
     * This is what makes returning to a screen instant. The caller reads it
     * synchronously from the session cache during render, so the first
     * paint after a re-mount already has the image, instead of showing a
     * placeholder while a transport it has used forty times before is
     * re-negotiated.
     */
    cached?: string | null;
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
  //
  // This is also where a session that has already learned the direct
  // transport is blocked skips straight to the fallback. Waiting for
  // `onError` to discover it again would cost this image the full retry
  // budget plus its stagger, for a verdict the session reached long ago.
  useEffect(() => {
    clear();
    attempts.current = 0;
    fallbackTried.current = false;
    setAttempt(0);
    setFailed(false);
    setFallbackSrc(null);

    // `!loader` is checked here rather than left to shouldSkipDirect's
    // `hasFallbackLoader` argument so TypeScript can narrow it for the call
    // below; the two together are the same condition the helper encodes.
    const loader = loaderRef.current;
    if (
      !loader ||
      !shouldSkipDirect(src, cached, true, isDirectTransportBlocked())
    ) {
      return;
    }

    fallbackTried.current = true;
    let cancelled = false;
    void loader()
      .then((url) => {
        if (cancelled) return;
        if (url) setFallbackSrc(url);
        else setFailed(true);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [src, cached]);

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
            if (url) {
              // The bytes exist and only the IPC could fetch them: that is
              // evidence about the transport, not about this image, so the
              // rest of the session can skip the direct attempt. A null
              // here means the image genuinely has no artwork, which says
              // nothing about the transport and must not latch.
              noteDirectTransportBlocked();
              setFallbackSrc(url);
            } else setFailed(true);
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

  // Cache first (nothing to fetch), then a resolved fallback — it is the
  // transport that worked — and only then the direct URL.
  return {
    src: pickImageSrc(cached, fallbackSrc, src, attempt, failed),
    onError,
    failed,
  };
}
