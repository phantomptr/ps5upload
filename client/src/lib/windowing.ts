/**
 * Fixed-row-height list windowing: which rows of a long list are worth
 * rendering for a scroll position. Long logs (the kernel log keeps 5,000
 * lines) re-rendered every row on every one-second poll; with this only the
 * rows on screen plus a margin are in the DOM.
 */
export interface WindowRange {
  /** First row to render (inclusive). */
  start: number;
  /** Row after the last one to render (exclusive). */
  end: number;
}

export function windowRange(
  scrollTop: number,
  viewportHeight: number,
  rowHeight: number,
  count: number,
  overscan = 20,
): WindowRange {
  if (count <= 0 || rowHeight <= 0) return { start: 0, end: 0 };
  // Before the first layout the viewport is unknown: render one screenful.
  const vh = viewportHeight > 0 ? viewportHeight : rowHeight * 50;
  const first = Math.floor(Math.max(0, scrollTop) / rowHeight);
  const visibleRows = Math.ceil(vh / rowHeight);
  const start = Math.max(0, Math.min(count - 1, first) - overscan);
  const end = Math.min(count, first + visibleRows + overscan);
  return { start, end: Math.max(end, start) };
}
