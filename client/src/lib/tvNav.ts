// Android TV and Google TV (#897): a remote has a D-pad, OK and Back, and no pointer. The WebView
// turns the D-pad into arrow keys, which only scroll the page, and OK into Enter, which presses
// the focused control. This moves focus to the nearest control in the arrow's direction, the
// way a TV app does, so every button, link and row (the tab bar too) can be reached.
//
// Only on a TV: MainActivity exposes PS5UploadTv (step 16 of
// scripts/release/android-postinit-patch.sh). Back is the system Back (lib/systemBack).
// A control that uses the arrows itself (a menu, a list of rows, a table, the Remote Play
// picture) calls preventDefault and keeps them; text fields keep Left and Right for the caret.

import { nextFocus, type Direction, type FocusCandidate } from "../console/nav";
import { FOCUSABLE_SELECTOR, openModalCount } from "./focusTrap";

declare global {
  interface Window {
    /** MainActivity's bridge, on Android only. */
    PS5UploadTv?: { isTv(): boolean };
  }
}

const DIRECTIONS: Record<string, Direction> = {
  ArrowUp: "up",
  ArrowDown: "down",
  ArrowLeft: "left",
  ArrowRight: "right",
};

/** Text-like inputs, where Left and Right move the caret. */
const TEXT_INPUT = /^(text|search|email|url|tel|password|number|date|time|datetime-local|month|week)$/;

/** Whether this device is a TV (the Android app on Android TV or Google TV). */
export function isTvDevice(w: Window | undefined = typeof window === "undefined" ? undefined : window): boolean {
  try {
    return !!w?.PS5UploadTv?.isTv();
  } catch {
    return false;
  }
}

/** Whether D-pad navigation is on (installTvNav ran on a TV). */
export function tvNavActive(doc: Document | undefined = typeof document === "undefined" ? undefined : document): boolean {
  return doc?.documentElement.dataset.tv === "1";
}

/** Whether the focused element keeps this arrow for itself. */
export function keepsArrow(el: Element | null, dir: Direction): boolean {
  if (!el) return false;
  if (el.closest("[data-own-arrows]")) return true;
  const tag = el.tagName;
  if (tag === "TEXTAREA") return true;
  if ((el as HTMLElement).isContentEditable) return true;
  const horizontal = dir === "left" || dir === "right";
  if (tag === "INPUT") {
    const type = ((el as HTMLInputElement).type || "text").toLowerCase();
    // A slider moves with Left and Right; the caret too.
    if (type === "range" || TEXT_INPUT.test(type)) return horizontal;
  }
  return false;
}

const DIALOG = '[aria-modal="true"], [role="dialog"], [role="alertdialog"]';

/** Where focus may go: inside the open dialog when there is one, else the whole page. */
export function navRoot(doc: Document): ParentNode {
  const own = doc.activeElement?.closest<HTMLElement>(DIALOG);
  if (own) return own;
  if (openModalCount() > 0) {
    const dialogs = doc.querySelectorAll<HTMLElement>(DIALOG);
    if (dialogs.length) return dialogs[dialogs.length - 1];
  }
  return doc;
}

/** The visible controls under `root`. */
export function candidates(root: ParentNode): FocusCandidate[] {
  const out: FocusCandidate[] = [];
  root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR).forEach((el) => {
    if (el.closest("[inert], [aria-hidden='true']")) return;
    if ((el as HTMLButtonElement).disabled) return;
    const rect = el.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    out.push({ el, rect });
  });
  return out;
}

/** Where the first press lands: the current page's tab, else the first control. */
function startAt(all: FocusCandidate[]): HTMLElement | null {
  const current = all.find((c) => c.el.getAttribute("aria-current") === "page");
  return (current ?? all[0])?.el ?? null;
}

/**
 * Handle one keydown. True when it moved focus (the key is used up). At the edge, or for a key
 * that is not an arrow, it does nothing and the page scrolls as usual.
 */
export function handleTvKey(e: KeyboardEvent, doc: Document = document): boolean {
  const dir = DIRECTIONS[e.key];
  if (!dir || e.defaultPrevented || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return false;
  const active = doc.activeElement;
  if (keepsArrow(active, dir)) return false;
  const all = candidates(navRoot(doc));
  if (!all.length) return false;
  const current = all.find((c) => c.el === active);
  const next = current
    ? nextFocus(
        current.rect,
        all.filter((c) => c !== current),
        dir,
      )
    : startAt(all);
  if (!next) return false;
  e.preventDefault();
  next.focus();
  if (typeof next.scrollIntoView === "function") next.scrollIntoView({ block: "nearest", inline: "nearest" });
  return true;
}

let installed = false;

/** Once, at start-up: on a TV, arrows move focus. Returns whether it is on. */
export function installTvNav(w: Window | undefined = typeof window === "undefined" ? undefined : window): boolean {
  if (installed || !w || !isTvDevice(w)) return installed;
  installed = true;
  w.document.documentElement.dataset.tv = "1";
  // Bubble phase: controls that use the arrows themselves have had them first.
  w.addEventListener("keydown", (e) => void handleTvKey(e, w.document));
  return true;
}
