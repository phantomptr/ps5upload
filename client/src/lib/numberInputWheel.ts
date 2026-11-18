/** A turn of the mouse wheel over a focused number box steps its value in Chromium-based
 *  webviews (the Windows app's WebView2, Chrome for the web UI), not in Safari/WebKit. The
 *  upload speed limit box steps by 0.5, so scrolling the Settings page past it with the box
 *  still focused quietly set a limit of 1.5 MB/s on Windows while the Mac stayed unlimited.
 *  Installed once on the document: the wheel takes the focus off a number box under it, so the
 *  page scrolls and the value stays. Typing and the arrow keys still change it. */
export function installNumberInputWheelGuard(doc: Document = document): () => void {
  const onWheel = (e: Event) => {
    const t = e.target as { tagName?: string; type?: string; blur?: () => void } | null;
    if (t && t.tagName === "INPUT" && t.type === "number" && doc.activeElement === (t as unknown as Element)) t.blur?.();
  };
  doc.addEventListener("wheel", onWheel, { capture: true, passive: true });
  return () => doc.removeEventListener("wheel", onWheel, { capture: true });
}
