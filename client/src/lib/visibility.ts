import { useEffect, useState } from "react";

/**
 * Page Visibility hook.
 *
 * Returns `true` when the window is visible, `false` when minimized
 * or backgrounded. Used by polling effects to skip wasteful network
 * round-trips while the user isn't looking.
 *
 * In Tauri we get visibility events for both webview-hidden (window
 * minimized) and tab-hidden cases. SSR returns `true` since there's
 * no document.
 */
export function useDocumentVisible(): boolean {
  const [visible, setVisible] = useState<boolean>(() => {
    if (typeof document === "undefined") return true;
    return document.visibilityState === "visible";
  });
  useEffect(() => {
    if (typeof document === "undefined") return;
    function onChange() {
      setVisible(document.visibilityState === "visible");
    }
    document.addEventListener("visibilitychange", onChange);
    // Re-sync on mount in case the visibility flipped between
    // initial state read and the listener attaching.
    onChange();
    return () => document.removeEventListener("visibilitychange", onChange);
  }, []);
  return visible;
}
