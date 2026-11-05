/**
 * Keyboard focus trap shared by every modal surface (Modal, Drawer, the
 * confirm/alert/prompt dialogs, the command palette, the folder picker).
 *
 * They all set `aria-modal="true"`, which tells assistive tech the page behind
 * is inert, but nothing made it so for the keyboard: Tab walked straight out
 * of the dialog into the controls under the scrim, where Enter could trigger a
 * button the user couldn't see. This keeps Tab and Shift+Tab cycling inside the
 * panel.
 *
 * Only the edges are handled (last -> first, first -> last, and focus that has
 * already left the panel). Moves between two controls inside the panel are left
 * to the browser so native tab order, radio groups and the like keep working.
 */

/** What Tab can land on. `[tabindex="-1"]` is focusable by script only. */
export const FOCUSABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled]):not([type='hidden'])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "summary",
  "[contenteditable='true']",
  "[tabindex]",
]
  .map((s) => `${s}:not([tabindex='-1'])`)
  .join(", ");

/** The controls inside `container` that Tab can reach, in document order. */
export function focusableWithin(container: HTMLElement): HTMLElement[] {
  return Array.from(
    container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
    // A control inside a `display: none` branch has no boxes and can't take
    // focus; counting it as the "last" one would break the wrap.
  ).filter((el) => el.getClientRects().length > 0);
}

/**
 * Where a Tab press should be sent, or null to let the browser move focus.
 *
 * @param items the panel's tabbable controls, in order
 * @param active whatever holds focus now
 * @param backwards true for Shift+Tab
 * @param inside whether `active` is the panel or inside it
 */
export function wrapTarget<T>(
  items: readonly T[],
  active: unknown,
  backwards: boolean,
  inside: boolean,
): T | null {
  if (items.length === 0) return null;
  const first = items[0];
  const last = items[items.length - 1];
  if (!inside) return backwards ? last : first;
  if (backwards) {
    // From the first control, or from the panel itself (it is focusable by
    // script and sits before every control), Shift+Tab would leave.
    if (active === first || !items.includes(active as T)) return last;
    return null;
  }
  return active === last ? first : null;
}

type TabKeyEvent = Pick<KeyboardEvent, "key" | "shiftKey" | "preventDefault">;

/**
 * Handle one keydown for a trapped panel. Returns true when it moved focus
 * (and called `preventDefault`), false when the key was left alone.
 */
export function trapTabKey(e: TabKeyEvent, container: HTMLElement): boolean {
  if (e.key !== "Tab") return false;
  const active = container.ownerDocument.activeElement;
  const inside = active != null && container.contains(active);
  const items = focusableWithin(container);
  if (items.length === 0) {
    // Nothing to tab to: keep focus on the panel rather than letting it out.
    e.preventDefault();
    container.focus({ preventScroll: true });
    return true;
  }
  const target = wrapTarget(items, active, e.shiftKey, inside);
  if (!target) return false;
  e.preventDefault();
  target.focus({ preventScroll: true });
  return true;
}

type KeyTarget = {
  addEventListener: (type: "keydown", fn: (e: KeyboardEvent) => void) => void;
  removeEventListener: (type: "keydown", fn: (e: KeyboardEvent) => void) => void;
};

/** Open traps, oldest first. A confirm opened from inside a Modal is a second
 *  entry; only the last one acts, or the Modal underneath would pull focus out
 *  of the confirm on every Tab. The same stack decides who owns Escape: every
 *  modal surface listens on `window`, so without it one Escape closed the
 *  confirm AND the Modal beneath it. */
const stack: symbol[] = [];
const stackListeners = new Set<() => void>();
/** What system Back closes, newest last: modal surfaces (by an Escape press) and the lighter
 *  surfaces that are not modal but do cover the page, such as a sheet or a menu. */
const backOrder: Array<() => void> = [];

function stackChanged(): void {
  for (const l of stackListeners) l();
}

function pressEscape(): void {
  const from = document.activeElement ?? document;
  from.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
}

/** How many surfaces system Back would close. */
export function openModalCount(): number {
  return backOrder.length;
}

/** Called whenever a modal surface opens or closes; returns the undo. */
export function onModalStackChange(listener: () => void): () => void {
  stackListeners.add(listener);
  return () => stackListeners.delete(listener);
}

/** Close what is on top, as an Escape press would: phones have no Escape key (lib/systemBack).
 *  False when nothing is open. A surface that refuses Escape stays, and the press is still spent. */
export function closeTopModal(): boolean {
  const top = backOrder[backOrder.length - 1];
  if (!top) return false;
  top();
  return true;
}

/** A surface that is not modal (a sheet, a menu) but that system Back should close first. Returns
 *  the undo. Escape stays its own business. */
export function registerBackDismiss(close: () => void): () => void {
  backOrder.push(close);
  stackChanged();
  return () => {
    const at = backOrder.lastIndexOf(close);
    if (at !== -1) backOrder.splice(at, 1);
    stackChanged();
  };
}

export type FocusTrapOptions = {
  /** Called for an Escape press while this is the topmost modal surface.
   *  Skipped when something inside already handled the key (called
   *  `preventDefault`, e.g. a menu or an inline rename closing itself). */
  onEscape?: (e: KeyboardEvent) => void;
};

/**
 * Trap Tab inside the element `getContainer` returns until the returned
 * function is called. Call it from the effect that opens the dialog and return
 * the result as (part of) the cleanup.
 */
export function activateFocusTrap(
  getContainer: () => HTMLElement | null,
  target: KeyTarget = window,
  options: FocusTrapOptions = {},
): () => void {
  const id = Symbol("focus-trap");
  stack.push(id);
  backOrder.push(pressEscape);
  stackChanged();
  const handler = (e: KeyboardEvent) => {
    if (stack[stack.length - 1] !== id) return;
    if (e.key === "Escape") {
      if (options.onEscape && !e.defaultPrevented) options.onEscape(e);
      return;
    }
    const container = getContainer();
    if (container) trapTabKey(e, container);
  };
  target.addEventListener("keydown", handler);
  return () => {
    target.removeEventListener("keydown", handler);
    const at = stack.indexOf(id);
    if (at !== -1) stack.splice(at, 1);
    const inOrder = backOrder.lastIndexOf(pressEscape);
    if (inOrder !== -1) backOrder.splice(inOrder, 1);
    stackChanged();
  };
}
