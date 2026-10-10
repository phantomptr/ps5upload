/**
 * Window drops on the Payloads screen.
 *
 * Tauri's drag-drop is one window-wide event with no element targeting, and the
 * Send form and the playlists' drop zone both listen to it. A drop on the
 * playlist zone belongs to the playlist only; without this the same drop also
 * replaced the Send form's file.
 */

/** Marks an element that handles its own drops; window-wide listeners skip them. */
export const OWN_DROP_ATTR = "data-own-drop";

type Point = { x: number; y: number };
type Rect = { left: number; right: number; top: number; bottom: number };

/** Whether Tauri's drop position (PHYSICAL pixels) falls inside a CSS-pixel rect. */
export function physicalPointInRect(pos: Point, rect: Rect, dpr: number): boolean {
  const scale = dpr || 1;
  const x = pos.x / scale;
  const y = pos.y / scale;
  return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
}

/** Whether a drop at `pos` lands on an element that handles its own drops. */
export function dropIsOwnedElsewhere(pos: Point): boolean {
  if (typeof document === "undefined") return false;
  const dpr = typeof window === "undefined" ? 1 : window.devicePixelRatio;
  return Array.from(document.querySelectorAll(`[${OWN_DROP_ATTR}]`)).some((el) =>
    physicalPointInRect(pos, el.getBoundingClientRect(), dpr),
  );
}
