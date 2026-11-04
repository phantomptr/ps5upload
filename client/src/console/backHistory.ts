/**
 * The PS5 browser's ○ is its own Back button: it never reaches the page as a
 * key (measured on 13.60: only a keyup Escape as the page was left). So every
 * in-page "back" step — a modal or sheet open, a folder entered in Files, a
 * section other than Home — is backed by browser history: while the page has
 * anywhere to go back to, one history entry (the guard) sits on top of the
 * page's own entry. ○ pops it, `popstate` runs the page's back action (close
 * the top modal, else up a folder, else rail, else Home), and the guard is put
 * back while there is still somewhere to go. On the first screen (Home, no
 * modal) there is no guard, so ○ leaves the page as the browser intends.
 *
 * One guard instead of one entry per step: the page also closes modals and
 * leaves folders with on-screen buttons, and a per-step stack would drift
 * from the page's real state; a single guard is right whatever the depth.
 * A level closed by a button with nothing left behind removes the guard
 * (history.back()), and that pop is ignored.
 *
 * Escape, Backspace and a gamepad ○ call requestBack(): history.back() when
 * the guard is up (so they take exactly the path ○ takes), else the back
 * action directly.
 *
 * On the first screen (Home, no level open) there is never a guard, so ○ there is always the
 * browser's own: with nothing to go back to it leaves the page for the PS5's home screen. WebKit
 * closes a page it did not open by script only while its back/forward list holds one entry
 * ("Can't close the window since it was not opened by JavaScript" otherwise; one pushState, even
 * popped again, is enough to refuse it for the page's lifetime: measured in WebKit). #480 kept a
 * guard on the first screen once the history was deep so ○ there could try window.close(); but
 * that close is exactly the one WebKit refuses, so the first ○ at Home only showed "press the PS
 * button" and it took a second ○ to leave. A user on 13.20 read that as "○ cannot exit" (Discord).
 * Escape, Backspace and a gamepad ○ (requestBack, which has no browser Back behind it) still call
 * onRoot on the first screen (window.close(), allowed while the history is a single entry), once
 * until a level opens again; after that they run the page's back action.
 *
 * WebKit can fire a popstate as the page loads that pops nothing. Such a pop
 * leaves history on the guard entry (atGuard) and is ignored; and for
 * opts.graceMs after start, before any key or click, every pop only puts the
 * guard back (Payload Manager's userNavigated flag + 1.5 s grace), so neither
 * a tour opened at load nor the page itself is closed by it.
 */
import { useEffect } from "react";
import { isPs5Browser } from "./fullscreen";

/** A second pop this soon after a back action is the same press reported twice: ignored. */
export const BACK_ECHO_MS = 150;

export interface HistoryEnv {
  /** history.pushState(guard) */
  push(): void;
  /** history.back() */
  back(): void;
  /** Listen for popstate; returns the unsubscribe function. */
  onPop(fn: () => void): () => void;
  now(): number;
  /** Run `fn` soon (after the current React commit). */
  defer(fn: () => void): void;
  /** The page loaded with the guard on top (a reload while it was up). */
  startsOnGuard(): boolean;
  /** History's current entry is the guard (a popstate that popped nothing). Optional. */
  atGuard?(): boolean;
  /** history.length: the entries back and forward (diagnostics; the guard no longer depends on it). */
  length?(): number;
}

export interface BackHistoryOptions {
  /** Escape / Backspace / gamepad ○ on the first screen (no level open): runs instead of the back action, once. */
  onRoot?: () => void;
  /** For this long after start, until noteInput(), a pop only puts the guard back. */
  graceMs?: number;
}

export interface BackHistory {
  /** One more place to go back from; call the returned function when it closes. */
  addLevel(): () => void;
  /** Escape / Backspace / gamepad ○: the same path as the browser's Back. */
  requestBack(): void;
  /** The guard entry is on top (tests). */
  hasGuard(): boolean;
  /** The user pressed a key or clicked: pops are real from now on (ends the grace). */
  noteInput(): void;
  dispose(): void;
}

export function createBackHistory(env: HistoryEnv, runBack: () => void, opts: BackHistoryOptions = {}): BackHistory {
  let levels = 0;
  let guard = env.startsOnGuard();
  let ignorePops = 0;
  let backPending = false;
  let lastBackAt = -Infinity;
  let scheduled = false;
  const startedAt = env.now();
  const graceMs = opts.graceMs ?? 0;
  let userInput = false;
  /** onRoot ran (a key or gamepad ○ on the first screen) and the page is still here: such a press runs the back action until a level opens. */
  let rootTried = false;

  /** A guard only while a level is open: the first screen leaves ○ to the browser (see the module comment). */
  const wantGuard = () => levels > 0;
  const sync = () => {
    scheduled = false;
    if (wantGuard() && !guard) {
      env.push();
      guard = true;
    } else if (!wantGuard() && guard) {
      guard = false;
      ignorePops++;
      env.back();
    }
  };
  const schedule = () => {
    if (scheduled) return;
    scheduled = true;
    env.defer(sync);
  };

  const off = env.onPop(() => {
    backPending = false;
    if (env.atGuard?.()) {
      guard = true; // nothing was popped (WebKit's load-time popstate)
      return;
    }
    guard = false; // the guard entry was just consumed
    if (ignorePops > 0) {
      ignorePops--;
      schedule();
      return;
    }
    const now = env.now();
    if (!userInput && now - startedAt < graceMs) {
      schedule();
      return;
    }
    if (now - lastBackAt < BACK_ECHO_MS) {
      schedule();
      return;
    }
    lastBackAt = now;
    if (levels === 0) {
      // The guard of a level that just closed, popped before it was taken down: nothing to step back from.
      schedule();
      return;
    }
    runBack();
    schedule();
  });
  if (guard || wantGuard()) schedule();

  return {
    addLevel() {
      levels++;
      rootTried = false;
      schedule();
      let closed = false;
      return () => {
        if (closed) return;
        closed = true;
        levels--;
        schedule();
      };
    },
    requestBack() {
      // Keyboard and native browser Back can describe the same press before popstate.
      if (backPending) return;
      userInput = true;
      if (guard && ignorePops === 0) {
        backPending = true;
        env.back();
        return;
      }
      lastBackAt = env.now();
      if (levels === 0 && opts.onRoot && !rootTried) {
        // The first screen with no guard (a single-entry history): leave, as ○ would.
        rootTried = true;
        opts.onRoot();
        return;
      }
      runBack();
      schedule();
    },
    hasGuard: () => guard,
    noteInput() {
      userInput = true;
    },
    dispose: off,
  };
}

/* ---- the page's instance ---- */

const GUARD = "cwBack";
let instance: BackHistory | null = null;

function isGuardState(): boolean {
  const st: unknown = window.history.state;
  return !!st && typeof st === "object" && (st as Record<string, unknown>)[GUARD] === 1;
}

/** WebKit's load-time popstate is ignored for this long (until a key or click). */
export const LOAD_GRACE_MS = 1500;

/** Fired on window when ○ on the first screen could not leave the page (the browser refused to close it). */
export const CLOSE_REFUSED_EVENT = "cw-close-refused";
/** How long a refused window.close() takes to show: the page is still visible after this. */
export const CLOSE_CHECK_MS = 600;

/**
 * Close the browser (PS5: back to the home screen). Never throws. The PS5 browser refuses once
 * the page has more than one history entry, and a page cannot send the console home by itself:
 * when the page is still showing a moment later, CLOSE_REFUSED_EVENT tells the user to press
 * the PS button.
 */
function closeWindow(): void {
  try {
    window.close();
  } catch {
    /* not closable: the page stays */
  }
  setTimeout(() => {
    if (!window.closed && document.visibilityState === "visible") {
      window.dispatchEvent(new Event(CLOSE_REFUSED_EVENT));
    }
  }, CLOSE_CHECK_MS);
}

let offInput: (() => void) | null = null;

/** Install on the real window (main.tsx). `runBack` is the page's back action. */
export function installBackHistory(runBack: () => void): BackHistory {
  instance?.dispose();
  offInput?.();
  instance = createBackHistory(
    {
      push: () => window.history.pushState({ [GUARD]: 1 }, ""),
      back: () => window.history.back(),
      onPop: (fn) => {
        window.addEventListener("popstate", fn);
        return () => window.removeEventListener("popstate", fn);
      },
      now: () => performance.now(),
      defer: (fn) => {
        setTimeout(fn, 0);
      },
      startsOnGuard: isGuardState,
      atGuard: isGuardState,
      length: () => window.history.length,
    },
    runBack,
    { onRoot: isPs5Browser() ? closeWindow : undefined, graceMs: LOAD_GRACE_MS },
  );
  const h = instance;
  const note = () => h.noteInput();
  window.addEventListener("keydown", note, true);
  window.addEventListener("mousedown", note, true);
  window.addEventListener("touchstart", note, true);
  offInput = () => {
    window.removeEventListener("keydown", note, true);
    window.removeEventListener("mousedown", note, true);
    window.removeEventListener("touchstart", note, true);
  };
  return instance;
}

/** Escape / Backspace / gamepad ○ (nav.ts): history.back() when installed, else `fallback`. */
export function requestBack(fallback: () => void): void {
  if (instance) instance.requestBack();
  else fallback();
}

/** While `on`, this component is a place the browser's Back returns from. No-op until installed. */
export function useBackLevel(on: boolean): void {
  useEffect(() => {
    if (!on || !instance) return;
    return instance.addLevel();
  }, [on]);
}
