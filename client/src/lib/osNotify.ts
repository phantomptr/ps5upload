// Mirror in-app notifications to the OS notification center (macOS /
// Windows / Linux) and the Android/iOS notification shade, via
// tauri-plugin-notification.
//
// Design:
//   * No-ops gracefully outside the Tauri runtime (browser dev / tests)
//     and swallows every error — a notification must never break a flow.
//   * Only fires when the app is NOT in the foreground, so you don't get
//     a redundant system banner while you're looking at the in-app
//     inbox. "Foreground" is tracked from window focus + page
//     visibility, which both fire on desktop (minimise / switch window)
//     and mobile (app backgrounded).
//   * Permission is requested once up front (ensureOsNotificationPermission,
//     called at app startup) so the OS prompt — and the Android 13+
//     POST_NOTIFICATIONS runtime dialog — appears before the first
//     notification rather than mid-transfer.

import { isTauriEnv } from "./tauriEnv";
import type { NotificationLevel } from "../state/notifications";

// ── Foreground tracking ───────────────────────────────────────────────
// Two signals — focus and visibility — combined carefully so the gate is
// right on BOTH desktop and mobile:
//   * Desktop: window focus/blur fire reliably, so a visible-but-unfocused
//     window (e.g. side-by-side) should count as background → use
//     visibility AND focus.
//   * Android/iOS WebView: window focus/blur are unreliable (often never
//     fire) and document.hasFocus() can read false even in the
//     foreground, while `visibilitychange` is the authoritative signal.
//     Letting the focus term veto visibility there would make a
//     foregrounded app look "backgrounded" and fire a redundant banner
//     while the user is looking at the in-app inbox.
// Resolution: only let focus participate once a focus/blur event has
// actually been observed (proving the platform reports it); until then,
// trust visibility alone.
let focusObserved = false;
let windowFocused =
  typeof document !== "undefined" && typeof document.hasFocus === "function"
    ? document.hasFocus()
    : true;
let documentVisible =
  typeof document !== "undefined"
    ? document.visibilityState !== "hidden"
    : true;

if (typeof window !== "undefined") {
  window.addEventListener("focus", () => {
    windowFocused = true;
    focusObserved = true;
  });
  window.addEventListener("blur", () => {
    windowFocused = false;
    focusObserved = true;
  });
}
if (typeof document !== "undefined") {
  document.addEventListener("visibilitychange", () => {
    documentVisible = document.visibilityState !== "hidden";
  });
}

/** Whether the app is in the foreground. The OS mirror only fires when
 *  this is false (app backgrounded/unfocused). Visibility is always
 *  required; focus is ANDed in only on platforms that actually report
 *  focus/blur (desktop), so an Android WebView that never fires focus
 *  isn't wrongly treated as backgrounded. */
export function appIsForeground(): boolean {
  return documentVisible && (focusObserved ? windowFocused : true);
}

// ── Permission ────────────────────────────────────────────────────────
type PermState = "unknown" | "granted" | "denied";
let permission: PermState = "unknown";

/** Request OS notification permission once (idempotent). Returns whether
 *  it's granted. Surfaces the macOS prompt / Android 13+
 *  POST_NOTIFICATIONS dialog. Safe to call outside Tauri (returns false).
 */
export async function ensureOsNotificationPermission(): Promise<boolean> {
  if (!isTauriEnv()) return false;
  try {
    const { isPermissionGranted, requestPermission } = await import(
      "@tauri-apps/plugin-notification"
    );
    if (await isPermissionGranted()) {
      permission = "granted";
      return true;
    }
    const result = await requestPermission();
    permission = result === "granted" ? "granted" : "denied";
    return permission === "granted";
  } catch {
    permission = "denied";
    return false;
  }
}

/** Fire a native OS notification mirroring an in-app inbox entry.
 *  Best-effort: no-ops outside Tauri, when permission isn't granted, or
 *  on any error. `level` is folded into the title with a small marker
 *  for warning/error (the OS notification has no native severity field).
 */
export async function sendOsNotification(
  level: NotificationLevel,
  title: string,
  body?: string,
): Promise<void> {
  if (!isTauriEnv()) return;
  try {
    const mod = await import("@tauri-apps/plugin-notification");
    if (permission !== "granted") {
      const granted =
        (await mod.isPermissionGranted()) ||
        (await mod.requestPermission()) === "granted";
      permission = granted ? "granted" : "denied";
      if (!granted) return;
    }
    const marker =
      level === "error" ? "⛔ " : level === "warning" ? "⚠️ " : "";
    mod.sendNotification({ title: marker + title, body });
  } catch {
    // best-effort — never surface a notification failure to the user
  }
}
