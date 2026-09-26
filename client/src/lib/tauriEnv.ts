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

/** Fire-and-forget call of a Tauri event-listener unregister function.
 *
 *  Tauri 2's `_unlisten` looks up `listeners[eventId].handlerId`. If
 *  the webview tore down its listener table between subscribe + cleanup
 *  (HMR reload, route remount during dev, parent webview destroyed),
 *  the lookup throws `TypeError: undefined is not an object (evaluating
 *  'listeners[eventId].handlerId')`.
 *
 *  The plain `try/catch(unlisten())` pattern only catches synchronous
 *  throws — but `_unlisten` returns a Promise that can reject
 *  asynchronously with the same TypeError, surfacing as an unhandled
 *  rejection in the renderer. This helper covers both paths. */
export function safeUnlisten(fn: () => unknown): void {
  try {
    const r = fn();
    if (r && typeof (r as { catch?: unknown }).catch === "function") {
      (r as Promise<unknown>).catch(() => {
        /* ignore — listener table is gone, nothing to do */
      });
    }
  } catch {
    /* ignore — sync throw, same intent */
  }
}
