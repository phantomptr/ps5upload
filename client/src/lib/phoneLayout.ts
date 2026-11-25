import { useSyncExternalStore } from "react";

/** The phone layout, as the `phone:` variant in index.css has it: narrower than `sm`, or a touch
 *  screen held sideways (about 520 px tall or less). Keep the two the same. */
export const PHONE_LAYOUT_QUERY =
  "not all and (min-width: 40rem), (pointer: coarse) and (max-height: 520px)";

function subscribe(onChange: () => void): () => void {
  if (typeof window === "undefined" || !window.matchMedia) return () => {};
  const mq = window.matchMedia(PHONE_LAYOUT_QUERY);
  mq.addEventListener?.("change", onChange);
  return () => mq.removeEventListener?.("change", onChange);
}

function snapshot(): boolean {
  return typeof window !== "undefined" && !!window.matchMedia?.(PHONE_LAYOUT_QUERY).matches;
}

/** True while the phone layout is on show (the `phone:` variant), live. */
export function usePhoneLayout(): boolean {
  return useSyncExternalStore(subscribe, snapshot, () => false);
}
