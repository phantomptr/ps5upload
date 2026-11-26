import { useSyncExternalStore } from "react";

/** The phone layout, as the `phone:` variant in index.css has it: narrower than `sm`, or a touch
 *  screen held sideways (about 520 px tall or less). Keep the two the same. */
export const PHONE_LAYOUT_QUERY =
  "not all and (min-width: 40rem), (pointer: coarse) and (max-height: 520px)";

/** Where the app shell shows its phone top bar (below `md`, AppShell) instead of the row beside
 *  the sidebar. */
export const PHONE_TOP_BAR_QUERY = "(max-width: 767.98px)";

function watch(query: string) {
  return {
    subscribe(onChange: () => void): () => void {
      if (typeof window === "undefined" || !window.matchMedia) return () => {};
      const mq = window.matchMedia(query);
      mq.addEventListener?.("change", onChange);
      return () => mq.removeEventListener?.("change", onChange);
    },
    snapshot: (): boolean => typeof window !== "undefined" && !!window.matchMedia?.(query).matches,
  };
}
const phoneLayout = watch(PHONE_LAYOUT_QUERY);
const phoneTopBar = watch(PHONE_TOP_BAR_QUERY);

/** True while the phone layout is on show (the `phone:` variant), live. */
export function usePhoneLayout(): boolean {
  return useSyncExternalStore(phoneLayout.subscribe, phoneLayout.snapshot, () => false);
}

/** True while `query` matches, live (one watcher per query). */
const watchers = new Map<string, ReturnType<typeof watch>>();
export function useMediaQuery(query: string): boolean {
  let w = watchers.get(query);
  if (!w) {
    w = watch(query);
    watchers.set(query, w);
  }
  return useSyncExternalStore(w.subscribe, w.snapshot, () => false);
}

/** True while the shell's phone top bar is on show (below `md`), live. */
export function usePhoneTopBar(): boolean {
  return useSyncExternalStore(phoneTopBar.subscribe, phoneTopBar.snapshot, () => false);
}
