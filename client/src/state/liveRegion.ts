import { create } from "zustand";

interface LiveRegionState {
  message: string;
  announce: (message: string) => void;
  clear: () => void;
}

/**
 * Global polite live region store. Any component can call
 * `useLiveRegionStore.getState().announce("...")` to push a message
 * that screen readers will read aloud (polite — waits for SR to finish
 * speaking, doesn't interrupt).
 *
 * The `<LiveRegion>` component (mounted once at app root) renders the
 * actual `aria-live` element.
 *
 * Use for: upload completed, task failed, settings saved — status
 * updates that don't have a visible focusable element to anchor them.
 */
export const useLiveRegionStore = create<LiveRegionState>((set) => ({
  message: "",
  announce: (message) => set({ message }),
  clear: () => set({ message: "" }),
}));
