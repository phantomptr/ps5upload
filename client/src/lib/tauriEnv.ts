/* Detect whether we're running inside a Tauri WebView vs. a plain
 * browser. Tauri 2 sets `window.__TAURI_INTERNALS__` and `window.isTauri`
 * before any user JS runs; both are absent in a regular browser.
 *
 * This isn't user-facing — production always runs in Tauri — but it
 * lets browser-based dev/Playwright sessions exercise the UI without
 * tripping unhandled errors when Tauri-only globals like
 * `getCurrentWindow()` / `getCurrentWebview()` dereference an absent
 * internals object. Without this guard, the entire app falls into
 * the RootErrorBoundary the moment any screen tries to subscribe to
 * a drag-drop event in a browser. */
export function isTauriEnv(): boolean {
  if (typeof window === "undefined") return false;
  const w = window as unknown as {
    isTauri?: boolean;
    __TAURI_INTERNALS__?: unknown;
  };
  return Boolean(w.isTauri || w.__TAURI_INTERNALS__);
}
