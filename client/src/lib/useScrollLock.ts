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
/** Per scroll root: how many overlays hold it, and the overflow to put back. Keyed on the
 *  element locked, never "the root on show now": each console keeps its own screens (and its
 *  own scroll root) alive, so a lock taken on one console and released after a switch used to
 *  restore the OTHER console's root and leave this one stuck at overflow:hidden, unscrollable
 *  until the app restarted. */
type Lockable = { style: { overflow: string } };
const locks = new Map<Lockable, { count: number; previous: string }>();

/** Lock `root` for one overlay; the returned function releases that same root. */
export function lockScrollRoot(root: Lockable): () => void {
  const held = locks.get(root);
  if (held) {
    held.count += 1;
  } else {
    locks.set(root, { count: 1, previous: root.style.overflow });
    root.style.overflow = "hidden";
  }
  let released = false;
  return () => {
    if (released) return;
    released = true;
    const l = locks.get(root);
    if (!l) return;
    l.count -= 1;
    if (l.count <= 0) {
      locks.delete(root);
      root.style.overflow = l.previous;
    }
  };
}

/** For tests. */
export function activeScrollLocks(): number {
  let n = 0;
  for (const l of locks.values()) n += l.count;
  return n;
}

/** Of the elements marked as scroll root, the one on show. Screens kept alive behind the
 *  current one (another screen, or another console's whole tree) are `display: none` and so
 *  have no boxes; one of them can still carry the marker for a moment, because React
 *  re-renders hidden trees late. */
export function pickScrollRoot<T extends { getClientRects(): { length: number } }>(
  candidates: readonly T[],
): T | null {
  return candidates.find((c) => c.getClientRects().length > 0) ?? candidates[0] ?? null;
}

function scrollRoot(): HTMLElement | null {
  return pickScrollRoot(
    Array.from(document.querySelectorAll<HTMLElement>("[data-scroll-root]")),
  );
}

/** `owner` (optional): an element of the overlay. A screen kept alive behind another (another
 *  console's, while it uploads) can open a dialog by itself; locking "the root on show" then
 *  froze the screen the user was looking at. With an owner, a hidden overlay locks nothing,
 *  and a shown one locks the scroll root it sits in. */
export function useScrollLock(
  active = true,
  owner?: { current: HTMLElement | null },
): void {
  useEffect(() => {
    if (!active || typeof document === "undefined") return;
    const el = owner?.current ?? null;
    if (el && el.getClientRects().length === 0) return;
    const root = el?.closest<HTMLElement>("[data-scroll-root]") || scrollRoot();
    if (!root) return;
    return lockScrollRoot(root);
  }, [active, owner]);
}
