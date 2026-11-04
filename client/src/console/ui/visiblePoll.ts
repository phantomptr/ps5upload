/**
 * Run `fn` now and every `ms` while the page is visible. While it is hidden
 * (the PS5 browser in the background, another tab) nothing runs; when it shows
 * again `fn` runs at once and the interval starts over. A promise it returns
 * holds the next ticks until it settles: on a slow helper reads must not stack
 * up behind each other and take every connection the browser has for this site.
 * Returns the stop function.
 */
export interface VisibilityDoc {
  readonly hidden: boolean;
  addEventListener(type: "visibilitychange", fn: () => void): void;
  removeEventListener(type: "visibilitychange", fn: () => void): void;
}

export function pollWhileVisible(fn: () => unknown, ms: number, doc: VisibilityDoc = document): () => void {
  let timer: ReturnType<typeof setInterval> | undefined;
  let busy = false;
  const tick = () => {
    if (busy) return;
    const r = fn();
    if (r instanceof Promise) {
      busy = true;
      const free = () => {
        busy = false;
      };
      r.then(free, free);
    }
  };
  const start = () => {
    if (timer !== undefined) return;
    tick();
    timer = setInterval(tick, ms);
  };
  const pause = () => {
    if (timer !== undefined) clearInterval(timer);
    timer = undefined;
  };
  const onChange = () => (doc.hidden ? pause() : start());
  doc.addEventListener("visibilitychange", onChange);
  if (!doc.hidden) start();
  return () => {
    pause();
    doc.removeEventListener("visibilitychange", onChange);
  };
}
