/**
 * Where focus goes back to when a modal (confirm dialog, actions sheet, path
 * picker) closes, or when the focused row disappears (deleted, moved away).
 *
 * The opener is remembered with its place: the [data-nav-list] it sits in and
 * the index of its [data-nav-item] there. If the opener is gone, focus goes to
 * the item now at that index, else the nearest remaining one (the list got
 * shorter), else the section's first control. Never to the rail: a deleted row
 * must not throw the user out of the folder they are in.
 */
import { useEffect, type RefObject } from "react";
import { FOCUSABLE, focusElement } from "../nav";

export interface FocusAnchor {
  el: HTMLElement | null;
  list: Element | null;
  index: number;
  section: Element | null;
}

export function anchorOf(el: Element | null | undefined): FocusAnchor {
  if (!el || el === document.body) return { el: null, list: null, index: -1, section: null };
  const list = el.closest("[data-nav-list]");
  const item = el.closest("[data-nav-item]");
  let index = -1;
  if (list && item) index = Array.prototype.indexOf.call(list.querySelectorAll("[data-nav-item]"), item) as number;
  return { el: el as HTMLElement, list, index, section: el.closest(".cw-content") };
}

function firstFocusable(within: Element): HTMLElement | null {
  if (within.matches(FOCUSABLE)) return within as HTMLElement;
  return within.querySelector<HTMLElement>(FOCUSABLE);
}

/** Focus the anchor's opener or its stand-in; returns what got focus (null: nothing). */
export function restoreFocus(a: FocusAnchor): HTMLElement | null {
  let target: HTMLElement | null = null;
  if (a.el && a.el.isConnected) target = a.el;
  if (!target && a.list && a.list.isConnected && a.index >= 0) {
    const items = a.list.querySelectorAll("[data-nav-item]");
    if (items.length) target = firstFocusable(items[Math.min(a.index, items.length - 1)]);
  }
  if (!target && a.section && a.section.isConnected) target = firstFocusable(a.section);
  if (target) focusElement(target);
  return target;
}

/** Nothing (or nothing real) has focus: the focused element was removed. */
export function focusLost(): boolean {
  const active = document.activeElement;
  return !active || active === document.body || !active.isConnected;
}

/**
 * The focused control removed itself (Calculate became Cancel, Keep both went
 * away once a free name was found): focus the first of `prefer` found inside
 * `within` (each a selector, in order), else its first control. Nothing happens
 * while focus is still on a live element.
 */
export function refocusIfLost(within: ParentNode | null, prefer: string[]): HTMLElement | null {
  if (!within || !focusLost()) return null;
  for (const sel of prefer) {
    const el = within.querySelector<HTMLElement>(sel);
    if (el && el.matches(FOCUSABLE)) {
      focusElement(el);
      return el;
    }
  }
  const any = within.querySelector<HTMLElement>(FOCUSABLE);
  if (any) focusElement(any);
  return any;
}

/**
 * Where a modal's focus starts: `el` itself when it is a control, else its [data-nav-default], else
 * its first control. A disabled default is passed over: focus() does nothing on it, focus would stay
 * on the page behind and the first ✕ would only pull it in (nav.ts).
 */
export function modalFocusTarget(el: HTMLElement | null): HTMLElement | null {
  if (!el) return null;
  if (el.matches(FOCUSABLE)) return el;
  return el.querySelector<HTMLElement>("[data-nav-default]:not([disabled])") ?? el.querySelector<HTMLElement>(FOCUSABLE);
}

/**
 * A modal closed: focus back to its opener. When the opener went with it and something live already
 * has focus (a sheet closing in the same commit as its confirm put focus back on its own opener), that
 * stays: a stand-in is only for focus that was lost.
 */
export function closeModalFocus(a: FocusAnchor): HTMLElement | null {
  if (!a.el?.isConnected && !focusLost()) return null;
  return restoreFocus(a);
}

/**
 * A modal's focus: on open, remember where focus was and move it to `first`
 * (a control, or a container: see modalFocusTarget); on close, back to the opener.
 */
export function useModalFocus(first: RefObject<HTMLElement | null>): void {
  useEffect(() => {
    const anchor = anchorOf(document.activeElement);
    const target = modalFocusTarget(first.current);
    if (target) focusElement(target);
    return () => {
      closeModalFocus(anchor);
    };
    // Once per open: the ref is stable.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
}
