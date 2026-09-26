import { isAndroid } from "./platform";
import { useAccessibilityStore } from "../state/accessibility";

/** Haptic feedback vocabulary (v5 §4.4 of mobile-design, §6.6 of
 *  cross-cutting).
 *
 *  Four events, each a vibration pattern suited to its meaning:
 *    tap       — 10ms, light tap on every button/tab press
 *    selection — 8ms, very light on toggle/checkbox/radio change
 *    confirm   — 20ms, medium on dialog confirm / drag-reorder drop
 *    danger    — [20,50,40] double-buzz on destructive confirm / task fail
 *
 *  Silenced when:
 *    - not Android (no Vibration API on iOS WebView; desktop has no vibrator)
 *    - the user disabled Haptic feedback in Settings → Accessibility
 *
 *  The function is a no-op (not an error) in both cases, so call sites
 *  can invoke `haptic("tap")` unconditionally without platform checks. */

export type HapticKind = "tap" | "confirm" | "danger" | "selection";

const PATTERNS: Record<HapticKind, number | number[]> = {
  tap: 10,
  confirm: 20,
  danger: [20, 50, 40],
  selection: 8,
};

export function haptic(kind: HapticKind): void {
  if (!isAndroid()) return;
  if (!useAccessibilityStore.getState().hapticsEnabled) return;
  if (typeof navigator === "undefined" || typeof navigator.vibrate !== "function")
    return;
  navigator.vibrate(PATTERNS[kind]);
}
