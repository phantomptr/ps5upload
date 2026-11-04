// System Back on a phone: close what is open on top first. Modals, drawers, confirm dialogs and
// pickers close on Escape (lib/focusTrap), which a phone never sends. So Back only moved the
// history, and the page changed behind a dialog that stayed up.
//
// The Android app asks window.__ps5uBack first (MainActivity, step 14 of
// scripts/release/android-postinit-patch.sh). A phone's browser (the web UI) gives a page no Back
// event, only history: an extra entry while something is open makes Back land here instead of
// changing the route.

import { useEffect, useRef } from "react";

import { closeTopModal, onModalStackChange, openModalCount, registerBackDismiss } from "./focusTrap";
import { isMobile } from "./platform";
import { isTauriEnv } from "./tauriEnv";

declare global {
  interface Window {
    /** True when the press was used up. */
    __ps5uBack?: () => boolean;
  }
}

/** The app: the shell calls this, and moves the history itself when it answers false. */
export function installAppBack(w: Window = window): void {
  w.__ps5uBack = () => closeTopModal();
}

/** A browser: returns the undo. */
export function installBrowserBack(w: Window = window): () => void {
  /** Our extra history entry exists. */
  let held = false;
  let heldAt = "";
  let settle: ReturnType<typeof setTimeout> | null = null;

  const reconcile = () => {
    settle = null;
    const open = openModalCount() > 0;
    if (open && !held) {
      // Same state as the page's own entry: the router sees no navigation.
      w.history.pushState(w.history.state, "");
      held = true;
      heldAt = w.location.href;
    } else if (!open && held) {
      held = false;
      // Not after a route change (a link inside the dialog): that is someone else's entry now.
      if (w.location.href === heldAt) w.history.back();
    }
  };
  // On the next turn: a confirm that closes and opens a Modal straight away must not go back and
  // forth through the history.
  const schedule = () => {
    if (settle === null) settle = setTimeout(reconcile, 0);
  };
  const onPop = () => {
    if (!held) return;
    held = false;
    closeTopModal();
    // Something still open (or one that refused Escape): Back stays with it.
    schedule();
  };
  w.addEventListener("popstate", onPop);
  const off = onModalStackChange(schedule);
  return () => {
    off();
    w.removeEventListener("popstate", onPop);
    if (settle !== null) clearTimeout(settle);
  };
}

/** While `open`, system Back closes this first (a sheet or menu that closes on its own Escape
 *  handler, not through the modal stack). */
export function useBackDismiss(open: boolean, close: () => void): void {
  const latest = useRef(close);
  useEffect(() => {
    latest.current = close;
  });
  useEffect(() => {
    if (!open) return;
    return registerBackDismiss(() => latest.current());
  }, [open]);
}

let installed = false;

/** Once, at start-up. */
export function installSystemBack(): void {
  if (installed || typeof window === "undefined") return;
  installed = true;
  if (isTauriEnv()) installAppBack();
  else if (isMobile()) installBrowserBack();
}
