import { create } from "zustand";

/** Hardware back-button stack (v5 §18.3 / mobile-design §12).
 *
 *  On Android, the hardware back button (and the gesture nav pill) needs
 *  deterministic behavior: close modals before menus before navigating
 *  up before exiting. This store is the explicit stack of "back intents"
 *  that the Android back-button handler drains.
 *
 *  Every push of a modal/sheet/palette/context-menu pushes an entry.
 *  Every sub-navigation pushes a `navigate` entry. Files directory
 *  changes push `fs-up`. Game Hub tab changes push `hub-tab-back`. Etc.
 *
 *  The handler (wired in AppShell or a Tauri plugin) calls `popAndRun()`,
 *  which removes the top entry and returns it so the caller can execute
 *  the appropriate close/navigate action. Returns null when the stack is
 *  empty, signaling the OS to minimize the app.
 *
 *  NOT persisted — the back stack is strictly session state. */

export type BackEntry =
  | { kind: "close-modal"; id: string }
  | { kind: "close-sheet"; id: string }
  | { kind: "close-palette" }
  | { kind: "close-context-menu" }
  | { kind: "exit-multi-select" }
  | { kind: "navigate"; to: string; state?: unknown }
  | { kind: "fs-up" }
  | { kind: "hub-tab-back" }
  | { kind: "console-section-back" }
  | { kind: "app-exit" };

interface BackStackState {
  stack: BackEntry[];
  /** Push one or more entries onto the stack. Most callers push one;
   *  the variadic form lets a navigation push "navigate up" + "close
   *  this menu" atomically. */
  push: (...entries: BackEntry[]) => void;
  /** Read the top entry without removing it. Returns null if empty. */
  top: () => BackEntry | null;
  /** Remove and return the top entry. Returns null if empty. */
  pop: () => BackEntry | null;
  /** Remove the topmost entry matching `kind` (and everything above it).
   *  Used when e.g. a modal closes itself normally — it pops its own
   *  entry without running the back-button handler. No-op if no match. */
  remove: (kind: BackEntry["kind"]) => void;
  /** Clear the entire stack (e.g. on tab switch, which resets context). */
  clear: () => void;
  /** Current depth (stack.length). Useful for debugging / assertions. */
  depth: () => number;
}

export const useBackStackStore = create<BackStackState>((set, get) => ({
  stack: [],

  push: (...entries) => {
    if (entries.length === 0) return;
    set((s) => ({ stack: [...s.stack, ...entries] }));
  },

  top: () => {
    const { stack } = get();
    return stack.length === 0 ? null : stack[stack.length - 1];
  },

  pop: () => {
    const { stack } = get();
    if (stack.length === 0) return null;
    const entry = stack[stack.length - 1];
    set({ stack: stack.slice(0, -1) });
    return entry;
  },

  remove: (kind) => {
    const { stack } = get();
    const idx = [...stack].reverse().findIndex((e) => e.kind === kind);
    if (idx === -1) return;
    const realIdx = stack.length - 1 - idx;
    set({ stack: [...stack.slice(0, realIdx), ...stack.slice(realIdx + 1)] });
  },

  clear: () => set({ stack: [] }),

  depth: () => get().stack.length,
}));
