import { useCallback, useEffect, useRef, useState } from "react";

import { windowRange } from "./windowing";

/** Fewer rows than this are simply all rendered. */
export const WINDOW_FROM = 200;
/** Rows kept rendered on each side of the screen. */
const OVERSCAN = 20;
/** A row's height with its gap, until one has been measured. */
const ROW_GUESS_PX = 44;

export interface RowWindow {
  start: number;
  end: number;
}

/**
 * The rows to render for a list of `count` rows, `listTop` px below the top of its scroller's
 * content view (negative once scrolled past). All of them below `WINDOW_FROM`.
 */
export function rowWindow(
  listTop: number,
  viewportHeight: number,
  pitch: number,
  count: number,
): RowWindow {
  if (count < WINDOW_FROM) return { start: 0, end: count };
  return windowRange(-listTop, viewportHeight, pitch, count, OVERSCAN);
}

/** Whether moving from one window to the next is worth a render: not until the screen has come
 *  within half the overscan of the rendered edge. */
export function windowMoved(
  cur: RowWindow,
  next: RowWindow,
  count: number,
): boolean {
  if (next.start === cur.start && next.end === cur.end) return false;
  const reachesEnd = (next.end === count) !== (cur.end === count);
  const reachesStart = (next.start === 0) !== (cur.start === 0);
  return (
    reachesEnd ||
    reachesStart ||
    Math.abs(next.start - cur.start) >= OVERSCAN / 2 ||
    Math.abs(next.end - cur.end) >= OVERSCAN / 2
  );
}

function scrollerOf(el: HTMLElement): HTMLElement | null {
  for (let p = el.parentElement; p; p = p.parentElement) {
    const o = getComputedStyle(p).overflowY;
    if (o === "auto" || o === "scroll") return p;
  }
  return null;
}

/**
 * Renders only the rows near the screen of a long list of same-height rows that scrolls with the
 * page around it, not in a box of its own. Put `listRef` on the list, `paddingTop: before` and
 * `paddingBottom: after` on its style, and render `items.slice(start, end)`.
 */
export function useWindowedRows(count: number) {
  const listRef = useRef<HTMLUListElement | null>(null);
  const [win, setWin] = useState<RowWindow>({
    start: 0,
    end: Math.min(count, WINDOW_FROM),
  });
  const [pitch, setPitch] = useState(ROW_GUESS_PX);
  const windowed = count >= WINDOW_FROM;

  const measure = useCallback(() => {
    const list = listRef.current;
    if (!list) return;
    const scroller = scrollerOf(list);
    const rows = list.children;
    let rowPitch = ROW_GUESS_PX;
    if (rows.length >= 2) {
      const step =
        (rows[1] as HTMLElement).offsetTop - (rows[0] as HTMLElement).offsetTop;
      // Zero while the screen is kept hidden: the last good one stands.
      if (step > 0) rowPitch = step;
    }
    setPitch((p) =>
      rows.length >= 2 && rowPitch !== p && rowPitch !== ROW_GUESS_PX
        ? rowPitch
        : p,
    );
    const top =
      list.getBoundingClientRect().top -
      (scroller?.getBoundingClientRect().top ?? 0);
    const height = scroller?.clientHeight ?? window.innerHeight;
    const next = rowWindow(top, height, rowPitch, count);
    setWin((cur) => (windowMoved(cur, next, count) ? next : cur));
  }, [count]);

  useEffect(() => {
    if (!windowed) return;
    const list = listRef.current;
    if (!list) return;
    const scroller = scrollerOf(list);
    let frame = 0;
    const onScroll = () => {
      if (frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        measure();
      });
    };
    measure();
    const target: HTMLElement | Window = scroller ?? window;
    target.addEventListener("scroll", onScroll, { passive: true });
    const ro =
      typeof ResizeObserver === "undefined"
        ? null
        : new ResizeObserver(onScroll);
    if (scroller) ro?.observe(scroller);
    return () => {
      if (frame) cancelAnimationFrame(frame);
      target.removeEventListener("scroll", onScroll);
      ro?.disconnect();
    };
  }, [windowed, measure]);

  /** Brings row `index` to the middle of the screen, rendered or not. */
  const scrollToRow = useCallback(
    (index: number) => {
      const list = listRef.current;
      if (!list || index < 0) return;
      const scroller = scrollerOf(list);
      if (!scroller) return;
      const listTop =
        list.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
      scroller.scrollTop += listTop + index * pitch - scroller.clientHeight / 2;
    },
    [pitch],
  );

  const start = windowed ? Math.min(win.start, count) : 0;
  const end = windowed ? Math.min(Math.max(win.end, start), count) : count;
  return {
    listRef,
    start,
    end,
    before: windowed ? start * pitch : 0,
    after: windowed ? Math.max(0, count - end) * pitch : 0,
    scrollToRow,
  };
}
