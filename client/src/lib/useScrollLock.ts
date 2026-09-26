import { useEffect } from "react";

/**
 * Lock background scrolling while an overlay (modal, drawer, command palette,
 * dialog) is open.
 *
 * Why this is needed: the app's main scroll area is the per-route
 * `[data-scroll-root]` container in AppShell, not the body (the body is
 * `overflow: hidden` globally). An *inline*-rendered modal — one whose scrim
 * lives inside that scroll container rather than a portal to <body> — leaves
 * the container as a scrollable ancestor of the scrim, so a wheel/touch scroll
 * over the scrim still moves the page behind it. Portaled modals don't have
 * this problem, but several modals render inline by design (the `.anim-screen`
 * `backwards` fill-mode keeps `position: fixed` correct for them). This hook
 * makes the behaviour uniform.
 *
 * Implementation: ref-counted so stacked overlays (a confirm dialog on top of a
 * modal) don't unlock prematurely — the scroll root is only restored when the
 * last lock releases. Best-effort: if the scroll root isn't present (e.g. a
 * pre-shell screen) it simply no-ops.
 *
 * @param active pass `false` to keep the hook mounted but inactive (e.g. an
 *   overlay component that's rendered but closed).
 */
let lockCount = 0;
let previousOverflow = "";

function scrollRoot(): HTMLElement | null {
  return document.querySelector<HTMLElement>("[data-scroll-root]");
}

export function useScrollLock(active = true): void {
  useEffect(() => {
    if (!active || typeof document === "undefined") return;
    const root = scrollRoot();
    if (!root) return;
    if (lockCount === 0) {
      previousOverflow = root.style.overflow;
      root.style.overflow = "hidden";
    }
    lockCount += 1;
    return () => {
      lockCount = Math.max(0, lockCount - 1);
      if (lockCount === 0) {
        // Re-query: the route may have changed and recreated the node.
        const r = scrollRoot();
        if (r) r.style.overflow = previousOverflow;
      }
    };
  }, [active]);
}
