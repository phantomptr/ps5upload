// The Linux app's picture laid out again after its window comes back (#652).
//
// After the window was minimized and maximized again, the Linux app (WebKitGTK) showed the
// stream with black bars on the right and bottom: the picture kept the size it had before, from
// the top-left corner, in a box that had grown. The picture's size is all CSS (index.css
// .rp-picture: container units against its place), so nothing of ours holds the old size;
// WebKit did not lay the canvas out again while the window was not drawn. Once the window has
// settled after a resize, or is shown again, the canvas is taken out of the layout and put back,
// which makes WebKit size it, and its layer, afresh.

/** How long a window's resizes must stop for before the picture is laid out again. */
export const RESTORE_SETTLE_MS = 200;

type Win = Pick<Window, "addEventListener" | "removeEventListener" | "setTimeout" | "clearTimeout"> & {
  document?: Pick<Document, "visibilityState">;
};

/** Calls `relayout` once `win` settles after a resize or is shown again; returns the stop. */
export function watchRestore(win: Win, relayout: () => void): () => void {
  let timer: number | null = null;
  const soon = () => {
    if (timer !== null) win.clearTimeout(timer);
    timer = win.setTimeout(() => {
      timer = null;
      relayout();
    }, RESTORE_SETTLE_MS) as unknown as number;
  };
  const onVis = () => {
    if (win.document?.visibilityState !== "hidden") soon();
  };
  win.addEventListener("resize", soon);
  win.addEventListener("focus", soon);
  win.addEventListener("visibilitychange", onVis);
  return () => {
    if (timer !== null) win.clearTimeout(timer);
    timer = null;
    win.removeEventListener("resize", soon);
    win.removeEventListener("focus", soon);
    win.removeEventListener("visibilitychange", onVis);
  };
}

/** Takes `el` out of the layout and puts it back (WebKit sizes it and its layer anew). */
export function relayoutElement(el: HTMLElement): void {
  const display = el.style.display;
  el.style.display = "none";
  void el.offsetWidth;
  el.style.display = display;
}
