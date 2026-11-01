import { useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";

import { dropSide } from "../../lib/queueOps";

/** The row attribute the drag looks for under the pointer. */
export const DRAG_ROW_ATTR = "data-drag-row";

/** The nearest ancestor that scrolls vertically (the app's main pane), or null. */
function scrollParent(el: HTMLElement | null): HTMLElement | null {
  for (let p = el?.parentElement ?? null; p; p = p.parentElement) {
    const oy = getComputedStyle(p).overflowY;
    if ((oy === "auto" || oy === "scroll") && p.scrollHeight > p.clientHeight)
      return p;
  }
  return null;
}

/** Near the top or bottom edge of the scrolling pane, scroll it a little, so a long queue
 *  can be dragged through. */
function edgeScroll(scroller: HTMLElement | null, y: number) {
  const top = scroller ? scroller.getBoundingClientRect().top : 0;
  const bottom = scroller
    ? scroller.getBoundingClientRect().bottom
    : window.innerHeight;
  const EDGE = 48;
  const step = y < top + EDGE ? -12 : y > bottom - EDGE ? 12 : 0;
  if (!step) return;
  if (scroller) scroller.scrollTop += step;
  else window.scrollBy(0, step);
}

/**
 * Drag to reorder a list with a handle, for mouse, pen and touch alike (pointer events; the
 * HTML drag-and-drop API has no touch support). The handle captures the pointer; the row
 * under it is found by `data-drag-row`. Escape or a cancelled pointer drops nothing.
 *
 * `order` is the list as shown; `canDrop(id, target)` says whether `id` may take `target`'s
 * place; `onDrop` does the move. Keyboard users keep the row's up/down buttons.
 */
export function useRowDrag({
  order,
  canDrop,
  onDrop,
}: {
  order: readonly string[];
  canDrop: (id: string, target: string) => boolean;
  onDrop: (id: string, target: string) => void;
}) {
  type Drag = { id: string; over: string | null } | null;
  const [drag, setDragState] = useState<Drag>(null);
  // The pointer handlers read the latest drag without waiting for a render.
  const dragRef = useRef<Drag>(null);
  const setDrag = (d: Drag) => {
    dragRef.current = d;
    setDragState(d);
  };
  const scrollerRef = useRef<HTMLElement | null>(null);

  // Escape cancels; text selection stays off while a row is held.
  useEffect(() => {
    if (!drag) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      dragRef.current = null;
      setDragState(null);
    };
    window.addEventListener("keydown", onKey);
    const prev = document.body.style.userSelect;
    document.body.style.userSelect = "none";
    return () => {
      window.removeEventListener("keydown", onKey);
      document.body.style.userSelect = prev;
    };
  }, [drag]);

  const handleProps = (id: string) => ({
    onPointerDown(e: ReactPointerEvent<HTMLElement>) {
      if (e.button !== 0) return;
      e.preventDefault();
      try {
        e.currentTarget.setPointerCapture(e.pointerId);
      } catch {
        // jsdom / an old WebKit: the row still follows while the pointer is over the handle
      }
      scrollerRef.current = scrollParent(e.currentTarget);
      setDrag({ id, over: null });
    },
    onPointerMove(e: ReactPointerEvent<HTMLElement>) {
      const d = dragRef.current;
      if (!d || d.id !== id) return;
      const hit =
        typeof document.elementFromPoint === "function"
          ? document.elementFromPoint(e.clientX, e.clientY)
          : null;
      const row = hit?.closest<HTMLElement>(`[${DRAG_ROW_ATTR}]`) ?? null;
      const target = row?.getAttribute(DRAG_ROW_ATTR) ?? null;
      const over = target && target !== id && canDrop(id, target) ? target : null;
      if (over !== d.over) setDrag({ id, over });
      edgeScroll(scrollerRef.current, e.clientY);
    },
    onPointerUp() {
      const d = dragRef.current;
      setDrag(null);
      if (d?.over) onDrop(d.id, d.over);
    },
    onPointerCancel() {
      setDrag(null);
    },
  });

  return {
    dragId: drag?.id ?? null,
    overId: drag?.over ?? null,
    /** Where the drop line goes on row `id` while it is the target. */
    sideOf(id: string): "before" | "after" | null {
      if (!drag?.over || drag.over !== id) return null;
      return dropSide(order, drag.id, id);
    },
    handleProps,
  };
}
