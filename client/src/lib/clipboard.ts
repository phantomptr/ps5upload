/**
 * Copy text to the clipboard, reliably, from inside the Tauri webview.
 *
 * The async Clipboard API (`navigator.clipboard.writeText`) is only exposed
 * in a *secure context*. A packaged Tauri app serves its UI from a custom
 * protocol (`tauri://localhost` / `http://tauri.localhost`) that WKWebView
 * (macOS) and WebView2 (Windows) don't always classify as secure — so
 * `navigator.clipboard` is frequently `undefined` or rejects. That's why the
 * Logs "Copy" button silently did nothing: the call threw and the empty
 * `catch` swallowed it.
 *
 * Strategy: try the modern API when it's actually present, then fall back to
 * the legacy `document.execCommand("copy")` over a hidden, focused textarea.
 * The legacy path predates the secure-context requirement and keeps working
 * inside webviews — and needs no Tauri plugin or capability/ACL change.
 *
 * @returns true if the copy succeeded, false otherwise (caller surfaces it).
 */
export async function writeClipboard(text: string): Promise<boolean> {
  // Modern API first — instant + handles large payloads — but only when the
  // webview both exposes it and considers itself a secure context.
  try {
    if (
      typeof navigator !== "undefined" &&
      navigator.clipboard &&
      typeof navigator.clipboard.writeText === "function" &&
      window.isSecureContext
    ) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // Fall through to the legacy path below.
  }

  return execCommandCopy(text);
}

/** Hidden-textarea + execCommand("copy") fallback. Synchronous by nature. */
function execCommandCopy(text: string): boolean {
  if (typeof document === "undefined") return false;
  const ta = document.createElement("textarea");
  ta.value = text;
  // Keep it off-screen and unfocusable-by-layout, but selectable.
  ta.style.position = "fixed";
  ta.style.top = "-9999px";
  ta.style.left = "-9999px";
  ta.setAttribute("readonly", "");
  document.body.appendChild(ta);
  try {
    ta.select();
    ta.setSelectionRange(0, text.length);
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    document.body.removeChild(ta);
  }
}
